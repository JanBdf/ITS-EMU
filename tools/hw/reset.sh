#!/bin/sh
# Resets the real ITS-Board (also one that hangs) via its ST-LINK.
set -e
serial=$("$(dirname "$0")/board-serial.sh")
exec openocd -d0 -f interface/stlink.cfg -c "adapter serial $serial" -f target/stm32f4x.cfg \
    -c "init; reset run; exit"
