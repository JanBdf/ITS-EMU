# Usage

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
  at once, show the virtual time at which each line began (*Time*), and
  copy or save its text.
- Ctrl + mouse wheel zooms the board at the pointer; the middle button or
  Shift + drag pans it; Home fits it to the window again.
- The buttons S0–S7, USER and the contacts of a rotary encoder bounce for a
  moment when they change, as on the real board: a program that does not
  debounce sees several edges per press (*Emulation → Contact bounce of the
  buttons* switches it off). Speed, theme (*View → Theme*), window size and
  position and the console's options are remembered in
  `~/.config/its-emu/gui.conf` (the platform's config directory elsewhere).
- *Devices* (Ctrl+E) connects devices on the header pins while the board
  runs: a [Pico](#raspberry-pi-pico), a rotary encoder (buttons for 1 or 10
  detents, or scroll over the knob), DS18B20 sensors and the
  [network](#network). They persist across firmware loads and restarts.

**Command line.** `its-emu Program.axf` runs 10 s of virtual time as fast
as possible (`--seconds`, `--realtime`), prints the virtual COM port's
output and ends with the stop reason, PC, cycles and LEDs. The exit status
is 0 at the time limit, 2 at a `BKPT` stop (`--continue-on-bkpt` steps over
`BKPT #n`, n ≠ 0), 3 on lockup, 1 on errors. `--json` adds one line of JSON
with the result (stop reason, pc and symbol, virtual and wall time, cycles,
LEDs, backlight, core clock) for scripts; `--screenshot FILE` saves the
display (`--as-seen`: the real panel's look); `--bounce` lets the buttons
bounce as in the GUI (off by default: scripted inputs are clean);
`--stdin` forwards the terminal's input to the COM port.

`--input` scripts inputs as comma-separated `<seconds>:<input>=<value>`:
`S0`..`S7`, `USER` (1/0), `TOUCH` (`x;y` or `0`), `SERIAL` (text, `\n` =
newline), a pin voltage, `ENC`, `TEMP`, `PICO`:

| Option | Effect |
|---|---|
| `--input 2:PC3=1.65` | 1.65 V on PC3 at 2 s (`open` disconnects) |
| `--device encoder@PF0,PF1` | rotary encoder (contacts to GND, pull-ups), A on IN0, B on IN1 as in task 2; `encoder@PC6,PC7` for TIM8's encoder mode |
| `--input 3:ENC=5` | turn it 5 detents clockwise (`-5` back) |
| `--device ds18b20@PG2` | DS18B20 at 21 °C; several on one pin share the line |
| `--device lm75@0x48` | LM75 on I2C1 (SCL PB8, SDA PB9; 0x48–0x4F) |
| `--input 4:TEMP=30.5` | set a sensor's temperature; `TEMP1` is the second sensor given |

## Network

`--net` plugs a virtual network into the Ethernet jack: gateway 192.168.33.1
(ARP, ping) and a DHCP server handing out 192.168.33.99 (the course's Stack
demo addresses; its fixed address works too). `--forward tcp:8080=80`
(repeatable, also `udp:`) forwards localhost:8080 to port 80 of the board; it
implies `--net` and real time. In the GUI the network is a device (`--device
net@tcp:8080=80,udp:5000`). No NAT: the board reaches no other hosts.

```sh
its-emu NetEcho.axf --forward tcp:7007=7   # then: nc localhost 7007
```

## Raspberry Pi Pico

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

## Platform notes

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

