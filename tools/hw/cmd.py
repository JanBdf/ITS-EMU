#!/usr/bin/env python3
"""Sends HwCheck commands to the real board over the ST-LINK virtual COM port
and prints the answers.  Usage: cmd.py [--port PORT] [--timeout S] [--wait-ready] CMD...
The default port is the virtual COM port of the real board's ST-LINK
(tools/hw/board-serial.sh), not that of an emulator exported over USB/IP.
Each command is sent as one line; the script waits for "ok <cmd>" or "? <cmd>"
(at most S seconds, default 10, between two lines).
"sleep SECONDS" waits between commands."""
import os
import select
import subprocess
import sys
import termios
import time

port = None
args = sys.argv[1:]
wait_ready = False
timeout = 10.0
while args and args[0].startswith("--"):
    if args[0] == "--timeout":
        timeout = float(args[1])
        args = args[2:]
    elif args[0] == "--port":
        port = args[1]
        args = args[2:]
    elif args[0] == "--wait-ready":
        wait_ready = True
        args = args[1:]
    else:
        sys.exit(__doc__)

if port is None:
    serial = subprocess.run([os.path.join(os.path.dirname(__file__), "board-serial.sh")],
                            capture_output=True, text=True, check=True).stdout.strip()
    port = f"/dev/serial/by-id/usb-STMicroelectronics_STM32_STLink_{serial}-if02"
fd = os.open(port, os.O_RDWR | os.O_NOCTTY)
attr = termios.tcgetattr(fd)
attr[0] = 0                                   # iflag
attr[1] = 0                                   # oflag
attr[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
attr[3] = 0                                   # lflag: raw
attr[4] = attr[5] = termios.B115200
termios.tcsetattr(fd, termios.TCSANOW, attr)

buf = b""


def read_line(timeout):
    global buf
    end = time.time() + timeout
    while b"\n" not in buf:
        left = end - time.time()
        if left <= 0:
            return None
        r, _, _ = select.select([fd], [], [], left)
        if r:
            buf += os.read(fd, 4096)
    line, buf = buf.split(b"\n", 1)
    return line.decode(errors="replace").rstrip("\r")


if wait_ready:
    while True:
        line = read_line(10)
        if line is None:
            sys.exit("no 'ready' from board")
        print(line)
        if line == "ready":
            break
else:
    termios.tcflush(fd, termios.TCIFLUSH)

for cmd in args:
    if cmd.startswith("sleep "):
        time.sleep(float(cmd.split()[1]))
        continue
    os.write(fd, (cmd + "\n").encode())
    name = cmd.split()[0]
    while True:
        line = read_line(timeout)
        if line is None:
            sys.exit(f"timeout waiting for 'ok {name}'")
        print(line)
        if line in (f"ok {name}", f"? {name}"):
            break
