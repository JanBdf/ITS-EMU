//! Ethernet MAC with its DMA (normal descriptors, ring or chain mode) and
//! the board's LAN8742A PHY (RMII, address 0) behind the MDIO interface.
//!
//! Frames go to and come from an [`EthLink`] (the cable): the DMA sends a
//! frame as soon as its descriptors are handed over (poll demand or start)
//! and receives a frame from the link as soon as it is there, without wire
//! time. Transmission inserts the checksums the descriptor asks for (CIC);
//! reception appends the FCS, filters by destination address (perfect
//! filter on MAC address 0, broadcast, PAM, promiscuous; no hash filter)
//! and reports no checksum errors. Not modelled: MMC counters, PTP, power
//! management, flow control, VLAN filtering, enhanced descriptors' extra
//! status words, and the DMA's arbitration (it moves a whole frame at once).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use emu_core::bus::{extract_read, merge_write};
use emu_core::{Bus, BusMaster, Clock, DeviceCtx, Peripheral, Ram};

use crate::rcc::SharedClocks;

pub const ETH_IRQ: u32 = 61;

/// The cable: frames the MAC sent (without FCS), frames for the MAC, and
/// whether the other end is connected (link up).
pub struct EthLink {
    pub sent: VecDeque<Vec<u8>>,
    pub to_mac: VecDeque<Vec<u8>>,
    pub link_up: bool,
}

impl Default for EthLink {
    fn default() -> Self {
        Self { sent: VecDeque::new(), to_mac: VecDeque::new(), link_up: true }
    }
}

pub type SharedEthLink = Rc<RefCell<EthLink>>;

/// Frames kept in [`EthLink::sent`] while nothing takes them (no network:
/// the oldest are dropped).
pub const SENT_KEPT: usize = 256;

const MACCR_RE: u32 = 1 << 2;
const MACCR_TE: u32 = 1 << 3;
const MACFFR_PM: u32 = 1 << 0;
const MACFFR_PAM: u32 = 1 << 4;
const MACFFR_RA: u32 = 1 << 31;
const MIIAR_MB: u32 = 1 << 0;
const MIIAR_MW: u32 = 1 << 1;
const DMABMR_SR: u32 = 1 << 0;
const DMAOMR_SR: u32 = 1 << 1;
const DMAOMR_ST: u32 = 1 << 13;
const DMAOMR_FTF: u32 = 1 << 20;
const DMASR_TS: u32 = 1 << 0;
const DMASR_TBUS: u32 = 1 << 2;
const DMASR_RS: u32 = 1 << 6;
const DMASR_RBUS: u32 = 1 << 7;
const DMASR_AIS: u32 = 1 << 15;
const DMASR_NIS: u32 = 1 << 16;
/// Normal and abnormal summary sources (DMASR / DMAIER bits).
const NORMAL: u32 = DMASR_TS | DMASR_TBUS | DMASR_RS | 1 << 14;
const ABNORMAL: u32 = 0x3FBE & !NORMAL;
const OWN: u32 = 1 << 31;

/// LAN8742A registers (those the HAL and lwIP ports use).
struct Phy {
    bcr: u16,
    anar: u16,
}

impl Phy {
    fn new() -> Self {
        Self { bcr: 0x3000, anar: 0x01E1 }
    }

    fn read(&self, reg: u32, link: bool) -> u16 {
        match reg {
            0 => self.bcr,
            // 100BASE-TX/10BASE-T full and half duplex, auto-negotiation,
            // extended capabilities; link and negotiation complete
            1 => 0x7809 | if link { 0x0024 } else { 0 },
            2 => 0x0007,
            3 => 0xC131,
            4 => self.anar,
            5 => {
                if link {
                    0x45E1
                } else {
                    0
                }
            }
            6 | 17 => 0x0002,
            18 => 0x60E0, // mode: all capable, PHY address 0
            27 => 0x000B,
            // special control/status: auto-negotiation done, 100BASE-TX full duplex
            31 => 0x0040 | if link { 0x1018 } else { 0 },
            _ => 0,
        }
    }

