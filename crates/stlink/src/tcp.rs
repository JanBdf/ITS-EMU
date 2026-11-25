//! The ST-LINK's debug interface over a local TCP connection, for pyOCD
//! where the probe cannot be a USB device (macOS, Windows without
//! usbip-win2). The pyOCD plugin (`pyocd/its_emu_pyocd.py`) adds it to the
//! devices of pyOCD's own `stlink` probe type, so `--probe stlink:` finds it.
//!
//! The connection carries the bulk transfers of the debug interface, as
//! pyOCD's `STLinkUSBInterface` does them. Requests (little endian):
//! `op: u8, len: u32, payload`; only `INFO`, `OPEN` and `IN` are answered,
//! with `status: u8, len: u32, data`:
//!
//! - `INFO` (0): data = `PROTOCOL\nserial\nvendor\nproduct`
//! - `OPEN` (1): claims the probe for this connection (status 1: another
//!   connection has it), like claiming the USB interface. Closing the
//!   connection releases it.
//! - `OUT` (2): payload to the debug OUT endpoint (command or data phase)
//! - `IN` (3): payload = `max: u32`; data = what the debug IN endpoint
//!   returns (answered once the device has data)

use std::io::{self, ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use emu_core::Machine;
use usbip::{Reply, UsbDevice};

use crate::{EP_DEBUG, STRINGS, Stlink};

/// Default port of the TCP probe (`ITS_EMU_STLINK` in the plugin overrides it).
pub const DEFAULT_PORT: u16 = 3241;
/// Version of the protocol above; the plugin refuses other versions.
pub const PROTOCOL: u32 = 1;

const OP_INFO: u8 = 0;
const OP_OPEN: u8 = 1;
const OP_OUT: u8 = 2;
const OP_IN: u8 = 3;

const STATUS_OK: u8 = 0;
const STATUS_BUSY: u8 = 1;
const STATUS_NOT_OPEN: u8 = 2;
const STATUS_STALL: u8 = 3;

/// Larger requests are a protocol error (ST-LINK transfers are at most a few KiB).
const MAX_PAYLOAD: u32 = 1 << 20;

enum Event {
    Connected(u64, TcpStream),
    Request(u64, u8, Vec<u8>),
    Closed(u64),
}

pub(crate) struct TcpServer {
    local: SocketAddr,
    events: Receiver<Event>,
    conns: Vec<(u64, TcpStream)>,
    /// The connection that has the probe open.
    owner: Option<u64>,
    /// An IN request of the owner waiting for data.
    pending_in: Option<usize>,
    stop: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl TcpServer {
    pub fn bind(addr: impl ToSocketAddrs, wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        let local = listener.local_addr()?;
        let (tx, events) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_accept = Arc::clone(&stop);
        let accept_thread = std::thread::Builder::new()
            .name("stlink-tcp".into())
            .spawn(move || accept_loop(listener, tx, wake, stop_accept))?;
        Ok(Self {
            local,
            events,
            conns: Vec::new(),
            owner: None,
            pending_in: None,
            stop,
            accept_thread: Some(accept_thread),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Handles the requests that arrived; returns true if any completed.
    pub fn poll(&mut self, dev: &mut Stlink, m: &mut Machine) -> bool {
        let mut progress = false;
        while let Ok(ev) = self.events.try_recv() {
            progress = true;
            match ev {
                Event::Connected(id, stream) => self.conns.push((id, stream)),
                Event::Closed(id) => {
                    self.conns.retain(|(c, _)| *c != id);
                    if self.owner == Some(id) {
                        log::info!("ST-LINK (TCP): closed by the debugger");
                        self.owner = None;
                        self.pending_in = None;
                    }
                }
                Event::Request(id, op, payload) => self.request(dev, m, id, op, &payload),
            }
        }
        progress | self.service(dev, m)
    }

    fn request(&mut self, dev: &mut Stlink, m: &mut Machine, id: u64, op: u8, payload: &[u8]) {
        match op {
            OP_INFO => {
                let info = format!("{PROTOCOL}\n{}\n{}\n{}", dev.serial_number, STRINGS[0], STRINGS[1]);
                self.send(id, STATUS_OK, info.as_bytes());
            }
            OP_OPEN => {
                if self.owner.is_some_and(|o| o != id) {
                    self.send(id, STATUS_BUSY, &[]);
                    return;
                }
                if self.owner.is_none() {
                    let peer = self.stream(id).and_then(|s| s.peer_addr().ok());
                    log::info!("ST-LINK (TCP): opened by {}", peer.map_or_else(|| "?".into(), |p| p.to_string()));
                }
                self.owner = Some(id);
                self.pending_in = None;
                // like pyOCD's _flush_rx: a new user sees no stale response
                dev.response.clear();
                self.send(id, STATUS_OK, &[]);
            }
            OP_OUT if self.owner == Some(id) => {
                // the debug endpoint always accepts (Reply::Done)
                let _ = dev.out(m, EP_DEBUG, payload);
            }
            OP_IN if self.owner == Some(id) && payload.len() == 4 => {
                self.pending_in = Some(u32::from_le_bytes(payload.try_into().unwrap()) as usize);
            }
            OP_IN => self.send(id, STATUS_NOT_OPEN, &[]),
            _ => {}
        }
    }

    fn service(&mut self, dev: &mut Stlink, m: &mut Machine) -> bool {
        let (Some(max), Some(owner)) = (self.pending_in, self.owner) else { return false };
        let (status, data) = match dev.input(m, EP_DEBUG, max) {
            Reply::Nak => return false,
            Reply::Done(data) => (STATUS_OK, data),
            Reply::Stall => (STATUS_STALL, Vec::new()),
        };
        self.pending_in = None;
        self.send(owner, status, &data);
        true
    }

    fn stream(&self, id: u64) -> Option<&TcpStream> {
        self.conns.iter().find(|(c, _)| *c == id).map(|(_, s)| s)
    }

    fn send(&mut self, id: u64, status: u8, data: &[u8]) {
        let Some((_, stream)) = self.conns.iter_mut().find(|(c, _)| *c == id) else { return };
        let mut msg = Vec::with_capacity(5 + data.len());
        msg.push(status);
        msg.extend_from_slice(&(data.len() as u32).to_le_bytes());
        msg.extend_from_slice(data);
        if let Err(e) = stream.write_all(&msg) {
            log::debug!("ST-LINK (TCP): write failed: {e}");
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for TcpServer {
    fn drop(&mut self) {
        // Stop the accept thread and wait for it to drop the listener so the
        // port can be bound again immediately.
        self.stop.store(true, Ordering::Release);
        let woke = match TcpStream::connect_timeout(&connect_addr(self.local), Duration::from_millis(200)) {
            Ok(_) => true,
            Err(e) => e.kind() == ErrorKind::ConnectionRefused,
        };
        if woke && let Some(t) = self.accept_thread.take() {
            let _ = t.join();
        }
        // ends the reader threads
        for (_, s) in &self.conns {
            let _ = s.shutdown(Shutdown::Both);
        }
        for ev in self.events.try_iter() {
            if let Event::Connected(_, s) = ev {
                let _ = s.shutdown(Shutdown::Both);
            }
        }
    }
}

fn connect_addr(local: SocketAddr) -> SocketAddr {
    match local {
        SocketAddr::V4(a) if a.ip().is_unspecified() => SocketAddr::from(([127, 0, 0, 1], a.port())),
        SocketAddr::V6(a) if a.ip().is_unspecified() => SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], a.port())),
        a => a,
    }
}

fn accept_loop(listener: TcpListener, tx: Sender<Event>, wake: Arc<dyn Fn() + Send + Sync>, stop: Arc<AtomicBool>) {
    let mut next_id = 0u64;
    for stream in listener.incoming() {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log::warn!("ST-LINK (TCP): accept failed: {e}");
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
        let (tx, wake) = (tx.clone(), wake.clone());
        let _ = std::thread::Builder::new().name("stlink-tcp-conn".into()).spawn(move || {
            if let Err(e) = read_loop(id, reader, &tx, &*wake)
                && e.kind() != ErrorKind::UnexpectedEof
            {
                log::debug!("ST-LINK (TCP): connection {id}: {e}");
            }
            let _ = tx.send(Event::Closed(id));
            wake();
        });
    }
}

fn read_loop(id: u64, mut r: TcpStream, tx: &Sender<Event>, wake: &dyn Fn()) -> io::Result<()> {
    loop {
        let mut head = [0u8; 5];
        r.read_exact(&mut head)?;
        let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]);
        if len > MAX_PAYLOAD {
            return Err(io::Error::new(ErrorKind::InvalidData, format!("request of {len} bytes")));
        }
        let mut payload = vec![0u8; len as usize];
        r.read_exact(&mut payload)?;
        tx.send(Event::Request(id, head[0], payload)).map_err(|_| io::Error::other("server gone"))?;
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(s: &mut TcpStream, op: u8, payload: &[u8]) {
        let mut msg = vec![op];
        msg.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        msg.extend_from_slice(payload);
        s.write_all(&msg).unwrap();
    }

    /// Polls until the reply to the last request arrives.
    fn reply(server: &mut TcpServer, dev: &mut Stlink, m: &mut Machine, s: &mut TcpStream) -> (u8, Vec<u8>) {
        s.set_nonblocking(true).unwrap();
        let mut buf = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            server.poll(dev, m);
            let mut chunk = [0u8; 256];
            match s.read(&mut chunk) {
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => panic!("{e}"),
            }
            if buf.len() >= 5 {
                let len = u32::from_le_bytes(buf[1..5].try_into().unwrap()) as usize;
                if buf.len() >= 5 + len {
                    s.set_nonblocking(false).unwrap();
                    return (buf[0], buf[5..5 + len].to_vec());
                }
            }
            assert!(std::time::Instant::now() < deadline, "no reply");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn info_open_and_version_command() {
        let (bus, mcu) = stm32f429::build();
        let mut m = stm32f429::machine(bus, &mcu).unwrap();
        let mut dev = Stlink::new(None);
        let mut server = TcpServer::bind("127.0.0.1:0", Arc::new(|| {})).unwrap();
        let mut a = TcpStream::connect(server.local_addr()).unwrap();
        let mut b = TcpStream::connect(server.local_addr()).unwrap();

        request(&mut a, OP_INFO, &[]);
        let (status, info) = reply(&mut server, &mut dev, &mut m, &mut a);
        assert_eq!(status, STATUS_OK);
        let info = String::from_utf8(info).unwrap();
        let serial = dev.serial_number().to_owned();
        assert_eq!(info.lines().collect::<Vec<_>>(), ["1", &serial, "STMicroelectronics", "STM32 STLink"]);

        // IN before OPEN is refused
        request(&mut a, OP_IN, &64u32.to_le_bytes());
        assert_eq!(reply(&mut server, &mut dev, &mut m, &mut a).0, STATUS_NOT_OPEN);

        request(&mut a, OP_OPEN, &[]);
        assert_eq!(reply(&mut server, &mut dev, &mut m, &mut a).0, STATUS_OK);
        request(&mut b, OP_OPEN, &[]);
        assert_eq!(reply(&mut server, &mut dev, &mut m, &mut b).0, STATUS_BUSY);

        // GET_VERSION: 6 bytes, the version word says V2 J40 M27
        let mut cmd = [0u8; 16];
        cmd[0] = 0xF1;
        request(&mut a, OP_OUT, &cmd);
        request(&mut a, OP_IN, &6u32.to_le_bytes());
        let (status, data) = reply(&mut server, &mut dev, &mut m, &mut a);
        assert_eq!(status, STATUS_OK);
        assert_eq!(data.len(), 6);
        let version = u16::from_be_bytes([data[0], data[1]]);
        assert_eq!((version >> 12, (version >> 6) & 0x3F), (2, 40));
        assert!(server.owner.is_some());

        // closing releases the probe for the other connection
        drop(a);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while server.owner.is_some() {
            server.poll(&mut dev, &mut m);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        request(&mut b, OP_OPEN, &[]);
        assert_eq!(reply(&mut server, &mut dev, &mut m, &mut b).0, STATUS_OK);
    }

    #[test]
    fn rebind_after_drop() {
        let server = TcpServer::bind("127.0.0.1:0", Arc::new(|| {})).unwrap();
        let addr = server.local_addr();
        drop(server);
        let server2 = TcpServer::bind(addr, Arc::new(|| {})).expect("rebind after drop");
        assert_eq!(server2.local_addr(), addr);
    }
}
