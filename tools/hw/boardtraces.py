#!/usr/bin/env python3
"""Vectorises the copper traces and tented vias under the solder mask from
hardware photos of the bare carrier board.

    boardtraces.py RECT_031521.jpg [CLOSEUP_RECT.jpg ...] [--debug DIR]

RECT_031521.jpg is a rectified photo of the bare carrier (e.g.
hardware_img/20261017_031521.jpg rectified to 1000 design units wide at K px/unit,
e.g. 6000x3891 at K=6). Optional additional rectified close-up photos
(031822, 031828, 031833, 031839) are used to cross-validate traces and vias in
glare regions.

Methods:
- Vias: Compact 21x21 radially-symmetrized normalized cross-correlation (NCC)
  matching the empirical tented-via profile (dark center hole r<=0.33u, bright
  raised annular ring r~0.83u, dark isolation moat r~1.5u), verified with an
  8-sector bright-ring check at r=0.87u (rejects 1D trace grooves) and multi-photo
  confirmation in glare regions -> tools/hw/boardvias.json.
- Traces: Non-linear Dual-Groove Ridge Filter
      sqrt(max(0, L_center - L_left) * max(0, L_center - L_right))
  across the router's four octilinear directions (0°, 45°, 90°, 135°) on
  noise-floored LAB luminance, with perpendicular non-maximum suppression,
  direct oriented chain tracing (no HoughLinesP), O(N log N) collinear merging,
  smart crossing removal, 45°/90° bend joining, and via endpoint snapping
  -> tools/hw/boardtraces.json.
"""
import json
import os
import re
import sys

import cv2
import numpy as np
from scipy.ndimage import gaussian_filter, maximum_filter

HERE = os.path.dirname(os.path.abspath(__file__))
BOARD_RS = os.path.join(HERE, '..', '..', 'apps', 'its-emu-gui', 'src', 'board.rs')
OUT_TRACES = os.path.join(HERE, 'boardtraces.json')
OUT_VIAS = os.path.join(HERE, 'boardvias.json')
DIRS = [0, 45, 90, 135]

# 8 clearly visible staircase vias below R31..R24 used to bootstrap the empirical radial via template
SEED_VIAS = [
    (786.83, 264.00), (797.17, 266.83), (807.83, 269.33), (818.00, 272.17),
    (828.67, 275.17), (839.17, 279.00), (849.67, 282.33), (860.00, 285.83),
]


def _rs_const(name):
    m = re.search(rf'const {name}: [^=]*=\s*([^;]*);', open(BOARD_RS).read())
    v = [float(x) for x in re.findall(r'-?\d+(?:\.\d+)?', m.group(1))]
    return v[0] if len(v) == 1 else v


