//! printf/stdin over USART3 (virtual COM port) through the course library.

mod common;

use common::Sim;

#[test]
fn test_stdinout_usart_prints_with_float_formatting() {
    let Some(mut sim) = Sim::boot("TestStdinoutUSART.axf") else { return };
    // LCD_init (~1.9 s) + HAL_Delay(10000) before the first line, then 3 s per line
    sim.run_until_ms(11_000);
    assert_eq!(sim.take_serial(), "");
    sim.run_until_ms(18_500);
    assert_eq!(
        sim.take_serial(),
        "Hallo liebes TI-Labor (c-project)\n\
         i = 0 (float) i / 10 = 0.000000\n\
         i = 1 (float) i / 10 = 0.100000\n\
         i = 2 (float) i / 10 = 0.200000\n"
    );
}

#[test]
fn serial_echo_round_trip() {
    let Some(mut sim) = Sim::boot("SerialEcho.axf") else { return };
    sim.run_until_ms(100);
    assert_eq!(sim.take_serial(), "ready\n");
    sim.send_serial("hello\n");
    sim.run_until_ms(200);
    sim.send_serial("ITS board\r");
    sim.run_until_ms(300);
    assert_eq!(sim.take_serial(), "echo 1: hello (5 chars)\necho 2: ITS board (9 chars)\n");
}

#[test]
fn bytes_sent_while_program_is_busy_are_lost_like_on_hardware() {
    // While the program prints its answer no Receive() is pending; the
    // driver drops bytes arriving then (as on the real board).
    let Some(mut sim) = Sim::boot("SerialEcho.axf") else { return };
    sim.run_until_ms(100);
    sim.take_serial();
    sim.send_serial("a\nbcdefghij\n");
    sim.run_until_ms(200);
    let out = sim.take_serial();
    assert!(out.starts_with("echo 1: a (1 chars)\n"), "{out}");
    assert!(!out.contains("bcdefghij"), "{out}");
}
