# ITS-Board Emulator — Plan

Emulator for the HAW Hamburg ITS-Board that runs **unmodified firmware** (`.axf`
built with Arm Compiler 6 from [ITS-BRD-VSC](../ITS-BRD-VSC)) and simulates the
physical board: LEDs, buttons, TFT display, touch, UART, timers.

## 1. Target hardware (from ITS-BRD-VSC)

| Function | MCU resource | Notes |
|---|---|---|
| MCU | STM32F429ZI, Cortex-M4F, 180 MHz | APB1 45 MHz → TIM2 clock 90 MHz (`TICKS_PER_US 90`) |
| Memory | Flash `0x08000000` (2 MB), SRAM `0x20000000` (192 KB), CCM `0x10000000` (64 KB) | from `*_ITSboard.sct` |
| Flash fonts | `fonts.bin` at `0x08100000`, version word `1595446945` | checked by `initLCDTouch()` |
| LEDs OUT0–7 / OUT8–15 | GPIOD[0..7] / GPIOE[0..7] | push-pull |
| Buttons S0–S7 (IN0–7) | GPIOF[0..7] | pull-up → presumably active-low (verify) |
| INT0–5 | GPIOG[0..5] | input, pull-up |
| Nucleo LD1/LD2/LD3, user button | PB0, PB7, PB14, PC13 | `Error_Handler` blinks LD1/LD2 |
| TFT ILI9486 480×320 | SPI1 (PA5/6/7), CS PD14, DC PF13, RST PF12, BL PD15 | Waveshare shield: 16-bit words over 8-bit SPI; register-polled driver |
| Touch XPT2046 | SPI1, CS PF14, IRQ PE13, BUSY PB10 | commands `0xD0` (X) / `0x90` (Y) |
| stdin/stdout | USART3 (PD8/PD9), 115200 8N1 | Keil `Driver_USART`, interrupt driven |
| Time | SysTick 1 kHz (`HAL_Delay`), TIM2 32-bit free-running | |
| Quirk | LCD SPI code touches `ETH->MACCR`, polls `ETH->DMASR` | needs RAZ/WI stub |

## 2. Requirements

- **R1** Run unmodified AC6 `.axf`: full Thumb-2 + FPv4-SP. ASM projects use armasm
  syntax → test fixtures must be built with AC6 (GCC cannot assemble them).
- **R2** Register-accurate peripherals for everything the HAL/ITS_BRD_LIB touch.
- **R3** Board simulation: LEDs, buttons, display, touch, Nucleo LEDs/button,
  extensible for external devices on the GPIO header.
- **R4** Cycle-based virtual time (bit-banging must work), throttled to wall clock.
- **R5** GDB remote server on `:3333`, compatible with the VS Code attach config.
- **R6** Linux, Windows, macOS (x64 + arm64).
- **R7** Small, dependency-free download for students.
- **R8** Headless/scripted mode for tests and CI; later diffing against real-board traces.
- **R9** Maintainable by a small team → one language, one process.

## 3. Tech stack decision

Evaluated: QEMU upstream (no F429, no GPIO/PWR/Flash models), xPack QEMU legacy
(unmaintained), Wokwi (closed, no F429), Arm FVP (no STM32 peripherals),
Renode + custom frontend, Unicorn + Rust, pure-Rust interpreter.

**Chosen: Rust + Unicorn** (single process, single language).

| Layer | Choice |
|---|---|
| Language | Rust stable, Cargo workspace |
| CPU | `unicorn-engine` 2.x (Cortex-M4 mode) behind a `Cpu` trait |
| Exceptions/NVIC | own implementation (stacking, EXC_RETURN, priorities, tail-chaining later) |
| Virtual time | instruction count ≈ cycles at 180 MHz, throttled to wall clock, idle skipping |
| Peripherals | hand-written Rust `Peripheral` trait, MMIO via Unicorn hooks |
| Board | pin nets + `Device` trait (LED, button, ILI9486, XPT2046, …) |
| Debugger | `gdbstub` (Cortex-M + FPU target description) |
| GUI | `egui` / `eframe` |
| CLI / tests | headless runner with YAML scripts, PNG screenshots |
| Fixtures | `Programs/*` built with AC6 (committed binaries); clang/lld-built micro fixtures for unit tests |
| Release | GitHub Actions matrix + `cargo-dist` |

