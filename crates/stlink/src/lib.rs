//! The on-board ST-LINK/V2-1 of the NUCLEO-F429ZI as a USB device: the
//! debug interface (ST-LINK protocol, as spoken by pyOCD, OpenOCD and the
//! STM32 tools) on the emulated MCU's debug port, and the virtual COM port
//! (CDC-ACM) on USART3. Exported over USB/IP ([`usbip`]).
//!
//! It identifies as an ST-LINK/V2-1 without mass storage (PID 0x3752) with
//! firmware V2J40M27. The protocol follows pyOCD's `probe/stlink` driver
//! and OpenOCD's `stlink_usb.c`.

mod commands;
mod vcp;

use std::collections::VecDeque;

use emu_core::Machine;
use emu_core::dap::Dap;
use stm32f429::usart::SharedSerial;
use usbip::descriptors::{self, StandardState};
use usbip::{Reply, SetupPacket, UsbDevice};

pub use usbip::{DEFAULT_PORT, UsbipServer};

pub const VID: u16 = 0x0483;
pub const PID: u16 = 0x3752;
/// Bus id under which the probe is exported (`usbip attach -b`).
pub const BUSID: &str = "1-1";

/// Endpoint numbers.
const EP_DEBUG: u8 = 1; // OUT 0x01 commands, IN 0x81 responses
const EP_SWV: u8 = 2; // IN 0x82 trace
const EP_VCP_DATA: u8 = 3; // OUT 0x03, IN 0x83
const EP_VCP_NOTIFY: u8 = 4; // IN 0x84 (interrupt)

const IF_VCP_COMM: u16 = 1;

const STRINGS: [&str; 6] =
    ["STMicroelectronics", "STM32 STLink", "", "ST-Link Debug", "ST-Link VCP Ctrl", "ST-Link VCP Data"];

/// The ST-LINK exported over USB/IP: the host polls it between emulation slices.
pub struct UsbProbe {
    server: UsbipServer,
    device: Stlink,
}

