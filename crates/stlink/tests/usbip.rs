//! The ST-LINK over USB/IP as the kernel's vhci-hcd driver talks to it,
//! with a minimal client in the test (no kernel module or root needed).

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use emu_core::Machine;
use stlink::UsbProbe;

struct Rig {
    probe: UsbProbe,
    machine: Machine,
    mcu: stm32f429::Stm32f429,
}

impl Rig {
    fn new() -> Self {
        let (bus, mcu) = stm32f429::build();
        let mut machine = stm32f429::machine(bus, &mcu).unwrap();
        // a vector table to read back
        machine.load_bin(0x0800_0000, &[0x00, 0x00, 0x03, 0x20, 0xC1, 0x01, 0x00, 0x08]).unwrap();
        machine.reset();
        let probe = UsbProbe::start("127.0.0.1:0", Some(mcu.usart3.clone()), Arc::new(|| {})).unwrap();
        Self { probe, machine, mcu }
    }

    fn connect(&self) -> TcpStream {
        let s = TcpStream::connect(self.probe.local_addr()).unwrap();
        s.set_nonblocking(true).unwrap();
        s
    }

    /// Reads exactly `n` bytes, polling the probe meanwhile (it runs on this thread).
    fn read(&mut self, s: &mut TcpStream, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        let mut got = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while got < n {
            self.probe.poll(&mut self.machine);
            match s.read(&mut buf[got..]) {
                Ok(0) => panic!("connection closed"),
                Ok(k) => got += k,
                Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(1)),
                Err(e) => panic!("{e}"),
            }
            assert!(Instant::now() < deadline, "no answer from the probe");
        }
        buf
    }

    /// Nothing arrives within `ms` (polling the probe).
    fn silent(&mut self, s: &mut TcpStream, ms: u64) -> bool {
        let end = Instant::now() + Duration::from_millis(ms);
        let mut b = [0u8; 1];
        while Instant::now() < end {
            self.probe.poll(&mut self.machine);
            match s.read(&mut b) {
                Ok(_) => return false,
                Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(1)),
                Err(e) => panic!("{e}"),
            }
        }
        true
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

/// USBIP_CMD_SUBMIT.
fn submit(seq: u32, ep: u32, dir_in: bool, len: u32, setup: [u8; 8], data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    for x in [1, seq, 0x0001_0001, u32::from(dir_in), ep, 0, len, 0, 0, 0] {
        v.extend(x.to_be_bytes());
    }
    v.extend(setup);
    v.extend(data);
    v
}

struct Ret {
    status: i32,
    data: Vec<u8>,
}

/// Reads a USBIP_RET_SUBMIT with `seq`.
fn ret_submit(rig: &mut Rig, s: &mut TcpStream, seq: u32, dir_in: bool) -> Ret {
    let h = rig.read(s, 48);
    assert_eq!(be32(&h, 0), 3, "RET_SUBMIT");
    assert_eq!(be32(&h, 4), seq);
    let status = be32(&h, 20) as i32;
    let len = be32(&h, 24) as usize;
    let data = if dir_in && status == 0 { rig.read(s, len) } else { Vec::new() };
    Ret { status, data }
}

fn control_in(rig: &mut Rig, s: &mut TcpStream, seq: u32, setup: [u8; 8]) -> Ret {
    let len = u16::from_le_bytes([setup[6], setup[7]]);
    s.write_all(&submit(seq, 0, true, len.into(), setup, &[])).unwrap();
    ret_submit(rig, s, seq, true)
}

/// A 16-byte ST-LINK command and its response of `resp` bytes.
fn command(rig: &mut Rig, s: &mut TcpStream, seq: &mut u32, cmd: &[u8], resp: u32) -> Vec<u8> {
    let mut c = [0u8; 16];
    c[..cmd.len()].copy_from_slice(cmd);
    *seq += 1;
    s.write_all(&submit(*seq, 1, false, 16, [0; 8], &c)).unwrap();
    assert_eq!(ret_submit(rig, s, *seq, false).status, 0);
    *seq += 1;
    s.write_all(&submit(*seq, 1, true, resp.max(64), [0; 8], &[])).unwrap();
    let r = ret_submit(rig, s, *seq, true);
    assert_eq!(r.status, 0);
    r.data
}

#[test]
fn device_list_and_import() {
    let mut rig = Rig::new();
    let mut s = rig.connect();
    s.write_all(&op(0x8005)).unwrap();
    let h = rig.read(&mut s, 12);
    assert_eq!(&h[2..4], &[0x00, 0x05], "OP_REP_DEVLIST");
    assert_eq!(be32(&h, 8), 1, "one device");
    let dev = rig.read(&mut s, 312);
    assert_eq!(&dev[256..259], b"1-1");
    assert_eq!(be32(&dev, 296), 2, "full speed");
    assert_eq!(&dev[300..304], &[0x04, 0x83, 0x37, 0x52], "VID 0483 PID 3752");
    assert_eq!(dev[311], 3, "three interfaces");
    let ifs = rig.read(&mut s, 12);
    assert_eq!(&ifs[..3], &[0xFF, 0xFF, 0xFF], "debug interface");
    assert_eq!(&ifs[4..7], &[0x02, 0x02, 0x01], "CDC ACM");
}