    fn write(&mut self, reg: u32, value: u16) {
        match reg {
            // soft reset and restart of auto-negotiation complete at once
            0 => self.bcr = if value & 0x8000 != 0 { 0x3000 } else { value & !0x0200 },
            4 => self.anar = value,
            _ => {}
        }
    }
}

struct EthState {
    mac: [u32; 0x60 / 4],
    /// MMC (0x100..) and PTP (0x700..) registers only store values.
    other: std::collections::HashMap<u32, u32>,
    phy: Phy,
    bmr: u32,
    rdlar: u32,
    tdlar: u32,
    sr: u32,
    omr: u32,
    ier: u32,
    /// Current descriptors and buffers.
    chtdr: u32,
    chrdr: u32,
    chtbar: u32,
    chrbar: u32,
    /// Missed frames (no descriptor), for DMAMFBOCR.
    missed: u32,
    /// Transmission was demanded (start or poll demand) and not suspended.
    tx_poll: bool,
    /// Reception is suspended for want of descriptors (until a poll demand).
    rx_suspended: bool,
}

impl EthState {
    fn new() -> Self {
        let mut s = Self {
            mac: [0; 0x60 / 4],
            other: Default::default(),
            phy: Phy::new(),
            bmr: 0x0002_0100, // SR (set at reset) clears once the reset is done: at once
            rdlar: 0,
            tdlar: 0,
            sr: 0,
            omr: 0,
            ier: 0,
            chtdr: 0,
            chrdr: 0,
            chtbar: 0,
            chrbar: 0,
            missed: 0,
            tx_poll: false,
            rx_suspended: false,
        };
        s.reset_mac();
        s
    }

    fn reset_mac(&mut self) {
        self.mac = [0; 0x60 / 4];
        self.mac[0] = 0x0000_8000; // MACCR: RD... reset value
        self.mac[0x40 / 4] = 0x8000_FFFF;
        self.mac[0x44 / 4] = 0xFFFF_FFFF;
        for i in [0x48, 0x50, 0x58] {
            self.mac[i / 4] = 0x0000_FFFF;
            self.mac[i / 4 + 1] = 0xFFFF_FFFF;
        }
    }

    fn maccr(&self) -> u32 {
        self.mac[0]
    }

    fn address0(&self) -> [u8; 6] {
        let (hi, lo) = (self.mac[0x40 / 4], self.mac[0x44 / 4]);
        let mut a = [0u8; 6];
        a[..4].copy_from_slice(&lo.to_le_bytes());
        a[4..].copy_from_slice(&hi.to_le_bytes()[..2]);
        a
    }

    /// DMASR with the summary bits and the process states.
    fn status(&self) -> u32 {
        let mut sr = self.sr & 0x3FFF & !(DMASR_AIS | DMASR_NIS);
        if sr & self.ier & NORMAL != 0 {
            sr |= DMASR_NIS;
        }
        if sr & self.ier & ABNORMAL != 0 {
            sr |= DMASR_AIS;
        }
        // RPS: stopped, waiting (running) or suspended; TPS: stopped or suspended
        let rps = match (self.omr & DMAOMR_SR != 0, self.rx_suspended) {
            (false, _) => 0,
            (true, false) => 3,
            (true, true) => 4,
        };
        let tps = if self.omr & DMAOMR_ST != 0 { 6 } else { 0 };
        sr | rps << 17 | tps << 20 | (self.sr & 0x1C00_0000)
    }

    fn irq(&self) -> bool {
        let sr = self.status();
        sr & self.ier & (DMASR_NIS | DMASR_AIS) != 0
    }

    /// The descriptor after `desc` (chained by DES3 or the ring's next).
    fn next_desc(&self, ram: &Ram, desc: u32, chained: bool, end_of_ring: bool, base: u32) -> u32 {
        if chained {
            ram.read(desc + 12, 4).unwrap_or(0)
        } else if end_of_ring {
            base
        } else {
            let skip = (self.bmr >> 2) & 0x1F;
            desc + 16 + 4 * skip
        }
    }

