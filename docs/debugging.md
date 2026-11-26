# Debugging

## Debugging with GDB

The GUI serves GDB on `127.0.0.1:3333` (`--gdb [ADDR]`, `--no-gdb`, or via the
*Emulation → GDB server* toggle / status bar); the CLI
waits for a debugger with `--gdb [ADDR]`. Connecting halts the core,
detaching resumes it. Without a firmware file both start with empty flash
and the debugger loads the program:

```sh
arm-none-eabi-gdb Program.axf -ex "target remote :3333"
(gdb) monitor load Program.axf
(gdb) tbreak main
(gdb) continue
```

`monitor load FILE` flashes an `.axf` (path relative to the emulator's
working directory) by its segments like pyOCD, resets and stays halted; the
GUI then shows its name and symbols. Do not use GDB's own `load` for course
programs: it writes sections at their run addresses, so AC6's `.data` initial
values (`RW_IRAM1`) land in RAM instead of flash, where startup copies them from.

The course's VS Code configurations start pyOCD with `--probe stlink:`,
which only finds an ST-LINK. The emulator can be one: on USB on Linux
([below](#the-boards-st-link-on-usb-linux)), through a pyOCD plugin elsewhere
([below](#the-boards-st-link-for-pyocd-macos-windows)). Otherwise add this to the
course project's `.vscode/launch.json` (CDT GDB adapter). Like
**STLink@pyOCD (launch)**, it loads the active CMSIS project's last build,
resets and stops at `main`:

```json
{
    "name": "ITS-Board Emulator",
    "type": "gdbtarget",
    "request": "attach",
    "cwd": "${command:cmsis-csolution.getSolutionDir}",
    "program": "${command:cmsis-csolution.getBinaryFile}",
    "gdb": "arm-none-eabi-gdb",
    "auxiliaryGdb": true,
    "initCommands": [
        "monitor load ${command:cmsis-csolution.getSolutionDir}/${command:cmsis-csolution.getBinaryFile}",
        "maintenance flush register-cache",
        "tbreak main"
    ],
    "customResetCommands": ["monitor reset halt", "maintenance flush register-cache", "tbreak main", "continue"],
    "target": { "port": "3333" }
}
```

The CDT adapter treats *Run Without Debugging* (Ctrl+F5) like F5. The
extension in [`tools/vscode-nodebug`](../tools/vscode-nodebug/README.md) drops
the `tbreak`s in that case, so the program just runs:

```sh
tools/vscode-nodebug/package.sh
code --install-extension tools/vscode-nodebug/gdbtarget-nodebug.vsix
```

Several debuggers may connect: the first controls the core, the others (e.g.
the CDT adapter's auxiliary GDB for live watch) access memory and registers
while it runs. Breakpoints do not modify memory, so they work in flash.
Watchpoints slow emulation while set.

## The board's ST-LINK on USB (Linux)

The emulator can be the Nucleo's ST-LINK/V2-1 (0483:3752, without mass
storage) on your computer's USB, exported over USB/IP and attached with the
kernel's `vhci-hcd`. The course tools then work unchanged: *CMSIS Load*
flashes with pyOCD and the pack's flash algorithm (running on the emulated
core), *STLink@pyOCD (launch)* debugs, and a terminal talks to `/dev/ttyACM0`.

With the [Arch](development.md#arch-linux-package) or
[Debian / Ubuntu](development.md#debian--ubuntu-package) package, the GUI plugs
the ST-LINK in by itself. **Emulation → USB ST-LINK plugged in** (on by
default, remembered in `~/.config/its-emu/gui.conf`), the CN1 USB cable on the
board, and the status bar show and toggle its state; quitting or switching off
unplugs it. After a kernel update, reboot first: pacman removes
the running kernel's modules, so vhci-hcd (if not loaded yet) and cdc_acm,
which gives the serial ports `/dev/ttyACM*`, cannot load until then. The
emulator's own GDB server yields port 3333 to pyOCD while pyOCD debugs and
takes it back afterwards, so both debug configurations work side by side.
Unplugging the virtual ST-LINK also stops the built-in GDB server (unless a GDB
client is actively attached) so pyOCD can debug a real board without closing
the emulator; plugging it back in restores the GDB server.

Without the package:

```sh
sudo pacman -S usbip            # Debian: apt install usbip; Ubuntu: linux-tools-generic
sudo modprobe vhci-hcd          # once per boot (or /etc/modules-load.d)

its-emu-gui                     # attaches with `sudo -n usbip attach` (passwordless sudo)
its-emu Program.axf --usb --usb-attach
sudo usbip attach -r 127.0.0.1 -b 1-1    # by hand (its-emu-gui --no-usb-attach, its-emu --usb)
```

`its-emu-gui --no-usb` / `--usb[=ADDR]` override the switch for one run.
The same server exports the firmware's USB device on [CN13](usage.md#usb-on-cn13-linux)
and that of each [Pico](usage.md#raspberry-pi-pico) as `1-2`, `1-3`, ..., in
the order they connect (`its-emu --usb --usb-attach` and the GUI plug those
in as well; by hand: `sudo usbip attach -r 127.0.0.1 -b 1-2`). The
udev rules pyOCD/OpenOCD install for the real probe (e.g. `60-openocd.rules`)
cover the emulated one. Other machines can attach it with `--usb
0.0.0.0:3240` and `usbip attach -r HOST -b 1-1` (usbip-win2 on Windows,
untested).

## The board's ST-LINK for pyOCD (macOS, Windows)

Where USB/IP devices cannot be attached, the GUI serves the same ST-LINK
to pyOCD over TCP (127.0.0.1:3241). At its first start (outside Linux) it
asks whether to; **Emulation → ST-LINK for pyOCD (VS Code)** switches it
later (`pyocd_stlink` in `gui.conf`; `--usb` uses USB/IP for one run).

Switched on, the emulator adds a plugin (`its_emu_pyocd.py` and a
`.dist-info` directory) to the frozen pyOCD of VS Code's Arm CMSIS
Debugger extension (`~/.vscode/extensions/arm.vscode-cmsis-debugger-*/tools/pyocd/_internal`,
also `.vscode-insiders`, `.cursor` …, and pyOCDs of the Arm Environment
Manager in `~/.vcpkg/artifacts`), at every start, since an update of the
extension comes without it. Switched off, it removes the plugin again. The
plugin adds the emulator to the devices of pyOCD's own `stlink` probe type,
so *CMSIS Load* and *STLink@pyOCD* work unchanged; real ST-LINKs keep
working, and without a running emulator it finds nothing.
`ITS_EMU_STLINK=HOST:PORT` points it elsewhere, `ITS_EMU_STLINK=off`
disables it. The protocol is in `crates/stlink/src/tcp.rs`.

The virtual COM port is a pseudo-terminal (`/dev/ttys…`, shown in the
status bar; click copies it). It is initialized in raw mode on creation;
terminal programs may still configure termios settings as needed.
Windows has no pseudo-terminals: use the serial console there.

```sh
its-emu --install-pyocd-plugin          # the CLI: install / --uninstall-pyocd-plugin
its-emu Program.axf --pyocd             # serve the ST-LINK (prints the COM port)
```

The plugin patches a method of pyOCD's ST-LINK driver
(`STLinkUSBInterface.get_all_connected_devices`); it warns and does nothing
if a pyOCD version lacks it. Tested with the extension's pyOCD 0.46.1.

While the probe halts the core, peripherals and the flash interface keep
running (only the core, SysTick and DWT->CYCCNT stop, as on the chip), so
OpenOCD's register-level flash driver works. With the probe plugged in, a
BKPT without a debugger is a HardFault, as on the chip.

Known differences: no mass storage drive and no SWO; the COM port ignores
the baud rate; programming non-erased flash bits stores the new value (the
chip ANDs); a flash store with a wrong size or without PG faults instead of
only setting PGPERR/PGSERR. ROM table, component IDs, DBGMCU_IDCODE
(device 0x419, revision 0x2003), CPUID and the processor feature registers
are those read on the measured board (`tests/hardware/probe-ids.txt`).

