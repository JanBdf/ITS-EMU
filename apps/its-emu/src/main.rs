//! Command line front end of the ITS-Board emulator.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use emu_core::{CoreReg, ElfImage, Machine, StopReason};

#[derive(Parser, Debug)]
#[command(version, about = "ITS-Board emulator (STM32F429ZI)")]
struct Args {
    /// Firmware image (.axf / .elf)
    firmware: PathBuf,

    /// Font table to flash at 0x0810_0000 instead of the built-in fonts.bin of ITS_BRD_LIB
    #[arg(long)]
    fonts: Option<PathBuf>,

    /// Do not flash any font table
    #[arg(long, conflicts_with = "fonts")]
    no_fonts: bool,

    /// Stop after this many seconds of virtual time (0 = no limit)
    #[arg(long, default_value_t = 10.0)]
    seconds: f64,

    /// Report and step over `BKPT #n` with n != 0 instead of stopping
    /// (`BKPT #0` always stops)
    #[arg(long)]
    continue_on_bkpt: bool,

    /// Run in real time instead of as fast as possible
    #[arg(long)]
    realtime: bool,

    /// Do not fast-forward idle loops (for comparison/debugging)
    #[arg(long)]
    no_idle_skip: bool,

    /// Write the display content to this PNG file when the run ends
    #[arg(long)]
    screenshot: Option<PathBuf>,

    /// Scripted inputs, comma separated `<seconds>:<input>=<value>`:
    /// `S0`..`S7` and `USER` take 1 (pressed) / 0 (released),
    /// `TOUCH` takes `x;y` or `0`, `SERIAL` takes text for the virtual COM
    /// port (`\n` = newline, no commas). Example: `2.5:S7=1,2.6:S7=0,3:SERIAL=hi\n`
    #[arg(long, value_parser = parse_inputs)]
    input: Option<InputScript>,

    /// Forward the terminal's standard input to the virtual COM port
    #[arg(long)]
    stdin: bool,
}


#[derive(Debug, Clone)]
enum Input {
    Button(u32, bool),
    User(bool),
    Touch(Option<(f32, f32)>),
    Serial(Vec<u8>),
}

#[derive(Debug, Clone)]
struct InputEvent {
    at_ns: u64,
    input: Input,
}

#[derive(Debug, Clone)]
struct InputScript(Vec<InputEvent>);

fn parse_inputs(s: &str) -> Result<InputScript, String> {
    let mut events = Vec::new();
    for item in s.split(',').map(str::trim).filter(|i| !i.is_empty()) {
        let (time, rest) = item.split_once(':').ok_or_else(|| format!("missing ':' in {item:?}"))?;
        let (name, value) = rest.split_once('=').ok_or_else(|| format!("missing '=' in {item:?}"))?;
        let seconds: f64 = time.parse().map_err(|_| format!("bad time in {item:?}"))?;
        let flag = || match value {
            "1" => Ok(true),
            "0" => Ok(false),
            _ => Err(format!("expected 0 or 1 in {item:?}")),
        };
        let input = match name.to_ascii_uppercase().as_str() {
            "USER" => Input::User(flag()?),
            "SERIAL" => Input::Serial(value.replace("\\n", "\n").replace("\\r", "\r").into_bytes()),
            "TOUCH" if value == "0" => Input::Touch(None),
            "TOUCH" => {
                let (x, y) = value.split_once(';').ok_or_else(|| format!("expected x;y in {item:?}"))?;
                let parse = |v: &str| v.parse::<f32>().map_err(|_| format!("bad coordinate in {item:?}"));
                Input::Touch(Some((parse(x)?, parse(y)?)))
            }
            b if b.len() == 2 && b.starts_with('S') => {
                let n = b[1..].parse::<u32>().ok().filter(|&n| n < 8).ok_or_else(|| format!("unknown button {name}"))?;
                Input::Button(n, flag()?)
            }
            _ => return Err(format!("unknown input {name:?}")),
        };
        events.push(InputEvent { at_ns: (seconds * 1e9) as u64, input });
    }
    events.sort_by_key(|e| e.at_ns);
    Ok(InputScript(events))
}

