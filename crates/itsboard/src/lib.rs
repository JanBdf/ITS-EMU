//! The HAW Hamburg ITS-Board: NUCLEO-F429ZI on the ITS carrier board with the
//! Waveshare 4" TFT touch shield.
//!
//! | Function            | Pins                                   |
//! |---------------------|----------------------------------------|
//! | LEDs OUT0–7 / 8–15  | PD0–7 / PE0–7                          |
//! | Buttons S0–S7       | PF0–7 (pressed = low, external pull-up) |
//! | INT0–5 / AUX0–5     | PG0–5                                  |
//! | Nucleo LD1/LD2/LD3  | PB0 / PB7 / PB14                       |
//! | Nucleo user button  | PC13 (pressed = high)                  |
//! | Analog header       | DAC1 = PA4, PC3, PC2, PC0, PB1, PF10, PF9, PF8 |
//! | TFT (ILI9486)       | SPI1, CS PD14, DC PF13, RST PF12, BL PD15 (TIM4_CH4) |
//! | Touch (XPT2046)     | SPI1, CS PF14, PENIRQ PE13, BUSY PB10  |

pub mod devices;
pub mod ili9486;
pub mod sim;
pub mod xpt2046;

pub use sim::Simulator;

use std::cell::RefCell;
use std::rc::Rc;

use devices::{DeviceHub, Ds18b20, OneWireBus, RotaryEncoder, SharedDevices};
use emu_core::Bus;
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
/// Input pins that external circuitry drives, measured on the real board
/// (HwCheck `pins`: high with the MCU pull-down, resp. low with the pull-up).
/// The buttons (PF0–7) and the touch IRQ (PE13) are modelled separately.
const EXTERNAL_LEVELS: &[(char, u32, bool)] = &[
    ('A', 1, false),  // RMII_REF_CLK from the Ethernet PHY
    ('A', 2, true),   // RMII_MDIO pull-up
    ('A', 9, false),  // USB_VBUS sense (USB OTG not connected)
    ('B', 8, true),   // I2C1_SCL pull-up on the carrier board
    ('B', 9, true),   // I2C1_SDA pull-up on the carrier board
    ('B', 14, false), // LD3 (red LED to GND) loads the pin
    ('C', 4, false),  // RMII_RXD0
    ('C', 5, false),  // RMII_RXD1
    ('E', 11, true),
    ('G', 6, false),
    ('G', 7, true),
    ('G', 9, true),
];

/// Levels that floating inputs (no pull, not driven) read on the real board,
/// per port A..K (HwCheck `pins`). Not guaranteed by the hardware, but they
/// give typical port values such as GPIOF->IDR = 0xFCFF.
const FLOATING_LEVELS: [u16; 11] = [
    0x05C0, // A: PA6 (SPI1_MISO), PA7 (SPI1_MOSI), PA8, PA10
    0x3C48, // B: PB3 (SWO), PB6, PB10 (touch BUSY, high-Z while not selected), PB11..13
    0x97C2, // C
    0xBF00, // D: PD8/PD9 (USART3 idle), PD10..13, PD15 (backlight PWM, mostly high)
    0x5500, // E: PE8, PE10, PE12, PE14
    0x8C00, // F: PF10, PF11, PF15
    0xB500, // G: PG8, PG10, PG12, PG13, PG15
    0, 0, 0, 0,
];
const USER_BUTTON: (char, u32) = ('C', 13);

/// Signal pads of the Analog header (left to right; GNDA pads omitted).
pub const ANALOG_PADS: [(&str, char, u32); 8] = [
    ("DAC1", 'A', 4),
    ("ADC123_13", 'C', 3),
    ("ADC123_12", 'C', 2),
    ("ADC123_10", 'C', 0),
    ("ADC12_9", 'B', 1),
    ("ADC3_8", 'F', 10),
    ("ADC3_7", 'F', 9),
    ("ADC3_6", 'F', 8),
];

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