    /// Sends the frames whose descriptors the DMA owns; stops (suspended,
    /// TBUS) at the first descriptor it does not own.
    fn transmit(&mut self, ram: &mut Ram, link: &SharedEthLink) {
        let mut frame: Vec<u8> = Vec::new();
        let mut first_tdes0 = 0;
        loop {
            let desc = self.chtdr;
            let Some(tdes0) = ram.read(desc, 4) else {
                self.sr |= 1 << 13; // FBES: bus error
                self.tx_poll = false;
                return;
            };
            if tdes0 & OWN == 0 {
                self.sr |= DMASR_TBUS;
                self.tx_poll = false;
                return;
            }
            let tdes1 = ram.read(desc + 4, 4).unwrap_or(0);
            let (b1, b2) = (ram.read(desc + 8, 4).unwrap_or(0), ram.read(desc + 12, 4).unwrap_or(0));
            let chained = tdes0 & (1 << 20) != 0;
            if tdes0 & (1 << 28) != 0 {
                frame.clear();
                first_tdes0 = tdes0;
            }
            // buffers in RAM, or anywhere the DMA reads (flash)
            let mut take = |addr: u32, len: u32| {
                let bytes = ram
                    .writable_bytes(addr, len)
                    .map(<[u8]>::to_vec)
                    .or_else(|| (0..len).map(|i| ram.read(addr + i, 1).map(|b| b as u8)).collect::<Option<Vec<u8>>>());
                if let Some(bytes) = bytes {
                    frame.extend_from_slice(&bytes);
                }
            };
            self.chtbar = b1;
            take(b1, tdes1 & 0x1FFF);
            if !chained {
                take(b2, (tdes1 >> 16) & 0x1FFF);
            }
            // hand the descriptor back: no errors
            ram.write(desc, 4, tdes0 & !OWN & !0xFFFF);
            let last = tdes0 & (1 << 29) != 0;
            if last {
                insert_checksums(&mut frame, (first_tdes0 >> 22) & 3);
                if self.maccr() & MACCR_TE != 0 && link.borrow().link_up {
                    let mut link = link.borrow_mut();
                    if link.sent.len() == SENT_KEPT {
                        link.sent.pop_front();
                    }
                    link.sent.push_back(std::mem::take(&mut frame));
                }
                if tdes0 & (1 << 30) != 0 {
                    self.sr |= DMASR_TS;
                }
            }
            self.chtdr = self.next_desc(ram, desc, chained, tdes0 & (1 << 21) != 0, self.tdlar);
        }
    }

    /// Whether the MAC takes a frame for `dest`.
    fn accepts(&self, dest: &[u8]) -> bool {
        let ffr = self.mac[1];
        if ffr & (MACFFR_PM | MACFFR_RA) != 0 {
            return true;
        }
        if dest == [0xFF; 6] {
            return ffr & (1 << 5) == 0; // BFD: broadcast frames disabled
        }
        if dest[0] & 1 != 0 {
            return ffr & MACFFR_PAM != 0;
        }
        dest == self.address0()
    }

