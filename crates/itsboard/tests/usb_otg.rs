//! USB OTG FS (CN13) with ST's USB device library and the HAL's PCD driver
//! (UsbCdcEcho): the host model's enumeration, control transfers with data
//! stages, a stall, bulk transfers and their rate, driven through the link
//! a USB/IP device uses.

mod common;

use common::Sim;
use usbip::forward::Transfer;

struct Host {
    sim: Sim,
    next_id: u64,
    /// Finished transfers not waited for yet.
    done: std::collections::HashMap<u64, Option<Vec<u8>>>,
}

impl Host {
    fn boot() -> Option<Host> {
        let sim = Sim::boot("UsbCdcEcho.axf")?;
        sim.board.mcu().otg.borrow_mut().cable = true;
        Some(Host { sim, next_id: 0, done: Default::default() })
    }

    fn ms(&self) -> f64 {
        self.sim.machine.clock().time_ns() as f64 / 1e6
    }

    /// Runs until the firmware has connected and the host model has read
    /// the descriptors; then a host on the computer takes over.
    fn wait_for_descriptors(&mut self) -> (Vec<u8>, Vec<u8>) {
        for _ in 0..10_000 {
            let descriptors = self.sim.board.mcu().otg.borrow().link.descriptors.clone();
            if let Some(d) = descriptors {
                self.sim.board.mcu().otg.borrow_mut().link.remote = true;
                return d;
            }
            let t = self.ms() as u64 + 1;
            self.sim.run_until_ms(t);
        }
        panic!("no USB device after {} ms", self.ms());
    }

    fn submit(&mut self, ep: u8, dir_in: bool, setup: [u8; 8], data: &[u8], max_len: usize) -> u64 {
        self.next_id += 1;
        let t = Transfer { id: self.next_id, ep, dir_in, setup, data: data.to_vec(), max_len };
        self.sim.board.mcu().otg.borrow_mut().link.requests.push_back(t);
        self.next_id
    }

    /// Runs until the transfers `ids` have finished; their results in that order.
    fn wait(&mut self, ids: &[u64]) -> Vec<Option<Vec<u8>>> {
        let deadline = self.ms() + 2000.0;
        while !ids.iter().all(|id| self.done.contains_key(id)) {
            let budget = self.sim.machine.clock().ns_to_cycles(100_000);
            self.sim.machine.run(budget);
            self.done.extend(self.sim.board.mcu().otg.borrow_mut().link.done.drain(..));
            let missing: Vec<_> = ids.iter().filter(|id| !self.done.contains_key(id)).collect();
            assert!(self.ms() < deadline, "transfers {missing:?} unfinished");
        }
        ids.iter().map(|id| self.done.remove(id).unwrap()).collect()
    }

    fn control(&mut self, setup: [u8; 8], data: &[u8]) -> Option<Vec<u8>> {
        let length = usize::from(u16::from_le_bytes([setup[6], setup[7]]));
        let id = self.submit(0, setup[0] & 0x80 != 0, setup, data, length);
        self.wait(&[id]).pop().unwrap()
    }
}

#[test]
fn cdc_device_on_cn13() {
    let Some(mut host) = Host::boot() else { return };
    let (device, config) = host.wait_for_descriptors();
    assert_eq!(&device[8..12], &[0x83, 0x04, 0x40, 0x57], "0483:5740");
    assert_eq!(config[1], 2, "configuration descriptor");
    assert_eq!(usize::from(u16::from_le_bytes([config[2], config[3]])), config.len());
    assert!(config.windows(3).any(|w| w == [4, 0, 0]) || config.contains(&0x0A), "CDC interfaces");
    let attached_at = host.ms();
    assert!(attached_at < 10_000.0, "connected after {attached_at} ms");

    // string descriptor (the device's own)
    let product = host.control([0x80, 6, 2, 3, 0x09, 0x04, 255, 0], &[]).expect("product string");
    let text: String = product[2..].chunks(2).map(|c| c[0] as char).collect();
    assert_eq!(text, "UsbCdcEcho");
    // a descriptor a full-speed device does not have: stalled
    assert_eq!(host.control([0x80, 6, 0, 6, 0, 0, 10, 0], &[]), None, "device qualifier stalls");
    // and the device still answers afterwards
    assert_eq!(host.control([0x80, 0, 0, 0, 0, 0, 2, 0], &[]).map(|d| d.len()), Some(2), "GET_STATUS");

    assert_eq!(host.control([0x00, 9, 1, 0, 0, 0, 0, 0], &[]), Some(vec![]), "SET_CONFIGURATION");
    // class requests: a data stage OUT, then read back
    let coding = [0x80, 0x25, 0, 0, 0, 0, 8]; // 9600 8N1
    assert_eq!(host.control([0x21, 0x20, 0, 0, 0, 0, 7, 0], &coding), Some(vec![]), "SET_LINE_CODING");
    assert_eq!(host.control([0xA1, 0x21, 0, 0, 0, 0, 7, 0], &[]), Some(coding.to_vec()), "GET_LINE_CODING");
    assert_eq!(host.control([0x21, 0x22, 3, 0, 0, 0, 0, 0], &[]), Some(vec![]), "SET_CONTROL_LINE_STATE");

    // echo
    let out = host.submit(1, false, [0; 8], b"hello, cn13!", 0);
    let inp = host.submit(1, true, [0; 8], &[], 128);
    let r = host.wait(&[out, inp]);
    assert_eq!(r[1].as_deref(), Some(&b"HELLO, CN13!"[..]));

    // 64 KiB through and back, with transfers queued as a host does
    let data: Vec<u8> = (0..65536u32).map(|i| b"abcdefghijklmnopqrstuvwxyz0123456789"[i as usize % 36]).collect();
    let start = host.ms();
    let mut ids = Vec::new();
    for chunk in data.chunks(4096) {
        ids.push(host.submit(1, false, [0; 8], chunk, 0));
    }
    let mut ins = Vec::new();
    for _ in 0..data.len() / 64 {
        ins.push(host.submit(1, true, [0; 8], &[], 128));
    }
    let results = host.wait(&ins);
    let echoed: Vec<u8> = results.into_iter().flat_map(|r| r.unwrap()).collect();
    host.wait(&ids);
    let ms = host.ms() - start;
    assert_eq!(echoed.len(), data.len());
    assert_eq!(echoed, data.to_ascii_uppercase());
    let rate = data.len() as f64 / ms; // bytes per ms = KB/s
    eprintln!("64 KiB echoed in {ms:.1} ms (virtual): {rate:.0} KB/s each way");
    // full speed's limit is about 1.2 MB/s; the firmware answers each
    // packet before it takes the next
    assert!(rate > 150.0 && rate < 1300.0, "{rate:.0} KB/s");
}
