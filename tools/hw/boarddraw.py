#!/usr/bin/env python3
"""Draws the ITS-Board (Rev 02 with NUCLEO-F429ZI and the Waveshare 4" shield)
as a vector image for the GUI.

    boarddraw.py [OUT_DIR]        (default: apps/its-emu-gui/assets)

The board is drawn in three layers, one SVG each with the same view box, so
that the GUI can show the parts stacked on the carrier see-through:
board-carrier.svg (the ITS-Board with everything soldered to it, including
what the Nucleo and the display hide), board-nucleo.svg (the NUCLEO-F429ZI
with its Morpho pins) and board-shield.svg (the display module).

Everything is drawn from shapes and fonts; there are no bitmaps in the
output.  Positions are GUI design units (the board is 1000 units wide,
1 unit = 0.2408 mm) and were measured in the rectified, focus-stacked photo
of the real board (hardware_img/board-rect.jpg) and the other photos there.

Text is converted to paths here, so the SVG does not need any fonts:
- the silkscreen of the carrier board uses a stroke font modelled on
  Altium's default font (octagonal glyphs, see GLYPHS),
- the logo block uses Open Sans and Noto Serif, the Nucleo board
  Liberation Sans (all SIL Open Font License; needed only to regenerate).
"""
import math
import os
import re
import sys

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

BOARD_RS = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', 'apps', 'its-emu-gui', 'src',
                        'board.rs')


def rs_const(name):
    """Numbers of a constant in board.rs (the GUI draws its overlays there)."""
    m = re.search(rf'const {name}: [^=]*=\s*([^;]*);', open(BOARD_RS).read())
    v = [float(x) for x in re.findall(r'-?\d+(?:\.\d+)?', m.group(1))]
    return v[0] if len(v) == 1 else v


# The drawing: the board outline, W × H units from (X0, Y0).  The design units
# come from board-rect.jpg, which shows the board cropped by about 5 units on
# each side, so the outline reaches into negative coordinates.
W = rs_const('BOARD_W')
H = rs_const('BOARD_H')
X0, Y0 = rs_const('BOARD_ORIGIN')
X1, Y1 = X0 + W, Y0 + H
CORNER = 9.0                        # all corners of the board, also those of the notch

# positions of the parts the GUI draws on (lights, presses, plugs, the display)
OUT_X = rs_const('OUT_X')           # OUT0..OUT15
IN_X = rs_const('IN_X')             # IN0..IN7
LED_Y = rs_const('LED_Y')
BUTTON_X = rs_const('BUTTON_X')     # S0..S7
BUTTON_Y = rs_const('BUTTON_Y')
AUX_X = rs_const('AUX_X')
AUX_Y = rs_const('AUX_Y')
USER = tuple(rs_const('USER'))
RESET = tuple(rs_const('RESET'))
_g = rs_const('GLASS')
GLASS = list(zip(_g[0::2], _g[1::2]))

# ---- orthographic view
# The Nucleo (14 mm) and the display (~30 mm) stand above the carrier.  The
# rectified photo (board-rect.jpg) shows them scaled about the point below the
# camera, by the magnifications below: from the Morpho pitch (11.01 units in
# the photo for 2.54 mm) and from the active area of the display (83.52 ×
# 55.68 mm).  Coordinates measured in that photo are drawn inside layer(),
# which undoes the scaling.
NADIR = (690.0, 330.0)
M_NUCLEO = 1.0438
M_SHIELD = 1.10
CAM_H = 14.0 * M_NUCLEO / (M_NUCLEO - 1)      # height of the camera above the carrier, mm


def mag(h):
    """Magnification in the photo of a plane h mm above the carrier."""
    return CAM_H / (CAM_H - h)


def layer(m):
    """SVG transform from photo coordinates of a layer with magnification m to the top view."""
    k = 1 / m
    return f'matrix({k:.6f} 0 0 {k:.6f} {NADIR[0] * (1 - k):.4f} {NADIR[1] * (1 - k):.4f})'


def ortho(x, y, m):
    """Top view position of the photo point (x, y) of a layer with magnification m."""
    return NADIR[0] + (x - NADIR[0]) / m, NADIR[1] + (y - NADIR[1]) / m


# ---- colours
MASK = '#1f4271'          # solder mask over bare laminate
MASK_CU = '#26497a'       # solder mask over copper (traces, planes)
SILK = '#dde4ea'
GOLD = '#d6a54e'
PLATING = '#a8894f'       # plated wall/rim of a hole
HOLE = '#16193a'
TIN = '#cdcac2'
LED_BODY = '#f3ecd9'
HEADER = '#1c1c1e'
NUCLEO = '#e3e3dd'
ST_BLUE = '#1f6fd0'
FELT = '#66665f'          # the grey sockets the Morpho pins are bent into
CAP_RED = '#d9434b'       # caps of S0..S7

FONT_DIRS = ['/usr/share/fonts/OpenSans', '/usr/share/fonts/noto', '/usr/share/fonts/liberation',
             '/usr/share/fonts/TTF']


def f(v):
    """Compact number formatting."""
    s = f'{v:.2f}'.rstrip('0').rstrip('.')
    return '0' if s == '-0' else s


class Svg:
    def __init__(self):
        self.defs = []
        self.body = []

    def add(self, s):
        self.body.append(s)

    def __iadd__(self, s):
        self.body.append(s)
        return self

    def text(self):
        return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{f(X0)} {f(Y0)} {f(W)} {f(H)}" '
                f'width="{f(W)}" height="{f(H)}">\n<defs>\n' + '\n'.join(self.defs) + '\n</defs>\n'
                + '\n'.join(self.body) + '\n</svg>\n')


svg = Svg()


def rect(x0, y0, x1, y1, fill, rx=0, extra=''):
    r = f' rx="{f(rx)}"' if rx else ''
    return f'<rect x="{f(x0)}" y="{f(y0)}" width="{f(x1 - x0)}" height="{f(y1 - y0)}"{r} fill="{fill}"{extra}/>'


def rect_stroke(x0, y0, x1, y1, color, w, rx=0, extra=''):
    r = f' rx="{f(rx)}"' if rx else ''
    return (f'<rect x="{f(x0)}" y="{f(y0)}" width="{f(x1 - x0)}" height="{f(y1 - y0)}"{r} fill="none" '
            f'stroke="{color}" stroke-width="{f(w)}"{extra}/>')


def circle(x, y, r, fill, extra=''):
    return f'<circle cx="{f(x)}" cy="{f(y)}" r="{f(r)}" fill="{fill}"{extra}/>'


def ring(x, y, r, color, w, extra=''):
    return f'<circle cx="{f(x)}" cy="{f(y)}" r="{f(r)}" fill="none" stroke="{color}" stroke-width="{f(w)}"{extra}/>'


def poly(pts, fill, extra=''):
    return f'<polygon points="{" ".join(f"{f(x)},{f(y)}" for x, y in pts)}" fill="{fill}"{extra}/>'


def polyline(pts, color, w, extra='', cap='round', join='round'):
    return (f'<polyline points="{" ".join(f"{f(x)},{f(y)}" for x, y in pts)}" fill="none" stroke="{color}" '
            f'stroke-width="{f(w)}" stroke-linecap="{cap}" stroke-linejoin="{join}"{extra}/>')


def line(x0, y0, x1, y1, color, w, cap='round', extra=''):
    return (f'<line x1="{f(x0)}" y1="{f(y0)}" x2="{f(x1)}" y2="{f(y1)}" stroke="{color}" stroke-width="{f(w)}" '
            f'stroke-linecap="{cap}"{extra}/>')


def path(d, fill='none', extra=''):
    return f'<path d="{d}" fill="{fill}"{extra}/>'


def octagon(x0, y0, x1, y1, c):
    """Rectangle with 45° chamfered corners."""
    return [(x0 + c, y0), (x1 - c, y0), (x1, y0 + c), (x1, y1 - c), (x1 - c, y1), (x0 + c, y1), (x0, y1 - c),
            (x0, y0 + c)]


# ======================================================================
# Stroke font after Altium's default font.  Glyph box: x 0..0.7, y 0..1
# (cap height 1, baseline y = 1, y down); lower case x-height at y = 0.38.
# Each glyph is a list of polylines.

C = 0.2          # chamfer
X = 0.38         # top of lower case letters


def _o(x0, y0, x1, y1, c=C):
    p = octagon(x0, y0, x1, y1, c)
    return p + [p[0]]


GLYPHS = {
    ' ': [],
    'A': [[(0, 1), (0, 0.38), (0.35, 0), (0.7, 0.38), (0.7, 1)], [(0, 0.68), (0.7, 0.68)]],
    'B': [[(0, 0), (0.55, 0), (0.7, 0.15), (0.7, 0.35), (0.55, 0.5), (0, 0.5)],
          [(0.55, 0.5), (0.7, 0.65), (0.7, 0.85), (0.55, 1), (0, 1), (0, 0)]],
    'C': [[(0.7, C), (0.7 - C, 0), (C, 0), (0, C), (0, 1 - C), (C, 1), (0.7 - C, 1), (0.7, 1 - C)]],
    'D': [[(0, 0), (0.5, 0), (0.7, 0.2), (0.7, 0.8), (0.5, 1), (0, 1), (0, 0)]],
    'E': [[(0.7, 0), (0, 0), (0, 1), (0.7, 1)], [(0, 0.5), (0.5, 0.5)]],
    'F': [[(0.7, 0), (0, 0), (0, 1)], [(0, 0.5), (0.5, 0.5)]],
    'G': [[(0.7, C), (0.7 - C, 0), (C, 0), (0, C), (0, 1 - C), (C, 1), (0.7 - C, 1), (0.7, 1 - C), (0.7, 0.55),
           (0.4, 0.55)]],
    'H': [[(0, 0), (0, 1)], [(0.7, 0), (0.7, 1)], [(0, 0.5), (0.7, 0.5)]],
    'I': [[(0.13, 0), (0.57, 0)], [(0.35, 0), (0.35, 1)], [(0.13, 1), (0.57, 1)]],
    'J': [[(0.7, 0), (0.7, 1 - C), (0.7 - C, 1), (C, 1), (0, 1 - C)]],
    'K': [[(0, 0), (0, 1)], [(0.7, 0), (0, 0.6)], [(0.25, 0.4), (0.7, 1)]],
    'L': [[(0, 0), (0, 1), (0.7, 1)]],
    'M': [[(0, 1), (0, 0), (0.35, 0.5), (0.7, 0), (0.7, 1)]],
    'N': [[(0, 1), (0, 0), (0.7, 1), (0.7, 0)]],
    'O': [_o(0, 0, 0.7, 1)],
    'P': [[(0, 1), (0, 0), (0.55, 0), (0.7, 0.15), (0.7, 0.4), (0.55, 0.55), (0, 0.55)]],
    'Q': [_o(0, 0, 0.7, 1), [(0.42, 0.72), (0.7, 1)]],
    'R': [[(0, 1), (0, 0), (0.55, 0), (0.7, 0.15), (0.7, 0.4), (0.55, 0.55), (0, 0.55)], [(0.4, 0.55), (0.7, 1)]],
    'S': [[(0.7, 0.15), (0.55, 0), (0.15, 0), (0, 0.15), (0, 0.35), (0.15, 0.5), (0.55, 0.5), (0.7, 0.65),
           (0.7, 0.85), (0.55, 1), (0.15, 1), (0, 0.85)]],
    'T': [[(0, 0), (0.7, 0)], [(0.35, 0), (0.35, 1)]],
    'U': [[(0, 0), (0, 1 - C), (C, 1), (0.7 - C, 1), (0.7, 1 - C), (0.7, 0)]],
    'V': [[(0, 0), (0, 0.6), (0.35, 1), (0.7, 0.6), (0.7, 0)]],
    'W': [[(0, 0), (0, 1), (0.35, 0.5), (0.7, 1), (0.7, 0)]],
    'X': [[(0, 0), (0.7, 1)], [(0.7, 0), (0, 1)]],
    'Y': [[(0, 0), (0.35, 0.5), (0.7, 0)], [(0.35, 0.5), (0.35, 1)]],
    'Z': [[(0, 0), (0.7, 0), (0, 1), (0.7, 1)]],
    '0': [_o(0, 0, 0.7, 1)],
    '1': [[(0.1, 0.22), (0.32, 0), (0.32, 1)], [(0.1, 1), (0.54, 1)]],
    '2': [[(0, 0.2), (0.2, 0), (0.5, 0), (0.7, 0.2), (0.7, 0.35), (0, 1), (0.7, 1)]],
    '3': [[(0, 0.15), (0.15, 0), (0.55, 0), (0.7, 0.15), (0.7, 0.35), (0.55, 0.5), (0.2, 0.5)],
          [(0.55, 0.5), (0.7, 0.65), (0.7, 0.85), (0.55, 1), (0.15, 1), (0, 0.85)]],
    '4': [[(0.5, 1), (0.5, 0), (0, 0.65), (0.7, 0.65)]],
    '5': [[(0.7, 0), (0, 0), (0, 0.45), (0.55, 0.45), (0.7, 0.6), (0.7, 0.85), (0.55, 1), (0.15, 1), (0, 0.85)]],
    '6': [[(0.6, 0), (0.2, 0), (0, 0.2), (0, 0.85), (0.15, 1), (0.55, 1), (0.7, 0.85), (0.7, 0.6), (0.55, 0.45),
           (0, 0.45)]],
    '7': [[(0, 0), (0.7, 0), (0.7, 0.12), (0.25, 1)]],
    '8': [[(0.15, 0.5), (0, 0.35), (0, 0.15), (0.15, 0), (0.55, 0), (0.7, 0.15), (0.7, 0.35), (0.55, 0.5),
           (0.15, 0.5), (0, 0.65), (0, 0.85), (0.15, 1), (0.55, 1), (0.7, 0.85), (0.7, 0.65), (0.55, 0.5)]],
    '9': [[(0.7, 0.55), (0.15, 0.55), (0, 0.4), (0, 0.15), (0.15, 0), (0.55, 0), (0.7, 0.15), (0.7, 0.8),
           (0.5, 1), (0.1, 1)]],
    'a': [[(0.05, X), (0.52, X), (0.7, X + 0.17), (0.7, 1), (0.15, 1), (0, 0.88), (0, 0.8), (0.12, 0.69),
           (0.7, 0.69)]],
    'b': [[(0, 0), (0, 1), (0.55, 1), (0.7, 0.85), (0.7, X + 0.15), (0.55, X), (0, X)]],
    'c': [[(0.7, X), (0.15, X), (0, X + 0.15), (0, 0.85), (0.15, 1), (0.7, 1)]],
    'd': [[(0.7, 0), (0.7, 1), (0.15, 1), (0, 0.85), (0, X + 0.15), (0.15, X), (0.7, X)]],
    'e': [[(0, 0.7), (0.7, 0.7), (0.7, X + 0.15), (0.55, X), (0.15, X), (0, X + 0.15), (0, 0.85), (0.15, 1),
           (0.65, 1)]],
    'f': [[(0.6, 0), (0.35, 0), (0.2, 0.15), (0.2, 1)], [(0, X), (0.55, X)]],
    'g': [[(0.7, 1), (0.15, 1), (0, 0.85), (0, X + 0.15), (0.15, X), (0.7, X), (0.7, 1.15), (0.55, 1.3),
           (0.1, 1.3)]],
    'h': [[(0, 0), (0, 1)], [(0, X), (0.55, X), (0.7, X + 0.15), (0.7, 1)]],
    'i': [[(0.35, X), (0.35, 1)], [(0.35, 0.12), (0.35, 0.13)]],
    'k': [[(0, 0), (0, 1)], [(0.65, X), (0, 0.75)], [(0.25, 0.62), (0.7, 1)]],
    'l': [[(0.12, 0), (0.35, 0), (0.35, 1)], [(0.1, 1), (0.6, 1)]],
    'm': [[(0, 1), (0, X)], [(0, X + 0.07), (0.07, X), (0.28, X), (0.35, X + 0.07), (0.35, 1)],
          [(0.35, X + 0.07), (0.42, X), (0.63, X), (0.7, X + 0.07), (0.7, 1)]],
    'n': [[(0, X), (0, 1)], [(0, X), (0.55, X), (0.7, X + 0.15), (0.7, 1)]],
    'o': [_o(0, X, 0.7, 1, 0.16)],
    'p': [[(0, 1.3), (0, X), (0.55, X), (0.7, X + 0.15), (0.7, 0.85), (0.55, 1), (0, 1)]],
    'r': [[(0, X), (0, 1)], [(0, X + 0.15), (0.15, X), (0.65, X)]],
    's': [[(0.7, X), (0.1, X), (0, X + 0.1), (0, 0.6), (0.1, 0.69), (0.6, 0.69), (0.7, 0.79), (0.7, 0.9),
           (0.6, 1), (0, 1)]],
    't': [[(0.25, 0.1), (0.25, 0.85), (0.4, 1), (0.62, 1)], [(0, 0.3), (0.6, 0.3)]],
    'u': [[(0, X), (0, 0.85), (0.15, 1), (0.7, 1)], [(0.7, X), (0.7, 1)]],
    'v': [[(0, X), (0.35, 1), (0.7, X)]],
    'x': [[(0, X), (0.7, 1)], [(0.7, X), (0, 1)]],
    'y': [[(0, X), (0, 0.85), (0.15, 1), (0.7, 1)], [(0.7, X), (0.7, 1.15), (0.55, 1.3), (0.1, 1.3)]],
    '-': [[(0.1, 0.55), (0.6, 0.55)]],
    '_': [[(0, 1.05), (0.7, 1.05)]],
    '.': [[(0.3, 1), (0.31, 1)]],
    ',': [[(0.33, 0.92), (0.25, 1.1)]],
    '/': [[(0, 1), (0.7, 0)]],
    '+': [[(0.05, 0.5), (0.65, 0.5)], [(0.35, 0.2), (0.35, 0.8)]],
    ':': [[(0.35, 0.45), (0.35, 0.46)], [(0.35, 0.99), (0.35, 1)]],
    '~': [[(0, 0.55), (0.15, 0.45), (0.55, 0.6), (0.7, 0.5)]],
}

