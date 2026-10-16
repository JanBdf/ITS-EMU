//! Drawing of the ITS-Board and its interactive parts.
//!
//! The board (Rev 02, 15.12.2020, with the Nucleo and the display shield) is a
//! vector drawing, assets/board.svg, made by tools/hw/boarddraw.py after the
//! focus- and exposure-stacked photo of the real board (stack-capture.sh,
//! boardstack.py, boardasset.py).  It is rendered at the size it is shown at.
//! LEDs, pressed buttons and the display are drawn on top of it.  Positions are
//! in design units (the board is 1000 units wide) and were measured in the
//! photo by boardasset.py.

use eframe::egui::{
    self, Color32, Mesh, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, TextureOptions, Vec2, epaint::Vertex,
};

pub const BOARD_W: f32 = 1000.0;
pub const BOARD_H: f32 = 648.55;

const SVG: &str = include_str!("../assets/board.svg");
/// A new size is rendered once the board has kept it for this long (seconds).
const RESIZE_SETTLE: f64 = 0.15;

const LED_YELLOW: Color32 = Color32::from_rgb(0xFF, 0xC4, 0x10);
const LED_BLUE: Color32 = Color32::from_rgb(0x30, 0x70, 0xFF);
const LED_RED: Color32 = Color32::from_rgb(0xFF, 0x20, 0x30);

/// OUT0..OUT15 LEDs (D8..D23), x of the package centre; all at `LED_Y`.
/// These positions are shared with tools/hw/boarddraw.py, which reads them from here.
const OUT_X: [f32; 16] =
    [580.7, 549.0, 517.3, 485.7, 454.1, 422.4, 390.4, 358.7, 327.2, 295.7, 264.3, 233.1, 201.6, 169.8, 137.9, 105.4];
/// IN0..IN7 LEDs (D0..D7).
const IN_X: [f32; 8] = [909.1, 877.2, 845.4, 813.4, 781.8, 749.6, 718.0, 686.3];
const LED_Y: f32 = 539.0;
/// Caps of S0..S7; all at `BUTTON_Y`.
const BUTTON_X: [f32; 8] = [911.2, 878.7, 846.6, 814.2, 782.0, 749.8, 717.6, 685.8];
const BUTTON_Y: f32 = 620.1;
const BUTTON_CAP_R: f32 = 7.2;
/// AUX0..AUX5 pads (INT0..5 = PG0..5).
const AUX_X: f32 = 945.8;
const AUX_Y: [f32; 6] = [285.5, 317.0, 348.6, 380.1, 411.7, 448.6];
/// Nucleo buttons B1 (USER, blue) and B2 (RESET, black).
const USER: (f32, f32) = (289.2, 71.7);
const RESET: (f32, f32) = (290.6, 318.1);
/// Active area of the display (480 × 320): top left, top right, bottom right,
/// bottom left.  Not quite a rectangle: the glass is ~15 mm above the PCB.
const GLASS: [(f32, f32); 4] = [(515.0, 61.9), (895.9, 63.5), (898.4, 314.5), (518.6, 317.2)];

/// The board drawing and its rendering at the current size.
pub struct Textures {
    tree: resvg::usvg::Tree,
    board: Option<(TextureHandle, [u32; 2])>,
    /// Size the board is shown at that differs from the rendered one, since when.
    pending: Option<([u32; 2], f64)>,
}

impl Textures {
    pub fn load(_ctx: &egui::Context) -> Self {
        let tree = resvg::usvg::Tree::from_str(SVG, &resvg::usvg::Options::default()).expect("board drawing");
        Self { tree, board: None, pending: None }
    }

