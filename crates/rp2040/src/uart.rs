//! UART0/1 (PL011): transmitted text goes to the log; nothing is received.

use emu_core::{Clock, DeviceCtx};

use crate::regs::{Block, Op};

pub struct Uart {
    name: &'static str,
    regs: [u32; 0x50 / 4],
    line: Vec<u8>,
}

impl Uart {
    pub fn new(name: &'static str) -> Self {
        Self { name, regs: [0; 0x50 / 4], line: Vec::new() }
    }
}

impl Block for Uart {
    fn name(&self) -> &str {
        self.name
    }

    fn read(&mut self, _ctx: &mut DeviceCtx, off: u32) -> u32 {
        match off {
            0x000 => 0,
            0x018 => (1 << 7) | (1 << 4), // FR: TX FIFO empty, RX FIFO empty
            0xFE0 => 0x11,
            0xFE4 => 0x10,
            0xFE8 => 0x34,
            0xFF0 => 0x0D,
            0xFF4 => 0xF0,
            0xFF8 => 0x05,
            0xFFC => 0xB1,
            _ => self.regs.get((off / 4) as usize).copied().unwrap_or(0),
        }
    }

    fn write(&mut self, _ctx: &mut DeviceCtx, off: u32, op: Op, value: u32) {
        if off == 0 {
            let b = value as u8;
            if b == b'\n' {
                log::info!("Pico {}: {}", self.name, String::from_utf8_lossy(&self.line).trim_end());
                self.line.clear();
            } else {
                self.line.push(b);
            }
            return;
        }
        if let Some(r) = self.regs.get_mut((off / 4) as usize) {
            *r = op.apply(*r, value);
        }
    }

    fn read_stable_until(&self, _off: u32, _clock: &Clock) -> u64 {
        u64::MAX
    }

    fn reset(&mut self) {
        self.regs = [0; 0x50 / 4];
        self.line.clear();
    }
}
