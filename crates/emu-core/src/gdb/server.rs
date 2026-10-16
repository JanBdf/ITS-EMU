//! Non-blocking GDB server: the host polls it between emulation slices, so
//! the machine stays on the host's thread.
//!
//! Several clients may be connected at the same time. The first one controls
//! execution (halt, step, continue, breakpoints). Further clients — e.g. the
//! "auxiliary GDB" that the VS Code CDT adapter uses for live watch — can read
//! and write memory and registers while the target runs; their resume
//! requests are ignored.

use std::io::{self, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

use gdbstub::common::Signal;
use gdbstub::conn::Connection;
use gdbstub::stub::state_machine::GdbStubStateMachine;
use gdbstub::stub::{GdbStub, SingleThreadStopReason};

use super::{Resume, stop_reason};
use crate::machine::{Machine, StopReason};

/// What the host should do with the machine after [`GdbServer::poll`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Execution {
    /// No debugger controls the target: run normally.
    Free,
    /// The debugger halted the target: do not run it.
    Halted,
    /// The debugger continued the target: run it and report stops with
    /// [`GdbServer::report_stop`].
    Running,
}

/// Socket in non-blocking mode; writes are buffered until `flush`.
struct Conn {
    stream: TcpStream,
    out: Vec<u8>,
}

impl Connection for Conn {
    type Error = io::Error;

    fn write(&mut self, byte: u8) -> io::Result<()> {
        self.out.push(byte);
        Ok(())
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.out.extend_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut data = &self.out[..];
        while !data.is_empty() {
            match Write::write(&mut self.stream, data) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(n) => data = &data[n..],
                Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(1)),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        self.out.clear();
        Ok(())
    }
}

type StateMachine = GdbStubStateMachine<'static, Machine, Conn>;

struct Session {
    /// `None` only transiently while a state transition is in progress.
    sm: Option<StateMachine>,
    stream: TcpStream,
    peer: SocketAddr,
    controller: bool,
}

pub struct GdbServer {
    listener: TcpListener,
    sessions: Vec<Session>,
}

impl GdbServer {
    /// Listens on `addr` (e.g. `127.0.0.1:3333`).
    pub fn bind(addr: impl ToSocketAddrs) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, sessions: Vec::new() })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// A client controls execution.
    pub fn has_controller(&self) -> bool {
        self.sessions.iter().any(|s| s.controller)
    }

    /// Accepts new clients and processes all pending packets.
    pub fn poll(&mut self, m: &mut Machine) -> Execution {
        self.accept(m);
        let mut i = 0;
        while i < self.sessions.len() {
            if self.pump(i, m) {
                i += 1;
            } else {
                let s = self.sessions.remove(i);
                log::info!("gdb: {} disconnected", s.peer);
                if s.controller {
                    // Do not leave the target stuck at breakpoints nobody sees.
                    m.clear_debug_points();
                }
            }
        }
        // Remaining observers stay observers; the next new client takes control.
        self.execution()
    }

    fn execution(&self) -> Execution {
        match self.sessions.iter().find(|s| s.controller).and_then(|s| s.sm.as_ref()) {
            None => Execution::Free,
            Some(GdbStubStateMachine::Running(_)) => Execution::Running,
            Some(_) => Execution::Halted,
        }
    }

    /// The machine stopped while [`Execution::Running`]. `Budget` is ignored.
    pub fn report_stop(&mut self, m: &mut Machine, reason: &StopReason) {
        let Some(reason) = stop_reason(reason) else { return };
        if let Some(i) = self.sessions.iter().position(|s| s.controller) {
            self.report(i, m, reason);
        }
    }

    fn report(&mut self, i: usize, m: &mut Machine, reason: SingleThreadStopReason<u32>) {
        let s = &mut self.sessions[i];
        s.sm = match s.sm.take() {
            Some(GdbStubStateMachine::Running(r)) => r.report_stop(m, reason).ok(),
            other => other,
        };
    }

    fn accept(&mut self, m: &mut Machine) {
        loop {
            let (stream, peer) = match self.listener.accept() {
                Ok(c) => c,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return,
                Err(e) => {
                    log::warn!("gdb: accept failed: {e}");
                    return;
                }
            };
            let setup = || -> io::Result<(StateMachine, TcpStream)> {
                stream.set_nodelay(true)?;
                stream.set_nonblocking(true)?;
                let conn = Conn { stream: stream.try_clone()?, out: Vec::new() };
                let sm = GdbStub::new(conn).run_state_machine(m).map_err(|e| io::Error::other(e.to_string()))?;
                Ok((sm, stream))
            };
            match setup() {
                Ok((sm, stream)) => {
                    let controller = !self.has_controller();
                    log::info!("gdb: {peer} connected{}", if controller { "" } else { " (observer)" });
                    self.sessions.push(Session { sm: Some(sm), stream, peer, controller });
                }
                Err(e) => log::warn!("gdb: cannot set up session for {peer}: {e}"),
            }
        }
    }

    /// Feeds all available bytes of session `i`. Returns false if it ended.
    fn pump(&mut self, i: usize, m: &mut Machine) -> bool {
        let mut buf = [0u8; 4096];
        loop {
            let n = match self.sessions[i].stream.read(&mut buf) {
                Ok(0) => {
                    log::debug!("gdb: {}: connection closed", self.sessions[i].peer);
                    return false;
                }
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return true,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    log::debug!("gdb: {}: {e}", self.sessions[i].peer);
                    return false;
                }
            };
            for &byte in &buf[..n] {
                if !self.feed(i, m, byte) {
                    return false;
                }
            }
        }
    }

    /// Passes one byte to session `i` and carries out what GDB requested.
    fn feed(&mut self, i: usize, m: &mut Machine, byte: u8) -> bool {
        let controller = self.sessions[i].controller;
        let s = &mut self.sessions[i];
        let result = match s.sm.take() {
            Some(GdbStubStateMachine::Idle(sm)) => sm.incoming_data(m, byte),
            Some(GdbStubStateMachine::Running(sm)) => sm.incoming_data(m, byte),
            other => {
                s.sm = other;
                return true;
            }
        };
        s.sm = match result {
            Ok(sm) => Some(sm),
            Err(e) => {
                log::warn!("gdb: {}: {e}", s.peer);
                return false;
            }
        };

        // Ctrl-C: the target stops right away (between slices). For an
        // observer only its own view of the target stops.
        s.sm = match s.sm.take() {
            Some(GdbStubStateMachine::CtrlCInterrupt(sm)) => {
                sm.interrupt_handled(m, Some(SingleThreadStopReason::Signal(Signal::SIGINT))).ok()
            }
            other => other,
        };
        if let Some(GdbStubStateMachine::Disconnected(d)) = &s.sm {
            log::debug!("gdb: {}: {:?}", s.peer, d.get_reason());
            return false;
        }
        if s.sm.is_none() {
            log::debug!("gdb: {}: state machine lost", s.peer);
            return false;
        }

        match m.gdb_resume.take() {
            Some(Resume::Step) if controller => {
                let reason = m.step();
                if let Some(r) = stop_reason(&reason) {
                    self.report(i, m, r);
                }
            }
            Some(Resume::Step) => {
                // An observer cannot move the target; pretend the step is done.
                self.report(i, m, SingleThreadStopReason::DoneStep);
            }
            // Continue: the host runs the machine (see `execution`).
            Some(Resume::Continue) | None => {}
        }
        true
    }
}
