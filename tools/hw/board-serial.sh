#!/bin/sh
# Prints the serial number of the real board's ST-LINK: $ITS_BOARD_SERIAL, or
# the first ST-LINK in /dev/serial/by-id that is not the emulator's
# (its-emu --usb exports one with serial 49545345...).
if [ -n "$ITS_BOARD_SERIAL" ]; then echo "$ITS_BOARD_SERIAL"; exit; fi
for link in /dev/serial/by-id/usb-STMicroelectronics_STM32_STLink_*-if02; do
    [ -e "$link" ] || continue
    serial=${link#*_STLink_}
    serial=${serial%-if02}
    case "$serial" in 49545345*) continue ;; esac
    echo "$serial"
    exit
done
echo "no ST-LINK of a real board found" >&2
exit 1
