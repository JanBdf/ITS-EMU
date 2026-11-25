//! Several devices on one USB/IP server, each polled on its own, as the
//! kernel's vhci-hcd driver talks to them (a minimal client in the test).

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use usbip::descriptors::{self, StandardState};
use usbip::{Reply, SetupPacket, UsbDevice, UsbipServer};

/// A vendor device whose bulk IN endpoint 1 answers with its tag and the
/// data its OUT endpoint 1 got.
struct Echo {
    pid: u16,
    tag: u8,
    standard: StandardState,
    got: Vec<u8>,
    attached: Vec<bool>,
}

impl Echo {
    fn new(pid: u16, tag: u8) -> Self {
        Self { pid, tag, standard: StandardState::default(), got: Vec::new(), attached: Vec::new() }
    }
}

impl UsbDevice<u32> for Echo {
    fn device_descriptor(&self) -> Vec<u8> {
        let [p0, p1] = self.pid.to_le_bytes();
        vec![18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x34, 0x12, p0, p1, 0, 1, 0, 0, 0, 1]
    }

    fn configuration_descriptor(&self) -> Vec<u8> {
        let mut body = descriptors::interface(0, 2, 0xFF, 0, 0, 0).to_vec();
        body.extend(descriptors::endpoint(0x01, 2, 64, 0));
        body.extend(descriptors::endpoint(0x81, 2, 64, 0));
        descriptors::configuration(1, 0x80, 100, &body)
    }

    fn control(&mut self, _ctx: &mut u32, setup: &SetupPacket, _data: &[u8]) -> Reply {
        let (dd, cd) = (self.device_descriptor(), self.configuration_descriptor());
        descriptors::standard_request(setup, &dd, &cd, &[], &mut self.standard).unwrap_or(Reply::Stall)
    }

    fn out(&mut self, ctx: &mut u32, _ep: u8, data: &[u8]) -> Reply {
        *ctx += 1;
        self.got.extend_from_slice(data);
        Reply::Done(Vec::new())
    }

    fn input(&mut self, ctx: &mut u32, _ep: u8, _max_len: usize) -> Reply {
        if self.got.is_empty() {
            return Reply::Nak;
        }
        *ctx += 1;
        let mut v = vec![self.tag];
        v.append(&mut self.got);
        Reply::Done(v)
    }

    fn attached(&mut self, _ctx: &mut u32, attached: bool) {
        self.attached.push(attached);
    }
}

struct Rig {
    server: UsbipServer,
    a: Echo,
    b: Echo,
    /// Transfers each device completed (its host context).
    done: [u32; 2],
}

impl Rig {
    fn new() -> Self {
        let mut server = UsbipServer::bind("127.0.0.1:0", Arc::new(|| {})).unwrap();
        let (a, b) = (Echo::new(0xA, b'a'), Echo::new(0xB, b'b'));
        server.export("1-1", &a);
        assert_eq!(server.free_busid(), "1-2");
        server.export("1-2", &b);
        Self { server, a, b, done: [0; 2] }
    }

    fn poll(&mut self) {
        self.server.poll("1-1", &mut self.a, &mut self.done[0]);
        self.server.poll("1-2", &mut self.b, &mut self.done[1]);
    }

    fn connect(&self) -> TcpStream {
        let s = TcpStream::connect(self.server.local_addr()).unwrap();
        s.set_nonblocking(true).unwrap();
        s
    }

    /// Reads exactly `n` bytes, polling the devices meanwhile.
    fn read(&mut self, s: &mut TcpStream, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        let mut got = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while got < n {
            self.poll();
            match s.read(&mut buf[got..]) {
                Ok(0) => panic!("connection closed"),
                Ok(k) => got += k,
                Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(1)),
                Err(e) => panic!("{e}"),
            }
            assert!(Instant::now() < deadline, "no answer from the server");
        }
        buf
    }

    /// The server closes the connection within a second.
    fn closed(&mut self, s: &mut TcpStream) -> bool {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut b = [0u8; 64];
        while Instant::now() < deadline {
            self.poll();
            match s.read(&mut b) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(1)),
                Err(_) => return true,
            }
        }
        false
    }

    /// OP_REQ_IMPORT of `busid`: its status (0 = accepted) and record.
    fn import(&mut self, s: &mut TcpStream, busid: &str) -> (u32, Vec<u8>) {
        let mut req = op(0x8003);
        let mut id = busid.as_bytes().to_vec();
        id.resize(32, 0);
        req.extend(id);
        s.write_all(&req).unwrap();
        let h = self.read(s, 8);
        let status = be32(&h, 4);
        let record = if status == 0 { self.read(s, 312) } else { Vec::new() };
        (status, record)
    }

    /// OUT then IN on endpoint 1.
    fn echo(&mut self, s: &mut TcpStream, seq: u32, data: &[u8]) -> Vec<u8> {
        s.write_all(&submit(seq, 1, false, data.len() as u32, data)).unwrap();
        let h = self.read(s, 48);
        assert_eq!((be32(&h, 0), be32(&h, 4), be32(&h, 20)), (3, seq, 0), "OUT completed");
        s.write_all(&submit(seq + 1, 1, true, 64, &[])).unwrap();
        let h = self.read(s, 48);
        assert_eq!((be32(&h, 0), be32(&h, 4), be32(&h, 20)), (3, seq + 1, 0), "IN completed");
        let len = be32(&h, 24) as usize;
        self.read(s, len)
    }
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

