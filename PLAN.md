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
         stm32f429  — memory map, RCC, PWR, FLASH, GPIO, EXTI/SYSCFG, TIM1–14, ADC/DAC,
                      I2C1–3, SPI1–6, U(S)ART1–8, DMA1/2, IWDG, CRC, RNG
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

- Timer outputs on pins (also as capture input: on the board, CH1 toggling
  PC6 is captured by CH2 12 timer clocks after the match), down/center-aligned
  counting, slave modes
- DMA requests of SPI, I2C, DAC and timers (the controllers and the
  ADC and U(S)ART requests exist), DMA FIFO/bursts; then Ethernet (`Programs/Stack`)
- External devices on the headers (I2C sensor via `I2cDevice`, 1-Wire
  sensor, rotary encoder) and in the GUI
- DAC hardware triggers (TIM6/TIM7 TRGO ...), ADC injected-group triggers and
  the CCx/EXTI11 trigger sources (the regular group starts on TIM2/TIM3/TIM8
  TRGO = update event)
- Time within a block: DWT->CYCCNT reads are placed at their instruction, but
  peripheral accesses see the end of their translated block, and interrupts
  pended by a write are taken only at the end of the block (`irqlat` without
  masking: the emulator is late by the rest of the block)
- Speed: most time goes to Unicorn's per-block hook and MMIO callbacks
  (measured; the models themselves are cheap)
- Release builds for students (cargo-dist)
