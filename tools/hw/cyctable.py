#!/usr/bin/env python3
"""Compares the HwCheck `cyc` output of the board and the emulator:
cycles per kernel body (10 copies per loop iteration, 1000 iterations,
loop overhead from the empty kernel subtracted).
Usage: cyctable.py board.txt emu.txt"""
import sys


def load(path):
    out = {}
    for line in open(path):
        parts = line.split()
        if len(parts) == 3 and parts[0] == "C":
            out[parts[1]] = int(parts[2])
    return out


board, emu = load(sys.argv[1]), load(sys.argv[2])
print(f"{'kernel':14s} {'board':>7s} {'emu':>7s}   per body: board   emu")
for name, b in board.items():
    e = emu.get(name)
    pb = (b - board["empty"]) / 10000
    pe = (e - emu["empty"]) / 10000 if e is not None else float("nan")
    mark = "" if abs(pb - pe) < 0.05 else "  <--"
    print(f"{name:14s} {b:7d} {e if e is not None else -1:7d}   {pb:14.2f} {pe:5.2f}{mark}")
print(f"loop overhead per iteration: board {board['empty'] / 1000:.2f}, emu {emu['empty'] / 1000:.2f}")
