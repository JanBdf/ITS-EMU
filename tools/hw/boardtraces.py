#!/usr/bin/env python3
"""Vectorises the copper traces under the solder mask of a board photo.

    boardtraces.py BOARD.jpg [--debug DIR]

BOARD.jpg is a rectified photo of the bare carrier (without the Nucleo and
the display, so that the traces under them are found too), 1000 design
units wide, e.g. hardware_img/20261017_031521.jpg rectified on the pads of
the board.  Under the mask a trace shows as a band slightly lighter than the
mask around it.  After normalising the local contrast (glare), the bands are
found with oriented line filters for the router's four directions (0°, 45°,
90°, 135°), thinned, fitted with straight segments, merged where collinear
and joined at their bends.  Segments crossing others (traces on one layer
do not cross; these are mostly texture) and short isolated pieces are
dropped.  The segments go to boardtraces.json, which boarddraw.py draws.
"""
import json
import os
import sys

import cv2
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, 'boardtraces.json')
DIRS = [0, 45, 90, 135]
# areas without traces where the photo shows something else: the notch (the
# table shows through), J1, the board edges
EXCLUDE = [(840.0, 88.0, 1010.0, 200.0), (150.0, 0.0, 300.0, 70.0), (-10.0, -10.0, 1010.0, 4.0),
           (-10.0, 644.0, 1010.0, 660.0)]


def line_filter(img, angle, k):
    """Response to dark lines (about 0.3 mm wide) running at `angle`."""
    sa, sc = 5.0 * k, 0.6 * k                 # along / across the line
    r = int(4 * sa)
    ys, xs = np.mgrid[-r:r + 1, -r:r + 1].astype(np.float32)
    c, s = np.cos(np.radians(angle)), np.sin(np.radians(angle))
    u, v = xs * c + ys * s, -xs * s + ys * c   # along, across
    g = np.exp(-u * u / (2 * sa * sa)) * np.exp(-v * v / (2 * sc * sc))
    kern = g * (v * v / sc ** 4 - 1 / sc ** 2)  # second derivative across: positive on dark lines
    kern -= kern.mean()
    kern /= np.abs(kern).sum()
    return cv2.filter2D(img, cv2.CV_32F, kern)


def main(photo, debug=None):
    img = cv2.imread(photo)
    k = img.shape[1] / 1000.0
    hsv = cv2.cvtColor(img, cv2.COLOR_BGR2HSV)
    h, s, v = hsv[..., 0], hsv[..., 1], hsv[..., 2]
    # bare solder mask: no silkscreen, pads, parts
    mask = ((h > 95) & (h < 118) & (s > 35) & (v > 50) & (v < 215)).astype(np.uint8)
    mask = cv2.morphologyEx(mask, cv2.MORPH_OPEN, np.ones((int(3 * k),) * 2, np.uint8))
    mask = cv2.erode(mask, np.ones((int(1.5 * k) | 1,) * 2, np.uint8))
    g = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY).astype(np.float32)
    # local contrast, inverted: the lighter copper bands become dark lines
    mu = cv2.GaussianBlur(g, (0, 0), 4 * k)
    sd = np.sqrt(cv2.GaussianBlur((g - mu) ** 2, (0, 0), 6 * k)) + 3
    g = -(g - mu) / sd * 40
    resp = np.stack([line_filter(g, a, k) for a in DIRS])
    best = resp.max(0) * mask
    which = resp.argmax(0)
    hi, lo = 5.0, 2.2
    strong = (best > hi).astype(np.uint8)
    weak = (best > lo).astype(np.uint8)
    n, lab = cv2.connectedComponents(weak, connectivity=8)
    keep = np.zeros(n, bool)
    keep[np.unique(lab[strong > 0])] = True
    keep[0] = False
    lines_img = (keep[lab]).astype(np.uint8) * 255
    thin = cv2.ximgproc.thinning(lines_img)

    segs = []
    for d, a in enumerate(DIRS):
        sel = np.where((which == d) & (thin > 0), 255, 0).astype(np.uint8)
        found = cv2.HoughLinesP(sel, 1, np.pi / 360, threshold=int(4 * k), minLineLength=int(7 * k),
                                maxLineGap=int(1.5 * k))
        for x0, y0, x1, y1 in (found.reshape(-1, 4) if found is not None else []):
            ang = np.degrees(np.arctan2(y1 - y0, x1 - x0)) % 180
            if min(abs(ang - a), 180 - abs(ang - a)) > 6:
                continue
            p0, p1 = np.array([x0, y0], float) / k, np.array([x1, y1], float) / k
            m, L = (p0 + p1) / 2, np.linalg.norm(p1 - p0)
            u = np.array([np.cos(np.radians(a)), np.sin(np.radians(a))])
            segs.append(np.concatenate([m - u * L / 2, m + u * L / 2]))
    segs = join_bends(merge(segs, 0.6, 3.0), 3.5)
    inside = lambda x, y: any(x0 <= x <= x1 and y0 <= y <= y1 for x0, y0, x1, y1 in EXCLUDE)
    segs = [s for s in segs if np.linalg.norm(s[2:] - s[:2]) > 7.0 and not inside(*s[:2]) and not inside(*s[2:])]
    segs = drop_isolated(drop_crossing(segs))
    out = [[round(float(c), 2) for c in s] for s in segs]
    json.dump(out, open(OUT, 'w'), separators=(',', ':'))
    print(f'{len(out)} segments -> {OUT}')
    if debug:
        os.makedirs(debug, exist_ok=True)
        cv2.imwrite(os.path.join(debug, 'lines.png'), lines_img)
        dbg = img.copy()
        for x0, y0, x1, y1 in out:
            cv2.line(dbg, (int(x0 * k), int(y0 * k)), (int(x1 * k), int(y1 * k)), (0, 0, 255), 2)
        cv2.imwrite(os.path.join(debug, 'traces.jpg'), dbg)


