//! ADC conversions of the temperature sensor and an input with a voltage
//! set from outside, polled by the firmware.

use rp2040::Pico;
use rp2040::adc::Analog;
use rp2040::testfw::{adc_uf2, stub_bootrom};

fn results(analog: Analog) -> [u32; 2] {
    let mut pico = Pico::new(&stub_bootrom(), &adc_uf2()).unwrap();
    pico.analog().set(analog);
    pico.run_until_ps(1_000_000_000).expect("runs for 1 ms");
    let mut buf = [0u8; 8];
    assert!(pico.machine().read_mem(0x2000_0000, &mut buf));
    [0, 4].map(|i| u32::from_le_bytes(buf[i..i + 4].try_into().unwrap()))
}

#[test]
fn temperature_sensor_and_an_input_voltage() {
    let [ts, gpio26] = results(Analog { volts: [Some(1.65), None, None, None], celsius: 27.0 });
    assert_eq!(ts, (0.706 / 3.3 * 4096.0) as u32, "0.706 V at 27 °C");
    assert_eq!(gpio26, 2048);
    let [ts, gpio26] = results(Analog { volts: [None; 4], celsius: 50.0 });
    assert_eq!(ts, ((0.706 - 23.0 * 0.001_721) / 3.3 * 4096.0) as u32);
    assert_eq!(gpio26, 0, "the pad's pull-down");
}