def build_exclusion_mask(H, W, K):
    """Mask out board edges, notch, headers, ICs, pads, LEDs, switches, mounting holes and logo."""
    excl = np.zeros((H, W), np.uint8)

    def ex_rect(x0, y0, x1, y1):
        excl[max(0, int(y0 * K)):min(H, int(y1 * K)), max(0, int(x0 * K)):min(W, int(x1 * K))] = 255

    def ex_circ(x, y, r):
        cv2.circle(excl, (int(round(x * K)), int(round(y * K))), int(round(r * K)), 255, -1)

    ex_rect(0, 0, 1000, 4); ex_rect(0, 643, 1000, 650)
    ex_rect(0, 0, 4, 650); ex_rect(995, 0, 1000, 650)
    ex_rect(842, 88, 1000, 195)   # USB notch
    ex_rect(42, 622, 70, 644)     # embossed "Top" copper text
    ex_rect(25, 110, 290, 280)    # HAW logo block

    out_pads = [_rs_const('OUT_PAD0') + i * _rs_const('OUT_PITCH') for i in range(18)]
    in_pads = [_rs_const('IN_PAD0') + i * _rs_const('IN_PITCH') for i in range(10)]
    an_pads = _rs_const('ANALOG_X')
    aux_x, aux_y = _rs_const('AUX_X'), _rs_const('AUX_Y')
    i2c_pads = _rs_const('HDR_PADS_Y')
    i2c_x, spi_x = _rs_const('I2C_PAD_X'), _rs_const('SPI_PAD_X')
    row_y = _rs_const('ROW_Y')
    tim_pads = [_rs_const('TIM_GND')] + [_rs_const('TIM_PAD0') + i * _rs_const('TIM_PITCH') for i in range(10)]
    tim_y = _rs_const('TIM_Y')
    led_y = _rs_const('LED_Y')
    btn_x, btn_y = _rs_const('BUTTON_X'), _rs_const('BUTTON_Y')
    out_x, in_x = _rs_const('OUT_X'), _rs_const('IN_X')

    for x, y in [(14.7, 13.7), (985.3, 13.7), (14.7, 634.5), (620.0, 634.5), (985.3, 211.0), (985.3, 634.5),
                 (419.0, 115.4), (419.0, 303.0), (735.1, 110.6), (729.9, 311.0), (15.0, 326.0)]:
        ex_circ(x, y, 8.8)
    for x in out_pads: ex_circ(x, row_y, 9.0)
    for x in in_pads: ex_circ(x, row_y, 9.0)
    for x in an_pads: ex_circ(x, 448.6, 9.0)
    for x in tim_pads: ex_circ(x, tim_y, 9.0)
    for y in aux_y: ex_circ(aux_x, y, 9.0)
    for y in i2c_pads:
        ex_circ(i2c_x, y, 9.0); ex_circ(spi_x, y, 9.0)
    for x in out_x: ex_rect(x - 10, led_y - 13, x + 10, led_y + 13)
    for x in in_x:  ex_rect(x - 10, led_y - 13, x + 10, led_y + 13)
    for x in btn_x: ex_rect(x - 15, btn_y - 15, x + 15, btn_y + 15)
    ex_rect(915, 28, 970, 56)

    for x0, y0, x1, y1 in [
        (20, 11, 152, 65), (155, 0, 294, 67),
        (342, 71, 728, 98), (342, 322, 728, 350),
        (709, 18, 842, 39), (522, 392, 638, 412), (912, 302, 941, 377),
        (246, 482, 444, 504), (742, 482, 855, 504),
        (112, 330, 132, 378), (112, 403, 132, 451), (303, 382, 323, 452),
        (30, 315, 100, 380),
        (584, 174, 652, 218), (490, 174, 558, 218), (396, 174, 464, 218),
        (346, 155, 376, 186), (234, 77, 290, 134), (66, 81, 93, 99),
        (104, 83, 161, 98), (718, 135, 750, 185), (812, 128, 855, 168),
        (696, 140, 718, 175), (758, 140, 780, 175),
        (376, 250, 460, 265), (492, 250, 576, 265), (587, 250, 670, 265),
        (780, 245, 864, 261), (550, 376, 626, 392), (594, 103, 612, 145),
    ]:
        ex_rect(x0, y0, x1, y1)
    return excl


def _build_radial_via_template(L0, K):
    r_t = int(round(1.67 * K))
    patches = []
    for x, y in SEED_VIAS:
        cx, cy = int(round(x * K)), int(round(y * K))
        p = L0[cy - r_t:cy + r_t + 1, cx - r_t:cx + r_t + 1].copy()
        p = (p - p.mean()) / (p.std() + 1e-6)
        patches.append(p)
    t2d = np.mean(patches, axis=0).astype(np.float32)
    yy, xx = np.mgrid[-r_t:r_t + 1, -r_t:r_t + 1]
    rr = np.sqrt(xx ** 2 + yy ** 2)
    trad = np.zeros_like(t2d)
    for r0 in range(0, r_t + 5):
        m = (rr >= r0 - 0.5) & (rr < r0 + 0.5)
        if m.any():
            trad[m] = t2d[m].mean()
    return ((trad - trad.mean()) / (trad.std() + 1e-6)).astype(np.float32), r_t


