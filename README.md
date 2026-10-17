# ITS-Board Emulator

Emulator for the **HAW Hamburg ITS-Board** (NUCLEO-F429ZI with the ITS carrier
board and the Waveshare 4" TFT touch shield). It runs the unmodified firmware
images (`.axf`) that the course toolchain ([ITS-BRD-VSC](https://github.com/Transport-Protocol/ITS-BRD-VSC),
Arm Compiler 6) produces, including the HAL, ITS_BRD_LIB and the display driver.

Status: work in progress — see [PLAN.md](PLAN.md).

## What is emulated

| | |
|---|---|
| CPU | Cortex-M4F (Thumb-2, FPU) via [Unicorn](https://www.unicorn-engine.org/); NVIC, SysTick, exceptions, faults in Rust |
| Clock | RCC clock tree, virtual time in CPU cycles (16 MHz HSI → 180 MHz PLL); cycle model of the Cortex-M4 (loads, branches, divisions, exceptions, APB wait states) measured on the board |
| Peripherals | GPIO A–K, TIM2–5, SPI1, USART3, PWR, flash interface, bit-banding |
| Board | LEDs OUT0–15, buttons S0–S7, INT0–5, Nucleo LEDs and USER button |
| Display | ILI9486 480×320 TFT with SPI timing, XPT2046 touch, backlight PWM, tone response of the real panel (GUI; `--ideal-colors` to switch off) |
| Serial | Virtual COM port (USART3, `printf` / `stdin_getchar`) |

| Debugging | GDB remote server: breakpoints, single step, watchpoints, registers incl. FPU, memory, `monitor reset halt` |

Not yet: Ethernet, DMA, ADC/DAC, I²C, further timers and UARTs.

## Usage

```sh
# graphical board
cargo run --release -p its-emu-gui -- path/to/Program.axf

# command line (headless): run 10 s of virtual time, press S7 at 2.5 s,
# send a line to the serial port and save the display
cargo run --release -p its-emu -- path/to/Program.axf --seconds 10 \
    --input "2.5:S7=1,2.6:S7=0,3:SERIAL=hello\n" --screenshot screen.png
```

GUI: keys `0`–`7` press S0–S7, right click latches a button, clicking the
display is a touch, firmware files can be dropped onto the window.

The font table that ITS_BRD_LIB expects in flash at `0x0810_0000` is built in.

## Debugging with GDB

The GUI starts a GDB server on `127.0.0.1:3333` (`--gdb ADDR` to change,
`--no-gdb` to switch it off). The command line version waits for a debugger
with `--gdb [ADDR]`. Connecting halts the core; detaching lets it run on.
Started without a firmware file, both come up like a board with empty flash,
and the debugger loads the program:

```sh
arm-none-eabi-gdb Program.axf -ex "target remote :3333"
(gdb) monitor load Program.axf
(gdb) tbreak main
(gdb) continue
```

`monitor load FILE` flashes an `.axf` from the computer the emulator runs on
(relative paths are relative to the emulator's working directory) by its
segments, as pyOCD does, then resets and stays halted; the GUI then shows the
file name and its symbols. GDB's own `load` also writes flash, but it writes
sections at their run addresses: in Arm Compiler 6 images the initial values
of `.data` (`RW_IRAM1`) then end up in RAM instead of flash, where the startup
code copies them from, and the program misbehaves. Use `monitor load` for
course programs.

The course's VS Code launch configurations always start pyOCD with
`--probe stlink:`, which only finds a USB ST-LINK, so the emulator needs its
own configuration (`.vscode/launch.json` of the course project, CDT GDB
adapter). It loads the program that was just built, resets and stops at
`main`, like **STLink@pyOCD (launch)** on the board:

```json
{
    "name": "ITS-Board Emulator",
    "type": "gdbtarget",
    "request": "attach",
    "cwd": "${workspaceFolder}/Programs/StoppuhrASM",
    "program": "out/StoppuhrASM/ITSboard_Assembly/Debug/StoppuhrASM.axf",
    "gdb": "arm-none-eabi-gdb",
    "auxiliaryGdb": true,
    "initCommands": [
        "monitor load ${workspaceFolder}/Programs/StoppuhrASM/out/StoppuhrASM/ITSboard_Assembly/Debug/StoppuhrASM.axf",
        "maintenance flush register-cache",
        "tbreak main"
    ],
    "customResetCommands": ["monitor reset halt", "maintenance flush register-cache", "tbreak main", "continue"],
    "target": { "port": "3333" }
}
```

Several debuggers may connect at once: the first one controls the core, the
others (such as the CDT adapter's auxiliary GDB for live watch) read and write
memory and registers while it runs. Breakpoints do not modify memory, so they
also work in flash. Watchpoints slow emulation down while they are set.

## Comparison with the real board

`tests/fixtures/ac6-src/HwCheck` is a test program that takes commands over
the virtual COM port (colour bars, fonts, geometry, LED patterns, backlight,
timing of library calls and instruction kernels with DWT->CYCCNT, pin
levels). The same commands run on the real board and in the emulator;
`crates/itsboard/tests/hardware.rs` compares the emulator with board results
stored in `tests/hardware`.

| | board | emulator |
|---|---|---|
| `GUI_clear` (SPI bound) | 39 325 428 TIM2 ticks | 39 325 157 |
| `GUI_disStr`, Font24, 20 characters | 2 456 720 | 2 445 960 (−0.4 %) |
| C delay loop / assembler loop, 100 000 iterations | 800 346 / 150 198 | 800 336 / 150 181 |
| filled circle, radius 100 (point by point) | 133.3 M | 124.7 M (−6.5 %) |
| input levels of ports A–I (no pull / pull-down / pull-up) | | identical |

`tools/hw` drives the board (Nucleo ST-LINK: `flash.sh`, `cmd.py`) and a
phone camera over adb (`camera-pro.sh`, `snap.sh`, Samsung camera in Pro
mode with fixed exposure); `boardphoto.py` extracts the display (perspective
corrected) and the LED states from a photo, `compare.sh` puts photo and
emulator screenshot side by side. The board in the GUI is a vector drawing
(`apps/its-emu-gui/assets/board.svg`) made by `boarddraw.py` after the
stacked board photo (`stack-capture.sh`, `boardstack.py`, `boardasset.py`);
`boardlabelfit.py` and `boardtraces.py` fit the silkscreen labels and trace
the copper under the mask in that photo.

```sh
tools/hw/flash.sh tests/fixtures/ac6/HwCheck.axf
python3 tools/hw/cmd.py --wait-ready time cyc   # on the board
tools/hw/compare.sh /tmp/cmp bars bars          # photo vs. emulator
```

## Building

Rust (stable), a C toolchain, CMake and libclang (for Unicorn and its
bindings). The GUI loads the X11/Wayland libraries at run time, so the CI
builds on the stock GitHub runners without extra packages.

```sh
cargo build --release
cargo test --release
```

CI (`.github/workflows/ci.yml`) checks formatting and clippy, and builds and
tests on Linux, Windows and macOS, each on x64 and arm64 (Windows on Arm is
experimental). It installs `gdb-multiarch` on Linux for the GDB server tests,
records throughput in the job summary and keeps the binaries as artifacts for
14 days. The tests that need the personal lab solutions in `tests/fixtures/ac6`
(not published) skip themselves there.

Test fixtures in `tests/fixtures/ac6` are rebuilt from a checkout of
ITS-BRD-VSC with `tools/build-fixtures.sh` (needs the CMSIS-Toolbox and Arm
Compiler 6 as installed by the course's VS Code setup); only the own test
programs with `tools/build-own.sh [Name]`.

## License

MIT for the emulator. The firmware images in `tests/fixtures/ac6` and the font
table are built from course material under the BSD 2-Clause license
(HAW Hamburg, Labor für technische Informatik); see the README files there.