#[test]
fn stlink_commands_over_usbip() {
    let mut rig = Rig::new();
    let mut s = rig.connect();
    let mut req = op(0x8003);
    let mut busid = b"1-1".to_vec();
    busid.resize(32, 0);
    req.extend(busid);
    s.write_all(&req).unwrap();
    let h = rig.read(&mut s, 8);
    assert_eq!(be32(&h, 4), 0, "import accepted");
    rig.read(&mut s, 312);
    assert!(rig.probe.is_attached());

    // enumeration
    let dd = control_in(&mut rig, &mut s, 1, [0x80, 6, 0, 1, 0, 0, 18, 0]);
    assert_eq!(&dd.data[8..12], &[0x83, 0x04, 0x52, 0x37]);
    let langid = control_in(&mut rig, &mut s, 2, [0x80, 6, 0, 3, 0, 0, 255, 0]);
    assert_eq!(langid.data, vec![4, 3, 0x09, 0x04]);
    let serial = control_in(&mut rig, &mut s, 3, [0x80, 6, 3, 3, 0x09, 0x04, 255, 0]);
    assert_eq!(serial.data.len(), 2 + 2 * 24);
    let cfg = control_in(&mut rig, &mut s, 4, [0x80, 6, 0, 2, 0, 0, 255, 0]);
    assert_eq!(cfg.data[1], 2);
    s.write_all(&submit(5, 0, false, 0, [0x00, 9, 1, 0, 0, 0, 0, 0], &[])).unwrap();
    assert_eq!(ret_submit(&mut rig, &mut s, 5, false).status, 0);

    // pyOCD's _flush_rx: an IN transfer with nothing to read, cancelled after 1 ms
    s.write_all(&submit(6, 1, true, 64, [0; 8], &[])).unwrap();
    assert!(rig.silent(&mut s, 20), "IN stays pending while there is no response");
    let mut unlink = Vec::new();
    for x in [2u32, 7, 0x0001_0001, 0, 0, 6] {
        unlink.extend(x.to_be_bytes());
    }
    unlink.extend([0; 24]);
    s.write_all(&unlink).unwrap();
    let u = rig.read(&mut s, 48);
    assert_eq!(be32(&u, 0), 4, "RET_UNLINK");
    assert_eq!(be32(&u, 4), 7);
    assert_eq!(be32(&u, 20) as i32, -104, "ECONNRESET: the transfer was still pending");

    let mut seq = 10;
    let v = command(&mut rig, &mut s, &mut seq, &[0xF1], 6);
    assert_eq!(v, vec![0x2A, 0x1B, 0x83, 0x04, 0x52, 0x37], "V2J40M27, VID, PID");
    // enter SWD
    assert_eq!(command(&mut rig, &mut s, &mut seq, &[0xF2, 0x30, 0xA3, 0], 2)[0], 0x80);
    // READ_DAP_REG DP 0 (DPIDR)
    let r = command(&mut rig, &mut s, &mut seq, &[0xF2, 0x45, 0xFF, 0xFF, 0, 0], 8);
    assert_eq!(r[0], 0x80);
    assert_eq!(u32::from_le_bytes(r[4..8].try_into().unwrap()), 0x2BA0_1477);
    // READMEM_32BIT of the vector table
    let r = command(&mut rig, &mut s, &mut seq, &[0xF2, 0x07, 0, 0, 0, 0x08, 8, 0, 0], 8);
    assert_eq!(r, vec![0x00, 0x00, 0x03, 0x20, 0xC1, 0x01, 0x00, 0x08]);
    let st = command(&mut rig, &mut s, &mut seq, &[0xF2, 0x3E], 12);
    assert_eq!(st[0], 0x80, "GETLASTRWSTATUS2");
    // a read from unmapped memory faults
    command(&mut rig, &mut s, &mut seq, &[0xF2, 0x07, 0, 0, 0, 0x60, 4, 0, 0], 4);
    let st = command(&mut rig, &mut s, &mut seq, &[0xF2, 0x3E], 12);
    assert_eq!(st[0], 0x11, "AP fault");
    assert_eq!(u32::from_le_bytes(st[4..8].try_into().unwrap()), 0x6000_0000);

    // virtual COM port: MCU output arrives on endpoint 0x83, host data goes to the USART
    rig.mcu.usart3.borrow_mut().push_output(b'A');
    seq += 1;
    s.write_all(&submit(seq, 3, true, 64, [0; 8], &[])).unwrap();
    assert_eq!(ret_submit(&mut rig, &mut s, seq, true).data, b"A");
    seq += 1;
    s.write_all(&submit(seq, 3, false, 2, [0; 8], b"hi")).unwrap();
    assert_eq!(ret_submit(&mut rig, &mut s, seq, false).status, 0);
    assert_eq!(rig.mcu.usart3.borrow().rx.iter().copied().collect::<Vec<_>>(), b"hi");

    // unplugging: the server notices the closed connection
    drop(s);
    let end = Instant::now() + Duration::from_secs(5);
    while rig.probe.is_attached() {
        rig.probe.poll(&mut rig.machine);
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
}
