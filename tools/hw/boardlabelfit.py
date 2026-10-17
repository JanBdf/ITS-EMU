#!/usr/bin/env python3
"""Fits the silkscreen labels of boarddraw.py to the photo of the board.

    boardlabelfit.py BOARD.jpg [BOARD2.jpg ...] [--report]

BOARD.jpg is the rectified photo (hardware_img/board-rect.jpg, 1000 design units wide).
For every label, position, cap height and width are varied around the
hand-measured values; the variant whose rendering matches the white
silkscreen in the photo best (normalised cross-correlation) is written to
boardlabels.json, which boarddraw.py applies.  Labels that do not match
well (covered by the Nucleo or the display, partly hidden) keep their values.
With several photos (taken from different points, so that the Nucleo and the
display hide different parts) the best match of all photos is used.
"""
import json
import sys

import cv2
import numpy as np

import boarddraw as bd

MARGIN = 4.0          # search range, design units
MIN_SCORE = 0.45


def silk_mask(photo):
    img = cv2.imread(photo)
    k = img.shape[1] / 1000.0
    hsv = cv2.cvtColor(img, cv2.COLOR_BGR2HSV).astype(np.float32) / 255.0
    silk = np.clip((hsv[..., 2] - 0.6) * 4, 0, 1) * np.clip((0.35 - hsv[..., 1]) * 5, 0, 1)
    return cv2.GaussianBlur(silk, (0, 0), 0.6 * k), k


def fit_one(l, silk, k):
    """(score, dx, dy, height scale, advance) of the best match of label `l`, or None."""
    sw = l['sw'] if l['sw'] is not None else 0.2 * l['h']
    best = None
    for hs in np.arange(0.85, 1.201, 0.05):
        for adv in np.arange(0.85, 1.151, 0.05):
            lines = bd.text_lines(l['text'], l['x'], l['y'], l['h'] * hs, l['anchor'], l['rot'], adv)
            pts = np.concatenate([np.array(p) for p in lines])
            x0, y0 = pts.min(0) - sw
            x1, y1 = pts.max(0) + sw
            tw, th = int((x1 - x0) * k) + 2, int((y1 - y0) * k) + 2
            tpl = np.zeros((th, tw), np.float32)
            for p in lines:
                q = ((np.array(p) - (x0, y0)) * k * 16).astype(np.int32)
                cv2.polylines(tpl, [q], False, 1.0, max(1, int(sw * k)), cv2.LINE_AA, 4)
            sx0, sy0 = int((x0 - MARGIN) * k), int((y0 - MARGIN) * k)
            if sx0 < 0 or sy0 < 0:
                continue
            win = silk[sy0:sy0 + th + int(2 * MARGIN * k), sx0:sx0 + tw + int(2 * MARGIN * k)]
            if win.shape[0] < th or win.shape[1] < tw or win.std() < 1e-3:
                continue
            res = cv2.matchTemplate(win, tpl, cv2.TM_CCOEFF_NORMED)
            _, score, _, loc = cv2.minMaxLoc(res)
            dx = (sx0 + loc[0]) / k - x0
            dy = (sy0 + loc[1]) / k - y0
            # slight preference for the measured size
            score -= 0.15 * abs(hs - 1) + 0.15 * abs(adv - l['adv'])
            if best is None or score > best[0]:
                best = (score, dx, dy, hs, adv)
    return best


def main(photos, report=False):
    masks = [silk_mask(p) for p in photos]
    fit = {}
    labels = bd.carrier_labels()
    scores = []
    for l in labels:
        fits = [b for b in (fit_one(l, silk, k) for silk, k in masks) if b is not None]
        if not fits:
            continue
        best = max(fits)
        scores.append((best[0], l['text']))
        # a fit at the edge of the search range is a match with something else
        if best[0] >= MIN_SCORE and max(abs(best[1]), abs(best[2])) < MARGIN - 0.6:
            fit[bd.label_key(l)] = [round(best[1], 2), round(best[2], 2), round(best[3], 3), round(best[4], 3)]
        if report:
            print(f"{best[0]:.2f} {l['text']:12s} d=({best[1]:+.1f},{best[2]:+.1f}) h*{best[3]:.2f} adv {best[4]:.2f}")
    json.dump(fit, open(bd.LABEL_FIT, 'w'), indent=0, sort_keys=True)
    print(f'{len(fit)} of {len(labels)} labels fitted, mean score {np.mean([s for s, _ in scores]):.2f}')


if __name__ == '__main__':
    main([a for a in sys.argv[1:] if not a.startswith('--')], '--report' in sys.argv)
