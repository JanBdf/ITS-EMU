# Development

## Building

Needs Rust (stable), a C toolchain, CMake and libclang (for Unicorn). The
GUI loads the X11/Wayland libraries at run time.

```sh
cargo build --release
cargo test --release
```

The workspace: `crates/emu-core` (Cortex-M4F machine on a vendored, patched
[Unicorn](../vendor/README.md), NVIC, SysTick, cycle model, idle-loop
skipping, GDB server, debug port), `crates/stm32f429` (peripherals),
`crates/itsboard` (board, display, touch, devices, network, Pico plugin),
`crates/rp2040`, `crates/usbip`, `crates/stlink`, `apps/its-emu` (CLI) and
`apps/its-emu-gui` (eframe/egui). [PLAN.md](../PLAN.md) has the
architecture and known deviations, [CLAUDE.md](../CLAUDE.md) the working
rules. A change for speed must not change behaviour:
`tools/perf/same-cycles.sh BEFORE AFTER` compares every fixture's cycles,
output and display between two builds; `tools/perf/bench.sh` times them.

CI (`.github/workflows/ci.yml`, on pushes to `main`/`experimental` and PRs)
checks formatting and clippy and tests on Linux, Windows and macOS, x64 and
arm64 (Windows on Arm runs the x64 build under emulation, as Unicorn does
not build there). It records throughput in the job summary and keeps
binaries and macOS disk images for 14 days. Tests that need the unpublished
lab solutions in `tests/fixtures/ac6` skip themselves.

Fixtures in `tests/fixtures/ac6` are rebuilt from an ITS-BRD-VSC checkout
with `tools/build-fixtures.sh` (CMSIS-Toolbox and AC6 from the course's VS
Code setup); `tools/build-own.sh [Name]` rebuilds only the own test programs.
`tools/build-own.sh Tutorial` also copies the GUI's tutorial program to
`apps/its-emu-gui/assets/tutorial.axf`, which the GUI embeds.

## Arch Linux package

`packaging/arch/build.sh` builds a pacman package of the checkout into
`dist/` (version `<version>.r<commits>.g<hash>`; uncommitted changes are
included without changing the version). It contains both programs, a desktop
entry, the `vscode-nodebug` extension (built into Arch's `code`, and as
`.vsix` in `/usr/share/its-emu`), and what the USB ST-LINK needs: `usbip`,
`vhci-hcd` loaded at boot, a polkit-allowed helper that attaches the probe
from 127.0.0.1 without a password, and udev rules for the probe and its COM
port.

```sh
packaging/arch/build.sh
sudo pacman -U dist/its-emu-<version>-x86_64.pkg.tar.zst   # install or update
sudo pacman -Rns its-emu                                   # remove
```

## Debian / Ubuntu package

`packaging/deb/build.sh` builds a `.deb` package of the checkout into `dist/`
(for Ubuntu 24.04, Ubuntu 26.04 and Debian 13+; needs `dpkg-deb` and `zip`).
It carries the same programs, desktop entry, VS Code `.vsix` extension
(`/usr/share/its-emu/gdbtarget-nodebug.vsix`) and USB ST-LINK setup (`vhci-hcd`,
polkit helper and udev rules) as the Arch package.

```sh
packaging/deb/build.sh
sudo apt install ./dist/its-emu_<version>_<arch>.deb   # install or update
sudo apt remove its-emu                                # remove
```

## macOS app

`packaging/macos/build.sh` builds *ITS-Board Emulator.app* for the host
architecture and `dist/its-emu-<version>-macos-<arch>.dmg` (needs
`rsvg-convert`: `brew install librsvg`). The app holds both programs, the
VS Code extension and the licenses; it is signed ad hoc (`codesign -s -`).

The emulator stores GUI settings in `~/.config/its-emu/gui.conf`, cached
runtime files (such as the downloaded RP2040 boot ROM and extracted tutorial
assets) in `~/.cache/its-emu/` (or the platform cache directory), and—when
*ST-LINK for pyOCD (VS Code)* is enabled—installs its pyOCD plugin into the
detected VS Code / Arm Environment Manager pyOCD directories.

## Releases

`.github/workflows/release.yml` runs on a tag `v<version>` (matching
`Cargo.toml`), builds the packages and publishes a GitHub release. Run by
hand (*Actions → Release → Run workflow*) it only keeps them as artifacts.
Each platform is one job uploading `package-<platform>`.

```sh
git tag v0.2.0
git push origin v0.2.0
```

