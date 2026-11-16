//! The tutorial of the first start: popups that lead through the inputs of
//! the board one after another, while the tutorial program
//! (tests/fixtures/ac6-src/Tutorial) shows on the display what it reads.
//! A step is done when the GUI sees the input, whatever program runs; it
//! then moves on by itself.

use std::path::PathBuf;

use eframe::egui::{self, Align2, Color32, Pos2, Rect, RichText, Stroke, Vec2};

use crate::app::OK_GREEN;
use crate::board::Spots;

/// The tutorial program, as built by tools/build-own.sh.
const FIRMWARE: &[u8] = include_bytes!("../assets/tutorial.axf");
const FIRMWARE_NAME: &str = "Tutorial.axf";

/// Width of the popup's text.
const WIDTH: f32 = 380.0;
const TEXT_SIZE: f32 = 16.0;
const TITLE_SIZE: f32 = 19.0;
/// Space between the popup and what it points at.
const GAP: f32 = 28.0;
const HIGHLIGHT: Color32 = Color32::from_rgb(0xFF, 0xC0, 0x20);
/// How long a done step stays (seconds), so that its effect can be seen.
const ADVANCE_AFTER: f64 = 1.8;

/// What the user did that a step waits for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Goal {
    Button,
    /// A latched button released again.
    Unlatch,
    User,
    /// An AUX pad at 3V3.
    Aux,
    /// An ADC pad at 2 V or more.
    Analog,
    Touch,
    Serial,
}

/// What a step points at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Spot {
    Buttons,
    User,
    Aux,
    Analog,
    Display,
    Console,
    Open,
}

/// Where the popup goes.
#[derive(Clone, Copy)]
enum Place {
    /// This corner of the popup at a point of the board (design units), so
    /// that it leaves free what the step is about.
    Board(Align2, f32, f32),
    /// Above the console, or left of it when it is right of the board.
    Console,
    /// Below the Open button.
    Open,
}

struct Step {
    title: &'static str,
    /// `{open}` and `{help}` stand for the shortcuts.
    text: &'static str,
    spot: Spot,
    place: Place,
    goal: Option<Goal>,
    /// Shown once the goal is reached.
    done: &'static str,
}

/// Left of the display, over the Nucleo.
const LEFT_OF_DISPLAY: Place = Place::Board(Align2::RIGHT_CENTER, 505.0, 200.0);

