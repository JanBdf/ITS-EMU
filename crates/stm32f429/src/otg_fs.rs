//! USB OTG FS (the Synopsys DWC2 core, CN13 on the Nucleo) in device mode
//! with FIFOs (slave mode, as ST's PCD driver uses it without DMA).
//!
//! The bus side is a host model. Once the cable is plugged in
//! ([`OtgPort::cable`]) and the firmware connects (transceiver on, soft
//! disconnect off), it resets the bus, reads the device and configuration
//! descriptor and sets an address (vhci-hcd answers SET_ADDRESS itself, and
//! ST's device stack only configures an addressed device). Then it carries
//! out the transfers of a host on the computer ([`HostLink`]), one packet
//! at a time at full speed's bit rate.
//!
//! RX FIFO entries are status words with their data; popping a SETUP done
//! entry raises STUP, popping an OUT transfer completed entry raises XFRC
//! and disables the endpoint. An IN packet goes out once all of it is in the
//! endpoint's TX FIFO. Not modelled: host mode, isochronous endpoints,
//! suspend/resume, SOF timing, dedicated endpoint 1 interrupts, DMA.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Clock, DeviceCtx, Peripheral};
use usbip::UsbipServer;
use usbip::forward::{ForwardedDevice, HostLink, SharedBridge, Transfer};

use crate::rcc::SharedClocks;

pub const OTG_FS_IRQ: u32 = 67;
/// Base address and size of the register window (with the FIFO windows).
pub const OTG_FS_BASE: u32 = 0x5000_0000;
pub const OTG_FS_SIZE: u32 = 0x4_0000;

const NUM_EP: usize = 4;
/// FIFO RAM in 32-bit words (1.25 KB).
const FIFO_WORDS: u32 = 320;
/// Synopsys core id (GSNPSID) of the F42x/F43x OTG FS: core 2.81a.
const GSNPSID: u32 = 0x4F54_281A;
/// CID (product id), reset value.
const CID: u32 = 0x0000_1200;

// GINTSTS / GINTMSK
const CMOD: u32 = 1 << 0;
const OTGINT: u32 = 1 << 2;
const RXFLVL: u32 = 1 << 4;
const NPTXFE: u32 = 1 << 5;
const GINAKEFF: u32 = 1 << 6;
const GONAKEFF: u32 = 1 << 7;
const USBRST: u32 = 1 << 12;
const ENUMDNE: u32 = 1 << 13;
const IEPINT: u32 = 1 << 18;
const OEPINT: u32 = 1 << 19;
const PTXFE: u32 = 1 << 26;
/// Bits that writes of 1 clear.
const GINTSTS_W1C: u32 = 0xF030_FC0A;

// GRXSTSP packet status
const STS_DATA: u32 = 2;
const STS_XFER_DONE: u32 = 3;
const STS_SETUP_DONE: u32 = 4;
const STS_SETUP: u32 = 6;

// DxEPCTL
const EPENA: u32 = 1 << 31;
const EPDIS: u32 = 1 << 30;
const SNAK: u32 = 1 << 27;
const CNAK: u32 = 1 << 26;
const STALL: u32 = 1 << 21;
const NAKSTS: u32 = 1 << 17;
const USBAEP: u32 = 1 << 15;
/// Bits a write stores (MPSIZ, USBAEP, EPTYP, SNPM, STALL, TXFNUM).
const EPCTL_STORED: u32 = 0x7FF | USBAEP | (3 << 18) | (1 << 20) | STALL | (0xF << 22);

// DxEPINT
const XFRC: u32 = 1 << 0;
const EPDISD: u32 = 1 << 1;
const STUP: u32 = 1 << 3;
const TXFE: u32 = 1 << 7;

// GCCFG, DCTL, GUSBCFG
const PWRDWN: u32 = 1 << 16;
const VBUSBSEN: u32 = 1 << 19;
const NOVBUSSENS: u32 = 1 << 21;
const SDIS: u32 = 1 << 1;
const FHMOD: u32 = 1 << 29;

/// Delay from the pull-up to the bus reset, and the reset's length.
const ATTACH_NS: u64 = 5_000_000;
const RESET_NS: u64 = 10_000_000;
/// Full speed: 12 Mbit/s; a transaction's token, handshake and gaps take
/// about 13 bytes' time besides the data.
const BIT_NS: u64 = 1000 / 12;
const TRANSACTION_OVERHEAD: u64 = 13;

/// How often a connected firmware without a cable looks for one (cycles).
const CABLE_POLL_CYCLES: u64 = 1_000_000;

/// Ids of the model's own control transfers (the computer's count from 1).
const OWN_ID: u64 = u64::MAX - 16;

/// The cable and the computer's side, shared with the board's front end.
#[derive(Default, Debug)]
pub struct OtgPort {
    /// A cable from a host is plugged into CN13.
    pub cable: bool,
    pub link: HostLink,
}

pub type SharedOtgPort = Rc<RefCell<OtgPort>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BusState {
    /// No host, or the device has not connected.
    Detached,
    /// Pull-up seen; the host resets the bus at the cycle.
    Attaching(u64),
    /// Bus reset until the cycle.
    Reset(u64),
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// The SETUP packet is to be sent.
    Setup,
    /// Waiting for the firmware to pop the SETUP done entry.
    SetupPending,
    DataIn,
    DataOut,
    StatusIn,
    StatusOut,
}

