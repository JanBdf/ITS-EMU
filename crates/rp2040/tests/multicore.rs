//! Both cores run and wake each other: core 0 launches core 1 through the
//! boot ROM's FIFO protocol, then they exchange counts with SEV and WFE.
//! Needs no third-party files (the ROM is a stand-in with the launch only).

use rp2040::Pico;
use rp2040::testfw::{handshake_uf2, launch_bootrom};

/// The state after 50 ms: both cores' cycles and PCs, the last answer.
fn run(idle_skipping: bool) -> ([u64; 2], [u32; 2], u32) {
    let mut pico = Pico::new(&launch_bootrom(), &handshake_uf2()).unwrap();
    pico.set_idle_skipping(idle_skipping);
    // in steps, as the board runs it
    for ms in 1..=50 {
        pico.run_until_ps(ms * 1_000_000_000).expect("both cores run");
    }
    let answer = pico.machine().read_u32(0x2000_0000).unwrap();
    ([0, 1].map(|n| pico.core(n).clock().cycles()), [0, 1].map(|n| pico.pc(n)), answer)
}

#[test]
fn the_cores_wake_each_other_at_the_same_cycles_however_they_are_run() {
    let (cycles, pcs, answer) = run(true);
    assert!(pcs[1] >= 0x1000_0000, "core 1 was launched: {:#x}", pcs[1]);
    // Pinned: every exchange takes the same cycles, whether blocks account
    // for themselves or through the block hook, so 50 ms are as many.
    assert_eq!(answer, 313, "the count core 1 answered last, plus 3");
    assert_eq!(run(false), (cycles, pcs, answer), "the same without idle skipping");
}
