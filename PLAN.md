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

- Timers: slave modes with ETR; of TIM1/TIM8 the lock levels (complementary
  outputs, dead time, MOE/AOE, the off states, the break input BKIN/BG and
  the COM event with CCPC/CCUS exist, not compared with the board yet; the
  break input is polled at the sweeps, not at its edge). Timer chaining (ITR0..3; TRGO = reset, enable, update) has
  not been compared with the board yet: record HwCheck `timchain` (delay
  of a TRGO to the slave, gated count, see `timer_chaining_matches_the_board`).
  TRGO from the CC1 compare pulse and OC1REF..OC4REF (MMS >= 3) triggers
  slave timers (TRGO times rounded like an overflow's, unmeasured), but not
  the ADCs/DAC; the TIM10/11/13/14 OC1 inputs of TIM9/TIM12 are missing. On the board, UG while
  the counter is stopped does not change CNT until it runs again (the
  emulator clears it at once)
- DMA: bursts and the FIFO threshold (the FIFO packs, but memory is written
  item by item at once); transfers take no time. Ethernet: the virtual network (`itsboard::net`, smoltcp)
  has no NAT to the computer's network, and IPv6 frames go nowhere. I2C with DMA keeps
  the CPU's byte timing: BTF is set after each byte although the DMA keeps
  DR filled on the board (the HAL ignores BTF while DMAEN is set).
  Timer DMA is not compared with the board yet: record HwCheck `timdma`
  (whether a request made while the stream is off is transferred when it is
  enabled: the emulator keeps it, line `T off`)
- External devices: more I2C devices (EEPROM, IMU, ...) via `I2cDevice`
  (the LM75 exists, `itsboard::lm75`). The course wires the encoder to
  IN0/IN1 (PF0 = A, PF1 = B, GS task 2; the GUI's default); which pin it
  uses for the DS18B20 is still open
- ADC/DAC triggers from the CCx events and EXTI11/EXTI15 (ADC) and EXTI9
  (DAC), and TRGO from compare pulses/OCxREF (MMS >= 3): which signal the
  ADC sees (OCxREF edges or compare pulses; the EXTI line before or after
  RTSR/FTSR/IMR) is to be measured on the board first. The timer TRGO
  triggers exist for both groups (update event, counter enable; UG with
  MMS = 000 triggers nothing, HwCheck `dacwave`)
- Pico: SPI and I2C only store their registers. DMA: no sniffer
  (CRC), no pacing by PWM/SPI/I2C/XIP DREQs, one item per cycle per channel
  (channels do not share the bus), writes to RAM do not drop translated
  code. PWM: not the DIVMODE inputs, PH_ADV/PH_RET, DREQs; ADC: the inputs see
  only digital levels from the board: `Pico::analog` sets voltages), flash
  program/erase commands are ignored (both noted to the user once). A core
  writing flash or RAM drops only its own translations: code one core
  writes and the other one has already run stays stale there
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
  Counting delay loops (`for (i = 0; i < N; i++);`, `Driver_Delay_us`) are
  skipped too (`machine/delay.rs`): a small interpreter for affine Thumb code
  (add/sub/mov/cmp, word loads and stores at fixed addresses, one conditional
  branch) runs the loop body, is checked against Unicorn on consecutive head
  visits with the same period, and solves for the exit iteration from the
  branch operands; all but the last iteration are skipped, up to the slice
  end. Off with breakpoints, watchpoints or halting debug.
  (`tests/fixtures/ac6-src/DelayLoops`: 45x faster, same cycles.)
  `--no-idle-skip` benchmarks are sensitive to code layout (±10 % with
  identical instruction counts): compare `perf stat` instruction counts
- Packages use the `dist` profile (fat LTO): 15 MB GUI / 3.4 MB CLI
- Release builds for students (cargo-dist)
