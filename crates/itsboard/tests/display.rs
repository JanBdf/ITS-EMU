//! End-to-end tests of the board with course programs built by AC6.

use emu_core::{ElfImage, Machine, StopReason};
use itsboard::{ItsBoard, SCREEN_HEIGHT, SCREEN_WIDTH};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/ac6");
const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/golden");

struct Sim {
    machine: Machine,
    board: ItsBoard,
    image: ElfImage,
}

impl Sim {
    /// Boots a fixture, or `None` if it is not available (personal lab solutions).
    fn boot(name: &str) -> Option<Sim> {
        let Ok(data) = std::fs::read(format!("{FIXTURES}/{name}")) else {
            eprintln!("skipping: {name} not available (run tools/build-fixtures.sh)");
            return None;
        };
        let image = ElfImage::parse(&data).unwrap();
        let (bus, mcu) = stm32f429::build();
        let board = ItsBoard::attach(&mcu);
        let mut machine = Machine::new(stm32f429::machine_config(), bus).unwrap();
        machine.load_elf(&image).unwrap();
        machine.load_bin(0x0810_0000, &std::fs::read(format!("{FIXTURES}/fonts.bin")).unwrap()).unwrap();
        machine.reset();
        Some(Sim { machine, board, image })
    }

    fn run_until_ms(&mut self, ms: u64) {
        let target = ms * 1_000_000;
        while self.machine.clock().time_ns() < target {
            let budget = self.machine.clock().ns_to_cycles(target - self.machine.clock().time_ns()).max(1);
            assert_eq!(self.machine.run(budget), StopReason::Budget, "pc {:#x}", self.machine.pc());
        }
    }

    fn screen(&self) -> Vec<u8> {
        let mut rgba = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        self.board.render_display(&mut rgba);
        rgba
    }

    fn c_string(&self, symbol: &str, len: usize) -> String {
        let addr = self.image.symbol(symbol).unwrap().addr;
        let mut buf = vec![0u8; len];
        assert!(self.machine.read_mem(addr, &mut buf));
        String::from_utf8_lossy(&buf).trim_end_matches('\0').to_owned()
    }
}

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