ADVANCE = 1.0     # glyph advance in cap heights
NARROW = {'I': 0.8, '1': 0.85, 'i': 0.7, 'l': 0.85, '.': 0.6, ' ': 0.9, ':': 0.6}


def text_lines(s, x, y, h, anchor='c', rot=0, adv=1.0):
    """Polylines (board coordinates) of `s` in the stroke font.  (x, y) is the
    anchor point at half cap height: left end ('l'), middle ('c') or right end
    ('r'); `rot` in degrees (clockwise), `h` the cap height between stroke
    centres, `adv` stretches the text horizontally."""
    cells = [NARROW.get(c, ADVANCE) for c in s]
    total = (sum(cells) - (cells[-1] - 0.7)) * h * adv if s else 0
    cx = {'l': 0.0, 'c': -total / 2, 'r': -total}[anchor]
    co, si = math.cos(math.radians(rot)), math.sin(math.radians(rot))
    out = []
    for c, cell in zip(s, cells):
        shift = (cell - ADVANCE) / 2 if cell < ADVANCE and c not in ' .:' else 0.0
        for pl in GLYPHS[c]:
            pts = []
            for px, py in pl:
                u, v = cx + (px + shift) * h * adv, (py - 0.5) * h
                pts.append((x + u * co - v * si, y + u * si + v * co))
            out.append(pts)
        cx += cell * h * adv
    return out


def stext(x, y, s, h, anchor='c', rot=0, sw=None, color=SILK, adv=1.0, extra=''):
    """Silkscreen text in the stroke font (see text_lines); `sw` = stroke width."""
    sw = sw if sw is not None else 0.2 * h
    d = ''.join('M' + ' '.join(f'{f(px)},{f(py)}' for px, py in pl) for pl in text_lines(s, x, y, h, anchor, rot, adv))
    return (f'<path d="{d}" fill="none" stroke="{color}" stroke-width="{f(sw)}" '
            f'stroke-linecap="round" stroke-linejoin="round"{extra}/>')


# ======================================================================
# Outline fonts (converted to paths)

_fonts = {}


def font(name):
    if name not in _fonts:
        for d in FONT_DIRS:
            p = os.path.join(d, name)
            if os.path.exists(p):
                _fonts[name] = TTFont(p)
                break
        else:
            raise SystemExit(f'font {name} not found in {FONT_DIRS}')
    return _fonts[name]


def ftext(x, y, s, fontname, cap, anchor='l', fill=SILK, rot=0, track=0.0, extra='', width=None, mid=False):
    """Text in an outline font; (x, y) on the baseline (at half cap height if
    `mid`), `cap` = cap height.  `width` stretches the text horizontally to
    that total width."""
    t = font(fontname)
    gs = t.getGlyphSet()
    cmap = t.getBestCmap()
    hmtx = t['hmtx']
    k = cap / t['OS/2'].sCapHeight
    kern = {}
    names = [cmap.get(ord(c), '.notdef') for c in s]
    adv = [hmtx[n][0] * k + track for n in names]
    total = sum(adv) - track
    sx = (width / total) if width else 1.0
    x0 = {'l': 0.0, 'c': -total * sx / 2, 'r': -total * sx}[anchor]
    pen = SVGPathPen(gs, ntos=f)  # full float precision would double the file size
    cx = 0.0
    for n, a in zip(names, adv):
        tp = TransformPen(pen, (k * sx, 0, 0, -k, x0 + cx * sx, cap / 2 if mid else 0))
        gs[n].draw(tp)
        cx += a
    tr = f'translate({f(x)} {f(y)})' + (f' rotate({f(rot)})' if rot else '')
    d = pen.getCommands()
    return f'<path transform="{tr}" d="{d}" fill="{fill}"{extra}/>'


# ======================================================================
# Parts

# Holes through the carrier (x, y, r): cut out of the board, so that what is
# behind the board shows through them.
HOLES = []


def pad(x, y, r=6.9, hole=5.0, square=False, color=GOLD):
    """Plated through hole with annular ring (measured: ring from r 5.0 to 6.9)."""
    HOLES.append((x, y, hole))
    out = []
    if square:
        out.append(rect(x - r, y - r, x + r, y + r, color))
    else:
        out.append(circle(x, y, r, color))
    out.append(ring(x, y, hole + 0.25, PLATING, 0.5))
    return '\n'.join(out)


def small_pad(x, y, r=2.6, hole=1.5, square=False):
    HOLES.append((x, y, hole))
    return rect(x - r, y - r, x + r, y + r, GOLD) if square else circle(x, y, r, GOLD)


def mount_hole(x, y, r=6.2):
    """Mounting hole: cut out of the board (the background shows), thin plated rim."""
    HOLES.append((x, y, r))
    return ring(x, y, r + 0.4, PLATING, 0.8)


def led_smd(x, y):
    """PLCC-2 LED (3528) seen from the top, vertical; (x, y) = centre."""
    out = [
        rect(x - 5.6, y - 10.6, x + 5.6, y - 7.0, TIN, 0.8),           # top pad
        rect(x - 5.6, y + 7.0, x + 5.6, y + 10.6, TIN, 0.8),           # bottom pad
        rect(x - 7.3, y - 8.2, x + 7.0, y + 7.7, '#e8dfc8', 1.0),       # body
        rect(x - 6.4, y - 7.2, x + 6.1, y + 6.8, LED_BODY, 0.8),        # top face
        circle(x - 0.2, y - 0.2, 4.6, '#efe3c4'),                       # lens
        circle(x - 0.2, y - 0.2, 4.6, 'none', ' stroke="#d8c9a6" stroke-width="0.5"'),
        rect(x - 1.6, y - 1.9, x + 1.2, y + 1.5, '#cdb68a', 0.3),        # die
    ]
    return '\n'.join(out)


def female_header(x0, y0, n, pitch=10.548, vertical=False, w=10.548):
    """Straight female header seen from the top: black body with square openings.
    (x0, y0) = centre of pin 1; pins along +x (or +y if vertical)."""
    out = []
    L = pitch * n
    if vertical:
        out.append(rect(x0 - w / 2, y0 - pitch / 2, x0 + w / 2, y0 - pitch / 2 + L, HEADER, 0.6))
        for i in range(n):
            cy = y0 + i * pitch
            out.append(rect(x0 - 2.3, cy - 2.3, x0 + 2.3, cy + 2.3, '#050505'))
            out.append(rect_stroke(x0 - 3.4, cy - 3.4, x0 + 3.4, cy + 3.4, '#38383b', 0.7))
    else:
        out.append(rect(x0 - pitch / 2, y0 - w / 2, x0 - pitch / 2 + L, y0 + w / 2, HEADER, 0.6))
        for i in range(n):
            cx = x0 + i * pitch
            out.append(rect(cx - 2.3, y0 - 2.3, cx + 2.3, y0 + 2.3, '#050505'))
            out.append(rect_stroke(cx - 3.4, y0 - 3.4, cx + 3.4, y0 + 3.4, '#38383b', 0.7))
    return '\n'.join(out)


def tact_switch(x, y, cap=CAP_RED):
    """6 × 6 mm tact switch (gull-wing legs at the top and bottom corners), (x, y) = cap centre."""
    out = []
    for dx in (-1, 1):
        for dy in (-1, 1):
            out.append(rect(x + dx * 9.0 - 2.3, y + dy * 12.0 - 3.6, x + dx * 9.0 + 2.3, y + dy * 12.0 + 3.6,
                            GOLD, 0.6))
    out.append(rect(x - 12.4, y - 12.4, x + 12.4, y + 12.4, '#d9d8da', 1.2))          # metal cover
    out.append(rect_stroke(x - 12.4, y - 12.4, x + 12.4, y + 12.4, '#a9a8ac', 0.6, 1.2))
    out.append(rect(x - 10.0, y - 10.0, x + 10.0, y + 10.0, '#eeedef', 0.8))
    for dx in (-1, 1):
        for dy in (-1, 1):
            out.append(circle(x + dx * 8.0, y + dy * 8.0, 1.3, '#8a898e'))           # rivets
    out.append(circle(x, y, 7.4, '#a3313a'))
    out.append(circle(x, y, 6.5, cap))
    return '\n'.join(out)


def smd_r(x, y, w, h, body='#202022', vertical=False):
    """0603/0805 chip part: tinned ends and body; (w, h) overall size."""
    if vertical:
        e = h * 0.28
        return '\n'.join([rect(x - w / 2, y - h / 2, x + w / 2, y - h / 2 + e, TIN, 0.3),
                          rect(x - w / 2, y + h / 2 - e, x + w / 2, y + h / 2, TIN, 0.3),
                          rect(x - w / 2, y - h / 2 + e, x + w / 2, y + h / 2 - e, body)])
    e = w * 0.28
    return '\n'.join([rect(x - w / 2, y - h / 2, x - w / 2 + e, y + h / 2, TIN, 0.3),
                      rect(x + w / 2 - e, y - h / 2, x + w / 2, y + h / 2, TIN, 0.3),
                      rect(x - w / 2 + e, y - h / 2, x + w / 2 - e, y + h / 2, body)])


def lit_led(x, y, color='#ffb030'):
    """0805 LED that is on (power LEDs D24/D25)."""
    gid = f'glow{int(x)}{int(y)}'
    svg.defs.append(f'<radialGradient id="{gid}"><stop offset="0" stop-color="#fffbe8"/>'
                    f'<stop offset="0.3" stop-color="#ffe9a0" stop-opacity="0.9"/>'
                    f'<stop offset="0.55" stop-color="{color}" stop-opacity="0.35"/>'
                    f'<stop offset="1" stop-color="{color}" stop-opacity="0"/></radialGradient>')
    return '\n'.join([rect(x - 4.5, y - 8.5, x + 4.5, y - 5.5, TIN, 0.5), rect(x - 4.5, y + 5.5, x + 4.5, y + 8.5, TIN, 0.5),
                      rect(x - 4.8, y - 6.5, x + 4.8, y + 6.5, '#f7d27a', 1),
                      circle(x, y, 10, f'url(#{gid})')])


# ======================================================================
# Carrier board