    /// Writes the next frame from the link into the descriptors the DMA
    /// owns; returns false if it has to wait (no descriptor).
    fn receive(&mut self, ram: &mut Ram, link: &SharedEthLink) -> bool {
        let Some(frame) = link.borrow().to_mac.front().cloned() else { return false };
        if frame.len() < 14 || !self.accepts(&frame[..6]) {
            link.borrow_mut().to_mac.pop_front();
            return true;
        }
        let first = self.chrdr;
        if ram.read(first, 4).is_none_or(|d| d & OWN == 0) {
            self.sr |= DMASR_RBUS;
            self.rx_suspended = true;
            return false;
        }
        link.borrow_mut().to_mac.pop_front();
        let mut data = frame.clone();
        // short frames are padded to 60 bytes on the wire, then the FCS
        data.resize(data.len().max(60), 0);
        data.extend_from_slice(&crc32(&data).to_le_bytes());
        let total = data.len() as u32;
        let mut done = 0usize;
        let mut desc = first;
        loop {
            let rdes0 = ram.read(desc, 4).unwrap_or(0);
            if rdes0 & OWN == 0 {
                // ran out of descriptors in the middle of the frame: dropped
                self.sr |= DMASR_RBUS;
                self.rx_suspended = true;
                self.missed += 1;
                return false;
            }
            let rdes1 = ram.read(desc + 4, 4).unwrap_or(0);
            let (b1, b2) = (ram.read(desc + 8, 4).unwrap_or(0), ram.read(desc + 12, 4).unwrap_or(0));
            let chained = rdes1 & (1 << 14) != 0;
            let mut room = vec![(b1, rdes1 & 0x1FFF)];
            if !chained {
                room.push((b2, (rdes1 >> 16) & 0x1FFF));
            }
            for (addr, size) in room {
                let n = (size as usize).min(data.len() - done);
                for (i, &b) in data[done..done + n].iter().enumerate() {
                    ram.write(addr + i as u32, 1, u32::from(b));
                }
                if n > 0 {
                    self.chrbar = addr;
                }
                done += n;
            }
            let last = done == data.len();
            let mut status = 0;
            if desc == first {
                status |= 1 << 9; // FS
            }
            if last {
                // LS, frame length, frame type (Ethernet II)
                status |= 1 << 8 | total << 16;
                if u16::from_be_bytes([frame[12], frame[13]]) >= 0x0600 {
                    status |= 1 << 5;
                }
            }
            ram.write(desc, 4, status);
            self.chrdr = self.next_desc(ram, desc, chained, rdes1 & (1 << 15) != 0, self.rdlar);
            desc = self.chrdr;
            if last {
                if rdes1 & (1 << 31) == 0 {
                    self.sr |= DMASR_RS;
                }
                return true;
            }
        }
    }
}

/// Inserts IPv4 header and TCP/UDP/ICMP checksums as the transmit
/// descriptor's CIC field asks (1: header, 2: + payload without the
/// pseudo-header, 3: + payload with it).
fn insert_checksums(frame: &mut [u8], cic: u32) {
    if cic == 0 || frame.len() < 34 || frame[12..14] != [0x08, 0x00] {
        return;
    }
    let ip = &mut frame[14..];
    let ihl = usize::from(ip[0] & 0xF) * 4;
    let total = usize::from(u16::from_be_bytes([ip[2], ip[3]])).min(ip.len());
    if ip[0] >> 4 != 4 || ihl < 20 || total < ihl {
        return;
    }
    ip[10..12].fill(0);
    let sum = checksum(&ip[..ihl], 0);
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    let fragment = u16::from_be_bytes([ip[6], ip[7]]) & 0x3FFF != 0;
    if cic < 2 || fragment {
        return;
    }
    let proto = ip[9];
    let (src, dst) = ([ip[12], ip[13], ip[14], ip[15]], [ip[16], ip[17], ip[18], ip[19]]);
    let payload = &mut ip[ihl..total];
    let at = match proto {
        1 => 2,
        6 => 16,
        17 => 6,
        _ => return,
    };
    if payload.len() < at + 2 {
        return;
    }
    payload[at..at + 2].fill(0);
    let pseudo = if proto == 1 {
        0
    } else {
        let mut p = Vec::with_capacity(12);
        p.extend_from_slice(&src);
        p.extend_from_slice(&dst);
        p.extend_from_slice(&[0, proto]);
        p.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        sum_words(&p)
    };
    let mut sum = checksum(payload, pseudo);
    if proto == 17 && sum == 0 {
        sum = 0xFFFF;
    }
    payload[at..at + 2].copy_from_slice(&sum.to_be_bytes());
}

fn sum_words(data: &[u8]) -> u32 {
    data.chunks(2).map(|c| u32::from(u16::from_be_bytes([c[0], c.get(1).copied().unwrap_or(0)]))).sum()
}

