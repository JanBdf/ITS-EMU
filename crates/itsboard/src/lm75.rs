//! LM75B temperature sensor (NXP) on the I2C header (I2C1, PB8/PB9).
//!
//! Registers as in the datasheet: a pointer register selects temperature
//! (11 bit, 0.125 °C, read-only), configuration, T_hyst and T_os (9 bit,
//! 0.5 °C); two-byte registers are sent MSB first, and reading on repeats
//! the selected register. The temperature register follows the set
//! temperature at once (the sensor converts every 100 ms), except in
//! shutdown mode. The OS output is not connected.

use std::cell::Cell;
use std::rc::Rc;

use stm32f429::i2c::I2cDevice;

/// The temperature at a sensor (°C), set from outside.
pub type SharedCelsius = Rc<Cell<f32>>;

pub struct Lm75 {
    addr: u8,
    celsius: SharedCelsius,
    temperature: u16,
    pointer: u8,
    config: u8,
    thyst: u16,
    tos: u16,
    /// The next written byte sets the pointer.
    expect_pointer: bool,
    /// Byte of the selected register sent or written next.
    index: usize,
}

impl Lm75 {
    /// A sensor at `addr` (0x48..=0x4F: A2..A0).
    pub fn new(addr: u8, celsius: SharedCelsius) -> Self {
        let mut s = Self {
            addr,
            celsius,
            temperature: 0,
            pointer: 0,
            config: 0,
            thyst: 0x4B00, // 75 °C
            tos: 0x5000,   // 80 °C
            expect_pointer: false,
            index: 0,
        };
        s.convert();
        s
    }

    fn convert(&mut self) {
        let eighths = (self.celsius.get().clamp(-55.0, 125.0) * 8.0).floor() as i16;
        self.temperature = (eighths << 5) as u16;
    }

    fn register(&self) -> [u8; 2] {
        match self.pointer {
            0 => self.temperature.to_be_bytes(),
            1 => [self.config, self.config],
            2 => self.thyst.to_be_bytes(),
            _ => self.tos.to_be_bytes(),
        }
    }
}

impl I2cDevice for Lm75 {
    fn start(&mut self, addr: u8, read: bool) -> bool {
        if addr != self.addr {
            return false;
        }
        self.expect_pointer = !read;
        self.index = 0;
        if read && self.config & 1 == 0 {
            self.convert();
        }
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        if std::mem::take(&mut self.expect_pointer) {
            self.pointer = byte & 3;
            self.index = 0;
            return true;
        }
        let i = self.index;
        self.index = (i + 1) % 2;
        match (self.pointer, i) {
            (0, _) => {} // read-only
            (1, _) => self.config = byte & 0x1F,
            (p, 0) => {
                let r = if p == 2 { &mut self.thyst } else { &mut self.tos };
                *r = (*r & 0x00FF) | u16::from(byte) << 8;
            }
            (p, _) => {
                let r = if p == 2 { &mut self.thyst } else { &mut self.tos };
                *r = (*r & 0xFF00) | u16::from(byte & 0x80);
            }
        }
        true
    }

    fn read(&mut self, _ack: bool) -> u8 {
        let byte = self.register()[self.index];
        self.index = (self.index + 1) % 2;
        byte
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read2(s: &mut Lm75, pointer: u8) -> u16 {
        let addr = s.addr;
        assert!(s.start(addr, false));
        s.write(pointer);
        assert!(s.start(addr, true));
        u16::from_be_bytes([s.read(true), s.read(false)])
    }

    #[test]
    fn temperature_in_eighths_of_a_degree() {
        let celsius = SharedCelsius::new(Cell::new(21.0));
        let mut s = Lm75::new(0x48, celsius.clone());
        assert!(!s.start(0x49, true), "other address");
        assert_eq!(read2(&mut s, 0), 0x1500);
        celsius.set(-0.125);
        assert_eq!(read2(&mut s, 0), 0xFFE0);
        celsius.set(125.5);
        assert_eq!(read2(&mut s, 0), 0x7D00, "clamped");
        // reading on repeats the register
        assert!(s.start(0x48, true));
        let bytes: Vec<u8> = (0..4).map(|_| s.read(true)).collect();
        assert_eq!(bytes, [0x7D, 0x00, 0x7D, 0x00]);
    }

    #[test]
    fn limits_and_shutdown() {
        let celsius = SharedCelsius::new(Cell::new(30.0));
        let mut s = Lm75::new(0x4F, celsius.clone());
        let mut write = |bytes: &[u8]| {
            assert!(s.start(0x4F, false));
            bytes.iter().for_each(|&b| assert!(s.write(b)));
        };
        write(&[3, 0x55, 0xFF]); // T_os = 85.5 °C (the low bits are not stored)
        write(&[1, 0x01]); // shutdown
        assert_eq!(read2(&mut s, 3), 0x5580);
        assert_eq!(read2(&mut s, 2), 0x4B00);
        celsius.set(40.0);
        assert_eq!(read2(&mut s, 0), 0x1E00, "frozen in shutdown");
        assert!(s.start(0x4F, false));
        s.write(1);
        s.write(0);
        assert_eq!(read2(&mut s, 0), 0x2800);
    }
}
