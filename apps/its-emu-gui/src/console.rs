//! Serial console of the virtual COM port (USART3): the output of the
//! firmware, what was sent (optional echo) and the input line.
//!
//! The text is kept as lines; only the rows on screen are laid out, so a
//! program that prints continuously costs the same per frame as a quiet one.

use std::collections::VecDeque;

use eframe::egui::{self, Color32, Key, RichText, text::LayoutJob};

use crate::settings::LineEnding;

/// Text kept in the console (bytes); the older half goes when it is full.
const MAX_TEXT: usize = 200_000;
const MAX_HISTORY: usize = 50;
const ECHO: Color32 = Color32::from_rgb(0x6c, 0xb4, 0xff);
/// Width of the time stamp column in characters (`[123.456] `).
const STAMP_CHARS: usize = 10;

/// What the console asks the application to do.
#[derive(Default)]
pub struct ConsoleAction {
    /// Bytes to send to the firmware.
    pub send: Option<Vec<u8>>,
    pub hide: bool,
    pub save: bool,
    /// The options changed (to be saved).
    pub options_changed: bool,
}

/// One line of text in pieces; `true`: sent by the user (echo).
struct Line {
    segs: Vec<(String, bool)>,
    /// Characters in all pieces.
    chars: usize,
    /// Virtual time (ns) the line began at.
    at_ns: u64,
    /// The line was ended by a newline (else it is still growing).
    complete: bool,
}

impl Line {
    fn new(at_ns: u64) -> Self {
        Self { segs: Vec::new(), chars: 0, at_ns, complete: false }
    }

    fn push(&mut self, text: &str, sent: bool) {
        self.chars += text.chars().count();
        match self.segs.last_mut() {
            Some((last, s)) if *s == sent => last.push_str(text),
            _ => self.segs.push((text.to_owned(), sent)),
        }
    }

    fn bytes(&self) -> usize {
        self.segs.iter().map(|(t, _)| t.len()).sum()
    }
}

pub struct Console {
    lines: VecDeque<Line>,
    /// Bytes in all lines (newlines included).
    len: usize,
    /// Tail of the received bytes that is not yet a complete UTF-8 character.
    pending: Vec<u8>,
    /// Previous chunk ended with `\r`, so a leading `\n` in the next chunk is part of `\r\n`.
    last_was_cr: bool,
    input: String,
    history: Vec<String>,
    /// Entry of `history` shown in the input line (Up/Down).
    history_pos: Option<usize>,
    /// Every key is sent at once instead of a line on Enter.
    pub send_keys: bool,
    pub line_ending: LineEnding,
    pub echo: bool,
    /// Show the virtual time at which each line began.
    pub timestamps: bool,
}

impl Console {
    pub fn new(line_ending: LineEnding, echo: bool, timestamps: bool) -> Self {
        Self {
            lines: VecDeque::new(),
            len: 0,
            pending: Vec::new(),
            last_was_cr: false,
            input: String::new(),
            history: Vec::new(),
            history_pos: None,
            send_keys: false,
            line_ending,
            echo,
            timestamps,
        }
    }

    /// Output of the firmware, received at virtual time `at_ns`. A multi-byte
    /// character split between two calls is kept until it is complete.
    pub fn received_bytes(&mut self, bytes: &[u8], at_ns: u64) {
        self.pending.extend_from_slice(bytes);
        let taken = std::mem::take(&mut self.pending);
        let (text, rest): (String, Vec<u8>) = match std::str::from_utf8(&taken) {
            Ok(s) => (s.to_owned(), Vec::new()),
            Err(e) => {
                let good = e.valid_up_to();
                match e.error_len() {
                    // incomplete at the end: wait for the rest
                    None if taken.len() - good < 4 => {
                        (String::from_utf8_lossy(&taken[..good]).into_owned(), taken[good..].to_vec())
                    }
                    _ => (String::from_utf8_lossy(&taken).into_owned(), Vec::new()),
                }
            }
        };
        self.pending = rest;
        self.push(&text, false, at_ns);
    }

    /// Output of the firmware (text, no time).
    #[cfg(test)]
    pub fn received(&mut self, text: &str) {
        self.push(text, false, 0);
    }

