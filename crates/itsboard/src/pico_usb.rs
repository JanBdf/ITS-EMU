//! A Pico's USB device on the computer's USB (USB/IP): the transfers of the
//! computer's host go to the Pico's thread, which plays them into its USB
//! controller ([`rp2040::usb::SerialPipe::remote`]), and come back as
//! replies. The device appears as the Pico would (its firmware's
//! descriptors, e.g. 2E8A:000A for pico-sdk's USB serial port).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rp2040::usb::{SerialPipe, Transfer};
use usbip::{Reply, SetupPacket, UsbDevice, UsbipServer};

/// Shared by the Pico's thread and the [`PicoUsbDevice`] on the host's thread.
#[derive(Default)]
pub struct UsbBridge {
    /// The firmware's descriptors, while the device is on the bus.
    descriptors: Option<(Vec<u8>, Vec<u8>)>,
    /// Counts the changes of `descriptors`.
    generation: u64,
    /// A host on the computer has the device attached.
    remote: bool,
    /// Transfers not yet handed to the Pico.
    requests: Vec<Transfer>,
    /// Finished transfers (`None`: stalled).
    done: HashMap<u64, Option<Vec<u8>>>,
    /// Called when transfers finished, so that the host polls soon.
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl UsbBridge {
    /// Exchanges state with the USB controller; on the Pico's thread,
    /// between two runs.
    pub fn sync(&mut self, pipe: &mut SerialPipe) {
        if pipe.descriptors != self.descriptors {
            self.descriptors = pipe.descriptors.clone();
            self.generation += 1;
        }
        pipe.remote = self.remote && self.descriptors.is_some();
        if pipe.remote {
            pipe.requests.extend(self.requests.drain(..));
        } else {
            // detached: what is left is cancelled
            pipe.requests.clear();
            self.requests.clear();
        }
        if !pipe.done.is_empty() {
            self.done.extend(pipe.done.drain(..));
            if let Some(wake) = &self.wake {
                wake();
            }
        }
    }
}

pub type SharedBridge = Arc<Mutex<UsbBridge>>;

/// A transfer the device has handed to the Pico and waits for.
struct Outstanding {
    id: u64,
    setup: [u8; 8],
    data: Vec<u8>,
}

/// The Pico's USB device, served by a [`UsbipServer`] on the host's thread.
pub struct PicoUsbDevice {
    bridge: SharedBridge,
    /// Descriptors when exported.
    device: Vec<u8>,
    config: Vec<u8>,
    generation: u64,
    busid: Option<String>,
    next_id: u64,
    /// Per endpoint and direction.
    outstanding: HashMap<(u8, bool), Outstanding>,
}

impl PicoUsbDevice {
    pub fn new(bridge: SharedBridge, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        bridge.lock().unwrap().wake = Some(wake);
        Self {
            bridge,
            device: Vec::new(),
            config: Vec::new(),
            generation: 0,
            busid: None,
            next_id: 0,
            outstanding: HashMap::new(),
        }
    }

    /// The bus id it is exported under, while the firmware is on the bus.
    pub fn busid(&self) -> Option<&str> {
        self.busid.as_deref()
    }

    /// Exports or withdraws the device as the firmware connects and
    /// disconnects, and serves the computer's transfers. Returns the bus id
    /// when the device has just been exported (to plug it in).
    pub fn poll(&mut self, server: &mut UsbipServer) -> Option<String> {
        let (generation, descriptors) = {
            let b = self.bridge.lock().unwrap();
            (b.generation, b.descriptors.clone())
        };
        let mut exported = None;
        if generation != self.generation {
            self.generation = generation;
            if let Some(busid) = self.busid.take() {
                server.unexport(&busid);
                self.outstanding.clear();
            }
            if let Some((device, config)) = descriptors
                && device.len() >= 18
                && config.len() >= 9
            {
                (self.device, self.config) = (device, config);
                let busid = server.free_busid();
                server.export(&busid, &*self);
                let ids = (
                    u16::from_le_bytes([self.device[8], self.device[9]]),
                    u16::from_le_bytes([self.device[10], self.device[11]]),
                );
                log::info!("Pico USB device {:04x}:{:04x} exported as {busid}", ids.0, ids.1);
                self.busid = Some(busid.clone());
                exported = Some(busid);
            }
        }
        if let Some(busid) = self.busid.clone() {
            server.poll(&busid, self, &mut ());
        }
        exported
    }

    /// Withdraws the device (the Pico is removed).
    pub fn unplug(&mut self, server: &mut UsbipServer) {
        if let Some(busid) = self.busid.take() {
            server.unexport(&busid);
        }
        self.bridge.lock().unwrap().remote = false;
    }

    /// Hands a transfer to the Pico, or answers with its result once there.
    fn transfer(&mut self, ep: u8, dir_in: bool, setup: [u8; 8], data: &[u8], max_len: usize) -> Reply {
        let key = (ep, dir_in);
        let mut bridge = self.bridge.lock().unwrap();
        if let Some(o) = self.outstanding.get(&key) {
            let Some(result) = bridge.done.remove(&o.id) else { return Reply::Nak };
            // The same transfer (retried by the server) gets the result. One
            // the host cancelled meanwhile has served its purpose: an IN
            // result is new data either way.
            let same = o.setup == setup && o.data == data;
            self.outstanding.remove(&key);
            if same || (ep != 0 && dir_in) {
                return result.map_or(Reply::Stall, Reply::Done);
            }
        }
        self.next_id += 1;
        let id = self.next_id;
        bridge.requests.push(Transfer { id, ep, dir_in, setup, data: data.to_vec(), max_len });
        self.outstanding.insert(key, Outstanding { id, setup, data: data.to_vec() });
        Reply::Nak
    }
}

impl UsbDevice<()> for PicoUsbDevice {
    fn device_descriptor(&self) -> Vec<u8> {
        self.device.clone()
    }

    fn configuration_descriptor(&self) -> Vec<u8> {
        self.config.clone()
    }

    fn control(&mut self, _ctx: &mut (), setup: &SetupPacket, data: &[u8]) -> Reply {
        let [v0, v1] = setup.value.to_le_bytes();
        let [i0, i1] = setup.index.to_le_bytes();
        let [l0, l1] = setup.length.to_le_bytes();
        let raw = [setup.request_type, setup.request, v0, v1, i0, i1, l0, l1];
        self.transfer(0, setup.request_type & 0x80 != 0, raw, data, setup.length.into())
    }

    fn out(&mut self, _ctx: &mut (), ep: u8, data: &[u8]) -> Reply {
        self.transfer(ep, false, [0; 8], data, 0)
    }

    fn input(&mut self, _ctx: &mut (), ep: u8, max_len: usize) -> Reply {
        self.transfer(ep, true, [0; 8], &[], max_len)
    }

    fn attached(&mut self, _ctx: &mut (), attached: bool) {
        self.outstanding.clear();
        let mut bridge = self.bridge.lock().unwrap();
        bridge.remote = attached;
        bridge.done.clear();
        log::info!("Pico USB device {}", if attached { "plugged in" } else { "unplugged" });
    }
}
