//! Drawing of the ITS-Board PCB and its interactive parts.
//!
//! Everything is laid out in design units (the board is 1000 × 650) after
//! photos of the real board (Rev 02, 15.12.2020), seen with the LED row
//! along the bottom edge.

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

pub const BOARD_W: f32 = 1040.0;
pub const BOARD_H: f32 = 650.0;

const PCB: Color32 = Color32::from_rgb(0x1E, 0x40, 0x8E);
const PCB_EDGE: Color32 = Color32::from_rgb(0x12, 0x2A, 0x62);
const SILK: Color32 = Color32::from_rgb(0xEC, 0xF0, 0xF8);
const GOLD: Color32 = Color32::from_rgb(0xD9, 0xA4, 0x41);
const HOLE: Color32 = Color32::from_rgb(0x1A, 0x1A, 0x1A);
const LED_BODY: Color32 = Color32::from_rgb(0xEC, 0xE9, 0xDF);
const LED_YELLOW: Color32 = Color32::from_rgb(0xFF, 0xD0, 0x1A);
const LED_BLUE: Color32 = Color32::from_rgb(0x2F, 0x7B, 0xFF);
const LED_RED: Color32 = Color32::from_rgb(0xFF, 0x2A, 0x22);
const LED_GREEN: Color32 = Color32::from_rgb(0x2E, 0xE0, 0x5A);
const CAP_PINK: Color32 = Color32::from_rgb(0xE9, 0x5B, 0x74);
const NUCLEO_WHITE: Color32 = Color32::from_rgb(0xF3, 0xF3, 0xEF);
const METAL: Color32 = Color32::from_rgb(0xB9, 0xBD, 0xC4);

/// State of the board shown in this frame.
pub struct BoardState<'a> {
    pub led_duty: &'a [f32; 16],
    pub inputs: u8,
    pub nucleo_duty: &'a [f32; 3],
    pub display: Option<&'a egui::TextureHandle>,
    pub ints: &'a [Option<bool>; 6],
    pub latched: &'a [bool; 8],
}

/// What the user did on the board in this frame.
#[derive(Default)]
pub struct BoardInput {
    /// S0..S7 held down (mouse) this frame.
    pub buttons: [bool; 8],
    /// S0..S7 right-clicked (toggle latch).
    pub latch_toggled: [bool; 8],
    pub user: bool,
    pub reset_clicked: bool,
    /// Touch position in display pixels.
    pub touch: Option<(f32, f32)>,
    /// AUX pad clicked (cycle open → GND → 3V3).
    pub int_clicked: Option<usize>,
}

/// Maps design units to screen coordinates.
struct Pcb<'a> {
    painter: &'a Painter,
    origin: Pos2,
    s: f32,
}