fn op(code: u16) -> Vec<u8> {
    let mut v = 0x0111u16.to_be_bytes().to_vec();
    v.extend(code.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    v
}

/// USBIP_CMD_SUBMIT without setup packet.
fn submit(seq: u32, ep: u32, dir_in: bool, len: u32, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    for x in [1, seq, 0x0001_0001, u32::from(dir_in), ep, 0, len, 0, 0, 0] {
        v.extend(x.to_be_bytes());
    }
    v.extend([0; 8]);
    v.extend(data);
    v
}

#[test]
fn device_list_has_every_device() {
    let mut rig = Rig::new();
    let mut s = rig.connect();
    s.write_all(&op(0x8005)).unwrap();
    let h = rig.read(&mut s, 12);
    assert_eq!(be32(&h, 8), 2, "two devices");
    for (busid, devnum, pid) in [(b"1-1", 1, 0xA), (b"1-2", 2, 0xB)] {
        let dev = rig.read(&mut s, 312);
        assert_eq!(&dev[256..259], busid);
        assert_eq!((be32(&dev, 288), be32(&dev, 292)), (1, devnum), "bus and device number");
        assert_eq!(&dev[300..304], &[0x12, 0x34, 0x00, pid]);
        assert_eq!(dev[311], 1, "one interface");
        assert_eq!(rig.read(&mut s, 4), vec![0xFF, 0, 0, 0]);
    }
}

#[test]
fn each_device_serves_its_own_connection() {
    let mut rig = Rig::new();
    let mut sb = rig.connect();
    assert_eq!(rig.import(&mut sb, "1-2").0, 0);
    let mut sa = rig.connect();
    let (status, record) = rig.import(&mut sa, "1-1");
    assert_eq!(status, 0);
    assert_eq!(&record[256..259], b"1-1");
    assert!(rig.server.is_attached("1-1") && rig.server.is_attached("1-2"));
    // a device is imported once
    let mut again = rig.connect();
    assert_ne!(rig.import(&mut again, "1-2").0, 0, "1-2 is busy");
    let mut unknown = rig.connect();
    assert_ne!(rig.import(&mut unknown, "1-3").0, 0, "no 1-3");

    assert_eq!(rig.echo(&mut sb, 1, b"to b"), b"bto b");
    assert_eq!(rig.echo(&mut sa, 1, b"to a"), b"ato a");
    assert_eq!(rig.done, [2, 2], "each device got its own transfers");
    assert_eq!((rig.a.attached.as_slice(), rig.b.attached.as_slice()), (&[true][..], &[true][..]));

    // detaching one leaves the other attached
    drop(sa);
    let deadline = Instant::now() + Duration::from_secs(5);
    while rig.server.is_attached("1-1") {
        rig.poll();
        assert!(Instant::now() < deadline, "detach not seen");
        std::thread::sleep(Duration::from_millis(1));
    }
    rig.poll();
    assert_eq!(rig.a.attached, vec![true, false]);
    assert!(rig.server.is_attached("1-2"));
    assert_eq!(rig.echo(&mut sb, 3, b"still"), b"bstill");

    // withdrawing a device unplugs it
    rig.server.unexport("1-2");
    assert!(rig.closed(&mut sb), "the host sees 1-2 unplugged");
    assert_eq!(rig.server.busids().collect::<Vec<_>>(), vec!["1-1"]);
    assert_eq!(rig.server.free_busid(), "1-2");
}

#[test]
fn rebind_after_drop() {
    let server = UsbipServer::bind("127.0.0.1:0", Arc::new(|| {})).unwrap();
    let addr = server.local_addr();
    drop(server);
    let server2 = UsbipServer::bind(addr, Arc::new(|| {})).expect("rebind after drop");
    assert_eq!(server2.local_addr(), addr);
}
