//! The tutorial of the first start: a program
//! (tests/fixtures/ac6-src/Tutorial) that leads through the inputs of the
//! board on its display, one step after another.

use std::path::PathBuf;

/// The tutorial program, as built by tools/build-own.sh.
const FIRMWARE: &[u8] = include_bytes!("../assets/tutorial.axf");
const FIRMWARE_NAME: &str = "Tutorial.axf";

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
    fn firmware_is_an_elf_file() {
        assert_eq!(&FIRMWARE[..4], b"\x7fELF");
    }
}
