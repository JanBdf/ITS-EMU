# Reference measurements from the real ITS-Board

Output of the `HwCheck` test program (`tests/fixtures/ac6-src/HwCheck`) on an
ITS-Board (Rev 02, NUCLEO-F429ZI, 180 MHz, ART accelerator on), taken
2026-10-16/17 with `tools/hw/cmd.py`:

| file | command | content |
|---|---|---|
| `HwCheck-time.txt` | `time` | durations of library calls in TIM2 ticks (90 per µs) |
| `HwCheck-cyc.txt` | `cyc` | `C`: DWT->CYCCNT cycles of 1000 iterations of instruction kernels (10 copies of the body + `subs`/`bne`); `F`: 1000 calls of library functions |
| `HwCheck-div.txt` | `div` | `D dividend divisor udiv sdiv`: cycles of 100 iterations of 10 divisions |
| `HwCheck-pins.txt` | `pins` | per port: MODER, PUPDR, ODR, input pins, IDR as configured, with pull-down and with pull-up on the input pins |
| `HwCheck-exti.txt` | `exti` | EXTI/SYSCFG register semantics, interrupt latency from a GPIO edge or SWIER |
| `HwCheck-tim.txt` | `tim` | timer compare flags, CC/update interrupt counts, TIM1 repetition counter, basic timers, shared interrupt lines, register masks |
| `HwCheck-adc.txt` | `adc` | ADC conversion times, internal channels, open pins, DAC1 measured by ADC1, flag semantics |
| `HwCheck-i2c.txt` | `i2c` | I2C1 with nothing on the header: START, address NACK, STOP, flags and durations |
| `HwCheck-dma.txt` | `dma` | DMA2 memory-to-memory copy, ADC1 scan into memory (single, after the end of the transfer, continuous + circular) |
| `HwCheck-dmatx.txt` | `dmatx` | USART3 transmission by DMA1 stream 3: output, flags, durations |
| `HwCheck-dmaspi.txt` | `dmaspi` | SPI3 transfer by DMA1 with nothing on the header |
| `HwCheck-adctrig.txt` | `adctrig` | ADC1 started by TIM3 TRGO at 1 kHz: conversions, delay after the trigger |
| `HwCheck-dacwave.txt` | `dacwave` | DAC1 noise and triangle generation (software trigger), TIM6 TRGO trigger with DMA1 stream 5 feeding DHR, DMA underrun |
| `HwCheck-timout.txt` | `timout` | TIM3/TIM8 CH1 driving PC6: PWM levels against CNT, CCR = 0 / above ARR, polarity, CCxE off, forced levels, mode change, toggle captured by CH2, MOE |
| `HwCheck-enc.txt` | `enc` | TIM8 encoder modes (SMS 1/2/3, inverted TI1, wrap below 0) on PC6/PC7, quadrature edges made by switching the pins' pull-up/pull-down |
| `HwCheck-irqlat.txt` | `irqlat` | interrupt latency to a handler that reads DWT->CYCCNT first: pended by NVIC->STIR or EXTI->SWIER, with interrupts enabled or masked until CPSIE |
| `HwCheck-regs.txt` | `wr`/`dump` (first line) | reset values of the registers of timers, SPIs, U(S)ARTs, I2Cs, ADC, DAC, SYSCFG, EXTI |

`crates/itsboard/tests/hardware.rs` runs the same commands in the emulator
and compares. Re-measure with:

```sh
tools/hw/flash.sh tests/fixtures/ac6/HwCheck.axf
python3 tools/hw/cmd.py --wait-ready time cyc div pins
```
