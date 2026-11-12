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
        while !rest.is_empty() {
            let end = rest.find(['\n', '\r']);
            let (piece, newline) = match end {
                Some(i) => {
                    let after = if rest[i..].starts_with("\r\n") { i + 2 } else { i + 1 };
                    (&rest[..i], Some(after))
                }
                None => (rest, None),
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
                    self.len += after - piece.len();
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
                    .toggle_value(&mut self.timestamps, "Time")
                    .on_hover_text("Show the virtual time at which each line began")
                    .changed()
                {
                    action.options_changed = true;
                }
            });
        });
        let row = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
        let input_height = row * if narrow { 2.0 } else { 1.0 } + ui.spacing().item_spacing.y + 2.0;
        self.show_text(ui, (ui.available_height() - input_height).max(20.0));
        if narrow {
            ui.horizontal(|ui| action.options_changed |= self.options(ui, false));
            self.input_line(ui, &mut action);
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
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).max_height(height).show(
            ui,
            |ui| {
                let cols = ((ui.available_width() / char_w).floor() as usize).saturating_sub(stamp).max(8);
                // where each line's rows start
                let mut starts = Vec::with_capacity(self.lines.len() + 1);
                let mut total = 0;
                for line in &self.lines {
                    starts.push(total);
                    total += line.chars.div_ceil(cols).max(1);
                }
                starts.push(total);
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::neither().show_rows(ui, row_h, total, |ui, rows| {
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
            },
        );
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
            "Enter sends · ↑↓ history · Esc: keys to the board"
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
}