# pads of the rows (measured, Hough circles)
OUT_PADS = [74.8 + i * 31.65 for i in range(18)]      # GND, OUT15..OUT0, GND
IN_PADS = [655.2 + i * 31.76 for i in range(10)]      # GND, IN7..IN0, GND
AN_PADS = rs_const('ANALOG_X')
AUX_PADS = AUX_Y
I2C_PADS = [359.4, 390.8, 422.2, 453.7]
ROW_Y = 575.0
PITCH = 10.548         # 2.54 mm
# The female headers are drawn on their footprint (their tops lean away from
# the camera in the photo; the pitches below follow the silkscreen).
CN2_X = [255.5 + i * PITCH for i in range(18)]
CN1_X = [750.8 + i * PITCH for i in range(10)]
CN4_X = [532.4 + i * PITCH for i in range(10)]
CN3_Y = [311.6 + i * PITCH for i in range(6)]
# straight single-row sockets of the I2C and SPI modules
CN6_Y = [338.4 + i * PITCH for i in range(4)]
CN5_Y = [411.9 + i * PITCH for i in range(4)]
CN7_Y = [390.6 + i * PITCH for i in range(6)]
I2C_HDR_X = 122.0
SPI_HDR_X = 312.8
# timer pads along the top edge (GND, then the channels) and CN8
TIM = [('1_1N', 'PE8'), ('1_2N', 'PE10'), ('1_4', 'PE14'), ('2_3', 'PB10'), ('2_4', 'PB11'), ('3_3', 'PB0'),
       ('8_1', 'PC6'), ('8_2', 'PC7'), ('8_3', 'PC8'), ('8_4', 'PC9')]
TIM_PADS = [355.7] + [387.1 + i * 31.73 for i in range(10)]
TIM_Y = 32.3
CN8_X = [716.1 + i * PITCH for i in range(12)]
CN8_Y = 29.0
# the grey sockets beside the Nucleo the Morpho pins are bent into (top view)
FELT_TOP = (349.5, 37.0, 720.5, 66.0)
FELT_BOTTOM = (349.5, 347.0, 720.5, 376.0)
# J1 (DE-9, its flange is 7.7 mm above the board): centre of the flange
DSUB = (223.5, 38.0)
# J3 (footprint of an I2C module): centre line
J3_X = 65.4



# carrier parts under the Nucleo and the display (measured on the bare board)
R_ARRAY_X = ([380.7, 391.3, 402.0, 412.6, 423.2, 433.9, 444.5, 454.8] +        # R23..R16
             [496.7, 507.4, 518.0, 528.8, 539.3, 549.9, 560.3, 570.6] +        # R15..R8
             [591.5, 602.2, 612.4, 622.6, 632.8, 643.3, 654.1, 664.5])         # R7..R0
R24_X = [785.0, 795.7, 806.3, 816.6, 827.0, 837.7, 847.9, 858.5]               # R31..R24
R41_X = [555.6 + i * 10.85 for i in range(7)]                                  # R47..R41
R37_Y = 107.7
# CN11 / CN12: 2 × 36 sockets the Nucleo's Morpho pins plug into; the Nucleo
# uses columns 2..36
MORPHO_SOCKET_X = [345.55 + i * 10.548 for i in range(36)]
# rows: under the Nucleo's Morpho pins
MORPHO_SOCKET_Y = [(75.98, 86.52), (328.0, 338.6)]
# holes under the Nucleo
HIDDEN_HOLES = [(419.0, 115.4), (419.0, 303.0), (735.1, 110.6), (729.9, 311.0)]

SEC = 2.7             # width of the section frame lines

# section frame lines (polylines)
SECTIONS = [
    [(-6, 312.3), (300, 312.3)],
    [(213.6, 312.3), (213.6, 464.4)],
    [(-6, 464.4), (429.1, 464.4), (445.7, 480.9), (482.3, 480.9), (497.4, 495.6), (719.2, 495.6),
     (733.4, 481.2), (750.6, 481.2), (762, 469.6), (1008, 469.6)],
    [(634.5, 495.6), (634.5, 655)],
    [(330, 382.7), (344.4, 382.7), (344.4, 464.4)],
    [(413, 464.4), (413, 355.8), (418, 350.8), (731, 350.8), (761, 380.8), (761, 469.6)],
    [(895.1, 190.5), (895.1, 469.6)],          # above y = 300 never seen (under the display), assumed
    [(869.0, -7), (869.0, 53.0), (851.0, 71.0), (738.0, 71.0)],      # Timer frame (bare board)
    [(339.5, -7), (339.5, 69.7)],
]


def L(text, x, y, h=5.3, anchor='c', rot=0, sw=None, adv=1.0):
    return dict(text=text, x=x, y=y, h=h, anchor=anchor, rot=rot, sw=sw, adv=adv)


def _u45(x, y, du, dn=0.0):
    """Point moved du along the 45° direction (down-right) and dn across it (down-left)."""
    c = math.sqrt(0.5)
    return x + (du - dn) * c, y + (du + dn) * c


def carrier_labels():
    out = []
    S, P = 5.3, 3.7            # cap heights of normal and small labels
    # ---- top left: CAN, RS232
    out += [L('J2', 27.4, 6.2, 4.8), L('9.', 70.6, 21.8), L('6.', 105.2, 21.8), L('5', 63.8, 55.3), L('1', 108.2, 55.3),
            L('CAN', 30.3, 70.7), L('C9', 53.1, 74.9), L('U6', 97.2, 89.8), L('JP2', 114.8, 74.9), L('JP1', 145.6, 75.7),
            L('RS232', 174.7, 71.4), L('U5', 260.8, 72.0), L('J1', 166.5, 7.2, 4.8)]
    # ---- I2C
    out += [L('J3', 24.7, 375.4), L('I2C1_5V', 51.8, 386.0), L('CN6', 123.7, 327.0), L('I2C1_5V', 123.7, 381.6),
            L('CN5', 123.7, 400.1), L('I2C1_3V3', 123.7, 455.2), L('I2C1', 59.3, 422.3, 15.0, sw=3.3)]
    for t, y in zip(['5V', 'SCL', 'GND', 'SDA'], [338.2, 348.9, 359.0, 369.7]):
        out.append(L(t, 131.6, y, P, 'l'))
    for t, y in zip(['3V3', 'SCL', 'GND', 'SDA'], [411.9, 422.0, 432.7, 442.9]):
        out.append(L(t, 131.6, y, P, 'l'))
    for (a, b), y in zip([('I2C1_SCL', 'PB8'), ('I2C1_SDA', 'PB9'), ('3V3', None), ('GND', None)], I2C_PADS):
        if b:
            out += [L(a, 166.5, y - 5.4, 4.6, 'l'), L(b, 166.5, y + 4.2, P, 'l')]
        else:
            out.append(L(a, 166.5, y, 4.6, 'l'))
    # ---- SPI
    out.append(L('SPI3', 249.4, 327.9, 15.5, 'l', sw=3.3))
    for (a, b), y in zip([('SPI3_SCK', 'PC10'), ('SPI3_MOSI', 'PC12'), ('SPI3_MISO', 'PC11'), ('SPI3_CS', 'PA15')],
                         I2C_PADS):
        out += [L(a, 250.4, y - 5.6, 4.6, 'l'), L(b, 250.4, y + 4.2, P, 'l')]
    out += [L('CN7', 312.9, 378.7), L('SPI3', 312.9, 455.2)]
    for t, y in zip(['SCK', 'MOSI', 'MISO', 'CS', '3V3', 'GND'], [390.6, 401.2, 411.4, 422.0, 432.7, 442.9]):
        out.append(L(t, 321.3, y, P, 'l'))
    # ---- Out
    out.append(L('Out', 48.2, 549.1, 14.5, sw=3.0))
    for i, x in enumerate(OUT_PADS):
        bit = 16 - i
        out.append(L('GND' if i in (0, 17) else f'OUT{bit}', x, 589.4))
        if 0 < i < 17:
            out.append(L(f'PE{bit - 8}' if bit >= 8 else f'PD{bit}', x, 561.6, 4.6))
            out.append(L(f'D{bit + 8}', x, 523.1))
    cn2 = CN2_X
    out += [L('CN2', 230.5, 496.6, anchor='l'), L('Out 15-0', 443.5, 496.4, anchor='l')]
    for i, x in enumerate(cn2):
        bit = 16 - i
        if 0 < i < 17:
            out.append(L(f'Out{bit}', x, 487.8, P, 'r', 90))
            out.append(L(f'PE{bit - 8}' if bit >= 8 else f'PD{bit}', x, 506.0, P, 'l', 90))
        else:
            out.append(L('GND', x, 506.0, P, 'l', 90))
    # ---- In
    out.append(L('In', 973.4, 549.0, 15.0, sw=3.0))
    for i, x in enumerate(IN_PADS):
        bit = 8 - i
        out.append(L('GND' if i in (0, 9) else f'IN{bit}', x, 589.4))
        if 0 < i < 9:
            out += [L(f'PF{bit}', x, 561.9, 4.6), L(f'D{bit}', x, 523.1), L(f'S{bit}', x, 641.3)]
    cn1 = CN1_X
    out += [L('CN1', 735.0, 496.9), L('IN 7-0', 853.7, 496.9, anchor='l')]
    for i, x in enumerate(cn1):
        bit = 8 - i
        if 0 < i < 9:
            out.append(L(f'IN{bit}', x, 489.5, P, 'r', 90))
            out.append(L(f'PF{bit}', x, 505.5, P, 'l', 90))
        else:
            out.append(L('GND', x, 505.5, P, 'l', 90))
    # ---- Analog
    out.append(L('Analog', 687.7, 398.8, 16.0, sw=3.2, adv=0.95))
    names = ['DAC1', 'GNDA', 'ADC123_13', 'ADC123_12', 'ADC123_10', 'ADC12_9', 'ADC3_8', 'ADC3_7', 'ADC3_6', 'GNDA']
    ports = ['PA4', None, 'PC3', 'PC2', 'PC0', 'PB1', 'PF10', 'PF9', 'PF8', None]
    for x, a, b in zip(AN_PADS, names, ports):
        out.append(L(a, *_u45(x, 448.6, 9.4), S, 'l', 45))
        if b:
            out.append(L(b, *_u45(x, 448.6, 9.4, 7.0), P, 'l', 45))
    cn4 = CN4_X
    out.append(L('CN4', 526.0, 401.1, anchor='r'))
    for x, t in zip(cn4, ['DAC1', 'AGND', 'ADC123_13', 'ADC123_12', 'ADC123_10', 'ADC12_9', 'ADC3_8', 'ADC3_7',
                          'ADC3_6', 'AGND']):
        out.append(L(t, x + 1.5, 408.0, P, 'l', 45))
    # ---- Aux
    out.append(L('Aux', 925.0, 232.5, 16.0, 'l', sw=3.2))
    for n, y in enumerate(AUX_PADS):
        out += [L(f'AUX{n}', 957.0, y - 5.2, S, 'l'), L(f'PG{n}', 957.0, y + 4.5, P, 'l')]
    for n in range(6):
        out.append(L(f'PG{n}', 916.5, CN3_Y[n], P, 'r'))
    out += [L('CN3', 921.0, 298.2, 5.0), L('AUX0-5', 925.1, 389.4, 5.0, rot=90)]
    # ---- top right
    out += [L('R50', 925.9, 1.5, 4.6), L('R49', 957.2, 1.5, 4.6), L('5V', 925.0, 27.2), L('3V3', 957.2, 27.2),
            L('D25', 924.3, 58.5), L('D24', 956.4, 58.5)]
    # ---- Timer: title, the pads along the top edge (names run off the board)
    # and CN8 with the same signals
    out.append(L('Timer', 781.0, 47.6, 16.0, 'l', sw=3.2))
    out.append(L('GND', TIM_PADS[0] + 4.4, TIM_Y - 10.6, S, 'l', -45))
    for x, (t, p) in zip(TIM_PADS[1:], TIM):
        out += [L('TIM' + t, x + 6.8, TIM_Y - 9.8, S, 'l', -45), L(p, x + 11.8, TIM_Y - 3.3, 4.6, 'l', -45)]
    out.append(L('CN8', 700.8, 31.5, 5.0))
    for x, t in zip(CN8_X, ['GND'] + ['TIM' + t for t, _ in TIM] + ['GND']):
        out.append(L(t, x - 0.7, 22.8, 3.9, 'l', -45))
    # ---- around U5
    out.append(L('C7', 282.0, 144.3))
    return out + hidden_labels()


def hidden_labels():
    """Labels under the Nucleo and the display (measured on the bare board)."""
    out = [L('C8', 335.6, 81.2), L('C6', 335.6, 102.4), L('C5', 335.8, 123.0), L('CN11', 356.5, 101.5),
           L('CN12', 357.0, 316.6), L('U4', 356.1, 153.2), L('C4', 340.6, 191.8), L('U3', 415.1, 170.0),
           L('U2', 509.9, 170.0), L('U1', 603.7, 170.6), L('C3', 476.0, 202.0), L('C2', 570.4, 202.5),
           L('C1', 664.0, 188.0), L('C12', 707.7, 134.8), L('C13', 691.2, 177.0), L('C11', 759.4, 131.5),
           L('C10', 775.4, 177.3), L('VR1', 732.8, 193.8), L('R48', 815.0, 120.3), L('CN0', 832.4, 173.0)]
    for n, y in zip((34, 35, 36), (155.5, 165.8, 176.0)):
        out.append(L(f'R{n}', 313.0, y))
    for n, x in zip((33, 32), (363.8, 397.7)):
        out.append(L(f'R{n}', x, 207.6, rot=-90))
    for n in range(37, 41):
        out.append(L(f'R{n}', 615.0, R37_Y + (n - 37) * 10.6, anchor='l'))
    for x, n in zip(R_ARRAY_X, list(range(23, -1, -1))):
        out.append(L(f'R{n}', x, 248.0, anchor='l', rot=-90))
    for x, n in zip(R24_X, range(31, 23, -1)):
        out.append(L(f'R{n}', x, 245.5, anchor='l', rot=-90))
    for x, n in zip(R41_X, range(47, 40, -1)):
        out.append(L(f'R{n}', x, 377.0, anchor='l', rot=-90))
    return out


NOTCH = (846.0, 93.5, 190.5)    # cut-out at the right edge (cable of the Nucleo's USB): x, top, bottom


