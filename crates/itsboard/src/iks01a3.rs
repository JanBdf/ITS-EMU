//! ST X-NUCLEO-IKS01A3 MEMS & environmental sensor expansion shield on I2C1
//! (`PB8` SCL / `PB9` SDA), as used by the HAW Hamburg ISS (*Sensorik*) module
//! (`ITS-BRD/ITS_BRD_sensorik`).
//!
//! Six I2C sensors share one [`Iks01a3State`] handle:
//!
//! | Sensor | 7-bit addr | WHO_AM_I | Measures |
//! |---|---|---|---|
//! | LSM6DSO | `0x6B` (`0xD7 >> 1`) | `0x0F` = `0x6C` | 3D accel (mg) + 3D gyro (mdps) + temp |
//! | LIS2DW12 | `0x19` (`0x33 >> 1`) | `0x0F` = `0x44` | 3D accel (mg) |
//! | LIS2MDL | `0x1E` (`0x3D >> 1`) | `0x4F` = `0x40` | 3D magnetometer (mGauss) |
//! | HTS221 | `0x5F` (`0xBF >> 1`) | `0x0F` = `0xBC` | Humidity (% RH) + temp (°C) |
//! | LPS22HH | `0x5D` (`0xBB >> 1`) | `0x0F` = `0xB3` | Pressure (hPa/mbar) + temp (°C) |
//! | STTS751 | `0x4A` (`0x94 >> 1`) | `0xFE` = `0x53` | Temperature (°C) |

use std::cell::RefCell;
use std::rc::Rc;

use stm32f429::i2c::I2cDevice;

pub const ADDR_LSM6DSO: u8 = 0x6B;
pub const ADDR_LIS2DW12: u8 = 0x19;
pub const ADDR_LIS2MDL: u8 = 0x1E;
pub const ADDR_HTS221: u8 = 0x5F;
pub const ADDR_LPS22HH: u8 = 0x5D;
pub const ADDR_STTS751: u8 = 0x4A;

/// Physical quantities seen by the six sensors on the X-NUCLEO-IKS01A3 shield.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Iks01a3State {
    /// Acceleration (X, Y, Z) in mg (1000 mg = 1 g).
    pub accel_mg: [f32; 3],
    /// Angular rate (X, Y, Z) in mdps (millidegrees per second).
    pub gyro_mdps: [f32; 3],
    /// Magnetic field (X, Y, Z) in mGauss.
    pub mag_mgauss: [f32; 3],
    /// Ambient temperature in °C (shared by HTS221, LPS22HH, STTS751, LSM6DSO).
    pub temp_c: f32,
    /// Relative humidity in % RH (0..=100).
    pub humidity_rh: f32,
    /// Barometric pressure in hPa / mbar (260..=1260).
    pub pressure_hpa: f32,
}

impl Default for Iks01a3State {
    fn default() -> Self {
        Self {
            accel_mg: [0.0, 0.0, 1000.0],
            gyro_mdps: [0.0, 0.0, 0.0],
            mag_mgauss: [0.0, 200.0, -400.0],
            temp_c: 21.0,
            humidity_rh: 45.0,
            pressure_hpa: 1013.25,
        }
    }
}

/// Shared handle to the shield's physical sensor readings.
pub type SharedIks01a3 = Rc<RefCell<Iks01a3State>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveChip {
    None,
    Lsm6dso,
    Lis2dw12,
    Lis2mdl,
    Hts221,
    Lps22hh,
    Stts751,
}

/// Single [`I2cDevice`] responding to all six X-NUCLEO-IKS01A3 addresses on I2C1.
pub struct Iks01a3 {
    state: SharedIks01a3,
    active: ActiveChip,
    reg: u8,
    auto_inc: bool,
    expect_reg: bool,
    lsm6dso_regs: [u8; 256],
    lis2dw12_regs: [u8; 256],
    lis2mdl_regs: [u8; 256],
    hts221_regs: [u8; 256],
    lps22hh_regs: [u8; 256],
    stts751_regs: [u8; 256],
}

impl Iks01a3 {
    pub fn new(state: SharedIks01a3) -> Self {
        let mut s = Self {
            state,
            active: ActiveChip::None,
            reg: 0,
            auto_inc: false,
            expect_reg: false,
            lsm6dso_regs: [0; 256],
            lis2dw12_regs: [0; 256],
            lis2mdl_regs: [0; 256],
            hts221_regs: [0; 256],
            lps22hh_regs: [0; 256],
            stts751_regs: [0; 256],
        };
        s.reset_lsm6dso();
        s.reset_lis2dw12();
        s.reset_lis2mdl();
        s.reset_hts221();
        s.reset_lps22hh();
        s.reset_stts751();
        s
    }