const STEPS: [Step; 9] = [
    Step {
        title: "Welcome",
        text: "This is the ITS-Board of the course: the STM32F429 Nucleo on the carrier board, with \
               the display on top. A tutorial program runs on it now. It shows every input on the \
               display and reports each change in the serial console.\n\nThe next steps show how to \
               work the inputs; each moves on by itself once you have done it.",
        spot: Spot::Display,
        place: LEFT_OF_DISPLAY,
        goal: None,
        done: "",
    },
    Step {
        title: "Buttons S0–S7",
        text: "Click a button, or hold one of the keys 0–7. The buttons are active low: the red LED \
               above a button shows the level of its input (IN0–7 = PF0–7) and goes dark while it \
               is pressed. The program lights the blue LED OUT0–7 of the same number.",
        spot: Spot::Buttons,
        // above the In LEDs
        place: Place::Board(Align2::CENTER_BOTTOM, 798.0, 500.0),
        goal: Some(Goal::Button),
        done: "The program saw the button.",
    },
    Step {
        title: "Holding a button",
        text: "Right-click a button: it stays pressed (yellow ring), so you can hold several at once. \
               Right-click it again to release it.",
        spot: Spot::Buttons,
        place: Place::Board(Align2::CENTER_BOTTOM, 798.0, 500.0),
        goal: Some(Goal::Unlatch),
        done: "Released.",
    },
    Step {
        title: "USER button",
        text: "The blue USER button of the Nucleo (PC13) is clicked the same way. Unlike S0–S7 it \
               reads high while pressed.",
        spot: Spot::User,
        // below and left of the button
        place: Place::Board(Align2::RIGHT_TOP, 335.0, 130.0),
        goal: Some(Goal::User),
        done: "The program saw USER.",
    },
    Step {
        title: "Digital inputs AUX0–5",
        text: "The pads AUX0–AUX5 (INT0–5 = PG0–5) take a wire. Click a pad to connect it to GND, \
               again for 3V3, a third time to leave it open. Bring one pad to 3V3.\n\nThe program \
               tells the three apart by switching the pin's pull-up and pull-down.",
        spot: Spot::Aux,
        place: Place::Board(Align2::RIGHT_CENTER, 905.0, 370.0),
        goal: Some(Goal::Aux),
        done: "3V3: see the AUX row on the display.",
    },
    Step {
        title: "Analog inputs",
        text: "Point at an Analog pad and turn the mouse wheel up until it reads 2 V or more \
               (0.05 V per step from 0 V; double-click for 3.3 V or open again, right-click for a \
               slider). The program converts the pads with ADC1 and ADC3 and shows the last change \
               on OUT8–15, too.\n\nDAC1 is an output: it shows the voltage the DAC puts out.",
        spot: Spot::Analog,
        // left of the pads (DAC1 too), above the Out LEDs
        place: Place::Board(Align2::RIGHT_BOTTOM, 420.0, 470.0),
        goal: Some(Goal::Analog),
        done: "2 V: the bar on the display follows the voltage.",
    },
    Step {
        title: "Touch screen",
        text: "Click or drag in the box on the display: the program draws where you touch it. \
               Right-click the display to show it alone and enlarged; Esc returns.",
        spot: Spot::Display,
        place: LEFT_OF_DISPLAY,
        goal: Some(Goal::Touch),
        done: "Touched.",
    },
    Step {
        title: "Serial console",
        text: "printf in a program writes to the virtual COM port (USART3), shown in the console. \
               Type a line in its input field and press Enter: the program answers.",
        spot: Spot::Console,
        place: Place::Console,
        goal: Some(Goal::Serial),
        done: "Sent. The answer is in the console.",
    },
    Step {
        title: "Your own program",
        text: "Open your .axf file with Open… ({open}) or drop it onto the window. It is loaded \
               again whenever you rebuild it.\n\n{help} lists all controls; Help ▸ Tutorial starts \
               this tour again.",
        spot: Spot::Open,
        place: Place::Open,
        goal: None,
        done: "",
    },
];

/// Screen rectangles of the parts a step can point at, and the board's panel.
pub struct Targets {
    pub board: Spots,
    pub console: Option<Rect>,
    pub open: Option<Rect>,
    pub area: Rect,
}

#[derive(Default)]
pub struct Tutorial {
    step: usize,
    /// The current step's goal was reached (and when, egui time).
    met: bool,
    met_at: Option<f64>,
    /// The tutorial program was loaded.
    loaded: bool,
    /// Size of the popup in the last frame.
    size: Option<Vec2>,
}

impl Tutorial {
    /// Notes an input of the user.
    pub fn saw(&mut self, goal: Goal) {
        if STEPS[self.step].goal == Some(goal) {
            self.met = true;
        }
    }

    /// Follows the loaded program (`firmware` as the status bar names it):
    /// false once another one replaced the tutorial program (a file was
    /// opened, a debugger uploaded one).
    pub fn program_kept(&mut self, firmware: Option<&str>) -> bool {
        let ours = firmware == Some(FIRMWARE_NAME);
        self.loaded |= ours;
        ours || !self.loaded
    }

    pub fn spot(&self) -> Spot {
        STEPS[self.step].spot
    }

    fn go_to(&mut self, step: usize) {
        self.step = step;
        self.met = false;
        self.met_at = None;
    }