impl UsbProbe {
    /// Listens on `addr` (USB/IP, e.g. `127.0.0.1:3240`). `wake` is called
    /// from other threads when the host should poll soon.
    pub fn start(
        addr: &str,
        serial: Option<SharedSerial>,
        wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<Self> {
        let server = UsbipServer::bind(addr, BUSID, wake)?;
        Ok(Self { server, device: Stlink::new(serial) })
    }

    pub fn local_addr(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }

    /// A host has the probe attached.
    pub fn is_attached(&self) -> bool {
        self.server.is_attached()
    }

    /// Handles USB requests; returns true if any completed.
    pub fn poll(&mut self, m: &mut Machine) -> bool {
        self.server.poll(&mut self.device, m)
    }

    /// The command that attaches the probe to this Linux host.
    pub fn attach_command(&self) -> String {
        attach_command(self.local_addr())
    }
}

/// `usbip attach` for a probe listening on `addr`.
pub fn attach_command(addr: std::net::SocketAddr) -> String {
    let host = if addr.ip().is_unspecified() { "127.0.0.1".to_owned() } else { addr.ip().to_string() };
    let port = if addr.port() == DEFAULT_PORT { String::new() } else { format!(" --tcp-port {}", addr.port()) };
    format!("sudo usbip{port} attach -r {host} -b {BUSID}")
}

/// Runs [`attach_command`] in the background (needs the `vhci-hcd` kernel
/// module and sudo rights); the result is logged.
pub fn spawn_attach(addr: std::net::SocketAddr) {
    let cmd = attach_command(addr);
    std::thread::spawn(move || {
        let mut words = cmd.split_whitespace();
        let program = words.next().unwrap_or("sudo");
        let result = std::process::Command::new(program).args(words).output();
        match result {
            Ok(out) if out.status.success() => log::info!("attached the ST-LINK: {cmd}"),
            Ok(out) => log::warn!(
                "`{cmd}` failed: {} (is the vhci-hcd module loaded? `sudo modprobe vhci-hcd`)",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => log::warn!("cannot run `{cmd}`: {e}"),
        }
    });
}

/// The ST-LINK as a USB device. Lives on the emulator thread.
pub struct Stlink {
    serial_number: String,
    standard: StandardState,
    /// GET_CURRENT_MODE: 0x01 (idle, "mass storage") or 0x02 (debug).
    mode: u8,
    dap: Dap,
    /// Status of the last memory access (GETLASTRWSTATUS2): code, fault address.
    last_rw: (u8, u32),
    /// A WRITEMEM command waiting for its data phase.
    pending_write: Option<commands::PendingWrite>,
    /// Response bytes for the debug IN endpoint.
    response: VecDeque<u8>,
    vcp: vcp::Vcp,
}

impl Stlink {
    /// `serial`: USART3, connected to the virtual COM port (if any).
    pub fn new(serial: Option<SharedSerial>) -> Self {
        Self {
            // 24 hex digits like the real probes ("ITSEMULATOR1")
            serial_number: "49545345 4D554C41 544F5231".replace(' ', ""),
            standard: StandardState::default(),
            mode: commands::MODE_MASS,
            dap: Dap::new(),
            last_rw: (commands::STATUS_OK, 0),
            pending_write: None,
            response: VecDeque::new(),
            vcp: vcp::Vcp::new(serial),
        }
    }

    pub fn serial_number(&self) -> &str {
        &self.serial_number
    }
}

impl UsbDevice<Machine> for Stlink {
    fn device_descriptor(&self) -> Vec<u8> {
        let [v0, v1] = VID.to_le_bytes();
        let [p0, p1] = PID.to_le_bytes();
        // USB 2.0, composite device with interface association (class EF/02/01)
        vec![18, descriptors::DESC_DEVICE, 0x00, 0x02, 0xEF, 0x02, 0x01, 64, v0, v1, p0, p1, 0x00, 0x01, 1, 2, 3, 1]
    }

    fn configuration_descriptor(&self) -> Vec<u8> {
        let mut body = Vec::new();
        // Interface 0: ST-LINK debug (vendor specific)
        body.extend(descriptors::interface(0, 3, 0xFF, 0xFF, 0xFF, 4));
        body.extend(descriptors::endpoint(0x80 | EP_DEBUG, 2, 64, 0));
        body.extend(descriptors::endpoint(EP_DEBUG, 2, 64, 0));
        body.extend(descriptors::endpoint(0x80 | EP_SWV, 2, 64, 0));
        // Interfaces 1 + 2: virtual COM port (CDC ACM)
        body.extend([8, descriptors::DESC_INTERFACE_ASSOCIATION, 1, 2, 0x02, 0x02, 0x01, 5]);
        body.extend(descriptors::interface(1, 1, 0x02, 0x02, 0x01, 5));
        body.extend([5, 0x24, 0x00, 0x10, 0x01]); // header, CDC 1.10
        body.extend([5, 0x24, 0x01, 0x00, 0x02]); // call management
        body.extend([4, 0x24, 0x02, 0x02]); // ACM: line coding + serial state
        body.extend([5, 0x24, 0x06, 1, 2]); // union: comm 1, data 2
        body.extend(descriptors::endpoint(0x80 | EP_VCP_NOTIFY, 3, 2, 255));
        body.extend(descriptors::interface(2, 2, 0x0A, 0x00, 0x00, 6));
        body.extend(descriptors::endpoint(EP_VCP_DATA, 2, 64, 0));
        body.extend(descriptors::endpoint(0x80 | EP_VCP_DATA, 2, 64, 0));
        descriptors::configuration(3, 0x80, 300, &body)
    }

    fn control(&mut self, _m: &mut Machine, setup: &SetupPacket, data: &[u8]) -> Reply {
        let dd = self.device_descriptor();
        let cd = self.configuration_descriptor();
        let serial = self.serial_number.clone();
        let mut strings = STRINGS;
        strings[2] = &serial;
        if let Some(r) = descriptors::standard_request(setup, &dd, &cd, &strings, &mut self.standard) {
            return r;
        }
        // class requests to the CDC communication interface
        if setup.request_type & 0x7F == 0x21 && setup.index == IF_VCP_COMM {
            return self.vcp.class_request(setup, data);
        }
        Reply::Stall
    }

    fn out(&mut self, m: &mut Machine, ep: u8, data: &[u8]) -> Reply {
        match ep {
            EP_DEBUG => {
                self.debug_out(m, data);
                Reply::Done(Vec::new())
            }
            EP_VCP_DATA => self.vcp.out(data),
            _ => Reply::Stall,
        }
    }

    fn input(&mut self, _m: &mut Machine, ep: u8, max_len: usize) -> Reply {
        match ep {
            EP_DEBUG if !self.response.is_empty() => {
                let n = max_len.min(self.response.len());
                Reply::Done(self.response.drain(..n).collect())
            }
            EP_VCP_DATA => self.vcp.input(max_len),
            EP_DEBUG | EP_SWV | EP_VCP_NOTIFY => Reply::Nak,
            _ => Reply::Stall,
        }
    }

    fn attached(&mut self, _m: &mut Machine, attached: bool) {
        // like plugging the probe in: idle, no half-done command
        self.mode = commands::MODE_MASS;
        self.pending_write = None;
        self.response.clear();
        self.vcp.attached(attached);
    }
}
