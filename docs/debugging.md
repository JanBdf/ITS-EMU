# Debugging

## Debugging with GDB

The GUI serves GDB on `127.0.0.1:3333` (`--gdb ADDR`, `--no-gdb`); the CLI
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
which only finds a USB ST-LINK. On Linux the emulator can be one
([below](#the-boards-st-link-on-usb-linux)); otherwise add this to the
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

With the [Arch package](development.md#arch-linux-package), the GUI plugs the ST-LINK in
by itself. The toolbar switch **USB ST-LINK** (on by default, remembered in
`~/.config/its-emu/gui.conf`) and the status bar show its state; quitting or
switching off unplugs it. After a kernel update, reboot first: pacman removes
the running kernel's modules, so vhci-hcd cannot load until then. The
emulator's own GDB server yields port 3333 to pyOCD while pyOCD debugs and
takes it back afterwards, so both debug configurations work side by side.

Without the package:

```sh
sudo pacman -S usbip            # Debian: apt install usbip; Ubuntu: linux-tools-generic
sudo modprobe vhci-hcd          # once per boot (or /etc/modules-load.d)

its-emu-gui                     # attaches with `sudo -n usbip attach` (passwordless sudo)
its-emu Program.axf --usb --usb-attach
sudo usbip attach -r 127.0.0.1 -b 1-1    # by hand (its-emu-gui --no-usb-attach, its-emu --usb)
```

`its-emu-gui --no-usb` / `--usb[=ADDR]` override the switch for one run. The
udev rules pyOCD/OpenOCD install for the real probe (e.g. `60-openocd.rules`)
cover the emulated one. Other machines can attach it with `--usb
0.0.0.0:3240` and `usbip attach -r HOST -b 1-1` (usbip-win2 on Windows,
untested).

While the probe halts the core, peripherals and the flash interface keep
running (only the core, SysTick and DWT->CYCCNT stop, as on the chip), so
OpenOCD's register-level flash driver works. With the probe plugged in, a
BKPT without a debugger is a HardFault, as on the chip.

Known differences: no mass storage drive and no SWO; the COM port ignores
the baud rate; programming non-erased flash bits stores the new value (the
chip ANDs); a flash store with a wrong size or without PG faults instead of
only setting PGPERR/PGSERR. ROM table and DBGMCU_IDCODE use ST's STM32F429
values (device 0x419, revision 3).

