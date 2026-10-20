#!/bin/sh
# Builds the macOS app of the current checkout for this Mac's architecture
# (arm64 or x86_64) and puts a disk image into dist/.
#
#   packaging/macos/build.sh
#   open dist/its-emu-<version>-macos-<arch>.dmg   # drag the app to Applications
#
# Needs rsvg-convert (brew install librsvg) for the icon. The app is signed
# ad hoc only: Gatekeeper asks once before the first start (README).
set -e
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$root"

base=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
build=$(git rev-list --count HEAD)
version="$base.r$build.g$(git rev-parse --short HEAD)"
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    echo "warning: uncommitted changes are packaged as $version" >&2
fi
arch=$(uname -m)

cargo build --profile dist --locked -p its-emu -p its-emu-gui
tools/vscode-nodebug/package.sh >/dev/null

work=$root/target/macos-pkg
app="$work/dmg/ITS-Board Emulator.app"
rm -rf "$work"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$work/its-emu-gui.iconset" "$root/dist"

# Contents/MacOS: the GUI is the app; its-emu beside it for the terminal
install -m755 target/dist/its-emu-gui target/dist/its-emu "$app/Contents/MacOS/"
sed -e "s/@VERSION@/$base/" -e "s/@BUILD@/$build/" packaging/macos/Info.plist >"$app/Contents/Info.plist"
printf 'APPL????' >"$app/Contents/PkgInfo"

# Icon from the same SVG as the Linux desktop entry
svg=packaging/arch/its-emu-gui.svg
for size in 16 32 128 256 512; do
    rsvg-convert -w $size -h $size "$svg" -o "$work/its-emu-gui.iconset/icon_${size}x${size}.png"
    rsvg-convert -w $((size * 2)) -h $((size * 2)) "$svg" -o "$work/its-emu-gui.iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$work/its-emu-gui.iconset" -o "$app/Contents/Resources/its-emu-gui.icns"

# Licenses and the VS Code extension (installed with `code --install-extension`)
res="$app/Contents/Resources"
install -m644 README.md LICENSE tools/vscode-nodebug/gdbtarget-nodebug.vsix "$res/"
# Unicorn is vendored (with patches, vendor/README.md)
install -m644 vendor/unicorn-engine-sys/COPYING "$res/COPYING.unicorn"

# Ad-hoc signature over the whole bundle: without one, Apple silicon reports
# a downloaded app as damaged instead of offering to open it
codesign --force --sign - --timestamp=none "$app/Contents/MacOS/its-emu"
codesign --force --sign - --timestamp=none "$app"

ln -s /Applications "$work/dmg/Applications"
dmg=$root/dist/its-emu-$version-macos-$arch.dmg
rm -f "$dmg"
# hdiutil fails now and then while the system still holds the source folder
# ("Resource busy"); a second try works
for try in 1 2 3; do
    hdiutil create -volname "ITS-Board Emulator" -srcfolder "$work/dmg" -fs HFS+ -format UDZO "$dmg" >/dev/null && break
    [ $try = 3 ] && exit 1
    sleep 2
done
echo
echo "Disk image: $dmg"
echo "Install or update:  open it and drag the app to Applications"
echo "Command line:       ln -s '/Applications/ITS-Board Emulator.app/Contents/MacOS/its-emu' /usr/local/bin/"