/// Internet checksum of `data`, starting from `initial` (an unfolded sum).
pub fn checksum(data: &[u8], initial: u32) -> u16 {
    let mut sum = initial + sum_words(data);
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// The Ethernet FCS (CRC-32, reflected).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// MAC and DMA state shared by the register interface and the bus master.
pub struct Eth {
    state: RefCell<EthState>,
    link: SharedEthLink,
}

pub type SharedEth = Rc<Eth>;

impl Eth {
    pub fn new(link: SharedEthLink) -> SharedEth {
        Rc::new(Self { state: RefCell::new(EthState::new()), link })
    }

    /// Something for the DMA to do: frames to send, or a frame from the link
    /// and a reception that is not suspended.
    fn busy(&self) -> bool {
        let s = self.state.borrow();
        let tx = s.tx_poll && s.omr & DMAOMR_ST != 0;
        let rx = s.omr & DMAOMR_SR != 0
            && !s.rx_suspended
            && s.maccr() & MACCR_RE != 0
            && !self.link.borrow().to_mac.is_empty();
        tx || rx
    }
}

/// The Ethernet DMA as a bus master.
pub struct EthMaster(pub SharedEth);

impl BusMaster for EthMaster {
    fn wants_service(&self) -> bool {
        self.0.busy()
    }

    fn service(&mut self, ctx: &mut DeviceCtx, _bus: &mut Bus, ram: &mut Ram) {
        let mut s = self.0.state.borrow_mut();
        if s.tx_poll && s.omr & DMAOMR_ST != 0 {
            s.transmit(ram, &self.0.link);
        }
        while s.omr & DMAOMR_SR != 0 && !s.rx_suspended && s.maccr() & MACCR_RE != 0 {
            if !s.receive(ram, &self.0.link) {
                break;
            }
        }
        ctx.set_irq(ETH_IRQ, s.irq());
    }
}

/// The register interface (MAC at 0x000, MMC 0x100, PTP 0x700, DMA 0x1000).
pub struct EthRegs {
    eth: SharedEth,
    clocks: SharedClocks,
}

impl EthRegs {
    pub fn new(eth: SharedEth, clocks: SharedClocks) -> Self {
        Self { eth, clocks }
    }

    fn clock_enabled(&self) -> bool {
        self.clocks.get().ahb1enr & (1 << 25) != 0
    }

    fn reg(&self, off: u32) -> u32 {
        let s = self.eth.state.borrow();
        match off {
            0x38 => 0,
            0x00..=0x5C => s.mac[(off / 4) as usize],
            0x1000 => s.bmr,
            0x100C => s.rdlar,
            0x1010 => s.tdlar,
            0x1014 => s.status(),
            0x1018 => s.omr,
            0x101C => s.ier,
            0x1020 => s.missed.min(0xFFFF),
            0x1048 => s.chtdr,
            0x104C => s.chrdr,
            0x1050 => s.chtbar,
            0x1054 => s.chrbar,
            _ => s.other.get(&off).copied().unwrap_or(0),
        }
    }
}

impl Peripheral for EthRegs {
    fn name(&self) -> &str {
        "ETH"
    }

    fn read_stable_until(&self, off: u32, _clock: &Clock) -> u64 {
        // frames arrive from outside between runs; DMASR and the current
        // descriptors change only then and on writes
        match off & !3 {
            0x1014 | 0x104C | 0x1054 if self.eth.busy() => 0,
            _ => u64::MAX,
        }
    }

    fn write_is_inert(&self, off: u32) -> bool {
        // the LCD driver switches the MAC off and on around SPI transfers:
        // nothing happens while the DMA is stopped
        off & !3 == 0 && self.eth.state.borrow().omr & (DMAOMR_SR | DMAOMR_ST) == 0
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        if !self.clock_enabled() {
            return 0;
        }
        let v = self.reg(off & !3);
        extract_read(v, off, size)
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        if !self.clock_enabled() {
            return;
        }
        let reg = off & !3;
        let v = merge_write(self.reg(reg), off, size, value);
        let link = self.eth.link.borrow().link_up;
        let mut s = self.eth.state.borrow_mut();
        match reg {
            0x10 => {
                s.mac[4] = v & 0xFFDF;
                if v & MIIAR_MB != 0 {
                    // the MDIO transaction completes at once
                    s.mac[4] &= !MIIAR_MB;
                    let (pa, mr) = ((v >> 11) & 0x1F, (v >> 6) & 0x1F);
                    if pa == 0 {
                        if v & MIIAR_MW != 0 {
                            let data = s.mac[5] as u16;
                            s.phy.write(mr, data);
                        } else {
                            s.mac[5] = u32::from(s.phy.read(mr, link));
                        }
                    } else if v & MIIAR_MW == 0 {
                        s.mac[5] = 0xFFFF; // nobody answers
                    }
                }
            }
            0x14 => s.mac[5] = v & 0xFFFF,
            0x38 => {}
            0x40 => s.mac[0x10] = (v & 0xFFFF) | 0x8000_0000,
            0x00..=0x5C => s.mac[(reg / 4) as usize] = v,
            0x1000 => {
                if v & DMABMR_SR != 0 {
                    // software reset: MAC and DMA registers, done at once
                    let phy = std::mem::replace(&mut s.phy, Phy::new());
                    *s = EthState::new();
                    s.phy = phy;
                } else {
                    s.bmr = v & 0x07FF_FFFE;
                }
            }
            0x1004 => {
                if s.omr & DMAOMR_ST != 0 {
                    s.tx_poll = true;
                }
            }
            0x1008 => s.rx_suspended = false,
            0x100C => {
                s.rdlar = v & !3;
                s.chrdr = s.rdlar;
            }
            0x1010 => {
                s.tdlar = v & !3;
                s.chtdr = s.tdlar;
            }
            0x1014 => s.sr &= !(v & 0x0001_E7FF),
            0x1018 => {
                let starting_tx = s.omr & DMAOMR_ST == 0 && v & DMAOMR_ST != 0;
                let starting_rx = s.omr & DMAOMR_SR == 0 && v & DMAOMR_SR != 0;
                s.omr = v & 0x0731_E0DE & !DMAOMR_FTF; // the flush is done at once
                if starting_tx {
                    s.tx_poll = true;
                }
                if starting_rx {
                    s.rx_suspended = false;
                }
            }
            0x101C => s.ier = v & 0x0001_E7FF,
            _ => {
                s.other.insert(reg, v);
            }
        }
        let irq = s.irq();
        drop(s);
        ctx.set_irq(ETH_IRQ, irq);
    }

    fn next_event(&self) -> Option<u64> {
        // the DMA works when serviced after an access or an update; a frame
        // from the link needs an update soon
        self.eth.busy().then_some(0)
    }

    fn update(&mut self, _ctx: &mut DeviceCtx) {}

    fn reset(&mut self) {
        let mut s = self.eth.state.borrow_mut();
        *s = EthState::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fcs_and_internet_checksum() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        // RFC 1071 example
        let data = [0x00, 0x01, 0xF2, 0x03, 0xF4, 0xF5, 0xF6, 0xF7];
        assert_eq!(checksum(&data, 0), !0xDDF2);
    }

    #[test]
    fn checksums_inserted_into_an_icmp_echo() {
        let mut f = vec![0u8; 14 + 20 + 8];
        f[12..14].copy_from_slice(&[0x08, 0x00]);
        let ip = &mut f[14..];
        ip[0] = 0x45;
        ip[2..4].copy_from_slice(&28u16.to_be_bytes());
        ip[8] = 64;
        ip[9] = 1;
        ip[12..16].copy_from_slice(&[192, 168, 33, 99]);
        ip[16..20].copy_from_slice(&[192, 168, 33, 1]);
        ip[20] = 8; // echo request
        insert_checksums(&mut f, 3);
        assert_eq!(checksum(&f[14..34], 0), 0, "IP header sums to 0");
        assert_eq!(checksum(&f[34..42], 0), 0, "ICMP sums to 0");
    }
}