    /// Shows the current step; false once the tutorial was finished or ended.
    pub fn show(&mut self, ctx: &egui::Context, targets: &Targets, open_key: &str, help_key: &str) -> bool {
        // a done step moves on by itself after a moment (the last step has no goal)
        let now = ctx.input(|i| i.time);
        if self.met {
            let at = *self.met_at.get_or_insert(now);
            if now - at >= ADVANCE_AFTER {
                self.go_to(self.step + 1);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(ADVANCE_AFTER - (now - at)));
            }
        }
        let step = &STEPS[self.step];
        let last = self.step + 1 == STEPS.len();
        let at = self.place(step.place, targets);
        let (mut next, mut back, mut end) = (false, false, false);
        let text = |t: &str| RichText::new(t).size(TEXT_SIZE);
        let window = egui::Window::new(RichText::new(step.title).size(TITLE_SIZE).strong())
            .id(egui::Id::unique("tutorial"))
            .collapsible(false)
            .resizable(false)
            .fixed_pos(at)
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                self.progress(ui);
                ui.add_space(4.0);
                ui.label(text(&step.text.replace("{open}", open_key).replace("{help}", help_key)));
                if step.goal.is_some() && self.met {
                    ui.add_space(2.0);
                    ui.label(text(&format!("✔ {}", step.done)).color(OK_GREEN));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if !last && ui.link(text("End tutorial")).clicked() {
                        end = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // a step that waits for an input can be skipped
                        let label = match (last, step.goal.is_some() && !self.met) {
                            (true, _) => "Finish",
                            (false, true) => "Skip",
                            (false, false) => "Next",
                        };
                        let mut button = egui::Button::new(text(label));
                        if label != "Skip" {
                            button = button.fill(ui.visuals().selection.bg_fill);
                        }
                        next = ui.add(button).clicked();
                        back = self.step > 0 && ui.button(text("Back")).clicked();
                    });
                });
            });
        if let Some(window) = &window {
            self.size = Some(window.response.rect.size());
        }
        let target = match step.spot {
            Spot::Buttons => targets.board.buttons,
            Spot::User => targets.board.user,
            Spot::Aux => targets.board.aux,
            Spot::Analog => targets.board.analog,
            Spot::Display => targets.board.display,
            Spot::Console => targets.console,
            Spot::Open => targets.open,
        };
        if let (Some(target), Some(window)) = (target, window) {
            highlight(ctx, window.response.rect, target);
        }
        if next && last {
            return false;
        }
        if next {
            self.go_to(self.step + 1);
        } else if back {
            self.go_to(self.step - 1);
        }
        !end
    }

    /// A dot per step (done, current, to come) and the step number.
    fn progress(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let weak = ui.visuals().weak_text_color();
            for n in 0..STEPS.len() {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), egui::Sense::hover());
                let (c, p) = (rect.center(), ui.painter());
                if n < self.step {
                    p.circle_filled(c, 4.5, HIGHLIGHT);
                } else if n == self.step {
                    p.circle_filled(c, 6.0, if self.met { OK_GREEN } else { HIGHLIGHT });
                } else {
                    p.circle_stroke(c, 4.5, Stroke::new(1.5, weak));
                }
            }
            ui.add_space(6.0);
            ui.label(RichText::new(format!("Step {} of {}", self.step + 1, STEPS.len())).size(14.0).weak());
        });
    }

    /// Top left corner of the popup for `place`, inside the board's panel.
    fn place(&self, place: Place, targets: &Targets) -> Pos2 {
        let size = self.size.unwrap_or(Vec2::new(WIDTH + 20.0, 300.0));
        let rect = match place {
            Place::Board(pivot, x, y) => targets.board.point(x, y).map(|p| pivot.anchor_size(p, size)),
            Place::Console => targets.console.map(|c| {
                if c.width() > c.height() {
                    Align2::CENTER_BOTTOM.anchor_size(c.center_top() - Vec2::new(0.0, GAP), size)
                } else {
                    Align2::RIGHT_CENTER.anchor_size(c.left_center() - Vec2::new(GAP, 0.0), size)
                }
            }),
            Place::Open => {
                targets.open.map(|o| Align2::LEFT_TOP.anchor_size(o.left_bottom() + Vec2::new(0.0, GAP), size))
            }
        };
        // the display alone, or the board zoomed: at the left of the panel
        let rect = rect.unwrap_or_else(|| {
            Align2::LEFT_CENTER.anchor_size(targets.area.left_center() + Vec2::new(12.0, 0.0), size)
        });
        inside(rect, targets.area.shrink(8.0)).min
    }
}