def _via_maps(im_bgr, tmpl_rad, r_t, K):
    lab = cv2.cvtColor(im_bgr, cv2.COLOR_BGR2LAB).astype(np.float32)
    L = lab[..., 0]
    hsv = cv2.cvtColor(im_bgr, cv2.COLOR_BGR2HSV)
    B, R = im_bgr[..., 0].astype(np.float32), im_bgr[..., 2].astype(np.float32)
    blue = ((B - R > 10) & (hsv[..., 0] >= 88) & (hsv[..., 0] <= 126) & (hsv[..., 2] > 35)).astype(np.float32)
    w_box = int(round(3.17 * K)) | 1
    blue_frac = cv2.boxFilter(blue, cv2.CV_32F, (w_box, w_box))
    ncc = cv2.matchTemplate(L, tmpl_rad, cv2.TM_CCOEFF_NORMED)
    ncc = np.pad(ncc, r_t, mode='constant', constant_values=0)
    g_core = gaussian_filter(L, sigma=0.22 * K)
    sec_diffs = []
    for ang in np.linspace(0, 2 * np.pi, 8, endpoint=False):
        dx, dy = int(round(0.87 * K * np.cos(ang))), int(round(0.87 * K * np.sin(ang)))
        sec_diffs.append(np.roll(np.roll(g_core, -dy, axis=0), -dx, axis=1) - g_core)
    sec_sorted = np.sort(np.stack(sec_diffs, axis=0), axis=0)
    return ncc, sec_sorted[1], np.median(sec_sorted, axis=0), blue_frac


def _oriented_blur(arr, angle_deg, sigma_along, sigma_across):
    r = int(np.ceil(2.8 * max(sigma_along, sigma_across)))
    ys, xs = np.mgrid[-r:r + 1, -r:r + 1].astype(np.float32)
    c, s = np.cos(np.radians(angle_deg)), np.sin(np.radians(angle_deg))
    u, v = xs * c + ys * s, -xs * s + ys * c
    kern = np.exp(-0.5 * (u / sigma_along) ** 2 - 0.5 * (v / sigma_across) ** 2)
    kern /= kern.sum()
    return cv2.filter2D(arr, cv2.CV_32F, kern)


def _dual_groove_response(im_bgr, K):
    H, W = im_bgr.shape[:2]
    L = cv2.cvtColor(im_bgr, cv2.COLOR_BGR2LAB)[..., 0].astype(np.float32)
    mu = gaussian_filter(L, sigma=4.0 * K)
    sd = np.sqrt(gaussian_filter((L - mu) ** 2, sigma=6.0 * K)) + 11.0
    L_norm = (L - mu) / sd * 25.0
    resps = []
    for ang in DIRS:
        L_s = _oriented_blur(L_norm, ang, sigma_along=1.25 * K, sigma_across=0.22 * K)
        nx, ny = -np.sin(np.radians(ang)), np.cos(np.radians(ang))
        resp_a = np.zeros((H, W), np.float32)
        for dist_u in (0.83, 1.08, 1.33):
            dx, dy = int(round(nx * dist_u * K)), int(round(ny * dist_u * K))
            L_pos = np.roll(np.roll(L_s, -dy, axis=0), -dx, axis=1)
            L_neg = np.roll(np.roll(L_s, dy, axis=0), dx, axis=1)
            resp_a = np.maximum(resp_a, np.sqrt(np.maximum(0.0, L_s - L_pos) * np.maximum(0.0, L_s - L_neg)))
        resps.append(resp_a)
    return resps