def board_outline():
    r = CORNER
    x, t, b = (f(v) for v in NOTCH)
    a = f'A{f(r)},{f(r)} 0 0 1'     # convex corner
    c = f'A{f(r)},{f(r)} 0 0 0'     # concave corner (inside the notch)
    l, u, rr, d = X0, Y0, X1, Y1
    return (f'M{f(l + r)},{f(u)} H{f(rr - r)} {a} {f(rr)},{f(u + r)} V{f(NOTCH[1] - r)} {a} {f(rr - r)},{t} '
            f'H{f(NOTCH[0] + r)} {c} {x},{f(NOTCH[1] + r)} V{f(NOTCH[2] - r)} {c} {f(NOTCH[0] + r)},{b} '
            f'H{f(rr - r)} {a} {f(rr)},{f(NOTCH[2] + r)} V{f(d - r)} {a} {f(rr - r)},{f(d)} H{f(l + r)} '
            f'{a} {f(l)},{f(d - r)} V{f(u + r)} {a} {f(l + r)},{f(u)} Z')


def board_clip():
    """The board outline with the holes cut out."""
    holes = ''.join(f'M{f(x - r)},{f(y)} a{f(r)},{f(r)} 0 1 0 {f(2 * r)},0 a{f(r)},{f(r)} 0 1 0 {f(-2 * r)},0 Z'
                    for x, y, r in HOLES)
    svg.defs.append(f'<clipPath id="board"><path clip-rule="evenodd" d="{board_outline()} {holes}"/></clipPath>')


LABEL_FIT = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'boardlabels.json')


def label_key(l):
    return f"{l['text']}@{l['x']:.1f},{l['y']:.1f}"


def fitted(labels):
    """Labels with the corrections found by boardlabelfit.py (position, size and
    width fitted to the silkscreen in the photo)."""
    import json
    fit = json.load(open(LABEL_FIT)) if os.path.exists(LABEL_FIT) else {}
    unused = set(fit) - {label_key(l) for l in labels}
    if unused:
        print(f'{len(unused)} label fits do not match any label any more, run boardlabelfit.py', file=sys.stderr)
    out = []
    for l in labels:
        l = dict(l)
        if label_key(l) in fit:
            dx, dy, hs, adv = fit[label_key(l)]
            l.update(x=l['x'] + dx, y=l['y'] + dy, h=l['h'] * hs, adv=adv,
                     sw=(l['sw'] if l['sw'] is not None else 0.2 * l['h']))
        out.append(l)
    return out


EDGE_GAP = 1.5     # silkscreen keeps this distance from the board edge


def silk_labels(labels):
    out = []
    for l in fitted(labels):
        x, y = l['x'], l['y']
        sw = l['sw'] if l['sw'] is not None else 0.2 * l['h']
        pts = [p for pl in text_lines(l['text'], x, y, l['h'], l['anchor'], l['rot'], l['adv']) for p in pl]
        if pts:
            x += (max(0.0, X0 + EDGE_GAP + sw / 2 - min(p[0] for p in pts))
                  - max(0.0, max(p[0] for p in pts) + sw / 2 + EDGE_GAP - X1))
            y += (max(0.0, Y0 + EDGE_GAP + sw / 2 - min(p[1] for p in pts))
                  - max(0.0, max(p[1] for p in pts) + sw / 2 + EDGE_GAP - Y1))
        out.append(stext(x, y, l['text'], l['h'], l['anchor'], l['rot'], sw, adv=l['adv']))
    return '\n'.join(out)


TRACES = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'boardtraces.json')


def traces():
    """Copper traces under the mask (fine dark lines, vectorised by boardtraces.py)."""
    import json
    if not os.path.exists(TRACES):
        return ''
    d = ''.join(f'M{f(x0)},{f(y0)}L{f(x1)},{f(y1)}' for x0, y0, x1, y1 in json.load(open(TRACES)))
    # copper under the mask: a slightly lighter band with darker edges
    svg.defs.append(f'<path id="traces" d="{d}" fill="none" stroke-linecap="round" stroke-linejoin="round"/>')
    return ('<use href="#traces" stroke="#132d52" stroke-width="1.4"/>\n'
            '<use href="#traces" stroke="#23467a" stroke-width="0.7"/>')


def logo():
    """HAW Hamburg logo and the board title (Open Sans, Noto Serif)."""
    out = []
    for i in range(8):
        y = 119.2 + i * 8.31
        x0, x1 = (36.0, 82.6) if i % 2 == 0 else (53.0, 99.9)
        out.append(rect(x0, y, x1, y + 3.7, SILK, 0.4))
    out.append(ftext(116.1, 140.2, 'HAW', 'OpenSans-Bold.ttf', 19.7, width=64.3))
    out.append(ftext(116.1, 173.2, 'HAMBURG', 'OpenSans-Bold.ttf', 19.6, width=140.4))
    out.append(ftext(36.0, 203.1, 'Department Informatik', 'OpenSans-Regular.ttf', 9.2, width=146.0))
    out.append(ftext(36.0, 214.4, 'Labor für Technische Informatik', 'OpenSans-Regular.ttf', 6.9, width=138.0))
    out.append(ftext(36.0, 239.7, 'ITS-BRD', 'NotoSerif-Bold.ttf', 17.9, width=110.7))
    out.append(line(36.8, 251.95, 100.0, 251.95, SILK, 2.2))
    out.append(ftext(36.0, 270.3, 'Rev 02 15.12.2020 JAN', 'OpenSans-Regular.ttf', 9.2, width=140.1))
    return '\n'.join(out)


def carrier():
    out = [path(board_outline(), MASK), traces(), logo()]
    # copper text "Top" (under the mask, bottom left)
    out.append(rect(48.1, 628.5, 64.0, 638.1, MASK_CU))
    out.append(stext(56.0, 633.3, 'Top', 6.0, color='#3a6491', sw=1.1))

    # ---- silkscreen
    for pl in SECTIONS:
        out.append(polyline(pl, SILK, SEC, cap='square', join='miter'))
    sw = 1.0
    # J2 (CAN, D-sub footprint not fitted) and the RS232 footprint frame
    for x0, x1 in [(21.8, 56.1), (57.2, 114.6), (115.8, 151.1)]:
        out.append(rect_stroke(x0, 12.5, x1, 64.1, SILK, sw))
    out.append(polyline([(159.6, 64.7), (278.0, 64.7)], SILK, sw))
    out.append(polyline([(282.7, 12.4), (289.5, 12.4), (289.5, 58.5)], SILK, sw))
    # C9, U6, JP1, JP2
    out.append(rect_stroke(48.4, 83.0, 55.8, 97.0, SILK, 0.8))
    out.append(rect_stroke(68.5, 82.9, 90.1, 96.9, SILK, sw))
    out.append(circle(72.8, 91.6, 1.7, SILK))
    for bx, dot in [((105.7, 84.7, 128.1, 96.2), (126.2, 81.8)), ((137.8, 84.7, 159.9, 96.4), (158.3, 81.8))]:
        out.append(rect_stroke(*bx, SILK, sw))
        out.append(circle(*dot, 0.7, SILK))
    # J3 (I2C module footprint), header brackets
    # (outline, holes and the pin group are symmetric about x = J3_X)
    out.append(rect_stroke(J3_X - 33.5, 316.9, J3_X + 33.5, 379.3, SILK, sw))
    out.append(line(129.4, 332.6, 129.4, 375.4, SILK, sw))
    out.append(line(129.4, 405.8, 129.4, 449.6, SILK, sw))
    out.append(line(319.0, 385.0, 319.0, 449.0, SILK, sw))
    out.append(line(919.6, 306.0, 919.6, 369.7, SILK, sw))
    # outlines along the long headers
    # LED outlines (thin frames around the packages)
    out.append(silk_labels(carrier_labels()))

    # ---- holes and pads
    for x, y in [(15.4, 15.4), (316.5, 16.5), (708.2, 14.0), (985.2, 13.3), (985.2, 75.9), (985.2, 209.2),
                 (16.3, 326.4), (15.1, 633.3), (623.7, 633.2), (985.0, 633.8), (J3_X - 27.45, 338.0),
                 (J3_X + 27.45, 338.0)]:
        out.append(mount_hole(x, y))
    for x in TIM_PADS:
        out.append(pad(x, TIM_Y))
    for x in (33.6, 137.9):
        out.append(pad(x, 37.9, 9.2, 6.4))
    for i, x in enumerate([68.3, 79.9, 91.4, 102.9]):
        out.append(small_pad(x, 31.9))
    for i, x in enumerate([62.5, 73.9, 85.5, 97.2, 108.9]):
        out.append(small_pad(x, 43.6, square=(i == 4)))
    for (x, y), sq in [((J3_X - 6.8, 348.9), 0), ((J3_X + 2.2, 348.9), 0), ((J3_X + 11.2, 348.9), 0),
                       ((J3_X - 11.3, 356.2), 1), ((J3_X - 2.3, 356.2), 0), ((J3_X + 6.7, 356.2), 0)]:
        out.append(small_pad(x, y, 3.0, 1.4, sq))
    for x, sq in [(111.1, 0), (122.0, 1), (143.5, 0), (154.1, 1)]:
        out.append(small_pad(x, 90.3, 3.6, 1.6, sq))
    for x in OUT_PADS:
        out.append(pad(x, ROW_Y))
    for x in IN_PADS:
        out.append(pad(x, ROW_Y))
    for x in AN_PADS:
        out.append(pad(x, rs_const('ANALOG_Y')))
    for y in AUX_PADS:
        out.append(pad(AUX_X, y))
    for y in I2C_PADS:
        out.append(pad(155.4, y))
        out.append(pad(239.4, y))
    # unpopulated SMD footprints: U6, C9 is fitted
    for x in [71.4, 76.7, 81.9, 87.2]:
        out.append(rect(x - 1.0, 76.3, x + 1.0, 81.2, TIN, 0.3))
        out.append(rect(x - 1.0, 98.6, x + 1.0, 103.5, TIN, 0.3))
    out.append(smd_r(52.1, 90.0, 5.0, 12.0, '#c9b48e', vertical=True))

    # ---- parts
    for x in OUT_X + IN_X:
        out.append(led_smd(x, LED_Y))
    for x in BUTTON_X:
        out.append(tact_switch(x, BUTTON_Y))
    out.append(female_header(CN2_X[0], 497.0, 18))
    out.append(female_header(CN1_X[0], 496.5, 10))
    out.append(female_header(CN4_X[0], 401.0, 10))
    out.append(female_header(923.4, CN3_Y[0], 6, vertical=True))
    out.append(female_header(CN8_X[0], CN8_Y, 12))
    out.append(female_header(I2C_HDR_X, CN6_Y[0], 4, vertical=True))
    out.append(female_header(I2C_HDR_X, CN5_Y[0], 4, vertical=True))
    out.append(female_header(SPI_HDR_X, CN7_Y[0], 6, vertical=True))
    for i in range(7):                               # chip parts above CN4
        out.append(smd_r(555.6 + i * 10.85, 384.8, 4.2, 5.4, '#e9e4d8', vertical=True))
    # C7 beside U5
    out.append(rect_stroke(274.4, 129.6, 289.0, 137.4, SILK, 0.8))
    out.append(smd_r(281.7, 133.5, 11.5, 5.4, '#c9b48e'))
    for x in (925.9, 957.2):                         # R50, R49
        out.append(smd_r(x, 9.9, 13.6, 6.0, '#262628'))
    out += [lit_led(925.9, 40.7), lit_led(957.2, 40.7)]
    out.append(rs232())
    out.append(carrier_hidden())
    return '\n'.join(out)


def socket_2row(x0, ys, n, pitch=10.548):
    """2-row female socket from above: (x0, ys) = centres of the first column."""
    out = [rect(x0 - pitch / 2, ys[0] - pitch / 2, x0 - pitch / 2 + n * pitch, ys[1] + pitch / 2, HEADER, 0.6)]
    cells = ''.join(f'M{f(x0 + i * pitch - 2.3)},{f(y - 2.3)}h4.6v4.6h-4.6Z' for i in range(n) for y in ys)
    frames = ''.join(f'M{f(x0 + i * pitch - 3.4)},{f(y - 3.4)}h6.8v6.8h-6.8Z' for i in range(n) for y in ys)
    out.append(path(frames, 'none', ' stroke="#38383b" stroke-width="0.7"'))
    out.append(path(cells, '#050505'))
    return '\n'.join(out)


def tantalum(x, y, w, h):
    """Tantalum capacitor standing vertically (polarity band at the top), (x, y) = centre."""
    return '\n'.join([rect(x - w * 0.33, y - h / 2, x + w * 0.33, y + h / 2, TIN, 0.4),
                      rect(x - w / 2, y - h / 2 + 2.2, x + w / 2, y + h / 2 - 2.2, '#e0a83a', 0.8),
                      rect(x - w / 2, y - h / 2 + 2.2, x + w / 2, y - h / 2 + 5.2, '#c88d27', 0.8)])


def soic(x0, y0, x1, y1, n, horizontal, pitch=5.27, lead=5.5, mark=None):
    """SOIC: body box and `n` leads per side (on the long sides: top/bottom if
    `horizontal`); `mark` = (x, y) of the pin 1 dimple."""
    leads = []
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    for i in range(n):
        o = (i - (n - 1) / 2) * pitch
        if horizontal:
            leads.append(f'M{f(cx + o - 1.0)},{f(y0 - lead)}h2v{f(y1 - y0 + 2 * lead)}h-2Z')
        else:
            leads.append(f'M{f(x0 - lead)},{f(cy + o - 1.0)}v2h{f(x1 - x0 + 2 * lead)}v-2Z')
    out = [path(''.join(leads), '#c9c4b8'), rect(x0, y0, x1, y1, '#262627', 0.8)]
    if mark:
        out.append(circle(*mark, 1.6, '#1a1a1b', ' stroke="#3a3a3b" stroke-width="0.4"'))
    return '\n'.join(out)


