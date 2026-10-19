//! Pin inspector: mode and level of every pin of ports A–G, with what the
//! ITS-Board connects to it.

use eframe::egui::{self, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use stm32f429::gpio::{PinMode, PortState, Pull};

/// Ports shown: A..G are complete on the LQFP144 package (PH0/PH1 are the
/// HSE input from the ST-LINK, ports I..K are not bonded out).
pub const PORTS: usize = 7;

/// One pin as the inspector shows it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PinView {
    pub mode: PinMode,
    /// Alternate function number (in alternate mode).
    pub af: u8,
    pub pull: Pull,
    pub open_drain: bool,
    /// Level at the input buffer.
    pub level: bool,
}

impl Default for PinView {
    fn default() -> Self {
        Self { mode: PinMode::Input, af: 0, pull: Pull::None, open_drain: false, level: false }
    }
}

pub type PinsView = [[PinView; 16]; PORTS];

pub fn view(ports: &[PortState]) -> PinsView {
    let mut v = PinsView::default();
    for (port, state) in ports.iter().take(PORTS).enumerate() {
        for pin in 0..16u32 {
            v[port][pin as usize] = PinView {
                mode: state.mode(pin),
                af: state.af(pin) as u8,
                pull: state.pull(pin),
                open_drain: state.otyper & (1 << pin) != 0,
                level: state.level(pin),
            };
        }
    }
    v
}

/// What the ITS-Board (and the Nucleo) connect to a pin.
fn board_use(port: usize, pin: usize) -> Option<String> {
    let p = |s: &str| Some(s.to_owned());
    match (b'A' + port as u8, pin) {
        (b'D', 0..=7) => Some(format!("OUT{pin} (LED D{})", pin + 8)),
        (b'E', 0..=7) => Some(format!("OUT{} (LED D{})", pin + 8, pin + 16)),
        (b'F', 0..=7) => Some(format!("IN{pin} (button S{pin}, LED D{pin})")),
        (b'G', 0..=5) => Some(format!("AUX{pin} / INT{pin}")),
        (b'A', 4) => p("DAC1 (Analog header)"),
        (b'A', 5) => p("SPI1_SCK (display, touch)"),
        (b'A', 6) => p("SPI1_MISO (display, touch)"),
        (b'A', 7) => p("SPI1_MOSI (display, touch)"),
        (b'A', 13) => p("SWDIO (ST-LINK)"),
        (b'A', 14) => p("SWCLK (ST-LINK)"),
        (b'A', 15) => p("SPI3_CS"),
        (b'B', 0) => p("Nucleo LD1 (green), TIM3_3 pad"),
        (b'B', 1) => p("ADC12_9 (Analog header)"),
        (b'B', 7) => p("Nucleo LD2 (blue)"),
        (b'B', 8) => p("I2C1_SCL"),
        (b'B', 9) => p("I2C1_SDA"),
        (b'B', 10) => p("TIM2_3 pad, touch BUSY"),
        (b'B', 11) => p("TIM2_4 pad"),
        (b'B', 14) => p("Nucleo LD3 (red)"),
        (b'C', 0) => p("ADC123_10 (Analog header)"),
        (b'C', 2) => p("ADC123_12 (Analog header)"),
        (b'C', 3) => p("ADC123_13 (Analog header)"),
        (b'C', 6..=9) => Some(format!("TIM8_{} pad", pin - 5)),
        (b'C', 10) => p("SPI3_SCK"),
        (b'C', 11) => p("SPI3_MISO"),
        (b'C', 12) => p("SPI3_MOSI"),
        (b'C', 13) => p("Nucleo USER button B1"),
        (b'D', 8) => p("USART3_TX (virtual COM port)"),
        (b'D', 9) => p("USART3_RX (virtual COM port)"),
        (b'D', 14) => p("display CS"),
        (b'D', 15) => p("display backlight (TIM4_CH4)"),
        (b'E', 8) => p("TIM1_1N pad"),
        (b'E', 10) => p("TIM1_2N pad"),
        (b'E', 13) => p("touch PENIRQ"),
        (b'E', 14) => p("TIM1_4 pad"),
        (b'F', 8) => p("ADC3_6 (Analog header)"),
        (b'F', 9) => p("ADC3_7 (Analog header)"),
        (b'F', 10) => p("ADC3_8 (Analog header)"),
        (b'F', 12) => p("display RST"),
        (b'F', 13) => p("display DC"),
        (b'F', 14) => p("touch CS"),
        _ => None,
    }
}

