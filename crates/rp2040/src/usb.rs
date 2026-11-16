//! USB controller in device mode (USBCTRL_REGS + DPRAM) with the host side
//! built in: once the firmware connects its pull-up, a minimal host resets
//! the bus, enumerates the device, configures its CDC ACM interface (line
//! coding, DTR) and then moves the serial data between the bulk endpoints
//! and a byte pipe ([`SerialPipe`]).
//!
//! Transfers complete without bus timing: the host looks at the buffer
//! control registers in DPRAM when the firmware arms a buffer and
//! periodically while it has data to send.
//!
//! A host on the computer (USB/IP) can take over once the built-in host has
//! enumerated the device ([`SerialPipe::remote`]): its transfers
//! ([`SerialPipe::requests`]) are then played into the endpoint buffers
//! as they are, and the built-in serial pump pauses.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use emu_core::{Clock, DeviceCtx, Peripheral};

use crate::regs::{Block, Op};

pub const USBCTRL_IRQ: u32 = 5;

const AVAILABLE: u32 = 1 << 10;
const STALL: u32 = 1 << 11;
const FULL: u32 = 1 << 15;
const LEN_MASK: u32 = 0x3FF;
const EP0_BUF: usize = 0x100;

const SIE_VBUS_DETECTED: u32 = 1 << 0;
const SIE_CONNECTED: u32 = 1 << 16;
const SIE_SETUP_REC: u32 = 1 << 17;
const SIE_BUS_RESET: u32 = 1 << 19;
/// SIE_STATUS bits that writes of 1 clear.
const SIE_WC: u32 = 0xFF0E_0810 | SIE_SETUP_REC | SIE_BUS_RESET;

/// Serial data between the device's CDC interface and the computer, and
/// the transfers of a host on the computer that drives the device itself.
#[derive(Default, Debug)]
pub struct SerialPipe {
    /// Bytes from the computer, not yet taken by the device.
    pub to_device: VecDeque<u8>,
    /// Bytes the device sent.
    pub from_device: Vec<u8>,
    /// The CDC interface is configured and DTR is set.
    pub open: bool,
    /// Device and configuration descriptor, once the built-in host has
    /// enumerated the device (`None` again when it disconnects).
    pub descriptors: Option<(Vec<u8>, Vec<u8>)>,
    /// A host on the computer drives the device: [`Self::requests`] are
    /// carried out and the serial pump above pauses.
    pub remote: bool,
    /// Transfers of the computer's host, in order per endpoint.
    pub requests: VecDeque<Transfer>,
    /// Finished transfers: id and result (`None`: stalled).
    pub done: Vec<(u64, Option<Vec<u8>>)>,
}

/// A transfer of a host on the computer.
#[derive(Debug, Clone)]
pub struct Transfer {
    pub id: u64,
    /// Endpoint number (without direction bit).
    pub ep: u8,
    pub dir_in: bool,
    /// The SETUP packet (endpoint 0).
    pub setup: [u8; 8],
    /// OUT data.
    pub data: Vec<u8>,
    /// IN: at most this many bytes.
    pub max_len: usize,
}