**Fallback:** Renode backend if Unicorn's M-profile gaps prove unfixable, or if
Ethernet/lwIP (`Programs/Stack`) becomes a priority. Board models/GUI/tests
designs carry over.

**Possible later:** pure-Rust interpreter behind the same `Cpu` trait → WebAssembly
build that runs in the browser.

## 4. Architecture

```
apps/its-emu (CLI)        apps/its-emu-gui (egui)
        \                     /
         its-board  — wiring, ILI9486, XPT2046, LEDs/buttons, fonts.bin
              |
         stm32f429  — Bus, RCC, PWR, FLASH, GPIO, TIM2, SPI1, USART3, EXTI, stubs
              |
         emu-core   — Cpu trait, Unicorn backend, Cortex-M system (NVIC, SCB,
                      SysTick, MPU), exceptions, virtual clock, ELF loader
              |
         gdb        — gdbstub target
```

Execution loop: run the CPU for a time slice (instruction budget) → advance
virtual clock → tick peripherals → raise IRQs → if an exception is pending and
allowed, perform exception entry at the slice boundary → throttle to wall clock.

Unmapped peripheral space reads as zero / ignores writes (warn once); other
unmapped memory raises a BusFault. HardFaults produce a readable report
(stacked PC/LR, CFSR/HFSR/MMFAR/BFAR).

## 5. Phases

| # | Phase | Exit criterion |
|---|---|---|
| 0 | **Spike** (see §6) | Go/no-go for Unicorn |
| 1 | CPU, memory map, ELF loader | A1ASM/A2ASM reach their final loop with expected registers/memory |
| 2 | System peripherals: RCC/PWR/FLASH flags, SysTick, NVIC/SCB, TIM2, MPU | `initITSboard()` returns, `HAL_Delay(1000)` = 1 s virtual |
| 3 | GPIO + board GUI (LEDs, buttons, reset/pause) | LauflichtASM runs; `Error_Handler` blinks LD1/LD2 |
| 4 | SPI1 + ILI9486 + framebuffer | TestNewFont matches real display; StoppuhrASM usable |
| 5 | XPT2046 touch | keypad works with mouse |
| 6 | USART3 + EXTI | TestStdinoutUSART works incl. stdin |
| 7 | GDB stub | VS Code attach config debugs the emulator |
| 8 | Headless test scripts, register viewer, snapshots, release builds | CI runs all sample programs on 3 OSes |
| — | Out of scope for now | ETH/lwIP, DMA, further timers |

## 6. Spike (Phase 0)

1. Toolchain: CMSIS-Toolbox + AC6 (MDK Community license) on Linux; build A1ASM,
   A2ASM, LauflichtASM, StoppuhrASM, TestNewFont, TestStdinoutUSART → `tests/fixtures/`.
2. Unicorn runs Thumb code from an ELF; registers/memory checked.
3. Exceptions + SysTick: interrupt entry/return works; `HAL_Delay` semantics.
4. Throughput on all three OSes (target ≥ 180 MIPS effective, or idle skipping).
5. `gdbstub` attach + single step from VS Code.

## 7. Validation strategy

- Unit tests per peripheral (register semantics from RM0090).
- Micro fixtures (clang + lld, bare-metal) for CPU/exception behaviour.
- AC6 sample programs as integration tests (headless scripts + screenshots).
- Real-board traces later: same GDB script run against board (pyOCD) and emulator,
  instruction/register traces diffed.

## 8. Open points

