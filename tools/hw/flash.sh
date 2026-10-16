#!/bin/sh
# Flashes an image (.axf/.elf) onto the real ITS-Board via the Nucleo's
# ST-LINK and resets it.  Usage: tools/hw/flash.sh Program.axf
set -e
exec openocd -d0 -f interface/stlink.cfg -f target/stm32f4x.cfg \
    -c "program {$1} verify reset exit"
