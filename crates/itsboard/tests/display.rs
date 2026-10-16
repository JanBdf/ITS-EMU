//! End-to-end tests of the board with course programs built by AC6.

mod common;

use common::{GOLDEN, Sim};
use itsboard::{SCREEN_HEIGHT, SCREEN_WIDTH};

/// Compares against a golden PNG; set UPDATE_GOLDEN=1 to (re)create it.
fn assert_golden(name: &str, rgba: &[u8]) {
    let path = format!("{GOLDEN}/{name}.png");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        let file = std::fs::File::create(&path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.write_header().unwrap().write_image_data(rgba).unwrap();
        return;
    }
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut golden = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut golden).unwrap();
    let differing = rgba.chunks(4).zip(golden.chunks(4)).filter(|(a, b)| a != b).count();
    assert_eq!(differing, 0, "{differing} pixels differ from {path}");
}

#[test]
fn test_new_font_prompt_and_start() {
    let Some(mut sim) = Sim::boot("TestNewFont.axf") else { return };
    sim.run_until_ms(3000);
    assert!(sim.board.backlight() > 0.7);
    assert_golden("testnewfont_prompt", &sim.screen());

    // The program waits for the blue USER button of the Nucleo board.
    let before = sim.board.display_generation();
    sim.board.set_user_button(true);
    sim.run_until_ms(3200);
    sim.board.set_user_button(false);
    sim.run_until_ms(6000); // first glyph ('!') drawn from all three font sources
    assert_ne!(sim.board.display_generation(), before, "display did not change after USER button");
    assert_golden("testnewfont_after_start", &sim.screen());
}

#[test]
fn stopwatch_measures_button_interval() {
    let Some(mut sim) = Sim::boot("StoppuhrASM.axf") else { return };
    sim.run_until_ms(2500); // LCD_init() alone takes ~1.9 s
    assert_eq!(sim.c_string("TEXT_TIME", 9), "00:00.00");
    sim.board.set_button(7, true); // S7: INIT -> RUNNING
    sim.run_until_ms(2600);
    sim.board.set_button(7, false);
    sim.run_until_ms(4000);
    sim.board.set_button(6, true); // S6: RUNNING -> HOLD
    sim.run_until_ms(4100);
    sim.board.set_button(6, false);
    sim.run_until_ms(5000);

    let shown = sim.c_string("TEXT_TIME", 9);
    let secs: f64 = shown[3..].parse().unwrap();
    assert!((1.48..=1.52).contains(&secs), "stopwatch shows {shown}");
    assert_eq!(sim.board.leds() & 0b11, 0b11, "LEDs for 'running' and 'hold'");
}
