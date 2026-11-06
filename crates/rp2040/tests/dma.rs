//! DMA: memory-to-memory copies with chaining and byte swapping, and a PIO
//! state machine fed by a channel its TX FIFO paces.

use rp2040::testfw::{dma_uf2, stub_bootrom};
use rp2040::{Pico, PinEdge};

fn words(pico: &Pico, addr: u32, n: usize) -> Vec<u32> {
    let mut buf = vec![0u8; 4 * n];
    assert!(pico.machine().read_mem(addr, &mut buf));
    buf.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
}

#[test]
fn copies_chain_and_feed_a_pio() {
    let mut pico = Pico::new(&stub_bootrom(), &dma_uf2()).unwrap();
    pico.run_until_ps(5_000_000_000).expect("runs for 5 ms");
    let table = [0x1122_3344, 0x5566_7788, 0x99AA_BBCC, 0xDDEE_FF00];
    assert_eq!(words(&pico, 0x2000_0100, 4), table);
    let swapped: Vec<u32> = table.iter().map(|w| ((w & 0x00FF_00FF) << 8) | ((w >> 8) & 0x00FF_00FF)).collect();
    assert_eq!(words(&pico, 0x2000_0200, 4), swapped, "halfwords with BSWAP");
    assert_eq!(words(&pico, 0x2000_0000, 2), [0, 2], "both waits ended");

    // 256 bits of 1010...: an edge every 10 cycles, no gap where the FIFO
    // was refilled
    let edges: Vec<PinEdge> = pico.take_edges().into_iter().filter(|e| e.pin == 0).collect();
    let toggles: Vec<u64> = edges.iter().filter(|e| e.level.is_some()).map(|e| e.ps).collect();
    assert!(toggles.len() >= 256, "{} edges", toggles.len());
    let last = &toggles[toggles.len() - 256..];
    // (10 cycles of the ring oscillator are not a whole number of ps)
    let period = (last[255] - last[0]) / 255;
    for pair in last.windows(2) {
        assert!((pair[1] - pair[0]).abs_diff(period) <= 1, "uneven bit at {} ps", pair[1]);
    }
    assert_eq!(edges.last().unwrap().level, Some(false), "the last bit is 0, then the SM waits");
}