    /// The board rendered at `size` pixels, re-rendered when the size has settled.
    fn board(&mut self, ctx: &egui::Context, size: [u32; 2]) -> &TextureHandle {
        let now = ctx.input(|i| i.time);
        let rendered = self.board.as_ref().map(|b| b.1);
        if rendered == Some(size) {
            self.pending = None;
        } else if rendered.is_some() {
            match self.pending {
                Some((s, since)) if s == size => {
                    if now - since < RESIZE_SETTLE {
                        ctx.request_repaint_after(std::time::Duration::from_secs_f64(RESIZE_SETTLE));
                    } else {
                        self.board = None;
                    }
                }
                _ => {
                    self.pending = Some((size, now));
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(RESIZE_SETTLE));
                }
            }
        }
        if self.board.is_none() {
            self.pending = None;
            let image = self.render(size);
            self.board = Some((ctx.load_texture("board", image, TextureOptions::LINEAR), size));
        }
        &self.board.as_ref().unwrap().0
    }

    fn render(&self, [w, h]: [u32; 2]) -> egui::ColorImage {
        let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).expect("board size");
        let tree_size = self.tree.size();
        let transform =
            resvg::tiny_skia::Transform::from_scale(w as f32 / tree_size.width(), h as f32 / tree_size.height());
        resvg::render(&self.tree, transform, &mut pixmap.as_mut());
        // tiny-skia gives premultiplied RGBA, as egui wants it
        egui::ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data())
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

    /// Adds light: a radial falloff over `rx` × `ry` units, `color` scaled by `k`
    /// (a fan of rings with vertex colours; alpha 0 makes the colour additive).
    fn glow(&self, x: f32, y: f32, rx: f32, ry: f32, color: Color32, k: f32) {
        const RINGS: usize = 12;
        const SEGMENTS: usize = 48;
        let k = k.clamp(0.0, 1.0);
        let light = |d2: f32| {
            let v = if d2 < 1.0 { (-4.0 * d2).exp() * (1.0 - d2) * k } else { 0.0 };
            let c = |c: u8| (f32::from(c) * v) as u8;
            Color32::from_rgba_premultiplied(c(color.r()), c(color.g()), c(color.b()), 0)
        };
        let mut mesh = Mesh::default();
        mesh.vertices.push(Vertex { pos: self.p(x, y), uv: egui::epaint::WHITE_UV, color: light(0.0) });
        for ring in 1..=RINGS {
            // rings closer together near the centre, where the light changes fastest
            let r = (ring as f32 / RINGS as f32).powf(1.5);
            for seg in 0..SEGMENTS {
                let a = seg as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
                let pos = self.p(x + a.cos() * rx * r, y + a.sin() * ry * r);
                mesh.vertices.push(Vertex { pos, uv: egui::epaint::WHITE_UV, color: light(r * r) });
            }
        }
        let at = |ring: usize, seg: usize| (1 + (ring - 1) * SEGMENTS + seg % SEGMENTS) as u32;
        for seg in 0..SEGMENTS {
            mesh.indices.extend([0, at(1, seg), at(1, seg + 1)]);
        }
        for ring in 1..RINGS {
            for seg in 0..SEGMENTS {
                let (a, b) = (at(ring, seg), at(ring, seg + 1));
                let (c, d) = (at(ring + 1, seg), at(ring + 1, seg + 1));
                mesh.indices.extend([a, c, b, b, c, d]);
            }
        }
        self.painter.add(Shape::mesh(mesh));
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
            egui::CornerRadius::same(self.len(1.5) as u8),
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

pub fn show(ui: &mut egui::Ui, tex: &mut Textures, state: &BoardState) -> BoardInput {
    let avail = ui.available_size();
    let s = (avail.x / BOARD_W).min(avail.y / BOARD_H).max(0.4);
    let (outer, _) = ui.allocate_exact_size(Vec2::new(BOARD_W, BOARD_H) * s, Sense::hover());
    let painter = ui.painter_at(outer);
    let pcb = Pcb { painter: &painter, origin: outer.min, s };
    let mut input = BoardInput::default();

    // ---- the drawing, rendered in physical pixels
    let max = ui.ctx().input(|i| i.max_texture_side) as f32;
    let px = (outer.size() * ui.ctx().pixels_per_point()).round();
    let fit = (max / px.x.max(px.y)).min(1.0);
    let size = [(px.x * fit).max(1.0) as u32, (px.y * fit).max(1.0) as u32];
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    painter.image(tex.board(ui.ctx(), size).id(), outer, uv, Color32::WHITE);

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