def carrier_hidden():
    """The parts the Nucleo and the display hide (seen in the see-through view)."""
    out = []
    for ys in MORPHO_SOCKET_Y:
        out.append(socket_2row(MORPHO_SOCKET_X[0], ys, 36))
        x0, x1 = MORPHO_SOCKET_X[0] - 6.3, MORPHO_SOCKET_X[-1] + 6.3
        out.append(rect_stroke(x0, ys[0] - 6.3, x1, ys[1] + 6.3, SILK, 0.9))
    for x, y in HIDDEN_HOLES:
        out.append(mount_hole(x, y, 5.6))
    # the three bus buffers (SN74LVC245A, SO-20, pin 1 at the top right), U4 (SO-8)
    for x0, mark in [(412.2, (460.4, 192.2)), (508.0, (556.0, 192.0)), (602.3, (650.6, 192.8))]:
        out.append(soic(x0, 186.0, x0 + 53.2, 216.0, 10, True, mark=mark))
        for dy, t in [(-3.0, '78CYVDK'), (4.5, 'LVC245A')]:
            out.append(ftext(x0 + 26.6, 201.0 + dy, t, 'LiberationSans-Regular.ttf', 3.6, 'c', '#6d6d6a', 180,
                             mid=True))
    out.append(soic(357.6, 160.0, 371.4, 180.2, 4, False, lead=4.6, mark=(368.6, 163.2)))
    # resistors and capacitors (0603 with their pads)
    for x in R_ARRAY_X + R24_X:
        out.append(smd_r(x, 260.0 if x < 700 else 256.6, 6.0, 13.5, '#242425', vertical=True))
    for x in R41_X:
        out.append(smd_r(x, 384.8, 6.0, 13.5, '#242425', vertical=True))
    for i in range(4):
        out.append(smd_r(602.0, R37_Y + i * 10.6, 13.5, 6.0, '#242425'))
    for y in (155.5, 165.8, 176.0):
        out.append(smd_r(333.3, y, 14.0, 6.6, '#242425'))
    for x, y in [(375.5, 207.7), (386.1, 207.7)]:
        out.append(smd_r(x, y, 6.0, 13.5, '#242425', vertical=True))
    for x, y in [(323.2, 80.9), (323.2, 102.3), (323.2, 123.5), (475.8, 216.9), (569.8, 217.3), (664.6, 203.0),
                 (707.2, 149.8), (759.4, 147.4)]:
        out.append(smd_r(x, y, 6.2, 14.0, '#c4a57a', vertical=True))
    out.append(smd_r(359.8, 191.8, 14.0, 6.0, '#c4a57a'))                          # C4
    out.append(smd_r(812.1, 136.0, 6.0, 13.0, '#242425', vertical=True))           # R48
    # VR1 (LF33 in DPAK, tab at the bottom), C13 and C10 (tantalum, case C)
    for x in (724.3, 743.6):
        out.append(rect(x - 1.6, 140.5, x + 1.6, 153.5, TIN, 0.4))
    out.append(rect(721.0, 177.0, 747.0, 186.5, TIN, 0.8))
    out.append(rect(720.3, 153.2, 747.3, 179.0, '#262627', 0.8))
    out.append(ftext(733.8, 166.0, 'LF33A', 'LiberationSans-Regular.ttf', 3.4, 'c', '#6d6d6a', 90, mid=True))
    out.append(tantalum(693.2, 154.1, 13.6, 25.2))
    out.append(tantalum(777.3, 154.6, 13.6, 26.2))
    out += [line(683.0, 137.7, 701.0, 137.7, SILK, 0.9), line(767.5, 139.0, 787.0, 139.0, SILK, 0.9)]
    # CN0 (micro USB, opening into the notch)
    for i in range(5):
        out.append(rect(824.5, 139.0 + i * 2.7, 830.0, 140.3 + i * 2.7, TIN, 0.2))
    out.append(rect(828.0, 129.8, 853.5, 163.2, '#bdbab2', 1.2, ' stroke="#8f8c84" stroke-width="0.5"'))
    out.append(rect(831.0, 133.0, 850.5, 160.0, '#cfccc4', 0.8))
    for y in (128.5, 160.5):
        out.append(rect(826.5, y - 1.8, 834.0, y + 3.0, TIN, 0.5))
    return '\n'.join(out)


def d_shape(cx, cy, top, bottom, h, rt, rb):
    """D-sub outline: trapezoid (half widths `top` and `bottom`, half height `h`)
    with corner radii `rt` at the top and `rb` at the bottom."""
    pts = [(cx - top, cy - h), (cx + top, cy - h), (cx + bottom, cy + h), (cx - bottom, cy + h)]
    d = ''
    for i in range(4):
        r = rt if i < 2 else rb
        p0, p1, p2 = pts[i - 1], pts[i], pts[(i + 1) % 4]
        a = (p0[0] - p1[0], p0[1] - p1[1])
        b = (p2[0] - p1[0], p2[1] - p1[1])
        la, lb = math.hypot(*a), math.hypot(*b)
        s = (p1[0] + a[0] / la * r, p1[1] + a[1] / la * r)
        e = (p1[0] + b[0] / lb * r, p1[1] + b[1] / lb * r)
        d += f'{"M" if i == 0 else "L"}{f(s[0])},{f(s[1])} Q{f(p1[0])},{f(p1[1])} {f(e[0])},{f(e[1])} '
    return d + 'Z'


def shade(color, k):
    """`color` lighter (k > 0) or darker (k < 0)."""
    c = [int(color[i:i + 2], 16) for i in (1, 3, 5)]
    c = [v + (255 - v) * k if k > 0 else v * (1 + k) for v in c]
    return '#' + ''.join(f'{int(round(min(max(v, 0), 255))):02x}' for v in c)


def hex_standoff(x, y, af=19.6, metal='#c4bfb3'):
    """Hex standoff (screw lock of the D-sub) from above: chamfered top face
    with the threaded hole; `af` = width across flats.  Light from the top left."""
    R = af / 2 / math.cos(math.radians(30))
    outer = [(x + R * math.cos(math.radians(a)), y + R * math.sin(math.radians(a))) for a in range(0, 360, 60)]
    inner = [(x + 0.8 * (px - x), y + 0.8 * (py - y)) for px, py in outer]
    out = [poly(outer, shade(metal, -0.35))]
    for i in range(6):                    # chamfer facets, lit by their direction
        a = math.radians(30 + 60 * i)
        lit = -(math.cos(a) + math.sin(a)) / math.sqrt(2)
        out.append(poly([outer[i], outer[(i + 1) % 6], inner[(i + 1) % 6], inner[i]],
                        shade(metal, 0.3 * lit if lit > 0 else 0.3 * lit)))
    out.append(poly(inner, metal))
    out.append(circle(x, y, 5.4, '#4a4740'))                 # threaded hole
    out.append(ring(x, y, 4.3, '#6b675d', 0.7))
    out.append(circle(x, y, 3.2, '#1a1917'))
    return '\n'.join(out)


def rs232():
    """J1 (female DE-9 with hex standoffs, from above) and U5 (MAX3232, wide SOIC-16)."""
    out = []
    # U5, pin 1 at the bottom left
    for i in range(8):
        y = 83.3 + i * 5.33
        out.append(rect(256.5, y - 1.1, 266.5, y + 1.1, TIN, 0.4))
        out.append(rect(293.0, y - 1.1, 303.0, y + 1.1, TIN, 0.4))
    out.append(rect(266.0, 80.0, 293.5, 124.7, '#2a2a2b', 0.8))
    out.append(path('M276.9,124.7 A2.8,2.8 0 0 1 282.5,124.7 Z', '#161617'))
    for x, s, cap in [(273.4, 'MAXIM', 4.0), (280.6, 'MAX3232ECWE', 2.5), (286.2, '1936', 2.5)]:
        out.append(ftext(x, 102.3, s, 'LiberationSans-Bold.ttf' if cap > 3 else 'LiberationSans-Regular.ttf', cap,
                         'c', '#727270', -90, mid=True))
    # J1: flange, D shell, insulator with the contacts, standoffs.  The narrow
    # side of the D (4 contacts) is at the top; the D tapers by 10° and its
    # corners on the narrow side are rounder.
    cx, cy = DSUB
    metal = '#c4bfb3'
    out.append(rect(cx - 63.5, cy - 25.5, cx + 63.5, cy + 25.5, metal, 7.5,
                    f' stroke="{shade(metal, -0.3)}" stroke-width="0.8"'))
    out.append(rect_stroke(cx - 62.0, cy - 24.0, cx + 62.0, cy + 24.0, shade(metal, 0.25), 0.7, 6.2))
    out.append(path(d_shape(cx, cy + 0.4, 31.8, 36.8, 19.6, 10.0, 7.0), shade(metal, -0.22)))    # flank
    out.append(path(d_shape(cx, cy, 29.6, 34.6, 17.6, 9.0, 6.2), shade(metal, 0.2),
                    f' stroke="{shade(metal, -0.15)}" stroke-width="0.5"'))                   # rim
    out.append(path(d_shape(cx, cy, 26.6, 31.6, 14.8, 7.5, 5.0), '#36352f'))                  # insulator
    for i in range(4):
        out.append(circle(cx - 17.25 + i * 11.5, cy - 6.0, 2.7, '#0b0b0a', ' stroke="#57554c" stroke-width="0.5"'))
    for i in range(5):
        out.append(circle(cx - 23.0 + i * 11.5, cy + 6.0, 2.7, '#0b0b0a', ' stroke="#57554c" stroke-width="0.5"'))
    for dx in (-51.9, 51.9):
        out.append(hex_standoff(cx + dx, cy, metal=metal))
    return '\n'.join(out)


# ======================================================================
# NUCLEO-F429ZI (MB1137).  The Nucleo and the display stand above the
# carrier; their parts are given in photo coordinates and drawn through
# layer() (see NADIR).  B1, B2 and the display's active area are at the
# positions in board.rs, which are top view coordinates.

N_SILK = '#4d63a3'
N_BLUE = '#1d6ad2'
N_FONT = 'LiberationSans-Regular.ttf'
N_BOLD = 'LiberationSans-Bold.ttf'
MORPHO_X = [341.5 + i * 11.01 for i in range(35)]


def on_nucleo(h, s):
    """Part whose top is h mm above the Nucleo, in photo coordinates of that top
    (nested in layer(M_NUCLEO), which takes care of the Nucleo's own height)."""
    return f'<g transform="{layer(mag(14.0 + h) / M_NUCLEO)}">\n{s}\n</g>'


def nt(x, y, s, cap=2.9, rot=90, fill=N_SILK, font=N_FONT, anchor='c'):
    return ftext(x, y, s, font, cap, anchor, fill, rot, mid=True)


def chip(x0, y0, x1, y1, pins_lr=0, pins_tb=0, pitch=5.27, color='#232325', plen=2.6, pw=1.6):
    """Black IC body with tinned gull-wing pins on two sides."""
    out = []
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    for i in range(pins_tb):
        x = cx + (i - (pins_tb - 1) / 2) * pitch
        out.append(rect(x - pw / 2, y0 - plen, x + pw / 2, y1 + plen, '#c9c3b5', 0.3))
    for i in range(pins_lr):
        y = cy + (i - (pins_lr - 1) / 2) * pitch
        out.append(rect(x0 - plen, y - pw / 2, x1 + plen, y + pw / 2, '#c9c3b5', 0.3))
    out.append(rect(x0, y0, x1, y1, color, 0.8))
    return '\n'.join(out)


MORPHO_ROWS = (64.5, 76.3, 327.5, 339.3)


def morpho_pin(x, y):
    """Solder ring of a Morpho pin (the pin itself is drawn by morpho_pins())."""
    return '\n'.join([circle(x, y, 3.6, GOLD), circle(x, y, 1.7, '#9a7330')])


def zio(x0, y0, x1, y1, xs, ys):
    """ZIO 2 × 6 female socket (8.5 mm high)."""
    out = [rect(x0, y0, x1, y1, '#2a2a2c', 1.0)]
    for x in xs:
        for y in ys:
            out.append(rect(x - 3.6, y - 3.6, x + 3.6, y + 3.6, '#111112', 0.4))
            out.append(rect(x - 2.0, y - 2.0, x + 2.0, y + 2.0, '#050505'))
    return '\n'.join(out)


def nucleo_logo(x, y, s=1.0):
    """The blue Nucleo emblem (four lobes with an S), centred at (x, y)."""
    out = []
    for dx, dy in [(-6.5, -7.5), (6.5, -7.5), (-6.5, 7.5), (6.5, 7.5)]:
        out.append(f'<ellipse cx="{f(x + dx * s)}" cy="{f(y + dy * s)}" rx="{f(5.6 * s)}" ry="{f(4.8 * s)}" '
                   f'fill="{N_BLUE}"/>')
    out.append(f'<ellipse cx="{f(x)}" cy="{f(y)}" rx="{f(9.0 * s)}" ry="{f(13.5 * s)}" fill="{N_BLUE}"/>')
    out.append(f'<ellipse cx="{f(x)}" cy="{f(y - 16.0 * s)}" rx="{f(3.6 * s)}" ry="{f(3.0 * s)}" fill="{N_BLUE}"/>')
    out.append(f'<ellipse cx="{f(x)}" cy="{f(y + 16.0 * s)}" rx="{f(3.6 * s)}" ry="{f(3.0 * s)}" fill="{N_BLUE}"/>')
    # the S (rotated with the board: runs along y)
    d = (f'M{f(x + 4.5 * s)},{f(y - 9 * s)} C{f(x + 4.5 * s)},{f(y - 14 * s)} {f(x - 4.5 * s)},{f(y - 14 * s)} '
         f'{f(x - 4.5 * s)},{f(y - 7 * s)} C{f(x - 4.5 * s)},{f(y - 1 * s)} {f(x + 4.5 * s)},{f(y + 1 * s)} '
         f'{f(x + 4.5 * s)},{f(y + 7 * s)} C{f(x + 4.5 * s)},{f(y + 14 * s)} {f(x - 4.5 * s)},{f(y + 14 * s)} '
         f'{f(x - 4.5 * s)},{f(y + 9 * s)}')
    out.append(f'<path d="{d}" fill="none" stroke="#e9eef8" stroke-width="{f(1.9 * s)}" stroke-linecap="round"/>')
    return '\n'.join(out)