    fn reset_lsm6dso(&mut self) {
        self.lsm6dso_regs.fill(0);
        self.lsm6dso_regs[0x0F] = 0x6C; // WHO_AM_I
        self.lsm6dso_regs[0x12] = 0x04; // CTRL3_C: IF_INC = 1
    }

    fn reset_lis2dw12(&mut self) {
        self.lis2dw12_regs.fill(0);
        self.lis2dw12_regs[0x0F] = 0x44; // WHO_AM_I
        self.lis2dw12_regs[0x21] = 0x04; // CTRL2: IF_ADD_INC = 1
    }

    fn reset_lis2mdl(&mut self) {
        self.lis2mdl_regs.fill(0);
        self.lis2mdl_regs[0x4F] = 0x40; // WHO_AM_I
    }

    fn reset_hts221(&mut self) {
        self.hts221_regs.fill(0);
        self.hts221_regs[0x0F] = 0xBC; // WHO_AM_I
        // Factory linear calibration points:
        // H0 = 0 %RH (x2 = 0) at H0_T0_OUT = 0; H1 = 100 %RH (x2 = 200) at H1_T0_OUT = 10000
        // -> 100 counts per 1 %RH.
        self.hts221_regs[0x30] = 0;
        self.hts221_regs[0x31] = 200;
        self.hts221_regs[0x36..=0x37].copy_from_slice(&0i16.to_le_bytes());
        self.hts221_regs[0x3A..=0x3B].copy_from_slice(&10000i16.to_le_bytes());
        // T0 = 0 °C (x8 = 0) at T0_OUT = 0; T1 = 50 °C (x8 = 400 = 0x190) at T1_OUT = 5000
        // -> 100 counts per 1 °C.
        self.hts221_regs[0x32] = 0;
        self.hts221_regs[0x33] = 0x90;
        self.hts221_regs[0x35] = 0x04; // T1_degC_x8 MSB = 1 in bits [3:2], T0 MSB = 0 in bits [1:0]
        self.hts221_regs[0x3C..=0x3D].copy_from_slice(&0i16.to_le_bytes());
        self.hts221_regs[0x3E..=0x3F].copy_from_slice(&5000i16.to_le_bytes());
    }

    fn reset_lps22hh(&mut self) {
        self.lps22hh_regs.fill(0);
        self.lps22hh_regs[0x0F] = 0xB3; // WHO_AM_I
        self.lps22hh_regs[0x11] = 0x10; // CTRL_REG2: IF_ADD_INC = 1
    }

    fn reset_stts751(&mut self) {
        self.stts751_regs.fill(0);
        self.stts751_regs[0x03] = 0x0C; // CONFIGURATION: 12-bit resolution (0.0625 °C)
        self.stts751_regs[0x04] = 0x04; // CONVERSION RATE: 1 Hz
        self.stts751_regs[0xFD] = 0x00; // PRODUCT_ID (STTS751-0 / -1)
        self.stts751_regs[0xFE] = 0x53; // MANUFACTURER_ID (STTS751_ID_MAN)
        self.stts751_regs[0xFF] = 0x01; // REVISION_ID
    }

    fn read_lsm6dso(&self, reg: u8) -> u8 {
        let st = *self.state.borrow();
        match reg {
            0x1E => 0x07, // STATUS_REG: TDA | GDA | XLDA
            0x20..=0x21 => {
                let raw = clamp_i16((st.temp_c - 25.0) * 256.0);
                raw.to_le_bytes()[(reg - 0x20) as usize]
            }
            0x22..=0x27 => {
                let ctrl2 = self.lsm6dso_regs[0x11];
                let sens = if ctrl2 & 0x02 != 0 {
                    4.375 // FS_125
                } else {
                    match (ctrl2 >> 2) & 3 {
                        0 => 8.75,
                        1 => 17.5,
                        2 => 35.0,
                        _ => 70.0,
                    }
                };
                let axis = ((reg - 0x22) / 2) as usize;
                let raw = clamp_i16(st.gyro_mdps[axis] / sens);
                raw.to_le_bytes()[((reg - 0x22) % 2) as usize]
            }
            0x28..=0x2D => {
                let ctrl1 = self.lsm6dso_regs[0x10];
                let xl_fs_mode = (self.lsm6dso_regs[0x17] >> 1) & 1;
                let sens = match ((ctrl1 >> 2) & 3, xl_fs_mode) {
                    (0, _) => 0.061,
                    (1, 0) => 0.488, // ±16g
                    (1, _) => 0.061, // ±2g when XL_FS_MODE=1
                    (2, _) => 0.122, // ±4g
                    _ => 0.244,      // ±8g
                };
                let axis = ((reg - 0x28) / 2) as usize;
                let raw = clamp_i16(st.accel_mg[axis] / sens);
                raw.to_le_bytes()[((reg - 0x28) % 2) as usize]
            }
            _ => self.lsm6dso_regs[reg as usize],
        }
    }

