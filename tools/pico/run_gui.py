#!/usr/bin/env python3
"""Runs a Python program written for a Raspberry Pi Pico on its USB serial
port (such as the GUI of the square wave generator) with the emulated Pico.

    python3 tools/pico/run_gui.py PORT PROGRAM.py [ARGS...]

PORT is the serial port its-emu prints for the Pico (`Pico 0: serial port
/dev/ttys004`). The program runs unchanged; pyserial's port list
(`serial.tools.list_ports.comports()`) also contains PORT, with the USB IDs
of a Pico (2E8A:000A), so programs that look for the Pico find it.
Needs pyserial (`python3 -m pip install pyserial`).
"""

import os
import runpy
import sys

PICO_VID = 0x2E8A
PICO_PID = 0x000A


def add_port(port):
    from serial.tools import list_ports, list_ports_common

    original = list_ports.comports

    def comports(*args, **kwargs):
        ports = [p for p in original(*args, **kwargs) if p.device != port]
        info = list_ports_common.ListPortInfo(port, skip_link_detection=True)
        info.vid, info.pid = PICO_VID, PICO_PID
        info.description = "Raspberry Pi Pico (ITS-EMU)"
        info.manufacturer = "Raspberry Pi"
        info.product = "Pico"
        info.hwid = "USB VID:PID=2E8A:000A"
        return ports + [info]

    list_ports.comports = comports
    # the platform modules are imported under their own names as well
    for name in ("serial.tools.list_ports_posix", "serial.tools.list_ports_windows", "serial.tools.list_ports_osx"):
        module = sys.modules.get(name)
        if module is not None and hasattr(module, "comports"):
            module.comports = comports


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    port, program = sys.argv[1], os.path.abspath(sys.argv[2])
    if not os.path.exists(port):
        sys.exit(f"{port} does not exist: is its-emu running with the Pico?")
    try:
        add_port(port)
    except ImportError:
        sys.exit("pyserial is missing: python3 -m pip install pyserial")
    # as if started with `python3 PROGRAM.py ARGS...` from its directory
    directory = os.path.dirname(program)
    sys.argv = [program] + sys.argv[3:]
    sys.path.insert(0, directory)
    os.chdir(directory)
    runpy.run_path(program, run_name="__main__")


if __name__ == "__main__":
    main()