pub type SharedSerial = Rc<RefCell<SerialPipe>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    GetDevice,
    SetAddress,
    GetConfigHeader,
    GetConfig,
    SetConfiguration,
    SetLineCoding,
    SetControlLineState,
    /// A control transfer of the computer's host.
    Remote(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Waiting for the IN data (`got` bytes so far).
    DataIn,
    /// Sending the OUT data.
    DataOut,
    /// Waiting for the device's zero-length IN packet.
    StatusIn,
    /// Sending the zero-length OUT packet.
    StatusOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Host {
    /// Waiting for the device's pull-up.
    Detached,
    /// Bus reset signalled; the first request follows at the cycle.
    Reset {
        until: u64,
    },
    Control {
        step: Step,
        stage: Stage,
        data: Vec<u8>,
        sent: usize,
    },
    /// Enumeration done (or failed).
    Configured,
}

/// CDC endpoints found in the configuration descriptor.
#[derive(Debug, Default, Clone, Copy)]
struct Cdc {
    comm_iface: u8,
    bulk_in: u8,
    bulk_out: u8,
}

pub struct UsbCore {
    dpram: Box<[u8; 4096]>,
    addr_endp: u32,
    main_ctrl: u32,
    sie_ctrl: u32,
    sie_status: u32,
    buff_status: u32,
    ep_abort: u32,
    ep_abort_done: u32,
    other: [u32; 0x9C / 4],
    inte: u32,
    intf: u32,
    sof_pending: bool,
    host: Host,
    device_desc: Vec<u8>,
    config_desc: Vec<u8>,
    cdc: Option<Cdc>,
    /// Time of the next look at the endpoints.
    poll_at: Option<u64>,
    /// Cycle of the last update (for scheduling from `&self`).
    last_now: u64,
    pipe: SharedSerial,
    /// Highest id of the computer's transfers seen so far.
    seen_request: u64,
    /// OUT bytes of the computer's transfer at the head of each endpoint's
    /// queue already given to the device.
    out_sent: [usize; 16],
}

/// Time between two looks at the endpoints while the host waits for something.
const POLL_NS: u64 = 50_000;
/// Delay from connection to bus reset, and length of the reset.
const ATTACH_NS: u64 = 2_000_000;
const RESET_NS: u64 = 2_000_000;

impl UsbCore {
    fn new(pipe: SharedSerial) -> Self {
        Self {
            dpram: Box::new([0; 4096]),
            addr_endp: 0,
            main_ctrl: 0,
            sie_ctrl: 0,
            sie_status: 0,
            buff_status: 0,
            ep_abort: 0,
            ep_abort_done: 0,
            other: [0; 0x9C / 4],
            inte: 0,
            intf: 0,
            sof_pending: false,
            host: Host::Detached,
            device_desc: Vec::new(),
            config_desc: Vec::new(),
            cdc: None,
            poll_at: None,
            last_now: 0,
            pipe,
            seen_request: 0,
            out_sent: [0; 16],
        }
    }

    fn word(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.dpram[off..off + 4].try_into().unwrap())
    }

    fn set_word(&mut self, off: usize, v: u32) {
        self.dpram[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn intr(&self) -> u32 {
        let s = self.sie_status;
        let mut v = 0;
        if s & (1 << 18) != 0 {
            v |= 1 << 3; // TRANS_COMPLETE
        }
        if self.buff_status != 0 {
            v |= 1 << 4;
        }
        if s & SIE_BUS_RESET != 0 {
            v |= 1 << 12;
        }
        if s & (1 << 4) != 0 {
            v |= 1 << 14; // DEV_SUSPEND
        }
        if s & (1 << 11) != 0 {
            v |= 1 << 15; // DEV_RESUME_FROM_HOST
        }
        if s & SIE_SETUP_REC != 0 {
            v |= 1 << 16;
        }
        if self.sof_pending {
            v |= 1 << 17;
        }
        if self.ep_abort_done != 0 {
            v |= 1 << 18;
        }
        v
    }

    fn sync_irq(&self, ctx: &mut DeviceCtx) {
        ctx.set_irq(USBCTRL_IRQ, (self.intr() | self.intf) & self.inte != 0);
    }

    fn connected(&self) -> bool {
        self.main_ctrl & 1 != 0 && self.sie_ctrl & (1 << 16) != 0
    }

    fn schedule(&mut self, at: u64) {
        self.poll_at = Some(self.poll_at.map_or(at, |p| p.min(at)));
    }

    fn send_setup(&mut self, setup: [u8; 8]) {
        self.dpram[..8].copy_from_slice(&setup);
        self.sie_status |= SIE_SETUP_REC;
        // a SETUP packet clears EP_STALL_ARM
        self.other[0x68 / 4] = 0;
    }

    fn request(&mut self, step: Step) {
        let comm = self.cdc.map_or(0, |c| u16::from(c.comm_iface));
        let total = self.config_desc.get(2..4).map_or(9, |b| u16::from_le_bytes([b[0], b[1]]));
        let (setup, stage, data): ([u8; 8], Stage, Vec<u8>) = match step {
            Step::GetDevice => (setup(0x80, 6, 0x0100, 0, 64), Stage::DataIn, Vec::new()),
            Step::SetAddress => (setup(0x00, 5, 1, 0, 0), Stage::StatusIn, Vec::new()),
            Step::GetConfigHeader => (setup(0x80, 6, 0x0200, 0, 9), Stage::DataIn, Vec::new()),
            Step::GetConfig => (setup(0x80, 6, 0x0200, 0, total), Stage::DataIn, Vec::new()),
            Step::SetConfiguration => (setup(0x00, 9, 1, 0, 0), Stage::StatusIn, Vec::new()),
            Step::SetLineCoding => {
                let coding = [0x00, 0xC2, 0x01, 0x00, 0, 0, 8]; // 115200 8N1
                (setup(0x21, 0x20, 0, comm, 7), Stage::DataOut, coding.to_vec())
            }
            Step::SetControlLineState => (setup(0x21, 0x22, 0x3, comm, 0), Stage::StatusIn, Vec::new()),
            Step::Remote(_) => unreachable!("started by start_remote_control"),
        };
        self.send_setup(setup);
        self.host = Host::Control { step, stage, data, sent: 0 };
    }

    /// A control request finished: the next one.
    fn completed(&mut self, step: Step, data: Vec<u8>) {
        let next = match step {
            Step::GetDevice => {
                self.device_desc = data;
                Some(Step::SetAddress)
            }
            Step::SetAddress => Some(Step::GetConfigHeader),
            Step::GetConfigHeader => {
                self.config_desc = data;
                Some(Step::GetConfig)
            }
            Step::GetConfig => {
                log::debug!("Pico: USB device {:02x?}, configuration {data:02x?}", self.device_desc);
                self.cdc = find_cdc(&data);
                self.pipe.borrow_mut().descriptors = Some((self.device_desc.clone(), data.clone()));
                self.config_desc = data;
                Some(Step::SetConfiguration)
            }
            Step::SetConfiguration if self.cdc.is_some() => Some(Step::SetLineCoding),
            Step::SetConfiguration => {
                log::warn!("Pico: the USB device has no CDC serial interface");
                None
            }
            Step::SetLineCoding => Some(Step::SetControlLineState),
            Step::SetControlLineState => {
                log::info!("Pico: USB serial port configured");
                self.pipe.borrow_mut().open = true;
                None
            }
            Step::Remote(id) => {
                self.pipe.borrow_mut().done.push((id, Some(data)));
                None
            }
        };
        match next {
            Some(s) => self.request(s),
            None => self.host = Host::Configured,
        }
    }

    /// Takes the data of an armed IN buffer of endpoint `ep` (buffer
    /// control at `bc_off`, data at `buf`); `None` if none is armed.
    fn take_in(&mut self, bc_off: usize, buf: usize, half: u32) -> Option<Vec<u8>> {
        let bc = self.word(bc_off);
        let b = bc >> half;
        if b & STALL != 0 && half == 0 && b & AVAILABLE != 0 {
            return None;
        }
        if b & AVAILABLE == 0 || b & FULL == 0 {
            return None;
        }
        let len = (b & LEN_MASK) as usize;
        let data = self.dpram[buf..buf + len].to_vec();
        log::trace!("Pico USB IN bc@{bc_off:#x}={bc:#010x} buf@{buf:#x}: {data:02x?}");
        self.set_word(bc_off, bc & !((AVAILABLE | FULL) << half));
        Some(data)
    }

    /// Fills an armed OUT buffer; returns false if it is not armed.
    fn give_out(&mut self, bc_off: usize, buf: usize, half: u32, data: &[u8]) -> bool {
        let bc = self.word(bc_off);
        let b = bc >> half;
        if b & AVAILABLE == 0 || b & FULL != 0 {
            return false;
        }
        let n = data.len().min((b & LEN_MASK) as usize).min(64);
        log::trace!("Pico USB OUT bc@{bc_off:#x}={bc:#010x} buf@{buf:#x}: {:02x?}", &data[..n]);
        self.dpram[buf..buf + n].copy_from_slice(&data[..n]);
        let b = (b & !(LEN_MASK | AVAILABLE)) | n as u32 | FULL;
        let mask = 0xFFFF << half;
        self.set_word(bc_off, (bc & !mask) | ((b << half) & mask));
        true
    }

    fn ep0_stalled(&self) -> bool {
        (self.word(0x80) | self.word(0x84)) & STALL != 0 && self.other[0x68 / 4] != 0
    }

    /// The host looks at the bus.
    fn poll(&mut self, now: u64, clock: &Clock) {
        let ns = |n: u64| clock.cycle_at_ps(clock.time_ps() + n * 1000).max(now + 1);
        log::trace!(
            "Pico USB host at {now}: {:?}, SIE_STATUS {:#x}, BUFF_STATUS {:#x}",
            self.host,
            self.sie_status,
            self.buff_status
        );
        match self.host.clone() {
            Host::Detached => {
                if self.connected() {
                    self.sie_status |= SIE_BUS_RESET | SIE_CONNECTED;
                    self.addr_endp = 0;
                    self.host = Host::Reset { until: ns(RESET_NS) };
                    self.schedule(ns(RESET_NS));
                }
                return;
            }
            Host::Reset { until } => {
                if now >= until {
                    self.request(Step::GetDevice);
                } else {
                    self.schedule(until);
                    return;
                }
            }
            Host::Configured | Host::Control { .. } => {}
        }
        if !self.connected() {
            self.host = Host::Detached;
            self.disconnected();
            return;
        }
        // Control transfers on EP0 (buffer shared by IN and OUT).
        while let Host::Control { step, stage, mut data, sent } = self.host.clone() {
            // A real host's next transaction comes long after the device has
            // handled the last one: wait for its SETUP and buffer interrupts.
            if self.sie_status & SIE_SETUP_REC != 0 || self.buff_status & 3 != 0 {
                break;
            }
            if self.ep0_stalled() {
                if let Step::Remote(id) = step {
                    self.pipe.borrow_mut().done.push((id, None));
                } else {
                    log::warn!("Pico: USB request {step:?} stalled");
                }
                self.host = Host::Configured;
                continue;
            }
            match stage {
                Stage::DataIn => {
                    let Some(chunk) = self.take_in(0x80, EP0_BUF, 0) else { break };
                    self.buff_status |= 1;
                    let short = chunk.len() < 64;
                    data.extend(chunk);
                    let want = u16::from_le_bytes([self.dpram[6], self.dpram[7]]) as usize;
                    let stage = if short || data.len() >= want { Stage::StatusOut } else { Stage::DataIn };
                    self.host = Host::Control { step, stage, data, sent };
                }
                Stage::DataOut => {
                    let end = (sent + 64).min(data.len());
                    if !self.give_out(0x84, EP0_BUF, 0, &data[sent..end]) {
                        break;
                    }
                    self.buff_status |= 2;
                    let stage = if end >= data.len() { Stage::StatusIn } else { Stage::DataOut };
                    self.host = Host::Control { step, stage, data, sent: end };
                }
                Stage::StatusIn => {
                    if self.take_in(0x80, EP0_BUF, 0).is_none() {
                        break;
                    }
                    self.buff_status |= 1;
                    self.completed(step, data);
                }
                Stage::StatusOut => {
                    if !self.give_out(0x84, EP0_BUF, 0, &[]) {
                        break;
                    }
                    self.buff_status |= 2;
                    self.completed(step, data);
                }
            }
        }
        if self.pipe.borrow().remote {
            if self.host == Host::Configured && self.start_remote_control() {
                // its SETUP packet is with the firmware now
                self.schedule(ns(POLL_NS));
                return;
            }
            self.remote_endpoints();
            if !self.pipe.borrow().requests.is_empty() {
                self.schedule(ns(POLL_NS));
            }
        } else if self.host == Host::Configured
            && let Some(cdc) = self.cdc
        {
            self.bulk(cdc);
        }
        if matches!(self.host, Host::Control { .. }) {
            self.schedule(ns(POLL_NS));
        }
    }

    /// The device went off the bus: the computer's host loses it.
    fn disconnected(&mut self) {
        let mut pipe = self.pipe.borrow_mut();
        pipe.open = false;
        pipe.descriptors = None;
        let failed: Vec<u64> = pipe.requests.drain(..).map(|t| t.id).collect();
        pipe.done.extend(failed.into_iter().map(|id| (id, None)));
        self.out_sent = [0; 16];
    }

    /// Starts the computer's first waiting control transfer; false if there is none.
    fn start_remote_control(&mut self) -> bool {
        let t = {
            let mut pipe = self.pipe.borrow_mut();
            let Some(i) = pipe.requests.iter().position(|t| t.ep == 0) else { return false };
            pipe.requests.remove(i).unwrap()
        };
        let length = u16::from_le_bytes([t.setup[6], t.setup[7]]);
        let stage = match (t.setup[0] & 0x80 != 0, length) {
            (_, 0) => Stage::StatusIn,
            (true, _) => Stage::DataIn,
            (false, _) => Stage::DataOut,
        };
        log::trace!("Pico USB: control transfer {:02x?} of the computer", t.setup);
        self.send_setup(t.setup);
        self.host = Host::Control { step: Step::Remote(t.id), stage, data: t.data, sent: 0 };
        true
    }

    /// Carries out the computer's transfers on endpoints other than 0: the
    /// first one of each endpoint and direction, as far as the firmware has
    /// buffers armed.
    fn remote_endpoints(&mut self) {
        let mut started: Vec<(u8, bool)> = Vec::new();
        let mut i = 0;
        loop {
            let Some(t) = self.pipe.borrow().requests.get(i).cloned() else { break };
            if t.ep == 0 || started.contains(&(t.ep, t.dir_in)) {
                i += 1;
                continue;
            }
            started.push((t.ep, t.dir_in));
            let result = if t.dir_in { self.remote_in(&t) } else { self.remote_out(&t) };
            match result {
                Some(data) => {
                    let mut pipe = self.pipe.borrow_mut();
                    pipe.requests.remove(i);
                    pipe.done.push((t.id, Some(data)));
                }
                None => i += 1,
            }
        }
    }

    /// The data of an armed IN buffer of `t.ep` (two with double buffering).
    fn remote_in(&mut self, t: &Transfer) -> Option<Vec<u8>> {
        let (bc, buf, double) = self.ep_buffer(t.ep, false)?;
        let bit = 1 << (2 * t.ep);
        if self.buff_status & bit != 0 {
            return None; // the firmware has not seen the last one yet
        }
        let mut data = self.take_in(bc, buf, 0)?;
        if double
            && data.len() == 64
            && t.max_len > 64
            && let Some(more) = self.take_in(bc, buf + 64, 16)
        {
            data.extend(more);
        }
        self.buff_status |= bit;
        Some(data)
    }

    /// Gives the OUT data of `t` to the armed buffers of `t.ep`, packet by
    /// packet; the transfer is done when all of it is with the device.
    fn remote_out(&mut self, t: &Transfer) -> Option<Vec<u8>> {
        let (bc, buf, double) = self.ep_buffer(t.ep, true)?;
        let bit = 1 << (2 * t.ep + 1);
        if self.buff_status & bit != 0 {
            return None;
        }
        let sent = &mut self.out_sent[usize::from(t.ep)];
        let start = *sent;
        let end = (start + 64).min(t.data.len());
        if !self.give_out(bc, buf, 0, &t.data[start..end]) {
            return None;
        }
        let mut sent_to = end;
        if double && end - start == 64 && end < t.data.len() {
            let end2 = (end + 64).min(t.data.len());
            if self.give_out(bc, buf + 64, 16, &t.data[end..end2]) {
                sent_to = end2;
            }
        }
        self.buff_status |= bit;
        if sent_to >= t.data.len() {
            self.out_sent[usize::from(t.ep)] = 0;
            Some(Vec::new())
        } else {
            self.out_sent[usize::from(t.ep)] = sent_to;
            None
        }
    }

    fn ep_buffer(&self, ep: u8, out: bool) -> Option<(usize, usize, bool)> {
        if ep == 0 {
            return None;
        }
        let ctrl = self.word(0x08 + (usize::from(ep) - 1) * 8 + if out { 4 } else { 0 });
        if ctrl & (1 << 31) == 0 {
            return None;
        }
        let bc = 0x80 + usize::from(ep) * 8 + if out { 4 } else { 0 };
        Some((bc, (ctrl & 0xFFC0) as usize, ctrl & (1 << 30) != 0))
    }

    /// Bulk transfers of the CDC data interface.
    fn bulk(&mut self, cdc: Cdc) {
        if let Some((bc, buf, double)) = self.ep_buffer(cdc.bulk_in, false)
            && self.buff_status & (1 << (2 * cdc.bulk_in)) == 0
        {
            let mut any = false;
            if let Some(d) = self.take_in(bc, buf, 0) {
                any = true;
                let full = d.len() == 64;
                self.pipe.borrow_mut().from_device.extend(d);
                if double
                    && full
                    && let Some(d) = self.take_in(bc, buf + 64, 16)
                {
                    self.pipe.borrow_mut().from_device.extend(d);
                }
            }
            if any {
                self.buff_status |= 1 << (2 * cdc.bulk_in);
            }
        }
        if let Some((bc, buf, double)) = self.ep_buffer(cdc.bulk_out, true)
            && self.buff_status & (1 << (2 * cdc.bulk_out + 1)) == 0
        {
            let chunk: Vec<u8> = self.pipe.borrow().to_device.iter().take(128).copied().collect();
            if !chunk.is_empty() {
                let first = chunk.len().min(64);
                if self.give_out(bc, buf, 0, &chunk[..first]) {
                    let mut taken = first;
                    if double && first == 64 && chunk.len() > 64 && self.give_out(bc, buf + 64, 16, &chunk[64..]) {
                        taken = chunk.len().min(128);
                    }
                    self.pipe.borrow_mut().to_device.drain(..taken);
                    self.buff_status |= 1 << (2 * cdc.bulk_out + 1);
                }
            }
        }
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        let now = ctx.now();
        self.last_now = now;
        let new_request = self.new_request();
        if new_request {
            self.seen_request = self.pipe.borrow().requests.iter().map(|t| t.id).max().unwrap_or(0);
        }
        if new_request || self.sending() || self.poll_at.is_some_and(|t| t <= now) {
            self.poll_at = None;
            self.poll(now, ctx.clock);
        }
        if self.poll_at.is_none() && self.host == Host::Detached && self.connected() {
            let at = ctx.clock.cycle_at_ps(ctx.clock.time_ps() + ATTACH_NS * 1000);
            self.schedule(at);
        }
        self.sync_irq(ctx);
    }

    /// The built-in host has serial data for the device.
    fn sending(&self) -> bool {
        let pipe = self.pipe.borrow();
        self.host == Host::Configured && !pipe.remote && !pipe.to_device.is_empty()
    }

    /// The computer's host has queued a transfer since the last look.
    fn new_request(&self) -> bool {
        self.pipe.borrow().requests.iter().any(|t| t.id > self.seen_request)
    }

    fn next_event(&self) -> Option<u64> {
        // the computer sent data or a request: look at the buffers soon
        let send_at = (self.sending() || self.new_request()).then_some(self.last_now + 1);
        [self.poll_at, send_at].into_iter().flatten().min()
    }
}

fn setup(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> [u8; 8] {
    let [v0, v1] = value.to_le_bytes();
    let [i0, i1] = index.to_le_bytes();
    let [l0, l1] = length.to_le_bytes();
    [request_type, request, v0, v1, i0, i1, l0, l1]
}

/// The CDC communication interface and the bulk endpoints of its data interface.
fn find_cdc(config: &[u8]) -> Option<Cdc> {
    let mut cdc = Cdc::default();
    let (mut comm, mut iface_class) = (None, 0u8);
    let mut i = 0;
    while i + 2 <= config.len() {
        let len = config[i] as usize;
        if len < 2 || i + len > config.len() {
            break;
        }
        let d = &config[i..i + len];
        match d[1] {
            4 if len >= 9 => {
                iface_class = d[5];
                if iface_class == 0x02 && comm.is_none() {
                    comm = Some(d[2]);
                }
            }
            5 if len >= 7 && iface_class == 0x0A && d[3] & 3 == 2 => {
                if d[2] & 0x80 != 0 {
                    cdc.bulk_in = d[2] & 0x0F;
                } else {
                    cdc.bulk_out = d[2] & 0x0F;
                }
            }
            _ => {}
        }
        i += len;
    }
    cdc.comm_iface = comm?;
    (cdc.bulk_in != 0 && cdc.bulk_out != 0).then_some(cdc)
}

pub type SharedUsb = Rc<RefCell<UsbCore>>;

/// USBCTRL_REGS.
pub struct UsbRegs(pub SharedUsb);

impl Block for UsbRegs {
    fn name(&self) -> &str {
        "USBCTRL_REGS"
    }

    fn read(&mut self, ctx: &mut DeviceCtx, off: u32) -> u32 {
        let mut u = self.0.borrow_mut();
        u.update(ctx);
        match off {
            0x00 => u.addr_endp,
            0x40 => u.main_ctrl,
            0x48 => {
                u.sof_pending = false;
                u.sync_irq(ctx);
                ((ctx.clock.time_ps() / 1_000_000_000) & 0x7FF) as u32
            }
            0x4C => u.sie_ctrl,
            0x50 => u.sie_status | SIE_VBUS_DETECTED,
            0x58 => u.buff_status,
            0x5C => 0,
            0x60 => u.ep_abort,
            0x64 => u.ep_abort_done,
            0x8C => u.intr(),
            0x90 => u.inte,
            0x94 => u.intf,
            0x98 => (u.intr() | u.intf) & u.inte,
            o if (o as usize) < 0x9C => u.other[(o / 4) as usize],
            _ => 0,
        }
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        let mut u = self.0.borrow_mut();
        u.update(ctx);
        match off {
            0x00 => u.addr_endp = op.apply(u.addr_endp, value),
            0x40 => u.main_ctrl = op.apply(u.main_ctrl, value),
            0x4C => {
                let was = u.connected();
                u.sie_ctrl = op.apply(u.sie_ctrl, value);
                if was != u.connected() {
                    let at = ctx.now() + 1;
                    u.schedule(at);
                }
            }
            0x50 => u.sie_status &= !(op.clears(value) & SIE_WC),
            0x58 => {
                u.buff_status &= !op.clears(value);
                // the firmware may re-arm a buffer right away
                let at = ctx.now() + 1;
                u.schedule(at);
            }
            0x60 => {
                u.ep_abort = op.apply(u.ep_abort, value);
                u.ep_abort_done |= u.ep_abort;
            }
            0x64 => u.ep_abort_done &= !op.clears(value),
            0x90 => u.inte = op.apply(u.inte, value) & 0xF_FFFF,
            0x94 => u.intf = op.apply(u.intf, value) & 0xF_FFFF,
            o if (o as usize) < 0x9C => {
                let r = &mut u.other[(o / 4) as usize];
                *r = op.apply(*r, value);
            }
            _ => {}
        }
        u.sync_irq(ctx);
    }

    fn update(&mut self, ctx: &mut DeviceCtx) {
        self.0.borrow_mut().update(ctx);
    }

    fn next_event(&self) -> Option<u64> {
        self.0.borrow().next_event()
    }

    fn reset(&mut self) {
        self.0.borrow_mut().disconnected();
        let pipe = self.0.borrow().pipe.clone();
        *self.0.borrow_mut() = UsbCore::new(pipe);
    }
}

/// USBCTRL_DPRAM: endpoint control, buffer control and the buffers.
pub struct UsbDpram(pub SharedUsb);

impl Peripheral for UsbDpram {
    fn name(&self) -> &str {
        "USBCTRL_DPRAM"
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32, size: u32) -> u32 {
        let u = self.0.borrow();
        let o = off as usize;
        match size {
            1 => u32::from(u.dpram[o]),
            2 => u32::from(u16::from_le_bytes([u.dpram[o], u.dpram[o + 1]])),
            _ => u.word(o),
        }
    }

    fn write(&mut self, ctx: &mut DeviceCtx, off: u32, size: u32, value: u32) {
        let mut u = self.0.borrow_mut();
        let o = off as usize;
        u.dpram[o..o + size as usize].copy_from_slice(&value.to_le_bytes()[..size as usize]);
        if (0x80..0x100).contains(&o) && u.host != Host::Detached {
            // a buffer control register: the host will see armed buffers
            let at = ctx.now() + 1;
            u.schedule(at);
        }
    }

    fn next_event(&self) -> Option<u64> {
        self.0.borrow().next_event()
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        self.0.borrow().poll_at.unwrap_or(u64::MAX)
    }
}

pub fn usb(pipe: SharedSerial) -> SharedUsb {
    Rc::new(RefCell::new(UsbCore::new(pipe)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_cdc_endpoints_of_pico_stdio() {
        // configuration descriptor of pico_stdio_usb (CDC + reset interface)
        let config = [
            9, 2, 84, 0, 3, 1, 0, 0x80, 125, // configuration
            8, 11, 0, 2, 2, 2, 0, 0, // IAD
            9, 4, 0, 0, 1, 2, 2, 0, 4, // CDC comm interface
            5, 0x24, 0, 0x20, 1, 5, 0x24, 1, 0, 1, 4, 0x24, 2, 2, 5, 0x24, 6, 0, 1, // functional
            7, 5, 0x81, 3, 8, 0, 16, // notification
            9, 4, 1, 0, 2, 0x0A, 0, 0, 0, // CDC data interface
            7, 5, 0x02, 2, 64, 0, 0, // bulk OUT
            7, 5, 0x82, 2, 64, 0, 0, // bulk IN
            9, 4, 2, 0, 0, 0xFF, 0, 1, 5, // reset interface
        ];
        let cdc = find_cdc(&config).unwrap();
        assert_eq!((cdc.comm_iface, cdc.bulk_in, cdc.bulk_out), (0, 2, 2));
    }
}
