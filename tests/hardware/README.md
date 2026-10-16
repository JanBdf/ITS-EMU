# Reference measurements from the real ITS-Board

Output of the `HwCheck` test program (`tests/fixtures/ac6-src/HwCheck`) on an
ITS-Board (Rev 02, NUCLEO-F429ZI, 180 MHz, ART accelerator on), taken
2026-10-16 with `tools/hw/cmd.py`:

| file | command | content |
|---|---|---|
| `HwCheck-time.txt` | `time` | durations of library calls in TIM2 ticks (90 per µs) |
| `HwCheck-cyc.txt` | `cyc` | `C`: DWT->CYCCNT cycles of 1000 iterations of instruction kernels (10 copies of the body + `subs`/`bne`); `F`: 1000 calls of library functions |
| `HwCheck-div.txt` | `div` | `D dividend divisor udiv sdiv`: cycles of 100 iterations of 10 divisions |
| `HwCheck-pins.txt` | `pins` | per port: MODER, PUPDR, ODR, input pins, IDR as configured, with pull-down and with pull-up on the input pins |

`crates/itsboard/tests/hardware.rs` runs the same commands in the emulator
and compares. Re-measure with:

```sh
tools/hw/flash.sh tests/fixtures/ac6/HwCheck.axf
python3 tools/hw/cmd.py --wait-ready time cyc div pins
```
