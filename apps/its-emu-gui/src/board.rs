//! Drawing of the ITS-Board and its interactive parts.
//!
//! The board (Rev 02, 15.12.2020, with the Nucleo and the display shield) is a
//! vector drawing in three layers, assets/board-{carrier,nucleo,shield}.svg,
//! made by tools/hw/boarddraw.py after photos of the real board.  Each layer is
//! rendered at the size it is shown at; the Nucleo and the display can be
//! shown see-through.
//! LEDs, pressed buttons and the display are drawn on top of it.  Positions are
//! in design units (the board is 1000 units wide); the drawing is an
//! orthographic top view, the parts on the Nucleo and the display were moved
//! from where the camera saw them to where they are.

use eframe::egui::{
    self, Color32, Mesh, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, TextureOptions, Vec2, epaint::Vertex,
};

/// The drawing (the board outline) spans `BOARD_W` × `BOARD_H` design units
/// from `BOARD_ORIGIN`.  The design units were set up on a photo that shows
/// the board a little cropped, so the board outline reaches slightly into
/// negative coordinates.  tools/hw/boarddraw.py reads these three constants.
pub const BOARD_W: f32 = 1010.8;
pub const BOARD_H: f32 = 658.5;
const BOARD_ORIGIN: (f32, f32) = (-4.8, -5.5);

/// The layers from the bottom up.
const LAYERS: [&str; 3] = [
    include_str!("../assets/board-carrier.svg"),
    include_str!("../assets/board-nucleo.svg"),
    include_str!("../assets/board-shield.svg"),
];
const NUCLEO: usize = 1;
const SHIELD: usize = 2;
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
/// Pads of the Analog header (DAC1, GNDA, ADC123_13 ... ADC3_6, GNDA), all at
/// `ANALOG_Y`, and the indices of the signal pads (`itsboard::ANALOG_PADS`).
const ANALOG_X: [f32; 10] = [440.3, 471.82, 503.34, 534.86, 566.38, 597.9, 629.42, 660.94, 692.46, 723.98];
const ANALOG_Y: f32 = 448.6;
const ANALOG_SIGNAL: [usize; 8] = [0, 2, 3, 4, 5, 6, 7, 8];
/// Nucleo buttons B1 (USER, blue) and B2 (RESET, black).
const USER: (f32, f32) = (321.2, 92.4);
const RESET: (f32, f32) = (321.2, 318.3);
/// Active area of the display (480 × 320, 83.52 × 55.68 mm): top left, top
/// right, bottom right, bottom left.  The board is drawn as seen from straight
/// above, so this is a rectangle.
const GLASS: [(f32, f32); 4] = [(532.1, 86.5), (878.9, 86.5), (878.9, 317.7), (532.1, 317.7)];

/// One layer of the board drawing and its rendering at the current size.
struct Layer {
    tree: resvg::usvg::Tree,
    /// Part of the drawing the layer covers, in design units from the top left
    /// corner of the drawing (`BOARD_ORIGIN`).
    bounds: Rect,
    /// Rendering, the board size in pixels it was made for and where it lies on the board (pixels).
    tex: Option<(TextureHandle, [u32; 2], Rect)>,
}

/// The board drawing and its rendering at the current size.
pub struct Textures {
    layers: Vec<Layer>,
    /// Size the board is shown at that differs from the rendered one, since when.
    pending: Option<([u32; 2], f64)>,
}