/// The control transfer under way.
#[derive(Debug)]
struct Control {
    t: Transfer,
    stage: Stage,
    /// IN data so far, or OUT data sent so far (bytes).
    data: Vec<u8>,
    sent: usize,
}

/// An RX FIFO entry: status word and data.
#[derive(Debug)]
struct RxEntry {
    status: u32,
    data: Vec<u8>,
}

impl RxEntry {
    fn new(ep: usize, pktsts: u32, data: Vec<u8>) -> Self {
        let status = ep as u32 | ((data.len() as u32) << 4) | (pktsts << 17);
        Self { status, data }
    }

    fn words(&self) -> u32 {
        1 + self.data.len().div_ceil(4) as u32
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct InEp {
    ctl: u32,
    int: u32,
    tsiz: u32,
}

#[derive(Debug, Default, Clone, Copy)]
struct OutEp {
    ctl: u32,
    int: u32,
    tsiz: u32,
}

/// One packet's outcome.
enum Packet {
    Data(Vec<u8>),
    Nak,
    Stall,
}

pub struct OtgFs {
    clocks: SharedClocks,
    port: SharedOtgPort,
    gotgint: u32,
    gahbcfg: u32,
    gusbcfg: u32,
    gintsts: u32,
    gintmsk: u32,
    grxfsiz: u32,
    /// DIEPTXF0 and DIEPTXF1..3 (start address and depth in words).
    txfsiz: [u32; NUM_EP],
    hptxfsiz: u32,
    gccfg: u32,
    dcfg: u32,
    dctl: u32,
    diepmsk: u32,
    doepmsk: u32,
    daintmsk: u32,
    diepempmsk: u32,
    pcgcctl: u32,
    inep: [InEp; NUM_EP],
    outep: [OutEp; NUM_EP],
    rx: VecDeque<RxEntry>,
    /// The entry whose data the firmware is reading, and how far.
    rx_current: Option<(Vec<u8>, usize)>,
    tx: [VecDeque<u32>; NUM_EP],
    bus: BusState,
    /// Frame counter base: cycle of the end of the bus reset.
    ready_at: u64,
    ctrl: Option<Control>,
    /// Bulk/interrupt transfers under way per endpoint (IN, OUT): the
    /// transfer and the data received / bytes sent.
    bulk_in: [Option<(Transfer, Vec<u8>)>; NUM_EP],
    bulk_out: [Option<(Transfer, usize)>; NUM_EP],
    /// The model's own requests: device descriptor, address, configuration.
    own: VecDeque<Transfer>,
    device_desc: Vec<u8>,
    /// The bus is busy with the last packet until this cycle.
    bus_free_at: u64,
    /// Something changed that may let a waiting transfer go on.
    kick: bool,
    /// Highest id of the computer's transfers seen so far.
    seen_request: u64,
    last_now: u64,
    warned_clock: bool,
}

impl OtgFs {
    pub fn new(clocks: SharedClocks, port: SharedOtgPort) -> Self {
        let mut s = Self {
            clocks,
            port,
            gotgint: 0,
            gahbcfg: 0,
            gusbcfg: 0,
            gintsts: 0,
            gintmsk: 0,
            grxfsiz: 0,
            txfsiz: [0; NUM_EP],
            hptxfsiz: 0,
            gccfg: 0,
            dcfg: 0,
            dctl: 0,
            diepmsk: 0,
            doepmsk: 0,
            daintmsk: 0,
            diepempmsk: 0,
            pcgcctl: 0,
            inep: [InEp::default(); NUM_EP],
            outep: [OutEp::default(); NUM_EP],
            rx: VecDeque::new(),
            rx_current: None,
            tx: Default::default(),
            bus: BusState::Detached,
            ready_at: 0,
            ctrl: None,
            bulk_in: Default::default(),
            bulk_out: Default::default(),
            own: VecDeque::new(),
            device_desc: Vec::new(),
            bus_free_at: 0,
            kick: false,
            seen_request: 0,
            last_now: 0,
            warned_clock: false,
        };
        s.reset_registers();
        s
    }

    fn reset_registers(&mut self) {
        self.gotgint = 0;
        self.gahbcfg = 0;
        self.gusbcfg = 0x0000_0A40;
        self.gintsts = 0x0400_0020 & !(PTXFE | NPTXFE);
        self.gintmsk = 0;
        self.grxfsiz = 0x200;
        self.txfsiz = [0x0200_0200, 0x0200_0400, 0x0200_0600, 0x0200_0800];
        self.hptxfsiz = 0x0200_0600;
        self.gccfg = 0;
        self.dcfg = 0x0220_0000;
        self.dctl = 0;
        self.diepmsk = 0;
        self.doepmsk = 0;
        self.daintmsk = 0;
        self.diepempmsk = 0;
        self.pcgcctl = 0;
        self.core_reset();
    }

    /// GRSTCTL.CSRST: interrupts, FIFOs and endpoints back to their reset state.
    fn core_reset(&mut self) {
        self.gintsts &= !GINTSTS_W1C;
        self.inep = [InEp::default(); NUM_EP];
        self.outep = [OutEp::default(); NUM_EP];
        self.inep[0].ctl = USBAEP;
        self.outep[0].ctl = USBAEP;
        self.rx.clear();
        self.rx_current = None;
        for f in &mut self.tx {
            f.clear();
        }
    }

    fn clock_enabled(&self) -> bool {
        self.clocks.get().ahb2enr & (1 << 7) != 0
    }

    /// The device is on the bus: clocked, transceiver on, pull-up on, in
    /// device mode, with a cable whose VBUS it accepts.
    fn connected(&self) -> bool {
        self.clock_enabled()
            && self.port.borrow().cable
            && self.gccfg & PWRDWN != 0
            && self.dctl & SDIS == 0
            && self.gusbcfg & FHMOD == 0
            && self.gccfg & (NOVBUSSENS | VBUSBSEN) != 0
    }

    fn ns(clock: &Clock, ns: u64) -> u64 {
        clock.cycles() + clock.ns_to_cycles(ns).max(1)
    }

    // --- FIFOs ---------------------------------------------------------------

    fn rx_depth(&self) -> u32 {
        (self.grxfsiz & 0xFFFF).min(FIFO_WORDS)
    }

    fn rx_used(&self) -> u32 {
        let current = self.rx_current.as_ref().map_or(0, |(d, pos)| (d.len() - pos).div_ceil(4) as u32);
        self.rx.iter().map(RxEntry::words).sum::<u32>() + current
    }

    fn rx_free(&self) -> u32 {
        self.rx_depth().saturating_sub(self.rx_used())
    }

    fn tx_depth(&self, ep: usize) -> u32 {
        self.txfsiz[ep] >> 16
    }

    fn tx_free(&self, ep: usize) -> u32 {
        self.tx_depth(ep).saturating_sub(self.tx[ep].len() as u32)
    }

    /// DIEPINT.TXFE: the TX FIFO is half empty (GAHBCFG.TXFELVL = 0) or empty.
    fn tx_empty(&self, ep: usize) -> bool {
        let used = self.tx[ep].len() as u32;
        if self.gahbcfg & (1 << 7) != 0 { used == 0 } else { used <= self.tx_depth(ep) / 2 }
    }

    /// GRXSTSP: pops the next entry; its data follows on the FIFO.
    fn pop_rx(&mut self) -> u32 {
        // data the firmware did not read is lost with the next pop
        self.rx_current = None;
        let Some(e) = self.rx.pop_front() else { return 0 };
        let ep = (e.status & 0xF) as usize;
        match (e.status >> 17) & 0xF {
            STS_SETUP_DONE => {
                self.outep[0].int |= STUP;
                // STUPCNT counts the SETUP packets still to be received
                let tsiz = self.outep[0].tsiz;
                let stupcnt = ((tsiz >> 29) & 3).saturating_sub(1);
                self.outep[0].tsiz = (tsiz & !(3 << 29)) | (stupcnt << 29);
            }
            STS_XFER_DONE => {
                self.outep[ep].ctl &= !EPENA;
                self.outep[ep].ctl |= NAKSTS;
                self.outep[ep].int |= XFRC;
            }
            _ => {}
        }
        if !e.data.is_empty() {
            self.rx_current = Some((e.data, 0));
        }
        self.kick = true;
        e.status
    }

    fn read_rx_word(&mut self) -> u32 {
        let Some((data, pos)) = &mut self.rx_current else { return 0 };
        let mut w = [0u8; 4];
        let n = (data.len() - *pos).min(4);
        w[..n].copy_from_slice(&data[*pos..*pos + n]);
        *pos += n;
        if *pos >= data.len() {
            self.rx_current = None;
        }
        u32::from_le_bytes(w)
    }

    fn flush_tx(&mut self, num: u32) {
        for (ep, f) in self.tx.iter_mut().enumerate() {
            if num == 0x10 || num as usize == ep {
                f.clear();
            }
        }
    }

    // --- interrupts -------------------------------------------------------------

    fn in_mask(&self, ep: usize) -> u32 {
        self.diepmsk | (((self.diepempmsk >> ep) & 1) << 7)
    }

    fn diepint(&self, ep: usize) -> u32 {
        let txfe = if self.tx_empty(ep) { TXFE } else { 0 };
        self.inep[ep].int | txfe
    }

    fn daint(&self) -> u32 {
        let mut v = 0;
        for ep in 0..NUM_EP {
            if self.diepint(ep) & self.in_mask(ep) != 0 {
                v |= 1 << ep;
            }
            if self.outep[ep].int & self.doepmsk != 0 {
                v |= 1 << (16 + ep);
            }
        }
        v
    }

    fn gintsts(&self) -> u32 {
        let mut v = self.gintsts & !(CMOD | RXFLVL | IEPINT | OEPINT | OTGINT);
        if self.gusbcfg & FHMOD != 0 {
            v |= CMOD;
        }
        if !self.rx.is_empty() {
            v |= RXFLVL;
        }
        let daint = self.daint() & self.daintmsk;
        if daint & 0xFFFF != 0 {
            v |= IEPINT;
        }
        if daint >> 16 != 0 {
            v |= OEPINT;
        }
        if self.gotgint != 0 {
            v |= OTGINT;
        }
        v
    }

    fn sync_irq(&self, ctx: &mut DeviceCtx) {
        let level = self.gahbcfg & 1 != 0 && self.gintsts() & self.gintmsk != 0;
        ctx.set_irq(OTG_FS_IRQ, level);
    }

    // --- the host ---------------------------------------------------------------

    /// Max packet size of IN endpoint `ep` (endpoint 0: the 2-bit code).
    fn in_mps(&self, ep: usize) -> usize {
        let ctl = self.inep[ep].ctl;
        if ep == 0 { [64, 32, 16, 8][(ctl & 3) as usize] } else { (ctl & 0x7FF).max(1) as usize }
    }

    fn out_mps(&self, ep: usize) -> usize {
        if ep == 0 { self.in_mps(0) } else { (self.outep[ep].ctl & 0x7FF).max(1) as usize }
    }

    /// The host sends an IN token to `ep`.
    fn in_packet(&mut self, ep: usize) -> Packet {
        let e = self.inep[ep];
        if e.ctl & STALL != 0 {
            return Packet::Stall;
        }
        let pktcnt = (e.tsiz >> 19) & 0x3FF;
        if e.ctl & EPENA == 0 || e.ctl & NAKSTS != 0 || pktcnt == 0 {
            return Packet::Nak;
        }
        let xfrsiz = (e.tsiz & 0x7FFFF) as usize;
        let len = xfrsiz.min(self.in_mps(ep));
        let words = len.div_ceil(4);
        if self.tx[ep].len() < words {
            return Packet::Nak; // the firmware has not written all of it yet
        }
        let mut data = Vec::with_capacity(words * 4);
        for _ in 0..words {
            data.extend(self.tx[ep].pop_front().unwrap().to_le_bytes());
        }
        data.truncate(len);
        let xfrsiz = xfrsiz - len;
        let pktcnt = pktcnt - 1;
        self.inep[ep].tsiz = (e.tsiz & !(0x7FFFF | (0x3FF << 19))) | xfrsiz as u32 | (pktcnt << 19);
        if pktcnt == 0 {
            self.inep[ep].ctl &= !EPENA;
            self.inep[ep].int |= XFRC;
        }
        Packet::Data(data)
    }

    /// The host sends an OUT packet to `ep`; NAKed while the endpoint is
    /// not ready or the RX FIFO has no room.
    fn out_packet(&mut self, ep: usize, data: &[u8]) -> Packet {
        let e = self.outep[ep];
        if e.ctl & STALL != 0 {
            return Packet::Stall;
        }
        let pktcnt = (e.tsiz >> 19) & 0x3FF;
        if e.ctl & EPENA == 0 || e.ctl & NAKSTS != 0 || pktcnt == 0 {
            return Packet::Nak;
        }
        let entry = RxEntry::new(ep, STS_DATA, data.to_vec());
        // room for the data and a transfer completed entry
        if self.rx_free() < entry.words() + 1 {
            return Packet::Nak;
        }
        self.rx.push_back(entry);
        let xfrsiz = (e.tsiz & 0x7FFFF).saturating_sub(data.len() as u32);
        let pktcnt = pktcnt - 1;
        self.outep[ep].tsiz = (e.tsiz & !(0x7FFFF | (0x3FF << 19))) | xfrsiz | (pktcnt << 19);
        if pktcnt == 0 || data.len() < self.out_mps(ep) {
            self.rx.push_back(RxEntry::new(ep, STS_XFER_DONE, Vec::new()));
        }
        Packet::Data(Vec::new())
    }

    /// The bus is busy for a packet of `len` bytes.
    fn occupy_bus(&mut self, clock: &Clock, len: usize) {
        let ns = (len as u64 + TRANSACTION_OVERHEAD) * 8 * BIT_NS;
        self.bus_free_at = Self::ns(clock, ns);
    }

    /// Finishes one of the computer's transfers, or one of the model's own.
    fn finish(&mut self, t: &Transfer, result: Option<Vec<u8>>) {
        if t.id < OWN_ID {
            self.port.borrow_mut().link.done.push((t.id, result));
            return;
        }
        let Some(data) = result else {
            log::warn!("OTG FS: the device stalled the host's request {:02x?}", t.setup);
            return;
        };
        match t.setup[3] {
            1 if t.setup[1] == 6 => {
                self.device_desc = data;
                self.own.push_back(own_request(OWN_ID + 1, [0x00, 5, 1, 0, 0, 0, 0, 0]));
            }
            _ if t.setup[1] == 5 => {
                self.own.push_back(own_request(OWN_ID + 2, [0x80, 6, 0, 2, 0, 0, 9, 0]));
            }
            2 if data.len() == 9 => {
                let [l0, l1] = [data[2], data[3]];
                self.own.push_back(own_request(OWN_ID + 3, [0x80, 6, 0, 2, 0, 0, l0, l1]));
            }
            2 => {
                log::info!("OTG FS: USB device {:02x?}, configuration {data:02x?}", self.device_desc);
                self.port.borrow_mut().link.descriptors = Some((self.device_desc.clone(), data));
            }
            _ => {}
        }
    }

    /// Moves the control transfer on by one packet; false if it waits.
    fn step_control(&mut self, clock: &Clock) -> bool {
        let Some(mut c) = self.ctrl.take() else {
            let t = match self.own.pop_front() {
                Some(t) => t,
                None => {
                    let mut port = self.port.borrow_mut();
                    let Some(i) = port.link.requests.iter().position(|t| t.ep == 0) else { return false };
                    port.link.requests.remove(i).unwrap()
                }
            };
            self.ctrl = Some(Control { t, stage: Stage::Setup, data: Vec::new(), sent: 0 });
            return true;
        };
        let length = usize::from(u16::from_le_bytes([c.t.setup[6], c.t.setup[7]]));
        let dir_in = c.t.setup[0] & 0x80 != 0;
        let mut progress = false;
        match c.stage {
            Stage::Setup => {
                // SETUP packets are always taken (into the RX FIFO)
                if self.rx_free() >= 4 {
                    self.rx.push_back(RxEntry::new(0, STS_SETUP, c.t.setup.to_vec()));
                    self.rx.push_back(RxEntry::new(0, STS_SETUP_DONE, Vec::new()));
                    // a SETUP packet clears the stall and NAKs the data stage
                    // until the firmware has prepared it
                    self.inep[0].ctl = (self.inep[0].ctl & !STALL) | NAKSTS;
                    self.outep[0].ctl = (self.outep[0].ctl & !STALL) | NAKSTS;
                    self.occupy_bus(clock, 8);
                    c.stage = Stage::SetupPending;
                    progress = true;
                }
            }
            Stage::SetupPending => {
                if self.rx.iter().all(|e| (e.status >> 17) & 0xF != STS_SETUP_DONE) {
                    c.stage = match (dir_in, length) {
                        (_, 0) => Stage::StatusIn,
                        (true, _) => Stage::DataIn,
                        (false, _) => Stage::DataOut,
                    };
                    progress = true;
                }
            }
            Stage::DataIn => match self.in_packet(0) {
                Packet::Data(d) => {
                    self.occupy_bus(clock, d.len());
                    let short = d.len() < self.in_mps(0);
                    c.data.extend(d);
                    if short || c.data.len() >= length {
                        c.data.truncate(length);
                        c.stage = Stage::StatusOut;
                    }
                    progress = true;
                }
                Packet::Nak => {}
                Packet::Stall => {
                    self.finish(&c.t, None);
                    return true;
                }
            },
            Stage::DataOut => {
                let end = (c.sent + self.out_mps(0)).min(c.t.data.len());
                let chunk = c.t.data[c.sent..end].to_vec();
                match self.out_packet(0, &chunk) {
                    Packet::Data(_) => {
                        self.occupy_bus(clock, chunk.len());
                        c.sent = end;
                        if c.sent >= c.t.data.len() {
                            c.stage = Stage::StatusIn;
                        }
                        progress = true;
                    }
                    Packet::Nak => {}
                    Packet::Stall => {
                        self.finish(&c.t, None);
                        return true;
                    }
                }
            }
            Stage::StatusIn => match self.in_packet(0) {
                Packet::Data(_) => {
                    self.occupy_bus(clock, 0);
                    self.finish(&c.t, Some(Vec::new()));
                    return true;
                }
                Packet::Nak => {}
                Packet::Stall => {
                    self.finish(&c.t, None);
                    return true;
                }
            },
            Stage::StatusOut => match self.out_packet(0, &[]) {
                Packet::Data(_) => {
                    self.occupy_bus(clock, 0);
                    let data = std::mem::take(&mut c.data);
                    self.finish(&c.t, Some(data));
                    return true;
                }
                Packet::Nak => {}
                Packet::Stall => {
                    self.finish(&c.t, None);
                    return true;
                }
            },
        }
        self.ctrl = Some(c);
        progress
    }

    /// Moves the bulk and interrupt transfers on by one packet each; false
    /// if all of them wait.
    fn step_bulk(&mut self, clock: &Clock) -> bool {
        // take the next transfer of each idle endpoint
        {
            let mut port = self.port.borrow_mut();
            let mut i = 0;
            while i < port.link.requests.len() {
                let t = &port.link.requests[i];
                let ep = usize::from(t.ep);
                let busy = ep == 0
                    || ep >= NUM_EP
                    || if t.dir_in { self.bulk_in[ep].is_some() } else { self.bulk_out[ep].is_some() };
                if busy {
                    if ep >= NUM_EP {
                        let t = port.link.requests.remove(i).unwrap();
                        port.link.done.push((t.id, None));
                        continue;
                    }
                    i += 1;
                    continue;
                }
                let t = port.link.requests.remove(i).unwrap();
                if t.dir_in {
                    self.bulk_in[ep] = Some((t, Vec::new()));
                } else {
                    self.bulk_out[ep] = Some((t, 0));
                }
            }
        }
        let mut progress = false;
        for ep in 1..NUM_EP {
            if let Some((t, mut got)) = self.bulk_in[ep].take() {
                match self.in_packet(ep) {
                    Packet::Data(d) => {
                        self.occupy_bus(clock, d.len());
                        let short = d.len() < self.in_mps(ep);
                        got.extend(d);
                        progress = true;
                        if short || got.len() >= t.max_len {
                            self.finish(&t, Some(got));
                        } else {
                            self.bulk_in[ep] = Some((t, got));
                        }
                    }
                    Packet::Nak => self.bulk_in[ep] = Some((t, got)),
                    Packet::Stall => {
                        self.finish(&t, None);
                        progress = true;
                    }
                }
            }
            if self.bus_free_at > clock.cycles() {
                break;
            }
            if let Some((t, sent)) = self.bulk_out[ep].take() {
                let end = (sent + self.out_mps(ep)).min(t.data.len());
                let chunk = t.data[sent..end].to_vec();
                match self.out_packet(ep, &chunk) {
                    Packet::Data(_) => {
                        self.occupy_bus(clock, chunk.len());
                        progress = true;
                        if end >= t.data.len() {
                            self.finish(&t, Some(Vec::new()));
                        } else {
                            self.bulk_out[ep] = Some((t, end));
                        }
                    }
                    Packet::Nak => self.bulk_out[ep] = Some((t, sent)),
                    Packet::Stall => {
                        self.finish(&t, None);
                        progress = true;
                    }
                }
            }
            if self.bus_free_at > clock.cycles() {
                break;
            }
        }
        progress
    }

    /// The device went off the bus: the host forgets it.
    fn detach(&mut self) {
        self.bus = BusState::Detached;
        self.ctrl = None;
        self.own.clear();
        let mut port = self.port.borrow_mut();
        port.link.descriptors = None;
        for t in self.bulk_in.iter_mut().filter_map(Option::take).map(|(t, _)| t) {
            port.link.done.push((t.id, None));
        }
        for t in self.bulk_out.iter_mut().filter_map(Option::take).map(|(t, _)| t) {
            port.link.done.push((t.id, None));
        }
        let lost: Vec<u64> = port.link.requests.drain(..).map(|t| t.id).collect();
        port.link.done.extend(lost.into_iter().map(|id| (id, None)));
    }

    /// The computer's host has queued a transfer since the last look.
    fn new_request(&self) -> bool {
        self.port.borrow().link.requests.iter().any(|t| t.id > self.seen_request)
    }

    /// Transfers are waiting for the bus.
    fn busy(&self) -> bool {
        self.ctrl.is_some()
            || !self.own.is_empty()
            || self.bulk_in.iter().any(Option::is_some)
            || self.bulk_out.iter().any(Option::is_some)
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        let now = ctx.now();
        self.last_now = now;
        if self.new_request() {
            self.seen_request = self.port.borrow().link.requests.iter().map(|t| t.id).max().unwrap_or(0);
            self.kick = true;
        }
        let connected = self.connected();
        match self.bus {
            BusState::Detached if connected => {
                let c = self.clocks.get();
                if c.pll48 != 48_000_000 && !self.warned_clock {
                    self.warned_clock = true;
                    log::warn!(
                        "OTG FS: the 48 MHz clock (PLL Q output) is {:.3} MHz: USB does not work on the board",
                        f64::from(c.pll48) / 1e6
                    );
                }
                self.bus = BusState::Attaching(Self::ns(ctx.clock, ATTACH_NS));
            }
            BusState::Detached => {}
            _ if !connected => self.detach(),
            BusState::Attaching(at) if now >= at => {
                self.gintsts |= USBRST;
                self.bus = BusState::Reset(Self::ns(ctx.clock, RESET_NS));
            }
            BusState::Reset(at) if now >= at => {
                self.gintsts |= ENUMDNE;
                self.bus = BusState::Ready;
                self.ready_at = now;
                self.bus_free_at = now;
                self.ctrl = None;
                self.own.clear();
                self.own.push_back(own_request(OWN_ID, [0x80, 6, 0, 1, 0, 0, 18, 0]));
            }
            BusState::Attaching(_) | BusState::Reset(_) => {}
            BusState::Ready => {
                if self.kick || now >= self.bus_free_at {
                    self.kick = false;
                    // packets until the bus is busy or everything waits
                    while now >= self.bus_free_at {
                        let remote = self.port.borrow().link.remote;
                        let control = self.step_control(ctx.clock);
                        let bulk = remote && now >= self.bus_free_at && self.step_bulk(ctx.clock);
                        if !control && !bulk {
                            break;
                        }
                    }
                }
            }
        }
        self.sync_irq(ctx);
    }

    /// The firmware has the device ready (clock, transceiver, pull-up) but
    /// no cable is in: a cable may come at any time.
    fn waiting_for_cable(&self) -> bool {
        self.clock_enabled() && self.gccfg & PWRDWN != 0 && self.dctl & SDIS == 0 && !self.port.borrow().cable
    }

    fn next_event(&self) -> Option<u64> {
        let mut at = None::<u64>;
        let mut add = |t: u64| at = Some(at.map_or(t, |a| a.min(t)));
        match self.bus {
            BusState::Attaching(t) | BusState::Reset(t) => add(t),
            BusState::Detached if self.connected() => add(self.last_now + 1),
            BusState::Detached if self.waiting_for_cable() => add(self.last_now + CABLE_POLL_CYCLES),
            BusState::Ready => {
                if self.kick || self.new_request() {
                    add(self.last_now + 1);
                } else if self.busy() || !self.port.borrow().link.requests.is_empty() {
                    // waiting for the bus, or polling an endpoint the
                    // firmware has not armed yet (it says so by writing)
                    if self.bus_free_at > self.last_now {
                        add(self.bus_free_at);
                    }
                }
            }
            BusState::Detached => {}
        }
        at
    }

    fn reg(&self, off: u32) -> u32 {
        match off {
            0x000 => {
                // GOTGCTL: B-device, session valid while a cable is in
                let cable = self.port.borrow().cable;
                (1 << 16) | if cable { 1 << 19 } else { 0 }
            }
            0x004 => self.gotgint,
            0x008 => self.gahbcfg,
            0x00C => self.gusbcfg,
            0x010 => 1 << 31, // GRSTCTL: AHB idle, resets and flushes done
            0x014 => self.gintsts(),
            0x018 => self.gintmsk,
            0x01C => self.rx.front().map_or(0, |e| e.status),
            0x024 => self.grxfsiz,
            0x028 => self.txfsiz[0],
            0x02C => {
                // HNPTXSTS: free space of the non-periodic TX FIFO (EP0)
                self.tx_free(0) | (8 << 16)
            }
            0x038 => self.gccfg,
            0x03C => CID,
            0x040 => GSNPSID,
            0x100 => self.hptxfsiz,
            0x104 | 0x108 | 0x10C => self.txfsiz[((off - 0x100) / 4) as usize],
            0x800 => self.dcfg,
            0x804 => self.dctl,
            0x808 => {
                // DSTS: full speed (48 MHz PHY clock), the frame number
                let frame = if self.bus == BusState::Ready { (self.last_now - self.ready_at) as u32 } else { 0 };
                let ms = frame / (self.clocks.get().hclk / 1000).max(1);
                (3 << 1) | ((ms & 0x3FFF) << 8)
            }
            0x810 => self.diepmsk,
            0x814 => self.doepmsk,
            0x818 => self.daint(),
            0x81C => self.daintmsk,
            0x834 => self.diepempmsk,
            0x900..=0x9FF => {
                let ep = ((off - 0x900) / 0x20) as usize;
                if ep >= NUM_EP {
                    return 0;
                }
                match (off - 0x900) % 0x20 {
                    0x00 => self.inep[ep].ctl,
                    0x08 => self.diepint(ep),
                    0x10 => self.inep[ep].tsiz,
                    0x18 => self.tx_free(ep),
                    _ => 0,
                }
            }
            0xB00..=0xBFF => {
                let ep = ((off - 0xB00) / 0x20) as usize;
                if ep >= NUM_EP {
                    return 0;
                }
                match (off - 0xB00) % 0x20 {
                    0x00 => self.outep[ep].ctl,
                    0x08 => self.outep[ep].int,
                    0x10 => self.outep[ep].tsiz,
                    _ => 0,
                }
            }
            0xE00 => self.pcgcctl,
            _ => 0,
        }
    }

    fn write_epctl(ctl: &mut u32, int: &mut u32, v: u32, ep0: bool) {
        let enabled = *ctl & EPENA != 0;
        let mut new = (*ctl & !EPCTL_STORED) | (v & EPCTL_STORED);
        if ep0 {
            new |= USBAEP;
        }
        if v & CNAK != 0 {
            new &= !NAKSTS;
        }
        if v & SNAK != 0 {
            new |= NAKSTS;
        }
        if v & EPENA != 0 {
            new |= EPENA;
        }
        if v & EPDIS != 0 && enabled {
            new &= !EPENA;
            new |= NAKSTS;
            *int |= EPDISD;
        }
        *ctl = new;
    }

    fn write_reg(&mut self, off: u32, v: u32) {
        match off {
            0x004 => self.gotgint &= !v,
            0x008 => self.gahbcfg = v & 0x1A1,
            // PHYSEL reads 1: the FS core has only the embedded PHY
            0x00C => self.gusbcfg = (v & 0x6000_3FBF) | 0x40,
            0x010 => {
                if v & 1 != 0 {
                    self.core_reset();
                }
                if v & (1 << 4) != 0 {
                    self.rx.clear();
                    self.rx_current = None;
                }
                if v & (1 << 5) != 0 {
                    self.flush_tx((v >> 6) & 0x1F);
                }
            }
            0x014 => self.gintsts &= !(v & GINTSTS_W1C),
            0x018 => self.gintmsk = v,
            0x024 => self.grxfsiz = v & 0xFFFF,
            0x028 => self.txfsiz[0] = v,
            0x038 => self.gccfg = v & 0x003F_0000,
            0x100 => self.hptxfsiz = v,
            0x104 | 0x108 | 0x10C => self.txfsiz[((off - 0x100) / 4) as usize] = v,
            0x800 => self.dcfg = v,
            0x804 => {
                let mut d = v & !(0x3 << 2) & 0xFFF;
                // global IN/OUT NAK: set, effective at once, cleared
                let mut status = self.dctl & (3 << 2);
                if v & (1 << 7) != 0 {
                    status |= 1 << 2;
                    self.gintsts |= GINAKEFF;
                }
                if v & (1 << 8) != 0 {
                    status &= !(1 << 2);
                    self.gintsts &= !GINAKEFF;
                }
                if v & (1 << 9) != 0 {
                    status |= 1 << 3;
                    self.gintsts |= GONAKEFF;
                }
                if v & (1 << 10) != 0 {
                    status &= !(1 << 3);
                    self.gintsts &= !GONAKEFF;
                }
                d &= !(0xF << 7);
                self.dctl = d | status;
            }
            0x810 => self.diepmsk = v,
            0x814 => self.doepmsk = v,
            0x81C => self.daintmsk = v,
            0x834 => self.diepempmsk = v & 0xFFFF,
            0x900..=0x9FF => {
                let ep = ((off - 0x900) / 0x20) as usize;
                if ep >= NUM_EP {
                    return;
                }
                let e = &mut self.inep[ep];
                match (off - 0x900) % 0x20 {
                    0x00 => Self::write_epctl(&mut e.ctl, &mut e.int, v, ep == 0),
                    0x08 => e.int &= !(v & !TXFE),
                    0x10 => e.tsiz = v & if ep == 0 { 0x0018_007F } else { 0x7FFF_FFFF },
                    _ => {}
                }
            }
            0xB00..=0xBFF => {
                let ep = ((off - 0xB00) / 0x20) as usize;
                if ep >= NUM_EP {
                    return;
                }
                let e = &mut self.outep[ep];
                match (off - 0xB00) % 0x20 {
                    0x00 => Self::write_epctl(&mut e.ctl, &mut e.int, v, ep == 0),
                    0x08 => e.int &= !v,
                    0x10 => e.tsiz = v & if ep == 0 { 0x6008_007F } else { 0x7FFF_FFFF },
                    _ => {}
                }
            }
            0xE00 => self.pcgcctl = v & 0x13,
            _ => {}
        }
    }
}

fn own_request(id: u64, setup: [u8; 8]) -> Transfer {
    let max_len = usize::from(u16::from_le_bytes([setup[6], setup[7]]));
    Transfer { id, ep: 0, dir_in: setup[0] & 0x80 != 0, setup, data: Vec::new(), max_len }
}

impl Peripheral for OtgFs {
    fn name(&self) -> &str {
        "OTG_FS"
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        self.update(ctx);
        if !self.clock_enabled() {
            return 0;
        }
        let v = if off >= 0x1000 {
            if off < 0x1000 * (NUM_EP as u32 + 1) {
                // any FIFO window reads the RX FIFO
                let w = self.read_rx_word();
                self.kick = true;
                w
            } else {
                0
            }
        } else if off & !3 == 0x020 {
            self.pop_rx()
        } else {
            self.reg(off & !3)
        };
        self.sync_irq(ctx);
        extract_read(v, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        self.update(ctx);
        if !self.clock_enabled() {
            return;
        }
        if off >= 0x1000 {
            let ep = (off / 0x1000 - 1) as usize;
            if ep < NUM_EP {
                if self.tx_free(ep) > 0 {
                    self.tx[ep].push_back(value);
                } else {
                    log::warn!("OTG FS: TX FIFO {ep} overflows");
                }
            }
        } else {
            let reg = off & !3;
            let v = if size == 4 { value } else { merge_write(self.reg(reg), off, size, value) };
            self.write_reg(reg, v);
        }
        self.kick = true;
        self.update(ctx);
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        OtgFs::update(self, ctx);
    }

    fn next_event(&self) -> Option<u64> {
        OtgFs::next_event(self)
    }

    fn reset(&mut self) {
        self.detach();
        self.reset_registers();
        self.warned_clock = false;
    }
}

/// CN13's device on the computer's USB: the OTG FS peripheral of the
/// board's machine, forwarded (USB/IP). Lives on the host's thread with the
/// machine; plugs the cable in while it exists.
pub struct OtgUsbDevice {
    port: SharedOtgPort,
    bridge: SharedBridge,
    device: ForwardedDevice,
}

impl OtgUsbDevice {
    pub fn new(port: SharedOtgPort, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        port.borrow_mut().cable = true;
        let bridge = Arc::new(Mutex::new(Default::default()));
        let device = ForwardedDevice::new("OTG FS (CN13)", Arc::clone(&bridge), wake);
        Self { port, bridge, device }
    }

    /// The bus id it is exported under, while the firmware is on the bus.
    pub fn busid(&self) -> Option<&str> {
        self.device.busid()
    }

    /// Exports or withdraws the device as the firmware connects and
    /// disconnects, and serves the computer's transfers. Returns the bus id
    /// when it has just been exported.
    pub fn poll(&mut self, server: &mut UsbipServer) -> Option<String> {
        self.sync();
        let exported = self.device.poll(server);
        self.sync();
        exported
    }

    /// Connects to the OTG FS of another board (after a reset or new firmware).
    pub fn set_port(&mut self, port: SharedOtgPort) {
        if !Rc::ptr_eq(&port, &self.port) {
            self.port.borrow_mut().cable = false;
            port.borrow_mut().cable = true;
            self.port = port;
        }
    }

    fn sync(&mut self) {
        self.bridge.lock().unwrap().sync(&mut self.port.borrow_mut().link);
    }

    /// Withdraws the device and pulls the cable.
    pub fn unplug(mut self, server: &mut UsbipServer) {
        self.device.unplug(server);
        self.sync();
        self.port.borrow_mut().cable = false;
    }
}
