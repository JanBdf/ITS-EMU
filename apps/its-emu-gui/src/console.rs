//! Serial console of the virtual COM port (USART3): the output of the
//! firmware, what was sent (optional echo) and the input line.

use std::collections::VecDeque;

use eframe::egui::{self, Color32, Key, RichText, text::LayoutJob};

use crate::settings::LineEnding;

/// Text kept in the console (bytes); the older half goes when it is full.
const MAX_TEXT: usize = 200_000;
const MAX_HISTORY: usize = 50;
const ECHO: Color32 = Color32::from_rgb(0x6c, 0xb4, 0xff);

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

pub struct Console {
    /// Received and echoed text in pieces; `true`: sent by the user.
    chunks: VecDeque<(String, bool)>,
    len: usize,
    input: String,
    history: Vec<String>,
    /// Entry of `history` shown in the input line (Up/Down).
    history_pos: Option<usize>,
    /// Every key is sent at once instead of a line on Enter.
    pub send_keys: bool,
    pub line_ending: LineEnding,
    pub echo: bool,
}

impl Console {
    pub fn new(line_ending: LineEnding, echo: bool) -> Self {
        Self {
            chunks: VecDeque::new(),
            len: 0,
            input: String::new(),
            history: Vec::new(),
            history_pos: None,
            send_keys: false,
            line_ending,
            echo,
        }
    }

    /// Output of the firmware.
    pub fn received(&mut self, text: &str) {
        self.push(text, false);
    }

    fn push(&mut self, text: &str, sent: bool) {
        if text.is_empty() {
            return;
        }
        match self.chunks.back_mut() {
            Some((last, s)) if *s == sent => last.push_str(text),
            _ => self.chunks.push_back((text.to_owned(), sent)),
        }
        self.len += text.len();
        if self.len <= MAX_TEXT {
            return;
        }
        let mut excess = self.len - MAX_TEXT / 2;
        while excess > 0
            && let Some((first, _)) = self.chunks.front_mut()
        {
            if first.len() <= excess {
                excess -= first.len();
                self.len -= first.len();
                self.chunks.pop_front();
            } else {
                let cut = (excess..first.len()).find(|&i| first.is_char_boundary(i)).unwrap_or(first.len());
                first.drain(..cut);
                self.len -= cut;
                excess = 0;
            }
        }
    }

    pub fn text(&self) -> String {
        self.chunks.iter().map(|(t, _)| t.as_str()).collect()
    }

    fn clear(&mut self) {
        self.chunks.clear();
        self.len = 0;
    }

    /// Sends `bytes` and shows them if echo is on.
    fn send(&mut self, bytes: Vec<u8>, action: &mut ConsoleAction) {
        if self.echo {
            // Backspace takes back the last echoed character
            if bytes == [0x08] {
                if let Some((last, true)) = self.chunks.back_mut()
                    && let Some(c) = last.pop()
                {
                    self.len -= c.len_utf8();
                }
                action.send.get_or_insert_with(Vec::new).extend(bytes);
                return;
            }
            let shown = String::from_utf8_lossy(&bytes).replace("\r\n", "\n").replace('\r', "\n");
            self.push(&shown, true);
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
            });
        });
        let row = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
        let input_height = row * if narrow { 2.0 } else { 1.0 } + ui.spacing().item_spacing.y + 2.0;
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .max_height((ui.available_height() - input_height).max(20.0))
            .show(ui, |ui| {
                let mut job = LayoutJob::default();
                let font = egui::TextStyle::Monospace.resolve(ui.style());
                let normal = ui.visuals().text_color();
                for (text, sent) in &self.chunks {
                    let format = egui::TextFormat {
                        font_id: font.clone(),
                        color: if *sent { ECHO } else { normal },
                        ..Default::default()
                    };
                    job.append(text, 0.0, format);
                }
                job.wrap.max_width = ui.available_width();
                ui.add(egui::Label::new(job).selectable(true));
            });
        if narrow {
            ui.horizontal(|ui| action.options_changed = self.options(ui, false));
            self.input_line(ui, &mut action);
        } else {
            ui.horizontal(|ui| {
                // options right of the input line, which takes the rest
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    action.options_changed = self.options(ui, true);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        self.input_line(ui, &mut action)
                    });
                });
            });
        }
        action
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
        let mut c = Console::new(LineEnding::Lf, true);
        c.received("abc");
        c.push("x\n", true);
        c.received("def");
        assert_eq!(c.text(), "abcx\ndef");
        assert_eq!(c.chunks.len(), 3);
        c.received(&"y".repeat(MAX_TEXT));
        assert_eq!(c.len, MAX_TEXT / 2);
        assert_eq!(c.text(), "y".repeat(MAX_TEXT / 2));
    }
}