def merge_collinear(segs, tol=1.10, gap=4.2):
    buckets = {0: [], 1: [], 2: [], 3: []}
    vecs = [(np.cos(np.radians(a)), np.sin(np.radians(a))) for a in DIRS]
    for s in segs:
        s = np.asarray(s, float)
        dx, dy = s[2] - s[0], s[3] - s[1]
        ang = np.degrees(np.arctan2(dy, dx)) % 180
        d = min(range(4), key=lambda k: min(abs(ang - DIRS[k]), 180 - abs(ang - DIRS[k])))
        c, si = vecs[d]
        u, n = np.array([c, si]), np.array([-si, c])
        t0, t1 = sorted([s[:2] @ u, s[2:] @ u])
        v = 0.5 * (s[:2] @ n + s[2:] @ n)
        buckets[d].append([v, t0, t1])

    out = []
    for d, items in buckets.items():
        if not items:
            continue
        c, si = vecs[d]
        u, n = np.array([c, si]), np.array([-si, c])
        items.sort(key=lambda x: (x[0], x[1]))
        used = [False] * len(items)
        for i in range(len(items)):
            if used[i]:
                continue
            used[i] = True
            v_acc, w_acc = [items[i][0]], [items[i][2] - items[i][1]]
            t0, t1 = items[i][1], items[i][2]
            changed = True
            while changed:
                changed = False
                v_cur = np.average(v_acc, weights=w_acc)
                for j in range(i + 1, len(items)):
                    if used[j]:
                        continue
                    if items[j][0] - v_cur > tol + 1.0:
                        break
                    if abs(items[j][0] - v_cur) <= tol and not (items[j][1] > t1 + gap or items[j][2] < t0 - gap):
                        used[j] = True
                        v_acc.append(items[j][0])
                        w_acc.append(items[j][2] - items[j][1])
                        t0, t1 = min(t0, items[j][1]), max(t1, items[j][2])
                        changed = True
            v_fin = np.average(v_acc, weights=w_acc)
            out.append(np.concatenate([u * t0 + n * v_fin, u * t1 + n * v_fin]))
    return out


def _crosses(a, b):
    r, s = a[2:] - a[:2], b[2:] - b[:2]
    rxs = r[0] * s[1] - r[1] * s[0]
    if abs(rxs) < 1e-9:
        return False
    q = b[:2] - a[:2]
    t = (q[0] * s[1] - q[1] * s[0]) / rxs
    u = (q[0] * r[1] - q[1] * r[0]) / rxs
    la, lb = np.linalg.norm(r), np.linalg.norm(s)
    ma, mb = min(0.25, 1.2 / (la + 1e-6)), min(0.25, 1.2 / (lb + 1e-6))
    return ma < t < 1 - ma and mb < u < 1 - mb


def drop_crossing_smart(segs):
    segs = [np.asarray(s, float) for s in segs]
    arr = np.array(segs)
    lens = np.linalg.norm(arr[:, 2:] - arr[:, :2], axis=1)
    lo, hi = np.minimum(arr[:, :2], arr[:, 2:]), np.maximum(arr[:, :2], arr[:, 2:])
    n_s = len(segs)
    adj = [set() for _ in range(n_s)]
    for i in range(n_s):
        near = np.where((lo[i + 1:, 0] <= hi[i, 0] + 1) & (hi[i + 1:, 0] >= lo[i, 0] - 1) &
                        (lo[i + 1:, 1] <= hi[i, 1] + 1) & (hi[i + 1:, 1] >= lo[i, 1] - 1))[0] + (i + 1)
        for j in near:
            if _crosses(arr[i], arr[j]):
                adj[i].add(j); adj[j].add(i)
    alive = [True] * n_s
    while True:
        worst, worst_key = -1, (-1, 0.0)
        for i in range(n_s):
            if not alive[i]:
                continue
            deg = sum(1 for j in adj[i] if alive[j])
            if deg > 0 and (deg, -lens[i]) > worst_key:
                worst_key, worst = (deg, -lens[i]), i
        if worst == -1:
            break
        alive[worst] = False
    return [segs[i] for i in range(n_s) if alive[i]]


