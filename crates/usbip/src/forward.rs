//! Devices whose USB controller is emulated elsewhere (on another thread,
//! or a peripheral of the host's machine): the computer's transfers go to
//! the controller model as [`Transfer`]s ([`HostLink`]), which plays them
//! into its endpoint buffers, and come back as replies. The device appears
//! with its firmware's own descriptors.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::{Queued, Reply, SetupPacket, UsbDevice, UsbipServer};

/// A transfer of the computer's host.
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

/// The controller model's side: what it knows of the device and the
/// transfers it is to carry out.
#[derive(Default, Debug)]
pub struct HostLink {
    /// Device and configuration descriptor, once the controller model has
    /// read them from the firmware (`None` again when it disconnects).
    pub descriptors: Option<(Vec<u8>, Vec<u8>)>,
    /// A host on the computer drives the device: [`Self::requests`] are
    /// carried out.
    pub remote: bool,
    /// Transfers of the computer's host, in order per endpoint.
    pub requests: VecDeque<Transfer>,
    /// Finished transfers: id and result (`None`: stalled or lost).
    pub done: Vec<(u64, Option<Vec<u8>>)>,
}

/// Shared by the device's thread and the [`ForwardedDevice`] on the host's thread.
#[derive(Default)]
pub struct UsbBridge {
    /// The firmware's descriptors, while the device is on the bus.
    descriptors: Option<(Vec<u8>, Vec<u8>)>,
    /// Counts the changes of `descriptors`.
    generation: u64,
    /// A host on the computer has the device attached.
    remote: bool,
    /// Transfers not yet handed to the device.
    requests: Vec<Transfer>,
    /// Transfers the computer cancelled.
    cancelled: Vec<u64>,
    /// Finished transfers (`None`: stalled, or cancelled before they started).
    done: Vec<(u64, Option<Vec<u8>>)>,
    /// Called when transfers finished, so that the host polls soon.
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl UsbBridge {
    /// Exchanges state with the USB controller; on the device's thread,
    /// between two runs.
    pub fn sync(&mut self, pipe: &mut HostLink) {
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
        // A cancelled transfer that has not started is dropped; one under
        // way finishes (its data is not lost, see ForwardedDevice).
        for id in self.cancelled.drain(..) {
            if let Some(i) = pipe.requests.iter().position(|t| t.id == id) {
                pipe.requests.remove(i);
                pipe.done.push((id, None));
            }
        }
        if !pipe.done.is_empty() {
            self.done.append(&mut pipe.done);
            if let Some(wake) = &self.wake {
                wake();
            }
        }
    }
}

pub type SharedBridge = Arc<Mutex<UsbBridge>>;

/// A transfer handed to the device and not finished yet.
struct Pending {
    seq: u32,
    ep: u8,
    dir_in: bool,
}

/// The forwarded device, served by a [`UsbipServer`] on the host's thread.
/// The computer's transfers are queued in the device as they come (several
/// per endpoint, as a host controller does).
pub struct ForwardedDevice {
    /// Names the device in the log.
    name: String,
    bridge: SharedBridge,
    /// Descriptors when exported.
    device: Vec<u8>,
    config: Vec<u8>,
    generation: u64,
    busid: Option<String>,
    next_id: u64,
    /// Transfers under way by id.
    pending: HashMap<u64, Pending>,
    /// IN transfers cancelled while under way, by id: their endpoint.
    cancelled_in: HashMap<u64, u8>,
    /// IN data of cancelled transfers per endpoint, for the next one (a
    /// controller keeps it in its FIFO until the host asks again).
    leftover: HashMap<u8, Vec<u8>>,
    /// Transfers finished without the device.
    ready: Vec<(u32, Reply)>,
}

impl ForwardedDevice {
    pub fn new(name: &str, bridge: SharedBridge, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        bridge.lock().unwrap().wake = Some(wake);
        Self {
            name: name.to_owned(),
            bridge,
            device: Vec::new(),
            config: Vec::new(),
            generation: 0,
            busid: None,
            next_id: 0,
            pending: HashMap::new(),
            cancelled_in: HashMap::new(),
            leftover: HashMap::new(),
            ready: Vec::new(),
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
                self.forget();
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
                log::info!("{}: USB device {:04x}:{:04x} exported as {busid}", self.name, ids.0, ids.1);
                self.busid = Some(busid.clone());
                exported = Some(busid);
            }
        }
        if let Some(busid) = self.busid.clone() {
            server.poll(&busid, self, &mut ());
        }
        exported
    }