/// Time-averaged LED brightness, see [`ItsBoard::take_led_duty`].
#[derive(Debug, Clone, Copy, Default)]
pub struct LedDuty {
    /// OUT0..OUT15
    pub out: [f32; 16],
    /// LD1 (green), LD2 (blue), LD3 (red)
    pub nucleo: [f32; 3],
}

pub struct ItsBoard {
    mcu: Stm32f429,
    lcd: Rc<RefCell<Ili9486>>,
    touch: Rc<RefCell<Xpt2046>>,
    devices: SharedDevices,
    encoders: RefCell<Vec<Rc<RefCell<RotaryEncoder>>>>,
    /// DS18B20 sensors: their 1-Wire line and their place on it.
    sensors: RefCell<Vec<(Rc<RefCell<OneWireBus>>, usize)>>,
}

impl ItsBoard {
    /// Connects the board to an MCU instance (and external devices, added
    /// later, to its bus).
    pub fn attach(mcu: &Stm32f429, bus: &mut Bus) -> Self {
        let devices = SharedDevices::default();
        bus.attach_device(Box::new(DeviceHub(devices.clone())));
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
        let board =
            Self { mcu: mcu.clone(), lcd, touch, devices, encoders: RefCell::default(), sensors: RefCell::default() };
        {
            let mut g = board.mcu.gpio.borrow_mut();
            for &(p, pin, level) in EXTERNAL_LEVELS {
                g.drive(port(p), pin, Some(level));
            }
            for (i, &levels) in FLOATING_LEVELS.iter().enumerate() {
                g.set_floating_levels(i, levels);
            }
        }
        for n in 0..8 {
            board.set_button(n, false);
        }
        board.sync_touch_irq();
        board
    }

    pub fn mcu(&self) -> &Stm32f429 {
        &self.mcu
    }

    /// Connects a rotary encoder (contacts A, B to GND, pull-ups on the
    /// module) to two pins; returns its number for [`Self::rotate_encoder`].
    pub fn add_encoder(&self, a: (char, u32), b: (char, u32)) -> usize {
        let enc = Rc::new(RefCell::new(RotaryEncoder::new(self.mcu.gpio.clone(), a, b)));
        self.devices.borrow_mut().push(enc.clone());
        let mut encoders = self.encoders.borrow_mut();
        encoders.push(enc);
        encoders.len() - 1
    }

    /// Connects a DS18B20 temperature sensor (1-Wire, external power, pull-up
    /// on the module) to a pin; returns its number for [`Self::set_temperature`].
    /// Several sensors on one pin share the line (Search ROM finds them).
    pub fn add_ds18b20(&self, pin: (char, u32)) -> usize {
        let mut sensors = self.sensors.borrow_mut();
        let serial = 0x0000_0B1A_D5A1 + 0x1_0000 * sensors.len() as u64; // distinct ROM codes
        let at = (port(pin.0), pin.1);
        let bus = match sensors.iter().find(|(b, _)| b.borrow().pin() == at) {
            Some((b, _)) => b.clone(),
            None => {
                let b = Rc::new(RefCell::new(OneWireBus::new(self.mcu.gpio.clone(), pin)));
                self.devices.borrow_mut().push(b.clone());
                b
            }
        };
        let index = {
            let mut b = bus.borrow_mut();
            b.sensors.push(Ds18b20::new(serial));
            b.sensors.len() - 1
        };
        sensors.push((bus, index));
        sensors.len() - 1
    }

    /// Temperature at DS18B20 `n` (°C, -55..125); the next conversion measures it.
    pub fn set_temperature(&self, n: usize, celsius: f32) -> Result<(), String> {
        let sensors = self.sensors.borrow();
        let (bus, i) = sensors.get(n).ok_or_else(|| format!("there is no DS18B20 {n}"))?;
        bus.borrow_mut().sensors[*i].set_celsius(celsius);
        Ok(())
    }

    /// ROM code (family, serial, CRC) of DS18B20 `n`.
    pub fn ds18b20_rom(&self, n: usize) -> Option<[u8; 8]> {
        self.sensors.borrow().get(n).map(|(bus, i)| bus.borrow().sensors[*i].rom())
    }