    fn read_lis2dw12(&self, reg: u8) -> u8 {
        let st = *self.state.borrow();
        match reg {
            0x27 => 0x01, // STATUS: DRDY
            0x28..=0x2D => {
                let fs_g = match (self.lis2dw12_regs[0x25] >> 4) & 3 {
                    0 => 2.0,
                    1 => 4.0,
                    2 => 8.0,
                    _ => 16.0,
                };
                // Left-justified 16-bit value matches both 14-bit (`raw >> 2`)
                // and 12-bit LP1 (`raw >> 4`) scaling in ST's driver.
                let lsb_mg = fs_g * 1000.0 / 32768.0;
                let axis = ((reg - 0x28) / 2) as usize;
                let raw = clamp_i16(st.accel_mg[axis] / lsb_mg);
                raw.to_le_bytes()[((reg - 0x28) % 2) as usize]
            }
            _ => self.lis2dw12_regs[reg as usize],
        }
    }

    fn read_lis2mdl(&self, reg: u8) -> u8 {
        let st = *self.state.borrow();
        match reg {
            0x67 => 0x0F, // STATUS_REG: ZYXDA | ZDA | YDA | XDA
            0x68..=0x6D => {
                let axis = ((reg - 0x68) / 2) as usize;
                let raw = clamp_i16(st.mag_mgauss[axis] / 1.5);
                raw.to_le_bytes()[((reg - 0x68) % 2) as usize]
            }
            _ => self.lis2mdl_regs[reg as usize],
        }
    }

    fn read_hts221(&self, reg: u8) -> u8 {
        let st = *self.state.borrow();
        match reg {
            0x27 => 0x03, // STATUS_REG: H_DA | T_DA
            0x28..=0x29 => {
                let raw = clamp_i16(st.humidity_rh.clamp(0.0, 100.0) * 100.0);
                raw.to_le_bytes()[(reg - 0x28) as usize]
            }
            0x2A..=0x2B => {
                let raw = clamp_i16(st.temp_c * 100.0);
                raw.to_le_bytes()[(reg - 0x2A) as usize]
            }
            _ => self.hts221_regs[reg as usize],
        }
    }

    fn read_lps22hh(&self, reg: u8) -> u8 {
        let st = *self.state.borrow();
        match reg {
            0x27 => 0x03, // STATUS: T_DA | P_DA
            0x28..=0x2A => {
                let raw = (st.pressure_hpa.clamp(0.0, 2000.0) * 4096.0).round() as i32;
                raw.to_le_bytes()[(reg - 0x28) as usize]
            }
            0x2B..=0x2C => {
                let raw = clamp_i16(st.temp_c * 100.0);
                raw.to_le_bytes()[(reg - 0x2B) as usize]
            }
            _ => self.lps22hh_regs[reg as usize],
        }
    }

    fn read_stts751(&self, reg: u8) -> u8 {
        let st = *self.state.borrow();
        match reg {
            0x00 | 0x02 => {
                // 12-bit signed fixed-point (0.0625 °C/LSB): TEMP_H = bits [15:8], TEMP_L = bits [7:4]
                let raw = clamp_i16(st.temp_c.clamp(-64.0, 127.9375) * 256.0) & !0x0F;
                if reg == 0x00 { (raw >> 8) as u8 } else { (raw & 0xFF) as u8 }
            }
            0x01 => 0x00, // STATUS: not busy
            _ => self.stts751_regs[reg as usize],
        }
    }

