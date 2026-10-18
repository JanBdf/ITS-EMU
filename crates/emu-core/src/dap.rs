//! Debug access port (ADIv5) of the Cortex-M4 as a debug probe drives it:
//! the SW-DP and the AHB-AP (AP 0) that reaches the system bus through
//! [`Machine::debug_read`] / [`Machine::debug_write`].

use crate::machine::{DebugFault, Machine};

/// DPIDR of the Cortex-M4's SW-DP (DPv1, Arm).
pub const DPIDR: u32 = 0x2BA0_1477;
/// IDR of the Cortex-M4's AHB-AP.
pub const AHB_AP_IDR: u32 = 0x2477_0011;
/// AHB-AP BASE: the ROM table at 0xE00F_F000, entry present, ADIv5 format.
pub const AHB_AP_BASE: u32 = 0xE00F_F003;

const CTRL_STICKYORUN: u32 = 1 << 1;
const CTRL_STICKYCMP: u32 = 1 << 4;
const CTRL_STICKYERR: u32 = 1 << 5;
const CTRL_WDATAERR: u32 = 1 << 7;
const CTRL_STICKY: u32 = CTRL_STICKYORUN | CTRL_STICKYCMP | CTRL_STICKYERR | CTRL_WDATAERR;
/// CSYSPWRUPREQ, CDBGPWRUPREQ, CDBGRSTREQ, TRNCNT, MASKLANE, ORUNDETECT
const CTRL_WRITABLE: u32 = (1 << 30) | (1 << 28) | (1 << 26) | 0x00FF_FF01;

const CSW_DEVICE_EN: u32 = 1 << 6;
const CSW_WRITABLE: u32 = 0x7F00_0F37;
const CSW_RESET: u32 = 0x0300_0040;

/// An AP access that faulted or addressed a missing AP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DapError {
    /// The memory access of a MEM-AP faulted (STICKYERR is set).
    Fault(DebugFault),
    /// No AP with this APSEL.
    NoAp,
}

#[derive(Debug, Clone)]
pub struct Dap {
    ctrl_stat: u32,
    select: u32,
    rdbuff: u32,
    csw: u32,
    tar: u32,
}

impl Default for Dap {
    fn default() -> Self {
        Self::new()
    }
}

impl Dap {
    pub fn new() -> Self {
        Self { ctrl_stat: 0, select: 0, rdbuff: 0, csw: CSW_RESET, tar: 0 }
    }

    /// Debug and system power are up (CDBGPWRUPACK, CSYSPWRUPACK).
    pub fn powered(&self) -> bool {
        self.ctrl_stat & (1 << 29) != 0
    }

    /// Reads DP register `addr` (bits 7:4 select the bank, as with DPBANKSEL).
    pub fn read_dp(&mut self, addr: u16) -> u32 {
        match addr & 0xFF {
            0x00 => DPIDR,
            0x04 => {
                // ACKs follow the requests at once.
                let reqs = self.ctrl_stat & ((1 << 30) | (1 << 28) | (1 << 26));
                self.ctrl_stat | (reqs << 1)
            }
            0x08 => 0, // RESEND
            0x0C => self.rdbuff,
            _ => 0, // DPv1: no banked registers
        }
    }

    pub fn write_dp(&mut self, addr: u16, value: u32) {
        match addr & 0xFF {
            0x00 => {
                // ABORT: clear the sticky flags
                if value & (1 << 2) != 0 {
                    self.ctrl_stat &= !CTRL_STICKYERR;
                }
                if value & (1 << 1) != 0 {
                    self.ctrl_stat &= !CTRL_STICKYCMP;
                }
                if value & (1 << 3) != 0 {
                    self.ctrl_stat &= !CTRL_WDATAERR;
                }
                if value & (1 << 4) != 0 {
                    self.ctrl_stat &= !CTRL_STICKYORUN;
                }
            }
            0x04 => {
                // Writing 1 to a sticky flag clears it (JTAG-DP); harmless for SW-DP.
                let sticky = self.ctrl_stat & CTRL_STICKY & !value;
                self.ctrl_stat = (value & CTRL_WRITABLE) | sticky;
            }
            0x08 => self.select = value,
            _ => {}
        }
    }

    /// Reads register `reg` (0x00..0xFC) of AP `apsel`.
    pub fn read_ap(&mut self, m: &mut Machine, apsel: u8, reg: u8) -> Result<u32, DapError> {
        if apsel != 0 {
            return Ok(0); // reads as zero: the end of the AP list
        }
        let v = match reg & 0xFC {
            0x00 => self.csw | CSW_DEVICE_EN,
            0x04 => self.tar,
            0x0C => {
                let v = self.drw_read(m)?;
                self.increment();
                v
            }
            r @ 0x10..=0x1C => m.debug_read((self.tar & !0xF) + u32::from(r - 0x10), 4).map_err(|f| self.fault(f))?,
            0xF4 => 0, // CFG: little endian
            0xF8 => AHB_AP_BASE,
            0xFC => AHB_AP_IDR,
            _ => 0,
        };
        self.rdbuff = v;
        Ok(v)
    }

    pub fn write_ap(&mut self, m: &mut Machine, apsel: u8, reg: u8, value: u32) -> Result<(), DapError> {
        if apsel != 0 {
            return Err(DapError::NoAp);
        }
        match reg & 0xFC {
            0x00 => self.csw = value & CSW_WRITABLE,
            0x04 => self.tar = value,
            0x0C => {
                let size = self.size();
                let lane = (self.tar & 3) * 8;
                let data = if size == 4 { value } else { (value >> lane) & ((1u32 << (8 * size)) - 1) };
                m.debug_write(self.tar, size, data).map_err(|f| self.fault(f))?;
                self.increment();
            }
            r @ 0x10..=0x1C => {
                m.debug_write((self.tar & !0xF) + u32::from(r - 0x10), 4, value).map_err(|f| self.fault(f))?
            }
            _ => {}
        }
        Ok(())
    }

    /// Access size of DRW in bytes (CSW.Size).
    fn size(&self) -> u32 {
        match self.csw & 7 {
            0 => 1,
            1 => 2,
            _ => 4,
        }
    }

    fn drw_read(&mut self, m: &mut Machine) -> Result<u32, DapError> {
        let size = self.size();
        let v = m.debug_read(self.tar, size).map_err(|f| self.fault(f))?;
        // byte lanes of the 32-bit data bus
        Ok(if size == 4 { v } else { v << ((self.tar & 3) * 8) })
    }

    /// CSW.AddrInc: TAR advances within its 1 KB block.
    fn increment(&mut self) {
        if self.csw & 0x30 != 0 {
            let next = self.tar.wrapping_add(self.size());
            self.tar = (self.tar & !0x3FF) | (next & 0x3FF);
        }
    }

    fn fault(&mut self, f: DebugFault) -> DapError {
        self.ctrl_stat |= CTRL_STICKYERR;
        DapError::Fault(f)
    }
}
