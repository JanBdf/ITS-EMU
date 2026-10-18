#!/bin/sh
# Builds the Arch package of the current checkout (for first installs and
# updates alike) and puts it into dist/.
#
#   packaging/arch/build.sh
#   sudo pacman -U dist/its-emu-<version>-x86_64.pkg.tar.zst   # install / update
#   sudo pacman -Rns its-emu                                   # remove
set -e
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$root"

# pacman sorts updates by the commit count; uncommitted changes go into the
# package but do not change the version
base=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
pkgver="$base.r$(git rev-list --count HEAD).g$(git rev-parse --short HEAD)"
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    echo "warning: uncommitted changes are packaged as $pkgver" >&2
fi

# makepkg works on a copy, so the tracked PKGBUILD stays untouched
work=$root/target/arch-pkg
rm -rf "$work"
mkdir -p "$work" "$root/dist"
cp packaging/arch/PKGBUILD packaging/arch/its-emu.install "$work/"
sed -i "s/^pkgver=.*/pkgver=$pkgver/" "$work/PKGBUILD"

cd "$work"
ITS_EMU_ROOT=$root PKGDEST=$root/dist makepkg -f --noconfirm
pkg=$root/dist/its-emu-$pkgver-1-x86_64.pkg.tar.zst
echo
echo "Package: $pkg"
echo "Install or update:  sudo pacman -U $pkg"
echo "Remove completely:  sudo pacman -Rns its-emu"
