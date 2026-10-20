//! Choosing the firmware file: the system's file dialog (rfd), and a file
//! browser drawn with egui for systems that have none. On Linux rfd needs the
//! XDG desktop portal with a FileChooser backend (xdg-desktop-portal-gtk,
//! -kde, -gnome) or zenity; window managers without either (a plain i3, sway
//! or Hyprland setup) show nothing, and rfd reports that like a cancel.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, Key, RichText};

/// A system dialog that returns without a file sooner than this was not
/// shown at all (nobody cancels that quickly).
const NO_DIALOG: Duration = Duration::from_millis(500);

const FIRMWARE: [&str; 2] = ["axf", "elf"];

pub struct FilePicker {
    /// Use the system's file dialog (until it turns out to show nothing).
    native: bool,
    state: State,
    /// Directory the last file was taken from.
    last_dir: Option<PathBuf>,
}

enum State {
    Idle,
    /// The system dialog runs in its own thread, so that the window keeps
    /// being drawn (and the emulator keeps running) while it is open; on
    /// macOS it has already returned.
    Native {
        result: Receiver<Option<PathBuf>>,
        since: Instant,
    },
    Builtin(Browser),
}

impl FilePicker {
    pub fn new() -> Self {
        // ITS_EMU_FILE_DIALOG=builtin|native chooses the dialog
        let native = std::env::var("ITS_EMU_FILE_DIALOG").map_or(true, |v| v != "builtin");
        Self { native, state: State::Idle, last_dir: None }
    }