- Button/LED polarity and display orientation → verify on hardware.
- macOS code signing requires an Apple developer account (otherwise unsigned builds).
- Which future labs use external hardware on the GPIO header (encoder, 1-Wire, …)?
- Board photos (`hardware_img/`, board Rev 02 15.12.2020) show further headers that
  later labs may use: I2C1, SPI3 (PA15/PC10–12), Analog (ADC inputs, DAC1 on PA4),
  AUX0–5 (PG0–PG5), RS232 (DB9 + MAX3232) and CAN. Each input S0–S7 has an indicator
  LED (D0–D7) next to it. The TFT is mounted in portrait orientation.

## 9. Spike status (2026-10-16)

| # | Item | Status |
|---|---|---|
| 1 | AC6 toolchain + reference `.axf` | ✅ all 10 `Programs/*` built with AC6 6.24 (`tools/build-fixtures.sh` → `tests/fixtures/ac6/`) |
| 2 | Unicorn runs Thumb code from ELF | ✅ `emu-core` + clang-built fixtures (`tests/fixtures/micro`) |
| 3 | Exceptions + SysTick | ✅ SysTick IRQ, SVC, STIR/NVIC IRQ, BusFault with BFAR and handler return, extended FP frame (mutation-tested) |
| 4 | Throughput | ✅ Linux x86-64 (release): ~300–380 MIPS compute-bound, ~100 MIPS in tight MMIO polling loops. Idle loops (`HAL_Delay`) are fast-forwarded: LauflichtASM runs ~200× faster than real time. Windows/macOS not measured yet (needs CI). |
| 5 | GDB stub | ✅ `emu_core::gdb` (feature `gdb`): gdbstub target + non-blocking multi-client server in GUI and CLI; tested with GDB 18.1 against micro fixture and AC6 programs |

Progress beyond the spike (Phase 2/3):
- RCC (clock tree → virtual core clock), PWR (over-drive handshake), flash
  interface, GPIO A–K (shared pin bank, RCC clock gating), TIM2–5 (up-counting,
  prescaler/ARR preload, update IRQ), peripheral + SRAM bit-band aliases,
  unaligned peripheral accesses split like the core does.
- `initITSboard()` reaches 180 MHz; LauflichtASM runs correctly (integration
  tests in `crates/stm32f429/tests/ac6_programs.rs`).