    /// Turns encoder `n` by `detents` (negative: counter-clockwise), one
    /// detent every few milliseconds of virtual time.
    pub fn rotate_encoder(&self, n: usize, detents: i32) -> Result<(), String> {
        let encoders = self.encoders.borrow();
        let enc = encoders.get(n).ok_or_else(|| format!("there is no encoder {n}"))?;
        enc.borrow_mut().rotate(detents);
        Ok(())
    }

    /// Presses (`true`) or releases button S0–S7.
    pub fn set_button(&self, n: u32, pressed: bool) {
        assert!(n < 8, "the ITS-Board has buttons S0..S7");
        // Pressed buttons pull the pin to GND; released ones are pulled up on
        // the carrier board (high even with the MCU's pull-down enabled).
        self.mcu.gpio.borrow_mut().drive(port('F'), n, Some(!pressed));
    }

    /// Drives one of the INT0–5 inputs (PG0–5); `None` leaves it open.
    pub fn set_int(&self, n: u32, level: Option<bool>) {
        assert!(n < 6, "the ITS-Board has inputs INT0..INT5");
        self.mcu.gpio.borrow_mut().drive(port('G'), n, level);
    }

    /// Applies a voltage to pad `n` of the Analog header ([`ANALOG_PADS`]);
    /// `None` leaves it open.
    pub fn set_analog(&self, n: usize, volts: Option<f32>) {
        let (_, p, pin) = ANALOG_PADS[n];
        self.mcu.analog.borrow_mut().set_pin(port(p), pin, volts);
    }

    /// Output voltage of DAC1 (on the DAC1 pad) while it is enabled.
    pub fn dac1(&self) -> Option<f32> {
        self.mcu.analog.borrow().dac_output(0)
    }

    pub fn set_user_button(&self, pressed: bool) {
        // Released: only a weak pull-down on the Nucleo (the MCU's pull-up wins).
        self.mcu.gpio.borrow_mut().drive(port(USER_BUTTON.0), USER_BUTTON.1, pressed.then_some(true));
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

    /// Average brightness (0.0–1.0) of OUT0–15 and of the Nucleo LEDs
    /// LD1–LD3 since the previous call; `now` is the current cycle count.
    /// Shows PWM-dimmed or multiplexed LEDs the way the eye sees them.
    pub fn take_led_duty(&self, now: u64) -> LedDuty {
        let mut g = self.mcu.gpio.borrow_mut();
        let d = g.take_duty(port('D'), now);
        let e = g.take_duty(port('E'), now);
        let b = g.take_duty(port('B'), now);
        let mut out = [0.0; 16];
        out[..8].copy_from_slice(&d[..8]);
        out[8..].copy_from_slice(&e[..8]);
        LedDuty { out, nucleo: [b[0], b[7], b[14]] }
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
        let mut frame = DisplayFrame::default();
        self.capture_display(&mut frame);
        frame.render(rgba);
    }

    /// Like [`Self::render_display`], but with the tone response and black
    /// level measured on the real panel: what the display looks like on the
    /// board rather than the ideal colours of the frame buffer.
    pub fn render_display_as_seen(&self, rgba: &mut [u8]) {
        let mut frame = DisplayFrame::default();
        self.capture_display(&mut frame);
        frame.render_as_seen(rgba);
    }

    /// Copies the display's content and backlight into `frame` (reusing its
    /// buffer): cheap, the colours are computed by [`DisplayFrame::render`]
    /// (possibly on another thread).
    pub fn capture_display(&self, frame: &mut DisplayFrame) {
        self.lcd.borrow().capture(&mut frame.panel);
        frame.backlight = self.backlight();
    }
}

/// The display at one moment (see [`ItsBoard::capture_display`]).
#[derive(Clone, Debug, Default)]
pub struct DisplayFrame {
    panel: ili9486::PanelFrame,
    backlight: f32,
}

impl DisplayFrame {
    /// See [`ItsBoard::render_display`].
    pub fn render(&self, rgba: &mut [u8]) {
        // Perceived brightness of an LED backlight is far from linear in the
        // PWM duty cycle (80 % looks almost fully bright).
        let bl = self.backlight.powf(1.0 / 2.2);
        self.render_with(rgba, |c| (f32::from(c) * bl) as u8);
    }

