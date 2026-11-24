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
/// A device's wire end on a pad and the tag beside it.
pub const DEVICE_WIRE: Color32 = Color32::from_rgb(0x2F, 0xB8, 0x6E);
const DEVICE_TAG: Color32 = Color32::from_rgba_premultiplied(0x10, 0x4a, 0x2c, 0xe0);
/// Marks a button latched by right click (not yellow: that would look like a lit part).
const LATCH: Color32 = Color32::from_rgb(0x7F, 0xE3, 0xFF);

#[derive(Clone, Copy, PartialEq)]
enum CableKind {
    MicroUsb,
    Ethernet,
}

/// Body, strain relief, cable and highlight of the cables.
const CABLE_BLACK: [Color32; 4] = [
    Color32::from_rgb(0x26, 0x26, 0x28),
    Color32::from_rgb(0x1d, 0x1d, 0x1f),
    Color32::from_rgb(0x1a, 0x1a, 0x1c),
    Color32::from_rgb(0x48, 0x48, 0x4c),
];
const CABLE_GREY: [Color32; 4] = [
    Color32::from_rgb(0xc9, 0xcd, 0xd1),
    Color32::from_rgb(0x8d, 0x93, 0x99),
    Color32::from_rgb(0x96, 0x9c, 0xa2),
    Color32::from_rgb(0xc4, 0xc9, 0xce),
];

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
/// Pads of the rows below the LEDs, all at `ROW_Y` (measured, Hough circles):
/// GND, OUT15..OUT0, GND from `OUT_PAD0` and GND, IN7..IN0, GND from
/// `IN_PAD0`, each `*_PITCH` apart.
const OUT_PAD0: f32 = 74.8;
const OUT_PITCH: f32 = 31.65;
const IN_PAD0: f32 = 655.2;
const IN_PITCH: f32 = 31.76;
const ROW_Y: f32 = 575.0;
/// Timer pads along the top edge, all at `TIM_Y`: GND at `TIM_GND`, then
/// PE8, PE10, PE14, PB10, PB11, PB0, PC6, PC7, PC8, PC9 from `TIM_PAD0`.
#[expect(dead_code, reason = "only tools/hw/boarddraw.py reads it")]
const TIM_GND: f32 = 355.7;
const TIM_PAD0: f32 = 387.1;
const TIM_PITCH: f32 = 31.73;
const TIM_Y: f32 = 32.3;
/// Pads of the I2C (x `I2C_PAD_X`: PB8, PB9, 3V3, GND) and SPI3 sections
/// (x `SPI_PAD_X`: PC10, PC12, PC11, PA15), at these y.
const HDR_PADS_Y: [f32; 4] = [359.4, 390.8, 422.2, 453.7];
const I2C_PAD_X: f32 = 155.4;
const SPI_PAD_X: f32 = 239.4;
/// Openings of the Nucleo's jacks, from tools/hw/boarddraw.py (photo
/// coordinates of the Nucleo's layer taken to the top view with
/// `ortho(x, y, M_NUCLEO)`): CN1 (ST-LINK USB, micro B, under the display,
/// opening to the right), CN13 (USB OTG FS, micro B, opening to the left) and
/// CN14 (Ethernet, RJ45, opening to the left).
const CN1_OPENING: (f32, f32) = (852.9, 265.6);
const CN13_OPENING: (f32, f32) = (287.1, 147.4);
const CN14_OPENING: (f32, f32) = (273.7, 242.7);
/// Right edge of the display module (where the ST-LINK cable comes out from under it).
const SHIELD_RIGHT: f32 = 888.7;
/// Nucleo buttons B1 (USER, blue) and B2 (RESET, black).
const USER: (f32, f32) = (321.2, 92.4);
const RESET: (f32, f32) = (321.2, 318.3);
/// Active area of the display (480 × 320, 83.52 × 55.68 mm): top left, top
/// right, bottom right, bottom left.  The board is drawn as seen from straight
/// above, so this is a rectangle.
const GLASS: [(f32, f32); 4] = [(532.1, 86.5), (878.9, 86.5), (878.9, 317.7), (532.1, 317.7)];

/// Signals of the timer pads, in the order of the pads after GND.
const TIM_PINS: [(&str, char, u32); 10] = [
    ("TIM1_1N", 'E', 8),
    ("TIM1_2N", 'E', 10),
    ("TIM1_4", 'E', 14),
    ("TIM2_3", 'B', 10),
    ("TIM2_4", 'B', 11),
    ("TIM3_3", 'B', 0),
    ("TIM8_1", 'C', 6),
    ("TIM8_2", 'C', 7),
    ("TIM8_3", 'C', 8),
    ("TIM8_4", 'C', 9),
];
/// Signals of the I2C and SPI3 pads, in the order of `HDR_PADS_Y`.
const I2C_PINS: [(&str, char, u32); 2] = [("I2C1_SCL", 'B', 8), ("I2C1_SDA", 'B', 9)];
const SPI_PINS: [(&str, char, u32); 4] =
    [("SPI3_SCK", 'C', 10), ("SPI3_MOSI", 'C', 12), ("SPI3_MISO", 'C', 11), ("SPI3_CS", 'A', 15)];