    /// Withdraws the device (it is removed).
    pub fn unplug(&mut self, server: &mut UsbipServer) {
        if let Some(busid) = self.busid.take() {
            server.unexport(&busid);
        }
        self.bridge.lock().unwrap().remote = false;
    }

    /// Forgets the transfers under way.
    fn forget(&mut self) {
        self.pending.clear();
        self.cancelled_in.clear();
        self.leftover.clear();
        self.ready.clear();
    }
}

impl Queued for ForwardedDevice {
    fn submit(&mut self, seq: u32, ep: u8, dir_in: bool, setup: [u8; 8], data: &[u8], max_len: usize) {
        if ep != 0
            && dir_in
            && let Some(left) = self.leftover.get_mut(&ep)
        {
            let n = left.len().min(max_len);
            let chunk: Vec<u8> = left.drain(..n).collect();
            if left.is_empty() {
                self.leftover.remove(&ep);
            }
            self.ready.push((seq, Reply::Done(chunk)));
            return;
        }
        self.next_id += 1;
        let id = self.next_id;
        let t = Transfer { id, ep, dir_in, setup, data: data.to_vec(), max_len };
        self.bridge.lock().unwrap().requests.push(t);
        self.pending.insert(id, Pending { seq, ep, dir_in });
    }

    fn cancel(&mut self, seq: u32) {
        let Some(id) = self.pending.iter().find(|(_, p)| p.seq == seq).map(|(&id, _)| id) else { return };
        let p = self.pending.remove(&id).unwrap();
        if p.dir_in && p.ep != 0 {
            self.cancelled_in.insert(id, p.ep);
        }
        let mut bridge = self.bridge.lock().unwrap();
        // not handed to the device yet: gone; otherwise the device decides
        match bridge.requests.iter().position(|t| t.id == id) {
            Some(i) => {
                bridge.requests.remove(i);
                self.cancelled_in.remove(&id);
            }
            None => bridge.cancelled.push(id),
        }
    }

    fn completions(&mut self) -> Vec<(u32, Reply)> {
        let done = std::mem::take(&mut self.bridge.lock().unwrap().done);
        for (id, result) in done {
            if let Some(p) = self.pending.remove(&id) {
                self.ready.push((p.seq, result.map_or(Reply::Stall, Reply::Done)));
            } else if let Some(ep) = self.cancelled_in.remove(&id)
                && let Some(data) = result.filter(|d| !d.is_empty())
            {
                self.leftover.entry(ep).or_default().extend(data);
            }
        }
        std::mem::take(&mut self.ready)
    }
}

impl UsbDevice<()> for ForwardedDevice {
    fn device_descriptor(&self) -> Vec<u8> {
        self.device.clone()
    }

    fn configuration_descriptor(&self) -> Vec<u8> {
        self.config.clone()
    }

    // transfers go through `queued`
    fn control(&mut self, _ctx: &mut (), _setup: &SetupPacket, _data: &[u8]) -> Reply {
        Reply::Stall
    }

    fn out(&mut self, _ctx: &mut (), _ep: u8, _data: &[u8]) -> Reply {
        Reply::Stall
    }

    fn input(&mut self, _ctx: &mut (), _ep: u8, _max_len: usize) -> Reply {
        Reply::Stall
    }

    fn attached(&mut self, _ctx: &mut (), attached: bool) {
        self.forget();
        let mut bridge = self.bridge.lock().unwrap();
        bridge.remote = attached;
        bridge.done.clear();
        bridge.requests.clear();
        log::info!("{}: USB device {}", self.name, if attached { "plugged in" } else { "unplugged" });
    }

    fn queued(&mut self) -> Option<&mut dyn Queued> {
        Some(self)
    }
}