    /// See [`ItsBoard::render_display_as_seen`].
    pub fn render_as_seen(&self, rgba: &mut [u8]) {
        if self.panel.is_dark() {
            rgba.as_chunks_mut::<4>().0.fill([0, 0, 0, 0xFF]);
            return;
        }
        // The backlight's luminance is linear in the PWM duty cycle (measured).
        let bl = self.backlight;
        self.render_with(rgba, |c| panel_channel(c, bl));
    }

    /// Renders with `channel` mapping an 8-bit colour component to the output.
    fn render_with(&self, rgba: &mut [u8], channel: impl Fn(u8) -> u8) {
        assert_eq!(rgba.len(), SCREEN_WIDTH * SCREEN_HEIGHT * 4);
        if self.panel.is_dark() {
            // black before the backlight: what `channel` makes of 0
            let black = channel(0);
            rgba.as_chunks_mut::<4>().0.fill([black, black, black, 0xFF]);
            return;
        }
        // The 5- and 6-bit levels expanded to 8 bits as the panel model does.
        let lut5: [u8; 32] = std::array::from_fn(|v| channel((v * 255 / 31) as u8));
        let lut6: [u8; 64] = std::array::from_fn(|v| channel((v * 255 / 63) as u8));
        let positions = (0..SCREEN_HEIGHT).flat_map(|vy| (0..SCREEN_WIDTH).map(move |vx| screen_to_panel(vx, vy)));
        let mut out = rgba.as_chunks_mut::<4>().0.iter_mut();
        self.panel.for_each_rgb565(positions, |r, g, b| {
            if let Some(px) = out.next() {
                *px = [lut5[usize::from(r)], lut6[usize::from(g)], lut5[usize::from(b)], 0xFF];
            }
        });
    }
}

/// Luminance of the real panel for the 32 levels of a 5-bit channel,
/// relative to the full level (above black). Measured on the ITS-Board from
/// raw photos of grey ramps, flat-field corrected (HwCheck `bars`): the
/// lowest levels are black, mid levels darker than an sRGB monitor shows them.
const PANEL_LUMINANCE: [f32; 32] = [
    0.000, 0.000, 0.001, 0.001, 0.001, 0.001, 0.002, 0.005, 0.010, 0.019, 0.032, 0.042, 0.066, 0.081, 0.101, 0.130,
    0.146, 0.183, 0.228, 0.278, 0.327, 0.387, 0.443, 0.506, 0.573, 0.654, 0.710, 0.776, 0.835, 0.891, 0.943, 1.000,
];

/// Light that the panel lets through for black (backlight leakage), relative
/// to white: a dark grey, as on the real display. Measured 2.5 % from the
/// camera's oblique view including ambient light; less when looking straight
/// at the TN panel.
const PANEL_BLACK: f32 = 0.01;

/// sRGB-encoded 0..255 value of a channel level `v` (0..255 as returned by
/// `panel_pixel`) on the real panel, for `backlight` (relative luminance).
fn panel_channel(v: u8, backlight: f32) -> u8 {
    let x = f32::from(v) / 255.0 * 31.0;
    let i = (x.floor() as usize).min(30);
    let l = PANEL_LUMINANCE[i] + (PANEL_LUMINANCE[i + 1] - PANEL_LUMINANCE[i]) * (x - i as f32);
    let lin = (PANEL_BLACK + (1.0 - PANEL_BLACK) * l) * backlight;
    let srgb = if lin <= 0.003_130_8 { 12.92 * lin } else { 1.055 * lin.powf(1.0 / 2.4) - 0.055 };
    (srgb.clamp(0.0, 1.0) * 255.0).round() as u8
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
