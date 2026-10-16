//! Drawing of the ITS-Board and its interactive parts.
//!
//! The board is a photo of the real board (Rev 02, 15.12.2020), focus- and
//! exposure-stacked from 30 shots and rectified (tools/hw/stack-capture.sh,
//! boardstack.py, boardasset.py; the lit IN LEDs are retouched to off).
//! LEDs, pressed buttons and the display are drawn on top of it.  Positions are
//! in design units (the photo is 1000 units wide) and were measured in the
//! photo by boardasset.py.

use eframe::egui::{
    self, Color32, CornerRadius, Mesh, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, TextureOptions, Vec2,
};

pub const BOARD_W: f32 = 1000.0;
pub const BOARD_H: f32 = 651.37;

const PHOTO: &[u8] = include_bytes!("../assets/board.jpg");
/// Rounded corners of the PCB.
const CORNER: f32 = 15.0;

const LED_YELLOW: Color32 = Color32::from_rgb(0xFF, 0xC4, 0x10);
const LED_BLUE: Color32 = Color32::from_rgb(0x30, 0x70, 0xFF);
const LED_RED: Color32 = Color32::from_rgb(0xFF, 0x20, 0x30);

/// OUT0..OUT15 LEDs (D8..D23), x of the package centre; all at `LED_Y`.
const OUT_X: [f32; 16] =
    [579.1, 547.8, 516.4, 484.9, 453.5, 421.9, 390.5, 359.0, 327.6, 296.1, 265.0, 233.7, 202.3, 170.9, 139.1, 107.1];
/// IN0..IN7 LEDs (D0..D7).
const IN_X: [f32; 8] = [905.9, 874.2, 842.9, 811.3, 779.5, 747.4, 715.8, 683.9];
const LED_Y: f32 = 536.8;
/// Caps of S0..S7; all at `BUTTON_Y`.
const BUTTON_X: [f32; 8] = [907.6, 875.6, 843.6, 811.6, 779.7, 747.9, 715.9, 684.5];
const BUTTON_Y: f32 = 617.5;
const BUTTON_CAP_R: f32 = 7.2;
/// AUX0..AUX5 pads (INT0..5 = PG0..5).
const AUX_X: f32 = 940.8;
const AUX_Y: [f32; 6] = [287.2, 318.4, 349.6, 380.8, 412.1, 448.1];
/// Nucleo buttons B1 (USER, blue) and B2 (RESET, black).
const USER: (f32, f32) = (294.0, 77.0);
const RESET: (f32, f32) = (297.0, 318.9);
/// Active area of the display (480 × 320): top left, top right, bottom right,
/// bottom left.  Not quite a rectangle: the glass is ~15 mm above the PCB.
const GLASS: [(f32, f32); 4] = [(518.8, 68.6), (896.6, 67.3), (897.9, 317.5), (521.4, 319.8)];

/// The board photo and the LED glow sprite.
pub struct Textures {
    photo: TextureHandle,
    glow: TextureHandle,
}

impl Textures {
    pub fn load(ctx: &egui::Context) -> Self {
        let mut img =
            image::load_from_memory_with_format(PHOTO, image::ImageFormat::Jpeg).expect("board photo").to_rgba8();
        let max = ctx.input(|i| i.max_texture_side) as u32;
        if img.width() > max {
            img = image::imageops::resize(
                &img,
                max,
                img.height() * max / img.width(),
                image::imageops::FilterType::Triangle,
            );
        }
        let photo =
            egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
        // mipmaps: the 4096 px photo is usually shown at a third of its size
        let options = TextureOptions { mipmap_mode: Some(egui::TextureFilter::Linear), ..TextureOptions::LINEAR };

        // Radial glow, premultiplied white; drawn with a tint of alpha 0 it adds light.
        const N: usize = 128;
        let glow = egui::ColorImage::new(
            [N, N],
            (0..N * N)
                .map(|i| {
                    let (x, y) = ((i % N) as f32 + 0.5, (i / N) as f32 + 0.5);
                    let d2 = ((x / N as f32 - 0.5).powi(2) + (y / N as f32 - 0.5).powi(2)) * 4.0;
                    let v = if d2 < 1.0 { (-4.0 * d2).exp() * (1.0 - d2) } else { 0.0 };
                    let v = (v * 255.0) as u8;
                    Color32::from_rgba_premultiplied(v, v, v, v)
                })
                .collect(),
        );
        Self {
            photo: ctx.load_texture("board", photo, options),
            glow: ctx.load_texture("glow", glow, TextureOptions::LINEAR),
        }
    }
}