    fn push(&mut self, text: &str, sent: bool, at_ns: u64) {
        if text.is_empty() {
            return;
        }
        // "\r\n" and "\r" end a line like "\n"; a lone "\r" does not return
        // the cursor (no overwriting in this console)
        let mut rest = text;
        if self.last_was_cr {
            self.last_was_cr = false;
            if let Some(after) = rest.strip_prefix('\n') {
                rest = after;
            }
        }
        while !rest.is_empty() {
            let end = rest.find(['\n', '\r']);
            let (piece, newline, ended_with_cr) = match end {
                Some(i) => {
                    if rest[i..].starts_with("\r\n") {
                        (&rest[..i], Some(i + 2), false)
                    } else {
                        (&rest[..i], Some(i + 1), rest.as_bytes()[i] == b'\r' && i + 1 == rest.len())
                    }
                }
                None => (rest, None, false),
            };
            if self.lines.back().is_none_or(|l| l.complete) {
                self.lines.push_back(Line::new(at_ns));
            }
            let line = self.lines.back_mut().unwrap();
            if !piece.is_empty() {
                line.push(piece, sent);
            }
            self.len += piece.len();
            match newline {
                Some(after) => {
                    line.complete = true;
                    self.len += 1;
                    self.last_was_cr = ended_with_cr;
                    rest = &rest[after..];
                }
                None => break,
            }
        }
        if self.len <= MAX_TEXT {
            return;
        }
        let mut excess = self.len - MAX_TEXT / 2;
        while excess > 0
            && let Some(first) = self.lines.front()
        {
            let bytes = first.bytes() + usize::from(first.complete);
            if bytes <= excess || self.lines.len() == 1 {
                excess = excess.saturating_sub(bytes);
                self.len -= bytes.min(self.len);
                self.lines.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.len);
        for line in &self.lines {
            for (t, _) in &line.segs {
                out.push_str(t);
            }
            if line.complete {
                out.push('\n');
            }
        }
        out
    }

    fn clear(&mut self) {
        self.lines.clear();
        self.len = 0;
        self.pending.clear();
        self.last_was_cr = false;
    }

    /// Sends `bytes` and shows them if echo is on.
    fn send(&mut self, bytes: Vec<u8>, action: &mut ConsoleAction) {
        if self.echo {
            // Backspace takes back the last echoed character
            if bytes == [0x08] {
                if let Some(line) = self.lines.back_mut()
                    && !line.complete
                    && let Some((last, true)) = line.segs.last_mut()
                    && let Some(c) = last.pop()
                {
                    line.chars -= 1;
                    self.len -= c.len_utf8();
                    if last.is_empty() {
                        line.segs.pop();
                    }
                }
                action.send.get_or_insert_with(Vec::new).extend(bytes);
                return;
            }
            let shown = String::from_utf8_lossy(&bytes).into_owned();
            let at = self.lines.back().map_or(0, |l| l.at_ns);
            self.push(&shown, true, at);
        }
        action.send.get_or_insert_with(Vec::new).extend(bytes);
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> ConsoleAction {
        let mut action = ConsoleAction::default();
        // in a narrow panel (right of the board) the options get a row of their own
        let narrow = ui.available_width() < 640.0;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Serial console").strong())
                .on_hover_text("Virtual COM port of the ST-LINK: USART3 (PD8/PD9), 115200 baud, 8N1");
            if !narrow {
                ui.label(RichText::new("USART3 · 115200 8N1").weak());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").on_hover_text("Hide the serial console · View menu").clicked() {
                    action.hide = true;
                }
                if ui.small_button("Save…").on_hover_text("Save the text to a file").clicked() {
                    action.save = true;
                }
                if ui.small_button("Copy").on_hover_text("Copy all text to the clipboard").clicked() {
                    ui.ctx().copy_text(self.text());
                }
                if ui.small_button("Clear").clicked() {
                    self.clear();
                }
                if ui
                    .checkbox(&mut self.timestamps, "Time")
                    .on_hover_text("Show the virtual time at which each line began")
                    .changed()
                {
                    action.options_changed = true;
                }
            });
        });
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            if narrow {
                self.input_line(ui, &mut action);
                ui.horizontal(|ui| action.options_changed |= self.options(ui, false));
            } else {
                ui.horizontal(|ui| {
                    // options right of the input line, which takes the rest
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        action.options_changed |= self.options(ui, true);
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            self.input_line(ui, &mut action);
                        });
                    });
                });
            }
            let text_h = ui.available_height().max(20.0);
            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                self.show_text(ui, text_h);
            });
        });
        action
    }

    /// The text, wrapped at the panel's width in rows of equal height; only
    /// the visible rows are laid out.
    fn show_text(&mut self, ui: &mut egui::Ui, height: f32) {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let (char_w, row_h) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let normal = ui.visuals().text_color();
        let weak = ui.visuals().weak_text_color();
        let stamp = if self.timestamps { STAMP_CHARS } else { 0 };
        let format = |color: Color32| egui::TextFormat { font_id: font.clone(), color, ..Default::default() };
        // the job for row `part` of `line`: characters [part * cols, (part + 1) * cols)
        let row_job = |line: &Line, part: usize, cols: usize| {
            let mut job = LayoutJob::default();
            if stamp > 0 {
                let text =
                    if part == 0 { format!("[{:7.3}] ", line.at_ns as f64 / 1e9) } else { " ".repeat(STAMP_CHARS) };
                job.append(&text, 0.0, format(weak));
            }
            let (from, to) = (part * cols, (part + 1) * cols);
            let mut at = 0;
            for (text, sent) in &line.segs {
                let n_chars = text.chars().count();
                let (s0, s1) = (at.max(from), (at + n_chars).min(to));
                if s0 < s1 {
                    let byte = |c: usize| text.char_indices().nth(c - at).map_or(text.len(), |(i, _)| i);
                    job.append(&text[byte(s0)..byte(s1)], 0.0, format(if *sent { ECHO } else { normal }));
                }
                at += n_chars;
            }
            if job.sections.is_empty() {
                job.append(" ", 0.0, format(normal));
            }
            job
        };
        // one scroll area that lays out only the visible rows (inside another
        // scroll area show_rows saw only the first screenful: blank rows
        // once the text was longer)
        let width = ui.available_width() - ui.spacing().scroll.allocated_width();
        let cols = ((width / char_w).floor() as usize).saturating_sub(stamp).max(8);
        // where each line's rows start
        let mut starts = Vec::with_capacity(self.lines.len() + 1);
        let mut total = 0;
        for line in &self.lines {
            starts.push(total);
            total += line.chars.div_ceil(cols).max(1);
        }
        starts.push(total);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .max_height(height)
                .show_rows(ui, row_h, total, |ui, rows| {
                    let mut n = starts.partition_point(|&s| s <= rows.start).saturating_sub(1);
                    for row in rows {
                        while n + 1 < starts.len() && starts[n + 1] <= row {
                            n += 1;
                        }
                        let Some(line) = self.lines.get(n) else { break };
                        let job = row_job(line, row - starts[n], cols);
                        ui.add(egui::Label::new(job).selectable(true).wrap_mode(egui::TextWrapMode::Extend));
                    }
                });
        });
    }

    /// Line ending, single keys, echo; `rtl`: added in a right-to-left
    /// layout. True when a saved option changed.
    fn options(&mut self, ui: &mut egui::Ui, rtl: bool) -> bool {
        let before = (self.line_ending, self.echo);
        let order = if rtl { [3, 2, 1, 0] } else { [0, 1, 2, 3] };
        for item in order {
            match item {
                0 => {
                    ui.label("Enter sends");
                }
                1 => {
                    egui::ComboBox::from_id_salt("line ending")
                        .selected_text(self.line_ending.label())
                        .width(84.0)
                        .show_ui(ui, |ui| {
                            for (ending, _) in LineEnding::ALL {
                                ui.selectable_value(&mut self.line_ending, ending, ending.label());
                            }
                        })
                        .response
                        .on_hover_text("What Enter sends after the line");
                }
                2 => {
                    ui.checkbox(&mut self.send_keys, "Each key").on_hover_text(
                        "Send every key at once (for programs that read single characters with \
                         stdin_getchar); Backspace sends 0x08",
                    );
                }
                _ => {
                    ui.checkbox(&mut self.echo, "Echo")
                        .on_hover_text("Show what is sent (in blue); off when the program echoes itself");
                }
            }
        }
        before != (self.line_ending, self.echo)
    }

    fn input_line(&mut self, ui: &mut egui::Ui, action: &mut ConsoleAction) {
        let hint = if self.send_keys {
            "every key is sent at once · Esc: keys to the board"
        } else {
            "Type a line · ↑↓ history · Esc: keys to the board"
        };
        let mut out = egui::TextEdit::singleline(&mut self.input).hint_text(hint).desired_width(f32::INFINITY).show(ui);
        let edit = out.response.clone();
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if self.send_keys {
            if !self.input.is_empty() {
                let typed = std::mem::take(&mut self.input).into_bytes();
                self.send(typed, action);
            }
            if edit.has_focus() && ui.input(|i| i.key_pressed(Key::Backspace)) {
                self.send(vec![0x08], action);
            }
            if enter {
                self.send(self.line_ending.bytes().to_vec(), action);
                edit.request_focus();
            }
            return;
        }
        if enter {
            let line = std::mem::take(&mut self.input);
            if !line.is_empty() && self.history.last() != Some(&line) {
                self.history.push(line.clone());
                if self.history.len() > MAX_HISTORY {
                    self.history.remove(0);
                }
            }
            self.history_pos = None;
            let mut bytes = line.into_bytes();
            bytes.extend_from_slice(self.line_ending.bytes());
            self.send(bytes, action);
            edit.request_focus();
        } else if edit.has_focus() && !self.history.is_empty() {
            let (up, down) = ui.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown)));
            let pos = match (up, down, self.history_pos) {
                (true, _, None) => Some(Some(self.history.len() - 1)),
                (true, _, Some(p)) => Some(Some(p.saturating_sub(1))),
                (_, true, Some(p)) if p + 1 < self.history.len() => Some(Some(p + 1)),
                (_, true, Some(_)) => Some(None),
                _ => None,
            };
            if let Some(pos) = pos {
                self.history_pos = pos;
                self.input = pos.map(|p| self.history[p].clone()).unwrap_or_default();
                // cursor to the end of the line
                let end = egui::text::CCursor::new(self.input.chars().count());
                out.state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
                out.state.store(ui.ctx(), edit.id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newer_half() {
        let mut c = Console::new(LineEnding::Lf, true, false);
        c.received("abc");
        c.push("x\n", true, 0);
        c.received("def");
        assert_eq!(c.text(), "abcx\ndef");
        assert_eq!(c.lines.len(), 2);
        assert_eq!(c.lines[0].segs.len(), 2);
        for _ in 0..MAX_TEXT / 100 {
            c.received(&"y".repeat(99));
            c.received("\n");
        }
        assert!(c.len <= MAX_TEXT && c.len >= MAX_TEXT / 2 - 100, "{}", c.len);
        assert!(c.text().starts_with("yyyy"));
    }

    #[test]
    fn utf8_split_between_snapshots() {
        let mut c = Console::new(LineEnding::Lf, false, false);
        let bytes = "21.5 °C\n".as_bytes();
        c.received_bytes(&bytes[..5], 1);
        c.received_bytes(&bytes[5..], 2);
        assert_eq!(c.text(), "21.5 °C\n");
        c.received_bytes(b"\xFFx\r\ny", 3);
        assert_eq!(c.text(), "21.5 °C\n\u{FFFD}x\ny");
        assert_eq!(c.lines[1].at_ns, 3);
    }

    #[test]
    fn bottom_panel_keeps_resized_height() {
        for width in [1000.0, 500.0] {
            let ctx = egui::Context::default();
            let mut console = Console::new(LineEnding::Lf, false, false);
            for i in 0..100 {
                console.received(&format!("line {i}: hello world\n"));
            }

            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 800.0));
            let mut show_console = true;
            let mut run_frame = |events: Vec<egui::Event>, show: bool| -> egui::Rect {
                let input = egui::RawInput { screen_rect: Some(screen), events, time: Some(0.0), ..Default::default() };
                let mut panel_rect = egui::Rect::NOTHING;
                let mut out = ctx.run_ui(input, |ui| {
                    if show {
                        panel_rect = egui::Panel::bottom("console")
                            .resizable(true)
                            .default_size(180.0)
                            .min_size(80.0)
                            .show(ui, |ui| console.show(ui))
                            .response
                            .rect;
                    }
                    egui::CentralPanel::default().show(ui, |_| {});
                });
                out.textures_delta.clear();
                panel_rect
            };

            let r0 = run_frame(vec![], show_console);
            let r1 = run_frame(vec![], show_console);
            let r2 = run_frame(vec![], show_console);
            assert!((r0.height() - 180.0).abs() < 1.0, "width={width}: initial {}", r0.height());
            assert!((r1.height() - r0.height()).abs() < 0.5, "width={width}: frame 1 drifted to {}", r1.height());
            assert!((r2.height() - r0.height()).abs() < 0.5, "width={width}: frame 2 drifted to {}", r2.height());

            // Drag the top edge up to height = 300, then back down to height = 140
            for target_h in [300.0_f32, 140.0] {
                let current = run_frame(vec![], show_console);
                let p_start = egui::pos2(width * 0.5, current.top());
                let p_end = egui::pos2(width * 0.5, screen.bottom() - target_h);
                let _ = run_frame(vec![egui::Event::PointerMoved(p_start)], show_console);
                let _ = run_frame(
                    vec![egui::Event::PointerButton {
                        pos: p_start,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    show_console,
                );
                let _ = run_frame(vec![egui::Event::PointerMoved(p_end)], show_console);
                let r_rel = run_frame(
                    vec![egui::Event::PointerButton {
                        pos: p_end,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    show_console,
                );
                assert!(
                    (r_rel.height() - target_h).abs() < 2.0,
                    "width={width}: expected ~{target_h}, got {}",
                    r_rel.height()
                );
                for frame in 0..10 {
                    let h = run_frame(vec![], show_console).height();
                    assert!(
                        (h - r_rel.height()).abs() < 0.5,
                        "width={width} target={target_h} frame={frame}: height drifted from {} to {h}",
                        r_rel.height()
                    );
                }
            }

            // Hiding and showing again within the same session keeps the resized height
            show_console = false;
            for _ in 0..3 {
                let _ = run_frame(vec![], show_console);
            }
            show_console = true;
            let reopened = run_frame(vec![], show_console);
            assert!(
                (reopened.height() - 140.0).abs() < 2.0,
                "width={width}: reopened height was {}",
                reopened.height()
            );
        }
    }

    #[test]
    fn right_panel_keeps_resized_width() {
        let ctx = egui::Context::default();
        let mut console = Console::new(LineEnding::Lf, false, false);
        console.received("hello\nworld\n");

        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
        let mut run_frame = |events: Vec<egui::Event>| -> egui::Rect {
            let input = egui::RawInput { screen_rect: Some(screen), events, time: Some(0.0), ..Default::default() };
            let mut panel_rect = egui::Rect::NOTHING;
            let mut out = ctx.run_ui(input, |ui| {
                panel_rect = egui::Panel::right("side")
                    .resizable(true)
                    .default_size(360.0)
                    .min_size(240.0)
                    .show(ui, |ui| console.show(ui))
                    .response
                    .rect;
                egui::CentralPanel::default().show(ui, |_| {});
            });
            out.textures_delta.clear();
            panel_rect
        };

        let r0 = run_frame(vec![]);
        assert!((r0.width() - 360.0).abs() < 1.0, "initial width {}", r0.width());
        for target_w in [480.0_f32, 320.0] {
            let current = run_frame(vec![]);
            let p_start = egui::pos2(current.left(), 400.0);
            let p_end = egui::pos2(screen.right() - target_w, 400.0);
            let _ = run_frame(vec![egui::Event::PointerMoved(p_start)]);
            let _ = run_frame(vec![egui::Event::PointerButton {
                pos: p_start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }]);
            let _ = run_frame(vec![egui::Event::PointerMoved(p_end)]);
            let r_rel = run_frame(vec![egui::Event::PointerButton {
                pos: p_end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            assert!((r_rel.width() - target_w).abs() < 2.0, "expected ~{target_w}, got {}", r_rel.width());
            for frame in 0..10 {
                let w = run_frame(vec![]).width();
                assert!(
                    (w - r_rel.width()).abs() < 0.5,
                    "target={target_w} frame={frame}: width drifted from {} to {w}",
                    r_rel.width()
                );
            }
        }
    }
}