    fn step_reg(&mut self) {
        let inc = match self.active {
            ActiveChip::Lsm6dso => self.lsm6dso_regs[0x12] & 0x04 != 0,
            ActiveChip::Lis2dw12 => self.lis2dw12_regs[0x21] & 0x04 != 0,
            ActiveChip::Lis2mdl | ActiveChip::Hts221 => self.auto_inc,
            ActiveChip::Lps22hh => self.lps22hh_regs[0x11] & 0x10 != 0,
            ActiveChip::Stts751 | ActiveChip::None => false,
        };
        if inc {
            self.reg = self.reg.wrapping_add(1);
        }
    }
}

fn clamp_i16(v: f32) -> i16 {
    v.round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

impl I2cDevice for Iks01a3 {
    fn start(&mut self, addr: u8, read: bool) -> bool {
        let chip = match addr {
            ADDR_LSM6DSO => ActiveChip::Lsm6dso,
            ADDR_LIS2DW12 => ActiveChip::Lis2dw12,
            ADDR_LIS2MDL => ActiveChip::Lis2mdl,
            ADDR_HTS221 => ActiveChip::Hts221,
            ADDR_LPS22HH => ActiveChip::Lps22hh,
            ADDR_STTS751 => ActiveChip::Stts751,
            _ => {
                self.active = ActiveChip::None;
                return false;
            }
        };
        self.active = chip;
        self.expect_reg = !read;
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.active == ActiveChip::None {
            return false;
        }
        if std::mem::take(&mut self.expect_reg) {
            match self.active {
                ActiveChip::Hts221 | ActiveChip::Lis2mdl => {
                    self.auto_inc = byte & 0x80 != 0;
                    self.reg = byte & 0x7F;
                }
                _ => {
                    self.auto_inc = false;
                    self.reg = byte;
                }
            }
            return true;
        }
        let r = self.reg;
        match self.active {
            ActiveChip::Lsm6dso => {
                if r == 0x12 && byte & 0x01 != 0 {
                    self.reset_lsm6dso();
                } else if r != 0x0F {
                    self.lsm6dso_regs[r as usize] = byte;
                }
            }
            ActiveChip::Lis2dw12 => {
                if r == 0x21 && byte & 0x40 != 0 {
                    self.reset_lis2dw12();
                } else if r != 0x0F {
                    self.lis2dw12_regs[r as usize] = byte;
                }
            }
            ActiveChip::Lis2mdl => {
                if r == 0x60 && byte & 0x20 != 0 {
                    self.reset_lis2mdl();
                } else if r != 0x4F {
                    self.lis2mdl_regs[r as usize] = byte;
                }
            }
            ActiveChip::Hts221 => {
                if r != 0x0F && !(0x30..=0x3F).contains(&r) {
                    self.hts221_regs[r as usize] = byte;
                }
            }
            ActiveChip::Lps22hh => {
                if r == 0x11 && byte & 0x04 != 0 {
                    self.reset_lps22hh();
                } else if r != 0x0F {
                    self.lps22hh_regs[r as usize] = byte;
                }
            }
            ActiveChip::Stts751 => {
                if r < 0xFD {
                    self.stts751_regs[r as usize] = byte;
                }
            }
            ActiveChip::None => return false,
        }
        self.step_reg();
        true
    }

    fn read(&mut self, _ack: bool) -> u8 {
        let val = match self.active {
            ActiveChip::Lsm6dso => self.read_lsm6dso(self.reg),
            ActiveChip::Lis2dw12 => self.read_lis2dw12(self.reg),
            ActiveChip::Lis2mdl => self.read_lis2mdl(self.reg),
            ActiveChip::Hts221 => self.read_hts221(self.reg),
            ActiveChip::Lps22hh => self.read_lps22hh(self.reg),
            ActiveChip::Stts751 => self.read_stts751(self.reg),
            ActiveChip::None => 0xFF,
        };
        self.step_reg();
        val
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_regs(dev: &mut Iks01a3, addr: u8, reg: u8, n: usize) -> Vec<u8> {
        assert!(dev.start(addr, false));
        assert!(dev.write(reg));
        assert!(dev.start(addr, true));
        (0..n).map(|i| dev.read(i + 1 < n)).collect()
    }

    fn write_reg(dev: &mut Iks01a3, addr: u8, reg: u8, val: u8) {
        assert!(dev.start(addr, false));
        assert!(dev.write(reg));
        assert!(dev.write(val));
    }

    #[test]
    fn all_six_who_am_i_ids_match_st_drivers() {
        let mut dev = Iks01a3::new(Rc::new(RefCell::new(Iks01a3State::default())));
        assert_eq!(read_regs(&mut dev, ADDR_LSM6DSO, 0x0F, 1), [0x6C]);
        assert_eq!(read_regs(&mut dev, ADDR_LIS2DW12, 0x0F, 1), [0x44]);
        assert_eq!(read_regs(&mut dev, ADDR_LIS2MDL, 0x4F, 1), [0x40]);
        assert_eq!(read_regs(&mut dev, ADDR_HTS221, 0x0F, 1), [0xBC]);
        assert_eq!(read_regs(&mut dev, ADDR_LPS22HH, 0x0F, 1), [0xB3]);
        assert_eq!(read_regs(&mut dev, ADDR_STTS751, 0xFE, 1), [0x53]);
        assert!(!dev.start(0x48, false), "does not collide with default LM75 at 0x48");
    }

    #[test]
    fn motion_and_environmental_readings_roundtrip() {
        let state = Rc::new(RefCell::new(Iks01a3State {
            accel_mg: [120.0, -250.0, 1000.0],
            gyro_mdps: [1000.0, -5000.0, 25000.0],
            mag_mgauss: [150.0, -300.0, 450.0],
            temp_c: 23.25,
            humidity_rh: 55.5,
            pressure_hpa: 1012.5,
        }));
        let mut dev = Iks01a3::new(state);

        // LSM6DSO: ±2g (0.061 mg/LSB) & 250 dps (8.75 mdps/LSB)
        write_reg(&mut dev, ADDR_LSM6DSO, 0x10, 0x40); // ODR 104 Hz, ±2g
        write_reg(&mut dev, ADDR_LSM6DSO, 0x11, 0x40); // ODR 104 Hz, 250 dps
        let a = read_regs(&mut dev, ADDR_LSM6DSO, 0x28, 6);
        let az = f32::from(i16::from_le_bytes([a[4], a[5]])) * 0.061;
        assert!((az - 1000.0).abs() < 1.0, "LSM6DSO az={az}");
        let g = read_regs(&mut dev, ADDR_LSM6DSO, 0x22, 6);
        let gz = f32::from(i16::from_le_bytes([g[4], g[5]])) * 8.75;
        assert!((gz - 25000.0).abs() < 10.0, "LSM6DSO gz={gz}");

        // LIS2DW12: ±2g, 14-bit (`raw >> 2` * 0.244 mg)
        let la = read_regs(&mut dev, ADDR_LIS2DW12, 0x28, 6);
        let laz = f32::from(i16::from_le_bytes([la[4], la[5]]) >> 2) * 0.244;
        assert!((laz - 1000.0).abs() < 2.0, "LIS2DW12 az={laz}");

        // LIS2MDL: 1.5 mGauss/LSB (sub-address with bit 7 set)
        let m = read_regs(&mut dev, ADDR_LIS2MDL, 0x68 | 0x80, 6);
        let mx = f32::from(i16::from_le_bytes([m[0], m[1]])) * 1.5;
        assert!((mx - 150.0).abs() < 2.0, "LIS2MDL mx={mx}");

        // HTS221: auto-increment with bit 7
        let h = read_regs(&mut dev, ADDR_HTS221, 0x28 | 0x80, 4);
        let hum = f32::from(i16::from_le_bytes([h[0], h[1]])) / 100.0;
        let temp = f32::from(i16::from_le_bytes([h[2], h[3]])) / 100.0;
        assert!((hum - 55.5).abs() < 0.05);
        assert!((temp - 23.25).abs() < 0.05);

        // LPS22HH: 4096 LSB/hPa, 100 LSB/°C
        let p = read_regs(&mut dev, ADDR_LPS22HH, 0x28, 5);
        let p_raw = i32::from_le_bytes([p[0], p[1], p[2], 0]);
        let press = p_raw as f32 / 4096.0;
        let p_temp = f32::from(i16::from_le_bytes([p[3], p[4]])) / 100.0;
        assert!((press - 1012.5).abs() < 0.05);
        assert!((p_temp - 23.25).abs() < 0.05);

        // STTS751: TEMP_H (0x00) + TEMP_L (0x02)
        let th = read_regs(&mut dev, ADDR_STTS751, 0x00, 1)[0];
        let tl = read_regs(&mut dev, ADDR_STTS751, 0x02, 1)[0];
        let st_temp = f32::from(i16::from_be_bytes([th, tl])) / 256.0;
        assert!((st_temp - 23.25).abs() < 0.01);
    }
}
