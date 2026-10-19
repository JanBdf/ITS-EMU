#!/usr/bin/env python3
"""Puts a region of the board drawing next to the same region of the
rectified photo of the real board, for reviewing the drawing.

    boardcmp.py X0 Y0 X1 Y1 OUT.png [--photo PHOTO] [--assets DIR] [--width PX]
                [--see-through [ALPHA]] [--layers carrier,nucleo,shield]

Coordinates are GUI design units (the board is 1000 units wide).  The
drawing is composed from its layers (board-carrier.svg, board-nucleo.svg,
board-shield.svg in DIR, default apps/its-emu-gui/assets) over a dark
background like the GUI's; with --see-through the Nucleo and the display are
drawn with opacity ALPHA (default 0.25) as in the GUI's see-through view.
PHOTO is the rectified, focus-stacked photo of the real board (default
hardware_img/board-rect.jpg; its width is 1000 units).  The carrier board
lines up exactly; the Nucleo and the display stand higher and are seen
slightly enlarged about the camera position (690, 330).  With --photo none
only the drawing is written.
"""
import argparse
import os
import subprocess
import tempfile

import cv2
import numpy as np

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..')
W = 1000.0
LAYERS = ['carrier', 'nucleo', 'shield']
BACKGROUND = (27, 27, 27)          # the GUI's panel (BGR)


def render(assets, layers, alpha, k, box):
    """The layers composed over the background, `k` pixels per unit, cropped to `box`."""
    x0, y0, x1, y1 = (int(round(v * k)) for v in box)
    out = np.empty((y1 - y0, x1 - x0, 3), np.float32)
    out[:] = BACKGROUND
    with tempfile.TemporaryDirectory() as tmp:
        for name in layers:
            png = os.path.join(tmp, name + '.png')
            subprocess.run(['resvg', '-w', str(int(round(W * k))), os.path.join(assets, f'board-{name}.svg'), png],
                           check=True)
            im = cv2.imread(png, cv2.IMREAD_UNCHANGED)[y0:y1, x0:x1].astype(np.float32)
            a = im[..., 3:] / 255.0 * (alpha if name != 'carrier' else 1.0)
            out = out * (1 - a) + im[..., :3] * a
    return out.round().astype(np.uint8)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('box', type=float, nargs=4)
    ap.add_argument('out')
    ap.add_argument('--photo', default=os.path.join(ROOT, 'hardware_img', 'board-rect.jpg'))
    ap.add_argument('--assets', default=os.path.join(ROOT, 'apps', 'its-emu-gui', 'assets'))
    ap.add_argument('--width', type=int, default=900, help='width of each half in pixels')
    ap.add_argument('--see-through', type=float, nargs='?', const=0.25, default=1.0, dest='alpha')
    ap.add_argument('--layers', default=','.join(LAYERS))
    a = ap.parse_args()

    x0, y0, x1, y1 = a.box
    k = a.width / (x1 - x0)
    drawing = render(a.assets, a.layers.split(','), a.alpha, k, a.box)
    if a.photo == 'none':
        cv2.imwrite(a.out, drawing)
        return
    photo = cv2.imread(a.photo)
    s = photo.shape[1] / W
    px = lambda v: int(round(v * s))
    crop_p = photo[px(y0):px(y1), px(x0):px(x1)]
    crop_p = cv2.resize(crop_p, (drawing.shape[1], drawing.shape[0]), interpolation=cv2.INTER_AREA)
    gap = np.full((drawing.shape[0], 8, 3), 255, np.uint8)
    cv2.imwrite(a.out, np.hstack([drawing, gap, crop_p]))


if __name__ == '__main__':
    main()
