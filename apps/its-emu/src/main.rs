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

    /// Raw binary to load at 0x0810_0000 (fonts.bin of ITS_BRD_LIB)
    #[arg(long)]
    fonts: Option<PathBuf>,

    /// Stop after this many seconds of virtual time
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
}

const FONTS_ADDR: u32 = 0x0810_0000;
/// Virtual time slice between throttling decisions in real-time mode.
const REALTIME_SLICE_NS: u64 = 2_000_000;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args = Args::parse();

    let data = std::fs::read(&args.firmware).with_context(|| format!("reading {}", args.firmware.display()))?;
    let image = ElfImage::parse(&data)?;
    let (bus, mcu) = stm32f429::build();
    let mut machine = Machine::new(stm32f429::machine_config(), bus)?;
    machine.load_elf(&image)?;
    if let Some(fonts) = &args.fonts {
        let bin = std::fs::read(fonts).with_context(|| format!("reading {}", fonts.display()))?;
        machine.load_bin(FONTS_ADDR, &bin)?;
    }
    machine.system_mut().idle_skipping = !args.no_idle_skip;
    machine.reset();

    let limit_ns = (args.seconds * 1e9) as u64;
    let start = Instant::now();
    let mut bkpts = 0u32;
    let stop = loop {
        let clock = machine.clock();
        let now_ns = clock.time_ns();
        if now_ns >= limit_ns {
            break None;
        }
        let slice_ns = if args.realtime { REALTIME_SLICE_NS } else { limit_ns - now_ns }.min(limit_ns - now_ns);
        let budget = clock.ns_to_cycles(slice_ns).max(1);
        match machine.run(budget) {
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
    report_board(&mcu);
    Ok(())
}

/// Prints the state of the ITS-Board LEDs (OUT0–15 = PD0–7, PE0–7) and Nucleo LEDs.
fn report_board(mcu: &stm32f429::Stm32f429) {
    use stm32f429::gpio::port;
    let gpio = mcu.gpio.borrow();
    let led = |p: char, pin: u32| if gpio.level(port(p), pin) { '●' } else { '○' };
    let out: String = (0..16).rev().map(|i| if i < 8 { led('D', i) } else { led('E', i - 8) }).collect();
    println!(
        "  OUT15..0 {}  LD1 {} LD2 {} LD3 {}  core clock {} MHz",
        out,
        led('B', 0),
        led('B', 7),
        led('B', 14),
        mcu.clocks.get().hclk / 1_000_000
    );
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
