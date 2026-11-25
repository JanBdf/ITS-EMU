#!/bin/sh
# Builds a Debian/Ubuntu (.deb) package of the current checkout (for Ubuntu
# 24.04, Ubuntu 26.04, Debian 13+) and puts it into dist/.
#
#   packaging/deb/build.sh
#   sudo apt install ./dist/its-emu_<version>_<arch>.deb   # install / update
#   sudo apt remove its-emu                                # remove
set -e
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$root"

# dpkg sorts updates by the commit count; uncommitted changes go into the
# package but do not change the version
base=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
version="${base}+r$(git rev-list --count HEAD).g$(git rev-parse --short HEAD)-1"
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    echo "warning: uncommitted changes are packaged as $version" >&2
fi

if command -v dpkg >/dev/null 2>&1; then
    arch=$(dpkg --print-architecture)
else
    case "$(uname -m)" in
        x86_64)  arch=amd64 ;;
        aarch64) arch=arm64 ;;
        *)       echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
    esac
fi

cargo build --profile dist --locked -p its-emu -p its-emu-gui
tools/vscode-nodebug/package.sh >/dev/null

work=$root/target/deb-pkg
stage=$work/root
rm -rf "$work"
mkdir -p "$stage/DEBIAN" "$root/dist"

bin=$root/target/dist
install -Dm755 "$bin/its-emu" "$bin/its-emu-gui" -t "$stage/usr/bin/"
install -Dm644 packaging/arch/its-emu-gui.desktop -t "$stage/usr/share/applications/"
install -Dm644 packaging/arch/its-emu-gui.svg -t "$stage/usr/share/icons/hicolor/scalable/apps/"
install -Dm644 README.md -t "$stage/usr/share/doc/its-emu/"

# Combined copyright file (MIT for ITS-EMU, GPL-2.0 for vendored Unicorn)
{
    cat LICENSE
    printf '\n---\n\nUnicorn Engine (statically linked, vendor/unicorn-engine-sys):\n\n'
    cat vendor/unicorn-engine-sys/COPYING
} > "$stage/usr/share/doc/its-emu/copyright"
chmod 644 "$stage/usr/share/doc/its-emu/copyright"

# The ST-LINK on USB/IP: vhci-hcd at boot, a helper that polkit lets the
# user at the computer run to attach the probe (from 127.0.0.1 only), and
# access for the logged-in user to the probe and its virtual COM port
install -Dm755 packaging/arch/usbip-attach "$stage/usr/lib/its-emu/usbip-attach"
install -Dm644 packaging/arch/its-emu.policy "$stage/usr/share/polkit-1/actions/its-emu.usbip-attach.policy"
install -Dm644 packaging/arch/vhci-hcd.conf "$stage/usr/lib/modules-load.d/its-emu.conf"
install -Dm644 packaging/arch/70-its-emu-stlink.rules -t "$stage/usr/lib/udev/rules.d/"

# VS Code extension (install with: code --install-extension /usr/share/its-emu/gdbtarget-nodebug.vsix)
install -Dm644 tools/vscode-nodebug/gdbtarget-nodebug.vsix -t "$stage/usr/share/its-emu/"

install -m755 packaging/deb/postinst "$stage/DEBIAN/postinst"
install -m755 packaging/deb/postrm "$stage/DEBIAN/postrm"

installed_size=$(du -sk "$stage" | awk '{print $1}')
cat > "$stage/DEBIAN/control" <<EOF
Package: its-emu
Version: $version
Architecture: $arch
Maintainer: Jan Bittendorf <https://github.com/JanBdf/ITS-EMU>
Section: electronics
Priority: optional
Homepage: https://github.com/JanBdf/ITS-EMU
Installed-Size: $installed_size
Depends: libc6, libgcc-s1, libdbus-1-3, libfontconfig1, libgl1, libegl1, libwayland-client0, libx11-6, libxcb1, libxcursor1, libxi6, libxkbcommon0, libxkbcommon-x11-0, hicolor-icon-theme, pkexec | policykit-1, usbip | linux-tools-generic
Recommends: gdb-multiarch, xdg-desktop-portal-gtk | xdg-desktop-portal-kde | xdg-desktop-portal-gnome | zenity
Suggests: code
Description: Emulator for the HAW Hamburg ITS-Board (STM32F429 + TFT)
 Runs unmodified .axf firmware built with the course toolchain (Arm Compiler 6)
 for the HAW Hamburg ITS-Board (NUCLEO-F429ZI, ITS carrier board, Waveshare
 4" TFT touch shield), with a graphical board view, serial console, GDB server
 and a USB ST-LINK over USB/IP for pyOCD/OpenOCD.
EOF

deb=$root/dist/its-emu_${version}_${arch}.deb
dpkg-deb --root-owner-group --build "$stage" "$deb"
echo
echo "Package: $deb"
echo "Install or update:  sudo apt install $deb"
echo "Remove:             sudo apt remove its-emu"
