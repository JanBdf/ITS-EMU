//! The course's lwIP demo (Programs/Stack: static 192.168.33.99) on the
//! emulated Ethernet: it answers ARP and ping, with the checksums the MAC
//! inserts (lwIP leaves them to the hardware).

mod common;
use common::Sim;

const HOST_MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 1];
const HOST_IP: [u8; 4] = [192, 168, 33, 1];
const BOARD_IP: [u8; 4] = [192, 168, 33, 99];

fn arp_request() -> Vec<u8> {
    let mut f = vec![0xFF; 6];
    f.extend_from_slice(&HOST_MAC);
    f.extend_from_slice(&[0x08, 0x06, 0, 1, 0x08, 0x00, 6, 4, 0, 1]);
    f.extend_from_slice(&HOST_MAC);
    f.extend_from_slice(&HOST_IP);
    f.extend_from_slice(&[0; 6]);
    f.extend_from_slice(&BOARD_IP);
    f
}

fn echo_request(board_mac: [u8; 6], seq: u16) -> Vec<u8> {
    let mut f = board_mac.to_vec();
    f.extend_from_slice(&HOST_MAC);
    f.extend_from_slice(&[0x08, 0x00]);
    let payload = b"its-emu ping";
    let mut icmp = vec![8, 0, 0, 0, 0x12, 0x34];
    icmp.extend_from_slice(&seq.to_be_bytes());
    icmp.extend_from_slice(payload);
    let sum = stm32f429::eth::checksum(&icmp, 0);
    icmp[2..4].copy_from_slice(&sum.to_be_bytes());
    let mut ip = vec![0x45, 0];
    ip.extend_from_slice(&((20 + icmp.len()) as u16).to_be_bytes());
    ip.extend_from_slice(&[0, 1, 0, 0, 64, 1, 0, 0]);
    ip.extend_from_slice(&HOST_IP);
    ip.extend_from_slice(&BOARD_IP);
    let sum = stm32f429::eth::checksum(&ip, 0);
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    f.extend_from_slice(&ip);
    f.extend_from_slice(&icmp);
    f
}

#[test]
fn stack_demo_answers_arp_and_ping() {
    let Some(mut sim) = Sim::boot("Stack.axf") else { return };
    let link = sim.board.mcu().eth_link.clone();
    // the HAL waits 2 x 4.1 s for the PHY (PHY_RESET_DELAY, PHY_CONFIG_DELAY)
    sim.run_until_ms(10_000);
    let startup: Vec<_> = link.borrow_mut().sent.drain(..).collect();
    assert!(startup.is_empty(), "static address: nothing to say at startup");

    link.borrow_mut().to_mac.push_back(arp_request());
    sim.run_until_ms(10_100);
    let sent: Vec<_> = link.borrow_mut().sent.drain(..).collect();
    let reply = sent.iter().find(|f| f[12..14] == [0x08, 0x06] && f[20..22] == [0, 2]).expect("an ARP reply");
    assert_eq!(&reply[..6], &HOST_MAC);
    assert_eq!(&reply[28..32], &BOARD_IP);
    let board_mac: [u8; 6] = reply[22..28].try_into().unwrap();

    for seq in 1..=3 {
        link.borrow_mut().to_mac.push_back(echo_request(board_mac, seq));
        sim.run_until_ms(10_100 + 100 * u64::from(seq));
        let sent: Vec<_> = link.borrow_mut().sent.drain(..).collect();
        let reply = sent.iter().find(|f| f.len() > 34 && f[23] == 1 && f[34] == 0).expect("an echo reply");
        assert_eq!(&reply[30..34], &HOST_IP);
        assert_eq!(stm32f429::eth::checksum(&reply[14..34], 0), 0, "IP header checksum");
        let total = usize::from(u16::from_be_bytes([reply[16], reply[17]]));
        assert_eq!(stm32f429::eth::checksum(&reply[34..14 + total], 0), 0, "ICMP checksum");
        assert_eq!(&reply[40..42], &seq.to_be_bytes());
        assert!(reply.ends_with(b"its-emu ping") || reply[42..14 + total].ends_with(b"its-emu ping"));
    }
}