- Display path: SPI1 (RXNE/OVR exact), ILI9486 (GRAM, CASET/PASET/RAMWR,
  MADCTL, B6 scan mirroring, panel mounting), XPT2046 touch (inverse of the
  library's default calibration), TIM4 PWM backlight, ETH register stub.
  TestNewFont and StoppuhrASM run correctly (golden-image and timing tests in
  `crates/itsboard/tests/display.rs`). CLI: `--screenshot`, `--input`.
- USART3 with real frame timing (baud from BRR/PCLK1), TXE/TC/RXNE/IDLE/ORE
  and their clear sequences, interrupt; host side via `SerialPort`
  (`--stdin`, `--input t:SERIAL=...`). printf incl. `%f` works through the
  Keil driver; own test program `tests/fixtures/ac6-src/SerialEcho` checks the
  receive path. Note: the library has no `fgetc` retarget, so programs read
  input with `stdin_getchar()`, not `scanf`.
- GUI (`apps/its-emu-gui`, egui): emulator on its own thread, board layout
  with LEDs/buttons/display/touch, Nucleo LEDs/USER/RESET, serial console,
  pause/reset/speed, drag & drop; adaptive layout. `ITS_EMU_GUI_SCREENSHOT`
  saves a picture of the window for docs/CI.
- GDB (Phase 7): breakpoints (Unicorn exits), single step, write/read/access
  watchpoints, registers incl. d0–d15/FPSCR and MSP/PSP/PRIMASK/BASEPRI/
  FAULTMASK/CONTROL, memory incl. flash and peripherals, `monitor reset
  [halt]`, Ctrl-C. Several clients: the first controls execution, further
  ones (CDT `auxiliaryGdb` live watch) observe. VS Code attach still to be
  tried with the real CDT adapter.
- Next candidates: CI on Linux/Windows/macOS, LICENSE file before publishing,
  reloading the firmware in the GUI when the `.axf` changes (rebuild while
  debugging), `load` over GDB.
- Polling loops: peripherals report until when a read value stays the same
  (`Peripheral::read_stable_until`), so loops polling a prescaled timer or
  SysTick are skipped in whole iterations up to the next counter change.
  Final `b .` self-loops are now recognised as loops (A1ASM, PrimzahlSiebASM:
  30–70 → ~7000 MIPS). Remaining gap: loops that do SPI output on every
  iteration (StoppuhrASM running, TestNewFont) are real work and run at
  ~250 MIPS (≈1.4× real time); TIM2 without prescaler changes every 2 cycles,
  so polling it cannot be skipped.
- Validation against the real board (2026-10-16, `HwCheck`, `tools/hw`, phone
  camera + ST-LINK): display content, orientation, fonts, geometry and LED
  mapping were already right. Fixed: SPI transfers took no time (GUI_clear
  46 ms instead of 437 ms), one cycle per instruction (loops 30–50 % too
  fast), floating/externally pulled input levels, panel tone response.
  Remaining timing deviations: neighbour effects of peripheral accesses
  (±1 cycle), conditional branches to the next instruction; point-by-point
  drawing −6.5 %. Not checked yet: touch, buttons pressed by hand, display
  off state, Ethernet, exception timing with FPU context.

Findings:
- Timed peripherals (SPI frames every 256 cycles) need events inside a Unicorn
  run: accesses clip the slice to the accessed peripheral's next event, due
  events are processed in the block hook (ending the run costs syscalls), a
  fired event invalidates the idle detector's observations. Reading the PC in
  an MMIO callback gives the start of the block, not the instruction.
- Cycle model (`emu_core::timing`): static block costs at translation time,
  dynamic parts in hooks (taken conditional branch = next block is not the
  fall-through; operand-dependent UDIV/SDIV via code hooks on the division
  sites, installed between runs; bus wait states in the MMIO hooks).
- Breakpoints use Unicorn *exits* (`ctl_exits_enable`/`ctl_set_exits`): checked
  per instruction at translation time, so they cost nothing at run time. A run
  that starts at an exit stops immediately, so resuming steps over it first.
  TBs around a changed breakpoint must be invalidated (`ctl_remove_cache`).
- Single step = `emu_start(count = 1)`; Unicorn drops its count hook (and
  flushes all TBs) on the next normal run. The block hook may fire twice in a
  step (branch), so `step` accounts the cycle itself.
- Watchpoints: memory hooks fire before the access; `emu_stop` only takes
  effect at the next TB, but Unicorn checks for stop requests after every code
  hook. A code hook on all instructions (only while watchpoints exist) stops
  right after the accessing instruction.
- CDT's `auxiliaryGdb` opens a second `target remote` to the same port while
  the first session is connected; the session fails if the server refuses it.
- Unicorn 2 contains QEMU's M-profile code but never calls its `do_interrupt`;
  exception entry **and** return are implemented in Rust (`exception.rs`), driven
  by Unicorn's interrupt hook (`intno 8` = EXC_RETURN, `2` = SVC, `7` = BKPT, …).
- `emu_stop()` must not be called from MMIO callbacks: Unicorn then restarts the
  current instruction and repeats its side effect. Stops are requested via a flag
  and executed by the block hook at the next block boundary.
- Unicorn always allows FPU access (CPACR is not enforced); CONTROL reads bit 3
  (SFPA) as set. Both are minor deviations from real hardware.
- Linux/glibc builds must link `libatomic` (Unicorn uses 16-byte atomics).
- Per-block hook cost dominates for small loops; a direct-mapped block cache
  instead of a `HashMap` roughly doubled throughput.
- Idle-loop fast-forwarding: a loop iteration without stores/MMIO/exceptions
  whose registers at the loop head are unchanged repeats identically, so time
  jumps to the next event. Verified to give identical results (`--no-idle-skip`).
  Reads of time-dependent registers are allowed if their value holds until a
  known cycle; then only whole loop iterations before that cycle are skipped,
  which keeps the timing identical to running them.
