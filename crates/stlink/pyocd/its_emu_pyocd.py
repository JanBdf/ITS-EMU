# ITS-Board Emulator: its ST-LINK for pyOCD.
#
# Installed by the ITS-Board Emulator (its-emu-gui) next to a pyOCD; it is
# removed again when the emulator's "ST-LINK for pyOCD" setting is switched
# off. It adds the emulator's ST-LINK to the probes of pyOCD's own `stlink`
# type, so `pyocd ... --probe stlink:` (the course's VS Code configurations)
# finds it like a real ST-LINK on USB. Without a running emulator nothing
# changes: the plugin finds no probe and real ST-LINKs work as before.
#
# The emulator listens on 127.0.0.1:3241 (ITS_EMU_STLINK=HOST:PORT changes
# it; ITS_EMU_STLINK=off disables the plugin). Protocol: crates/stlink/src/tcp.rs.

import logging
import os
import socket
import struct

from pyocd.core import exceptions
from pyocd.core.plugin import Plugin

LOG = logging.getLogger(__name__)

PLUGIN_VERSION = 1
PROTOCOL = 1
DEFAULT_ADDR = "127.0.0.1:3241"

OP_INFO, OP_OPEN, OP_OUT, OP_IN = 0, 1, 2, 3
STATUS_OK, STATUS_BUSY = 0, 1
CMD_SIZE = 16


def _address():
    value = os.environ.get("ITS_EMU_STLINK", DEFAULT_ADDR).strip()
    if value.lower() in ("", "0", "off", "no"):
        return None
    host, _, port = value.rpartition(":")
    return (host or "127.0.0.1", int(port))


def _recv_exact(sock, n):
    data = bytearray()
    while len(data) < n:
        chunk = sock.recv(n - len(data))
        if not chunk:
            raise exceptions.ProbeError("ITS-Board Emulator closed the connection")
        data += chunk
    return data


def _request(sock, op, payload=b""):
    sock.sendall(struct.pack("<BI", op, len(payload)) + payload)


def _reply(sock):
    status, length = struct.unpack("<BI", _recv_exact(sock, 5))
    return status, _recv_exact(sock, length)


class EmulatorSTLinkInterface:
    """Stands in for pyOCD's STLinkUSBInterface (pyocd/probe/stlink/usb.py)."""

    def __init__(self, address, serial, vendor, product):
        self._address = address
        self._serial_number = serial
        self._vendor_name = vendor
        self._product_name = product + " (ITS-Board Emulator)"
        self._sock = None

    @classmethod
    def find(cls):
        """The emulator's probe, or None if no emulator listens."""
        address = _address()
        if address is None:
            return None
        try:
            with socket.create_connection(address, timeout=0.3) as sock:
                sock.settimeout(2)
                _request(sock, OP_INFO)
                status, data = _reply(sock)
        except (OSError, exceptions.ProbeError, struct.error):
            return None
        fields = data.decode("utf-8", "replace").split("\n")
        if status != STATUS_OK or len(fields) < 4 or fields[0] != str(PROTOCOL):
            LOG.warning("ITS-Board Emulator at %s:%d speaks another protocol; update the emulator or "
                        "switch its \"ST-LINK for pyOCD\" off and on again", *address)
            return None
        return cls(address, fields[1], fields[2], fields[3])

    def open(self):
        assert self._sock is None
        try:
            sock = socket.create_connection(self._address, timeout=2)
            sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            _request(sock, OP_OPEN)
            status, _ = _reply(sock)
        except OSError as exc:
            raise exceptions.ProbeError("cannot reach the ITS-Board Emulator: %s" % exc) from exc
        if status == STATUS_BUSY:
            sock.close()
            raise exceptions.ProbeError("the ITS-Board Emulator's ST-LINK is in use by another program")
        self._sock = sock

    def close(self):
        assert self._sock is not None
        self._sock.close()
        self._sock = None

    @property
    def is_open(self):
        return self._sock is not None

    @property
    def serial_number(self):
        return self._serial_number

    @property
    def vendor_name(self):
        return self._vendor_name

    @property
    def product_name(self):
        return self._product_name

    @property
    def version_name(self):
        return "V2-1"

    @property
    def max_packet_size(self):
        return 64

    def transfer(self, cmd, writeData=None, readSize=None, timeout=1000):
        assert self._sock is not None
        assert len(cmd) <= CMD_SIZE
        padded = bytes(cmd) + bytes(CMD_SIZE - len(cmd))
        msg = struct.pack("<BI", OP_OUT, CMD_SIZE) + padded
        if writeData is not None:
            msg += struct.pack("<BI", OP_OUT, len(writeData)) + bytes(writeData)
        if readSize is not None:
            msg += struct.pack("<BI", OP_IN, 4) + struct.pack("<I", readSize)
        try:
            # the emulator may pause between time slices: wait longer than USB
            self._sock.settimeout(max(timeout, 1000) / 1000 * 5)
            self._sock.sendall(msg)
            if readSize is None:
                return None
            status, data = _reply(self._sock)
        except OSError as exc:
            raise exceptions.ProbeError("ITS-Board Emulator: %s" % exc) from exc
        if status != STATUS_OK:
            raise exceptions.ProbeError("ITS-Board Emulator: ST-LINK endpoint stalled (status %d)" % status)
        if len(data) < readSize:
            raise exceptions.ProbeError("received incomplete command response from STLink "
                                        "(got %d, expected %d)" % (len(data), readSize))
        return bytearray(data[:readSize])

    def read_swv(self, size, timeout=1000):
        raise exceptions.ProbeError("the ITS-Board Emulator's ST-LINK has no SWO")

    def __repr__(self):
        return "<EmulatorSTLinkInterface %s:%d>" % self._address


def _patch():
    """Adds the emulator to the devices of pyOCD's ST-LINK probe type."""
    try:
        from pyocd.probe.stlink.usb import STLinkUSBInterface
    except ImportError as exc:
        LOG.warning("ITS-Board Emulator plugin: pyOCD has no STLinkUSBInterface (%s)", exc)
        return
    original = STLinkUSBInterface.__dict__.get("get_all_connected_devices")
    if not isinstance(original, classmethod):
        LOG.warning("ITS-Board Emulator plugin: this pyOCD version is not supported")
        return
    if getattr(original.__func__, "_its_emu", False):
        return

    def get_all_connected_devices(cls):
        devices = original.__func__(cls)
        emulator = EmulatorSTLinkInterface.find()
        if emulator is not None:
            devices.append(emulator)
        return devices

    get_all_connected_devices._its_emu = True
    STLinkUSBInterface.get_all_connected_devices = classmethod(get_all_connected_devices)


class ITSEmuProbePlugin(Plugin):
    """Registers no probe type of its own: it extends pyOCD's `stlink` type
    when pyOCD loads its probe plugins, whatever order they load in."""

    def should_load(self):
        _patch()
        return False

    def load(self):
        return None

    @property
    def name(self):
        return "its-emu"

    @property
    def description(self):
        return "ITS-Board Emulator ST-LINK (adds to the stlink probe type)"
