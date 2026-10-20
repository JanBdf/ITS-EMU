# ITS-Board Emulator — Status and next steps

Emulator for the HAW Hamburg ITS-Board (NUCLEO-F429ZI, ITS carrier board,
Waveshare 4" TFT) that runs unmodified AC6 firmware. Rust + Unicorn, one
process; see README.md for what is emulated and how to use it.

## Architecture

```
apps/its-emu (CLI)        apps/its-emu-gui (egui)
        \                     /
         stlink     — ST-LINK/V2-1 (debug commands, virtual COM port) on USB/IP
         usbip      — USB/IP server for one emulated device (vhci-hcd attaches it)
         itsboard   — wiring, ILI9486, XPT2046, LEDs/buttons, fonts.bin
              |
         stm32f429  — memory map, RCC, PWR, FLASH, GPIO, EXTI/SYSCFG, TIM1–14, ADC/DAC,
                      I2C1–3, SPI1–6, U(S)ART1–8, DMA1/2, IWDG, CRC, RNG
              |
         emu-core   — Unicorn CPU, NVIC/SCB/SysTick, exceptions, cycle model,
                      virtual clock, idle skipping, ELF loader, GDB server,
                      debug architecture (halting debug, FPB, DWT, ROM table),
                      SW-DP/AHB-AP
```

## Validation

Every peripheral model is checked against the real board with the HwCheck
test program (`tests/fixtures/ac6-src/HwCheck`, commands over the virtual COM
port; `rd`/`wr` for ad-hoc register probes). Board results live in
`tests/hardware`, `crates/itsboard/tests/hardware.rs` compares.

## Next steps

- USB probe: compare with the board — ROM table/component IDs, FP_CTRL,
  DWT_CTRL and DBGMCU_IDCODE with HwCheck `rd`; a pyOCD session with
  `pyocd load -vv` and usbmon against the real ST-LINK (exact command
  sequence, timing of erase/program). Mass storage (drag-and-drop .bin) and
  SWO are not there; the flash AND semantics and PGSERR without a fault
  (see `stm32f429::flash`) neither

- Timers: slave modes with internal triggers (ITR: timer chaining) or ETR,
  complementary outputs and dead time of TIM1/TIM8. On the board, UG while
  the counter is stopped does not change CNT until it runs again (the
  emulator clears it at once)
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
- Interrupts pended by a peripheral write mid-block (STIR, SWIER): taken
  at the end of the block, 35 cycles late in `irqlat` (26 vs 61). Taking
  them right after the store needs `emu_stop` from the MMIO callback, but
  Unicorn then restores the PC to the store itself (`cpu_loop_exit_restore`
  in `helper_check_exit_request`), so the store runs twice; skipping it by
  hand breaks for writeback stores, STM and IT blocks (no exit check there)
- Speed: Unicorn is vendored with patches (`vendor/README.md`). What is
  left in busy programs: the per-block hook (~20–30 %), the lookup of the
  next block after indirect branches (function returns, up to 25 %), the MMIO
  path through QEMU's memory API (~20 % while polling a peripheral) and, at
  every peripheral event, a sweep over all ~55 peripherals (`update`,
  `next_event`; ~15 % while the display is drawn). A per-peripheral cache of
  the next event would cut the sweeps, but `update` re-asserts interrupt
  levels (which re-pends them) and some `next_event`s read shared state, so
  it needs an audit of every model. The CPU stays on one thread (one
  deterministic, cycle-counted core); the GUI renders the display on its own
  thread. Idle detection tracks the RAM pages a loop stores to through
  Unicorn's first-write path instead of copying RAM. `tools/perf/bench.sh` times the fixtures, `tools/perf/same-cycles.sh`
  checks that a change leaves every fixture's cycles and output unchanged.
  Busy-wait delay loops (`for (i = 0; i < N; i++);`) are the common case
  that idle detection cannot skip (the counter changes): they run at about
  1.5-2x real time, one block hook per iteration. Skipping them would need
  the loop's exit condition. A sketch on top of idle detection: if two
  consecutive visits of the loop head differ only in one register or RAM word
  by a constant step (everything else equal, clean period, constant period),
  and the loop's conditional branch compares that value with a constant or an
  unchanged register (decoded by `timing`), the number of iterations left
  follows; skip all but the last, as idle skipping does, up to the slice end.
  `--no-idle-skip` benchmarks are sensitive to code layout (±10 % with
  identical instruction counts): compare `perf stat` instruction counts
- Packages use the `dist` profile (fat LTO): 15 MB GUI / 3.4 MB CLI
- Release builds for students (cargo-dist)