def _crosses(a, b):
    """Segments a and b cross away from their ends."""
    r, s = a[2:] - a[:2], b[2:] - b[:2]
    rxs = r[0] * s[1] - r[1] * s[0]
    if abs(rxs) < 1e-9:
        return False
    q = b[:2] - a[:2]
    t = (q[0] * s[1] - q[1] * s[0]) / rxs
    u = (q[0] * r[1] - q[1] * r[0]) / rxs
    la, lb = np.linalg.norm(r), np.linalg.norm(s)
    return 1 / la < t < 1 - 1 / la and 1 / lb < u < 1 - 1 / lb


def drop_crossing(segs):
    segs = np.array(segs, float)
    lo = np.minimum(segs[:, :2], segs[:, 2:])
    hi = np.maximum(segs[:, :2], segs[:, 2:])
    keep = []
    for i, a in enumerate(segs):
        near = np.where(np.all(lo <= hi[i] + 2, 1) & np.all(hi >= lo[i] - 2, 1))[0]
        keep.append(not any(j != i and _crosses(a, segs[j]) for j in near))
    return list(segs[np.array(keep)])


def _dist(p, a):
    d = a[2:] - a[:2]
    t = np.clip((p - a[:2]) @ d / (d @ d), 0, 1)
    return np.linalg.norm(a[:2] + t * d - p)


def drop_isolated(segs, passes=2):
    """Drops short segments not connected to others (texture of the mask)."""
    for _ in range(passes):
        keep = []
        for i, a in enumerate(segs):
            n = np.linalg.norm(a[2:] - a[:2])
            if n >= 14:
                keep.append(a)
                continue
            ends = sum(any(j != i and _dist(e, b) < 1.6 for j, b in enumerate(segs)) for e in (a[:2], a[2:]))
            if ends == 2 or (ends == 1 and n >= 9):
                keep.append(a)
        segs = keep
    return segs


def merge(segs, tol=0.5, gap=2.0):
    """Joins collinear segments (same direction, offset < tol, gap < `gap` units)."""
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
                if tb[0] > ta[1] + gap or tb[1] < ta[0] - gap:
                    keep.append(b)
                    continue
                lo, hi = min(ta[0], tb[0]), max(ta[1], tb[1])
                a = np.concatenate([a[:2] + ua * lo, a[:2] + ua * hi])
                da = a[2:] - a[:2]
                changed = True
            segs = keep
            out.append(a)
        segs = out
    return segs


def join_bends(segs, reach=3.0):
    """Extends segments of different directions to their intersection when
    their ends are within `reach` units of it (a 45° or 90° bend)."""
    segs = [s.copy() for s in segs]
    for i, a in enumerate(segs):
        for j, b in enumerate(segs):
            if j <= i:
                continue
            da, db = a[2:] - a[:2], b[2:] - b[:2]
            cross = da[0] * db[1] - da[1] * db[0]
            if abs(cross) < 1e-6 * np.linalg.norm(da) * np.linalg.norm(db):
                continue
            t = ((b[0] - a[0]) * db[1] - (b[1] - a[1]) * db[0]) / cross
            p = a[:2] + t * da
            ea = 0 if np.linalg.norm(a[:2] - p) < np.linalg.norm(a[2:] - p) else 1
            eb = 0 if np.linalg.norm(b[:2] - p) < np.linalg.norm(b[2:] - p) else 1
            if np.linalg.norm(a[2 * ea:2 * ea + 2] - p) < reach and np.linalg.norm(b[2 * eb:2 * eb + 2] - p) < reach:
                a[2 * ea:2 * ea + 2] = p
                b[2 * eb:2 * eb + 2] = p
    return segs


if __name__ == '__main__':
    args = [a for a in sys.argv[1:] if not a.startswith('--')]
    dbg = sys.argv[sys.argv.index('--debug') + 1] if '--debug' in sys.argv else None
    if dbg in args:
        args.remove(dbg)
    if not args:
        raise SystemExit(__doc__)
    main(args[0], dbg)
