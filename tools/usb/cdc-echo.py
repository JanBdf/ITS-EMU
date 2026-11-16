#!/usr/bin/env python3
"""Measures UsbCdcEcho (tests/fixtures/ac6-src/UsbCdcEcho) on CN13 through its
serial port: round trips of 1, 63, 64 and 512 bytes and the throughput of an
echo of 64 KiB, on the real board or the emulator (its-emu --usb, device 1-2
attached).  Usage: cdc-echo.py /dev/ttyACMn"""
import fcntl
import os
import select
import sys
import termios
import threading
import time

if len(sys.argv) != 2:
    sys.exit(__doc__)
# Non-blocking open: the firmware never reports a carrier.
fd = os.open(sys.argv[1], os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
attr = termios.tcgetattr(fd)
attr[0] = 0                                   # iflag
attr[1] = 0                                   # oflag
attr[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
attr[3] = 0                                   # lflag: raw
termios.tcsetattr(fd, termios.TCSANOW, attr)
termios.tcflush(fd, termios.TCIOFLUSH)
fcntl.fcntl(fd, fcntl.F_SETFL, fcntl.fcntl(fd, fcntl.F_GETFL) & ~os.O_NONBLOCK)


def round_trip(data, timeout=5):
    """Seconds until the echo of `data` is back, or None."""
    start = time.perf_counter()
    os.write(fd, data)
    got = b""
    while len(got) < len(data):
        r, _, _ = select.select([fd], [], [], timeout)
        if not r:
            return None
        got += os.read(fd, 65536)
    if got != data.upper():
        sys.exit(f"wrong echo: {got[:20]!r}")
    return time.perf_counter() - start


for n in (1, 63, 64, 512):
    times = sorted(round_trip(b"a" * n) for _ in range(30))
    if None in times:
        sys.exit(f"{n} bytes: no echo")
    print(f"{n:4} bytes: min {times[0] * 1e3:.3f} ms median {times[15] * 1e3:.3f} ms "
          f"max {times[-1] * 1e3:.3f} ms")

# Throughput: the echo is read while the data is written.
data = bytes(97 + i % 26 for i in range(65536))
got = bytearray()


def reader():
    while len(got) < len(data):
        r, _, _ = select.select([fd], [], [], 5)
        if not r:
            return
        got.extend(os.read(fd, 65536))


thread = threading.Thread(target=reader)
start = time.perf_counter()
thread.start()
os.write(fd, data)
thread.join()
seconds = time.perf_counter() - start
ok = "ok" if bytes(got) == data.upper() else "WRONG"
print(f"64 KiB: {len(got) / seconds / 1024:.0f} KiB/s, {len(got)} bytes {ok}")