fn apply_input(board: &itsboard::ItsBoard, input: &Input) {
    match input {
        Input::Button(n, pressed) => board.set_button(*n, *pressed),
        Input::User(pressed) => board.set_user_button(*pressed),
        Input::Touch(pos) => board.set_touch(*pos),
        Input::Serial(data) => board.mcu().usart3.borrow_mut().send(data),
    }
}
/// Virtual time slice between throttling decisions in real-time mode.
const REALTIME_SLICE_NS: u64 = 2_000_000;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args = Args::parse();

    let data = std::fs::read(&args.firmware).with_context(|| format!("reading {}", args.firmware.display()))?;
    let fonts = match &args.fonts {
        Some(path) => Some(std::fs::read(path).with_context(|| format!("reading {}", path.display()))?),
        None if args.no_fonts => None,
        None => Some(itsboard::sim::DEFAULT_FONTS.to_vec()),
    };
    let itsboard::Simulator { mut machine, board, image } = itsboard::Simulator::new(&data, fonts.as_deref())?;
    let mcu = board.mcu().clone();
    machine.system_mut().idle_skipping = !args.no_idle_skip;

    let limit_ns = if args.seconds > 0.0 { (args.seconds * 1e9) as u64 } else { u64::MAX };
    let stdin_rx = args.stdin.then(spawn_stdin_reader);
    let mut inputs = args.input.clone().map(|s| s.0).unwrap_or_default().into_iter().peekable();
    let start = Instant::now();
    let mut bkpts = 0u32;
    let stop = loop {
        let clock = machine.clock();
        let now_ns = clock.time_ns();
        if now_ns >= limit_ns {
            break None;
        }
        while let Some(ev) = inputs.next_if(|e| e.at_ns <= now_ns) {
            log::info!("input at {:.3} s: {:?}", now_ns as f64 / 1e9, ev.input);
            apply_input(&board, &ev.input);
        }
        let mut slice_ns = if args.realtime { REALTIME_SLICE_NS } else { limit_ns - now_ns }.min(limit_ns - now_ns);
        if let Some(ev) = inputs.peek() {
            slice_ns = slice_ns.min(ev.at_ns - now_ns);
        }
        let budget = clock.ns_to_cycles(slice_ns).max(1);
        let result = machine.run(budget);
        forward_serial(&mcu, stdin_rx.as_ref());
        match result {
            StopReason::Budget => {}
            StopReason::Breakpoint { pc } if args.continue_on_bkpt && bkpt_imm(&machine, pc) != 0 => {
                bkpts += 1;
                report_progress(&machine, &image, start, &format!("BKPT #{bkpts} at {}", describe(&image, pc)));
                machine.set_reg(CoreReg::Pc, (pc + 2) | 1);
            }
            other => break Some(other),
        }
        if args.realtime {
            let ahead = Duration::from_nanos(machine.clock().time_ns()).saturating_sub(start.elapsed());
            if !ahead.is_zero() {
                std::thread::sleep(ahead);
            }
        }
    };

    let what = match stop {
        None => "time limit reached".to_owned(),
        Some(StopReason::Breakpoint { pc }) => format!("BKPT at {}", describe(&image, pc)),
        Some(StopReason::Lockup { pc, reason }) => format!("LOCKUP at {}: {reason}", describe(&image, pc)),
        Some(StopReason::Budget) => unreachable!(),
    };
    report_progress(&machine, &image, start, &what);
    report_board(&board);
    if let Some(path) = &args.screenshot {
        save_screenshot(&board, path)?;
        println!("  display saved to {}", path.display());
    }
    Ok(())
}

/// Prints the state of the ITS-Board LEDs and the Nucleo LEDs.
fn report_board(board: &itsboard::ItsBoard) {
    let dot = |on: bool| if on { '●' } else { '○' };
    let leds = board.leds();
    let out: String = (0..16).rev().map(|i| dot(leds & (1 << i) != 0)).collect();
    let [ld1, ld2, ld3] = board.nucleo_leds();
    println!(
        "  OUT15..0 {out}  LD1 {} LD2 {} LD3 {}  backlight {:.0} %  core clock {} MHz",
        dot(ld1),
        dot(ld2),
        dot(ld3),
        board.backlight() * 100.0,
        board.mcu().clocks.get().hclk / 1_000_000
    );
}

fn save_screenshot(board: &itsboard::ItsBoard, path: &std::path::Path) -> Result<()> {
    use itsboard::{SCREEN_HEIGHT, SCREEN_WIDTH};
    let mut rgba = vec![0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
    board.render_display(&mut rgba);
    let file = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgba)?;
    Ok(())
}

/// Reads standard input on a separate thread.
fn spawn_stdin_reader() -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 256];
        let mut stdin = std::io::stdin();
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    rx
}

/// Copies UART output to stdout and pending stdin data to the UART.
fn forward_serial(mcu: &stm32f429::Stm32f429, stdin_rx: Option<&std::sync::mpsc::Receiver<Vec<u8>>>) {
    use std::io::Write;
    let mut port = mcu.usart3.borrow_mut();
    let out = port.take_output();
    if !out.is_empty() {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&out);
        let _ = stdout.flush();
    }
    if let Some(rx) = stdin_rx {
        while let Ok(data) = rx.try_recv() {
            port.send(&data);
        }
    }
}

/// Immediate of the 16-bit `BKPT #imm` instruction at `pc`.
fn bkpt_imm(machine: &Machine, pc: u32) -> u8 {
    let mut insn = [0u8; 2];
    if machine.read_mem(pc, &mut insn) { insn[0] } else { 0 }
}

fn describe(image: &ElfImage, pc: u32) -> String {
    match image.symbolize(pc) {
        Some((sym, off)) => format!("{pc:#010x} <{}+{off:#x}>", sym.name),
        None => format!("{pc:#010x}"),
    }
}

fn report_progress(machine: &Machine, image: &ElfImage, start: Instant, what: &str) {
    let clock = machine.clock();
    let wall = start.elapsed().as_secs_f64();
    let cycles = clock.cycles();
    println!(
        "{what}\n  pc {}  virtual time {:.6} s  cycles {cycles}  wall {wall:.3} s  ({:.1} MIPS)",
        describe(image, machine.pc()),
        clock.time_ns() as f64 / 1e9,
        cycles as f64 / wall / 1e6,
    );
}