impl Textures {
    pub fn load(_ctx: &egui::Context) -> Self {
        let layers = LAYERS
            .iter()
            .map(|svg| {
                let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).expect("board drawing");
                let b = tree.root().abs_layer_bounding_box();
                let bounds = Rect::from_min_max(Pos2::new(b.left(), b.top()), Pos2::new(b.right(), b.bottom()))
                    .intersect(Rect::from_min_size(Pos2::ZERO, Vec2::new(BOARD_W, BOARD_H)));
                Layer { tree, bounds, tex: None }
            })
            .collect();
        Self { layers, pending: None }
    }

    /// The layers rendered for a board of `size` pixels (texture and its pixel
    /// rectangle), re-rendered when the size has settled.
    fn layers(&mut self, ctx: &egui::Context, size: [u32; 2]) -> Vec<(egui::TextureId, Rect)> {
        let now = ctx.input(|i| i.time);
        let rendered = self.layers[0].tex.as_ref().map(|t| t.1);
        let mut stale = rendered.is_none();
        if rendered == Some(size) {
            self.pending = None;
        } else if rendered.is_some() {
            match self.pending {
                Some((s, since)) if s == size => {
                    if now - since < RESIZE_SETTLE {
                        ctx.request_repaint_after(std::time::Duration::from_secs_f64(RESIZE_SETTLE));
                    } else {
                        stale = true;
                    }
                }
                _ => {
                    self.pending = Some((size, now));
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(RESIZE_SETTLE));
                }
            }
        }
        if stale {
            self.pending = None;
            for (n, layer) in self.layers.iter_mut().enumerate() {
                let (image, at) = layer.render(size);
                layer.tex = Some((ctx.load_texture(format!("board{n}"), image, TextureOptions::LINEAR), size, at));
            }
        }
        // until the new size has settled, the old rendering is stretched
        let [w, h] = self.layers[0].tex.as_ref().unwrap().1;
        let k = Vec2::new(size[0] as f32 / w as f32, size[1] as f32 / h as f32);
        self.layers
            .iter()
            .map(|l| {
                let (tex, _, at) = l.tex.as_ref().unwrap();
                (tex.id(), Rect::from_min_max((at.min.to_vec2() * k).to_pos2(), (at.max.to_vec2() * k).to_pos2()))
            })
            .collect()
    }
}

impl Layer {
    /// Renders the part of the layer that has content, for a board of `w` × `h`
    /// pixels; also returns where that part lies (pixels).
    fn render(&self, [w, h]: [u32; 2]) -> (egui::ColorImage, Rect) {
        let k = Vec2::new(w as f32 / BOARD_W, h as f32 / BOARD_H);
        let min = (self.bounds.min.to_vec2() * k).floor();
        let max = (self.bounds.max.to_vec2() * k).ceil().min(Vec2::new(w as f32, h as f32));
        let px = (max - min).max(Vec2::splat(1.0));
        let mut pixmap = resvg::tiny_skia::Pixmap::new(px.x as u32, px.y as u32).expect("board size");
        let tree_size = self.tree.size();
        let transform = resvg::tiny_skia::Transform::from_row(
            w as f32 / tree_size.width(),
            0.0,
            0.0,
            h as f32 / tree_size.height(),
            -min.x,
            -min.y,
        );
        resvg::render(&self.tree, transform, &mut pixmap.as_mut());
        // tiny-skia gives premultiplied RGBA, as egui wants it
        let image = egui::ColorImage::from_rgba_premultiplied([px.x as usize, px.y as usize], pixmap.data());
        (image, Rect::from_min_size(min.to_pos2(), px))
    }
}

/// How the board is shown.
#[derive(Clone, Copy, PartialEq)]
pub struct View {
    /// Opacity of the Nucleo (1: as on the real board).
    pub nucleo_opacity: f32,
    /// Opacity of the display module including its picture.
    pub shield_opacity: f32,
}

impl Default for View {
    fn default() -> Self {
        Self { nucleo_opacity: 1.0, shield_opacity: 1.0 }
    }
}

/// State of the board shown in this frame.
pub struct BoardState<'a> {
    pub led_duty: &'a [f32; 16],
    pub inputs: u8,
    pub display: Option<&'a egui::TextureHandle>,
    pub ints: &'a [Option<bool>; 6],
    pub latched: &'a [bool; 8],
    /// Voltages applied to the Analog header (`None`: open).
    pub analog: &'a [Option<f32>; 8],
    /// DAC1 output while it is enabled.
    pub dac1: Option<f32>,
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
    /// New voltage for a pad of the Analog header (`None`: open).
    pub analog: Option<(usize, Option<f32>)>,
    /// The display was right-clicked (switch between the board and the display alone).
    pub focus_toggled: bool,
}