def join_bends(segs, reach=3.2):
    segs = [s.copy() for s in segs]
    n_s = len(segs)
    if n_s == 0:
        return segs
    arr = np.array(segs)
    lo, hi = np.minimum(arr[:, :2], arr[:, 2:]) - reach, np.maximum(arr[:, :2], arr[:, 2:]) + reach
    for i in range(n_s):
        a = segs[i]
        da = a[2:] - a[:2]
        la = np.linalg.norm(da)
        cand = np.where((lo[i + 1:, 0] <= hi[i, 0]) & (hi[i + 1:, 0] >= lo[i, 0]) &
                        (lo[i + 1:, 1] <= hi[i, 1]) & (hi[i + 1:, 1] >= lo[i, 1]))[0] + (i + 1)
        for j in cand:
            b = segs[j]
            db = b[2:] - b[:2]
            cross = da[0] * db[1] - da[1] * db[0]
            if abs(cross) < 1e-4 * la * np.linalg.norm(db):
                continue
            t = ((b[0] - a[0]) * db[1] - (b[1] - a[1]) * db[0]) / cross
            p = a[:2] + t * da
            ea = 0 if np.linalg.norm(a[:2] - p) < np.linalg.norm(a[2:] - p) else 1
            eb = 0 if np.linalg.norm(b[:2] - p) < np.linalg.norm(b[2:] - p) else 1
            if np.linalg.norm(a[2 * ea:2 * ea + 2] - p) < reach and np.linalg.norm(b[2 * eb:2 * eb + 2] - p) < reach:
                a[2 * ea:2 * ea + 2] = p
                b[2 * eb:2 * eb + 2] = p
    return segs


def _dist_pt_seg(p, a):
    d = a[2:] - a[:2]
    denom = d @ d
    if denom < 1e-9:
        return np.linalg.norm(p - a[:2])
    t = np.clip((p - a[:2]) @ d / denom, 0, 1)
    return np.linalg.norm(a[:2] + t * d - p)


def drop_small_components(segs, vias_arr, min_total_len=14.0):
    n_s = len(segs)
    if n_s == 0:
        return segs
    arr = np.array(segs)
    lens = np.linalg.norm(arr[:, 2:] - arr[:, :2], axis=1)
    lo, hi = np.minimum(arr[:, :2], arr[:, 2:]) - 2.2, np.maximum(arr[:, :2], arr[:, 2:]) + 2.2
    parent = list(range(n_s))

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    for i in range(n_s):
        near = np.where((lo[i + 1:, 0] <= hi[i, 0]) & (hi[i + 1:, 0] >= lo[i, 0]) &
                        (lo[i + 1:, 1] <= hi[i, 1]) & (hi[i + 1:, 1] >= lo[i, 1]))[0] + (i + 1)
        for j in near:
            if (_dist_pt_seg(arr[i, :2], arr[j]) < 2.0 or _dist_pt_seg(arr[i, 2:], arr[j]) < 2.0 or
                    _dist_pt_seg(arr[j, :2], arr[i]) < 2.0 or _dist_pt_seg(arr[j, 2:], arr[i]) < 2.0):
                rx, ry = find(i), find(j)
                if rx != ry:
                    parent[rx] = ry

    comps = {}
    for i in range(n_s):
        comps.setdefault(find(i), []).append(i)
    keep = []
    for idxs in comps.values():
        tot = sum(lens[i] for i in idxs)
        if tot >= min_total_len:
            keep.extend(idxs)
            continue
        if len(vias_arr) and tot >= 4.0:
            if any(min(np.min(np.linalg.norm(vias_arr - arr[i, :2], axis=1)),
                       np.min(np.linalg.norm(vias_arr - arr[i, 2:], axis=1))) < 3.5 for i in idxs):
                keep.extend(idxs)
    return [segs[i] for i in sorted(keep)]


def snap_to_vias(segs, vias_arr, reach=2.8):
    if not len(vias_arr):
        return segs
    out = []
    for s in segs:
        s = s.copy()
        for e in (0, 2):
            dists = np.linalg.norm(vias_arr - s[e:e + 2], axis=1)
            idx = int(np.argmin(dists))
            if dists[idx] < reach:
                s[e:e + 2] = vias_arr[idx]
        out.append(s)
    return out