    pub fn open(&mut self, ctx: &egui::Context) {
        if !matches!(self.state, State::Idle) {
            return;
        }
        if !self.native {
            self.state = State::Builtin(Browser::new(self.start_dir()));
            return;
        }
        let dialog = rfd::FileDialog::new()
            .add_filter("Firmware", &FIRMWARE)
            .add_filter("All files", &["*"])
            .set_title("Open firmware");
        let dialog = match &self.last_dir {
            Some(dir) => dialog.set_directory(dir),
            None => dialog,
        };
        let since = Instant::now();
        let (tx, result) = std::sync::mpsc::channel();
        // AppKit only shows dialogs from the main thread (and there always has one)
        #[cfg(target_os = "macos")]
        {
            tx.send(dialog.pick_file()).ok();
            ctx.request_repaint();
        }
        #[cfg(not(target_os = "macos"))]
        {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                tx.send(dialog.pick_file()).ok();
                ctx.request_repaint();
            });
        }
        self.state = State::Native { result, since };
    }

    /// Draws the built-in browser if it is open; returns the chosen file.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<PathBuf> {
        match &mut self.state {
            State::Idle => None,
            State::Native { result, since } => match result.try_recv() {
                Ok(path) => {
                    let since = *since;
                    self.native_done(path, since)
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(200));
                    None
                }
                Err(TryRecvError::Disconnected) => self.native_done(None, Instant::now()),
            },
            State::Builtin(browser) => match browser.show(ctx) {
                Some(Some(path)) => {
                    self.state = State::Idle;
                    self.taken(path)
                }
                Some(None) => {
                    self.state = State::Idle;
                    None
                }
                None => None,
            },
        }
    }

    fn native_done(&mut self, path: Option<PathBuf>, since: Instant) -> Option<PathBuf> {
        self.state = State::Idle;
        match path {
            Some(path) => self.taken(path),
            None if since.elapsed() < NO_DIALOG => {
                log::warn!(
                    "the system file dialog did not open (on Linux it needs xdg-desktop-portal with a \
                     FileChooser backend such as xdg-desktop-portal-gtk, or zenity); using the built-in one"
                );
                self.native = false;
                self.state = State::Builtin(Browser::new(self.start_dir()));
                None
            }
            None => None,
        }
    }

    fn taken(&mut self, path: PathBuf) -> Option<PathBuf> {
        self.last_dir = path.parent().map(Path::to_path_buf);
        Some(path)
    }

    fn start_dir(&self) -> PathBuf {
        self.last_dir
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .or_else(home_dir)
            .unwrap_or_else(|| PathBuf::from("/"))
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

struct Entry {
    name: String,
    dir: bool,
}

/// File browser in an egui window.
struct Browser {
    dir: PathBuf,
    /// Text of the path field.
    typed: String,
    entries: Vec<Entry>,
    error: Option<String>,
    selected: Option<usize>,
    all_files: bool,
}

impl Browser {
    fn new(dir: PathBuf) -> Self {
        let mut b = Self {
            dir: PathBuf::new(),
            typed: String::new(),
            entries: Vec::new(),
            error: None,
            selected: None,
            all_files: false,
        };
        b.go(dir);
        b
    }

    fn go(&mut self, dir: PathBuf) {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        match std::fs::read_dir(&dir) {
            Ok(read) => {
                self.entries = read
                    .filter_map(Result::ok)
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        // follows symbolic links
                        let dir = std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir());
                        (!name.starts_with('.')).then_some(Entry { name, dir })
                    })
                    .collect();
                self.entries
                    .sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                self.error = None;
                self.typed = display_path(&dir);
                self.dir = dir;
            }
            Err(e) => self.error = Some(format!("{}: {e}", dir.display())),
        }
        self.selected = None;
    }

    fn visible(&self, e: &Entry) -> bool {
        e.dir
            || self.all_files
            || Path::new(&e.name).extension().is_some_and(|x| FIRMWARE.iter().any(|f| x.eq_ignore_ascii_case(f)))
    }

    /// `Some(Some(file))` when a file was chosen, `Some(None)` when cancelled.
    fn show(&mut self, ctx: &egui::Context) -> Option<Option<PathBuf>> {
        let mut outcome = None;
        let mut go_to = None;
        let mut open = true;
        egui::Window::new("Open firmware")
            .open(&mut open)
            .collapsible(false)
            .default_size([560.0, 420.0])
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("⬆").on_hover_text("Parent folder").clicked()
                        && let Some(parent) = self.dir.parent()
                    {
                        go_to = Some(parent.to_path_buf());
                    }
                    if let Some(home) = home_dir()
                        && ui.button("🏠").on_hover_text("Home folder").clicked()
                    {
                        go_to = Some(home);
                    }
                    #[cfg(windows)]
                    for drive in b'A'..=b'Z' {
                        let root = format!("{}:\\", drive as char);
                        if Path::new(&root).exists() && ui.button(&root[..2]).clicked() {
                            go_to = Some(root.into());
                        }
                    }
                    let field = ui.add(egui::TextEdit::singleline(&mut self.typed).desired_width(f32::INFINITY));
                    if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        let typed = PathBuf::from(self.typed.trim());
                        if typed.is_file() {
                            outcome = Some(Some(typed));
                        } else {
                            go_to = Some(typed);
                        }
                    }
                });
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.separator();
                let footer = ui.spacing().interact_size.y + 2.0 * ui.spacing().item_spacing.y + 6.0;
                egui::ScrollArea::vertical().auto_shrink(false).max_height(ui.available_height() - footer).show(
                    ui,
                    |ui| {
                        let mut clicked = None;
                        for (i, e) in self.entries.iter().enumerate().filter(|(_, e)| self.visible(e)) {
                            let label = if e.dir { format!("📁 {}", e.name) } else { format!("🗋 {}", e.name) };
                            let row = ui.selectable_label(self.selected == Some(i), label);
                            if row.clicked() {
                                clicked = Some(i);
                            }
                            if row.double_clicked() {
                                let path = self.dir.join(&e.name);
                                if e.dir {
                                    go_to = Some(path);
                                } else {
                                    outcome = Some(Some(path));
                                }
                            }
                        }
                        if clicked.is_some() {
                            self.selected = clicked;
                        }
                    },
                );
                ui.separator();
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.all_files, "All files")
                        .on_hover_text("Show all files, not only firmware (.axf, .elf)");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let chosen = self.selected.and_then(|i| self.entries.get(i));
                        if ui.add_enabled(chosen.is_some(), egui::Button::new(RichText::new("Open").strong())).clicked()
                            && let Some(e) = chosen
                        {
                            let path = self.dir.join(&e.name);
                            if e.dir {
                                go_to = Some(path);
                            } else {
                                outcome = Some(Some(path));
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            outcome = Some(None);
                        }
                    });
                });
            });
        if !open || ctx.input(|i| i.key_pressed(Key::Escape)) {
            outcome = Some(None);
        }
        if let Some(dir) = go_to {
            self.go(dir);
        }
        outcome
    }
}

/// Path for the path field; without Windows' `\\?\` prefix from canonicalize.
fn display_path(p: &Path) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_lists_folders_first_and_filters_firmware() {
        let dir = std::env::temp_dir().join(format!("its-emu-files-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for f in ["b.txt", "a.AXF", "c.elf", ".hidden.axf"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let mut b = Browser::new(dir.clone());
        let shown = |b: &Browser| b.entries.iter().filter(|e| b.visible(e)).map(|e| e.name.clone()).collect::<Vec<_>>();
        assert_eq!(shown(&b), ["sub", "a.AXF", "c.elf"]);
        b.all_files = true;
        assert_eq!(shown(&b), ["sub", "a.AXF", "b.txt", "c.elf"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
