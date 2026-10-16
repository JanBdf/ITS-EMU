#!/bin/sh
# Rebuilds all programs of ITS-BRD-VSC with Arm Compiler 6 (Debug) and copies
# the images into tests/fixtures/ac6/. Usage: tools/build-fixtures.sh [path/to/ITS-BRD-VSC]
set -e
here=$(cd "$(dirname "$0")/.." && pwd)
brd=$(cd "${1:-$here/../ITS-BRD-VSC}" && pwd)
. "$here/tools/arm-env.sh"
dest="$here/tests/fixtures/ac6"
mkdir -p "$dest"
for dir in "$brd"/Programs/*/; do
    name=$(basename "$dir")
    [ -f "$dir/$name.csolution.yml" ] || continue
    echo "== $name"
    (cd "$dir" && cbuild "$name.csolution.yml" --context .Debug --packs >/dev/null)
    # Strip debug info (it contains absolute build paths); symbols are kept.
    fromelf --elf --strip=debug,comment -o "$dest/$name.axf" "$(find "$dir/out" -name "$name.axf" | head -n1)" >/dev/null
done
cp "$brd/ITS_BRD_LIB/Utilities/AddFonts/fonts.bin" "$dest/"

# __FILE__ strings (asserts) still contain the build directory. Overwrite its
# parent prefix with the same number of '/' so no offsets change.
prefix=$(dirname "$brd")
python3 - "$prefix" "$dest"/*.axf <<'PY'
import sys
prefix = sys.argv[1].encode()
for path in sys.argv[2:]:
    data = open(path, "rb").read()
    open(path, "wb").write(data.replace(prefix, b"/" * len(prefix)))
PY
echo "fixtures in $dest"
