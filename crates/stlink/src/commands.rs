//! The ST-LINK command protocol: 16-byte command packets on endpoint 0x01,
//! responses (if the command has one) on 0x81, data of memory writes as a
//! separate OUT transfer after the command.

use emu_core::Machine;
use emu_core::dap::{DPIDR, DapError};

use crate::{PID, Stlink, VID};

pub const MODE_MASS: u8 = 0x01;
const MODE_DEBUG: u8 = 0x02;

pub const STATUS_OK: u8 = 0x80;
const STATUS_UNKNOWN_CMD: u8 = 0x42;
const STATUS_ALIGNMENT_ERROR: u8 = 0x0A;
const STATUS_AP_FAULT: u8 = 0x11;
const STATUS_AP_ERROR: u8 = 0x12;
const STATUS_BAD_AP: u8 = 0x1D;

/// V2 hardware, JTAG/SWD firmware 40, MSC/VCP 27 ("V2J40M27").
const VERSION: u16 = (2 << 12) | (40 << 6) | 27;
/// Mbed board ID of the NUCLEO-F429ZI.
const BOARD_ID: &[u8; 4] = b"0796";

const GET_VERSION: u8 = 0xF1;
const JTAG_COMMAND: u8 = 0xF2;
const DFU_COMMAND: u8 = 0xF3;
const SWIM_COMMAND: u8 = 0xF4;
const GET_CURRENT_MODE: u8 = 0xF5;
const GET_TARGET_VOLTAGE: u8 = 0xF7;

