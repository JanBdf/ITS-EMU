//! Virtual COM port: CDC ACM on USART3 of the emulated MCU.
//!
//! The emulated USART does not depend on the baud rate the host sets: bytes
//! pass unchanged at the firmware's rate (on the board a mismatch would
//! garble them).

use stm32f429::usart::SharedSerial;
use usbip::{Reply, SetupPacket};

const SET_LINE_CODING: u8 = 0x20;
const GET_LINE_CODING: u8 = 0x21;
const SET_CONTROL_LINE_STATE: u8 = 0x22;
const SEND_BREAK: u8 = 0x23;

/// Bytes from the host that may wait for the USART before the probe NAKs.
const RX_LIMIT: usize = 1024;
/// Output kept while nobody reads the port (older bytes are dropped).
const TX_LIMIT: usize = 16 * 1024;

pub struct Vcp {
    serial: Option<SharedSerial>,
    /// dwDTERate, bCharFormat, bParityType, bDataBits
    line_coding: [u8; 7],
}

impl Vcp {
    pub fn new(serial: Option<SharedSerial>) -> Self {
        let mut line_coding = [0u8; 7];
        line_coding[..4].copy_from_slice(&115_200u32.to_le_bytes());
        line_coding[6] = 8;
        Self { serial, line_coding }
    }

    pub fn set_serial(&mut self, serial: Option<SharedSerial>, attached: bool) {
        self.attached(false);
        self.serial = serial;
        self.attached(attached);
    }

    /// The host attached or detached the probe: start or stop copying the output.
    pub fn attached(&mut self, attached: bool) {
        if let Some(s) = &self.serial {
            s.borrow_mut().mirror = attached.then(Vec::new);
        }
    }

    pub fn class_request(&mut self, setup: &SetupPacket, data: &[u8]) -> Reply {
        match setup.request {
            SET_LINE_CODING if data.len() >= 7 => {
                self.line_coding.copy_from_slice(&data[..7]);
                let baud = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                log::debug!("VCP: line coding {baud} baud");
                Reply::Done(Vec::new())
            }
            GET_LINE_CODING => Reply::Done(self.line_coding.to_vec()),
            SET_CONTROL_LINE_STATE | SEND_BREAK => Reply::Done(Vec::new()),
            _ => Reply::Stall,
        }
    }

    /// Data from the host for the MCU's receiver.
    pub fn out(&mut self, data: &[u8]) -> Reply {
        let Some(s) = &self.serial else { return Reply::Done(Vec::new()) };
        let mut port = s.borrow_mut();
        if port.rx.len() > RX_LIMIT {
            return Reply::Nak;
        }
        port.send(data);
        Reply::Done(Vec::new())
    }

    /// Data the MCU has sent.
    pub fn input(&mut self, max_len: usize) -> Reply {
        let Some(s) = &self.serial else { return Reply::Nak };
        let mut port = s.borrow_mut();
        let Some(m) = port.mirror.as_mut() else { return Reply::Nak };
        if m.is_empty() {
            return Reply::Nak;
        }
        if m.len() > TX_LIMIT {
            let excess = m.len() - TX_LIMIT;
            m.drain(..excess);
        }
        let n = max_len.min(m.len());
        Reply::Done(m.drain(..n).collect())
    }
}
