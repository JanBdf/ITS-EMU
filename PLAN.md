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

- Timers: down/center-aligned counting, slave modes other than the encoder
  modes, complementary outputs and dead time of TIM1/TIM8
- DMA requests of I2C and timers (the controllers and the ADC, U(S)ART, SPI
  and DAC requests exist), DMA FIFO/bursts; then Ethernet (`Programs/Stack`)
- External devices: an I2C sensor via `I2cDevice`; the devices in the GUI
  (rotary encoder and DS18B20 exist on the command line, `itsboard::devices`;
  which header pins the course uses for them is still open)
- ADC injected-group triggers, the CCx/EXTI11 (ADC) and EXTI9 (DAC) trigger
  sources; TRGO other than the update event (the ADC regular group starts on
  TIM2/TIM3/TIM8 TRGO, the DAC on TIM2/4/5/6/7/8 TRGO)
- Time within a block: DWT->CYCCNT reads are placed at their instruction, but
  peripheral accesses see the end of their translated block, and interrupts
  pended by a write are taken only at the end of the block (`irqlat` without
  masking: the emulator is late by the rest of the block)
- Speed: most time goes to Unicorn's per-block hook and MMIO callbacks
  (measured; the models themselves are cheap)
- Release builds for students (cargo-dist)
