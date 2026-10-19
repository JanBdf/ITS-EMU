# ITS-Board Emulator

Emulator for the **HAW Hamburg ITS-Board** (NUCLEO-F429ZI with the ITS carrier
board and the Waveshare 4" TFT touch shield). It runs the unmodified firmware
images (`.axf`) that the course toolchain ([ITS-BRD-VSC](https://github.com/Transport-Protocol/ITS-BRD-VSC),
Arm Compiler 6) produces, including the HAL, ITS_BRD_LIB and the display driver.

Status: work in progress — see [PLAN.md](PLAN.md).

> **Disclaimer:** This is an independent, private project. It is **not
> affiliated with, endorsed by or supported by** the HAW Hamburg, its Labor
> für technische Informatik or the [ITS-BRD-VSC](https://github.com/Transport-Protocol/ITS-BRD-VSC)
> project. The names *HAW Hamburg*, *ITS-Board* and *ITS-BRD-VSC* are used
> only to describe what the emulator is compatible with. The emulator is not a
> substitute for the real board: results may differ from the hardware, and
> course or exam requirements always refer to the real ITS-Board. Please do
> not contact the course staff about this emulator.

## Installation

Packages are published on the [Releases](https://github.com/JanBdf/ITS-EMU/releases) page:

| Platform | Package | Install |
|---|---|---|
| Arch Linux (x86_64) | `its-emu-<version>-x86_64.pkg.tar.zst` | `sudo pacman -U its-emu-*.pkg.tar.zst` |

Packages for other platforms will follow. Until then, build from source
([below](#building)); the CI also keeps binaries for Linux, Windows and macOS
as workflow artifacts for 14 days.

## What is emulated

| | |
|---|---|
| CPU | Cortex-M4F (Thumb-2, FPU) via [Unicorn](https://www.unicorn-engine.org/); NVIC, SysTick, exceptions, faults in Rust |
| Clock | RCC clock tree, virtual time in CPU cycles (16 MHz HSI → 180 MHz PLL); cycle model of the Cortex-M4 (loads, branches, divisions, exceptions, APB wait states) measured on the board |
| Peripherals | GPIO A–K, EXTI + SYSCFG, TIM1–14 (up/down/center-aligned counting, output compare and PWM on the pins, input capture, encoder mode, slave modes reset/gated/trigger/external clock with TI1/TI2, TIM1/8 repetition counter), ADC1–3 (regular group started by TIM2/TIM3/TIM8 TRGO), DAC (timer triggers, noise/triangle waves), I²C1–3 (master; nothing connected answers with NACK), SPI1–6, USART1–6 + UART4/5/7/8 (only USART3 connected), IWDG (watchdog reset, reset flags in RCC_CSR), CRC, RNG, DMA1/2 (direct mode; requests from the ADCs, U(S)ARTs, SPIs and the DAC), PWR, flash interface (programming and sector/mass erase with the datasheet's timing), bit-banding |
| Board | LEDs OUT0–15, buttons S0–S7, INT0–5, Analog header (voltages on the ADC pins, DAC1), Nucleo LEDs and USER button; external devices on any pins: rotary encoder, DS18B20 1-Wire temperature sensors (command line) |
| Display | ILI9486 480×320 TFT with SPI timing, XPT2046 touch, backlight PWM, tone response of the real panel (GUI; `--ideal-colors` to switch off) |
| Serial | Virtual COM port (USART3, `printf` / `stdin_getchar`) |
| Debugging | GDB remote server: breakpoints, single step, watchpoints, registers incl. FPU, memory, `monitor reset halt` |
| USB (Linux) | The Nucleo's ST-LINK/V2-1 over USB/IP: debug probe for pyOCD (the course's *STLink@pyOCD* configurations, *CMSIS Load*) and OpenOCD, and the virtual COM port (`/dev/ttyACM*`). Behind it the debug port: halting debug (DHCSR, DCRSR/DCRDR, DEMCR, vector catch), FPB breakpoints, DWT watchpoints, ROM table and CoreSight IDs |

Not yet: Ethernet, DMA requests of I²C/timers, devices on the I²C header, timer slave modes with internal triggers, the ADC trigger sources other than timer update events.

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
display is a touch, clicking an AUX pad connects it to GND/3V3, the mouse
wheel or a right click on a pad of the Analog header applies a voltage,
firmware files can be dropped onto the window. A right click on the display
(or *Display*, Ctrl+D) shows it alone, as large as the window allows, with
the LEDs, S0–S7 and the USER button in a row below; right click or Esc
returns. *See-through* (Ctrl+T) makes the Nucleo and the display
transparent (slider: opacity), to see the carrier board under them. When a
new build replaces the firmware file, the GUI loads it again and restarts
(not while a debugger is attached; switch off in the ☰ menu, which also
saves the display as a PNG, Ctrl+S). Ctrl+O opens a file, Ctrl+R resets.
On the command line,
`--input 2:PC3=1.65` applies 1.65 V to PC3 (`open` disconnects it).
`--device encoder@PC6,PC7` connects a rotary encoder (contacts to GND,
pull-ups; the TIM8_1/TIM8_2 header pins suit TIM8's encoder mode) and
`--input 3:ENC=5` turns it five detents clockwise (`ENC=-5` back).
`--device ds18b20@PG2` connects a DS18B20 temperature sensor (21 °C,
`--input 4:TEMP=30.5` changes it); several on one pin share the 1-Wire line.

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
`--probe stlink:`, which only finds a USB ST-LINK. On Linux the emulator can
be one ([below](#the-boards-st-link-on-usb-linux)) and they work unchanged;
otherwise the emulator needs its own configuration (`.vscode/launch.json` of
the course project, CDT GDB adapter). It loads the last build of the project that is active in the CMSIS
extension, resets and stops at `main`, like **STLink@pyOCD (launch)** on the
board:

```json
{
    "name": "ITS-Board Emulator",
    "type": "gdbtarget",
    "request": "attach",
    "cwd": "${command:cmsis-csolution.getSolutionDir}",
    "program": "${command:cmsis-csolution.getBinaryFile}",
    "gdb": "arm-none-eabi-gdb",
    "auxiliaryGdb": true,
    "initCommands": [
        "monitor load ${command:cmsis-csolution.getSolutionDir}/${command:cmsis-csolution.getBinaryFile}",
        "maintenance flush register-cache",
        "tbreak main"
    ],
    "customResetCommands": ["monitor reset halt", "maintenance flush register-cache", "tbreak main", "continue"],
    "target": { "port": "3333" }
}
```

The CDT adapter ignores *Run Without Debugging* (Ctrl+F5) and stops at `main`
as with F5. The small extension in `tools/vscode-nodebug` removes the `tbreak`
commands in that case, so the same configuration then just runs the program:

```sh
tools/vscode-nodebug/package.sh
code --install-extension tools/vscode-nodebug/gdbtarget-nodebug.vsix
```

Several debuggers may connect at once: the first one controls the core, the
others (such as the CDT adapter's auxiliary GDB for live watch) read and write
memory and registers while it runs. Breakpoints do not modify memory, so they
also work in flash. Watchpoints slow emulation down while they are set.

## The board's ST-LINK on USB (Linux)

The emulator can also be the Nucleo's on-board ST-LINK/V2-1 on the USB of
your computer, exported over USB/IP and attached with the kernel's
`vhci-hcd` driver. The computer then sees the same USB device as with the
real board plugged in: a debug probe (0483:3752, ST-LINK/V2-1 without mass
storage) and a virtual COM port. The course tools work unchanged — *CMSIS
Load* flashes with pyOCD and the pack's flash algorithm (which runs on the
emulated core and programs flash through the emulated FLASH interface),
*STLink@pyOCD (launch)* debugs through pyOCD's GDB server, and a terminal
program talks to `/dev/ttyACM0`.

**Arch Linux package** ([below](#arch-linux-package),
`packaging/arch/build.sh`): installs the GUI with a desktop entry
(*ITS-Board Emulator* in the app launcher), `its-emu`, and everything the
ST-LINK needs — `usbip`, `vhci-hcd` loaded at boot, a helper that polkit lets
the user at the computer run without a password to attach the probe (only
from 127.0.0.1), and udev access to the probe and its COM port. The package
file can be installed on other Arch machines as it is
(`sudo pacman -U its-emu-*.pkg.tar.zst`).

Started from the launcher, the GUI plugs the ST-LINK in by itself: the
toolbar switch **USB ST-LINK** (remembered in `~/.config/its-emu/gui.conf`,
on by default) and the status bar show its state (*plugged in*, *in use by a
debugger*, or why attaching failed). Quitting or switching it off unplugs
it. After a kernel update the computer needs a restart first: pacman removes
the running kernel's modules, so vhci-hcd cannot be loaded until then (the
installer and the status bar say so). The emulator's own GDB server (port 3333, for the *ITS-Board Emulator*
debug configuration) leaves the port to pyOCD while pyOCD has the ST-LINK in
debug mode and takes it back afterwards, so both debug configurations work
without changing anything.

**Without the package**:

```sh
sudo pacman -S usbip            # Debian: apt install usbip; Ubuntu: linux-tools-generic
sudo modprobe vhci-hcd          # once per boot (or add it to /etc/modules-load.d)

its-emu-gui                     # attaches with `sudo -n usbip attach` (passwordless sudo)
its-emu Program.axf --usb --usb-attach
sudo usbip attach -r 127.0.0.1 -b 1-1    # by hand (its-emu-gui --no-usb-attach, its-emu --usb)
```

`its-emu-gui --no-usb` / `--usb[=ADDR]` override the switch for one run.
Without the packaged udev rule, the rules that pyOCD/OpenOCD install for the
real probe (e.g. `60-openocd.rules`) cover the emulated one too. Other
machines can attach it as well (`--usb 0.0.0.0:3240`, then
`usbip attach -r HOST -b 1-1`; on Windows the usbip-win2 client should do the
same, not tried yet).

While the probe holds the core halted, time goes on for the peripherals and
the flash interface (as on the chip, where only the core and with it SysTick
and DWT->CYCCNT stop), so OpenOCD's register-level flash driver works too.
With the probe plugged in, a BKPT without a debugger is a HardFault as on the
chip (pyOCD leaves the core on its flash algorithm's BKPT when it detaches).

Known differences: there is no mass storage drive (drag-and-drop flashing) and
no SWO trace; the virtual COM port ignores the baud rate the host sets;
programming flash bits that are not erased stores the new value (the chip
ANDs it); a flash store with a wrong size or without PG faults instead of
only setting PGPERR/PGSERR. The ROM table and DBGMCU_IDCODE use ST's values
for the STM32F429 (device 0x419, revision 3); compare with the board using
HwCheck's `rd`.

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

`tools/hw` drives the board through the Nucleo's ST-LINK (`flash.sh`,
`cmd.py`; HwCheck's `rd`/`wr` read and write any register). The board in the
GUI is a vector drawing (`apps/its-emu-gui/assets/board.svg`) made by
`boarddraw.py` after a rectified, focus-stacked photo of the real board
(`hardware_img/board-rect.jpg`, not published); `boardlabelfit.py` and
`boardtraces.py` fit the silkscreen labels and trace the copper under the
mask in that photo, `boardcmp.py` puts a region of drawing and photo side by
side.

```sh
tools/hw/flash.sh tests/fixtures/ac6/HwCheck.axf
python3 tools/hw/cmd.py --wait-ready time cyc exti   # on the board
python3 tools/hw/cmd.py "rd 40013c14"                # EXTI->PR
python3 tools/hw/boardcmp.py 0 0 300 160 /tmp/cmp.png
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
experimental: Unicorn does not build for it natively, so it runs the x64 build
under emulation). It installs `gdb-multiarch` on Linux for the GDB server tests,
records throughput in the job summary and keeps the binaries as artifacts for
14 days. The tests that need the personal lab solutions in `tests/fixtures/ac6`
(not published) skip themselves there.

### Arch Linux package

`packaging/arch/build.sh` builds a pacman package of the current checkout
into `dist/` (version `0.1.0.r<commits>.g<hash>`, so each new commit is an
update; uncommitted changes are included but keep the version). It contains
both programs, a desktop entry with icon, the VS Code extension from
`tools/vscode-nodebug`, built in to Arch's `code` package (and as `.vsix` in
`/usr/share/its-emu` for other VS Code builds), and what the USB ST-LINK
needs ([above](#the-boards-st-link-on-usb-linux)). The emulator writes
nothing to the home directory except the GUI's toolbar setting
(`~/.config/its-emu/gui.conf`).

```sh
packaging/arch/build.sh
sudo pacman -U dist/its-emu-<version>-x86_64.pkg.tar.zst   # install or update
sudo pacman -Rns its-emu                                   # remove
```

### Releases

`.github/workflows/release.yml` runs on a tag `v<version>` (the version in
`Cargo.toml`): it builds the packages and publishes them as a GitHub release
with generated release notes. Started by hand (*Actions → Release → Run
workflow*), it only builds them and keeps them as workflow artifacts. Each
platform is one job that uploads an artifact `package-<platform>`; packages
for further platforms are added as further jobs of that kind.

```sh
git tag v0.1.0
git push origin v0.1.0
```

Test fixtures in `tests/fixtures/ac6` are rebuilt from a checkout of
ITS-BRD-VSC with `tools/build-fixtures.sh` (needs the CMSIS-Toolbox and Arm
Compiler 6 as installed by the course's VS Code setup); only the own test
programs with `tools/build-own.sh [Name]`.

## License

The emulator's source code is licensed under the [MIT license](LICENSE).

The binaries (release packages, CI artifacts, own builds) contain
[Unicorn](https://www.unicorn-engine.org/), linked statically, which is
licensed under the GPL-2.0; the binaries as a whole are therefore distributed
under the terms of the GPL-2.0. The Arch package carries both license texts
(`/usr/share/licenses/its-emu`). The other Rust dependencies use permissive
licenses (MIT, Apache-2.0, BSD, Zlib, Unicode).

The firmware images in `tests/fixtures/ac6` and the font table
(`crates/itsboard/assets/fonts.bin`) are built from course material of the
[ITS-BRD-VSC](https://github.com/Transport-Protocol/ITS-BRD-VSC) project under
the BSD 2-Clause license (Copyright (c) 2022, HAW Hamburg, Labor für
technische Informatik), with parts of the STM32CubeF4 HAL (STMicroelectronics,
BSD 3-Clause) and CMSIS (Arm Limited, Apache-2.0); see the README files there.

All product names and trademarks belong to their owners. Their use does not
imply any affiliation with or endorsement by them (see the disclaimer at the
top).
