//! A network behind the board's Ethernet jack: a gateway (smoltcp) that
//! answers ARP and ping, a DHCP server that hands out the board's address,
//! and TCP/UDP ports of the computer forwarded to the board (`--forward
//! tcp:8080=80`: connections to localhost:8080 reach port 80 of the board).
//!
//! It keeps the board's time: smoltcp's timers run on virtual time, and the
//! computer's sockets are looked at every millisecond of it while ports are
//! forwarded. The board cannot reach other hosts (no NAT).

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};

use emu_core::DeviceCtx;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::{tcp, udp};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use stm32f429::eth::{SharedEthLink, checksum};

use crate::devices::PinDevice;

/// The gateway's MAC address (locally administered).
const GATEWAY_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

/// A port of the computer (on localhost) forwarded to a port of the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Forward {
    pub proto: Proto,
    pub host_port: u16,
    pub board_port: u16,
}

impl Forward {
    /// `tcp:8080=80`, `udp:5000` (the same port on both sides).
    pub fn parse(s: &str) -> Result<Self, String> {
        let err = || format!("expected tcp:HOSTPORT[=BOARDPORT] or udp:..., got {s:?}");
        let (proto, ports) = s.trim().split_once(':').ok_or_else(err)?;
        let proto = match proto.to_ascii_lowercase().as_str() {
            "tcp" => Proto::Tcp,
            "udp" => Proto::Udp,
            _ => return Err(err()),
        };
        let (host, board) = ports.split_once('=').unwrap_or((ports, ports));
        let port = |p: &str| p.trim().parse::<u16>().ok().filter(|&p| p > 0).ok_or_else(err);
        Ok(Self { proto, host_port: port(host)?, board_port: port(board)? })
    }
}

impl std::fmt::Display for Forward {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let proto = match self.proto {
            Proto::Tcp => "tcp",
            Proto::Udp => "udp",
        };
        write!(f, "{proto}:{}", self.host_port)?;
        if self.board_port != self.host_port {
            write!(f, "={}", self.board_port)?;
        }
        Ok(())
    }
}

/// Addresses on the virtual network.
#[derive(Debug, Clone)]
pub struct NetConfig {
    pub gateway: [u8; 4],
    pub board: [u8; 4],
    pub prefix: u8,
    pub forwards: Vec<Forward>,
}

impl Default for NetConfig {
    /// The course's Stack demo: board 192.168.33.99, gateway .1.
    fn default() -> Self {
        Self { gateway: [192, 168, 33, 1], board: [192, 168, 33, 99], prefix: 24, forwards: Vec::new() }
    }
}

/// Frames between smoltcp and the cable.
struct Wire<'a> {
    from_board: &'a mut VecDeque<Vec<u8>>,
    to_board: &'a mut VecDeque<Vec<u8>>,
}

struct Rx(Vec<u8>);
struct Tx<'a>(&'a mut VecDeque<Vec<u8>>);

impl RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = vec![0; len];
        let r = f(&mut frame);
        self.0.push_back(frame);
        r
    }
}

impl Device for Wire<'_> {
    type RxToken<'a>
        = Rx
    where
        Self: 'a;
    type TxToken<'a>
        = Tx<'a>
    where
        Self: 'a;

    fn receive(&mut self, _: Instant) -> Option<(Rx, Tx<'_>)> {
        let frame = self.from_board.pop_front()?;
        Some((Rx(frame), Tx(self.to_board)))
    }

    fn transmit(&mut self, _: Instant) -> Option<Tx<'_>> {
        Some(Tx(self.to_board))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps
    }
}

/// A forwarded TCP connection: the computer's end and the gateway's socket
/// to the board, with bytes the computer could not take yet.
struct TcpLink {
    stream: TcpStream,
    socket: SocketHandle,
    to_host: Vec<u8>,
    host_closed: bool,
}

/// A forwarded UDP port and where the last datagram from the computer came from.
struct UdpLink {
    host: UdpSocket,
    socket: SocketHandle,
    board_port: u16,
    peer: Option<SocketAddr>,
}

