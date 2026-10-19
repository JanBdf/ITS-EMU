#!/bin/bash
# Checks that two builds of the command line emulator behave identically:
# runs every fixture with scripted inputs, with and without idle skipping,
# and compares the final PC, cycle count, LEDs, serial output and the
# display (screenshot). A change for speed must not change any of it.
# Usage: tools/perf/same-cycles.sh path/to/its-emu-before path/to/its-emu-after
# Overrides: SECONDS_VIRTUAL (virtual time, default 2.5), INPUT (the --input script),
# FIXTURES (firmware files), MODES ("idle noidle", default both).
# Idle skipping must not change anything either: compare a build with a
# wrapper that adds --no-idle-skip, with MODES=idle.
here=$(cd "$(dirname "$0")/../.." && pwd)
[ $# -eq 2 ] || { echo "usage: $0 BEFORE AFTER" >&2; exit 2; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
input=${INPUT:-"0.3:S0=1,0.35:S0=0,0.5:S7=1,0.6:S7=0,0.8:SERIAL=hello\n,1:TOUCH=100;100,1.1:TOUCH=0,1.3:S1=1,1.4:S1=0,1.5:SERIAL=rd 40023800\n,1.6:USER=1,1.7:USER=0,1.8:PC3=1.65"}
seconds=${SECONDS_VIRTUAL:-2.5}
fixtures=${FIXTURES:-"$here/tests/fixtures/ac6/*.axf $here/tests/fixtures/micro/*.elf"}
modes=${MODES:-"idle noidle"}
status=0
# shellcheck disable=SC2086
for fw in $fixtures; do
    for m in $modes; do
        mode=
        [ "$m" = noidle ] && mode=--no-idle-skip
        for side in 1 2; do
            bin=${!side}
            # shellcheck disable=SC2086
            "$bin" "$fw" --seconds "$seconds" --continue-on-bkpt $mode --input "$input" \
                --screenshot "$tmp/shot.png" >"$tmp/out$side" 2>&1
            # drop wall time, speed, paths and log timestamps
            sed -i -E 's/wall [0-9.]+ s +\(.*\)//; s#/[^ ]*shot\.png##; s/^\[[0-9TZ:-]+ /[/' "$tmp/out$side"
            [ -f "$tmp/shot.png" ] && md5sum <"$tmp/shot.png" >>"$tmp/out$side"
            rm -f "$tmp/shot.png"
        done
        if cmp -s "$tmp/out1" "$tmp/out2"; then
            echo "same     $(basename "$fw") $mode"
        else
            echo "DIFFERS  $(basename "$fw") $mode"
            diff "$tmp/out1" "$tmp/out2" | head -6
            status=1
        fi
    done
done
exit $status
