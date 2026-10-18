//! USB/IP server for one emulated USB device.
//!
//! The Linux kernel's `vhci-hcd` driver attaches a device exported over
//! USB/IP (`usbip attach -r HOST -b BUSID`) like a locally plugged-in one, so
//! the emulator can appear as a real USB device to libusb, `cdc_acm` & co.
//! Protocol: <https://docs.kernel.org/usb/usbip_protocol.html>.
//!
//! Sockets are read on helper threads, which wake the host through a
//! callback; the device itself is only touched from [`UsbipServer::poll`], on
//! the host's thread (like the GDB server). IN transfers the device cannot
//! answer yet stay pending and are retried on every poll.

pub mod descriptors;

use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// Default USB/IP port (`usbipd`).
pub const DEFAULT_PORT: u16 = 3240;

const USBIP_VERSION: u16 = 0x0111;
const OP_REQ_DEVLIST: u16 = 0x8005;
const OP_REP_DEVLIST: u16 = 0x0005;
const OP_REQ_IMPORT: u16 = 0x8003;
const OP_REP_IMPORT: u16 = 0x0003;
const CMD_SUBMIT: u32 = 1;
const CMD_UNLINK: u32 = 2;
const RET_SUBMIT: u32 = 3;
const RET_UNLINK: u32 = 4;
const DIR_IN: u32 = 1;

const EPIPE: i32 = 32;
const ECONNRESET: i32 = 104;

/// `enum usb_device_speed` of the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    Low = 1,
    Full = 2,
    High = 3,
}

/// The 8-byte SETUP packet of a control transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupPacket {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl SetupPacket {
    pub fn parse(b: &[u8; 8]) -> Self {
        Self {
            request_type: b[0],
            request: b[1],
            value: u16::from_le_bytes([b[2], b[3]]),
            index: u16::from_le_bytes([b[4], b[5]]),
            length: u16::from_le_bytes([b[6], b[7]]),
        }
    }

    pub fn to_bytes(&self) -> [u8; 8] {
        let [v0, v1] = self.value.to_le_bytes();
        let [i0, i1] = self.index.to_le_bytes();
        let [l0, l1] = self.length.to_le_bytes();
        [self.request_type, self.request, v0, v1, i0, i1, l0, l1]
    }

    pub fn is_in(&self) -> bool {
        self.request_type & 0x80 != 0
    }
}

/// A device's answer to a transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// Completed: data for IN, empty for OUT.
    Done(Vec<u8>),
    /// Not ready (no data yet / buffer full): retried on the next poll.
    Nak,
    /// Endpoint halted / request not supported.
    Stall,
}

/// An emulated USB device. `C` is host context passed through
/// [`UsbipServer::poll`] (e.g. the machine a debug probe acts on).
pub trait UsbDevice<C: ?Sized> {
    /// The 18-byte device descriptor.
    fn device_descriptor(&self) -> Vec<u8>;
    /// The complete descriptor of configuration 1 (with interfaces and endpoints).
    fn configuration_descriptor(&self) -> Vec<u8>;
    fn speed(&self) -> Speed {
        Speed::Full
    }
    /// A control transfer on endpoint 0 (`data`: the OUT data stage).
    fn control(&mut self, ctx: &mut C, setup: &SetupPacket, data: &[u8]) -> Reply;
    /// Data for OUT endpoint `ep` (number without direction bit).
    fn out(&mut self, ctx: &mut C, ep: u8, data: &[u8]) -> Reply;
    /// Data from IN endpoint `ep` (number without direction bit), at most `max_len` bytes.
    fn input(&mut self, ctx: &mut C, ep: u8, max_len: usize) -> Reply;
    /// A host attached (`true`) or detached the device.
    fn attached(&mut self, _ctx: &mut C, _attached: bool) {}
}

/// A transfer request from the host.
#[derive(Debug, Clone)]
struct Urb {
    seqnum: u32,
    ep: u8,
    dir_in: bool,
    length: usize,
    setup: [u8; 8],
    data: Vec<u8>,
}