pub struct VirtualNet {
    link: SharedEthLink,
    config: NetConfig,
    iface: Interface,
    sockets: SocketSet<'static>,
    listeners: Vec<(TcpListener, u16)>,
    tcp: Vec<TcpLink>,
    udp: Vec<UdpLink>,
    next_port: u16,
    /// Frames from the board for smoltcp (DHCP is answered before).
    pending: VecDeque<Vec<u8>>,
    /// Cycle of the next look at the timers and the computer's sockets.
    next_poll: Option<u64>,
    /// The forwards with the addresses they listen on.
    bound: Vec<(Forward, SocketAddr)>,
}

impl VirtualNet {
    /// The network on `link`; binds the forwarded ports on localhost.
    pub fn new(link: SharedEthLink, config: NetConfig) -> std::io::Result<Self> {
        let mut pending = VecDeque::new();
        let mut out = VecDeque::new();
        let mut wire = Wire { from_board: &mut pending, to_board: &mut out };
        let hw = HardwareAddress::Ethernet(EthernetAddress(GATEWAY_MAC));
        let mut iface = Interface::new(Config::new(hw), &mut wire, Instant::ZERO);
        let [a, b, c, d] = config.gateway;
        iface.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::new(IpAddress::v4(a, b, c, d), config.prefix));
        });
        let mut sockets = SocketSet::new(Vec::new());
        let mut listeners = Vec::new();
        let mut udp_links = Vec::new();
        let mut bound = Vec::new();
        // the gateway's ports: UDP forwards first, then TCP connections
        let mut next_port = 49152;
        for f in &config.forwards {
            match f.proto {
                Proto::Tcp => {
                    let l = TcpListener::bind(("127.0.0.1", f.host_port))?;
                    l.set_nonblocking(true)?;
                    bound.push((*f, l.local_addr()?));
                    listeners.push((l, f.board_port));
                }
                Proto::Udp => {
                    let host = UdpSocket::bind(("127.0.0.1", f.host_port))?;
                    host.set_nonblocking(true)?;
                    bound.push((*f, host.local_addr()?));
                    let buf = || udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 16], vec![0; 16 * 1500]);
                    let mut s = udp::Socket::new(buf(), buf());
                    let _ = s.bind(next_port);
                    next_port += 1;
                    udp_links.push(UdpLink { host, socket: sockets.add(s), board_port: f.board_port, peer: None });
                }
            }
        }
        Ok(Self {
            link,
            config,
            iface,
            sockets,
            listeners,
            tcp: Vec::new(),
            udp: udp_links,
            next_port,
            pending,
            next_poll: None,
            bound,
        })
    }

    /// The forwarded ports and the addresses they listen on (port 0 in a
    /// [`Forward`]: one the system chose).
    pub fn bound(&self) -> &[(Forward, SocketAddr)] {
        &self.bound
    }

    fn board_ip(&self) -> IpAddress {
        let [a, b, c, d] = self.config.board;
        IpAddress::v4(a, b, c, d)
    }

    /// Frames the board sent: DHCP answered here, the rest to smoltcp.
    fn take_frames(&mut self) {
        let frames: Vec<_> = self.link.borrow_mut().sent.drain(..).collect();
        for frame in frames {
            match dhcp_reply(&frame, &self.config) {
                Some(reply) => self.link.borrow_mut().to_mac.push_back(reply),
                None => self.pending.push_back(frame),
            }
        }
    }

    /// The computer's side: new connections, data both ways, closes.
    fn pump_host(&mut self) {
        for (listener, board_port) in &self.listeners {
            while let Ok((stream, _)) = listener.accept() {
                if stream.set_nonblocking(true).is_err() {
                    continue;
                }
                let buf = || tcp::SocketBuffer::new(vec![0; 16 * 1024]);
                let mut s = tcp::Socket::new(buf(), buf());
                let local = self.next_port;
                self.next_port = if local == u16::MAX { 49152 + self.udp.len() as u16 } else { local + 1 };
                let [a, b, c, d] = self.config.board;
                if s.connect(self.iface.context(), (IpAddress::v4(a, b, c, d), *board_port), local).is_err() {
                    continue;
                }
                let socket = self.sockets.add(s);
                self.tcp.push(TcpLink { stream, socket, to_host: Vec::new(), host_closed: false });
            }
        }
        let mut buf = [0u8; 4096];
        self.tcp.retain_mut(|t| {
            let s = self.sockets.get_mut::<tcp::Socket>(t.socket);
            // computer -> board
            if !t.host_closed && s.may_send() {
                let room = s.send_capacity() - s.send_queue();
                if room > 0 {
                    let n = room.min(buf.len());
                    match t.stream.read(&mut buf[..n]) {
                        Ok(0) => {
                            t.host_closed = true;
                            s.close();
                        }
                        Ok(n) => {
                            let _ = s.send_slice(&buf[..n]);
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                        Err(_) => {
                            t.host_closed = true;
                            s.abort();
                        }
                    }
                }
            }
            // board -> computer
            while s.can_recv() {
                match s.recv_slice(&mut buf) {
                    Ok(n) if n > 0 => t.to_host.extend_from_slice(&buf[..n]),
                    _ => break,
                }
            }
            while !t.to_host.is_empty() {
                match t.stream.write(&t.to_host) {
                    Ok(n) if n > 0 => {
                        t.to_host.drain(..n);
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    _ => {
                        t.to_host.clear();
                        s.abort();
                    }
                }
            }
            // the board closed its side (not: still connecting)
            let connecting = matches!(s.state(), tcp::State::SynSent | tcp::State::SynReceived);
            let board_done = !s.is_active() || (!connecting && !s.may_recv() && t.to_host.is_empty());
            if board_done {
                let _ = t.stream.shutdown(std::net::Shutdown::Both);
                if !s.is_active() {
                    self.sockets.remove(t.socket);
                    return false;
                }
            }
            true
        });
        let board = self.board_ip();
        for u in &mut self.udp {
            let s = self.sockets.get_mut::<udp::Socket>(u.socket);
            while let Ok((n, peer)) = u.host.recv_from(&mut buf) {
                u.peer = Some(peer);
                let _ = s.send_slice(&buf[..n], (board, u.board_port));
            }
            while let Ok((n, _)) = s.recv_slice(&mut buf) {
                if let Some(peer) = u.peer {
                    let _ = u.host.send_to(&buf[..n], peer);
                }
            }
        }
    }

    fn poll(&mut self, ctx: &DeviceCtx) {
        let now = Instant::from_micros((ctx.clock.time_ps() / 1_000_000) as i64);
        self.take_frames();
        self.pump_host();
        let mut out = VecDeque::new();
        let mut wire = Wire { from_board: &mut self.pending, to_board: &mut out };
        self.iface.poll(now, &mut wire, &mut self.sockets);
        self.link.borrow_mut().to_mac.extend(out);
        // again: what the board's data made smoltcp take or send
        self.pump_host();
        let mut out = VecDeque::new();
        let mut wire = Wire { from_board: &mut self.pending, to_board: &mut out };
        self.iface.poll(now, &mut wire, &mut self.sockets);
        self.link.borrow_mut().to_mac.extend(out);

        // timers of smoltcp, and the computer's sockets every millisecond
        let mut delay = self.iface.poll_delay(now, &self.sockets).map(|d| d.total_micros());
        if !self.listeners.is_empty() || !self.udp.is_empty() || !self.tcp.is_empty() {
            delay = Some(delay.map_or(1_000, |d| d.min(1_000)));
        }
        self.next_poll =
            delay.map(|us| ctx.clock.cycle_at_ps(ctx.clock.time_ps() + us.max(1) * 1_000_000).max(ctx.now() + 1));
    }
}

impl PinDevice for VirtualNet {
    fn update(&mut self, ctx: &mut DeviceCtx) {
        let frames = !self.link.borrow().sent.is_empty();
        if frames || self.next_poll.is_some_and(|at| at <= ctx.now()) {
            self.poll(ctx);
        }
    }

    fn next_event(&self) -> Option<u64> {
        // frames from the board: at once
        if !self.link.borrow().sent.is_empty() {
            return Some(0);
        }
        self.next_poll
    }
}

/// The DHCP server's answer to a DISCOVER or REQUEST in `frame`: the
/// board's address for a lease of a day, the gateway as router and server.
fn dhcp_reply(frame: &[u8], config: &NetConfig) -> Option<Vec<u8>> {
    if frame.len() < 14 + 20 + 8 + 240 || frame[12..14] != [0x08, 0x00] {
        return None;
    }
    let ip = &frame[14..];
    let ihl = usize::from(ip[0] & 0xF) * 4;
    if ip[9] != 17 || ip.len() < ihl + 8 + 240 {
        return None;
    }
    let udp = &ip[ihl..];
    if udp[2..4] != 67u16.to_be_bytes() {
        return None;
    }
    let bootp = &udp[8..];
    if bootp[0] != 1 || bootp[236..240] != [99, 130, 83, 99] {
        return None;
    }
    // options: the message type
    let mut kind = 0;
    let mut opts = &bootp[240..];
    while let [code, rest @ ..] = opts {
        match *code {
            0 => opts = rest,
            255 => break,
            _ => {
                let [len, rest @ ..] = rest else { break };
                let len = usize::from(*len);
                if rest.len() < len {
                    break;
                }
                if *code == 53 && len == 1 {
                    kind = rest[0];
                }
                opts = &rest[len..];
            }
        }
    }
    let answer = match kind {
        1 => 2, // DISCOVER -> OFFER
        3 => 5, // REQUEST -> ACK
        _ => return None,
    };
    let mask = (!0u32 << (32 - u32::from(config.prefix))).to_be_bytes();
    let mut reply = vec![0u8; 240];
    reply[0] = 2;
    reply[1..3].copy_from_slice(&[1, 6]);
    reply[4..8].copy_from_slice(&bootp[4..8]); // xid
    reply[10..12].copy_from_slice(&bootp[10..12]); // flags
    reply[16..20].copy_from_slice(&config.board); // yiaddr
    reply[20..24].copy_from_slice(&config.gateway); // siaddr
    reply[28..44].copy_from_slice(&bootp[28..44]); // chaddr
    reply[236..240].copy_from_slice(&[99, 130, 83, 99]);
    reply.extend_from_slice(&[53, 1, answer, 54, 4]);
    reply.extend_from_slice(&config.gateway);
    reply.extend_from_slice(&[51, 4, 0, 1, 0x51, 0x80, 1, 4]); // a day
    reply.extend_from_slice(&mask);
    reply.extend_from_slice(&[3, 4]);
    reply.extend_from_slice(&config.gateway);
    reply.extend_from_slice(&[6, 4]);
    reply.extend_from_slice(&config.gateway);
    reply.push(255);

    let mut udp_out = Vec::with_capacity(8 + reply.len());
    udp_out.extend_from_slice(&67u16.to_be_bytes());
    udp_out.extend_from_slice(&68u16.to_be_bytes());
    udp_out.extend_from_slice(&((8 + reply.len()) as u16).to_be_bytes());
    udp_out.extend_from_slice(&[0, 0]); // no checksum
    udp_out.extend_from_slice(&reply);
    let mut ip_out = vec![0x45, 0];
    ip_out.extend_from_slice(&((20 + udp_out.len()) as u16).to_be_bytes());
    ip_out.extend_from_slice(&[0, 0, 0, 0, 64, 17, 0, 0]);
    ip_out.extend_from_slice(&config.gateway);
    ip_out.extend_from_slice(&[255; 4]);
    let sum = checksum(&ip_out, 0);
    ip_out[10..12].copy_from_slice(&sum.to_be_bytes());
    let mut out = vec![0xFF; 6];
    out.extend_from_slice(&GATEWAY_MAC);
    out.extend_from_slice(&[0x08, 0x00]);
    out.extend_from_slice(&ip_out);
    out.extend_from_slice(&udp_out);
    Some(out)
}

/// The board's address on the network (for messages).
pub fn describe(config: &NetConfig) -> String {
    let ip = |a: [u8; 4]| Ipv4Address::new(a[0], a[1], a[2], a[3]).to_string();
    format!("network: gateway {}/{}, board {} (DHCP)", ip(config.gateway), config.prefix, ip(config.board))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_specs() {
        assert_eq!(Forward::parse("tcp:8080=80"), Ok(Forward { proto: Proto::Tcp, host_port: 8080, board_port: 80 }));
        assert_eq!(Forward::parse("UDP:5000"), Ok(Forward { proto: Proto::Udp, host_port: 5000, board_port: 5000 }));
        assert!(Forward::parse("tcp:0").is_err());
        assert!(Forward::parse("icmp:1").is_err());
        for s in ["tcp:8080=80", "udp:5000"] {
            assert_eq!(Forward::parse(s).unwrap().to_string(), s);
        }
    }
}