def st_logo(x0, y0, x1, y1):
    """ST logo, rotated 90° like the rest of the Nucleo print (box in photo coordinates)."""
    # drawn upright in a 100 × 60 box, then rotated into place
    w, h = y1 - y0, x1 - x0
    d = ('M8,0 H100 L92,14 H40 Q30,14 34,22 L44,38 Q50,50 40,58 L36,60 H0 L8,46 H32 Q40,46 36,38 L26,22 '
         'Q18,8 28,2 Z M58,20 H82 L60,60 H36 Z')
    tr = f'translate({f(x1)} {f(y0)}) rotate(90) scale({f(w / 100)} {f(h / 60)})'
    return f'<path transform="{tr}" d="{d}" fill="{N_BLUE}"/>'


NUCLEO_HOLES = [(400.5, 101.5, 6.6), (401.5, 300.5, 6.6), (509.9, 241.9, 6.8), (735.8, 94.7, 6.3)]
# snap-off slots between the Nucleo and its ST-LINK part (x range, y ranges)
NUCLEO_SLOTS = ((746.6, 750.4), [(44.0, 118.5), (130.4, 216.6), (232.7, 269.0), (281.7, 360.0)])


def nucleo_outline():
    """The Nucleo's PCB with its mounting holes and the snap-off slots cut out."""
    x0, y0, x1, y1, r = 279.8, 50.6, 856.0, 352.0, 3.5
    d = (f'M{f(x0 + r)},{f(y0)}H{f(x1 - r)}A{r},{r} 0 0 1 {f(x1)},{f(y0 + r)}V{f(y1 - r)}'
         f'A{r},{r} 0 0 1 {f(x1 - r)},{f(y1)}H{f(x0 + r)}A{r},{r} 0 0 1 {f(x0)},{f(y1 - r)}V{f(y0 + r)}'
         f'A{r},{r} 0 0 1 {f(x0 + r)},{f(y0)}Z')
    for x, y, rr in NUCLEO_HOLES:
        d += f'M{f(x - rr)},{f(y)}a{f(rr)},{f(rr)} 0 1 0 {f(2 * rr)},0a{f(rr)},{f(rr)} 0 1 0 {f(-2 * rr)},0Z'
    (sx0, sx1), segs = NUCLEO_SLOTS
    w = (sx1 - sx0) / 2
    for a, b in segs:                     # open at the board edge, round inside
        top = f'M{f(sx0)},{f(y0)}H{f(sx1)}' if a <= y0 else f'M{f(sx0)},{f(a + w)}a{f(w)},{f(w)} 0 0 1 {f(2 * w)},0'
        bottom = f'V{f(y1)}H{f(sx0)}' if b >= y1 else f'V{f(b - w)}a{f(w)},{f(w)} 0 0 1 {f(-2 * w)},0'
        d += top + bottom + 'Z'
    return d


# ZIO sockets (2 rows, 2.54 mm pitch) as footprints: first column, rows, and
# the columns of each socket in pitches from that first column (the Arduino
# connectors are not on one grid)
ZIO_X0 = 418.3
ZIO_ROWS = [(94.75, 105.76), (292.25, 303.26)]
ZIO = [(0, 0, 15, 'CN9'), (0, 16, 8, 'CN8'), (1, 0, 17, 'CN10'), (1, 17.61, 10, 'CN7')]


def zio_sockets():
    """The four ZIO female sockets (8.5 mm high) seen from straight above."""
    out = []
    p = 11.01
    for row, c0, n, _ in ZIO:
        ya, yb = ZIO_ROWS[row]
        xa = ZIO_X0 + c0 * p
        out.append(rect(xa - p / 2 - 0.4, ya - p / 2 - 0.4, xa + (n - 0.5) * p + 0.4, yb + p / 2 + 0.4, '#262628', 1.0))
        frames = ''.join(f'M{f(xa + i * p - 3.7)},{f(y - 3.7)}h7.4v7.4h-7.4Z' for i in range(n) for y in (ya, yb))
        holes = ''.join(f'M{f(xa + i * p - 2.1)},{f(y - 2.1)}h4.2v4.2h-4.2Z' for i in range(n) for y in (ya, yb))
        out.append(path(frames, '#151516'))
        out.append(path(holes, '#030303'))
    return '\n'.join(out)


def lqfp(cx, cy, body, n, pitch, lead, pin_w=1.0):
    """LQFP: square body and `n` gull-wing leads per side."""
    h = body / 2
    d = ''
    for i in range(n):
        o = (i - (n - 1) / 2) * pitch - pin_w / 2
        d += f'M{f(cx + o)},{f(cy - h - lead)}h{f(pin_w)}v{f(lead)}h{f(-pin_w)}Z'
        d += f'M{f(cx + o)},{f(cy + h)}h{f(pin_w)}v{f(lead)}h{f(-pin_w)}Z'
        d += f'M{f(cx - h - lead)},{f(cy + o)}v{f(pin_w)}h{f(lead)}v{f(-pin_w)}Z'
        d += f'M{f(cx + h)},{f(cy + o)}v{f(pin_w)}h{f(lead)}v{f(-pin_w)}Z'
    return '\n'.join([path(d, '#c7c1b3'), rect(cx - h, cy - h, cx + h, cy + h, '#2b2b2b', 1.2)])


def sot(x, y, w, h, top, bottom, lead=2.6, color='#232325', vertical=False):
    """Small outline transistor/regulator: body w × h centred at (x, y), `top`
    and `bottom` = lists of lead offsets along the long side (leads across it)."""
    out = []
    for offs, sgn in ((top, -1), (bottom, 1)):
        for o in offs:
            if vertical:
                x0 = x + sgn * h / 2 if sgn > 0 else x - h / 2 - lead
                out.append(rect(x0, y + o - 0.8, x0 + lead, y + o + 0.8, '#c7c1b3', 0.3))
            else:
                y0 = y + h / 2 if sgn > 0 else y - h / 2 - lead
                out.append(rect(x + o - 0.8, y0, x + o + 0.8, y0 + lead, '#c7c1b3', 0.3))
    out.append(rect(x - (h if vertical else w) / 2, y - (w if vertical else h) / 2, x + (h if vertical else w) / 2,
                    y + (w if vertical else h) / 2, color, 0.5))
    return '\n'.join(out)


def jumper_block(x0, y0, x1, y1):
    """Black jumper cap / plastic header body seen from above."""
    return '\n'.join([rect(x0, y0, x1, y1, '#1d1d1e', 1.2), rect(x0 + 1.5, y0 + 1.5, x1 - 1.5, y1 - 1.5, '#2b2b2c', 0.8)])


def nucleo_hidden():
    """Parts of the Nucleo under the display (measured on the bare Nucleo, 031702)."""
    out = []
    R, C, LED = '#2b2a28', '#c9b48e', '#efe9d6'
    # STM32F429ZIT6 (LQFP-144) and its corner pads
    out.append(lqfp(588.5, 202.3, 87.0, 36, 2.255, 6.6, 1.05))
    out.append(circle(553.0, 167.0, 1.8, '#121212'))
    for x, y in [(555.0, 235.0), (621.5, 167.7)]:
        out.append(circle(x, y, 4.2, '#262626'))
    for x, y in [(540.0, 152.0), (637.0, 152.0), (540.0, 253.0), (637.0, 253.0)]:
        a = 45 if (x < 588) == (y < 202) else -45
        out.append(f'<rect x="{f(x - 2.0)}" y="{f(y - 5.0)}" width="4" height="10" rx="0.6" fill="{TIN}" '
                   f'transform="rotate({a} {f(x)} {f(y)})"/>')
    out.append(nt(574.0, 202.0, 'ARM', 9.0, 90, '#3a3a3a', N_BOLD))
    out.append(nt(596.0, 202.0, 'STM32F429ZIT6', 4.0, 90, '#3f3f3f'))
    out.append(nt(603.0, 202.0, 'GH24P 93', 3.6, 90, '#3f3f3f'))
    # small parts: (x, y, body colour, vertical)
    parts = [(549.2, 141.1, C, 1), (563.2, 141.1, R, 1), (577.7, 141.1, C, 1), (599.9, 141.1, C, 1),
             (553.0, 261.5, C, 1), (582.2, 261.5, C, 1), (603.7, 261.5, C, 1), (625.9, 261.5, C, 1),
             (648.7, 160.1, C, 0), (649.3, 179.1, R, 0), (648.7, 195.0, C, 0), (647.9, 215.9, C, 0),
             (502.3, 174.7, C, 0), (502.3, 185.5, C, 0), (525.1, 176.6, R, 0), (525.1, 183.6, R, 0),
             (528.3, 229.8, C, 1), (482.0, 129.0, TIN, 1), (487.7, 129.0, TIN, 1), (487.7, 273.6, TIN, 1),
             (498.5, 273.6, TIN, 1), (496.6, 208.3, TIN, 0), (496.6, 216.5, TIN, 0)]
    for y in (209.6, 215.9, 222.9, 229.9, 236.2, 250.9, 257.9, 264.2, 274.7):
        parts.append((671.7, y, R if y in (264.2, 274.7) else (C if y > 240 else TIN), 0))
    for y in (215.9, 222.2, 228.5, 235.5):
        parts.append((688.2, y, TIN if y < 223 else C, 0))
    parts += [(720.4, 179.5, R, 0), (722.5, 192.8, R, 0), (722.5, 207.5, R, 0), (735.8, 278.2, R, 0),
              # ST-LINK part
              (763.8, 102.4, R, 1), (775.0, 102.4, R, 1), (786.2, 102.4, R, 1), (808.7, 101.7, R, 1),
              (818.5, 101.7, R, 1), (789.7, 152.8, C, 1), (798.1, 152.8, C, 1), (775.7, 169.0, R, 0),
              (775.7, 176.0, R, 0), (761.0, 192.0, C, 1), (761.7, 208.2, C, 1), (761.7, 220.0, R, 1),
              (781.3, 232.7, C, 1), (787.6, 232.7, R, 1), (803.0, 232.7, C, 1), (817.0, 246.0, R, 0),
              (817.0, 253.0, R, 0), (759.6, 253.0, C, 1), (766.6, 266.3, C, 0), (766.6, 272.6, C, 0),
              (766.6, 279.0, R, 0), (783.4, 283.0, C, 0), (783.4, 290.0, C, 0), (780.6, 307.6, TIN, 1)]
    for y in (169.6, 176.6, 183.0, 189.3, 195.5, 202.0, 208.5, 215.0):
        parts.append((819.9, y, R if y not in (183.0, 195.5) else C, 0))
    for y in (278.2, 285.2, 292.0, 307.0, 313.5, 320.0):
        parts.append((808.0, y, R, 0))
    for x, y, c, v in parts:
        out.append(smd_r(x, y, 4.4, 9.2, c, vertical=True) if v else smd_r(x, y, 9.2, 4.4, c))
    # crystals X2 (32 kHz) and X3 (8 MHz, pads only on this board), X1
    out.append(rect(509.5, 172.5, 518.0, 188.5, '#cdbf9c', 1.0, ' stroke="#9d9070" stroke-width="0.5"'))
    out += [rect(506.0, 205.8, 517.5, 211.5, TIN, 0.6), rect(506.0, 214.0, 517.5, 219.7, TIN, 0.6)]
    out.append(rect(786.2, 162.6, 801.7, 174.5, '#2a2a2b', 1.0))
    # U5 (LD1117S50, SOT-223) with C17 (tantalum), U2 (STM32F103CBT6, LQFP-48), U4 (ST890, SO-8)
    for x in (700.8, 710.6, 720.4):
        out.append(rect(x - 1.8, 134.6, x + 1.8, 145.5, TIN, 0.4))
    out.append(rect(705.0, 157.0, 718.5, 171.0, TIN, 0.6))
    out.append(rect(698.0, 145.0, 726.7, 158.4, '#262627', 0.6))
    out.append(nt(712.3, 151.7, 'LD50 F022', 3.0, 180, '#6d6d6a'))
    out.append(tantalum(705.3, 115.0, 13.3, 15.4).replace('</', '</'))
    out.append(rect_stroke(696.5, 104.5, 714.5, 125.0, N_SILK, 0.45))
    out.append(lqfp(793.0, 199.0, 30.5, 12, 2.255, 4.2, 0.9))
    out.append(nt(793.0, 199.0, 'STM32F103CBT6', 2.6, 180, '#5a5a58'))
    out.append(soic(756.5, 301.3, 777.0, 318.8, 4, True, pitch=5.5, lead=4.6))
    out.append(nt(766.7, 310.0, 'ST890C 6K022', 2.6, 180, '#6d6d6a'))
    out.append(rect(756.8, 284.5, 775.0, 293.0, '#e0a83a', 0.8))
    out.append(rect(756.8, 284.5, 759.8, 293.0, '#c88d27', 0.5))
    # small transistors, diodes, regulators
    out.append(sot(822.7, 222.9, 7.0, 3.0, [], [], color='#1e1e1f'))                       # D2
    for x in (770.0, 779.2, 788.3):                                                        # D5, D4, D3
        out.append(sot(x, 253.0, 7.0, 4.6, [0], [0], 1.8, vertical=True))
    out.append(rect_stroke(764.0, 245.0, 794.0, 261.0, N_SILK, 0.45))
    out.append(sot(818.5, 265.0, 7.0, 5.0, [-2.2, 0, 2.2], [-2.2, 2.2], 2.0))              # U1
    out.append(sot(820.6, 286.6, 7.0, 5.0, [0], [-2.2, 2.2], 2.0))                          # T1
    out.append(sot(783.4, 270.5, 10.0, 7.0, [-3.0, 0, 3.0], [-3.0, 3.0], 2.4))              # U3
    out.append(sot(833.2, 101.0, 4.4, 3.2, [0], [0], 1.5, color='#1e1e1f', vertical=True))   # D1
    out.append(sot(688.2, 252.3, 9.0, 7.5, [-3.0, 0, 3.0], [-3.0, 3.0], 2.2))               # U6
    out.append(sot(688.9, 269.5, 7.0, 4.6, [0], [-2.2, 2.2], 2.0))                          # U7
    out.append(rect(706.0, 180.0, 716.0, 201.0, '#2c2c2d', 1.0))                            # D? (SMA)
    # user LEDs LD1..LD3, LD6 (PWR), LD4 (COM)
    for y in (180.2, 194.9, 207.9):
        out.append(rect_stroke(731.0, y - 3.4, 744.2, y + 3.4, '#3a64c4', 0.7, 0.6))
        out.append(rect(732.6, y - 2.2, 742.6, y + 2.2, LED, 0.5))
    out.append(rect_stroke(731.5, 251.0, 742.5, 257.8, '#3a64c4', 0.7, 0.6))
    out.append(rect(733.0, 252.2, 741.0, 256.6, LED, 0.5))
    out.append(rect(824.0, 304.0, 843.7, 314.7, TIN, 0.6))
    out.append(rect(829.0, 303.5, 843.0, 315.2, '#f2ecdc', 0.8))
    # jumpers and headers: USB_EN, IDD, JP3 (power source), CN4, CN5, CN6 (SWD), JP1
    for box in [(684.0, 154.0, 703.0, 179.0), (682.0, 186.0, 704.0, 212.0), (700.0, 246.0, 733.0, 263.0),
                (805.0, 119.5, 845.0, 134.0), (763.0, 81.4, 828.0, 89.1), (760.5, 125.5, 776.0, 133.8),
                (836.7, 190.0, 855.0, 209.6)]:
        out.append(jumper_block(*box))
    pins = ''
    for i in range(6):                                                                     # SWD pins, bent
        x = 768.2 + i * 11.01
        pins += f'M{f(x - 1.0)},{f(88.0)}L{f(x + 6.0)},{f(80.5)}L{f(x + 7.6)},{f(81.8)}L{f(x + 0.8)},{f(89.0)}Z'
    for i in range(2):                                                                     # CN5 RX/TX
        x = 765.0 + i * 11.01
        pins += f'M{f(x - 1.0)},{f(132.0)}L{f(x + 14.0)},{f(126.0)}L{f(x + 14.6)},{f(127.6)}L{f(x)},{f(133.4)}Z'
    for y in (195.0, 205.0):                                                               # JP1
        pins += f'M855,{f(y - 0.8)}h7v1.6h-7Z'
    for x in (689.0, 698.0, 707.0, 716.0, 725.0):                                          # JP3 pins
        pins += f'M{f(x)},{f(264.0)}l6,14l-1.5,0.6l-6,-14Z'
    out.append(path(pins, '#d9b25c'))
    # CN1 (ST-LINK USB, micro B)
    out.append(rect(835.3, 246.0, 860.0, 279.6, '#bdbab2', 1.2, ' stroke="#8f8c84" stroke-width="0.5"'))
    out.append(rect(838.0, 249.5, 857.0, 276.0, '#cfccc4', 0.8))
    for i in range(5):
        out.append(rect(830.0, 256.0 + i * 3.0, 836.0, 257.4 + i * 3.0, TIN, 0.2))
    # silkscreen: frames of the parts and the labels
    out += [rect_stroke(759.6, 79.0, 838.0, 91.0, N_SILK, 0.45), rect_stroke(804.0, 117.0, 847.0, 136.0, N_SILK, 0.45),
            rect(806.0, 136.6, 820.6, 138.0, N_BLUE), rect(827.5, 136.6, 842.3, 138.0, N_BLUE),
            rect_stroke(822.0, 301.5, 845.5, 317.0, N_SILK, 0.45), rect_stroke(753.0, 282.5, 777.5, 295.0, N_SILK, 0.45)]
    labels = [(555.5, 142.0, 'C33'), (569.5, 142.0, 'R36'), (584.0, 142.0, 'C30'), (606.0, 142.0, 'C28'),
              (624.5, 142.4, 'U8'), (559.5, 262.0, 'C32'), (588.5, 262.0, 'C31'), (610.0, 262.0, 'C29'),
              (632.0, 262.0, 'C27'), (660.0, 160.0, 'C24'), (660.0, 179.0, 'R35'), (660.0, 195.0, 'C25'),
              (659.0, 216.0, 'C26'), (514.0, 165.0, 'X2'), (508.5, 172.0, 'C35'), (508.5, 190.0, 'C36'),
              (531.0, 172.0, 'R37'), (531.0, 190.0, 'R38'), (512.0, 225.0, 'X3'), (534.5, 230.0, 'C34'),
              (715.0, 172.0, 'U5'), (720.0, 115.0, 'C17'), (747.5, 161.0, 'User LEDs'), (751.0, 180.2, 'LD3'),
              (751.0, 194.9, 'LD2'), (751.0, 207.9, 'LD1'), (750.0, 254.4, 'LD6'), (737.0, 240.0, 'JP3'),
              (770.0, 113.0, 'R23'), (781.0, 113.0, 'R22'), (792.0, 113.0, 'R19'), (815.0, 113.6, 'ST-LINK'),
              (821.0, 147.9, 'NUCLEO'), (793.0, 75.1, 'SWD'), (754.7, 85.6, 'CN6'), (787.0, 128.0, 'CN5'),
              (808.0, 156.0, 'X1'), (764.0, 160.0, 'C6'), (793.0, 230.0, 'U2'), (812.0, 263.0, 'U1'),
              (829.0, 286.0, 'T1'), (789.0, 316.0, 'U4'), (850.0, 309.0, 'LD4'), (848.0, 262.0, 'CN1'),
              (800.0, 253.0, 'D3'), (727.0, 223.0, 'SB2')]
    for x, y, t in labels:
        out.append(nt(x, y, t, 3.3, 180 if t in ('ST-LINK', 'NUCLEO', 'SWD') else 90))
    for x0, x1, y, t in [(477.6, 510.6, 116.0, 'SAI_A'), (532.7, 577.0, 116.0, 'USART'), (621.5, 660.0, 116.0, 'SDMMC'),
                         (489.0, 532.0, 284.0, 'QSPI'), (537.0, 566.0, 284.0, 'ADC_IN'), (616.0, 640.0, 284.0, 'SPI_B'),
                         (646.0, 668.0, 284.0, 'I2S_B'), (674.0, 700.0, 284.0, 'I2S_A')]:
        out.append(nt((x0 + x1) / 2, y, t, 3.6, 180))
        top = y < 200
        yy = y + (3.4 if top else -3.4)
        out.append(polyline([(x0, yy + (-2 if top else 2)), (x0, yy), (x1, yy), (x1, yy + (-2 if top else 2))],
                            N_SILK, 0.45))
    for x, t in [(521.3, 'GND'), (603.0, 'IO')]:
        out.append(nt(x, 120.0, t, 3.6, 90))
    for x, t in [(577.0, 'GND'), (588.5, 'AGND'), (600.0, 'AVDD')]:
        out.append(nt(x, 280.0, t, 3.4, 90))
    return '\n'.join(out)