/// The pad of the carrier a pin is on: its silkscreen name and centre.
pub fn pad(port: char, pin: u32) -> Option<(String, (f32, f32))> {
    let n = pin as usize;
    match (port, pin) {
        ('F', 0..=7) => return Some((format!("IN{pin}"), (IN_PAD0 + (8 - n) as f32 * IN_PITCH, ROW_Y))),
        ('D', 0..=7) => return Some((format!("OUT{pin}"), (OUT_PAD0 + (16 - n) as f32 * OUT_PITCH, ROW_Y))),
        ('E', 0..=7) => return Some((format!("OUT{}", pin + 8), (OUT_PAD0 + (8 - n) as f32 * OUT_PITCH, ROW_Y))),
        ('G', 0..=5) => return Some((format!("AUX{pin}"), (AUX_X, AUX_Y[n]))),
        _ => {}
    }
    let find = |pins: &[(&str, char, u32)]| pins.iter().position(|&(_, p, i)| (p, i) == (port, pin));
    if let Some(i) = find(&TIM_PINS) {
        return Some((TIM_PINS[i].0.to_owned(), (TIM_PAD0 + i as f32 * TIM_PITCH, TIM_Y)));
    }
    if let Some(i) = find(&I2C_PINS) {
        return Some((I2C_PINS[i].0.to_owned(), (I2C_PAD_X, HDR_PADS_Y[i])));
    }
    if let Some(i) = find(&SPI_PINS) {
        return Some((SPI_PINS[i].0.to_owned(), (SPI_PAD_X, HDR_PADS_Y[i])));
    }
    let i = itsboard::ANALOG_PADS.iter().position(|&(_, p, i)| (p, i) == (port, pin))?;
    Some((itsboard::ANALOG_PADS[i].0.to_owned(), (ANALOG_X[ANALOG_SIGNAL[i]], ANALOG_Y)))
}

/// A device's wire on a board pin.
pub struct PinMark {
    pub port: char,
    pub pin: u32,
    /// Short tag drawn next to the pad ("ENC A", "GP0").
    pub tag: String,
    /// What is connected, for the tooltip.
    pub text: String,
}

/// State of a cable on one of the Nucleo's jacks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Plug {
    /// No cable.
    #[default]
    Out,
    /// The cable is there, but the computer has not taken the device yet
    /// (exported over USB/IP, not attached): drawn pulled out a little.
    Waiting,
    /// Plugged in and in use by the computer.
    In,
}

/// Cables on the Nucleo's jacks.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Cables {
    /// CN1, the ST-LINK's USB.
    pub stlink: Plug,
    /// CN13, the firmware's own USB device (OTG FS).
    pub otg: Plug,
    /// CN14 (the network device).
    pub ethernet: bool,
}

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
            let t0 = std::time::Instant::now();
            // in bands of rows, on all cores
            let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16) as f32;
            let areas: Vec<Rect> = self.layers.iter().map(|l| l.area(size)).collect();
            let total: f32 = areas.iter().map(|a| a.area()).sum();
            let bands: Vec<(usize, std::ops::Range<u32>)> = areas
                .iter()
                .enumerate()
                .flat_map(|(n, a)| {
                    let rows = a.height() as u32;
                    let count = ((a.area() / total * threads).ceil() as u32).clamp(1, rows);
                    let step = rows.div_ceil(count);
                    (0..rows).step_by(step as usize).map(move |r| (n, r..(r + step).min(rows)))
                })
                .collect();
            let layers = &self.layers;
            let pixels: Vec<Vec<u8>> = std::thread::scope(|scope| {
                let jobs: Vec<_> = bands
                    .iter()
                    .map(|(n, rows)| {
                        let area = areas[*n];
                        scope.spawn(move || layers[*n].render_rows(size, area, rows.clone()))
                    })
                    .collect();
                jobs.into_iter().map(|job| job.join().expect("board rendering")).collect()
            });
            let mut images: Vec<Vec<u8>> = vec![Vec::new(); self.layers.len()];
            for ((n, _), band) in bands.iter().zip(pixels) {
                images[*n].extend_from_slice(&band);
            }
            for (n, (layer, rgba)) in self.layers.iter_mut().zip(images).enumerate() {
                let at = areas[n];
                // tiny-skia gives premultiplied RGBA, as egui wants it
                let image =
                    egui::ColorImage::from_rgba_premultiplied([at.width() as usize, at.height() as usize], &rgba);
                layer.tex = Some((ctx.load_texture(format!("board{n}"), image, TextureOptions::LINEAR), size, at));
            }
            log::debug!("board rendered at {size:?} in {:?}", t0.elapsed());
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
    /// The part of the layer that has content, for a board of `w` × `h`
    /// pixels (whole pixels).
    fn area(&self, [w, h]: [u32; 2]) -> Rect {
        let k = Vec2::new(w as f32 / BOARD_W, h as f32 / BOARD_H);
        let min = (self.bounds.min.to_vec2() * k).floor();
        let max = (self.bounds.max.to_vec2() * k).ceil().min(Vec2::new(w as f32, h as f32));
        Rect::from_min_size(min.to_pos2(), (max - min).max(Vec2::splat(1.0)))
    }

    /// Renders rows `rows` of `area` (premultiplied RGBA).
    fn render_rows(&self, [w, h]: [u32; 2], area: Rect, rows: std::ops::Range<u32>) -> Vec<u8> {
        let mut pixmap = resvg::tiny_skia::Pixmap::new(area.width() as u32, rows.len() as u32).expect("board size");
        let tree_size = self.tree.size();
        let transform = resvg::tiny_skia::Transform::from_row(
            w as f32 / tree_size.width(),
            0.0,
            0.0,
            h as f32 / tree_size.height(),
            -area.min.x,
            -area.min.y - rows.start as f32,
        );
        resvg::render(&self.tree, transform, &mut pixmap.as_mut());
        pixmap.take()
    }
}

