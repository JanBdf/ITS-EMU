#!/usr/bin/env python3
"""Draws the ITS-Board (Rev 02 with NUCLEO-F429ZI and the Waveshare 4" shield)
as a vector image for the GUI.

    boarddraw.py [OUT.svg]        (default: apps/its-emu-gui/assets/board.svg)

Everything is drawn from shapes and fonts; there are no bitmaps in the
output.  Positions are GUI design units (the board is 1000 units wide,
1 unit = 0.2408 mm) and were measured in the rectified, focus-stacked photo
of the real board (boardstack.py / boardasset.py) and in hardware_img/.

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

W, H = 1000.0, 648.55
CORNER = 11.3

BOARD_RS = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', 'apps', 'its-emu-gui', 'src',
                        'board.rs')


def rs_const(name):
    """Numbers of a constant in board.rs (the GUI draws its overlays there)."""
    m = re.search(rf'const {name}: [^=]*=\s*([^;]*);', open(BOARD_RS).read())
    v = [float(x) for x in re.findall(r'-?\d+(?:\.\d+)?', m.group(1))]
    return v[0] if len(v) == 1 else v


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
# rectified photo (rect.png) shows them scaled about the point below the
# camera, by the magnifications below: from the Morpho pitch (11.01 units in
# the photo for 2.54 mm) and from the active area of the display (83.52 ×
# 55.68 mm).  The second photo (rect_b.png, camera moved by 644, 369 units)
# gives the same layer heights.  Coordinates measured in rect.png are drawn
# inside layer(), which undoes the scaling.
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
MASK = '#30598a'          # solder mask over bare laminate
MASK_CU = '#386395'       # solder mask over copper (traces, planes)
MASK_EDGE = '#264a75'
SILK = '#eef5fa'
GOLD = '#e2b35a'
HOLE = '#16193a'
TIN = '#d9d6cf'
LED_BODY = '#f3ecd9'
HEADER = '#1c1c1e'
NUCLEO = '#f1f1ec'
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
        return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {f(W)} {f(H)}" '
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
    pen = SVGPathPen(gs)
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

def pad(x, y, r=8.4, hole=4.9, square=False, color=GOLD):
    """Plated through hole with annular ring."""
    out = []
    if square:
        out.append(rect(x - r, y - r, x + r, y + r, color))
    else:
        out.append(circle(x, y, r, color))
    out.append(circle(x, y, hole, HOLE))
    out.append(ring(x, y, hole + 0.35, '#b88a3c', 0.7))
    return '\n'.join(out)


def small_pad(x, y, r=3.4, hole=1.5, square=False):
    out = [rect(x - r, y - r, x + r, y + r, GOLD) if square else circle(x, y, r, GOLD)]
    out.append(circle(x, y, hole, HOLE))
    return '\n'.join(out)


def mount_hole(x, y, r=6.2):
    return '\n'.join([circle(x, y, r + 0.7, MASK_EDGE), circle(x, y, r, HOLE)])


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
        circle(x + 7.8, y, 1.0, '#1d2333'),                              # polarity notch
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
AN_PADS = [440.3 + i * 31.52 for i in range(10)]
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



SEC = 2.7             # width of the section frame lines

# section frame lines (polylines)
SECTIONS = [
    [(0, 312.3), (300, 312.3)],
    [(213.6, 312.3), (213.6, 464.4)],
    [(0, 464.4), (429.1, 464.4), (445.7, 480.9), (482.3, 480.9), (497.4, 495.6), (719.2, 495.6),
     (733.4, 481.2), (750.6, 481.2), (762, 469.6), (1001, 469.6)],
    [(634.5, 495.6), (634.5, 649)],
    [(330, 382.7), (344.4, 382.7), (344.4, 464.4)],
    [(413, 464.4), (413, 358.8), (418, 353.8), (728, 353.8), (761, 386.6), (761, 469.6)],
    [(895.1, 300), (895.1, 469.6)],
    [(870.0, -1), (870.0, 50.0), (840.0, 80.0)],
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
    return out


NOTCH = (900.0, 93.5, 190.5)    # cut-out at the right edge (cable of the Nucleo's USB): x, top, bottom


def board_outline():
    r, n, i = CORNER, 3.5, 4.0       # board corners, outer and inner corners of the notch
    x, t, b = NOTCH
    return (f'M{r},0 H{W - r} A{r},{r} 0 0 1 {W},{r} V{t - n} A{n},{n} 0 0 1 {W - n},{t} H{x + i} '
            f'A{i},{i} 0 0 0 {x},{t + i} V{b - i} A{i},{i} 0 0 0 {x + i},{b} H{W - n} '
            f'A{n},{n} 0 0 1 {W},{b + n} V{H - r} A{r},{r} 0 0 1 {W - r},{H} H{r} A{r},{r} 0 0 1 0,{H - r} '
            f'V{r} A{r},{r} 0 0 1 {r},0 Z')


def gradients():
    svg.defs += [
        f'<clipPath id="board"><path d="{board_outline()}"/></clipPath>',
    ]


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


def silk_labels(labels):
    out = []
    for l in fitted(labels):
        out.append(stext(l['x'], l['y'], l['text'], l['h'], l['anchor'], l['rot'], l['sw'], adv=l['adv']))
    return '\n'.join(out)


TRACES = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'boardtraces.json')


def traces():
    """Copper traces under the mask (edges vectorised by boardtraces.py)."""
    import json
    if not os.path.exists(TRACES):
        return ''
    d = ''.join(f'M{f(x0)},{f(y0)}L{f(x1)},{f(y1)}' for x0, y0, x1, y1 in json.load(open(TRACES)))
    return path(d, extra=' stroke="#4372a6" stroke-width="0.6" stroke-linecap="round" opacity="0.8"')


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
    out.append(stext(56.0, 633.3, 'Top', 6.0, color='#2d5583', sw=1.3))

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
    out.append(rect_stroke(31.8, 316.9, 99.2, 379.3, SILK, sw))
    out.append(line(129.4, 332.6, 129.4, 375.4, SILK, sw))
    out.append(line(129.4, 405.8, 129.4, 449.6, SILK, sw))
    out.append(line(319.0, 385.0, 319.0, 449.0, SILK, sw))
    out.append(line(919.6, 306.0, 919.6, 369.7, SILK, sw))
    # outlines along the long headers
    # LED outlines (thin frames around the packages)
    out.append(silk_labels(carrier_labels()))

    # ---- holes and pads
    for x, y in [(15.4, 15.4), (316.5, 16.5), (708.2, 14.0), (985.2, 13.3), (985.2, 75.9), (985.2, 209.2),
                 (16.3, 326.4), (15.1, 633.3), (623.7, 633.2), (985.0, 633.8), (39.9, 338.2), (92.8, 338.2)]:
        out.append(mount_hole(x, y))
    for x in TIM_PADS:
        out.append(pad(x, TIM_Y))
    for x in (33.6, 137.9):
        out.append(pad(x, 37.9, 10.0, 6.3))
    for i, x in enumerate([68.3, 79.9, 91.4, 102.9]):
        out.append(small_pad(x, 31.9))
    for i, x in enumerate([62.5, 73.9, 85.5, 97.2, 108.9]):
        out.append(small_pad(x, 43.6, square=(i == 4)))
    for (x, y), sq in [((59.6, 348.9), 0), ((68.1, 348.9), 0), ((76.5, 348.9), 0), ((54.6, 356.2), 1),
                       ((63.6, 356.2), 0), ((72.0, 356.2), 0)]:
        out.append(small_pad(x, y, 3.0, 1.4, sq))
    for x, sq in [(111.1, 0), (122.0, 1), (143.5, 0), (154.1, 1)]:
        out.append(small_pad(x, 90.3, 3.6, 1.6, sq))
    for x in OUT_PADS:
        out.append(pad(x, ROW_Y))
    for x in IN_PADS:
        out.append(pad(x, ROW_Y))
    for x in AN_PADS:
        out.append(pad(x, 448.6))
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
    return '\n'.join(out)


def d_shape(cx, cy, top, bottom, h, r):
    """D-sub outline: trapezoid (half widths `top` and `bottom`, half height `h`) with rounded corners."""
    pts = [(cx - top, cy - h), (cx + top, cy - h), (cx + bottom, cy + h), (cx - bottom, cy + h)]
    d = ''
    for i in range(4):
        p0, p1, p2 = pts[i - 1], pts[i], pts[(i + 1) % 4]
        a = (p0[0] - p1[0], p0[1] - p1[1])
        b = (p2[0] - p1[0], p2[1] - p1[1])
        la, lb = math.hypot(*a), math.hypot(*b)
        s = (p1[0] + a[0] / la * r, p1[1] + a[1] / la * r)
        e = (p1[0] + b[0] / lb * r, p1[1] + b[1] / lb * r)
        d += f'{"M" if i == 0 else "L"}{f(s[0])},{f(s[1])} Q{f(p1[0])},{f(p1[1])} {f(e[0])},{f(e[1])} '
    return d + 'Z'


def rs232():
    """J1 (female DE-9 with standoffs, from above) and U5 (MAX3232, wide SOIC-16)."""
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
    # J1: flange, D shell, insulator with the contacts, standoffs
    cx, cy = DSUB
    out.append(rect(cx - 63.5, cy - 25.5, cx + 63.5, cy + 25.5, '#d4d0c6', 2.5))
    out.append(rect_stroke(cx - 63.5, cy - 25.5, cx + 63.5, cy + 25.5, '#a19c90', 0.7, 2.5))
    out.append(path(d_shape(cx, cy, 34.5, 30.0, 18.5, 5.0), '#bcb8ad'))
    out.append(path(d_shape(cx, cy, 33.0, 28.5, 17.0, 4.5), '#e6e2d8', ' stroke="#a8a397" stroke-width="0.6"'))
    out.append(path(d_shape(cx, cy, 30.0, 26.0, 14.0, 4.0), '#46463f'))
    for i in range(4):
        out.append(circle(cx - 17.25 + i * 11.5, cy - 6.0, 2.9, '#0c0c0c'))
    for i in range(5):
        out.append(circle(cx - 23.0 + i * 11.5, cy + 6.0, 2.9, '#0c0c0c'))
    for dx in (-51.9, 51.9):                         # screw locks (round knurled tops)
        x = cx + dx
        out.append(circle(x, cy, 8.6, '#cfcabe', ' stroke="#9d978a" stroke-width="0.7"'))
        out.append(ring(x, cy, 6.4, '#b3ad9f', 0.8))
        out.append(circle(x, cy, 2.8, '#4d4a43'))
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


def nucleo():
    out = []
    # PCB with the slot of the snap-off ST-LINK part at the bottom
    out.append(rect(279.8, 50.6, 856.0, 352.0, NUCLEO, 3.5))
    out.append(rect(746.2, 333.0, 748.6, 352.0, '#16181c'))
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
    for x, y in [(400.5, 101.5), (401.5, 300.5)]:          # mounting holes beside them
        out.append(circle(x, y, 6.6, '#1b1b1d'))
    out.append(on_nucleo(8.5, zio(404.6, 83.0, 474.5, 113.9, [410.3, 421.6, 432.9, 444.2, 455.5, 466.8], [87.5, 99.8])))
    out.append(on_nucleo(8.5, zio(405.4, 286.3, 478.6, 311.9, [411.5, 422.9, 434.3, 445.6, 456.9, 468.2],
                                  [291.2, 302.6])))
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
    out.append(on_nucleo(13.5, '\n'.join([rect(255.5, 203.2, 322.5, 274.5, '#d3cdbd', 1.5,
                                                ' stroke="#aaa391" stroke-width="0.5"'),
                                           line(258.0, 207.5, 320.0, 207.5, '#bdb6a4', 0.6),
                                           line(258.0, 270.2, 320.0, 270.2, '#bdb6a4', 0.6)])))
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
    # ST-LINK corner (bottom right, beside the display)
    out += [nt(726.0, 339.0, '12', 3.3), nt(756.0, 342.0, 'R28', 2.8), nt(763.5, 342.0, 'C11', 2.8),
            nt(774.5, 342.5, 'LD5', 2.8), nt(784.5, 341.5, 'R21', 2.8), nt(816.0, 340.0, 'CN3', 3.0)]
    for x in (754.5, 761.5, 781.5):
        out.append(smd_r(x, 336.5, 3.6, 5.6, '#3a3631', vertical=True))
    out.append(rect_stroke(769.0, 333.5, 776.0, 341.0, '#3a64c4', 0.6))
    out.append(rect(770.2, 334.5, 774.8, 340.0, '#efe9d6', 0.4))
    out.append(poly([(795.0, 332.0), (803.0, 332.0), (797.5, 345.0)], N_BLUE))
    out.append(poly([(803.5, 346.0), (811.5, 337.0), (809.0, 350.5)], N_BLUE))
    out.append(path('M822.8,333 H829 A5,5 0 0 1 829,343 H822.8 Z', N_BLUE))
    out.append(rect_stroke(822.0, 333.0, 844.0, 346.0, N_SILK, 0.5))
    for x in (827.6, 839.5):
        out.append(circle(x, 337.5, 2.6, '#d6c8a6'))
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
    """The grey sockets beside the Nucleo and the Morpho pins, bent outwards into them."""
    out = []
    for (x0, y0, x1, y1), rows in [(FELT_TOP, MORPHO_ROWS[:2]), (FELT_BOTTOM, MORPHO_ROWS[2:])]:
        top = y1 < 200
        for xp in MORPHO_X:
            x, yi = ortho(xp, rows[1] if top else rows[0], M_NUCLEO)        # inner row
            out.append(line(x, yi, x, y1 if top else y0, '#c99a43', 2.3, cap='butt'))
            out.append(line(x - 0.35, yi, x - 0.35, y1 if top else y0, '#f0cd7a', 0.8, cap='butt'))
        out.append(rect(x0, y0, x1, y1, FELT, 1.5))
        out.append(rect_stroke(x0, y0, x1, y1, '#4e4e48', 0.8, 1.5))
    return '\n'.join(out)


# ======================================================================
# 4" display shield (Waveshare, ILI9486).  GLASS (from board.rs) is the active
# area, which the GUI fills with the emulated display.


def shield():
    """The display module (photo coordinates, drawn inside layer(M_SHIELD))."""
    out = [rect(475.5, 50.6, 908.6, 330.8, '#ece7d9', 1.5),               # touch panel
           rect(482.4, 56.5, 903.6, 326.0, '#d9d3c5', 0.8),
           rect(496.0, 58.7, 900.6, 322.0, '#c4beaf', 0.8),               # glass edge
           rect(505.5, 60.5, 901.0, 320.0, '#2f3438', 0.5)]
    out.append(rect(479.8, 150.0, 491.6, 327.5, '#141414', 1.0))           # FPC tape
    out.append(nt(485.7, 293.5, 'H1043  5 2', 3.6, 90, '#d9d9d9'))
    out.append(rect(481.5, 52.5, 489.5, 148.0, '#f1ede2', 0.8))            # FPC clamp
    out.append(rect(472.5, 96.0, 482.0, 145.0, '#e9a73e', 1.5, ' opacity="0.85"'))
    return '\n'.join(out)


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), '..', '..', 'apps',
                                                              'its-emu-gui', 'assets', 'board.svg')
    gradients()
    svg.add('<g clip-path="url(#board)">')
    svg.add(carrier())
    svg.add('</g>')
    svg.add(f'<g transform="{layer(M_NUCLEO)}">')
    svg.add(nucleo())
    svg.add('</g>')
    svg.add(nucleo_top_right())
    svg.add(nucleo_caps())
    svg.add(morpho_pins())
    svg.add(f'<g transform="{layer(M_SHIELD)}">')
    svg.add(shield())
    svg.add('</g>')
    svg.add(poly(GLASS, '#08090a'))
    with open(out, 'w') as fh:
        fh.write(svg.text())
    print(out, os.path.getsize(out) // 1024, 'KiB')


if __name__ == '__main__':
    main()
