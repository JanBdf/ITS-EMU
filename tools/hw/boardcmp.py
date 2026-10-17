#!/usr/bin/env python3
"""Puts a region of the board drawing next to the same region of the
rectified photo of the real board, for reviewing the drawing.

    boardcmp.py X0 Y0 X1 Y1 OUT.png [--photo PHOTO] [--svg SVG] [--width PX]

Coordinates are GUI design units (the board is 1000 units wide).  PHOTO is
the rectified, focus-stacked photo of the real board (default
hardware_img/board-rect.jpg; its width is 1000 units).  The carrier board
lines up exactly; the Nucleo and the display stand higher and are seen
slightly enlarged about the camera position (690, 330).
"""
import argparse
import os
import subprocess
import tempfile

import cv2
import numpy as np

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..')
W = 1000.0

ap = argparse.ArgumentParser()
ap.add_argument('box', type=float, nargs=4)
ap.add_argument('out')
ap.add_argument('--photo', default=os.path.join(ROOT, 'hardware_img', 'board-rect.jpg'))
ap.add_argument('--svg', default=os.path.join(ROOT, 'apps', 'its-emu-gui', 'assets', 'board.svg'))
ap.add_argument('--width', type=int, default=900, help='width of each half in pixels')
a = ap.parse_args()

photo = cv2.imread(a.photo)
s = photo.shape[1] / W

x0, y0, x1, y1 = a.box
px = lambda v: int(round(v * s))
crop_p = photo[px(y0):px(y1), px(x0):px(x1)]

zoom = a.width / crop_p.shape[1]
with tempfile.TemporaryDirectory() as tmp:
    out = os.path.join(tmp, 'r.png')
    # render only the region, at the output resolution
    k = zoom * s
    subprocess.run(['resvg', '-w', str(int(round(W * k))), a.svg, out], check=True)
    full = cv2.imread(out)
crop_r = full[int(round(y0 * k)):int(round(y1 * k)), int(round(x0 * k)):int(round(x1 * k))]
crop_p = cv2.resize(crop_p, (crop_r.shape[1], crop_r.shape[0]), interpolation=cv2.INTER_AREA)
gap = np.full((crop_r.shape[0], 8, 3), 255, np.uint8)
cv2.imwrite(a.out, np.hstack([crop_r, gap, crop_p]))
