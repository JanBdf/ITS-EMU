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
| Clock | RCC clock tree, virtual time in CPU cycles (16 MHz HSI → 180 MHz PLL) |
| Peripherals | GPIO A–K, TIM2–5, SPI1, USART3, PWR, flash interface, bit-banding |
| Board | LEDs OUT0–15, buttons S0–S7, INT0–5, Nucleo LEDs and USER button |
| Display | ILI9486 480×320 TFT, XPT2046 touch, backlight PWM |
| Serial | Virtual COM port (USART3, `printf` / `stdin_getchar`) |

Not yet: Ethernet, DMA, ADC/DAC, I²C, further timers and UARTs, GDB debugging.

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

## Building

Rust (stable) and a C toolchain with CMake (for Unicorn). On Linux the usual
X11/Wayland development libraries are needed for the GUI.

```sh
cargo build --release
cargo test --release
```

Test fixtures in `tests/fixtures/ac6` are rebuilt from a checkout of
ITS-BRD-VSC with `tools/build-fixtures.sh` (needs the CMSIS-Toolbox and Arm
Compiler 6 as installed by the course's VS Code setup).

## License

MIT for the emulator. The firmware images in `tests/fixtures/ac6` and the font
table are built from course material under the BSD 2-Clause license
(HAW Hamburg, Labor für technische Informatik); see the README files there.