/// Zoom and pan of the board inside its panel.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Zoom {
    /// Scale relative to fitting the panel (1: fits).
    pub factor: f32,
    /// Offset of the board's centre from the panel's centre (points).
    pub pan: Vec2,
}

impl Default for Zoom {
    fn default() -> Self {
        Self { factor: 1.0, pan: Vec2::ZERO }
    }
}

impl Zoom {
    pub const MAX: f32 = 4.0;
}

/// Widest rendering of the drawing (pixels); zoomed in further it is
/// stretched (three layers of RGBA at this width take 150 MB).
const MAX_RENDER_W: f32 = 3600.0;

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
    /// Brightness of the Nucleo's LD1–LD3.
    pub nucleo_duty: &'a [f32; 3],
    /// No firmware: what the display says about opening one.
    pub open_hint: Option<&'a str>,
    /// Wires of the connected devices.
    pub marks: &'a [PinMark],
    pub cables: Cables,
    /// Tooltip of the ST-LINK's cable.
    pub stlink_text: &'a str,
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
    /// The empty display was clicked (open a firmware file).
    pub open_requested: bool,
    /// The ST-LINK's USB cable was clicked (plug it in or out).
    pub stlink_clicked: bool,
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
            // interpolated unmultiplied (Color32 holds premultiplied values),
            // then premultiplied for the mesh
            let (mut c0, mut c1) = (c0.to_srgba_unmultiplied(), c1.to_srgba_unmultiplied());
            // a transparent stop has no colour of its own: fade in its neighbour's
            if c0[3] == 0 {
                c0 = [c1[0], c1[1], c1[2], 0];
            }
            if c1[3] == 0 {
                c1 = [c0[0], c0[1], c0[2], 0];
            }
            let alpha = m(c0[3], c1[3]) / 255.0;
            let px = |k: usize| (m(c0[k], c1[k]) * alpha) as u8;
            Color32::from_rgba_premultiplied(px(0), px(1), px(2), (alpha * 255.0) as u8)
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
            5.8,
            // bright lens with a tight coloured rim over the package
            &[
                (0.0, tint(lerp(color, Color32::WHITE, 0.85), a(1.0))),
                (0.28, tint(lerp(color, Color32::WHITE, 0.55), a(1.0))),
                (0.55, tint(color, a(0.95))),
                (0.78, tint(color, a(0.30))),
                (1.0, tint(color, 0)),
            ],
        );
    }

    /// A cable plugged into the jack whose opening is at `at` (the plug points
    /// into the jack against `dir`: +1 = the cable leaves to the right, -1 to
    /// the left), bent to `end` (design units).  Waiting: pulled out a little.
    fn cable(&self, kind: CableKind, plug: Plug, at: (f32, f32), dir: f32, end: [(f32, f32); 3], alpha: f32) {
        let k = if plug == Plug::Waiting { 0.7 * alpha } else { alpha };
        let c = |c: Color32| c.gamma_multiply(k);
        // pulled out by this much while the computer has not taken the device
        let gap = if plug == Plug::Waiting { 9.0 } else { 0.0 };
        let x0 = at.0 + dir * gap;
        let y = at.1;
        // body (overmold or the RJ45's clear plug and its boot), strain relief, cable
        let (body, relief, w_body, w_relief, w_cable, colors) = match kind {
            CableKind::MicroUsb => (60.0, 43.0, 20.0, (15.0, 8.0), 6.5, CABLE_BLACK),
            CableKind::Ethernet => (22.0, 30.0, 22.0, (17.0, 11.0), 11.0, CABLE_GREY),
        };
        let r = |a: f32, b: f32, h: f32| {
            let (xa, xb) = (x0 + dir * a, x0 + dir * b);
            self.r(xa.min(xb), y - h, xa.max(xb), y + h)
        };
        let [body_color, relief_color, cable_color, shine] = colors;
        // the cable first, so that the plug lies on its end
        let start = self.p(x0 + dir * (body + relief - 2.0), y);
        let [c1, c2, e] = end.map(|(x, y)| self.p(x, y));
        let cable = |width: f32, color: Color32, offset: f32| {
            let o = Vec2::new(0.0, self.len(offset));
            egui::epaint::CubicBezierShape::from_points_stroke(
                [start + o, c1 + o, c2 + o, e + o],
                false,
                Color32::TRANSPARENT,
                Stroke::new(self.len(width), color),
            )
        };
        self.painter.add(cable(2.0 * w_cable, c(cable_color), 0.0));
        self.painter.add(cable(w_cable * 0.5, c(shine), -w_cable * 0.4));
        // strain relief: tapering, with ribs
        let steps = 6;
        for i in 0..steps {
            let t = i as f32 / steps as f32;
            let h = w_relief.0 + (w_relief.1 - w_relief.0) * t;
            let a = body + relief * t;
            let b = body + relief * (i + 1) as f32 / steps as f32;
            self.painter.rect_filled(r(a, b, h), 0.0, c(relief_color));
            if kind == CableKind::MicroUsb && i > 0 {
                self.painter.rect_filled(r(a, a + 2.2, h * 0.92), 0.0, c(Color32::from_gray(12)));
            }
        }
        self.painter.rect_filled(r(0.0, body, w_body), self.len(2.0), c(body_color));
        // highlight along the top of the body
        self.painter.rect_filled(
            r(1.5, body - 1.5, 1.2).translate(Vec2::new(0.0, -self.len(w_body - 3.0))),
            0.0,
            c(shine),
        );
        if kind == CableKind::Ethernet {
            // the clear plug shows its gold contacts and the latch
            for i in 0..8 {
                let yy = y - 9.0 + i as f32 * 2.6;
                let xa = x0 + dir * 2.0;
                let xb = x0 + dir * 9.0;
                self.painter.rect_filled(
                    self.r(xa.min(xb), yy - 0.5, xa.max(xb), yy + 0.5),
                    0.0,
                    c(Color32::from_rgb(0xd6, 0xa5, 0x4e)),
                );
            }
            self.painter.rect_filled(r(body - 4.0, body + 6.0, 8.0), 0.0, c(Color32::from_rgb(0x6f, 0x75, 0x7b)));
        }
    }

    /// A device's wire on a pad: its end on the pad and a tag with what it is.
    fn wire_mark(&self, x: f32, y: f32, tag: &str) {
        self.painter.circle_filled(self.p(x, y), self.len(4.6), DEVICE_WIRE);
        self.painter.circle_stroke(
            self.p(x, y),
            self.len(4.6),
            Stroke::new(self.len(0.8), Color32::from_white_alpha(170)),
        );
        let galley =
            self.painter.layout_no_wrap(tag.to_owned(), egui::FontId::proportional(self.len(7.0)), Color32::WHITE);
        let at = self.p(x, y - 15.0) - galley.size() / 2.0;
        let bg = Rect::from_min_size(at, galley.size()).expand(self.len(1.2));
        self.painter.rect_filled(bg, egui::CornerRadius::same(self.len(2.0) as u8), DEVICE_TAG);
        self.painter.galley(at, galley, Color32::WHITE);
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

/// Gives a spot of the drawing a name for screen readers.
fn named(resp: egui::Response, role: egui::Role, label: impl ToString) -> egui::Response {
    let label = label.to_string();
    resp.widget_info(|| egui::WidgetInfo::labeled(role, true, &label));
    resp
}

/// A tooltip above the spot: the board's rows lie close together, and one
/// below the spot would cover the next row (the buttons under the LEDs).
fn tip(resp: egui::Response, text: impl Into<egui::WidgetText>) -> egui::Response {
    let mut tooltip = egui::Tooltip::for_enabled(&resp);
    tooltip.popup = tooltip.popup.align(egui::RectAlign::TOP);
    tooltip.show(|ui| ui.label(text));
    resp
}

/// The call to open a firmware file on the empty display.
fn draw_open_hint(painter: &egui::Painter, glass: Rect, hint: &str, hovered: bool) {
    let size = glass.height() / 14.0;
    let color = if hovered { Color32::from_gray(235) } else { Color32::from_gray(170) };
    painter.text(
        glass.center() - Vec2::new(0.0, size * 0.9),
        egui::Align2::CENTER_CENTER,
        "No firmware",
        egui::FontId::proportional(size * 1.3),
        color,
    );
    // centred lines, wrapped to the glass
    let mut job = egui::text::LayoutJob::simple(
        hint.to_owned(),
        egui::FontId::proportional(size * 0.75),
        color.gamma_multiply(0.85),
        glass.width() * 0.85,
    );
    job.halign = egui::Align::Center;
    let galley = painter.layout_job(job);
    painter.galley(Pos2::new(glass.center().x, glass.center().y + size * 0.3), galley, Color32::WHITE);
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

pub fn show(ui: &mut egui::Ui, tex: &mut Textures, state: &BoardState, view: &View, zoom: &mut Zoom) -> BoardInput {
    let panel = ui.available_rect_before_wrap();
    let (panel_rect, bench) = ui.allocate_exact_size(panel.size(), Sense::click_and_drag());
    // Ctrl + wheel zooms at the pointer, the middle button or Shift + drag pans
    let (wheel, ctrl, shift, middle) = ui.input(|i| {
        let wheel: f32 = i
            .raw
            .events
            .iter()
            .map(|e| if let egui::Event::MouseWheel { delta, .. } = e { delta.y } else { 0.0 })
            .sum();
        (wheel, i.modifiers.command, i.modifiers.shift, i.pointer.middle_down())
    });
    let fit_s = (panel_rect.width() / BOARD_W).min(panel_rect.height() / BOARD_H).max(0.2);
    if ctrl && wheel != 0.0 && bench.contains_pointer() {
        let old = zoom.factor;
        zoom.factor = (zoom.factor * 1.15_f32.powf(wheel.signum())).clamp(1.0, Zoom::MAX);
        if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
            // keep the point under the pointer where it is
            let centre = panel_rect.center() + zoom.pan;
            zoom.pan += (p - centre) * (1.0 - zoom.factor / old);
        }
    }
    if bench.dragged() && (middle || shift) {
        zoom.pan += bench.drag_delta();
    }
    if zoom.factor <= 1.0 {
        *zoom = Zoom::default();
    } else {
        // not further than half the board out of the panel
        let half = Vec2::new(BOARD_W, BOARD_H) * fit_s * zoom.factor / 2.0;
        zoom.pan = zoom.pan.clamp(-half, half);
    }
    let s = fit_s * zoom.factor;
    let outer = Rect::from_center_size(panel_rect.center() + zoom.pan, Vec2::new(BOARD_W, BOARD_H) * s);
    // the drawing is rendered in physical pixels: place it on whole pixels, so
    // that it is not resampled
    let ppp = ui.ctx().pixels_per_point();
    let max = (ui.ctx().input(|i| i.max_texture_side) as f32).min(MAX_RENDER_W);
    let px = (outer.size() * ppp).round();
    let fit = (max / px.x.max(px.y)).min(1.0);
    let size = [(px.x * fit).max(1.0) as u32, (px.y * fit).max(1.0) as u32];
    let outer = Rect::from_min_size((outer.min.to_vec2() * ppp).round().to_pos2() / ppp, px / ppp);
    let s = outer.width() / BOARD_W;
    let painter = ui.painter_at(panel_rect);
    let pcb = Pcb { painter: &painter, origin: outer.min, s };
    let mut input = BoardInput::default();

    ui.set_clip_rect(panel_rect);
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    // the ST-LINK's cable comes out from under the display: clicking it plugs it in or out
    let stlink = ui.interact(
        pcb.r(SHIELD_RIGHT, 250.0, BOARD_W + BOARD_ORIGIN.0, 280.0),
        ui.scope_id().with("stlink"),
        Sense::click(),
    );
    let stlink = named(stlink, egui::Role::Button, "ST-LINK USB cable (CN1)");
    input.stlink_clicked = stlink.clicked();
    let stlink_ghost = state.cables.stlink == Plug::Out && stlink.hovered();
    tip(stlink.on_hover_cursor(egui::CursorIcon::PointingHand), state.stlink_text);
    for (n, (id, at)) in tex.layers(ui.ctx(), size).into_iter().enumerate() {
        if n == SHIELD {
            // cables on the Nucleo's jacks, under the display
            let c = &state.cables;
            if c.stlink != Plug::Out || stlink_ghost {
                let (plug, alpha) = if stlink_ghost { (Plug::In, 0.35) } else { (c.stlink, 1.0) };
                let end = [(960.0, 265.6), (990.0, 266.5), (1030.0, 268.0)];
                pcb.cable(CableKind::MicroUsb, plug, CN1_OPENING, 1.0, end, alpha);
            }
            if c.otg != Plug::Out {
                // to the top edge, over the RS232 jack
                pcb.cable(
                    CableKind::MicroUsb,
                    c.otg,
                    CN13_OPENING,
                    -1.0,
                    [(190.0, 147.4), (182.0, 110.0), (182.0, -25.0)],
                    1.0,
                );
            }
            if c.ethernet {
                pcb.cable(
                    CableKind::Ethernet,
                    Plug::In,
                    CN14_OPENING,
                    -1.0,
                    [(150.0, 242.7), (80.0, 246.0), (-25.0, 248.0)],
                    1.0,
                );
            }
        }
        let alpha = match n {
            NUCLEO => view.nucleo_opacity,
            SHIELD => view.shield_opacity,
            _ => 1.0,
        };
        if alpha > 0.0 {
            // the rendering may be smaller than the drawing (MAX_RENDER_W)
            let k = outer.width() / size[0] as f32;
            let at = Rect::from_min_max(outer.min + at.min.to_vec2() * k, outer.min + at.max.to_vec2() * k);
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
        let text = format!("OUT{bit} = {pin} (LED D{}): {:.0} %", bit + 8, state.led_duty[bit] * 100.0);
        let resp = ui.interact(
            pcb.r(x - 14.0, LED_Y - 22.0, x + 14.0, LED_Y + 52.0),
            ui.scope_id().with(("out", bit)),
            Sense::hover(),
        );
        tip(named(resp, egui::Role::Label, &text), text);
    }

    // ---- In: IN7..IN0 (PF7..PF0), red LEDs show the line level, buttons S7..S0
    for bit in 0..8 {
        let high = state.inputs & (1 << bit) != 0;
        pcb.led(IN_X[bit], LED_Y, LED_RED, if high { 1.0 } else { 0.0 });
        let text = format!("IN{bit} = PF{bit} (LED D{bit}): {}", if high { "high" } else { "low" });
        let resp = ui.interact(
            pcb.r(IN_X[bit] - 14.0, LED_Y - 22.0, IN_X[bit] + 14.0, LED_Y + 52.0),
            ui.scope_id().with(("in", bit)),
            Sense::hover(),
        );
        tip(named(resp, egui::Role::Label, &text), text);

        let (x, y) = (BUTTON_X[bit], BUTTON_Y);
        let resp = ui.interact(
            pcb.r(x - 14.0, y - 14.0, x + 14.0, y + 14.0),
            ui.scope_id().with(("button", bit)),
            Sense::click_and_drag(),
        );
        let latch = if state.latched[bit] { "latched: right click releases" } else { "right click latches" };
        let resp = tip(
            named(resp, egui::Role::Button, format!("Button S{bit}")),
            format!("S{bit} → IN{bit} (PF{bit})\nclick or key {bit} · {latch}"),
        );
        let held = resp.is_pointer_button_down_on() && primary_down(ui);
        input.buttons[bit] = held;
        input.latch_toggled[bit] = resp.secondary_clicked();
        if held || state.latched[bit] || !high {
            pcb.pressed(x, y, BUTTON_CAP_R);
        }
        if state.latched[bit] {
            painter.circle_stroke(pcb.p(x, y), pcb.len(16.5), Stroke::new(pcb.len(1.6).max(1.0), LATCH));
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
        let hit = ui.interact(
            pcb.r(AUX_X - 10.0, y - 10.0, AUX_X + 30.0, y + 10.0),
            ui.scope_id().with(("aux", n)),
            Sense::click(),
        );
        let hit = tip(
            named(hit, egui::Role::Button, format!("AUX{n} pad: {txt}")),
            format!("AUX{n} / INT{n} = PG{n}: {txt}\nclick: open → GND → 3V3"),
        );
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
        if n == 0 {
            // the DAC's output: shown, not driven from outside
            let hover = match dac {
                Some(d) => format!("{name} = P{port}{pin}: DAC output {d:.2} V"),
                None => format!("{name} = P{port}{pin}: DAC output (the DAC is off)"),
            };
            let resp = ui.interact(
                pcb.r(x - 10.0, y - 10.0, x + 10.0, y + 10.0),
                ui.scope_id().with(("analog", n)),
                Sense::hover(),
            );
            tip(named(resp, egui::Role::Label, &hover), hover);
            continue;
        }
        let hover = format!(
            "{name} = P{port}{pin}: {}\nscroll: ±0.05 V · double click: open ↔ 3.3 V · right click: set",
            volts.map_or_else(|| "open".to_owned(), |v| format!("{v:.2} V applied"))
        );
        let resp = ui.interact(
            pcb.r(x - 10.0, y - 10.0, x + 10.0, y + 10.0),
            ui.scope_id().with(("analog", n)),
            Sense::click(),
        );
        let resp = tip(named(resp, egui::Role::Button, hover.replace('\n', ", ")), hover);
        if resp.hovered() {
            let scroll: f32 = ui.input(|i| {
                i.raw
                    .events
                    .iter()
                    .map(|e| if let egui::Event::MouseWheel { delta, .. } = e { delta.y } else { 0.0 })
                    .sum()
            });
            if scroll != 0.0 {
                // an open pad starts from 0 V
                let v = (volts.unwrap_or(0.0) + 0.05 * scroll.signum()).clamp(0.0, 3.3);
                input.analog = Some((n, Some((v * 100.0).round() / 100.0)));
            }
        }
        if resp.double_clicked() {
            input.analog = Some((n, if volts.is_some() { None } else { Some(3.3) }));
        }
        resp.context_menu(|ui| {
            ui.label(format!("{name} (P{port}{pin})"));
            let mut v = volts.unwrap_or(0.0);
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

    // ---- devices' wires on the pads (over the other spots, so that their tooltip wins)
    for (i, m) in state.marks.iter().enumerate() {
        let Some((name, (x, y))) = pad(m.port, m.pin) else { continue };
        pcb.wire_mark(x, y, &m.tag);
        let text = format!("{name} = P{}{}: {}", m.port, m.pin, m.text);
        let resp =
            ui.interact(pcb.r(x - 8.0, y - 22.0, x + 8.0, y + 8.0), ui.scope_id().with(("mark", i)), Sense::hover());
        tip(named(resp, egui::Role::Label, &text), text);
    }

    // ---- Nucleo buttons
    let user = ui.interact(
        pcb.r(USER.0 - 13.0, USER.1 - 13.0, USER.0 + 13.0, USER.1 + 13.0),
        ui.scope_id().with("user"),
        Sense::click_and_drag(),
    );
    let user = named(user, egui::Role::Button, "USER button B1").on_hover_text("USER button B1 (PC13)");
    input.user = user.is_pointer_button_down_on();
    if input.user {
        pcb.pressed(USER.0, USER.1, 8.5);
    }
    let reset = ui.interact(
        pcb.r(RESET.0 - 13.0, RESET.1 - 13.0, RESET.0 + 13.0, RESET.1 + 13.0),
        ui.scope_id().with("reset"),
        Sense::click(),
    );
    let reset = named(reset, egui::Role::Button, "RESET button B2").on_hover_text("RESET button B2 · Ctrl+R");
    input.reset_clicked = reset.clicked();
    if reset.is_pointer_button_down_on() {
        pcb.pressed(RESET.0, RESET.1, 8.5);
    }

    // ---- touch on the display glass
    let xs = GLASS.map(|p| p.0);
    let ys = GLASS.map(|p| p.1);
    let glass = pcb.r(xs[0].max(xs[3]), ys[0].max(ys[1]), xs[1].min(xs[2]), ys[2].min(ys[3]));
    let touch = ui.interact(glass, ui.scope_id().with("display"), Sense::click_and_drag());
    let touch = named(touch, egui::Role::Image, "Display (touch screen)");
    if let Some(hint) = state.open_hint {
        draw_open_hint(&painter, glass, hint, touch.hovered());
        input.open_requested = touch.clicked();
        input.focus_toggled = touch.secondary_clicked();
        touch.on_hover_cursor(egui::CursorIcon::PointingHand);
        return input;
    }
    let touch = touch.on_hover_cursor(egui::CursorIcon::Crosshair);
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
    const STRIP: f32 = 78.0;
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
    // the module as on the board: the touch panel's light edge around the dark border
    let frame = glass.expand(480.0 * rim * s);
    painter.rect_filled(frame.expand((480.0 * 0.01 * s).max(2.0)), 2.0, Color32::from_rgb(0xbd, 0xb3, 0x9f));
    painter.rect_filled(frame, 1.0, Color32::from_rgb(0x1c, 0x1e, 0x20));
    painter.rect_stroke(
        frame.shrink(480.0 * 0.006 * s),
        0.0,
        Stroke::new(1.0, Color32::from_rgb(0x5d, 0x5a, 0x52)),
        egui::StrokeKind::Inside,
    );
    match state.display {
        Some(t) => {
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            painter.image(t.id(), glass, uv, Color32::WHITE);
        }
        None => {
            painter.rect_filled(glass, 0.0, Color32::from_rgb(8, 9, 10));
        }
    }
    let touch = ui.interact(glass, ui.scope_id().with("lcd"), Sense::click_and_drag());
    let touch = named(touch, egui::Role::Image, "Display (touch screen)");
    input.focus_toggled = touch.secondary_clicked();
    if let Some(hint) = state.open_hint {
        draw_open_hint(&painter, glass, hint, touch.hovered());
        input.open_requested = touch.clicked();
        touch.on_hover_cursor(egui::CursorIcon::PointingHand);
    } else {
        let touch = touch.on_hover_cursor(egui::CursorIcon::Crosshair);
        let to_px = |p: Pos2| {
            let v = (p - glass.min) / glass.size();
            ((v.x * 480.0).clamp(0.0, 479.99), (v.y * 320.0).clamp(0.0, 319.99))
        };
        input.touch = (touch.is_pointer_button_down_on() && primary_down(ui))
            .then(|| touch.interact_pointer_pos())
            .flatten()
            .filter(|p| glass.contains(*p))
            .map(to_px);
        if let Some((x, y)) = input.touch.or_else(|| touch.hover_pos().map(to_px)) {
            touch.on_hover_text_at_pointer(format!(
                "x = {:.0}, y = {:.0} · right click: back to the board",
                x.floor(),
                y.floor()
            ));
        }
    }

    // ---- LEDs and buttons below: OUT15..OUT0, IN7..IN0 with S7..S0,
    // USER and RESET, LD1–LD3 (slots of width d)
    const SLOTS: f32 = 36.0;
    let d = 21.0_f32.min(frame.width() / (SLOTS + 1.0));
    let y = (frame.bottom() + 6.0 + STRIP / 2.0).min(avail.bottom() - STRIP / 2.0) - d * 0.4;
    let x0 = avail.center().x - SLOTS * d / 2.0 + d / 2.0;
    // a name under each part, as on the silkscreen, and one under each group
    let name = |x: f32, text: &str| {
        painter.text(
            Pos2::new(x, y + d * 1.25),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(d * 0.5),
            Color32::from_gray(200),
        );
    };
    let label = |x: f32, text: &str| {
        painter.text(
            Pos2::new(x, y + d * 1.95),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(d * 0.62),
            Color32::from_gray(160),
        );
    };
    for bit in (0..16).rev() {
        let x = x0 + (15 - bit) as f32 * d;
        let color = if bit >= 8 { LED_YELLOW } else { LED_BLUE };
        let duty = state.led_duty[bit];
        strip_led(&painter, Pos2::new(x, y - d * 0.45), d, color, duty);
        let r = Rect::from_center_size(Pos2::new(x, y), Vec2::new(d, 2.0 * d));
        let text = format!("OUT{bit}: {:.0} %", duty * 100.0);
        named(ui.interact(r, ui.scope_id().with(("lcd-out", bit)), Sense::hover()), egui::Role::Label, &text)
            .on_hover_text(text);
        name(x, &bit.to_string());
    }
    label(x0 + 7.5 * d, "OUT15 … OUT0 (LEDs D23 … D8)");
    let x1 = x0 + 18.0 * d;
    for bit in (0..8).rev() {
        let x = x1 + (7 - bit) as f32 * d;
        let high = state.inputs & (1 << bit) != 0;
        strip_led(&painter, Pos2::new(x, y - d * 0.45), d, LED_RED, if high { 1.0 } else { 0.0 });
        let c = Pos2::new(x, y + d * 0.5);
        let resp = ui.interact(
            Rect::from_center_size(c, Vec2::splat(d)),
            ui.scope_id().with(("lcd-button", bit)),
            Sense::click_and_drag(),
        );
        let latch = if state.latched[bit] { "latched: right click releases" } else { "right click latches" };
        let resp = named(resp, egui::Role::Button, format!("Button S{bit}"))
            .on_hover_text(format!("S{bit} → IN{bit} (PF{bit}), LED D{bit}\nclick or key {bit} · {latch}"));
        let held = resp.is_pointer_button_down_on() && primary_down(ui);
        input.buttons[bit] = held;
        input.latch_toggled[bit] = resp.secondary_clicked();
        name(x, &format!("S{bit}"));
        let down = held || state.latched[bit] || !high;
        strip_button(&painter, c, d, Color32::from_rgb(0xd9, 0x43, 0x4b), down);
        if state.latched[bit] {
            painter.rect_stroke(
                Rect::from_center_size(c, Vec2::splat(d * 0.92)),
                d * 0.18,
                Stroke::new(1.5, LATCH),
                egui::StrokeKind::Outside,
            );
        }
    }
    label(x1 + 3.5 * d, "IN7 … IN0 (LEDs D7 … D0)");
    // the Nucleo's buttons
    let x2 = x1 + 10.0 * d;
    for (n, (label_text, color)) in
        [("USER", Color32::from_rgb(0x2a, 0x8b, 0xd8)), ("RESET", Color32::from_gray(25))].into_iter().enumerate()
    {
        let c = Pos2::new(x2 + n as f32 * 2.4 * d, y + d * 0.5);
        let resp = ui.interact(
            Rect::from_center_size(c, Vec2::splat(d * 1.2)),
            ui.scope_id().with(("lcd-nucleo", n)),
            Sense::click_and_drag(),
        );
        let (full, hover) = if n == 0 {
            ("USER button B1", "USER button B1 (PC13)")
        } else {
            ("RESET button B2", "RESET button B2 · Ctrl+R")
        };
        let resp = named(resp, egui::Role::Button, full).on_hover_text(hover);
        let down = resp.is_pointer_button_down_on() && primary_down(ui);
        if n == 0 {
            input.user = down;
        } else {
            input.reset_clicked = resp.clicked();
        }
        strip_button(&painter, c, d * 1.1, color, down);
        name(c.x, label_text);
    }
    // the Nucleo's LEDs (under the display on the real board)
    let x3 = x2 + 5.2 * d;
    for (i, (name_text, color)) in
        [("LD1", NUCLEO_GREEN), ("LD2", NUCLEO_BLUE), ("LD3", NUCLEO_RED)].into_iter().enumerate()
    {
        let x = x3 + i as f32 * d * 1.3;
        let duty = state.nucleo_duty[i];
        strip_led(&painter, Pos2::new(x, y - d * 0.45), d, color, duty);
        name(x, name_text);
        let r = Rect::from_center_size(Pos2::new(x, y), Vec2::new(d, 2.0 * d));
        let text = format!("Nucleo {name_text}: {:.0} %", duty * 100.0);
        named(ui.interact(r, ui.scope_id().with(("lcd-ld", i)), Sense::hover()), egui::Role::Label, &text)
            .on_hover_text(text);
    }
    label(x3 + 1.3 * d, "Nucleo");
    input
}

/// Colours of the Nucleo's user LEDs.
pub const NUCLEO_GREEN: Color32 = Color32::from_rgb(0x2E, 0xE0, 0x5A);
pub const NUCLEO_BLUE: Color32 = Color32::from_rgb(0x2F, 0x7B, 0xFF);
pub const NUCLEO_RED: Color32 = Color32::from_rgb(0xFF, 0x2A, 0x22);

/// A tact switch of the row below the display alone: a light square body
/// with a coloured cap, like on the board (so that it does not look like an LED).
fn strip_button(painter: &egui::Painter, c: Pos2, d: f32, color: Color32, down: bool) {
    let body = Rect::from_center_size(c, Vec2::splat(d * 0.82));
    let shade = |c: Color32| if down { lerp(c, Color32::BLACK, 0.4) } else { c };
    painter.rect_filled(body, d * 0.14, shade(Color32::from_gray(205)));
    painter.circle_filled(c, d * 0.27, shade(color));
}

/// An LED of the row below the display alone: a white-hot centre in a glow,
/// like the LEDs on the board.
fn strip_led(painter: &egui::Painter, c: Pos2, d: f32, color: Color32, duty: f32) {
    let b = duty.clamp(0.0, 1.0).powf(1.0 / 2.2);
    if b > 0.01 {
        painter.circle_filled(c, d * 0.5, color.gamma_multiply(0.55 * b));
    }
    painter.circle_filled(c, d * 0.3, lerp(Color32::from_gray(55), color, b));
    if b > 0.01 {
        painter.circle_filled(c, d * 0.15, lerp(color, Color32::WHITE, 0.7).gamma_multiply(b));
    }
}
