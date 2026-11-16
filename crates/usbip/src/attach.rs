//! Plugging an exported device into this Linux host (`usbip attach`, the
//! kernel's vhci-hcd driver).

use std::net::SocketAddr;
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};

use crate::DEFAULT_PORT;

/// Helper installed by the package (`packaging/arch`): attaches a device on
/// 127.0.0.1 through polkit, so that the GUI needs no terminal for sudo.
pub const ATTACH_HELPER: &str = "/usr/lib/its-emu/usbip-attach";

/// `usbip attach` for device `busid` of a server listening on `addr`.
pub fn attach_command(addr: SocketAddr, busid: &str) -> String {
    let host = if addr.ip().is_unspecified() { "127.0.0.1".to_owned() } else { addr.ip().to_string() };
    let port = if addr.port() == DEFAULT_PORT { String::new() } else { format!(" --tcp-port {}", addr.port()) };
    format!("sudo usbip{port} attach -r {host} -b {busid}")
}

fn vhci_loaded() -> bool {
    Path::new("/sys/devices/platform/vhci_hcd.0").exists()
}

/// The running kernel's modules are gone (a kernel update without a restart
/// since): modules not loaded yet cannot be.
fn modules_gone() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .is_ok_and(|release| !Path::new("/usr/lib/modules").join(release.trim()).exists())
}

/// The vhci-hcd module is not loaded and cannot be before a restart.
pub fn restart_needed() -> bool {
    !vhci_loaded() && modules_gone()
}

/// Serial ports of attached devices (`/dev/ttyACM*`) cannot appear before a
/// restart: the cdc_acm driver is not loaded and its module is gone.
pub fn serial_needs_restart() -> bool {
    !Path::new("/sys/bus/usb/drivers/cdc_acm").exists() && modules_gone()
}

/// The packaged helper is installed and can attach `busid`: helpers of
/// packages before the CN13 and Pico exports attach bus id 1-1 whatever they
/// are given.
fn helper_takes(busid: &str) -> bool {
    match std::fs::read_to_string(ATTACH_HELPER) {
        Ok(script) => busid == "1-1" || script.contains("busid"),
        Err(_) => false,
    }
}

/// Plugs device `busid` of the server on `addr` into this Linux host, in
/// the background: with the packaged helper through `pkexec` if it is
/// installed (local address), otherwise with `sudo -n usbip attach`. Needs
/// the `vhci-hcd` kernel module. The outcome arrives on the returned channel
/// (and is logged); `what` names the device in the log.
pub fn spawn_attach(addr: SocketAddr, busid: &str, what: &str) -> Receiver<Result<(), String>> {
    let (tx, rx) = channel();
    let local = addr.ip().is_loopback() || addr.ip().is_unspecified();
    let argv: Vec<String> = if local && helper_takes(busid) {
        vec!["pkexec".into(), ATTACH_HELPER.into(), addr.port().to_string(), busid.to_owned()]
    } else {
        let cmd = attach_command(addr, busid).replacen("sudo ", "sudo -n ", 1);
        cmd.split_whitespace().map(String::from).collect()
    };
    let what = what.to_owned();
    std::thread::spawn(move || {
        let shown = argv.join(" ");
        let result = match std::process::Command::new(&argv[0]).args(&argv[1..]).output() {
            Ok(out) if out.status.success() => {
                // usbip reports the port it uses; the helper warns about cdc_acm
                let err = String::from_utf8_lossy(&out.stderr);
                let warnings: Vec<&str> = err.lines().filter(|l| l.contains("warning")).collect();
                if !warnings.is_empty() {
                    log::warn!("{what}: {}", warnings.join("; "));
                } else if serial_needs_restart() {
                    log::warn!(
                        "{what}: the kernel was updated since the last start: its serial port \
                         (/dev/ttyACM*) appears only after a restart (cdc_acm kernel module)"
                    );
                }
                Ok(())
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
                let hint = if vhci_loaded() {
                    ""
                } else if restart_needed() {
                    " — the kernel was updated since the last start: restart the computer to load the \
                     vhci-hcd kernel module"
                } else {
                    " — the vhci-hcd kernel module is not loaded (sudo modprobe vhci-hcd)"
                };
                Err(format!("{err}{hint}"))
            }
            Err(e) => Err(format!("cannot run {}: {e}", argv[0])),
        };
        match &result {
            Ok(()) => log::info!("attached the {what}: {shown}"),
            Err(e) => log::warn!("`{shown}` failed: {e}"),
        }
        let _ = tx.send(result);
    });
    rx
}
