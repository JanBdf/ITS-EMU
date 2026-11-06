//! An LM75 on the I2C header read and written with the HAL (AC6, -O0):
//! polled, with interrupts and with DMA.

mod common;
use common::Sim;

#[test]
fn hal_reads_and_writes_an_lm75() {
    let Some(mut sim) = Sim::boot("I2cLm75.axf") else { return };
    let n = sim.board.add_lm75(0x48);
    sim.run_until_ms(500);
    let out = sim.take_serial();
    let lines: Vec<&str> = out.lines().filter(|l| l.starts_with("L ") && !l.starts_with("L t ")).collect();
    assert_eq!(
        lines,
        [
            "L ready 1",
            "L absent 0",
            "L poll 0 1500 err 0", // 21 °C
            "L tos 0 5500",
            "L it 0 1 5500",
            "L dmatx 0 1",
            "L dmarx 0 1 5080 5080",
            "L dmatemp 0 1 1500",
            "L done",
        ]
    );
    sim.board.set_temperature(n, -10.25).unwrap();
    sim.run_until_ms(750);
    assert!(sim.take_serial().ends_with("L t f5c0\n"));
}

#[test]
fn without_a_sensor_the_address_is_not_acknowledged() {
    let Some(mut sim) = Sim::boot("I2cLm75.axf") else { return };
    sim.run_until_ms(400);
    let out = sim.take_serial();
    assert!(out.contains("L ready 0\n") && out.contains("L poll 1 0000 err 4\n"), "{out}");
}