def nucleo():
    out = []
    # PCB with the slot of the snap-off ST-LINK part at the bottom
    out.append(path(nucleo_outline(), NUCLEO, ' fill-rule="evenodd"'))
    out.append(rect(352.5, 88.6, 374.6, 167.3, '#fbfbf9'))          # white label field
    # Morpho pins and GND test points
    for x in MORPHO_X:
        for y in MORPHO_ROWS:
            out.append(morpho_pin(x, y))
    for y0 in (59.4, 320.6):
        out.append(rect_stroke(328.2, y0, 338.6, y0 + 21.0, N_SILK, 0.5))
        for dy in (6.6, 15.8):
            out.append(circle(333.4, y0 + dy, 3.4, '#cfcbc2'))
            out.append(circle(333.4, y0 + dy, 1.7, '#e8e5de'))
    out.append(nt(323.6, 71.5, 'GND', 3.6))
    out.append(nt(323.6, 330.2, 'GND', 3.6))
    # ZIO sockets CN7 / CN10 (CN8 / CN9 are under the display)
    out.append(zio_sockets())
    out += [nt(419.6, 118.9, 'IO', 3.6, 180), nt(448.6, 119.6, 'SAI_B', 3.6, 180),
            polyline([(441.5, 115.8), (441.5, 121.5), (460.0, 121.5), (460.0, 115.8)], N_SILK, 0.45),
            nt(431.4, 281.0, 'TIMER', 3.6, 180), nt(455.2, 281.0, 'GND', 3.4),
            polyline([(419.5, 285.0), (419.5, 283.2), (443.0, 283.2), (443.0, 285.0)], N_SILK, 0.45),
            polyline([(419.5, 313.5), (419.5, 318.6), (452.0, 318.6)], N_SILK, 0.45),
            nt(462.0, 318.9, 'TIMER', 3.6, 180), nt(416.0, 278.6, 'CN10', 3.2)]
    # pads of B1 (USER) and B2 (RESET); the switches are drawn by nucleo_caps()
    for y0, y1 in [(60.3, 66.8), (96.9, 107.5), (296.9, 303.8), (331.5, 341.3)]:
        for x0, x1 in [(292.4, 299.7), (311.9, 319.2)]:
            out.append(rect(x0, y0, x1, y1, '#d8d3c6', 0.5))
            out.append(rect_stroke(x0 - 0.6, y0 - 0.6, x1 + 0.6, y1 + 0.6, N_SILK, 0.35))
    out += [nt(283.0, 64.0, 'B1', 3.0), nt(283.0, 86.5, 'USER', 3.0), nt(283.0, 110.5, 'CN13', 3.0),
            nt(284.9, 287.0, 'CN14', 3.0), nt(311.9, 282.9, 'Ethernet', 3.2, 180), nt(284.9, 312.5, 'RESET', 3.0),
            nt(284.9, 327.6, 'B2', 3.0), nt(284.2, 171.7, 'User', 3.0), nt(284.2, 193.6, 'USB', 3.0),
            nt(288.3, 175.4, 'C54', 2.8)]
    # CN13 micro USB (opening to the left, over the edge)
    for i in range(6):
        out.append(rect(294.4, 128.0 + i * 4.6, 298.6, 129.6 + i * 4.6, '#cfcabd', 0.3))
    out.append(on_nucleo(2.9, '\n'.join([rect(269.5, 120.1, 294.4, 158.7, '#d9dad8', 2.0),
                                          rect_stroke(271.5, 123.0, 292.4, 155.8, '#a9aaa8', 0.5, 1.5),
                                          nt(281.0, 139.4, '2021', 2.6, 90, '#a8a294')])))
    # CN14 RJ45 (opening to the left): top of the shield
    n0, n1, depth = 230.0, 247.7, 18.0
    shell = (f'M257,203.2 H321 Q322.5,203.2 322.5,204.7 V273 Q322.5,274.5 321,274.5 H257 Q255.5,274.5 255.5,273 '
             f'V{n1} H{255.5 + depth} V{n0} H255.5 V204.7 Q255.5,203.2 257,203.2 Z')
    # the cut-out in the middle of the cable side shows what is below; at its
    # end the latch window of the jack
    out.append(on_nucleo(13.5, '\n'.join([path(shell, '#c9c3b3', ' stroke="#a39c8a" stroke-width="0.5"'),
                                           rect(255.5 + depth - 0.2, n0 + 0.6, 255.5 + depth + 2.4, n1 - 0.6,
                                                '#3d3b36'),
                                           line(258.0, 207.5, 320.0, 207.5, '#b3ac9a', 0.6),
                                           line(258.0, 270.2, 320.0, 270.2, '#b3ac9a', 0.6)])))
    # small parts
    for x, y in [(328.2, 85.8), (328.2, 92.0), (328.2, 105.2), (327.7, 131.5), (309.4, 165.8), (309.4, 172.5),
                 (328.2, 183.5), (309.4, 195.7), (309.4, 199.8), (328.2, 193.3), (328.2, 199.6), (360.5, 181.9),
                 (360.5, 189.2), (360.5, 195.7), (328.5, 283.0), (360.5, 262.0)]:
        out.append(smd_r(x, y, 9.6, 3.8, '#2b2a28'))
    for x, y in [(328.2, 98.7), (309.4, 123.3), (327.7, 142.9), (327.7, 150.0), (394.4, 189.2)]:
        out.append(smd_r(x, y, 9.6, 3.8, '#e6e3db'))
    out.append(smd_r(328.5, 298.2, 9.6, 3.8, '#b89b6a'))
    for x0, y0, x1, y1 in [(320.8, 120.9, 335.9, 127.8), (322.0, 173.3, 337.5, 179.8)]:     # LD7, LD8
        out.append(rect_stroke(x0, y0, x1, y1, '#3a64c4', 0.9, 0.8))
        out.append(rect(x0 + 2.0, y0 + 1.2, x1 - 2.0, y1 - 1.2, '#f2efe4', 0.5))
        out.append(rect((x0 + x1) / 2 - 1.2, y0 + 2.0, (x0 + x1) / 2 + 1.2, y1 - 2.0, '#e7c97a', 0.3))
    out.append(chip(304.5, 136.5, 315.5, 145.0, pins_tb=3, pitch=3.8, plen=2.4))           # SOT-23-6 near D6
    out.append(chip(323.0, 160.5, 330.0, 170.0, pins_tb=2, pitch=4.0, plen=1.5))           # T2
    out.append(chip(308.0, 178.5, 315.5, 187.0, pins_tb=3, pitch=2.8, plen=2.0))           # U12
    out.append(rect(292.0, 162.8, 301.7, 181.5, '#e9a43d', 1.0))                            # C54 tantalum
    out.append(rect(292.0, 162.8, 301.7, 165.5, '#c98a2c', 1.0))
    out.append(polyline([(291.0, 161.5), (302.8, 161.5), (302.8, 163.5)], N_SILK, 0.45))
    out.append(polyline([(291.0, 182.8), (302.8, 182.8), (302.8, 180.8)], N_SILK, 0.45))
    out.append(rect(372.0, 180.2, 380.2, 196.9, '#d9b98a', 1.2))                             # R55 (1206)
    out.append(rect(373.0, 183.0, 379.2, 194.0, '#e8cf9f', 0.8))
    # chips around the crystal
    out.append(chip(356.6, 199.0, 392.0, 255.0, pins_lr=8, pitch=6.1, plen=4.0, pw=2.0, color='#2a2927'))  # U11
    out.append(nt(366.0, 227.0, 'DGKYD 2011', 3.6, 90, '#6b6a66'))
    out.append(nt(373.5, 227.0, 'KMS-1102NL', 3.6, 90, '#6b6a66'))
    out.append(circle(378.0, 249.0, 1.8, '#3a3936'))
    out.append(chip(414.4, 195.7, 427.8, 207.1, pins_tb=3, pitch=3.8, plen=2.2))           # U10
    out.append(chip(437.2, 192.0, 455.9, 211.1))                                            # U9 (QFN)
    for i in range(6):
        out.append(rect(438.5 + i * 3.0, 211.1, 440.0 + i * 3.0, 212.6, '#bfb9aa'))
    for x, y, c in [(415.4, 218.5, '#2b2a28'), (424.4, 218.5, '#2b2a28'), (431.4, 218.5, '#2b2a28'),
                    (437.4, 218.5, '#c2a26e'), (452.8, 218.5, '#e6e3db'), (459.3, 218.5, '#c2a26e'),
                    (416.8, 170.9, '#2b2a28'), (425.4, 170.9, '#c2a26e'), (447.0, 180.6, '#2b2a28'),
                    (453.6, 180.6, '#2b2a28'), (460.0, 180.6, '#2b2a28'), (432.0, 183.5, '#c2a26e'),
                    (416.8, 183.5, '#2b2a28'), (425.4, 183.5, '#c2a26e'), (440.0, 258.0, '#c2a26e'),
                    (456.0, 258.0, '#c2a26e')]:
        out.append(smd_r(x, y, 4.0, 9.4, c, vertical=True))
    for y in (177.4, 184.7, 191.2, 196.1, 202.6, 209.1, 215.6, 222.1):
        out.append(smd_r(471.3, y, 10.0, 4.4, '#2b2a28'))
    out.append(rect(428.6, 153.4, 438.4, 172.5, '#1f1f20', 1.0))                              # C46
    out.append(polyline([(427.5, 155.5), (427.5, 152.2), (439.5, 152.2), (439.5, 155.5)], N_SILK, 0.45))
    out.append(rect(435.9, 238.0, 458.7, 250.2, '#ddd5c2', 2.5))                              # X4 crystal
    out.append(rect_stroke(437.2, 239.3, 457.4, 248.9, '#b8ab8c', 0.5, 1.8))
    out.append(nt(447.3, 244.1, 'JFVNY 25.000', 2.2, 0, '#b4aa92'))
    for x0, x1 in [(412.0, 430.3), (461.9, 476.8)]:                                            # jumpers
        jp = [rect(x0, 233.9, x1, 258.3, '#1e1e1f', 1.5), rect(x0 + 2.2, 236.4, x1 - 2.2, 255.8, '#2e2d2b', 1.0)]
        for y in (240.5, 251.5):
            jp.append(rect((x0 + x1) / 2 - 1.8, y - 1.8, (x0 + x1) / 2 + 1.8, y + 1.8, '#d9a23a', 0.4))
        out.append(on_nucleo(8.0, '\n'.join(jp)))
    out.append(rect_stroke(410.5, 232.5, 422.0, 261.0, '#3a64c4', 0.6))
    # labels
    for x, y, s in [(341.8, 87.5, 'R58'), (341.8, 94.0, 'R59'), (341.8, 100.5, 'C52'), (341.8, 107.0, 'R60'),
                    (309.4, 117.6, 'R70'), (314.3, 130.6, 'D6'), (341.2, 131.0, 'R61'), (341.5, 140.0, 'R62'),
                    (341.5, 149.9, 'R63'), (310.2, 154.2, 'R68'), (310.2, 160.6, 'R69'), (341.0, 187.0, 'R64'),
                    (341.7, 194.2, 'R65'), (341.7, 200.0, 'R66'), (298.0, 193.8, 'R71'), (298.0, 200.0, 'R74'),
                    (358.1, 268.2, 'R57'), (361.4, 175.7, 'R54'), (392.4, 183.0, 'SB16'), (409.9, 261.5, 'C50'),
                    (427.0, 264.0, 'PB13'), (408.3, 268.5, 'SB17')]:
        out.append(nt(x, y, s, 3.3, 180))
    for x, y, s in [(318.2, 196.5, 'U12'), (338.9, 282.5, 'R67'), (338.9, 301.3, 'C53'), (378.0, 264.5, 'U11'),
                    (349.9, 129.8, 'USB_FAULT'), (341.5, 121.1, 'LD7'), (340.0, 163.5, 'T2'), (341.8, 180.3, 'LD8'),
                    (350.0, 178.9, 'VBUS'), (377.6, 170.9, 'C51'), (368.7, 189.0, 'R55'), (432.6, 145.7, 'C46'),
                    (425.4, 158.3, 'C48'), (416.8, 160.7, 'L2'), (447.3, 168.5, 'R48'), (453.9, 168.5, 'R47'),
                    (460.4, 168.5, 'R46'), (436.7, 189.6, 'C47'), (410.7, 190.5, 'R52'), (410.7, 198.0, 'U10'),
                    (404.5, 190.5, 'R53'), (404.5, 198.0, 'C49'), (432.4, 201.7, 'U9'), (415.2, 232.0, 'R51'),
                    (424.4, 232.0, 'R50'), (431.0, 232.0, 'R49'), (437.6, 232.0, 'C44'), (445.5, 232.0, 'X4'),
                    (452.8, 232.0, 'C42'), (459.3, 232.0, 'C41'), (445.7, 257.9, 'C45'), (462.1, 259.1, 'C43'),
                    (476.8, 262.8, 'PA7'), (464.0, 275.7, 'SB15')]:
        out.append(nt(x, y, s, 3.3))
    out.append(nt(346.6, 265.2, 'www.st.com/stm32nucleo', 4.6, 90, '#2f5fc0'))
    out.append(nt(357.8, 295.9, 'MB1137 Rev B', 3.9, 90, '#3a4f95'))
    out.append(nt(369.2, 129.3, 'NUCLEO', 6.6, 90, '#1b1b1b', N_BOLD))
    out.append(nt(359.0, 129.2, '-F429ZI', 6.6, 90, '#1b1b1b', N_BOLD))
    for y in (89.3, 166.8):
        out.append(line(353.7, y, 374.5, y, N_SILK, 0.5))
    out.append(nt(381.5, 137.3, 'Nucleo', 5.8, 90, N_BLUE, N_BOLD))
    out.append(nucleo_logo(402.0, 137.0, 0.75))
    out.append(st_logo(364.3, 270.9, 391.1, 320.1))
    out.append(nucleo_hidden())
    # ST-LINK corner (bottom right, beside the display)
    out += [nt(726.0, 339.0, '12', 3.3), nt(756.0, 346.0, 'R28', 3.0), nt(763.0, 346.0, 'C11', 3.0),
            nt(774.0, 346.0, 'LD5', 3.0), nt(785.5, 345.0, 'R21', 3.0), nt(817.5, 339.0, 'CN3', 3.3)]
    for x, c in ((756.3, '#2b2a28'), (762.8, '#c9b48e'), (780.8, '#2b2a28')):
        out.append(smd_r(x, 335.5, 4.4, 10.0, c, vertical=True))
    # LD5 (white 0603 LED in a thin frame)
    out.append(rect_stroke(770.5, 326.5, 777.0, 341.0, '#3a64c4', 0.45))
    out.append(rect(771.6, 329.0, 775.9, 339.2, TIN, 0.4))
    out.append(rect(771.9, 330.6, 775.6, 337.6, '#f1ecdf', 0.4))
    out.append(rect(773.0, 333.2, 774.5, 335.0, '#d8b45a', 0.2))
    # ESD caution symbol (triangle with a struck-through hand), mostly under the display
    esd = [[(785.7, 322.6), (787.5, 321.6), (798.5, 327.6), (798.3, 329.0), (794.0, 330.0), (790.5, 329.3),
            (789.5, 330.6), (794.0, 332.6), (795.2, 334.6), (794.0, 335.2), (789.0, 333.6), (788.0, 334.6),
            (792.3, 338.0), (792.5, 340.2), (786.5, 349.8), (785.7, 349.0)],
           [(793.3, 340.6), (794.2, 340.2), (798.6, 345.0), (798.3, 346.2), (788.6, 351.5), (787.6, 350.9)],
           [(800.6, 328.6), (801.6, 328.6), (812.3, 336.0), (812.0, 337.6), (803.6, 342.3), (802.6, 341.8),
            (800.4, 330.0)]]
    for pts in esd:
        out.append(poly(pts, N_BLUE, f' stroke="{N_BLUE}" stroke-width="0.6" stroke-linejoin="round"'))
    # CN3 (2 pins, bent over) with its frame and pin 1 mark
    out.append(rect_stroke(823.0, 334.0, 845.5, 344.0, N_SILK, 0.45))
    for x in (828.5, 839.0):
        out.append(circle(x, 339.6, 3.0, TIN))
        out.append(circle(x, 339.6, 1.5, '#9f9a8e'))
    out.append(path('M829.2,341 L835,345.5 L833.8,346.6 L828,342.2 Z M839.7,341 L846,345.5 L844.8,346.6 L838.8,342.2 Z',
                    '#b9b4a8'))
    return '\n'.join(out)


