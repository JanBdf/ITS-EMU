//! Settings the GUI remembers between runs: `key = value` lines in
//! `$XDG_CONFIG_HOME/its-emu/gui.conf` (`~/.config/...`; on Windows/macOS
//! the platform's config directory).

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// The ST-LINK is plugged in (USB/IP) at start.
    pub usb: bool,
    /// The firmware file is loaded again when a new build replaces it.
    pub auto_reload: bool,
    /// Firmware files opened last, the latest first.
    pub recent: Vec<PathBuf>,
}

const MAX_RECENT: usize = 8;

impl Default for Settings {
    fn default() -> Self {
        // Like the real board: the probe is there when the emulator runs.
        Self { usb: cfg!(target_os = "linux"), auto_reload: true, recent: Vec::new() }
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
            let on = matches!(value.trim(), "true" | "1" | "yes" | "on");
            match key.trim() {
                "usb" => self.usb = on,
                "auto_reload" => self.auto_reload = on,
                "recent" if self.recent.len() < MAX_RECENT => self.recent.push(PathBuf::from(value.trim())),
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
        let mut text = format!("# ITS-Board emulator (GUI)\nusb = {}\nauto_reload = {}\n", self.usb, self.auto_reload);
        for p in &self.recent {
            text += &format!("recent = {}\n", p.display());
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
        let mut s = Settings { usb: true, auto_reload: true, recent: Vec::new() };
        s.parse("# comment\nusb = false\nother = 3\nauto_reload = no\nrecent = /a/b c.axf\nrecent = /d.axf\n");
        assert!(!s.usb);
        assert!(!s.auto_reload);
        assert_eq!(s.recent, [PathBuf::from("/a/b c.axf"), PathBuf::from("/d.axf")]);
        s.add_recent(std::path::Path::new("/d.axf"));
        assert_eq!(s.recent, [PathBuf::from("/d.axf"), PathBuf::from("/a/b c.axf")]);
        s.parse("usb=on");
        assert!(s.usb);
    }
}
