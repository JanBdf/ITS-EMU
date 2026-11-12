//! Settings the GUI remembers between runs: `key = value` lines in
//! `$XDG_CONFIG_HOME/its-emu/gui.conf` (`~/.config/...`; on Windows/macOS
//! the platform's config directory).

use std::path::PathBuf;

use itsboard::spec::DeviceSpec;

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// The ST-LINK is plugged in (USB/IP) at start.
    pub usb: bool,
    /// The firmware file is loaded again when a new build replaces it.
    pub auto_reload: bool,
    /// Firmware files opened last, the latest first.
    pub recent: Vec<PathBuf>,
    /// Devices on the header pins.
    pub devices: Vec<DeviceSpec>,
    /// Boot ROM of the Picos (`None`: downloaded into the cache).
    pub pico_bootrom: Option<PathBuf>,
    /// Python interpreter for the programs on a Pico's serial port.
    pub python: String,
    /// The Python program started for a Pico last.
    pub python_program: Option<PathBuf>,
    /// The serial console is shown.
    pub console: bool,
    /// Where the serial console is.
    pub console_side: ConsoleSide,
    /// What Enter sends after the line in the serial console.
    pub line_ending: LineEnding,
    /// The serial console shows what was sent.
    pub echo: bool,
    /// The serial console shows the virtual time of each line.
    pub timestamps: bool,
    /// Light, dark or the system's theme.
    pub theme: Theme,
    /// Emulation speed (virtual seconds per second; 0: as fast as possible).
    pub speed: f64,
    /// Contact bounce of the buttons and encoders.
    pub bounce: bool,
    /// Window size and position of the last run (logical points).
    pub window: Option<WindowState>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowState {
    pub size: [f32; 2],
    pub pos: Option<[f32; 2]>,
    pub maximized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Theme {
    System,
    Dark,
    Light,
}

impl Theme {
    pub const ALL: [(Self, &str); 3] = [(Self::System, "system"), (Self::Dark, "dark"), (Self::Light, "light")];
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConsoleSide {
    /// Below the board, or to its right in wide windows.
    Auto,
    Bottom,
    Right,
}

impl ConsoleSide {
    pub const ALL: [(Self, &str); 3] = [(Self::Auto, "auto"), (Self::Bottom, "bottom"), (Self::Right, "right")];
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineEnding {
    Lf,
    Cr,
    CrLf,
    None,
}

impl LineEnding {
    pub const ALL: [(Self, &str); 4] = [(Self::Lf, "lf"), (Self::Cr, "cr"), (Self::CrLf, "crlf"), (Self::None, "none")];

    pub fn bytes(self) -> &'static [u8] {
        match self {
            Self::Lf => b"\n",
            Self::Cr => b"\r",
            Self::CrLf => b"\r\n",
            Self::None => b"",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF (\\n)",
            Self::Cr => "CR (\\r)",
            Self::CrLf => "CR LF",
            Self::None => "nothing",
        }
    }
}

/// The name of `value` in the settings file.
fn name<T: PartialEq + Copy>(all: &[(T, &'static str)], value: T) -> &'static str {
    all.iter().find(|(v, _)| *v == value).map_or("", |(_, n)| n)
}

fn by_name<T: Copy>(all: &[(T, &str)], name: &str) -> Option<T> {
    all.iter().find(|(_, n)| *n == name).map(|(v, _)| *v)
}

const MAX_RECENT: usize = 8;

impl Default for Settings {
    fn default() -> Self {
        // Like the real board: the probe is there when the emulator runs.
        Self {
            usb: cfg!(target_os = "linux"),
            auto_reload: true,
            recent: Vec::new(),
            devices: Vec::new(),
            pico_bootrom: None,
            python: if cfg!(windows) { "python" } else { "python3" }.to_owned(),
            python_program: None,
            console: true,
            console_side: ConsoleSide::Auto,
            line_ending: LineEnding::Lf,
            echo: false,
            timestamps: false,
            theme: Theme::System,
            speed: 1.0,
            // like the real board: students debounce
            bounce: true,
            window: None,
        }
    }
}

impl Settings {
    fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("its-emu").join("gui.conf"))
    }

    pub fn load() -> Self {
        let mut s = Self::default();
        let Some(text) = Self::path().and_then(|p| std::fs::read_to_string(p).ok()) else { return s };
        s.parse(&text);
        s
    }

    fn parse(&mut self, text: &str) {
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            let on = matches!(value, "true" | "1" | "yes" | "on");
            match key.trim() {
                "usb" => self.usb = on,
                "auto_reload" => self.auto_reload = on,
                "console" => self.console = on,
                "echo" => self.echo = on,
                "timestamps" => self.timestamps = on,
                "bounce" => self.bounce = on,
                "theme" => self.theme = by_name(&Theme::ALL, value).unwrap_or(self.theme),
                "speed" => self.speed = value.parse().unwrap_or(self.speed),
                "window" => {
                    // w h [x y] [max]
                    let v: Vec<&str> = value.split_whitespace().collect();
                    let f = |i: usize| v.get(i).and_then(|t| t.parse::<f32>().ok());
                    if let (Some(w), Some(h)) = (f(0), f(1)) {
                        let pos = match (f(2), f(3)) {
                            (Some(x), Some(y)) => Some([x, y]),
                            _ => None,
                        };
                        self.window = Some(WindowState { size: [w, h], pos, maximized: v.contains(&"max") });
                    }
                }
                "console_side" => self.console_side = by_name(&ConsoleSide::ALL, value).unwrap_or(self.console_side),
                "line_ending" => self.line_ending = by_name(&LineEnding::ALL, value).unwrap_or(self.line_ending),
                "recent" if self.recent.len() < MAX_RECENT => self.recent.push(PathBuf::from(value.trim())),
                "device" => match DeviceSpec::parse(value.trim()) {
                    Ok(d) => self.devices.push(d),
                    Err(e) => log::warn!("settings: device {}: {e}", value.trim()),
                },
                "pico_bootrom" => self.pico_bootrom = Some(PathBuf::from(value.trim())),
                "python" => self.python = value.trim().to_owned(),
                "python_program" => self.python_program = Some(PathBuf::from(value.trim())),
                _ => {}
            }
        }
    }

    /// Puts `path` at the top of the recently opened files.
    pub fn add_recent(&mut self, path: &std::path::Path) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        self.recent.retain(|p| *p != path);
        self.recent.insert(0, path);
        self.recent.truncate(MAX_RECENT);
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        let mut text = format!(
            "# ITS-Board emulator (GUI)\nusb = {}\nauto_reload = {}\nconsole = {}\nconsole_side = {}\n\
             line_ending = {}\necho = {}\ntimestamps = {}\ntheme = {}\nspeed = {}\nbounce = {}\n",
            self.usb,
            self.auto_reload,
            self.console,
            name(&ConsoleSide::ALL, self.console_side),
            name(&LineEnding::ALL, self.line_ending),
            self.echo,
            self.timestamps,
            name(&Theme::ALL, self.theme),
            self.speed,
            self.bounce,
        );
        if let Some(w) = &self.window {
            text += &format!("window = {:.0} {:.0}", w.size[0], w.size[1]);
            if let Some([x, y]) = w.pos {
                text += &format!(" {x:.0} {y:.0}");
            }
            if w.maximized {
                text += " max";
            }
            text += "\n";
        }
        for p in &self.recent {
            text += &format!("recent = {}\n", p.display());
        }
        for d in &self.devices {
            text += &format!("device = {d}\n");
        }
        if let Some(p) = &self.pico_bootrom {
            text += &format!("pico_bootrom = {}\n", p.display());
        }
        text += &format!("python = {}\n", self.python);
        if let Some(p) = &self.python_program {
            text += &format!("python_program = {}\n", p.display());
        }
        let result = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, text));
        if let Err(e) = result {
            log::warn!("cannot save settings to {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_switch() {
        let mut s = Settings { usb: true, ..Settings::default() };
        s.parse(
            "# comment\nusb = false\nother = 3\nauto_reload = no\nrecent = /a/b c.axf\nrecent = /d.axf\n\
             console_side = right\nline_ending = crlf\nline_ending = bogus\necho = on\n",
        );
        assert!(!s.usb);
        assert!(!s.auto_reload);
        assert_eq!(s.console_side, ConsoleSide::Right);
        assert_eq!(s.line_ending, LineEnding::CrLf);
        assert!(s.echo && s.console);
        assert_eq!(s.recent, [PathBuf::from("/a/b c.axf"), PathBuf::from("/d.axf")]);
        s.add_recent(std::path::Path::new("/d.axf"));
        assert_eq!(s.recent, [PathBuf::from("/d.axf"), PathBuf::from("/a/b c.axf")]);
        s.parse("usb=on");
        assert!(s.usb);
        s.parse("theme = light\nspeed = 10\nbounce = off\nwindow = 1200 900 10 20 max\n");
        assert_eq!(s.theme, Theme::Light);
        assert_eq!(s.speed, 10.0);
        assert!(!s.bounce);
        assert_eq!(s.window, Some(WindowState { size: [1200.0, 900.0], pos: Some([10.0, 20.0]), maximized: true }));
    }

    #[test]
    fn parses_devices() {
        let mut s = Settings::default();
        s.parse("device = encoder@PC6,PC7\ndevice = pico=/x/gen 2.uf2@GP0=PF0,GP5=PF1\ndevice = lamp@PA1\n");
        let names: Vec<String> = s.devices.iter().map(ToString::to_string).collect();
        assert_eq!(names, ["encoder@PC6,PC7", "pico=/x/gen 2.uf2@GP0=PF0,GP5=PF1"]);
    }
}
