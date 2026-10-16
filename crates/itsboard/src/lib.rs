//! The HAW Hamburg ITS-Board: NUCLEO-F429ZI on the ITS carrier board with the
//! Waveshare 4" TFT touch shield.
//!
//! | Function            | Pins                                   |
//! |---------------------|----------------------------------------|
//! | LEDs OUT0–7 / 8–15  | PD0–7 / PE0–7                          |
//! | Buttons S0–S7       | PF0–7 (pressed = low, pull-up in MCU)  |
//! | INT0–5 / AUX0–5     | PG0–5                                  |
//! | Nucleo LD1/LD2/LD3  | PB0 / PB7 / PB14                       |
//! | Nucleo user button  | PC13 (pressed = high)                  |
//! | TFT (ILI9486)       | SPI1, CS PD14, DC PF13, RST PF12, BL PD15 (TIM4_CH4) |
//! | Touch (XPT2046)     | SPI1, CS PF14, PENIRQ PE13, BUSY PB10  |

pub mod ili9486;
pub mod xpt2046;

use std::cell::RefCell;
use std::rc::Rc;

use stm32f429::Stm32f429;
use stm32f429::gpio::{PinMode, SharedGpio, port};
use stm32f429::spi::SpiDevice;

use ili9486::{GRAM_COLS, GRAM_ROWS, Ili9486};
use xpt2046::{RawTouch, Xpt2046};

/// Size of the display as seen on the board (landscape).
pub const SCREEN_WIDTH: usize = 480;
pub const SCREEN_HEIGHT: usize = 320;

const LCD_CS: (char, u32) = ('D', 14);
const LCD_DC: (char, u32) = ('F', 13);
const LCD_RST: (char, u32) = ('F', 12);
const LCD_BL: (char, u32) = ('D', 15);
const TP_CS: (char, u32) = ('F', 14);
const TP_IRQ: (char, u32) = ('E', 13);
const TP_BUSY: (char, u32) = ('B', 10);
const USER_BUTTON: (char, u32) = ('C', 13);

/// Routes SPI1 frames to the chip whose chip select is low.
struct Spi1Router {
    gpio: SharedGpio,
    lcd: Rc<RefCell<Ili9486>>,
    touch: Rc<RefCell<Xpt2046>>,
    lcd_cs_rises: u32,
    tp_cs_rises: u32,
    rst_rises: u32,
}

impl SpiDevice for Spi1Router {
    fn transfer(&mut self, mosi: u16) -> u16 {
        let g = self.gpio.borrow();
        let level = |(p, pin): (char, u32)| g.level(port(p), pin);
        let rises = |(p, pin): (char, u32)| g.rises(port(p), pin);

        let mut lcd = self.lcd.borrow_mut();
        let mut touch = self.touch.borrow_mut();
        // Edges since the last transfer: chip-select pulses end frames,
        // the end of a reset pulse resets the controller.
        if rises(LCD_RST) != self.rst_rises {
            self.rst_rises = rises(LCD_RST);
            lcd.reset();
        }
        if rises(LCD_CS) != self.lcd_cs_rises {
            self.lcd_cs_rises = rises(LCD_CS);
            lcd.end_frame();
        }
        if rises(TP_CS) != self.tp_cs_rises {
            self.tp_cs_rises = rises(TP_CS);
            touch.end_frame();
        }

        let byte = mosi as u8;
        let mut miso = 0xFF;
        if !level(LCD_CS) && level(LCD_RST) {
            lcd.write_byte(level(LCD_DC), byte); // the shield cannot read the TFT
        }
        if !level(TP_CS) {
            miso &= touch.transfer(byte);
        }
        u16::from(miso)
    }
}

pub struct ItsBoard {
    mcu: Stm32f429,
    lcd: Rc<RefCell<Ili9486>>,
    touch: Rc<RefCell<Xpt2046>>,
}