/// `r` moved into `bounds` (its top left corner when it is larger).
fn inside(r: Rect, bounds: Rect) -> Rect {
    // back from the far side first, then from the near one
    let shift = |lo: f32, hi: f32, min: f32, max: f32| {
        let d = (max - hi).min(0.0);
        d + (min - (lo + d)).max(0.0)
    };
    r.translate(Vec2::new(
        shift(r.left(), r.right(), bounds.left(), bounds.right()),
        shift(r.top(), r.bottom(), bounds.top(), bounds.bottom()),
    ))
}

/// A pulsing frame around `target` and a line to it from the popup.
fn highlight(ctx: &egui::Context, popup: Rect, target: Rect) {
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("tutorial-spot")));
    let pulse = ((ctx.input(|i| i.time) * 4.0).sin() * 0.5 + 0.5) as f32;
    // inside the window (the console reaches its edges)
    let frame = target.expand(5.0 + 3.0 * pulse).intersect(ctx.content_rect().shrink(3.0));
    painter.rect_stroke(frame, 6.0, Stroke::new(3.5, HIGHLIGHT), egui::StrokeKind::Outside);
    if !popup.intersects(frame) {
        let (a, b) = (edge(popup, frame.center()), edge(frame, popup.center()));
        painter.line_segment([a, b], Stroke::new(3.0, HIGHLIGHT));
        painter.circle_filled(a, 4.0, HIGHLIGHT);
        painter.circle_filled(b, 6.0, HIGHLIGHT);
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(40));
}

/// Where the line from the centre of `r` towards `p` leaves `r`.
fn edge(r: Rect, p: Pos2) -> Pos2 {
    let d = p - r.center();
    let scale = (r.width() / 2.0 / d.x.abs()).min(r.height() / 2.0 / d.y.abs()).min(1.0);
    r.center() + d * scale
}

/// Writes the tutorial program next to the emulator's cached files and
/// returns its path (loaded as a file, the title bar and Reset work as usual).
pub fn firmware_file() -> std::io::Result<PathBuf> {
    let path = rp2040::bootrom::cache_path().with_file_name(FIRMWARE_NAME);
    if std::fs::read(&path).ok().as_deref() != Some(FIRMWARE) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, FIRMWARE)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_leaves_the_rectangle_at_its_edge() {
        let r = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 50.0));
        assert_eq!(edge(r, Pos2::new(250.0, 25.0)), Pos2::new(100.0, 25.0));
        assert_eq!(edge(r, Pos2::new(50.0, -100.0)), Pos2::new(50.0, 0.0));
        // a point inside stays where it is
        assert_eq!(edge(r, Pos2::new(60.0, 30.0)), Pos2::new(60.0, 30.0));
    }

    #[test]
    fn popup_is_moved_into_the_panel() {
        let bounds = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(500.0, 400.0));
        let r = Rect::from_min_size(Pos2::new(-50.0, 350.0), Vec2::new(100.0, 100.0));
        assert_eq!(inside(r, bounds).min, Pos2::new(0.0, 300.0));
        let r = Rect::from_min_size(Pos2::new(450.0, -20.0), Vec2::new(100.0, 100.0));
        assert_eq!(inside(r, bounds).min, Pos2::new(400.0, 0.0));
        // larger than the panel: its top left corner in it
        let r = Rect::from_min_size(Pos2::new(-10.0, -10.0), Vec2::new(600.0, 500.0));
        assert_eq!(inside(r, bounds).min, Pos2::new(0.0, 0.0));
    }

    #[test]
    fn ends_when_another_program_is_loaded() {
        let mut t = Tutorial::default();
        // before the tutorial program is there
        assert!(t.program_kept(Some("Mine.axf")));
        assert!(t.program_kept(Some(FIRMWARE_NAME)));
        assert!(!t.program_kept(Some("uploaded by debugger")));
    }

    #[test]
    fn firmware_is_an_elf_file() {
        assert_eq!(&FIRMWARE[..4], b"\x7fELF");
    }
}