/// State of the board shown in this frame.
pub struct BoardState<'a> {
    pub led_duty: &'a [f32; 16],
    pub inputs: u8,
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
    painter: &'a egui::Painter,
    tex: &'a Textures,
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

    /// Adds light: the glow sprite over `rx` × `ry` units, `color` scaled by `k`.
    fn glow(&self, x: f32, y: f32, rx: f32, ry: f32, color: Color32, k: f32) {
        let k = k.clamp(0.0, 1.0);
        let c = |v: u8| (f32::from(v) * k) as u8;
        let tint = Color32::from_rgba_premultiplied(c(color.r()), c(color.g()), c(color.b()), 0);
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        self.painter.image(self.tex.glow.id(), self.r(x - rx, y - ry, x + rx, y + ry), uv, tint);
    }

    /// SMD LED (vertical package, about 11 × 20 units) lit with `brightness`.
    fn led(&self, x: f32, y: f32, color: Color32, brightness: f32) {
        let b = brightness.clamp(0.0, 1.0).powf(1.0 / 2.2); // perceived brightness
        if b < 0.01 {
            return;
        }
        let hot = lerp(color, Color32::WHITE, 0.6);
        // the beige package takes the colour of the light (additive light alone would only whiten it)
        self.painter.rect_filled(
            self.r(x - 5.0, y - 7.0, x + 5.0, y + 7.0),
            CornerRadius::same(self.len(1.5) as u8),
            color.gamma_multiply(0.6 * b),
        );
        self.glow(x, y, 42.0, 42.0, color, 0.9 * b); // light on the solder mask
        self.glow(x, y, 10.0, 13.0, color, 0.9 * b); // the package lights up
        self.glow(x, y, 4.5, 4.5, hot, b); // the die
    }

    /// Shading of a pressed tact switch / latch marker.
    fn pressed(&self, x: f32, y: f32, r: f32) {
        self.painter.circle_filled(self.p(x, y), self.len(r), Color32::from_black_alpha(95));
        self.painter.circle_stroke(
            self.p(x, y - 0.4),
            self.len(r - 0.6),
            Stroke::new(self.len(1.0), Color32::from_black_alpha(110)),
        );
    }
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t) as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Display pixel under the design-unit point `(x, y)` (inverse of the bilinear
/// mapping of the glass quad).
fn glass_pixel(x: f32, y: f32) -> (f32, f32) {
    let [tl, tr, br, bl] = GLASS;
    let (mut u, mut v) = (0.5, 0.5);
    for _ in 0..4 {
        let left = (tl.0 + (bl.0 - tl.0) * v, tl.1 + (bl.1 - tl.1) * v);
        let right = (tr.0 + (br.0 - tr.0) * v, tr.1 + (br.1 - tr.1) * v);
        u = (x - left.0) / (right.0 - left.0);
        let top = tl.1 + (tr.1 - tl.1) * u;
        let bottom = bl.1 + (br.1 - bl.1) * u;
        v = (y - top) / (bottom - top);
    }
    (u * 480.0, v * 320.0)
}

pub fn show(ui: &mut egui::Ui, tex: &Textures, state: &BoardState) -> BoardInput {
    let avail = ui.available_size();
    let s = (avail.x / BOARD_W).min(avail.y / BOARD_H).max(0.4);
    let (outer, _) = ui.allocate_exact_size(Vec2::new(BOARD_W, BOARD_H) * s, Sense::hover());
    let painter = ui.painter_at(outer);
    let pcb = Pcb { painter: &painter, tex, origin: outer.min, s };
    let mut input = BoardInput::default();

    // ---- the photo
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    painter.add(
        egui::epaint::RectShape::filled(outer, CornerRadius::same(pcb.len(CORNER) as u8), Color32::WHITE)
            .with_texture(tex.photo.id(), uv),
    );

    // ---- display
    let quad = GLASS.map(|(x, y)| pcb.p(x, y));
    let mut mesh = match state.display {
        Some(t) => Mesh::with_texture(t.id()),
        None => Mesh::default(),
    };
    let color = if state.display.is_some() { Color32::WHITE } else { Color32::from_rgb(8, 9, 10) };
    for (p, uv) in quad.iter().zip([(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]) {
        mesh.vertices.push(egui::epaint::Vertex { pos: *p, uv: Pos2::new(uv.0, uv.1), color });
    }
    mesh.indices.extend([0, 1, 2, 0, 2, 3]);
    painter.add(Shape::mesh(mesh));

    // ---- Out: OUT15..OUT8 (PE7..PE0, yellow), OUT7..OUT0 (PD7..PD0, blue)
    for (bit, &x) in OUT_X.iter().enumerate() {
        let (color, pin) =
            if bit >= 8 { (LED_YELLOW, format!("PE{}", bit - 8)) } else { (LED_BLUE, format!("PD{bit}")) };
        pcb.led(x, LED_Y, color, state.led_duty[bit]);
        ui.interact(
            pcb.r(x - 14.0, LED_Y - 22.0, x + 14.0, LED_Y + 52.0),
            ui.scope_id().with(("out", bit)),
            Sense::hover(),
        )
        .on_hover_text(format!("OUT{bit} = {pin} (LED D{}): {:.0} %", bit + 8, state.led_duty[bit] * 100.0));
    }

    // ---- In: IN7..IN0 (PF7..PF0), red LEDs show the line level, buttons S7..S0
    for bit in 0..8 {
        let high = state.inputs & (1 << bit) != 0;
        pcb.led(IN_X[bit], LED_Y, LED_RED, if high { 1.0 } else { 0.0 });
        ui.interact(
            pcb.r(IN_X[bit] - 14.0, LED_Y - 22.0, IN_X[bit] + 14.0, LED_Y + 52.0),
            ui.scope_id().with(("in", bit)),
            Sense::hover(),
        )
        .on_hover_text(format!("IN{bit} = PF{bit} (LED D{bit}): {}", if high { "high" } else { "low" }));

        let (x, y) = (BUTTON_X[bit], BUTTON_Y);
        let resp = ui
            .interact(
                pcb.r(x - 14.0, y - 14.0, x + 14.0, y + 14.0),
                ui.scope_id().with(("button", bit)),
                Sense::click_and_drag(),
            )
            .on_hover_text(format!("S{bit} → IN{bit} (PF{bit}) · key {bit} · right click latches"));
        let held = resp.is_pointer_button_down_on();
        input.buttons[bit] = held;
        input.latch_toggled[bit] = resp.secondary_clicked();
        if held || state.latched[bit] || !high {
            pcb.pressed(x, y, BUTTON_CAP_R);
        }
        if state.latched[bit] {
            painter.circle_stroke(
                pcb.p(x, y),
                pcb.len(16.5),
                Stroke::new(pcb.len(1.6).max(1.0), Color32::from_rgb(0xFF, 0xE0, 0x40)),
            );
        }
    }

    // ---- Aux (INT0..5 = PG0..5): a wire plugged into the pad
    for (n, &y) in AUX_Y.iter().enumerate() {
        let level = state.ints[n];
        if let Some(high) = level {
            let wire = if high { Color32::from_rgb(0xE8, 0x30, 0x28) } else { Color32::from_rgb(0x2A, 0x5C, 0xD8) };
            painter.circle_filled(pcb.p(AUX_X, y), pcb.len(4.6), wire);
            painter.circle_stroke(
                pcb.p(AUX_X, y),
                pcb.len(4.6),
                Stroke::new(pcb.len(0.8), Color32::from_white_alpha(150)),
            );
        }
        let txt = match level {
            None => "open",
            Some(false) => "GND",
            Some(true) => "3V3",
        };
        let hit = ui
            .interact(
                pcb.r(AUX_X - 10.0, y - 10.0, AUX_X + 30.0, y + 10.0),
                ui.scope_id().with(("aux", n)),
                Sense::click(),
            )
            .on_hover_text(format!("AUX{n} / INT{n} = PG{n}: {txt} — click: open → GND → 3V3"));
        if hit.clicked() {
            input.int_clicked = Some(n);
        }
    }

    // ---- Nucleo buttons
    let user = ui
        .interact(
            pcb.r(USER.0 - 13.0, USER.1 - 13.0, USER.0 + 13.0, USER.1 + 13.0),
            ui.scope_id().with("user"),
            Sense::click_and_drag(),
        )
        .on_hover_text("USER button B1 (PC13)");
    input.user = user.is_pointer_button_down_on();
    if input.user {
        pcb.pressed(USER.0, USER.1, 8.5);
    }
    let reset = ui
        .interact(
            pcb.r(RESET.0 - 13.0, RESET.1 - 13.0, RESET.0 + 13.0, RESET.1 + 13.0),
            ui.scope_id().with("reset"),
            Sense::click(),
        )
        .on_hover_text("RESET button B2");
    input.reset_clicked = reset.clicked();
    if reset.is_pointer_button_down_on() {
        pcb.pressed(RESET.0, RESET.1, 8.5);
    }

    // ---- touch on the display glass
    let xs = GLASS.map(|p| p.0);
    let ys = GLASS.map(|p| p.1);
    let glass = pcb.r(xs[0].max(xs[3]), ys[0].max(ys[1]), xs[1].min(xs[2]), ys[2].min(ys[3]));
    let touch = ui
        .interact(glass, ui.scope_id().with("display"), Sense::click_and_drag())
        .on_hover_cursor(egui::CursorIcon::Crosshair);
    let to_px = |p: Pos2| {
        let (x, y) = glass_pixel((p.x - outer.min.x) / s, (p.y - outer.min.y) / s);
        (x.clamp(0.0, 479.99), y.clamp(0.0, 319.99))
    };
    input.touch = touch
        .is_pointer_button_down_on()
        .then(|| touch.interact_pointer_pos())
        .flatten()
        .filter(|p| glass.contains(*p))
        .map(to_px);
    if let Some((x, y)) = input.touch.or_else(|| touch.hover_pos().map(to_px)) {
        touch.on_hover_text_at_pointer(format!("x = {:.0}, y = {:.0}", x.floor(), y.floor()));
    }
    input
}
