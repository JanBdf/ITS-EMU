//! Firmware for tests that need no third-party files: a stand-in for the
//! boot ROM and a program that blinks a GPIO, both small enough to read.

use crate::{VECTOR_TABLE, uf2};

/// A 16 KiB stand-in for the boot ROM: its reset handler parks core 1 in a
/// WFE loop (the real ROM waits there for core 0 to launch it). It has none
/// of the ROM's functions, so only firmware that does not call them runs.
pub fn stub_bootrom() -> Vec<u8> {
    let mut rom = vec![0u8; crate::bootrom::ROM_SIZE];
    rom[0..4].copy_from_slice(&0x2004_1000u32.to_le_bytes()); // SP
    rom[4..8].copy_from_slice(&0x0000_0009u32.to_le_bytes()); // reset handler at 0x8
    // 0x8: wfe; 0xa: b 0x8
    rom[8..12].copy_from_slice(&[0x20, 0xBF, 0xFD, 0xE7]);
    rom
}

/// Toggles GPIO 0 through SIO every 1000 turns of a delay loop, from the
/// vector table that boot stage 2 hands over to (assembled for ARMv6-M):
///
/// ```text
///     .word 0x20042000        @ SP
///     .word 0x10000109        @ reset
/// reset:
///     ldr  r0, =0x40014004    @ IO_BANK0 GPIO0_CTRL
///     movs r1, #5             @ FUNCSEL = SIO
///     str  r1, [r0]
///     ldr  r0, =0xD0000000    @ SIO
///     movs r1, #1
///     str  r1, [r0, #0x24]    @ GPIO_OE_SET
/// loop:
///     str  r1, [r0, #0x1C]    @ GPIO_XOR
///     ldr  r2, =1000
/// delay:
///     subs r2, #1
///     bne  delay
///     b    loop
/// ```
pub const BLINK: [u8; 44] = [
    0x00, 0x20, 0x04, 0x20, 0x09, 0x01, 0x00, 0x10, 0x05, 0x48, 0x05, 0x21, 0x01, 0x60, 0x05, 0x48, 0x01, 0x21, 0x41,
    0x62, 0xC1, 0x61, 0x04, 0x4A, 0x01, 0x3A, 0xFD, 0xD1, 0xFA, 0xE7, 0x00, 0x00, 0x04, 0x40, 0x01, 0x40, 0x00, 0x00,
    0x00, 0xD0, 0xE8, 0x03, 0x00, 0x00,
];

/// [`BLINK`] as a UF2 file.
pub fn blink_uf2() -> Vec<u8> {
    uf2::write(VECTOR_TABLE, &BLINK)
}

/// Puts PWM slice 0 channel A on GPIO 0 (period 1000 cycles of clk_sys,
/// high for 250), then idles:
///
/// ```text
///     .word 0x20042000        @ SP
///     .word 0x10000109        @ reset
/// reset:
///     ldr  r0, =0x40014004    @ IO_BANK0 GPIO0_CTRL
///     movs r1, #4             @ FUNCSEL = PWM
///     str  r1, [r0]
///     ldr  r0, =0x40050000    @ PWM slice 0
///     ldr  r1, =999
///     str  r1, [r0, #0x10]    @ TOP
///     movs r1, #250
///     str  r1, [r0, #0x0C]    @ CC: A = 250
///     movs r1, #1
///     str  r1, [r0, #0x00]    @ CSR: EN
/// loop:
///     b    loop
/// ```
pub const PWM: [u8; 44] = [
    0x00, 0x20, 0x04, 0x20, 0x09, 0x01, 0x00, 0x10, 0x05, 0x48, 0x04, 0x21, 0x01, 0x60, 0x05, 0x48, 0x05, 0x49, 0x01,
    0x61, 0xFA, 0x21, 0xC1, 0x60, 0x01, 0x21, 0x01, 0x60, 0xFE, 0xE7, 0x00, 0x00, 0x04, 0x40, 0x01, 0x40, 0x00, 0x00,
    0x05, 0x40, 0xE7, 0x03, 0x00, 0x00,
];

/// [`PWM`] as a UF2 file.
pub fn pwm_uf2() -> Vec<u8> {
    uf2::write(VECTOR_TABLE, &PWM)
}
