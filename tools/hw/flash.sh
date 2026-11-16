#!/bin/sh
# Flashes an image (.axf/.elf) onto the real ITS-Board via the Nucleo's
# ST-LINK and resets it.  Usage: tools/hw/flash.sh Program.axf
# tools/hw/reset.sh resets it without flashing.
set -e
serial=$("$(dirname "$0")/board-serial.sh")
exec openocd -d0 -f interface/stlink.cfg -c "adapter serial $serial" -f target/stm32f4x.cfg \
    -c "program {$1} verify reset exit"
