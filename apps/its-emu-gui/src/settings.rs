//! Settings the GUI remembers between runs: `key = value` lines in
//! `$XDG_CONFIG_HOME/its-emu/gui.conf` (`~/.config/...`; on Windows/macOS
//! the platform's config directory).

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// The ST-LINK is plugged in (USB/IP) at start.
    pub usb: bool,
}

impl Default for Settings {
    fn default() -> Self {
        // Like the real board: the probe is there when the emulator runs.
        Self { usb: cfg!(target_os = "linux") }
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
            if key.trim() == "usb" {
                self.usb = on;
            }
        }
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        let text = format!("# ITS-Board emulator (GUI)\nusb = {}\n", self.usb);
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
        let mut s = Settings { usb: true };
        s.parse("# comment\nusb = false\nother = 3\n");
        assert!(!s.usb);
        s.parse("usb=on");
        assert!(s.usb);
    }
}