/// A memory write whose data phase is still to come.
pub struct PendingWrite {
    addr: u32,
    len: usize,
    size: u32,
    increment: bool,
    data: Vec<u8>,
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl Stlink {
    /// An OUT transfer on the debug endpoint: a command or write data.
    pub(crate) fn debug_out(&mut self, m: &mut Machine, data: &[u8]) {
        if let Some(mut w) = self.pending_write.take() {
            let need = w.len - w.data.len();
            w.data.extend_from_slice(&data[..need.min(data.len())]);
            if w.data.len() < w.len {
                self.pending_write = Some(w);
            } else {
                self.finish_write(m, w);
            }
            return;
        }
        let mut cmd = [0u8; 16];
        let n = data.len().min(16);
        cmd[..n].copy_from_slice(&data[..n]);
        self.command(m, &cmd);
    }

    fn respond(&mut self, bytes: &[u8]) {
        self.response.extend(bytes);
    }

    fn status(&mut self, status: u8) {
        self.respond(&[status, 0]);
    }

    fn command(&mut self, m: &mut Machine, cmd: &[u8; 16]) {
        log::trace!("stlink: command {:02x?}", &cmd[..]);
        match cmd[0] {
            GET_VERSION => {
                let mut r = VERSION.to_be_bytes().to_vec();
                r.extend(VID.to_le_bytes());
                r.extend(PID.to_le_bytes());
                self.respond(&r);
            }
            GET_CURRENT_MODE => self.respond(&[self.mode, 0]),
            GET_TARGET_VOLTAGE => {
                // reference and target readings: 2 * 2200 * 1.2 / 1600 = 3.3 V
                let mut r = 1600u32.to_le_bytes().to_vec();
                r.extend(2200u32.to_le_bytes());
                self.respond(&r);
            }
            // DFU_EXIT, SWIM_EXIT: no response
            DFU_COMMAND | SWIM_COMMAND => {}
            JTAG_COMMAND => self.jtag_command(m, cmd),
            other => {
                log::debug!("stlink: unknown command {other:#04x}");
                self.status(STATUS_UNKNOWN_CMD);
            }
        }
    }

    fn jtag_command(&mut self, m: &mut Machine, cmd: &[u8; 16]) {
        match cmd[1] {
            // JTAG_EXIT: no response
            0x21 => self.mode = MODE_MASS,
            // JTAG_ENTER2 (SWD or JTAG)
            0x30 => {
                self.mode = MODE_DEBUG;
                self.status(STATUS_OK);
            }
            // READ_IDCODES (API v2)
            0x31 => {
                let mut r = vec![STATUS_OK, 0, 0, 0];
                r.extend(DPIDR.to_le_bytes());
                r.extend([0; 4]);
                self.respond(&r);
            }
            // RESETSYS: SYSRESETREQ
            0x32 => {
                let _ = m.debug_write(0xE000_ED0C, 4, 0x05FA_0004);
                self.status(STATUS_OK);
            }
            // READREG (DCRSR register number)
            0x33 => {
                let v = read_core_register(m, u32::from(cmd[2]));
                let mut r = vec![STATUS_OK, 0, 0, 0];
                r.extend(v.to_le_bytes());
                self.respond(&r);
            }
            // WRITEREG
            0x34 => {
                write_core_register(m, u32::from(cmd[2]), le32(cmd, 3));
                self.status(STATUS_OK);
            }
            // WRITEDEBUGREG: one word
            0x35 => {
                let ok = m.debug_write(le32(cmd, 2), 4, le32(cmd, 6)).is_ok();
                self.status(if ok { STATUS_OK } else { STATUS_AP_FAULT });
            }
            // READDEBUGREG: one word
            0x36 => {
                let (status, v) = match m.debug_read(le32(cmd, 2), 4) {
                    Ok(v) => (STATUS_OK, v),
                    Err(_) => (STATUS_AP_FAULT, 0),
                };
                let mut r = vec![status, 0, 0, 0];
                r.extend(v.to_le_bytes());
                self.respond(&r);
            }
            // READALLREGS (API v2): status word + 21 registers
            0x3A => {
                let mut r = vec![STATUS_OK, 0, 0, 0];
                for sel in 0..21 {
                    r.extend(read_core_register(m, sel).to_le_bytes());
                }
                self.respond(&r);
            }
            // GETLASTRWSTATUS
            0x3B => self.status(self.last_rw.0),
            // GETLASTRWSTATUS2
            0x3E => {
                let (status, addr) = self.last_rw;
                let mut r = vec![status, 0, 0, 0];
                r.extend(addr.to_le_bytes());
                r.extend([0; 4]);
                self.respond(&r);
            }
            // DRIVE_NRST: low, high, pulse
            0x3C => {
                match cmd[2] {
                    0 => m.set_reset_pin(true),
                    1 => m.set_reset_pin(false),
                    _ => {
                        m.set_reset_pin(true);
                        m.set_reset_pin(false);
                    }
                }
                self.status(STATUS_OK);
            }
            // SWV start / stop (no trace output is produced)
            0x40 | 0x41 => self.status(STATUS_OK),
            // SWV_GET_TRACE_NEW_RECORD_NB
            0x42 => self.respond(&[0, 0]),
            // SWD_SET_FREQ, JTAG_SET_FREQ
            0x43 | 0x44 => self.status(STATUS_OK),
            // READ_DAP_REG
            0x45 => {
                let (port, addr) = (le16(cmd, 2), le16(cmd, 4));
                let (status, v) = match self.read_dap(m, port, addr) {
                    Ok(v) => (STATUS_OK, v),
                    Err(s) => (s, 0),
                };
                let mut r = vec![status, 0, 0, 0];
                r.extend(v.to_le_bytes());
                self.respond(&r);
            }
            // WRITE_DAP_REG
            0x46 => {
                let (port, addr, value) = (le16(cmd, 2), le16(cmd, 4), le32(cmd, 6));
                let status = match self.write_dap(m, port, addr, value) {
                    Ok(()) => STATUS_OK,
                    Err(s) => s,
                };
                self.status(status);
            }
            // INIT_AP, CLOSE_AP_DBG
            0x4B | 0x4C => self.status(if cmd[2] == 0 { STATUS_OK } else { STATUS_BAD_AP }),
            // GET_BOARD_IDENTIFIERS
            0x56 => {
                let mut r = vec![0u8; 128];
                r[0] = STATUS_OK;
                r[2..6].copy_from_slice(BOARD_ID);
                self.respond(&r);
            }
            // READMEM_32BIT, READMEM_8BIT, READMEM_16BIT, READMEM_32BIT_NO_ADDR_INC
            0x07 => self.read_mem(m, cmd, 4, true),
            0x0C => self.read_mem(m, cmd, 1, true),
            0x47 => self.read_mem(m, cmd, 2, true),
            0x54 => self.read_mem(m, cmd, 4, false),
            // WRITEMEM_32BIT, WRITEMEM_8BIT, WRITEMEM_16BIT, WRITEMEM_32BIT_NO_ADDR_INC
            0x08 => self.start_write(cmd, 4, true),
            0x0D => self.start_write(cmd, 1, true),
            0x48 => self.start_write(cmd, 2, true),
            0x50 => self.start_write(cmd, 4, false),
            other => {
                log::debug!("stlink: unknown JTAG command {other:#04x}");
                self.status(STATUS_UNKNOWN_CMD);
            }
        }
    }

    fn read_dap(&mut self, m: &mut Machine, port: u16, addr: u16) -> Result<u32, u8> {
        if port == 0xFFFF {
            return Ok(self.dap.read_dp(addr));
        }
        if port > 0xFF {
            return Err(STATUS_BAD_AP);
        }
        self.dap.read_ap(m, port as u8, addr as u8).map_err(dap_status)
    }

    fn write_dap(&mut self, m: &mut Machine, port: u16, addr: u16, value: u32) -> Result<(), u8> {
        if port == 0xFFFF {
            self.dap.write_dp(addr, value);
            return Ok(());
        }
        if port > 0xFF {
            return Err(STATUS_BAD_AP);
        }
        self.dap.write_ap(m, port as u8, addr as u8, value).map_err(dap_status)
    }

    fn read_mem(&mut self, m: &mut Machine, cmd: &[u8; 16], size: u32, increment: bool) {
        let addr = le32(cmd, 2);
        let len = le16(cmd, 6) as usize;
        let apsel = cmd[8];
        let mut buf = vec![0u8; len];
        self.last_rw = if apsel != 0 {
            (STATUS_BAD_AP, addr)
        } else if size > 1 && (!addr.is_multiple_of(size) || !(len as u32).is_multiple_of(size)) {
            (STATUS_ALIGNMENT_ERROR, addr)
        } else {
            let result = if increment {
                m.debug_read_block(addr, &mut buf, size)
            } else {
                fixed_address(&mut buf, |chunk| {
                    let v = m.debug_read(addr, 4)?;
                    chunk.copy_from_slice(&v.to_le_bytes());
                    Ok(())
                })
            };
            match result {
                Ok(()) => (STATUS_OK, 0),
                Err(f) => (STATUS_AP_FAULT, f.addr),
            }
        };
        // the data phase always has the requested length
        self.respond(&buf);
    }

    fn start_write(&mut self, cmd: &[u8; 16], size: u32, increment: bool) {
        let addr = le32(cmd, 2);
        let len = le16(cmd, 6) as usize;
        if cmd[8] != 0 {
            self.last_rw = (STATUS_BAD_AP, addr);
        }
        let w = PendingWrite { addr, len, size, increment, data: Vec::with_capacity(len) };
        if len == 0 {
            self.last_rw = (STATUS_OK, 0);
        } else {
            self.pending_write = Some(w);
        }
    }

    fn finish_write(&mut self, m: &mut Machine, w: PendingWrite) {
        let PendingWrite { addr, size, increment, data, .. } = w;
        self.last_rw = if size > 1 && (!addr.is_multiple_of(size) || !(data.len() as u32).is_multiple_of(size)) {
            (STATUS_ALIGNMENT_ERROR, addr)
        } else {
            let result = if increment {
                m.debug_write_block(addr, &data, size)
            } else {
                data.chunks(4).try_for_each(|c| m.debug_write(addr, 4, le32(&[c, &[0; 4]].concat(), 0)))
            };
            match result {
                Ok(()) => (STATUS_OK, 0),
                Err(f) => (STATUS_AP_FAULT, f.addr),
            }
        };
    }
}

fn fixed_address(
    buf: &mut [u8],
    mut f: impl FnMut(&mut [u8]) -> Result<(), emu_core::DebugFault>,
) -> Result<(), emu_core::DebugFault> {
    buf.chunks_mut(4).filter(|c| c.len() == 4).try_for_each(&mut f)
}

fn dap_status(e: DapError) -> u8 {
    match e {
        DapError::Fault(_) => STATUS_AP_FAULT,
        DapError::NoAp => STATUS_AP_ERROR,
    }
}

const DCRSR: u32 = 0xE000_EDF4;
const DCRDR: u32 = 0xE000_EDF8;

fn read_core_register(m: &mut Machine, sel: u32) -> u32 {
    let _ = m.debug_write(DCRSR, 4, sel);
    m.debug_read(DCRDR, 4).unwrap_or(0)
}

fn write_core_register(m: &mut Machine, sel: u32, value: u32) {
    let _ = m.debug_write(DCRDR, 4, value);
    let _ = m.debug_write(DCRSR, 4, sel | (1 << 16));
}
