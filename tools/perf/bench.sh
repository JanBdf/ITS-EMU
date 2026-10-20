#!/bin/bash
# Wall time of the command line emulator on the fixtures (best of 3), with
# idle skipping (as the GUI runs) and without (busy firmware).
# Usage: tools/perf/bench.sh [path/to/its-emu]   (default: target/release/its-emu)
here=$(cd "$(dirname "$0")/../.." && pwd)
bin=${1:-$here/target/release/its-emu}
f=$here/tests/fixtures/ac6
run() {
    local name=$1 best=
    shift
    [ -f "$1" ] || { printf "%-16s (missing)\n" "$name"; return; }
    for _ in 1 2 3; do
        local t0 t1
        t0=$(date +%s%N)
        "$bin" "$@" >/dev/null 2>&1
        t1=$(date +%s%N)
        local ms=$(((t1 - t0) / 1000000))
        if [ -z "$best" ] || [ "$ms" -lt "$best" ]; then best=$ms; fi
    done
    printf "%-16s %6d ms\n" "$name" "$best"
}
run stack            "$f/Stack.axf" --seconds 10
run newfont          "$f/TestNewFont.axf" --seconds 10
run newfont-draw     "$f/TestNewFont.axf" --seconds 8 --input 3:USER=1,3.5:USER=0
run newfont-busy     "$f/TestNewFont.axf" --seconds 3 --no-idle-skip
run hwcheck-busy     "$f/HwCheck.axf" --seconds 3 --no-idle-skip
run stack-busy       "$f/Stack.axf" --seconds 3 --no-idle-skip
run stoppuhr-busy    "$f/StoppuhrASM.axf" --seconds 3 --no-idle-skip
run sieve-busy       "$f/PrimzahlSiebASM.axf" --seconds 5 --no-idle-skip
