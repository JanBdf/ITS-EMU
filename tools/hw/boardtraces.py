#!/usr/bin/env python3
"""Vectorises the copper traces under the solder mask of the board photo.

    boardtraces.py BOARD.jpg [--debug DIR]

BOARD.jpg is the rectified photo (boardasset.py, 1000 design units wide).
Under the mask the traces show only as a slight relief: a thin dark edge
along each side.  The edges are found as dark ridges against the local mask
colour, thinned and fitted with straight segments; only directions in steps
of 45° (the router's) are kept.  The segments go to boardtraces.json, which
boarddraw.py draws as fine lines.
"""
import json
import os
import sys

import cv2
import numpy as np

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'boardtraces.json')


def main(photo, debug=None):
    img = cv2.imread(photo)
    k = img.shape[1] / 1000.0
    hsv = cv2.cvtColor(img, cv2.COLOR_BGR2HSV).astype(np.float32)
    h, s, v = hsv[..., 0], hsv[..., 1], hsv[..., 2]
    mask = ((h > 95) & (h < 125) & (s > 90) & (v > 80) & (v < 215)).astype(np.uint8)
    mask = cv2.erode(mask, np.ones((int(k * 1.2) | 1,) * 2, np.uint8))
    g = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY).astype(np.float32)
    sig = 4 * k
    bg = cv2.GaussianBlur(g * mask, (0, 0), sig) / (cv2.GaussianBlur(mask.astype(np.float32), (0, 0), sig) + 1e-3)
    d = cv2.GaussianBlur((g - bg) * mask, (0, 0), 0.3 * k)
    ridge = ((d < -7) & (mask > 0)).astype(np.uint8) * 255
    ridge = cv2.morphologyEx(ridge, cv2.MORPH_OPEN, np.ones((2, 2), np.uint8))
    thin = cv2.ximgproc.thinning(ridge)
    lines = cv2.HoughLinesP(thin, 1, np.pi / 360, threshold=int(4 * k), minLineLength=int(5 * k),
                            maxLineGap=int(1.2 * k))
    segs = []
    for x0, y0, x1, y1 in (lines.reshape(-1, 4) if lines is not None else []):
        a = np.degrees(np.arctan2(y1 - y0, x1 - x0)) % 180
        snap = round(a / 45) * 45
        if abs(a - snap) > 3:
            continue
        p0, p1 = np.array([x0, y0], float) / k, np.array([x1, y1], float) / k
        # snap the direction exactly, around the midpoint
        m, L = (p0 + p1) / 2, np.linalg.norm(p1 - p0)
        u = np.array([np.cos(np.radians(snap)), np.sin(np.radians(snap))])
        p0, p1 = m - u * L / 2, m + u * L / 2
        segs.append([round(c, 2) for c in (*p0, *p1)])
    segs = merge(segs)
    json.dump(segs, open(OUT, 'w'), separators=(',', ':'))
    print(f'{len(segs)} segments -> {OUT}')
    if debug:
        os.makedirs(debug, exist_ok=True)
        cv2.imwrite(os.path.join(debug, 'ridge.png'), ridge)
        dbg = img.copy()
        for x0, y0, x1, y1 in segs:
            cv2.line(dbg, (int(x0 * k), int(y0 * k)), (int(x1 * k), int(y1 * k)), (0, 0, 255), 2)
        cv2.imwrite(os.path.join(debug, 'traces.jpg'), dbg)


def merge(segs, tol=0.6):
    """Joins collinear overlapping segments (same direction, offset < tol units)."""
    segs = [np.array(s, float) for s in segs]
    changed = True
    while changed:
        changed = False
        out = []
        while segs:
            a = segs.pop()
            da = a[2:] - a[:2]
            ua = da / (np.linalg.norm(da) + 1e-9)
            na = np.array([-ua[1], ua[0]])
            keep = []
            for b in segs:
                db = b[2:] - b[:2]
                ub = db / (np.linalg.norm(db) + 1e-9)
                if abs(abs(ua @ ub) - 1) > 1e-3 or abs((b[:2] - a[:2]) @ na) > tol:
                    keep.append(b)
                    continue
                ta = sorted([0, da @ ua])
                tb = sorted([(b[:2] - a[:2]) @ ua, (b[2:] - a[:2]) @ ua])
                if tb[0] > ta[1] + 1.0 or tb[1] < ta[0] - 1.0:
                    keep.append(b)
                    continue
                lo, hi = min(ta[0], tb[0]), max(ta[1], tb[1])
                a = np.concatenate([a[:2] + ua * lo, a[:2] + ua * hi])
                da = a[2:] - a[:2]
                changed = True
            segs = keep
            out.append(a)
        segs = out
    return [[round(float(c), 2) for c in s] for s in segs]


if __name__ == '__main__':
    dbg = sys.argv[sys.argv.index('--debug') + 1] if '--debug' in sys.argv else None
    main(sys.argv[1], dbg)