impl ItsBoard {
    /// Connects the board to an MCU instance.
    pub fn attach(mcu: &Stm32f429) -> Self {
        let lcd = Rc::new(RefCell::new(Ili9486::new()));
        let touch = Rc::new(RefCell::new(Xpt2046::new()));
        *mcu.spi1.borrow_mut() = Some(Box::new(Spi1Router {
            gpio: mcu.gpio.clone(),
            lcd: lcd.clone(),
            touch: touch.clone(),
            lcd_cs_rises: 0,
            tp_cs_rises: 0,
            rst_rises: 0,
        }));
        let board = Self { mcu: mcu.clone(), lcd, touch };
        {
            let mut g = board.mcu.gpio.borrow_mut();
            g.drive(port(TP_BUSY.0), TP_BUSY.1, Some(false));
            g.drive(port(USER_BUTTON.0), USER_BUTTON.1, Some(false)); // external pull-down
        }
        board.sync_touch_irq();
        board
    }

    pub fn mcu(&self) -> &Stm32f429 {
        &self.mcu
    }

    /// Presses (`true`) or releases button S0–S7.
    pub fn set_button(&self, n: u32, pressed: bool) {
        assert!(n < 8, "the ITS-Board has buttons S0..S7");
        // Pressed buttons pull the pin to GND; released pins float to the MCU pull-up.
        self.mcu.gpio.borrow_mut().drive(port('F'), n, pressed.then_some(false));
    }

    /// Drives one of the INT0–5 inputs (PG0–5); `None` leaves it open.
    pub fn set_int(&self, n: u32, level: Option<bool>) {
        assert!(n < 6, "the ITS-Board has inputs INT0..INT5");
        self.mcu.gpio.borrow_mut().drive(port('G'), n, level);
    }

    pub fn set_user_button(&self, pressed: bool) {
        self.mcu.gpio.borrow_mut().drive(port(USER_BUTTON.0), USER_BUTTON.1, Some(pressed));
    }

    /// OUT15..OUT0 as a bit mask (bit n = OUTn lit).
    pub fn leds(&self) -> u16 {
        let g = self.mcu.gpio.borrow();
        (0..16)
            .filter(|&i| if i < 8 { g.level(port('D'), i) } else { g.level(port('E'), i - 8) })
            .fold(0, |acc, i| acc | (1 << i))
    }

    /// Levels of the input pins IN0–7 (bit set = high, i.e. button released).
    pub fn inputs(&self) -> u8 {
        let g = self.mcu.gpio.borrow();
        (0..8).filter(|&i| g.level(port('F'), i)).fold(0, |acc, i| acc | (1 << i))
    }

    /// Nucleo LD1 (green), LD2 (blue), LD3 (red).
    pub fn nucleo_leds(&self) -> [bool; 3] {
        let g = self.mcu.gpio.borrow();
        [g.level(port('B'), 0), g.level(port('B'), 7), g.level(port('B'), 14)]
    }

    /// Touches the screen at a position in screen coordinates (as seen on the
    /// board), or releases it.
    pub fn set_touch(&self, pos: Option<(f32, f32)>) {
        let raw = pos.map(|(x, y)| screen_to_raw_touch(x, y));
        self.touch.borrow_mut().set_touch(raw);
        self.sync_touch_irq();
    }

    fn sync_touch_irq(&self) {
        let level = self.touch.borrow().pen_irq();
        self.mcu.gpio.borrow_mut().drive(port(TP_IRQ.0), TP_IRQ.1, Some(level));
    }

    /// Backlight brightness 0.0–1.0 (TIM4_CH4 PWM on PD15, or plain GPIO).
    pub fn backlight(&self) -> f32 {
        let g = self.mcu.gpio.borrow();
        let p = &g.ports[port(LCD_BL.0)];
        match p.mode(LCD_BL.1) {
            PinMode::Alternate => {
                let af = (p.afr[1] >> (4 * (LCD_BL.1 - 8))) & 0xF;
                if af == 2 { self.mcu.tim_pwm[2].get()[3].unwrap_or(0.0) } else { 0.0 }
            }
            PinMode::Output if p.level(LCD_BL.1) => 1.0,
            _ => 0.0,
        }
    }