enum Event {
    Connected(u64, TcpStream),
    DevList(u64),
    Import(u64, String),
    Submit(u64, Urb),
    Unlink { conn: u64, seqnum: u32, victim: u32 },
    Closed(u64),
}

struct Attached {
    id: u64,
    stream: TcpStream,
    peer: Option<SocketAddr>,
    pending: VecDeque<Urb>,
}

/// Exports one device under bus id `busid` (e.g. `1-1`).
pub struct UsbipServer {
    local: SocketAddr,
    busid: String,
    events: Receiver<Event>,
    /// Connections that have not imported the device (yet).
    others: Vec<(u64, TcpStream)>,
    attached: Option<Attached>,
}

impl UsbipServer {
    /// Listens on `addr`; `wake` is called (from another thread) whenever a
    /// request arrives, so that the host polls soon.
    pub fn bind(addr: impl ToSocketAddrs, busid: &str, wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        let local = listener.local_addr()?;
        let (tx, events) = channel();
        let busid_owned = busid.to_owned();
        std::thread::Builder::new().name("usbip-accept".into()).spawn(move || {
            accept_loop(listener, tx, wake, busid_owned);
        })?;
        Ok(Self { local, busid: busid.to_owned(), events, others: Vec::new(), attached: None })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    pub fn busid(&self) -> &str {
        &self.busid
    }

    /// A host has the device attached.
    pub fn is_attached(&self) -> bool {
        self.attached.is_some()
    }

    /// Handles the requests that arrived and retries pending transfers.
    /// Returns true if anything was completed.
    pub fn poll<C: ?Sized, D: UsbDevice<C> + ?Sized>(&mut self, dev: &mut D, ctx: &mut C) -> bool {
        let mut progress = false;
        while let Ok(ev) = self.events.try_recv() {
            progress = true;
            self.handle(ev, dev, ctx);
        }
        progress | self.service(dev, ctx)
    }

    fn handle<C: ?Sized, D: UsbDevice<C> + ?Sized>(&mut self, ev: Event, dev: &mut D, ctx: &mut C) {
        match ev {
            Event::Connected(id, stream) => self.others.push((id, stream)),
            Event::DevList(id) => {
                let mut reply = op_header(OP_REP_DEVLIST, 0);
                reply.extend_from_slice(&1u32.to_be_bytes());
                reply.extend(device_record(&self.busid, dev, true));
                self.send_other(id, &reply);
            }
            Event::Import(id, busid) => {
                let ok = busid == self.busid && self.attached.is_none();
                if !ok {
                    log::warn!("usbip: import of '{busid}' refused (busy or unknown bus id)");
                    self.send_other(id, &op_header(OP_REP_IMPORT, 1));
                    return;
                }
                let mut reply = op_header(OP_REP_IMPORT, 0);
                reply.extend(device_record(&self.busid, dev, false));
                self.send_other(id, &reply);
                if let Some(i) = self.others.iter().position(|(c, _)| *c == id) {
                    let (_, stream) = self.others.remove(i);
                    let peer = stream.peer_addr().ok();
                    log::info!("usbip: device {} attached by {}", self.busid, fmt_peer(peer));
                    self.attached = Some(Attached { id, stream, peer, pending: VecDeque::new() });
                    dev.attached(ctx, true);
                }
            }
            Event::Submit(id, urb) => {
                if let Some(a) = self.attached.as_mut().filter(|a| a.id == id) {
                    a.pending.push_back(urb);
                }
            }
            Event::Unlink { conn, seqnum, victim } => {
                let Some(a) = self.attached.as_mut().filter(|a| a.id == conn) else { return };
                // Still pending: cancelled, no RET_SUBMIT follows. Otherwise it
                // has completed already.
                let status = match a.pending.iter().position(|u| u.seqnum == victim) {
                    Some(i) => {
                        a.pending.remove(i);
                        -ECONNRESET
                    }
                    None => 0,
                };
                let mut msg = urb_header(RET_UNLINK, seqnum);
                msg.extend_from_slice(&status.to_be_bytes());
                msg.extend_from_slice(&[0; 24]);
                Self::send_attached(a, &msg);
            }
            Event::Closed(id) => {
                self.others.retain(|(c, _)| *c != id);
                if self.attached.as_ref().is_some_and(|a| a.id == id) {
                    let a = self.attached.take().unwrap();
                    log::info!("usbip: device {} detached by {}", self.busid, fmt_peer(a.peer));
                    dev.attached(ctx, false);
                }
            }
        }
    }

    /// Tries the pending transfers in order; a NAKed transfer blocks later
    /// ones on the same endpoint.
    fn service<C: ?Sized, D: UsbDevice<C> + ?Sized>(&mut self, dev: &mut D, ctx: &mut C) -> bool {
        let Some(a) = self.attached.as_mut() else { return false };
        let mut progress = false;
        let mut blocked: Vec<(u8, bool)> = Vec::new();
        let mut i = 0;
        while i < a.pending.len() {
            let urb = &a.pending[i];
            let key = (urb.ep, urb.dir_in);
            if blocked.contains(&key) {
                i += 1;
                continue;
            }
            let reply = if urb.ep == 0 {
                let setup = SetupPacket::parse(&urb.setup);
                dev.control(ctx, &setup, &urb.data)
            } else if urb.dir_in {
                dev.input(ctx, urb.ep, urb.length)
            } else {
                dev.out(ctx, urb.ep, &urb.data)
            };
            let (status, data) = match reply {
                Reply::Nak => {
                    blocked.push(key);
                    i += 1;
                    continue;
                }
                Reply::Stall => (-EPIPE, Vec::new()),
                Reply::Done(mut data) => {
                    data.truncate(urb.length);
                    (0, data)
                }
            };
            let urb = a.pending.remove(i).unwrap();
            let actual = if urb.dir_in { data.len() } else { urb.data.len() };
            let mut msg = urb_header(RET_SUBMIT, urb.seqnum);
            for v in [status, actual as i32, 0, 0, 0] {
                msg.extend_from_slice(&v.to_be_bytes());
            }
            msg.extend_from_slice(&[0; 8]);
            if urb.dir_in && status == 0 {
                msg.extend_from_slice(&data);
            }
            Self::send_attached(a, &msg);
            progress = true;
        }
        progress
    }

    fn send_other(&mut self, id: u64, msg: &[u8]) {
        if let Some((_, stream)) = self.others.iter_mut().find(|(c, _)| *c == id)
            && let Err(e) = stream.write_all(msg)
        {
            log::debug!("usbip: write failed: {e}");
        }
    }

    fn send_attached(a: &mut Attached, msg: &[u8]) {
        if let Err(e) = a.stream.write_all(msg) {
            log::debug!("usbip: write failed: {e}");
            let _ = a.stream.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for UsbipServer {
    fn drop(&mut self) {
        // Ends the reader threads; the kernel sees the device unplugged.
        if let Some(a) = &self.attached {
            let _ = a.stream.shutdown(Shutdown::Both);
        }
        for (_, s) in &self.others {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

fn fmt_peer(peer: Option<SocketAddr>) -> String {
    peer.map_or_else(|| "?".to_owned(), |p| p.to_string())
}

fn accept_loop(listener: TcpListener, tx: Sender<Event>, wake: Arc<dyn Fn() + Send + Sync>, busid: String) {
    let mut next_id = 0u64;
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log::warn!("usbip: accept failed: {e}");
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let Ok(reader) = stream.try_clone() else { continue };
        next_id += 1;
        let id = next_id;
        if tx.send(Event::Connected(id, stream)).is_err() {
            return; // server dropped
        }
        let (tx, wake, busid) = (tx.clone(), wake.clone(), busid.clone());
        let _ = std::thread::Builder::new().name("usbip-conn".into()).spawn(move || {
            let result = read_loop(id, reader, &tx, &*wake);
            if let Err(e) = result
                && e.kind() != ErrorKind::UnexpectedEof
            {
                log::debug!("usbip {busid}: connection {id}: {e}");
            }
            let _ = tx.send(Event::Closed(id));
            wake();
        });
    }
}

fn read_exact<const N: usize>(r: &mut impl Read) -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Parses the requests of one connection: operations until the device is
/// imported, then URB commands.
fn read_loop(id: u64, mut r: TcpStream, tx: &Sender<Event>, wake: &dyn Fn()) -> io::Result<()> {
    let send = |ev: Event| -> io::Result<()> {
        tx.send(ev).map_err(|_| io::Error::other("server gone"))?;
        wake();
        Ok(())
    };
    loop {
        let op: [u8; 8] = read_exact(&mut r)?;
        let version = u16::from_be_bytes([op[0], op[1]]);
        let code = u16::from_be_bytes([op[2], op[3]]);
        match code {
            OP_REQ_DEVLIST => send(Event::DevList(id))?,
            OP_REQ_IMPORT => {
                let busid: [u8; 32] = read_exact(&mut r)?;
                let end = busid.iter().position(|&b| b == 0).unwrap_or(32);
                send(Event::Import(id, String::from_utf8_lossy(&busid[..end]).into_owned()))?;
                break;
            }
            _ => {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("unknown operation {code:#06x} (version {version:#06x})"),
                ));
            }
        }
    }
    loop {
        let h: [u8; 48] = read_exact(&mut r)?;
        let command = be32(&h, 0);
        let seqnum = be32(&h, 4);
        match command {
            CMD_SUBMIT => {
                let dir_in = be32(&h, 12) == DIR_IN;
                let ep = be32(&h, 16) as u8;
                let length = be32(&h, 24) as i32;
                let packets = be32(&h, 32) as i32;
                let mut setup = [0u8; 8];
                setup.copy_from_slice(&h[40..48]);
                let length = length.max(0) as usize;
                let mut data = Vec::new();
                if !dir_in {
                    data.resize(length, 0);
                    r.read_exact(&mut data)?;
                }
                if packets > 0 {
                    // isochronous packet descriptors (no isochronous endpoints here)
                    let mut iso = vec![0u8; packets as usize * 16];
                    r.read_exact(&mut iso)?;
                }
                send(Event::Submit(id, Urb { seqnum, ep, dir_in, length, setup, data }))?;
            }
            CMD_UNLINK => send(Event::Unlink { conn: id, seqnum, victim: be32(&h, 20) })?,
            other => {
                return Err(io::Error::new(ErrorKind::InvalidData, format!("unknown command {other}")));
            }
        }
    }
}

fn op_header(code: u16, status: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(8);
    v.extend_from_slice(&USBIP_VERSION.to_be_bytes());
    v.extend_from_slice(&code.to_be_bytes());
    v.extend_from_slice(&status.to_be_bytes());
    v
}

fn urb_header(command: u32, seqnum: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(48);
    for x in [command, seqnum, 0, 0, 0] {
        v.extend_from_slice(&x.to_be_bytes());
    }
    v
}

const BUSNUM: u32 = 1;
const DEVNUM: u32 = 1;

/// `struct usbip_usb_device` (+ interfaces for the device list).
fn device_record<C: ?Sized, D: UsbDevice<C> + ?Sized>(busid: &str, dev: &D, with_interfaces: bool) -> Vec<u8> {
    let dd = dev.device_descriptor();
    let cd = dev.configuration_descriptor();
    let mut v = Vec::with_capacity(312);
    let mut path = format!("/sys/devices/platform/its-emu/usb{BUSNUM}/{busid}").into_bytes();
    path.resize(256, 0);
    v.extend(path);
    let mut bus = busid.as_bytes().to_vec();
    bus.resize(32, 0);
    v.extend(bus);
    for x in [BUSNUM, DEVNUM, dev.speed() as u32] {
        v.extend_from_slice(&x.to_be_bytes());
    }
    let le16 = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
    for x in [le16(&dd, 8), le16(&dd, 10), le16(&dd, 12)] {
        v.extend_from_slice(&x.to_be_bytes());
    }
    let interfaces = descriptors::interfaces(&cd);
    v.extend_from_slice(&[dd[4], dd[5], dd[6], cd[5], dd[17], interfaces.len() as u8]);
    if with_interfaces {
        for (class, sub, proto) in interfaces {
            v.extend_from_slice(&[class, sub, proto, 0]);
        }
    }
    v
}
