//! Installs the pyOCD plugin (`pyocd/its_emu_pyocd.py`) into the pyOCD
//! copies the course's tools run: the frozen pyOCD of Arm's CMSIS Debugger
//! extension for VS Code (`tools/pyocd/_internal` of the extension) and the
//! ones the Arm Environment Manager keeps under `~/.vcpkg/artifacts`.
//!
//! The frozen pyOCD imports from `_internal` and finds plugins through the
//! package metadata there, so the plugin is a module plus a `.dist-info`
//! directory with its entry point (group `pyocd.probe`). An update of the
//! extension installs into a new directory, without the plugin: installing
//! again at every start of the emulator covers that.

use std::io;
use std::path::{Path, PathBuf};

/// The plugin module.
pub const PLUGIN: &str = include_str!("../pyocd/its_emu_pyocd.py");
/// Bumped when the plugin changes (part of the `.dist-info` name).
pub const PLUGIN_VERSION: u32 = 1;

const MODULE: &str = "its_emu_pyocd.py";
const DIST_PREFIX: &str = "its_emu_pyocd-";

/// Where pyOCD lives below the CMSIS Debugger extension.
const EXTENSION_PREFIX: &str = "arm.vscode-cmsis-debugger-";
/// VS Code and its relatives that install the same extensions.
const EDITOR_DIRS: [&str; 5] = [".vscode", ".vscode-insiders", ".vscode-oss", ".cursor", ".windsurf"];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// A directory with a frozen pyOCD's modules (`pyocd/probe/stlink/usb.py`).
fn is_pyocd_dir(dir: &Path) -> bool {
    dir.join("pyocd/probe/stlink/usb.py").is_file()
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    v.sort();
    v
}

/// The pyOCD module directories found under `home`.
pub fn targets_in(home: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for editor in EDITOR_DIRS {
        for ext in subdirs(&home.join(editor).join("extensions")) {
            let name = ext.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let dir = ext.join("tools/pyocd/_internal");
            if name.starts_with(EXTENSION_PREFIX) && is_pyocd_dir(&dir) {
                found.push(dir);
            }
        }
    }
    // ~/.vcpkg/artifacts/<registry>/<artifact>[/<version>]/_internal
    for registry in subdirs(&home.join(".vcpkg/artifacts")) {
        for artifact in subdirs(&registry) {
            if !artifact.to_string_lossy().contains("pyocd") {
                continue;
            }
            for dir in std::iter::once(artifact.clone()).chain(subdirs(&artifact)) {
                if is_pyocd_dir(&dir.join("_internal")) {
                    found.push(dir.join("_internal"));
                }
            }
        }
    }
    found
}

/// The pyOCD module directories of this user.
pub fn targets() -> Vec<PathBuf> {
    home().map_or_else(Vec::new, |h| targets_in(&h))
}

/// The plugin is installed (in its current version) in `dir`.
pub fn is_installed(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(MODULE)).is_ok_and(|s| s == PLUGIN)
        && dist_info(dir).join("entry_points.txt").is_file()
}

fn dist_info(dir: &Path) -> PathBuf {
    dir.join(format!("{DIST_PREFIX}{PLUGIN_VERSION}.dist-info"))
}

/// Installs the plugin into `dir` (a pyOCD module directory), replacing older versions.
pub fn install_into(dir: &Path) -> io::Result<()> {
    if is_installed(dir) {
        return Ok(());
    }
    remove_from(dir)?;
    let info = dist_info(dir);
    std::fs::create_dir_all(&info)?;
    std::fs::write(
        info.join("METADATA"),
        format!(
            "Metadata-Version: 2.1\nName: its-emu-pyocd\nVersion: {PLUGIN_VERSION}\n\
             Summary: ITS-Board Emulator ST-LINK for pyOCD (installed by its-emu-gui)\n"
        ),
    )?;
    std::fs::write(info.join("entry_points.txt"), "[pyocd.probe]\nits-emu = its_emu_pyocd:ITSEmuProbePlugin\n")?;
    std::fs::write(info.join("INSTALLER"), "its-emu\n")?;
    // the module last: is_installed checks it
    std::fs::write(dir.join(MODULE), PLUGIN)
}

/// Removes every version of the plugin from `dir`.
pub fn remove_from(dir: &Path) -> io::Result<()> {
    match std::fs::remove_file(dir.join(MODULE)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(DIST_PREFIX) && name.ends_with(".dist-info") {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    let cache = dir.join("__pycache__");
    for entry in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with("its_emu_pyocd.") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

/// What [`install`] or [`uninstall`] did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// pyOCD directories that have (or no longer have) the plugin.
    pub done: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

/// Installs the plugin into every pyOCD found.
pub fn install() -> Report {
    each(install_into, "installed into")
}

/// Removes the plugin from every pyOCD found.
pub fn uninstall() -> Report {
    each(remove_from, "removed from")
}

fn each(f: fn(&Path) -> io::Result<()>, what: &str) -> Report {
    let mut report = Report::default();
    for dir in targets() {
        match f(&dir) {
            Ok(()) => {
                log::info!("pyOCD plugin {what} {}", dir.display());
                report.done.push(dir);
            }
            Err(e) => {
                log::warn!("pyOCD plugin: {}: {e}", dir.display());
                report.failed.push((dir, e.to_string()));
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_installs_and_removes() {
        let home = std::env::temp_dir().join(format!("its-emu-pyocd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let ext = home.join(".vscode/extensions/arm.vscode-cmsis-debugger-1.10.1-darwin-arm64/tools/pyocd/_internal");
        let other = home.join(".vscode/extensions/someone.else-1.0/tools/pyocd/_internal");
        let vcpkg = home.join(".vcpkg/artifacts/2139c4c6/debuggers.pyocd.pyocd/_internal");
        for d in [&ext, &other, &vcpkg] {
            std::fs::create_dir_all(d.join("pyocd/probe/stlink")).unwrap();
            std::fs::write(d.join("pyocd/probe/stlink/usb.py"), "").unwrap();
        }
        assert_eq!(targets_in(&home), [ext.clone(), vcpkg.clone()]);

        // an older version is replaced
        std::fs::create_dir_all(ext.join("its_emu_pyocd-0.dist-info")).unwrap();
        install_into(&ext).unwrap();
        assert!(is_installed(&ext));
        assert!(!ext.join("its_emu_pyocd-0.dist-info").exists());
        let entry = std::fs::read_to_string(dist_info(&ext).join("entry_points.txt")).unwrap();
        assert!(entry.contains("its_emu_pyocd:ITSEmuProbePlugin"));
        install_into(&ext).unwrap();

        remove_from(&ext).unwrap();
        assert!(!is_installed(&ext));
        let left: Vec<_> = std::fs::read_dir(&ext).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, ["pyocd"]);
        std::fs::remove_dir_all(&home).unwrap();
    }
}
