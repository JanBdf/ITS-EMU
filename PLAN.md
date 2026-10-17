# ITS-Board Emulator — Status and next steps

Emulator for the HAW Hamburg ITS-Board (NUCLEO-F429ZI, ITS carrier board,
Waveshare 4" TFT) that runs unmodified AC6 firmware. Rust + Unicorn, one
process; see README.md for what is emulated and how to use it.

## Architecture

```
apps/its-emu (CLI)        apps/its-emu-gui (egui)
        \                     /
         itsboard   — wiring, ILI9486, XPT2046, LEDs/buttons, fonts.bin
              |
         stm32f429  — memory map, RCC, PWR, FLASH, GPIO, EXTI/SYSCFG, TIM1–14, SPI1, USART3
              |
         emu-core   — Unicorn CPU, NVIC/SCB/SysTick, exceptions, cycle model,
                      virtual clock, idle skipping, ELF loader, GDB server
```

## Validation

Every peripheral model is checked against the real board with the HwCheck
test program (`tests/fixtures/ac6-src/HwCheck`, commands over the virtual COM
port; `rd`/`wr` for ad-hoc register probes). Board results live in
`tests/hardware`, `crates/itsboard/tests/hardware.rs` compares.

## Next steps

- ADC1–3 and DAC (Analog header), with analog inputs in the GUI
- I2C1 (I2C header; NACK with nothing connected), SPI2–6, further U(S)ARTs
- Timer input capture and down/center-aligned counting
- DMA, then Ethernet (`Programs/Stack`)
- External devices on the headers (e.g. 1-Wire sensor, rotary encoder)
- Release builds for students (cargo-dist)
