# ITS-Board Emulator — Status and next steps

Runs unmodified AC6 firmware for the HAW Hamburg ITS-Board (NUCLEO-F429ZI,
ITS carrier board, Waveshare 4" TFT). Rust + Unicorn, one process; see
README.md for features and usage.

## Architecture

```
apps/its-emu (CLI)        apps/its-emu-gui (egui)
        \                     /
         stlink     — ST-LINK/V2-1 (debug commands, virtual COM port) on USB/IP
         usbip      — USB/IP server for the emulated devices, bus ids 1-1 (ST-LINK),
                      1-2... (CN13, Picos); vhci-hcd attaches them; forwarding of
                      devices whose controller is modelled elsewhere
         itsboard   — wiring, ILI9486, XPT2046, LM75, LEDs/buttons, network, fonts.bin
              |   \
              |    rp2040 — Raspberry Pi Pico (both cores, PIO, DMA, USB device), own thread
              |
         stm32f429  — memory map, RCC, PWR, FLASH, GPIO, EXTI/SYSCFG, TIM1–14, ADC/DAC,
                      I2C1–3, SPI1–6, U(S)ART1–8, DMA1/2, ETH, OTG FS, IWDG, CRC, RNG
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

- **TIM1/TIM8 advanced features**: complementary outputs, dead time,
  MOE/AOE, off states, BKIN, COM event.
- **USB probe**: a pyOCD session (`pyocd load -vv` + usbmon against the
  real ST-LINK) for command sequence and erase/program timing. (The debug
  IDs match the board: `tests/hardware/probe-ids.txt`.)
- **ADC/DAC triggers** from CCx, OCxREF/compare-pulse TRGO, EXTI11/15 (ADC)
  and EXTI9 (DAC): measure which signal the ADC sees (OCxREF edge or compare
  pulse; EXTI line before or after RTSR/FTSR/IMR) before modelling.

Known gaps:

- **Timers**: slave modes with ETR; lock levels; break input polled at the
  sweeps, not at its edge. TRGO from the CC1 pulse and OC1REF–OC4REF
  (MMS ≥ 3) is timed like an overflow, unmeasured.
- **Cycle model**: peripheral accesses that are not back to back cost too
  little: an APB read between other instructions takes ~12 cycles on the
  board, 10 in the emulator (HwCheck `cyc` ldr_apb1_nop; `timsync` reads),
  and AHB accesses next to SRAM accesses miss 1–2 cycles. Course code that
  drives the display or polls peripherals runs 7–11 % fast (`cyc` F lines).
- **DMA**: memory is written item by item (no bursts, FIFO threshold
  ignored); transfers take no time.
- **I2C with DMA** keeps CPU byte timing: BTF is set after each byte though
  DMA keeps DR filled on the board (the HAL ignores BTF with DMAEN).
- **Flash**: programming does not AND; PGSERR faults instead of only setting
  the flag (`stm32f429::flash`). No mass storage, no SWO.
- **USB OTG FS (CN13)**: the host model sends packets back to back at the
  bit rate and retries a NAKed one at once. Through a real host controller
  UsbCdcEcho's echo of 64 KiB runs at 208 KiB/s (562 in the emulator) and
  512 bytes take 2.3 ms (1.0): it retries NAKed packets later
  (`tests/hardware/UsbCdcEcho.txt`). Short round trips match (0.2 ms).
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
- **USB OTG FS**: device mode with FIFOs only (no host mode, DMA,
  isochronous endpoints, suspend/resume, SOF interrupts); PA9 VBUS is not
  driven (the course's firmware does not sense it). Register reset values
  and the core id (GSNPSID 0x4F54281A) are not compared with the board yet
  (HwCheck). Over USB/IP, ping-pong traffic (a CDC echo) reaches about
  80 KB/s at the wall clock against about 580 KB/s in virtual time: each
  completion reaches the host once per emulation slice.
- **Timing within a block**: DWT->CYCCNT reads are exact, but peripheral
  accesses see the end of their translated block, and interrupts pended by a
  write (STIR, SWIER) are taken at the block end — 35 cycles late in
  `irqlat` (26 vs 61). Stopping right after the store needs `emu_stop` from
  the MMIO callback, but Unicorn then restores the PC to the store
  (`cpu_loop_exit_restore` in `helper_check_exit_request`) and runs it
  twice; skipping it by hand breaks writeback stores, STM and IT blocks.

Speed (Unicorn is vendored with patches, `vendor/README.md`):

- Blocks charge their cycles in translated code (`FastAccount` in
  `machine.rs`, `uc_its_account`); the block hook (`block_hook`), which was
  60–70 % of busy code on an arm64 host, runs only at slice ends, at the idle
  detector's loop heads, after exceptions and for blocks with divisions, SEV
  or a fall-through end. The hook stays the reference:
  `tools/perf/same-cycles.sh` compares against a build without the inline
  code. With a second core (`set_end_run_signals`) a raised wake signal
  sends the next block to the hook, which ends the run there.
- Indirect jumps find their target in a table per jump site (`its_jc`,
  `vendor/README.md`) instead of QEMU's `lookup_tb_ptr` (18–34 % of busy
  code after the change above).
- Both together, on an Apple M-series host: busy fixtures run 2.0–2.2× as
  fast (3 s of HwCheck with `--no-idle-skip`: 1.56 s → 0.71 s), the default
  mode about 1.15×.
- Remaining cost (same host): busy code is translated code only, including
  the accounting at each block start (some 25 host instructions). While
  drawing, 95 % is outside translated code: MMIO through QEMU's memory API
  and the sweep over all ~55 peripherals at every event
  (`update`/`next_event`). A per-peripheral next-event cache would cut the
  sweeps, but `update` re-asserts IRQ levels and some `next_event`s read
  shared state, so every model needs an audit.
- While an enabled interrupt is pending but masked, slices are 1000 cycles
  (`masked_quantum`) so that unmasking is noticed: I2cLm75 runs 1.8 million
  slices in 10 s. Entering and leaving `emu_start` is cheap now on macOS
  (`vendor/README.md`: 3.5 s → 2.1 s), and the block hook starts the next
  slice itself when the run loop would do nothing else (`renew_slice`:
  1.9 s); what remains per slice is the peripheral sweep twice
  (`sync_devices`, `next_slice_end`). Noticing CPSIE
  and BASEPRI writes at their block instead would remove the short slices,
  but takes the interrupt earlier than now: a change of behaviour.
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
