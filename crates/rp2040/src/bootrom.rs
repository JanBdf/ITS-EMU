//! The RP2040's boot ROM (revision B2). Firmware calls into it (bit and
//! memory functions, flash access, the floating-point library), so the real
//! one is used. It is not part of this repository: its floating-point
//! library may only be used on RP2040 devices, so it is downloaded from
//! Raspberry Pi's release on first use and cached.

use std::path::{Path, PathBuf};

use emu_core::ElfImage;

pub const ROM_SIZE: usize = 16 * 1024;
pub const B2_URL: &str = "https://github.com/raspberrypi/pico-bootrom-rp2040/releases/download/b2/b2.elf";
/// FNV-1a 64 of the 16 KiB B2 image.
const B2_FNV: u64 = 0xD25F_1409_6EF3_1A9F;

#[derive(Debug, thiserror::Error)]
pub enum BootromError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0} is not the RP2040 boot ROM B2 (expected the 16 KiB image or b2.elf)")]
    NotB2(PathBuf),
    #[error(
        "the RP2040 boot ROM is not available and could not be downloaded ({0}); \
         download {B2_URL} and pass it with --pico-bootrom or put it at {1}"
    )]
    Download(String, PathBuf),
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xCBF2_9CE4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3))
}

/// The ROM image from a raw 16 KiB binary or the release's ELF file.
pub fn from_bytes(data: &[u8]) -> Option<Vec<u8>> {
    let image = if data.len() == ROM_SIZE {
        data.to_vec()
    } else {
        let elf = ElfImage::parse(data).ok()?;
        elf.segments.into_iter().find(|s| s.addr == 0 && s.data.len() == ROM_SIZE)?.data
    };
    (fnv1a(&image) == B2_FNV).then_some(image)
}

pub fn load(path: &Path) -> Result<Vec<u8>, BootromError> {
    let data = std::fs::read(path).map_err(|e| BootromError::Io(path.to_owned(), e))?;
    from_bytes(&data).ok_or_else(|| BootromError::NotB2(path.to_owned()))
}

/// Where the downloaded ROM is kept.
pub fn cache_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            if cfg!(target_os = "macos") {
                home.map(|h| h.join("Library/Caches"))
            } else if cfg!(windows) {
                std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
            } else {
                home.map(|h| h.join(".cache"))
            }
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("its-emu").join("rp2040-bootrom-b2.bin")
}

/// The ROM from `path`, else from the cache, else downloaded (with `curl`)
/// into the cache.
pub fn locate(path: Option<&Path>) -> Result<Vec<u8>, BootromError> {
    if let Some(p) = path {
        return load(p);
    }
    let cache = cache_path();
    if let Ok(rom) = load(&cache) {
        return Ok(rom);
    }
    log::info!("downloading the RP2040 boot ROM from {B2_URL}");
    let fail = |msg: String| BootromError::Download(msg, cache.clone());
    let dir = cache.parent().expect("cache path has a directory");
    std::fs::create_dir_all(dir).map_err(|e| fail(e.to_string()))?;
    let tmp = dir.join("b2.elf.part");
    let status = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "60", "-o"])
        .arg(&tmp)
        .arg(B2_URL)
        .status()
        .map_err(|e| fail(format!("curl: {e}")))?;
    if !status.success() {
        return Err(fail(format!("curl exited with {status}")));
    }
    let data = std::fs::read(&tmp).map_err(|e| fail(e.to_string()))?;
    let _ = std::fs::remove_file(&tmp);
    let rom = from_bytes(&data).ok_or_else(|| fail("the download is not the B2 boot ROM".to_owned()))?;
    std::fs::write(&cache, &rom).map_err(|e| fail(e.to_string()))?;
    Ok(rom)
}