const IN_HI: Color32 = Color32::from_rgb(0x4d, 0x9d, 0xff);
const OUT_HI: Color32 = Color32::from_rgb(0x3d, 0xd6, 0x6a);
const AF_HI: Color32 = Color32::from_rgb(0xc0, 0x7c, 0xff);
const ANALOG: Color32 = Color32::from_rgb(0x8a, 0x8a, 0x80);

fn colors(v: &PinView) -> (Color32, &'static str) {
    let (hi, mode) = match v.mode {
        PinMode::Input => (IN_HI, "input"),
        PinMode::Output => (OUT_HI, "output"),
        PinMode::Alternate => (AF_HI, "alternate function"),
        PinMode::Analog => return (ANALOG.gamma_multiply(0.45), "analog"),
    };
    (if v.level { hi } else { hi.gamma_multiply(0.22) }, mode)
}

pub fn show(ui: &mut egui::Ui, pins: &PinsView) {
    ui.label(egui::RichText::new("Pins").strong());
    let cell = 17.0;
    let label_w = 26.0;
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(label_w + 16.0 * cell, cell * (PORTS as f32 + 1.0)), Sense::hover());
    let painter = ui.painter_at(rect);
    let text_color = ui.visuals().weak_text_color();
    for pin in 0..16 {
        let c = Pos2::new(rect.left() + label_w + (pin as f32 + 0.5) * cell, rect.top() + cell * 0.5);
        painter.text(c, egui::Align2::CENTER_CENTER, pin.to_string(), FontId::monospace(9.0), text_color);
    }
    for (port, row) in pins.iter().enumerate() {
        let y = rect.top() + cell * (port as f32 + 1.0);
        let name = format!("P{}", (b'A' + port as u8) as char);
        painter.text(
            Pos2::new(rect.left() + 2.0, y + cell * 0.5),
            egui::Align2::LEFT_CENTER,
            &name,
            FontId::monospace(11.0),
            ui.visuals().text_color(),
        );
        for (pin, v) in row.iter().enumerate() {
            let r = Rect::from_min_size(Pos2::new(rect.left() + label_w + pin as f32 * cell, y), Vec2::splat(cell))
                .shrink(1.5);
            let (fill, mode) = colors(v);
            painter.rect_filled(r, CornerRadius::same(3), fill);
            let used = board_use(port, pin);
            if used.is_some() {
                painter.rect_stroke(
                    r,
                    CornerRadius::same(3),
                    Stroke::new(1.0, Color32::from_white_alpha(70)),
                    egui::StrokeKind::Inside,
                );
            }
            let letter = match v.mode {
                PinMode::Input => "I".to_owned(),
                PinMode::Output => "O".to_owned(),
                PinMode::Alternate => format!("{:X}", v.af),
                PinMode::Analog => "~".to_owned(),
            };
            let ink = if v.level && v.mode != PinMode::Analog { Color32::BLACK } else { Color32::from_gray(200) };
            painter.text(r.center(), egui::Align2::CENTER_CENTER, letter, FontId::monospace(9.5), ink);
            let mut hover = format!("{name}{pin}: {mode}");
            if v.mode == PinMode::Alternate {
                hover += &format!(" AF{}", v.af);
            }
            if matches!(v.mode, PinMode::Output | PinMode::Alternate) {
                hover += if v.open_drain { ", open drain" } else { ", push-pull" };
            }
            match v.pull {
                Pull::Up => hover += ", pull-up",
                Pull::Down => hover += ", pull-down",
                Pull::None => {}
            }
            if v.mode != PinMode::Analog {
                hover += if v.level { " · high" } else { " · low" };
            }
            if let Some(u) = used {
                hover += &format!("\n{u}");
            }
            ui.interact(r, ui.scope_id().with(("pin", port, pin)), Sense::hover()).on_hover_text(hover);
        }
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        for (color, text) in
            [(IN_HI, "I input"), (OUT_HI, "O output"), (AF_HI, "0–F alternate function"), (ANALOG, "~ analog")]
        {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
            ui.painter().rect_filled(r, CornerRadius::same(2), color);
            ui.label(egui::RichText::new(text).small());
        }
    });
    ui.label(egui::RichText::new("bright: high, dark: low · framed: used on the board").small().weak());
}
