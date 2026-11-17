//! The virtual COM port as a pseudo-terminal (`/dev/ttys…` on macOS,
//! `/dev/pts/…` on Linux), where it cannot be a USB CDC-ACM device. Terminal
//! programs open the path like `/dev/ttyACM0` of the USB probe.
//!
//! The emulator keeps only the controlling side open: while no program has
//! the terminal open, writes fail and the output stays in the USART's
//! mirror buffer, as with the USB port nobody reads. Each open starts with
//! the system's default terminal settings (echo, line editing), like
//! `/dev/ttyACM0`; terminal programs set raw mode themselves.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use emu_core::Machine;
use usbip::{Reply, UsbDevice};

use crate::{EP_VCP_DATA, Stlink};

/// Output of the MCU not yet written to the terminal.
const OUT_LIMIT: usize = 4096;

pub(crate) struct Pty {
    master: File,
    path: String,
    from_host: Receiver<Vec<u8>>,
    /// Bytes from the host the USART did not take yet.
    to_mcu: Vec<u8>,
    to_host: Vec<u8>,
    stop: Arc<AtomicBool>,
}

impl Pty {
    pub fn open(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        // SAFETY: plain libc calls on the descriptor they return; errors checked
        let (master, path) = unsafe {
            let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let master = OwnedFd::from_raw_fd(fd);
            if libc::grantpt(fd) != 0 || libc::unlockpt(fd) != 0 {
                return Err(io::Error::last_os_error());
            }
            let name = libc::ptsname(fd);
            if name.is_null() {
                return Err(io::Error::last_os_error());
            }
            let path = std::ffi::CStr::from_ptr(name).to_string_lossy().into_owned();
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(io::Error::last_os_error());
            }
            (File::from(master), path)
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, from_host) = channel();
        let mut reader = master.try_clone()?;
        let stop_reader = stop.clone();
        std::thread::Builder::new().name("stlink-vcp".into()).spawn(move || {
            let mut buf = [0u8; 512];
            while !stop_reader.load(Ordering::Relaxed) {
                let mut pfd = libc::pollfd { fd: reader.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                // SAFETY: one valid pollfd
                if unsafe { libc::poll(&mut pfd, 1, 200) } <= 0 {
                    continue;
                }
                match reader.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            return;
                        }
                        wake();
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    // no terminal open (EOF / EIO): poll reports that at once
                    _ => std::thread::sleep(Duration::from_millis(100)),
                }
            }
        })?;
        Ok(Self { master, path, from_host, to_mcu: Vec::new(), to_host: Vec::new(), stop })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Copies between the terminal and the VCP endpoints; true if anything moved.
    pub fn poll(&mut self, dev: &mut Stlink, m: &mut Machine) -> bool {
        let mut progress = false;
        while let Ok(data) = self.from_host.try_recv() {
            self.to_mcu.extend(data);
        }
        if !self.to_mcu.is_empty() && dev.out(m, EP_VCP_DATA, &self.to_mcu) != Reply::Nak {
            self.to_mcu.clear();
            progress = true;
        }
        while self.to_host.len() < OUT_LIMIT {
            match dev.input(m, EP_VCP_DATA, OUT_LIMIT - self.to_host.len()) {
                Reply::Done(data) if !data.is_empty() => self.to_host.extend(data),
                _ => break,
            }
        }
        while !self.to_host.is_empty() {
            match self.master.write(&self.to_host) {
                Ok(n) => {
                    self.to_host.drain(..n);
                    progress = true;
                }
                // the terminal has not read the last output yet (or none is open yet)
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                // the terminal was closed: its output is gone, like on the USB port
                Err(_) => self.to_host.clear(),
            }
        }
        progress
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_pass_both_ways() {
        let (bus, mcu) = stm32f429::build();
        let mut m = stm32f429::machine(bus, &mcu).unwrap();
        let mut dev = Stlink::new(Some(mcu.usart3.clone()));
        dev.plug(true);
        let mut pty = Pty::open(Arc::new(|| {})).unwrap();
        // output while no terminal is open stays in the USART's buffer
        mcu.usart3.borrow_mut().mirror.as_mut().unwrap().extend_from_slice(b"early ");
        pty.poll(&mut dev, &mut m);

        let mut term = std::fs::OpenOptions::new().read(true).write(true).open(pty.path()).unwrap();
        raw_mode(&term);
        mcu.usart3.borrow_mut().mirror.as_mut().unwrap().extend_from_slice(b"hello");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        set_nonblocking(&term);
        while !got.ends_with(b"hello") {
            pty.poll(&mut dev, &mut m);
            let mut b = [0u8; 64];
            if let Ok(n) = term.read(&mut b) {
                got.extend_from_slice(&b[..n]);
            }
            assert!(std::time::Instant::now() < deadline, "got {got:?}");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(got == b"early hello" || got == b"hello", "{got:?}");

        term.write_all(b"in\r").unwrap();
        while mcu.usart3.borrow().rx.len() < 3 {
            pty.poll(&mut dev, &mut m);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let rx: Vec<u8> = mcu.usart3.borrow().rx.iter().copied().collect();
        assert_eq!(rx, b"in\r");
    }

    /// What terminal programs do: no echo, no line editing.
    fn raw_mode(f: &File) {
        // SAFETY: termios filled by tcgetattr on an open terminal
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(f.as_raw_fd(), &mut t), 0);
            libc::cfmakeraw(&mut t);
            assert_eq!(libc::tcsetattr(f.as_raw_fd(), libc::TCSANOW, &t), 0);
        }
    }

    fn set_nonblocking(f: &File) {
        // SAFETY: flags of an open descriptor
        unsafe {
            let flags = libc::fcntl(f.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(f.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}
