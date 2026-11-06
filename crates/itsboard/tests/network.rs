//! The virtual network behind the Ethernet jack with an lwIP program built
//! from the course's Stack demo: DHCP, and TCP/UDP echo servers on port 7
//! reached through ports of the computer.

mod common;
use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::time::Duration;

use common::Sim;
use itsboard::net::{Forward, NetConfig, Proto};

#[test]
fn dhcp_then_echo_over_tcp_and_udp_from_the_computer() {
    let Some(mut sim) = Sim::boot("NetEcho.axf") else { return };
    let forwards = vec![
        Forward { proto: Proto::Tcp, host_port: 0, board_port: 7 },
        Forward { proto: Proto::Udp, host_port: 0, board_port: 7 },
    ];
    let bound = sim.board.add_network(NetConfig { forwards, ..NetConfig::default() }).unwrap();
    // the HAL waits 2 x 4.1 s for the PHY, then DHCP
    sim.run_until_ms(12_000);
    let out = sim.take_serial();
    assert!(out.contains("N ip 192.168.33.99\n"), "{out}");

    // TCP: the emulator runs in steps while the test talks to it
    let mut stream = TcpStream::connect(bound[0].1).unwrap();
    stream.set_read_timeout(Some(Duration::from_millis(10))).unwrap();
    stream.write_all(b"hello board").unwrap();
    let mut echoed = Vec::new();
    let mut t = 12_000;
    while echoed.len() < 11 && t < 15_000 {
        t += 10;
        sim.run_until_ms(t);
        let mut buf = [0u8; 64];
        if let Ok(n) = stream.read(&mut buf) {
            echoed.extend_from_slice(&buf[..n]);
        }
    }
    assert_eq!(echoed, b"hello board");
    assert!(sim.take_serial().contains("N tcp 11\n"));
    drop(stream);

    // UDP
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(10))).unwrap();
    sock.send_to(b"datagram", bound[1].1).unwrap();
    let mut got = None;
    while got.is_none() && t < 18_000 {
        t += 10;
        sim.run_until_ms(t);
        let mut buf = [0u8; 64];
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            got = Some(buf[..n].to_vec());
        }
    }
    assert_eq!(got.as_deref(), Some(&b"datagram"[..]));
}
