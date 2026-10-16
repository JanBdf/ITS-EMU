//! XPT2046 resistive touch controller.
//!
//! A control byte (start bit set) selects a channel; the 12-bit result is then
//! shifted out during the next 16 clocks: one busy bit, 12 data bits, three
//! zeros. Bytes clocked in during readout do not start a new conversion.

/// Raw 12-bit readings for the current touch, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawTouch {
    /// Result of the "X" measurement (control byte 0xD0, A2..A0 = 101).
    pub x: u16,
    /// Result of the "Y" measurement (control byte 0x90, A2..A0 = 001).
    pub y: u16,
    /// Pressure readings (Z1/Z2); constant for an ideal press.
    pub z1: u16,
    pub z2: u16,
}

#[derive(Default)]
pub struct Xpt2046 {
    touch: Option<RawTouch>,
    readout: [u8; 2],
    readout_left: usize,
}

impl Xpt2046 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_touch(&mut self, touch: Option<RawTouch>) {
        self.touch = touch;
    }

    /// PENIRQ is low while the screen is touched.
    pub fn pen_irq(&self) -> bool {
        self.touch.is_none()
    }

    pub fn end_frame(&mut self) {
        self.readout_left = 0;
    }

    pub fn transfer(&mut self, mosi: u8) -> u8 {
        if self.readout_left > 0 {
            let out = self.readout[2 - self.readout_left];
            self.readout_left -= 1;
            return out;
        }
        if mosi & 0x80 != 0 {
            let value = self.convert((mosi >> 4) & 7, mosi & 0x08 != 0);
            let frame = value << 3; // busy bit (0), 12 bits, 3 zeros
            self.readout = [(frame >> 8) as u8, frame as u8];
            self.readout_left = 2;
        }
        0
    }

    fn convert(&self, channel: u8, eight_bit: bool) -> u16 {
        let t = self.touch;
        let v = match (channel, t) {
            (5, Some(t)) => t.x,
            (1, Some(t)) => t.y,
            (3, Some(t)) => t.z1,
            (4, Some(t)) => t.z2,
            (3, None) => 0,
            (4, None) => 4095,
            (2, _) => 2048, // VBAT
            (6, _) => 2048, // AUX
            (0, _) => 1000, // TEMP0
            (7, _) => 1100, // TEMP1
            _ => 0,         // no touch: plates not connected
        };
        let v = v.min(4095);
        if eight_bit { (v >> 4) << 4 } else { v }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same sequence as TP_Read_ADC() of the Waveshare driver.
    fn read_adc(t: &mut Xpt2046, cmd: u8) -> u16 {
        t.transfer(cmd);
        let mut data = u16::from(t.transfer(0xFF)) << 8;
        data |= u16::from(t.transfer(0xFF));
        t.end_frame();
        data >> 3
    }

    #[test]
    fn driver_reads_back_12_bit_values() {
        let mut t = Xpt2046::new();
        t.set_touch(Some(RawTouch { x: 3000, y: 1234, z1: 400, z2: 3000 }));
        assert!(!t.pen_irq());
        assert_eq!(read_adc(&mut t, 0xD0), 3000);
        assert_eq!(read_adc(&mut t, 0x90), 1234);
        t.set_touch(None);
        assert!(t.pen_irq());
    }
}
