# ITS-Board Emulator

Emulator for the **HAW Hamburg ITS-Board** (NUCLEO-F429ZI with the ITS carrier
board and the Waveshare 4" TFT touch shield). It runs the unmodified `.axf`
images that the course toolchain ([ITS-BRD-VSC](https://github.com/Transport-Protocol/ITS-BRD-VSC),
Arm Compiler 6) builds, including the HAL, ITS_BRD_LIB and the display driver.

Status: work in progress — see [PLAN.md](PLAN.md).

> **Disclaimer:** An independent, private project, **not affiliated with,
> endorsed by or supported by** the HAW Hamburg, its Labor für technische
> Informatik or the ITS-BRD-VSC project; their names only describe what the
> emulator is compatible with. Results may differ from the hardware, and
> course or exam requirements always refer to the real board. Please do not
> contact the course staff about this emulator.

## Installation

Packages are on the [Releases](https://github.com/JanBdf/ITS-EMU/releases) page:

| Platform | Package | Install |
|---|---|---|
| Arch Linux (x86_64) | `its-emu-<version>-x86_64.pkg.tar.zst` | `sudo pacman -U its-emu-*.pkg.tar.zst` |
| macOS (Apple silicon / Intel) | `its-emu-<version>-macos-arm64.dmg` / `-x86_64.dmg` | drag *ITS-Board Emulator* to Applications |

The macOS app is signed ad hoc, so macOS refuses the first start; allow it
once in *System Settings → Privacy & Security → Open Anyway*. The CLI is
inside the app:
`ln -s '/Applications/ITS-Board Emulator.app/Contents/MacOS/its-emu' /usr/local/bin/`.
The USB ST-LINK is Linux only; on macOS use the [GDB server](#debugging-with-gdb).

Other platforms: [build from source](#building), or take the Linux, Windows
and macOS binaries the CI keeps as workflow artifacts for 14 days.

## What is emulated

| | |
|---|---|
| CPU | Cortex-M4F (Thumb-2, FPU) via [Unicorn](https://www.unicorn-engine.org/); NVIC, SysTick, exceptions and faults in Rust |
| Clock | RCC clock tree (16 MHz HSI → 180 MHz PLL); virtual time in CPU cycles with a Cortex-M4 cycle model (loads, branches, divisions, exceptions, APB wait states) measured on the board |
| Peripherals | GPIO A–K, EXTI + SYSCFG, TIM1–14 (up/down/center-aligned, output compare/PWM on the pins, input capture, encoder mode, slave modes reset/gated/trigger/external clock from TI1/TI2 or another timer's TRGO, TIM1/8 repetition counter, complementary outputs with dead time, break, COM), ADC1–3 (regular and injected, timer TRGO triggers), DAC (timer triggers, noise/triangle), I²C1–3 (master; optional LM75, other addresses NACK), SPI1–6, USART1–6 + UART4/5/7/8 (USART3 connected), IWDG, CRC, RNG, DMA1/2 (direct/FIFO with packing, double buffer, all peripheral requests, timer bursts), Ethernet MAC + LAN8742A PHY, PWR, flash interface (program/erase with datasheet timing), bit-banding |
| Board | LEDs OUT0–15, buttons S0–S7, INT0–5, Analog header (pin voltages, DAC1), Nucleo LEDs and USER button; on any pins: rotary encoder, DS18B20 sensors, a Raspberry Pi Pico running its own firmware; a virtual network on the Ethernet jack |
| Pico | RP2040 with both M0+ cores and the real boot ROM, firmware from UF2: clocks, resets, timer, SIO, PIO0/1, PWM, ADC, DMA, RTC, XIP flash, USB CDC serial as a pseudo terminal. Own thread |
| Display | ILI9486 480×320 TFT with SPI timing, XPT2046 touch, backlight PWM, the real panel's tone response (`--ideal-colors` turns it off) |
| Serial | Virtual COM port (USART3, `printf` / `stdin_getchar`) |
| Debugging | GDB server: breakpoints, single step, watchpoints, registers incl. FPU, memory, `monitor reset halt` |
| USB (Linux) | The Nucleo's ST-LINK/V2-1 over USB/IP for pyOCD and OpenOCD, plus its virtual COM port; behind it halting debug, FPB, DWT, ROM table and CoreSight IDs |

Not yet: I²C devices other than the LM75, timer slave modes with ETR, ADC
triggers from CCx and EXTI.

## Usage

```sh
# graphical board
cargo run --release -p its-emu-gui -- path/to/Program.axf

# headless: 10 s of virtual time, press S7 at 2.5 s, send a serial line,
# save the display
cargo run --release -p its-emu -- path/to/Program.axf --seconds 10 \
    --input "2.5:S7=1,2.6:S7=0,3:SERIAL=hello\n" --screenshot screen.png
```

**GUI.** F1 lists all controls. The main ones:

- Keys `0`–`7` press S0–S7; right click latches a button. Clicking the
  display touches it; clicking an AUX pad ties it to GND/3V3; mouse wheel or
  right click on an Analog header pad sets a voltage.
- Drop a firmware file onto the window or click the empty display; Ctrl+O
  opens one (*File* lists recent files and saves the display as PNG, Ctrl+S).
  A rebuilt firmware file is reloaded automatically unless a debugger is
  attached (*Emulation* menu).
- Space runs/pauses, Ctrl+R resets.
- *Display* (Ctrl+D, or right click on it) shows the display alone with the
  LEDs and buttons below; *See-through* (Ctrl+T) makes Nucleo and display
  transparent; *Pins* (Ctrl+P) shows mode, level and wiring of ports A–G.
- The serial console (Ctrl+L) can echo, choose the line end, send each key
  at once, and copy or save its text.
- *Devices* (Ctrl+E) connects devices on the header pins while the board
  runs: a [Pico](#raspberry-pi-pico), a rotary encoder, DS18B20 sensors and
  the [network](#network). They persist across firmware loads and restarts.

**Command line devices and inputs:**

| Option | Effect |
|---|---|
| `--input 2:PC3=1.65` | 1.65 V on PC3 at 2 s (`open` disconnects) |
| `--device encoder@PF0,PF1` | rotary encoder (contacts to GND, pull-ups), A on IN0, B on IN1 as in task 2; `encoder@PC6,PC7` for TIM8's encoder mode |
| `--input 3:ENC=5` | turn it 5 detents clockwise (`-5` back) |
| `--device ds18b20@PG2` | DS18B20 at 21 °C; several on one pin share the line |
| `--device lm75@0x48` | LM75 on I2C1 (SCL PB8, SDA PB9; 0x48–0x4F) |
| `--input 4:TEMP=30.5` | set a sensor's temperature; `TEMP1` is the second sensor given |

### Network

`--net` plugs a virtual network into the Ethernet jack: gateway 192.168.33.1
(ARP, ping) and a DHCP server handing out 192.168.33.99 (the course's Stack
demo addresses; its fixed address works too). `--forward tcp:8080=80`
(repeatable, also `udp:`) forwards localhost:8080 to port 80 of the board; it
implies `--net` and real time. In the GUI the network is a device (`--device
net@tcp:8080=80,udp:5000`). No NAT: the board reaches no other hosts.

```sh
its-emu NetEcho.axf --forward tcp:7007=7   # then: nc localhost 7007
```

### Raspberry Pi Pico

`--device pico=FIRMWARE.uf2@GP0=PF0,GP5=PF1` (or the Devices panel) runs a
Pico and wires its GPIOs to board pins. Without `@...` the wiring is that of
the Drehgeber exercise's square wave generator (GP0 → PF0, GP5 → PF1). In the
GUI, `--device` replaces the saved devices for that run.

The Pico's USB serial port is a pseudo terminal whose path is printed at
start (`Pico 0: serial port /dev/ttys004`); `RUST_LOG=itsboard=info` logs its
output, `--input 1:PICO=s3600\r\n` writes to it. Programs that find the Pico
by its USB IDs, such as the generator's Python GUI, work when started via
*Python program…* in the Devices panel (needs pyserial) or
`tools/pico/run_gui.py`:

```sh
its-emu-gui Drehgeber.axf --device pico=pio_2_squarewaves.uf2
python3 tools/pico/run_gui.py /dev/ttys004 Rechteckgenerator/Python_GUI/main.py
```

- **Timing.** The Pico has its own thread and clock, at most 20 ms (virtual)
  ahead of the board; pin changes reach the board at the time they were made.
  It runs at about 0.7–0.8× real time while busy-waiting, and so does the
  board with it. While the board is paused the Pico runs in real time.
- **Boot ROM.** Not shipped (RP2040-only license): the first start downloads
  it from the [Raspberry Pi release](https://github.com/raspberrypi/pico-bootrom-rp2040/releases)
  into the cache directory; `--pico-bootrom FILE` or *Choose…* uses a local copy.
- **Peripherals.** PWM drives its pins (free-running, phase-correct, wrap
  IRQs; no B-pin input modes). The ADC reads the temperature sensor (27 °C)
  and GPIO 26–29 as 0 or 3.3 V. DMA feeds PIO, ADC and UART, paced by FIFOs,
  PWM wraps or timers, with chaining, rings and control blocks. The RTC
  counts from clk_rtc (alarm via `rtc_set_alarm`).
- **Missing.** SPI and I²C only store their registers; flash program/erase
  is ignored. The first use of either warns in the Devices panel and log.
- **Windows** has no pseudo terminals; the panel's terminal and `--input
  PICO=...` still work.

`cargo test -p rp2040 --test blink` and the first test of `--test pico` (in
`crates/itsboard`) need no third-party files (stand-in boot ROM and
`rp2040::testfw`). `tests/fixtures/ac6-src/PicoQuadrature` decodes the
generator's signals on PF0/PF1 with EXTI and shows angle, speed, direction
and errors; `ITS_EMU_PICO_UF2=pio_2_squarewaves.uf2 cargo test -p itsboard
--test pico` runs it. Commands sent before *PicoQuadrature ready* (~2 s of
library init) are lost, and connecting the generator (`h`) counts two
backward steps.

The font table ITS_BRD_LIB expects at `0x0810_0000` is built in.

### Platform notes

- **File dialog.** Linux uses the XDG desktop portal with a FileChooser
  backend (`xdg-desktop-portal-gtk`, `-kde`, `-gnome`; not the Hyprland or
  wlroots portals) or `zenity`; without either, the GUI's own browser
  (Shift+click *Open…* or `ITS_EMU_FILE_DIALOG=builtin` forces it). Drag and
  drop does not work on Wayland (winit).
- **Graphics.** OpenGL on Linux; wgpu (DirectX 12, Metal) on Windows and
  macOS, falling back to OpenGL without an adapter (VMs, remote desktops).
  `--renderer gl|wgpu` picks one without fallback.
- **Windows.** The release GUI opens no console; from a terminal it logs there.
- **macOS CI binaries** are unsigned: `chmod +x its-emu its-emu-gui` (the zip
  loses the bit) and `xattr -d com.apple.quarantine its-emu its-emu-gui`.
- **Log.** Warnings and errors by default; `RUST_LOG=info` or `debug` for more.

## Debugging with GDB

The GUI serves GDB on `127.0.0.1:3333` (`--gdb ADDR`, `--no-gdb`); the CLI
waits for a debugger with `--gdb [ADDR]`. Connecting halts the core,
detaching resumes it. Without a firmware file both start with empty flash
and the debugger loads the program:

```sh
arm-none-eabi-gdb Program.axf -ex "target remote :3333"
(gdb) monitor load Program.axf
(gdb) tbreak main
(gdb) continue
```

`monitor load FILE` flashes an `.axf` (path relative to the emulator's
working directory) by its segments like pyOCD, resets and stays halted; the
GUI then shows its name and symbols. Do not use GDB's own `load` for course
programs: it writes sections at their run addresses, so AC6's `.data` initial
values (`RW_IRAM1`) land in RAM instead of flash, where startup copies them from.

The course's VS Code configurations start pyOCD with `--probe stlink:`,
which only finds a USB ST-LINK. On Linux the emulator can be one
([below](#the-boards-st-link-on-usb-linux)); otherwise add this to the
course project's `.vscode/launch.json` (CDT GDB adapter). Like
**STLink@pyOCD (launch)**, it loads the active CMSIS project's last build,
resets and stops at `main`:

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

The CDT adapter treats *Run Without Debugging* (Ctrl+F5) like F5. The
extension in [`tools/vscode-nodebug`](tools/vscode-nodebug/README.md) drops
the `tbreak`s in that case, so the program just runs:

```sh
tools/vscode-nodebug/package.sh
code --install-extension tools/vscode-nodebug/gdbtarget-nodebug.vsix
```

Several debuggers may connect: the first controls the core, the others (e.g.
the CDT adapter's auxiliary GDB for live watch) access memory and registers
while it runs. Breakpoints do not modify memory, so they work in flash.
Watchpoints slow emulation while set.

## The board's ST-LINK on USB (Linux)

The emulator can be the Nucleo's ST-LINK/V2-1 (0483:3752, without mass
storage) on your computer's USB, exported over USB/IP and attached with the
kernel's `vhci-hcd`. The course tools then work unchanged: *CMSIS Load*
flashes with pyOCD and the pack's flash algorithm (running on the emulated
core), *STLink@pyOCD (launch)* debugs, and a terminal talks to `/dev/ttyACM0`.

With the [Arch package](#arch-linux-package), the GUI plugs the ST-LINK in
by itself. The toolbar switch **USB ST-LINK** (on by default, remembered in
`~/.config/its-emu/gui.conf`) and the status bar show its state; quitting or
switching off unplugs it. After a kernel update, reboot first: pacman removes
the running kernel's modules, so vhci-hcd cannot load until then. The
emulator's own GDB server yields port 3333 to pyOCD while pyOCD debugs and
takes it back afterwards, so both debug configurations work side by side.

Without the package:

```sh
sudo pacman -S usbip            # Debian: apt install usbip; Ubuntu: linux-tools-generic
sudo modprobe vhci-hcd          # once per boot (or /etc/modules-load.d)

its-emu-gui                     # attaches with `sudo -n usbip attach` (passwordless sudo)
its-emu Program.axf --usb --usb-attach
sudo usbip attach -r 127.0.0.1 -b 1-1    # by hand (its-emu-gui --no-usb-attach, its-emu --usb)
```

`its-emu-gui --no-usb` / `--usb[=ADDR]` override the switch for one run. The
udev rules pyOCD/OpenOCD install for the real probe (e.g. `60-openocd.rules`)
cover the emulated one. Other machines can attach it with `--usb
0.0.0.0:3240` and `usbip attach -r HOST -b 1-1` (usbip-win2 on Windows,
untested).

While the probe halts the core, peripherals and the flash interface keep
running (only the core, SysTick and DWT->CYCCNT stop, as on the chip), so
OpenOCD's register-level flash driver works. With the probe plugged in, a
BKPT without a debugger is a HardFault, as on the chip.

Known differences: no mass storage drive and no SWO; the COM port ignores
the baud rate; programming non-erased flash bits stores the new value (the
chip ANDs); a flash store with a wrong size or without PG faults instead of
only setting PGPERR/PGSERR. ROM table and DBGMCU_IDCODE use ST's STM32F429
values (device 0x419, revision 3).

## Comparison with the real board

`tests/fixtures/ac6-src/HwCheck` takes commands over the virtual COM port
(colour bars, fonts, LED patterns, backlight, DWT->CYCCNT timing of library
calls and instruction kernels, pin levels, peripheral probes). The same
commands run on the board and in the emulator;
`crates/itsboard/tests/hardware.rs` compares against the board results in
[`tests/hardware`](tests/hardware/README.md).

| | board | emulator |
|---|---|---|
| `GUI_clear` (SPI bound) | 39 325 428 TIM2 ticks | 39 325 157 |
| `GUI_disStr`, Font24, 20 characters | 2 456 720 | 2 445 960 (−0.4 %) |
| C delay loop / assembler loop, 100 000 iterations | 800 346 / 150 198 | 800 336 / 150 181 |
| filled circle, radius 100 (point by point) | 133.3 M | 124.7 M (−6.5 %) |
| input levels of ports A–I (no pull / pull-down / pull-up) | | identical |

`tools/hw` drives the board through its ST-LINK (`flash.sh`, `cmd.py`;
HwCheck's `rd`/`wr` access any register). The GUI's board drawings
(`apps/its-emu-gui/assets/board-{carrier,nucleo,shield}.svg`) are made by
`boarddraw.py` from rectified photos of the real board (not published);
`boardlabelfit.py` and `boardtraces.py` fit the silkscreen labels and copper
traces, `boardcmp.py` shows a region of drawing and photo side by side.

```sh
tools/hw/flash.sh tests/fixtures/ac6/HwCheck.axf
python3 tools/hw/cmd.py --wait-ready time cyc exti   # on the board
python3 tools/hw/cmd.py "rd 40013c14"                # EXTI->PR
python3 tools/hw/boardcmp.py 0 0 300 160 /tmp/cmp.png
```

## Building

Needs Rust (stable), a C toolchain, CMake and libclang (for Unicorn). The
GUI loads the X11/Wayland libraries at run time.

```sh
cargo build --release
cargo test --release
```

CI (`.github/workflows/ci.yml`, on pushes to `main`/`experimental` and PRs)
checks formatting and clippy and tests on Linux, Windows and macOS, x64 and
arm64 (Windows on Arm runs the x64 build under emulation, as Unicorn does
not build there). It records throughput in the job summary and keeps
binaries and macOS disk images for 14 days. Tests that need the unpublished
lab solutions in `tests/fixtures/ac6` skip themselves.

Fixtures in `tests/fixtures/ac6` are rebuilt from an ITS-BRD-VSC checkout
with `tools/build-fixtures.sh` (CMSIS-Toolbox and AC6 from the course's VS
Code setup); `tools/build-own.sh [Name]` rebuilds only the own test programs.

### Arch Linux package

`packaging/arch/build.sh` builds a pacman package of the checkout into
`dist/` (version `0.1.0.r<commits>.g<hash>`; uncommitted changes are
included without changing the version). It contains both programs, a desktop
entry, the `vscode-nodebug` extension (built into Arch's `code`, and as
`.vsix` in `/usr/share/its-emu`), and what the USB ST-LINK needs: `usbip`,
`vhci-hcd` loaded at boot, a polkit-allowed helper that attaches the probe
from 127.0.0.1 without a password, and udev rules for the probe and its COM
port.

```sh
packaging/arch/build.sh
sudo pacman -U dist/its-emu-<version>-x86_64.pkg.tar.zst   # install or update
sudo pacman -Rns its-emu                                   # remove
```

### macOS app

`packaging/macos/build.sh` builds *ITS-Board Emulator.app* for the host
architecture and `dist/its-emu-<version>-macos-<arch>.dmg` (needs
`rsvg-convert`: `brew install librsvg`). The app holds both programs, the
VS Code extension and the licenses; it is signed ad hoc (`codesign -s -`).

The emulator writes nothing to the home directory except the GUI's
`~/.config/its-emu/gui.conf`.

### Releases

`.github/workflows/release.yml` runs on a tag `v<version>` (matching
`Cargo.toml`), builds the packages and publishes a GitHub release. Run by
hand (*Actions → Release → Run workflow*) it only keeps them as artifacts.
Each platform is one job uploading `package-<platform>`.

```sh
git tag v0.1.0
git push origin v0.1.0
```

## License

The source code is [MIT](LICENSE) licensed.

The binaries link [Unicorn](https://www.unicorn-engine.org/) statically
(GPL-2.0), so they are distributed as a whole under the GPL-2.0; the Arch
package carries both texts (`/usr/share/licenses/its-emu`). Other Rust
dependencies are permissive (MIT, Apache-2.0, BSD, Zlib, Unicode).

The firmware in `tests/fixtures/ac6` and `crates/itsboard/assets/fonts.bin`
are built from [ITS-BRD-VSC](https://github.com/Transport-Protocol/ITS-BRD-VSC)
course material (BSD 2-Clause, Copyright (c) 2022, HAW Hamburg, Labor für
technische Informatik) with parts of the STM32CubeF4 HAL (STMicroelectronics,
BSD 3-Clause) and CMSIS (Arm Limited, Apache-2.0); see the READMEs there.

All product names and trademarks belong to their owners; their use implies
no affiliation or endorsement.