def nucleo_caps():
    """B1 (USER, blue cap) and B2 (RESET, black cap) seen from above, at the positions in board.rs."""
    out = []
    for (x, y), rim, cap in [(USER, '#155c9c', '#2a8bd8'), (RESET, '#060607', '#1f1f21')]:
        out.append(rect(x - 12.0, y - 12.0, x + 12.0, y + 12.0, '#2b2c2f', 1.2))     # body
        out.append(rect(x - 10.2, y - 10.2, x + 10.2, y + 10.2, '#c3c5c8', 0.8))     # metal top
        out.append(circle(x, y, 7.6, rim))
        out.append(circle(x, y, 6.6, cap))
    return '\n'.join(out)


def nucleo_top_right():
    """Parts of the Nucleo between its top edge and the display (top view coordinates)."""
    out = [nt(752.7, 69.3, 'JP1', 3.0, 90),
           rect_stroke(757.4, 68.8, 777.4, 81.7, N_SILK, 0.45)]
    for x in (760.7, 771.4):
        out += [circle(x, 73.6, 3.3, GOLD), circle(x, 73.6, 1.5, '#9a7330')]
    for x in (784.4, 791.4):
        out.append(smd_r(x, 73.6, 3.6, 5.4, '#2b2a28', vertical=True))
    out += [nt(797.4, 70.6, 'R1', 3.0, 180), nt(816.4, 70.6, 'CN2', 3.0, 180),
            rect_stroke(819.9, 69.7, 842.3, 80.0, N_SILK, 0.45)]
    for x in (826.0, 836.0):
        out += [circle(x, 74.0, 3.0, '#d6c8a6'), circle(x, 74.0, 1.4, '#b5a684')]
    return '\n'.join(out)


def morpho_pins():
    """The Morpho pins of the Nucleo, bent outwards into the grey sockets."""
    out = []
    for (x0, y0, x1, y1), rows in [(FELT_TOP, MORPHO_ROWS[:2]), (FELT_BOTTOM, MORPHO_ROWS[2:])]:
        top = y1 < 200
        for xp in MORPHO_X:
            x, yi = ortho(xp, rows[1] if top else rows[0], M_NUCLEO)        # inner row
            out.append(line(x, yi, x, y1 if top else y0, '#c99a43', 2.3, cap='butt'))
            out.append(line(x - 0.35, yi, x - 0.35, y1 if top else y0, '#f0cd7a', 0.8, cap='butt'))
    return '\n'.join(out)


def morpho_sockets():
    """The grey sockets on the carrier beside the Nucleo."""
    out = []
    for x0, y0, x1, y1 in (FELT_TOP, FELT_BOTTOM):
        out.append(rect(x0, y0, x1, y1, FELT, 1.5))
        out.append(rect_stroke(x0, y0, x1, y1, '#4e4e48', 0.8, 1.5))
    return '\n'.join(out)


# ======================================================================
# 4" display shield (Waveshare, ILI9486).  GLASS (from board.rs) is the active
# area, which the GUI fills with the emulated display.


def shield():
    """The display module (photo coordinates, drawn inside layer(M_SHIELD))."""
    out = [rect(475.5, 50.6, 908.6, 330.8, '#1c1e20', 1.0, ' stroke="#4a4945" stroke-width="0.4"'),  # glass rim
           rect_stroke(477.4, 52.5, 906.7, 328.9, '#85827a', 1.0, 0.6),    # edge of the touch panel
           rect(478.6, 53.7, 905.5, 327.7, '#15191b', 0.4)]                # border of the LCD
    out.append(rect(479.8, 150.0, 488.6, 327.0, '#101112', 0.8))           # FPC tape
    out.append(nt(484.2, 293.5, 'H1043  5 2', 3.2, 90, '#9a9a96'))
    # the folded FPC of the touch panel along the left edge
    out.append(line(480.6, 53.5, 480.6, 148.0, '#8d897f', 0.8, cap='butt'))
    out.append(line(484.0, 56.0, 484.0, 148.0, '#77746c', 0.6, cap='butt'))
    out.append(rect(472.5, 96.0, 481.0, 145.0, '#d9952e', 1.2, ' opacity="0.8"'))
    return '\n'.join(out)


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), '..', '..', 'apps',
                                                              'its-emu-gui', 'assets')
    def carrier_layer():
        board = carrier()
        board_clip()
        return ['<g clip-path="url(#board)">', board, '</g>']

    layers = {
        'carrier': carrier_layer,
        # the grey sockets sit on the Nucleo's bent Morpho pins
        'nucleo': lambda: [morpho_sockets(), f'<g transform="{layer(M_NUCLEO)}">', nucleo(), '</g>',
                           nucleo_top_right(), nucleo_caps(), morpho_pins()],
        'shield': lambda: [f'<g transform="{layer(M_SHIELD)}">', shield(), '</g>', poly(GLASS, '#08090a')],
    }
    for name, make in layers.items():
        svg.defs = []                     # each layer has its own definitions
        svg.body = make()
        file = os.path.join(out, f'board-{name}.svg')
        with open(file, 'w') as fh:
            fh.write(svg.text())
        print(file, os.path.getsize(file) // 1024, 'KiB')


if __name__ == '__main__':
    main()