impl Pcb<'_> {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + Vec2::new(x, y) * self.s
    }
    fn r(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::from_min_max(self.p(x0, y0), self.p(x1, y1))
    }
    fn len(&self, v: f32) -> f32 {
        v * self.s
    }
    fn text(&self, x: f32, y: f32, text: &str, size: f32, color: Color32, align: Align2) {
        if self.len(size) >= 5.0 {
            self.painter.text(self.p(x, y), align, text, FontId::proportional(self.len(size)), color);
        }
    }
    fn label(&self, x: f32, y: f32, text: &str, size: f32) {
        self.text(x, y, text, size, SILK, Align2::CENTER_CENTER);
    }
    fn rect(&self, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32, fill: Color32) {
        self.painter.rect_filled(self.r(x0, y0, x1, y1), CornerRadius::same(self.len(radius) as u8), fill);
    }
    fn outline(&self, x0: f32, y0: f32, x1: f32, y1: f32, color: Color32, width: f32) {
        self.painter.rect_stroke(self.r(x0, y0, x1, y1), CornerRadius::ZERO, Stroke::new(self.len(width).max(1.0), color), StrokeKind::Middle);
    }
    fn circle(&self, x: f32, y: f32, r: f32, fill: Color32) {
        self.painter.circle_filled(self.p(x, y), self.len(r), fill);
    }
    /// Plated through-hole pad.
    fn pad(&self, x: f32, y: f32, r: f32) {
        self.circle(x, y, r, GOLD);
        self.circle(x, y, r * 0.45, HOLE);
    }
    /// Section frame with title, as printed on the PCB.
    fn section(&self, x0: f32, y0: f32, x1: f32, y1: f32, title: &str) {
        self.outline(x0, y0, x1, y1, SILK.gamma_multiply(0.85), 1.2);
        self.text(x0 + 6.0, y0 + 4.0, title, 15.0, SILK, Align2::LEFT_TOP);
    }

    /// SMD LED (vertical 0805 package) with glow according to `brightness`.
    fn led(&self, x: f32, y: f32, color: Color32, brightness: f32) {
        let b = brightness.clamp(0.0, 1.0).powf(1.0 / 2.2); // perceived brightness
        if b > 0.01 {
            for (r, a) in [(22.0, 0.10), (15.0, 0.18), (10.0, 0.30)] {
                self.circle(x, y, r, color.gamma_multiply(a * b));
            }
        }
        // solder pads, body, lit chip
        self.rect(x - 5.0, y - 9.0, x + 5.0, y - 5.5, 1.0, METAL);
        self.rect(x - 5.0, y + 5.5, x + 5.0, y + 9.0, 1.0, METAL);
        let body = lerp(LED_BODY, color, 0.25 + 0.75 * b);
        self.rect(x - 4.5, y - 6.0, x + 4.5, y + 6.0, 1.0, if b > 0.01 { body } else { LED_BODY });
        if b > 0.01 {
            self.circle(x, y, 2.5, lerp(color, Color32::WHITE, 0.6 * b));
        }
    }

    /// Tactile switch; `down` draws the cap pressed.
    fn tact(&self, x: f32, y: f32, cap: Color32, down: bool, latched: bool) {
        for (dx, dy) in [(-11.0, -9.0), (11.0, -9.0), (-11.0, 9.0), (11.0, 9.0)] {
            self.rect(x + dx - 3.0, y + dy - 2.0, x + dx + 3.0, y + dy + 2.0, 0.5, METAL);
        }
        self.rect(x - 11.0, y - 11.0, x + 11.0, y + 11.0, 2.0, Color32::from_rgb(0x26, 0x28, 0x2C));
        let r = if down { 6.3 } else { 7.0 };
        let c = if down { cap.gamma_multiply(0.7) } else { cap };
        self.circle(x, y + if down { 0.6 } else { 0.0 }, r, c);
        if !down {
            self.circle(x - 2.0, y - 2.0, 2.2, Color32::from_white_alpha(60));
        }
        if latched {
            self.painter.circle_stroke(self.p(x, y), self.len(13.5), Stroke::new(self.len(1.5).max(1.0), Color32::from_rgb(0xFF, 0xE0, 0x40)));
        }
    }
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t) as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Column x positions of the Out section (OUT15 … OUT0, left to right).
fn out_x(bit: u32) -> f32 {
    66.0 + (15 - bit) as f32 * 36.0
}

/// Column x positions of the In section (IN7 … IN0).
fn in_x(bit: u32) -> f32 {
    684.0 + (7 - bit) as f32 * 37.0
}

/// Display glass in design units (480 × 320 pixels).
const GLASS: (f32, f32, f32, f32) = (505.0, 46.0, 965.0, 352.67);

