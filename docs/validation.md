# Comparison with the real board

`tests/fixtures/ac6-src/HwCheck` takes commands over the virtual COM port
(colour bars, fonts, LED patterns, backlight, DWT->CYCCNT timing of library
calls and instruction kernels, pin levels, peripheral probes). The same
commands run on the board and in the emulator;
`crates/itsboard/tests/hardware.rs` compares against the board results in
[`tests/hardware`](../tests/hardware/README.md).

| | board | emulator |
|---|---|---|
| `GUI_clear` (SPI bound) | 39 325 428 TIM2 ticks | 39 325 157 |
| `GUI_disStr`, Font24, 20 characters | 2 456 720 | 2 445 960 (−0.4 %) |
| C delay loop / assembler loop, 100 000 iterations | 800 346 / 150 198 | 800 336 / 150 181 |
| filled circle, radius 100 (point by point) | 133.3 M | 124.7 M (−6.5 %) |
| input levels of ports A–I (no pull / pull-down / pull-up) | | identical |

`tools/hw` drives the board through its ST-LINK (`flash.sh`, `cmd.py`;
HwCheck's `rd`/`wr` access any register). The GUI's board drawings
(`apps/its-emu-gui/assets/board-{carrier,nucleo,shield}.svg`) are made by
`boarddraw.py` from rectified photos of the real board (not published);
`boardlabelfit.py` and `boardtraces.py` fit the silkscreen labels and copper
traces, `boardcmp.py` shows a region of drawing and photo side by side.

```sh
tools/hw/flash.sh tests/fixtures/ac6/HwCheck.axf
python3 tools/hw/cmd.py --wait-ready time cyc exti   # on the board
python3 tools/hw/cmd.py "rd 40013c14"                # EXTI->PR
python3 tools/hw/boardcmp.py 0 0 300 160 /tmp/cmp.png
```

Modelled from typical values rather than measured on this board: the
oscillators' start-up and the PLLs' lock time (datasheet), the contact
bounce of buttons and encoder (a few bounces within 0.1–2 ms, a
deterministic sequence), the ADC's noise on applied voltages (±2 counts)
and the touch controller's jitter (±2 counts, more on the pressure
channels). Known deviations from the chip are listed in
[PLAN.md](../PLAN.md).
