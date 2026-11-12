# ITS-Board Emulator — Status and next steps

Runs unmodified AC6 firmware for the HAW Hamburg ITS-Board (NUCLEO-F429ZI,
ITS carrier board, Waveshare 4" TFT). Rust + Unicorn, one process; see
README.md for features and usage.

## Architecture

```
apps/its-emu (CLI)        apps/its-emu-gui (egui)
        \                     /
         stlink     — ST-LINK/V2-1 (debug commands, virtual COM port) on USB/IP
         usbip      — USB/IP server for one emulated device (vhci-hcd attaches it)
         itsboard   — wiring, ILI9486, XPT2046, LM75, LEDs/buttons, network, fonts.bin
              |   \
              |    rp2040 — Raspberry Pi Pico (both cores, PIO, DMA, USB CDC), own thread
              |
         stm32f429  — memory map, RCC, PWR, FLASH, GPIO, EXTI/SYSCFG, TIM1–14, ADC/DAC,
                      I2C1–3, SPI1–6, U(S)ART1–8, DMA1/2, ETH, IWDG, CRC, RNG
              |
         emu-core   — Unicorn CPU, NVIC/SCB/SysTick, exceptions, cycle model,
                      virtual clock, idle and delay-loop skipping, ELF loader,
                      GDB server, debug architecture (halting debug, FPB, DWT,
                      ROM table), SW-DP/AHB-AP
```

## Validation

Each peripheral model is checked against the real board with HwCheck
(`tests/fixtures/ac6-src/HwCheck`, commands over the virtual COM port,
`rd`/`wr` for ad-hoc register probes). Board results are in
`tests/hardware`; `crates/itsboard/tests/hardware.rs` compares.

## Next steps

Not yet compared with the board (record with HwCheck first):

- **Timer chaining** (ITR0–3, TRGO reset/enable/update): `timchain` — TRGO
  delay to the slave, gated count (`timer_chaining_matches_the_board`).
  TRGO from the CC1 pulse and OC1REF–OC4REF (MMS ≥ 3) times are rounded like
  an overflow's, unmeasured.
- **Timer DMA**: `timdma` — is a request made while the stream is off
  transferred once it is enabled? (The emulator keeps it, line `T off`.)
- **TIM1/TIM8 advanced features**: complementary outputs, dead time,
  MOE/AOE, off states, BKIN, COM event.
- **USB probe**: ROM table/component IDs, FP_CTRL, DWT_CTRL, DBGMCU_IDCODE
  (HwCheck `rd`); a pyOCD session (`pyocd load -vv` + usbmon against the
  real ST-LINK) for command sequence and erase/program timing.
- **ADC/DAC triggers** from CCx, OCxREF/compare-pulse TRGO, EXTI11/15 (ADC)
  and EXTI9 (DAC): measure which signal the ADC sees (OCxREF edge or compare
  pulse; EXTI line before or after RTSR/FTSR/IMR) before modelling.

Known gaps:

- **Timers**: slave modes with ETR; lock levels; break input polled at the
  sweeps, not at its edge. On the board UG with a stopped counter leaves CNT
  until it runs again (the emulator clears it at once).
- **DMA**: memory is written item by item (no bursts, FIFO threshold
  ignored); transfers take no time.
- **I2C with DMA** keeps CPU byte timing: BTF is set after each byte though
  DMA keeps DR filled on the board (the HAL ignores BTF with DMAEN).
- **Flash**: programming does not AND; PGSERR faults instead of only setting
  the flag (`stm32f429::flash`). No mass storage, no SWO.
- **Network**: no NAT to the host network; IPv6 frames are dropped.
- **Devices**: more I2C devices (EEPROM, IMU, …) via `I2cDevice`. Which pin
  the course uses for the DS18B20 is open (encoder: IN0/IN1, GS task 2).
- **Pico**: SPI/I2C only store registers; flash program/erase is ignored
  (both warned once). PWM lacks DIVMODE inputs and PH_ADV/PH_RET. ADC inputs
  see digital board levels only (`Pico::analog` sets voltages). DMA: no
  sniffer, no SPI/I2C/XIP DREQs, one item per cycle per channel with no bus
  sharing, RAM writes do not invalidate translated code. A core writing
  flash or RAM drops only its own translations; the other core may run
  stale code.
- **Timing within a block**: DWT->CYCCNT reads are exact, but peripheral
  accesses see the end of their translated block, and interrupts pended by a
  write (STIR, SWIER) are taken at the block end — 35 cycles late in
  `irqlat` (26 vs 61). Stopping right after the store needs `emu_stop` from
  the MMIO callback, but Unicorn then restores the PC to the store
  (`cpu_loop_exit_restore` in `helper_check_exit_request`) and runs it
  twice; skipping it by hand breaks writeback stores, STM and IT blocks.

Speed (Unicorn is vendored with patches, `vendor/README.md`):

- Remaining cost in busy programs: per-block hook (~45–50 %; registered
  directly with Unicorn, `block_hook` in `machine.rs`, as the binding's
  proxy cost a quarter of it), next-block lookup after function returns
  (~15 %; direct branches now chain across pages), MMIO through QEMU's memory
  API (~20 % when polling), and the sweep over all ~55 peripherals at every
  event (`update`/`next_event`, ~15 % while drawing). A per-peripheral
  next-event cache would cut the sweeps, but `update` re-asserts IRQ levels
  and some `next_event`s read shared state, so every model needs an audit.
- In place: idle detection via Unicorn's first-write path per RAM page;
  delay-loop skipping (`machine/delay.rs`: a small affine Thumb interpreter
  checked against Unicorn solves for the exit iteration; off with
  breakpoints, watchpoints or halting debug; DelayLoops fixture 45× faster
  with identical cycles). One deterministic CPU thread; the GUI renders on
  its own.
- Tools: `tools/perf/bench.sh` times the fixtures, `tools/perf/same-cycles.sh`
  checks that cycles and output stay unchanged. `--no-idle-skip` timings vary
  ±10 % with code layout; compare `perf stat` instruction counts instead.

Packaging:

- Packages use the `dist` profile (fat LTO): GUI 15 MB, CLI 3.4 MB.
- Release packages for more platforms (Windows, other Linux distributions)
  as further `release.yml` jobs.