pub fn show(ui: &mut egui::Ui, state: &BoardState) -> BoardInput {
    let avail = ui.available_size();
    let s = (avail.x / BOARD_W).min(avail.y / BOARD_H).max(0.4);
    let (outer, _) = ui.allocate_exact_size(Vec2::new(BOARD_W, BOARD_H) * s, Sense::hover());
    let painter = ui.painter_at(outer);
    let pcb = Pcb { painter: &painter, origin: outer.min, s };
    let mut input = BoardInput::default();

    // ---- PCB
    pcb.rect(0.0, 0.0, BOARD_W, BOARD_H, 16.0, PCB_EDGE);
    pcb.rect(3.0, 3.0, BOARD_W - 3.0, BOARD_H - 3.0, 14.0, PCB);
    for (x, y) in [(20.0, 20.0), (BOARD_W - 20.0, 20.0), (20.0, 630.0), (BOARD_W - 20.0, 630.0)] {
        pcb.circle(x, y, 11.0, GOLD.gamma_multiply(0.8));
        pcb.circle(x, y, 7.5, HOLE);
    }

    // ---- logo and board name
    for (i, h) in [34.0f32, 50.0, 62.0, 50.0, 34.0].iter().enumerate() {
        let x = 40.0 + i as f32 * 7.0;
        pcb.rect(x, 92.0 - h, x + 4.0, 92.0, 0.5, SILK);
    }
    pcb.text(84.0, 34.0, "HAW", 34.0, SILK, Align2::LEFT_TOP);
    pcb.text(84.0, 70.0, "HAMBURG", 18.0, SILK, Align2::LEFT_TOP);
    pcb.text(40.0, 104.0, "Department Informatik", 11.0, SILK, Align2::LEFT_TOP);
    pcb.text(40.0, 118.0, "Labor für technische Informatik", 9.5, SILK, Align2::LEFT_TOP);
    pcb.text(40.0, 134.0, "ITS-BRD", 24.0, SILK, Align2::LEFT_TOP);
    pcb.text(40.0, 162.0, "Rev 02 15.12.2020 JAN", 9.0, SILK, Align2::LEFT_TOP);

    // ---- RS232 (DB9 + MAX3232) and CAN
    pcb.rect(40.0, 196.0, 150.0, 250.0, 4.0, METAL);
    pcb.rect(52.0, 206.0, 138.0, 240.0, 6.0, Color32::from_gray(70));
    for i in 0..5 {
        pcb.circle(64.0 + i as f32 * 15.0, 216.0, 2.6, GOLD);
    }
    for i in 0..4 {
        pcb.circle(71.5 + i as f32 * 15.0, 230.0, 2.6, GOLD);
    }
    pcb.label(95.0, 260.0, "RS232", 11.0);
    pcb.rect(170.0, 204.0, 200.0, 244.0, 1.0, Color32::from_gray(25));
    pcb.label(185.0, 254.0, "U5", 8.0);
    for i in 0..4 {
        pcb.pad(234.0 + i as f32 * 18.0, 220.0, 5.0);
    }
    pcb.label(261.0, 238.0, "CAN", 11.0);

    // ---- I2C1 and SPI3
    pcb.section(30.0, 280.0, 315.0, 368.0, "I2C1");
    for (x0, name) in [(110.0, "I2C1_3V3"), (215.0, "I2C1_5V")] {
        pcb.rect(x0, 306.0, x0 + 80.0, 322.0, 1.0, Color32::from_gray(20));
        for i in 0..4 {
            pcb.rect(x0 + 8.0 + i as f32 * 20.0, 311.0, x0 + 13.0 + i as f32 * 20.0, 317.0, 0.5, GOLD);
        }
        pcb.label(x0 + 40.0, 300.0, name, 8.0);
    }
    for (i, (name, pin)) in [("GND", ""), ("3V3", ""), ("I2C1_SDA", "PB9"), ("I2C1_SCL", "PB8")].iter().enumerate() {
        let x = 70.0 + i as f32 * 68.0;
        pcb.pad(x, 340.0, 6.0);
        pcb.label(x, 356.0, format!("{name} {pin}").trim_end(), 7.5);
    }
    pcb.section(30.0, 378.0, 315.0, 462.0, "SPI3");
    for (i, (name, pin)) in [("SPI3_CS", "PA15"), ("SPI3_MISO", "PC11"), ("SPI3_MOSI", "PC12"), ("SPI3_SCK", "PC10")].iter().enumerate() {
        let x = 70.0 + i as f32 * 68.0;
        pcb.pad(x, 418.0, 6.0);
        pcb.label(x, 436.0, name, 7.5);
        pcb.label(x, 448.0, pin, 7.5);
    }

    // ---- Nucleo-144 (only the part beside the display is visible)
    pcb.rect(330.0, 14.0, 500.0, 372.0, 4.0, NUCLEO_WHITE);
    pcb.rect(372.0, 10.0, 428.0, 44.0, 2.0, METAL); // USB (ST-LINK)
    pcb.label(400.0, 50.0, "CN1", 7.0);
    pcb.rect(440.0, 18.0, 490.0, 62.0, 2.0, METAL); // RJ45
    for i in 0..30 {
        let y = 112.0 + i as f32 * 8.6;
        pcb.rect(336.0, y, 341.0, y + 5.0, 0.5, Color32::from_gray(40));
        pcb.rect(344.0, y, 349.0, y + 5.0, 0.5, Color32::from_gray(40));
    }
    pcb.text(362.0, 72.0, "RESET B2", 7.5, Color32::from_gray(60), Align2::CENTER_CENTER);
    pcb.text(436.0, 72.0, "USER B1", 7.5, Color32::from_gray(60), Align2::CENTER_CENTER);
    for (i, (name, color)) in [("LD1", LED_GREEN), ("LD2", LED_BLUE), ("LD3", LED_RED)].iter().enumerate() {
        let x = 384.0 + i as f32 * 30.0;
        pcb.led(x, 140.0, *color, state.nucleo_duty[i]);
        pcb.text(x, 160.0, name, 8.0, Color32::from_gray(60), Align2::CENTER_CENTER);
    }
    pcb.text(415.0, 196.0, "NUCLEO", 15.0, Color32::from_rgb(0x03, 0x23, 0x4B), Align2::CENTER_CENTER);
    pcb.text(415.0, 214.0, "-F429ZI", 13.0, Color32::from_rgb(0x03, 0x23, 0x4B), Align2::CENTER_CENTER);
    pcb.rect(382.0, 240.0, 448.0, 300.0, 2.0, Color32::from_gray(30)); // MCU / ST-LINK
    pcb.text(415.0, 270.0, "STM32F429ZIT6", 7.0, Color32::from_gray(150), Align2::CENTER_CENTER);
    pcb.text(415.0, 330.0, "ST", 22.0, Color32::from_rgb(0x03, 0x8C, 0xD8), Align2::CENTER_CENTER);

    // ---- Waveshare 4" TFT shield and display
    pcb.rect(492.0, 24.0, 980.0, 380.0, 4.0, Color32::from_rgb(0x1A, 0x2B, 0x3E));
    let (gx0, gy0, gx1, gy1) = GLASS;
    pcb.rect(gx0 - 8.0, gy0 - 8.0, gx1 + 8.0, gy1 + 8.0, 3.0, Color32::from_gray(10));
    let glass = pcb.r(gx0, gy0, gx1, gy1);
    match state.display {
        Some(tex) => {
            painter.image(tex.id(), glass, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            painter.rect_filled(glass, CornerRadius::ZERO, Color32::BLACK);
        }
    }
    // faint reflection on the glass
    painter.rect_filled(
        Rect::from_min_max(glass.min, Pos2::new(glass.max.x, glass.min.y + glass.height() * 0.08)),
        CornerRadius::ZERO,
        Color32::from_white_alpha(6),
    );
    pcb.text(gx0, gy1 + 16.0, "4inch TFT Touch Shield  ·  480 × 320  ·  ILI9486 / XPT2046", 8.0, SILK.gamma_multiply(0.7), Align2::LEFT_CENTER);

    // ---- supply indicators beside the display (lit while the board is powered)
    for (x, name, supply) in [(1000.0, "D25", "5V"), (1022.0, "D24", "3V3")] {
        pcb.label(x, 54.0, name, 7.0);
        pcb.led(x, 72.0, LED_YELLOW, 1.0);
        pcb.label(x, 91.0, supply, 7.5);
    }

    // ---- Aux (INT0..5 = PG0..5) and Analog
    pcb.section(330.0, 390.0, 500.0, 462.0, "Aux");
    for n in 0..6usize {
        let x = 360.0 + (n % 3) as f32 * 52.0;
        let y = if n < 3 { 420.0 } else { 446.0 };
        let level = state.ints[n];
        pcb.pad(x, y, 6.0);
        let (txt, color) = match level {
            None => ("open", SILK.gamma_multiply(0.6)),
            Some(false) => ("GND", Color32::from_rgb(0x9C, 0xD3, 0xFF)),
            Some(true) => ("3V3", Color32::from_rgb(0xFF, 0x9C, 0x8C)),
        };
        pcb.text(x + 9.0, y - 5.0, &format!("AUX{n}"), 7.0, SILK, Align2::LEFT_CENTER);
        pcb.text(x + 9.0, y + 5.0, txt, 7.0, color, Align2::LEFT_CENTER);
        let hit = ui
            .interact(pcb.r(x - 9.0, y - 9.0, x + 34.0, y + 9.0), ui.scope_id().with(("aux", n)), Sense::click())
            .on_hover_text(format!("AUX{n} / INT{n} = PG{n} — click: open → GND → 3V3"));
        if hit.clicked() {
            input.int_clicked = Some(n);
        }
    }
    pcb.section(510.0, 390.0, 980.0, 462.0, "Analog");
    for (i, (name, pin)) in [
        ("DAC1", "PA4"),
        ("ADC123_13", "PC3"),
        ("ADC123_12", "PC2"),
        ("ADC123_10", "PC0"),
        ("ADC12_9", "PB1"),
        ("ADC12_8", "PB0"),
        ("ADC3_9", "PF3"),
        ("GNDA", ""),
    ]
    .iter()
    .enumerate()
    {
        let x = 548.0 + i as f32 * 56.0;
        pcb.pad(x, 424.0, 6.0);
        pcb.label(x, 440.0, name, 7.0);
        pcb.label(x, 451.0, pin, 7.0);
    }

    // ---- Out: OUT15..OUT8 (PE7..PE0, yellow), OUT7..OUT0 (PD7..PD0, blue)
    pcb.section(40.0, 472.0, 640.0, 624.0, "Out");
    for bit in 0..16u32 {
        let x = out_x(bit);
        let (color, pin) = if bit >= 8 { (LED_YELLOW, format!("PE{}", bit - 8)) } else { (LED_BLUE, format!("PD{bit}")) };
        pcb.label(x, 496.0, &format!("D{}", bit + 8), 8.5);
        pcb.led(x, 513.0, color, state.led_duty[bit as usize]);
        pcb.label(x, 533.0, &pin, 8.0);
        pcb.pad(x, 552.0, 7.5);
        pcb.label(x, 572.0, &format!("OUT{bit}"), 8.5);
        ui.interact(pcb.r(x - 15.0, 488.0, x + 15.0, 580.0), ui.scope_id().with(("out", bit)), Sense::hover())
            .on_hover_text(format!("OUT{bit} = {pin} (LED D{}): {:.0} %", bit + 8, state.led_duty[bit as usize] * 100.0));
    }
    pcb.pad(66.0, 602.0, 6.0);
    pcb.text(78.0, 602.0, "GND", 8.0, SILK, Align2::LEFT_CENTER);

    // ---- In: IN7..IN0 (PF7..PF0), red LEDs show the line level, buttons S7..S0
    pcb.section(650.0, 472.0, 962.0, 624.0, "In");
    for bit in 0..8u32 {
        let x = in_x(bit);
        let high = state.inputs & (1 << bit) != 0;
        pcb.label(x, 496.0, &format!("D{bit}"), 8.5);
        pcb.led(x, 513.0, LED_RED, if high { 1.0 } else { 0.0 });
        pcb.label(x, 532.0, &format!("PF{bit}"), 8.0);
        pcb.pad(x, 548.0, 7.0);
        pcb.label(x, 564.0, &format!("IN{bit}"), 8.5);
        let cap_rect = pcb.r(x - 13.0, 575.0, x + 13.0, 603.0);
        let resp = ui
            .interact(cap_rect, ui.scope_id().with(("button", bit)), Sense::click_and_drag())
            .on_hover_text(format!("S{bit} → IN{bit} (PF{bit}) · key {bit} · right click latches"));
        let held = resp.is_pointer_button_down_on();
        input.buttons[bit as usize] = held;
        input.latch_toggled[bit as usize] = resp.secondary_clicked();
        pcb.tact(x, 589.0, CAP_PINK, held || state.latched[bit as usize] || !high, state.latched[bit as usize]);
        pcb.label(x, 612.0, &format!("S{bit}"), 8.0);
    }

    // ---- Nucleo buttons (drawn last so their hit areas are on top)
    let user = ui.interact(pcb.r(422.0, 80.0, 450.0, 108.0), ui.scope_id().with("user"), Sense::click_and_drag()).on_hover_text("USER button (PC13)");
    input.user = user.is_pointer_button_down_on();
    pcb.tact(436.0, 94.0, Color32::from_rgb(0x1F, 0x6F, 0xE5), input.user, false);
    let reset = ui.interact(pcb.r(348.0, 80.0, 376.0, 108.0), ui.scope_id().with("reset"), Sense::click()).on_hover_text("RESET button");
    input.reset_clicked = reset.clicked();
    pcb.tact(362.0, 94.0, Color32::from_gray(25), reset.is_pointer_button_down_on(), false);

    // ---- touch on the display glass
    let touch = ui.interact(glass, ui.scope_id().with("display"), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::Crosshair);
    let to_px = |p: Pos2| ((p.x - glass.min.x) / glass.width() * 480.0, (p.y - glass.min.y) / glass.height() * 320.0);
    input.touch = touch.is_pointer_button_down_on().then(|| touch.interact_pointer_pos()).flatten().filter(|p| glass.contains(*p)).map(to_px);
    if let Some((x, y)) = input.touch.or_else(|| touch.hover_pos().map(to_px)) {
        touch.on_hover_text_at_pointer(format!("x = {:.0}, y = {:.0}", x.floor(), y.floor()));
    }
    input
}