    /// Changes whenever the display content may have changed.
    pub fn display_generation(&self) -> u64 {
        self.lcd.borrow().generation()
    }

    /// Renders the display as seen on the board into `rgba`
    /// (`SCREEN_WIDTH * SCREEN_HEIGHT * 4` bytes), including backlight.
    pub fn render_display(&self, rgba: &mut [u8]) {
        assert_eq!(rgba.len(), SCREEN_WIDTH * SCREEN_HEIGHT * 4);
        let lcd = self.lcd.borrow();
        // Perceived brightness of an LED backlight is far from linear in the
        // PWM duty cycle (80 % looks almost fully bright).
        let bl = self.backlight().powf(1.0 / 2.2);
        for vy in 0..SCREEN_HEIGHT {
            for vx in 0..SCREEN_WIDTH {
                let (px, py) = screen_to_panel(vx, vy);
                let c = lcd.panel_pixel(px, py);
                let i = 4 * (vy * SCREEN_WIDTH + vx);
                rgba[i] = ((c >> 16) as u8 as f32 * bl) as u8;
                rgba[i + 1] = ((c >> 8) as u8 as f32 * bl) as u8;
                rgba[i + 2] = (c as u8 as f32 * bl) as u8;
                rgba[i + 3] = 0xFF;
            }
        }
    }
}

/// The panel (native portrait, 320 × 480) is mounted rotated on the board.
/// With the configuration used by the course library (MADCTL MV, SS = GS = 1)
/// this makes firmware coordinates (x right, y down) appear upright.
fn screen_to_panel(vx: usize, vy: usize) -> (usize, usize) {
    (GRAM_COLS - 1 - vy, GRAM_ROWS - 1 - vx)
}

/// Inverse of the default touch calibration of the course library
/// (`TP_SetDefaultCalibration`, scan direction D2U_L2R):
///   x = 0.132443 * Y - 37,   y = 341 - 0.089997 * X
/// The +0.5 places the result in the middle of the touched pixel.
fn screen_to_raw_touch(x: f32, y: f32) -> RawTouch {
    let x = x.clamp(0.0, (SCREEN_WIDTH - 1) as f32).floor();
    let y = y.clamp(0.0, (SCREEN_HEIGHT - 1) as f32).floor();
    let raw_y = ((x + 37.0 + 0.5) / 0.132_443).round().clamp(0.0, 4095.0) as u16;
    let raw_x = ((341.0 - y - 0.5) / 0.089_997).round().clamp(0.0, 4095.0) as u16;
    RawTouch { x: raw_x, y: raw_y, z1: 600, z2: 2500 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_mapping_matches_course_calibration() {
        for (x, y) in [(0.0, 0.0), (479.0, 319.0), (240.0, 160.0), (13.0, 300.0)] {
            let raw = screen_to_raw_touch(x, y);
            // Same arithmetic as TP_Scan() (float, truncated to uint16)
            let cx = (479.0f32 - (-0.132_443f32) * f32::from(raw.y) - 516.0) as u16;
            let cy = (319.0f32 - 0.089_997f32 * f32::from(raw.x) - (-22.0)) as u16;
            assert_eq!((cx, cy), (x as u16, y as u16), "raw {raw:?}");
        }
    }

    #[test]
    fn mounting_shows_firmware_coordinates_upright() {
        // Firmware writes pixel (x, y) via MADCTL MV: GRAM (col = y, row = x).
        // B6 SS/GS mirror the scan: panel (319 - y, 479 - x).
        for (x, y) in [(0usize, 0usize), (479, 0), (0, 319), (100, 50)] {
            let panel = (GRAM_COLS - 1 - y, GRAM_ROWS - 1 - x);
            assert_eq!(screen_to_panel(x, y), panel);
        }
    }
}