def main(photo, closeups=(), debug=None):
    img0 = cv2.imread(photo)
    H, W = img0.shape[:2]
    K = W / 1000.0
    excl = build_exclusion_mask(H, W, K)

    L0 = cv2.cvtColor(img0, cv2.COLOR_BGR2LAB)[..., 0].astype(np.float32)
    hsv0 = cv2.cvtColor(img0, cv2.COLOR_BGR2HSV)
    B0, R0 = img0[..., 0].astype(np.float32), img0[..., 2].astype(np.float32)
    glare0 = (hsv0[..., 1] < 85) & (L0 > 110)

    # 1. Vias
    tmpl_rad, r_t = _build_radial_via_template(L0, K)
    ncc0, rmin0, rmed0, blue0 = _via_maps(img0, tmpl_rad, r_t, K)
    ncc_other_max = np.zeros_like(ncc0)
    rmin_other_max = np.full_like(ncc0, -999.0)
    cov_other = np.zeros((H, W), bool)

    other_confirm = [np.zeros((H, W), np.float32) for _ in range(4)]
    for cp in closeups:
        im_c = cv2.imread(cp)
        cov = cv2.erode((im_c.sum(axis=2) > 0).astype(np.uint8), np.ones((41, 41), np.uint8)) > 0
        ncc_c, rmin_c, _, _ = _via_maps(im_c, tmpl_rad, r_t, K)
        ncc_other_max = np.where(cov, np.maximum(ncc_other_max, maximum_filter(ncc_c, size=5)), ncc_other_max)
        rmin_other_max = np.where(cov, np.maximum(rmin_other_max, maximum_filter(rmin_c, size=5)), rmin_other_max)
        rc = _dual_groove_response(im_c, K)
        for d in range(4):
            other_confirm[d] = np.where(cov, np.maximum(other_confirm[d], maximum_filter(rc[d], size=5)),
                                        other_confirm[d])
        cov_other |= cov

    ncc_glare = np.where(ncc_other_max > 0.42, 0.50 * ncc0 + 0.50 * ncc_other_max, 0.0)
    ncc_norm = np.where(cov_other, np.maximum(0.55 * ncc0 + 0.45 * ncc_other_max, 0.85 * ncc0), ncc0)
    ncc_fused = np.where(glare0 & cov_other, ncc_glare, ncc_norm)
    ring_ok = (rmin0 > 3.5) | (cov_other & (rmin_other_max > 5.0) & (rmin0 > 1.0))
    valid_via = (excl == 0) & (blue0 > 0.48) & ring_ok & (rmed0 > 10.0)
    nms_sz = int(round(2.17 * K)) | 1
    s_via = np.where(valid_via, ncc_fused, -1.0)
    vy, vx = np.where((s_via == maximum_filter(s_via, size=nms_sz)) & (s_via > 0.48))
    vias_list = [[round(float(x / K), 2), round(float(y / K), 2)] for y, x in zip(vy, vx)]
    vias_arr = np.array(vias_list, float) if vias_list else np.zeros((0, 2))
    json.dump(vias_list, open(OUT_VIAS, 'w'), separators=(',', ':'))
    print(f'{len(vias_list)} vias -> {OUT_VIAS}')

    # 2. Traces
    resps0 = _dual_groove_response(img0, K)
    blue = ((B0 - R0 > 8) & (hsv0[..., 0] >= 88) & (hsv0[..., 0] <= 126) &
            (hsv0[..., 2] > 35) & (hsv0[..., 2] < 248) & (L0 < 205)).astype(np.uint8)
    blue = cv2.erode(blue, np.ones((int(round(1.17 * K)) | 1,) * 2, np.uint8))
    mask_tr = (blue > 0) & (excl == 0)

    best = np.zeros((H, W), np.float32)
    best_dir = np.zeros((H, W), np.uint8)
    for d_idx, ang in enumerate(DIRS):
        r0 = resps0[d_idx]
        r_fused = np.where(glare0 & cov_other, np.sqrt(r0 * other_confirm[d_idx]), r0)
        nx, ny = -np.sin(np.radians(ang)), np.cos(np.radians(ang))
        nms = r_fused.copy()
        for step in (1.0, 2.0, 3.0, 4.0):
            ddx, ddy = int(round(nx * step)), int(round(ny * step))
            nb1 = np.roll(np.roll(r_fused, -ddy, axis=0), -ddx, axis=1)
            nb2 = np.roll(np.roll(r_fused, ddy, axis=0), ddx, axis=1)
            nms = np.where((r_fused >= nb1) & (r_fused >= nb2), nms, 0.0)
        nms *= mask_tr
        better = nms > best
        best = np.where(better, nms, best)
        best_dir = np.where(better, d_idx, best_dir)

    strong = (best > 14.5).astype(np.uint8)
    weak = (best > 8.0).astype(np.uint8)
    n_cc, lab_cc = cv2.connectedComponents(weak, connectivity=8)
    keep_cc = np.zeros(n_cc, bool)
    keep_cc[np.unique(lab_cc[strong > 0])] = True
    keep_cc[0] = False
    ridge_bin = keep_cc[lab_cc]

    segs = []
    for d_idx, ang in enumerate(DIRS):
        sel = (ridge_bin & (best_dir == d_idx)).astype(np.uint8)
        c, s = np.cos(np.radians(ang)), np.sin(np.radians(ang))
        kern = np.zeros((5, 5), np.uint8)
        for step in (-2, -1, 0, 1, 2):
            kern[2 + int(round(step * s)), 2 + int(round(step * c))] = 1
        sel_closed = cv2.morphologyEx(sel, cv2.MORPH_CLOSE, kern)
        n_c, lbl, stats, _ = cv2.connectedComponentsWithStats(sel_closed, connectivity=8)
        u, n_vec = np.array([c, s], np.float64), np.array([-s, c], np.float64)
        for cid in range(1, n_c):
            if stats[cid, cv2.CC_STAT_AREA] < 12:
                continue
            x0_b, y0_b, w_b, h_b = stats[cid, :4]
            ys_c, xs_c = np.where(lbl[y0_b:y0_b + h_b, x0_b:x0_b + w_b] == cid)
            pts = np.column_stack([xs_c + x0_b, ys_c + y0_b]).astype(np.float64) / K
            t_vals, v_vals = pts @ u, pts @ n_vec
            if t_vals.max() - t_vals.min() < 3.5 or np.std(v_vals) > 1.0:
                continue
            v_med = np.median(v_vals)
            segs.append(np.concatenate([u * t_vals.min() + n_vec * v_med, u * t_vals.max() + n_vec * v_med]))

    segs = merge_collinear(segs, tol=1.10, gap=4.2)
    segs = drop_crossing_smart(segs)
    segs = join_bends(segs, reach=3.2)
    segs = drop_small_components(segs, vias_arr, min_total_len=14.0)
    segs = snap_to_vias(segs, vias_arr, reach=2.8)
    out = [[round(float(c), 2) for c in s] for s in segs]
    json.dump(out, open(OUT_TRACES, 'w'), separators=(',', ':'))
    print(f'{len(out)} segments -> {OUT_TRACES}')

    if debug:
        os.makedirs(debug, exist_ok=True)
        dbg = img0.copy()
        for x0, y0, x1, y1 in out:
            cv2.line(dbg, (int(round(x0 * K)), int(round(y0 * K))), (int(round(x1 * K)), int(round(y1 * K))),
                     (0, 255, 80), 2)
        for x, y in vias_list:
            cv2.circle(dbg, (int(round(x * K)), int(round(y * K))), int(round(1.7 * K)), (0, 255, 255), 2)
        cv2.imwrite(os.path.join(debug, 'traces_vias.jpg'), dbg)


if __name__ == '__main__':
    args = [a for a in sys.argv[1:] if not a.startswith('--')]
    dbg = sys.argv[sys.argv.index('--debug') + 1] if '--debug' in sys.argv else None
    if dbg in args:
        args.remove(dbg)
    if not args:
        raise SystemExit(__doc__)
    main(args[0], args[1:], dbg)