/// Maps design units to screen coordinates.
struct Pcb<'a> {
    painter: &'a egui::Painter,
    origin: Pos2,
    s: f32,
}

impl Pcb<'_> {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + Vec2::new(x - BOARD_ORIGIN.0, y - BOARD_ORIGIN.1) * self.s
    }
    fn r(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::from_min_max(self.p(x0, y0), self.p(x1, y1))
    }
    fn len(&self, v: f32) -> f32 {
        v * self.s
    }

    /// A round light spot of radius `r` units: concentric rings blended over
    /// the board, colours (with alpha) given at relative radii `stops`
    /// (increasing, the first at 0, the last at 1).
    fn light(&self, x: f32, y: f32, r: f32, stops: &[(f32, Color32)]) {
        const RINGS: usize = 16;
        const SEGMENTS: usize = 48;
        let at_radius = |t: f32| {
            let i = stops.iter().rposition(|s| s.0 <= t).unwrap_or(0).min(stops.len() - 2);
            let ((t0, c0), (t1, c1)) = (stops[i], stops[i + 1]);
            let u = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
            let m = |a: u8, b: u8| f32::from(a) + (f32::from(b) - f32::from(a)) * u;
            // interpolated unmultiplied, then premultiplied for the mesh
            let alpha = m(c0.a(), c1.a()) / 255.0;
            let px = |a: u8, b: u8| (m(a, b) * alpha) as u8;
            Color32::from_rgba_premultiplied(
                px(c0.r(), c1.r()),
                px(c0.g(), c1.g()),
                px(c0.b(), c1.b()),
                (alpha * 255.0) as u8,
            )
        };
        let mut mesh = Mesh::default();
        mesh.vertices.push(Vertex { pos: self.p(x, y), uv: egui::epaint::WHITE_UV, color: at_radius(0.0) });
        for ring in 1..=RINGS {
            let t = ring as f32 / RINGS as f32;
            for seg in 0..SEGMENTS {
                let a = seg as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
                let pos = self.p(x + a.cos() * r * t, y + a.sin() * r * t);
                mesh.vertices.push(Vertex { pos, uv: egui::epaint::WHITE_UV, color: at_radius(t) });
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

    /// SMD LED lit with `brightness`: a round glow like the one of the power
    /// LEDs D24/D25 in the drawing (a white-hot lens fading into the colour).
    fn led(&self, x: f32, y: f32, color: Color32, brightness: f32) {
        let b = brightness.clamp(0.0, 1.0).powf(1.0 / 2.2); // perceived brightness
        if b < 0.01 {
            return;
        }
        let a = |v: f32| (v * b * 255.0) as u8;
        let tint = |c: Color32, alpha: u8| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha);
        self.light(
            x,
            y,
            12.0,
            &[
                (0.0, tint(lerp(color, Color32::WHITE, 0.9), a(1.0))),
                (0.28, tint(lerp(color, Color32::WHITE, 0.6), a(1.0))),
                (0.45, tint(lerp(color, Color32::WHITE, 0.15), a(0.8))),
                (0.7, tint(color, a(0.35))),
                (1.0, tint(color, 0)),
            ],
        );
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

pub fn show(ui: &mut egui::Ui, tex: &mut Textures, state: &BoardState, view: &View) -> BoardInput {
    let avail = ui.available_size();
    let s = (avail.x / BOARD_W).min(avail.y / BOARD_H).max(0.4);
    let (outer, _) = ui.allocate_exact_size(Vec2::new(BOARD_W, BOARD_H) * s, Sense::hover());
    // the drawing is rendered in physical pixels: place it on whole pixels, so
    // that it is not resampled
    let ppp = ui.ctx().pixels_per_point();
    let max = ui.ctx().input(|i| i.max_texture_side) as f32;
    let px = (outer.size() * ppp).round();
    let fit = (max / px.x.max(px.y)).min(1.0);
    let size = [(px.x * fit).max(1.0) as u32, (px.y * fit).max(1.0) as u32];
    let outer = Rect::from_min_size((outer.min.to_vec2() * ppp).round().to_pos2() / ppp, px / ppp);
    let s = outer.width() / BOARD_W;
    let painter = ui.painter_at(outer);
    let pcb = Pcb { painter: &painter, origin: outer.min, s };
    let mut input = BoardInput::default();

    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    for (n, (id, at)) in tex.layers(ui.ctx(), size).into_iter().enumerate() {
        let alpha = match n {
            NUCLEO => view.nucleo_opacity,
            SHIELD => view.shield_opacity,
            _ => 1.0,
        };
        if alpha > 0.0 {
            let at = Rect::from_min_max(outer.min + at.min.to_vec2() / ppp, outer.min + at.max.to_vec2() / ppp);
            painter.image(id, at, uv, Color32::WHITE.gamma_multiply(alpha));
        }
    }

    // ---- display
    let quad = GLASS.map(|(x, y)| pcb.p(x, y));
    let mut mesh = match state.display {
        Some(t) => Mesh::with_texture(t.id()),
        None => Mesh::default(),
    };
    let color = if state.display.is_some() { Color32::WHITE } else { Color32::from_rgb(8, 9, 10) };
    let color = color.gamma_multiply(view.shield_opacity);
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
        let held = resp.is_pointer_button_down_on() && primary_down(ui);
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

    // ---- Analog header: a wire with a voltage on the pad
    for (n, &(name, port, pin)) in itsboard::ANALOG_PADS.iter().enumerate() {
        let (x, y) = (ANALOG_X[ANALOG_SIGNAL[n]], ANALOG_Y);
        let volts = state.analog[n];
        let dac = if n == 0 { state.dac1 } else { None };
        if let Some(v) = volts {
            let wire = lerp(Color32::from_rgb(0x2A, 0x5C, 0xD8), Color32::from_rgb(0xE8, 0x30, 0x28), v / 3.3);
            painter.circle_filled(pcb.p(x, y), pcb.len(4.6), wire);
            painter.circle_stroke(pcb.p(x, y), pcb.len(4.6), Stroke::new(pcb.len(0.8), Color32::from_white_alpha(150)));
        }
        if let Some(v) = volts.or(dac) {
            let text = if volts.is_some() { format!("{v:.2} V") } else { format!("DAC {v:.2} V") };
            let galley = painter.layout_no_wrap(text, egui::FontId::proportional(pcb.len(7.5)), Color32::WHITE);
            let at = pcb.p(x, y - 15.0) - galley.size() / 2.0;
            let bg = Rect::from_min_size(at, galley.size()).expand(pcb.len(1.2));
            painter.rect_filled(bg, egui::CornerRadius::same(pcb.len(2.0) as u8), Color32::from_black_alpha(170));
            painter.galley(at, galley, Color32::WHITE);
        }
        let mut hover = format!(
            "{name} = P{port}{pin}: {}",
            volts.map_or_else(|| "open".to_owned(), |v| format!("{v:.2} V applied"))
        );
        if let Some(d) = dac {
            hover += &format!(", DAC1 output {d:.2} V");
        }
        hover += " — scroll: ±0.05 V, right click: set or open";
        let resp = ui
            .interact(pcb.r(x - 10.0, y - 10.0, x + 10.0, y + 10.0), ui.scope_id().with(("analog", n)), Sense::click())
            .on_hover_text(hover);
        if resp.hovered() {
            let scroll: f32 = ui.input(|i| {
                i.raw
                    .events
                    .iter()
                    .map(|e| if let egui::Event::MouseWheel { delta, .. } = e { delta.y } else { 0.0 })
                    .sum()
            });
            if scroll != 0.0 {
                let v = (volts.unwrap_or(1.65) + 0.05 * scroll.signum()).clamp(0.0, 3.3);
                input.analog = Some((n, Some((v * 100.0).round() / 100.0)));
            }
        }
        resp.context_menu(|ui| {
            ui.label(format!("{name} (P{port}{pin})"));
            let mut v = volts.unwrap_or(1.65);
            if ui.add(egui::Slider::new(&mut v, 0.0..=3.3).suffix(" V").fixed_decimals(2)).changed() {
                input.analog = Some((n, Some(v)));
            }
            ui.horizontal(|ui| {
                for preset in [0.0, 1.65, 3.3] {
                    if ui.button(format!("{preset} V")).clicked() {
                        input.analog = Some((n, Some(preset)));
                    }
                }
                if ui.button("open").clicked() {
                    input.analog = Some((n, None));
                    ui.close();
                }
            });
        });
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
        let (x, y) = glass_pixel((p.x - outer.min.x) / s + BOARD_ORIGIN.0, (p.y - outer.min.y) / s + BOARD_ORIGIN.1);
        (x.clamp(0.0, 479.99), y.clamp(0.0, 319.99))
    };
    input.touch = (touch.is_pointer_button_down_on() && primary_down(ui))
        .then(|| touch.interact_pointer_pos())
        .flatten()
        .filter(|p| glass.contains(*p))
        .map(to_px);
    input.focus_toggled = touch.secondary_clicked();
    if let Some((x, y)) = input.touch.or_else(|| touch.hover_pos().map(to_px)) {
        touch.on_hover_text_at_pointer(format!(
            "x = {:.0}, y = {:.0} · right click: display alone",
            x.floor(),
            y.floor()
        ));
    }
    input
}

fn primary_down(ui: &egui::Ui) -> bool {
    ui.input(|i| i.pointer.primary_down())
}

/// The display alone, as large as it fits, with the LEDs and buttons of the
/// board in a row below it.
pub fn show_lcd(ui: &mut egui::Ui, state: &BoardState) -> BoardInput {
    let mut input = BoardInput::default();
    const STRIP: f32 = 58.0;
    let avail = ui.available_rect_before_wrap();
    let rim = 0.035; // black border of the panel around the active area, relative to its width
    let area = Vec2::new(avail.width(), (avail.height() - STRIP).max(40.0));
    let s = (area.x / (480.0 * (1.0 + 2.0 * rim))).min(area.y / (320.0 + 2.0 * 480.0 * rim)).max(0.1);
    // whole physical pixels, so that the pixels of the display stay sharp
    let ppp = ui.ctx().pixels_per_point();
    let size = (Vec2::new(480.0, 320.0) * s * ppp).floor() / ppp;
    let center = Pos2::new(avail.center().x, avail.top() + area.y / 2.0);
    let glass = Rect::from_center_size(((center.to_vec2() * ppp).round() / ppp).to_pos2(), size);
    let glass = Rect::from_min_size(((glass.min.to_vec2() * ppp).round() / ppp).to_pos2(), size);
    let painter = ui.painter_at(avail);
    let frame = glass.expand(480.0 * rim * s);
    painter.rect_filled(frame.expand(2.0), 6.0, Color32::from_rgb(0x3a, 0x3d, 0x40));
    painter.rect_filled(frame, 3.0, Color32::from_rgb(0x1f, 0x25, 0x28));
    match state.display {
        Some(t) => {
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            painter.image(t.id(), glass, uv, Color32::WHITE);
        }
        None => {
            painter.rect_filled(glass, 0.0, Color32::from_rgb(8, 9, 10));
        }
    }
    let touch = ui
        .interact(glass, ui.scope_id().with("lcd"), Sense::click_and_drag())
        .on_hover_cursor(egui::CursorIcon::Crosshair);
    let to_px = |p: Pos2| {
        let v = (p - glass.min) / glass.size();
        ((v.x * 480.0).clamp(0.0, 479.99), (v.y * 320.0).clamp(0.0, 319.99))
    };
    input.touch = (touch.is_pointer_button_down_on() && primary_down(ui))
        .then(|| touch.interact_pointer_pos())
        .flatten()
        .filter(|p| glass.contains(*p))
        .map(to_px);
    input.focus_toggled = touch.secondary_clicked();
    if let Some((x, y)) = input.touch.or_else(|| touch.hover_pos().map(to_px)) {
        touch.on_hover_text_at_pointer(format!(
            "x = {:.0}, y = {:.0} · right click: back to the board",
            x.floor(),
            y.floor()
        ));
    }

    // ---- LEDs and buttons below: OUT15..OUT0, IN7..IN0 with S7..S0, USER
    let d = 16.0_f32.min(frame.width() / 30.0);
    let y = (frame.bottom() + 6.0 + STRIP / 2.0).min(avail.bottom() - STRIP / 2.0) - d * 0.4;
    let x0 = avail.center().x - 29.0 * d / 2.0 + d / 2.0;
    let label = |x: f32, text: &str| {
        painter.text(
            Pos2::new(x, y + d * 1.35),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(d * 0.62),
            Color32::from_gray(120),
        );
    };
    for bit in (0..16).rev() {
        let x = x0 + (15 - bit) as f32 * d;
        let color = if bit >= 8 { LED_YELLOW } else { LED_BLUE };
        let duty = state.led_duty[bit];
        strip_led(&painter, Pos2::new(x, y - d * 0.45), d, color, duty);
        let r = Rect::from_center_size(Pos2::new(x, y), Vec2::new(d, 2.0 * d));
        ui.interact(r, ui.scope_id().with(("lcd-out", bit)), Sense::hover())
            .on_hover_text(format!("OUT{bit}: {:.0} %", duty * 100.0));
        painter.text(
            Pos2::new(x, y + d * 0.45),
            egui::Align2::CENTER_CENTER,
            bit.to_string(),
            egui::FontId::proportional(d * 0.55),
            Color32::from_gray(150),
        );
    }
    label(x0 + 7.5 * d, "Out 15–0");
    let x1 = x0 + 18.0 * d;
    for bit in (0..8).rev() {
        let x = x1 + (7 - bit) as f32 * d;
        let high = state.inputs & (1 << bit) != 0;
        strip_led(&painter, Pos2::new(x, y - d * 0.45), d, LED_RED, if high { 1.0 } else { 0.0 });
        let c = Pos2::new(x, y + d * 0.45);
        let resp = ui
            .interact(
                Rect::from_center_size(c, Vec2::splat(d)),
                ui.scope_id().with(("lcd-button", bit)),
                Sense::click_and_drag(),
            )
            .on_hover_text(format!("S{bit} → IN{bit} (PF{bit}) · key {bit} · right click latches"));
        let held = resp.is_pointer_button_down_on() && primary_down(ui);
        input.buttons[bit] = held;
        input.latch_toggled[bit] = resp.secondary_clicked();
        let down = held || state.latched[bit] || !high;
        strip_button(&painter, c, d, Color32::from_rgb(0xd9, 0x43, 0x4b), down);
        if state.latched[bit] {
            painter.circle_stroke(c, d * 0.48, Stroke::new(1.5, Color32::from_rgb(0xFF, 0xE0, 0x40)));
        }
    }
    label(x1 + 3.5 * d, "In 7–0 · S7–S0");
    let c = Pos2::new(x1 + 10.0 * d, y + d * 0.45);
    let user = ui
        .interact(
            Rect::from_center_size(c, Vec2::splat(d * 1.2)),
            ui.scope_id().with("lcd-user"),
            Sense::click_and_drag(),
        )
        .on_hover_text("USER button B1 (PC13)");
    input.user = user.is_pointer_button_down_on() && primary_down(ui);
    strip_button(&painter, c, d * 1.15, Color32::from_rgb(0x2a, 0x8b, 0xd8), input.user);
    label(c.x, "User");
    input
}

/// A push button of the row below the display alone.
fn strip_button(painter: &egui::Painter, c: Pos2, d: f32, color: Color32, down: bool) {
    painter.circle_filled(c, d * 0.36, if down { lerp(color, Color32::BLACK, 0.45) } else { color });
}

/// A lit LED of the row below the display alone.
fn strip_led(painter: &egui::Painter, c: Pos2, d: f32, color: Color32, duty: f32) {
    let b = duty.clamp(0.0, 1.0).powf(1.0 / 2.2);
    painter.circle_filled(c, d * 0.3, lerp(Color32::from_gray(55), color, b));
    if b > 0.01 {
        painter.circle_filled(c, d * 0.5, color.gamma_multiply(0.25 * b));
    }
}
