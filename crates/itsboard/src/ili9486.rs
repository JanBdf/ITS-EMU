//! ILI9486 TFT controller as wired on the Waveshare 4" shield.
//!
//! The shield converts the SPI stream into a 16-bit parallel bus: commands are
//! single bytes (DC = 0), parameters and pixels are 16-bit words sent MSB
//! first (DC = 1). Command parameters use the low byte of a word; pixels are
//! RGB565.
//!
//! The panel is modelled physically: GRAM has 320 columns (sources) and 480
//! rows (gates). MADCTL (0x36) controls how CASET/PASET addresses map onto
//! GRAM, the SS/GS bits of Display Function Control (0xB6) mirror the scan.

pub const GRAM_COLS: usize = 320;
pub const GRAM_ROWS: usize = 480;

const MADCTL_MY: u8 = 1 << 7;
const MADCTL_MX: u8 = 1 << 6;
const MADCTL_MV: u8 = 1 << 5;
const MADCTL_BGR: u8 = 1 << 3;
const DFC_GS: u8 = 1 << 6;
const DFC_SS: u8 = 1 << 5;

pub struct Ili9486 {
    gram: Vec<u16>,
    madctl: u8,
    dfc: [u8; 3],
    col_start: u16,
    col_end: u16,
    page_start: u16,
    page_end: u16,
    cur_col: u16,
    cur_page: u16,
    sleeping: bool,
    display_on: bool,
    inverted: bool,
    /// Current command and the index of its next parameter word.
    cmd: u8,
    param_idx: usize,
    params: [u8; 4],
    /// First byte of a data word that is still incomplete.
    pending_byte: Option<u8>,
    /// Incremented whenever the visible image may have changed.
    generation: u64,
}

impl Default for Ili9486 {
    fn default() -> Self {
        Self::new()
    }
}

impl Ili9486 {
    pub fn new() -> Self {
        let mut d = Self {
            gram: vec![0; GRAM_COLS * GRAM_ROWS],
            madctl: 0,
            dfc: [0x0A, 0x82, 0x3B],
            col_start: 0,
            col_end: 0,
            page_start: 0,
            page_end: 0,
            cur_col: 0,
            cur_page: 0,
            sleeping: true,
            display_on: false,
            inverted: false,
            cmd: 0,
            param_idx: 0,
            params: [0; 4],
            pending_byte: None,
            generation: 0,
        };
        d.reset();
        d
    }

    /// Hardware or software reset: registers return to defaults, GRAM keeps its content.
    pub fn reset(&mut self) {
        self.madctl = 0;
        self.dfc = [0x0A, 0x82, 0x3B];
        self.col_start = 0;
        self.col_end = (GRAM_COLS - 1) as u16;
        self.page_start = 0;
        self.page_end = (GRAM_ROWS - 1) as u16;
        self.cur_col = 0;
        self.cur_page = 0;
        self.sleeping = true;
        self.display_on = false;
        self.inverted = false;
        self.cmd = 0;
        self.param_idx = 0;
        self.pending_byte = None;
        self.generation += 1;
    }

    /// Chip select went high: an incomplete data word is discarded.
    pub fn end_frame(&mut self) {
        self.pending_byte = None;
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Byte received while CS is low. `dc` = level of the data/command pin.
    pub fn write_byte(&mut self, dc: bool, byte: u8) {
        if !dc {
            self.pending_byte = None;
            self.command(byte);
            return;
        }
        match self.pending_byte.take() {
            None => self.pending_byte = Some(byte),
            Some(hi) => self.data(u16::from_be_bytes([hi, byte])),
        }
    }

    fn command(&mut self, cmd: u8) {
        self.cmd = cmd;
        self.param_idx = 0;
        match cmd {
            0x01 => self.reset(), // software reset
            0x10 => self.set_state(|d| d.sleeping = true),
            0x11 => self.set_state(|d| d.sleeping = false),
            0x20 => self.set_state(|d| d.inverted = false),
            0x21 => self.set_state(|d| d.inverted = true),
            0x28 => self.set_state(|d| d.display_on = false),
            0x29 => self.set_state(|d| d.display_on = true),
            0x2C => {
                self.cur_col = self.col_start;
                self.cur_page = self.page_start;
            }
            _ => {}
        }
    }

    fn set_state(&mut self, f: impl FnOnce(&mut Self)) {
        f(self);
        self.generation += 1;
    }

    fn data(&mut self, word: u16) {
        if self.cmd == 0x2C || self.cmd == 0x3C {
            self.write_pixel(word);
            return;
        }
        let p = word as u8;
        let idx = self.param_idx;
        self.param_idx += 1;
        if idx < self.params.len() {
            self.params[idx] = p;
        }
        match (self.cmd, idx) {
            (0x2A, 3) => {
                self.col_start = u16::from_be_bytes([self.params[0], self.params[1]]);
                self.col_end = u16::from_be_bytes([self.params[2], self.params[3]]);
            }
            (0x2B, 3) => {
                self.page_start = u16::from_be_bytes([self.params[0], self.params[1]]);
                self.page_end = u16::from_be_bytes([self.params[2], self.params[3]]);
            }
            (0x36, 0) => self.set_state(|d| d.madctl = p),
            (0xB6, 0..=2) => {
                self.dfc[idx] = p;
                self.generation += 1;
            }
            _ => {}
        }
    }

    /// Logical address limits: with MV the column address runs along the gates.
    fn limits(&self) -> (u16, u16) {
        if self.madctl & MADCTL_MV != 0 {
            (GRAM_ROWS as u16, GRAM_COLS as u16)
        } else {
            (GRAM_COLS as u16, GRAM_ROWS as u16)
        }
    }

    fn write_pixel(&mut self, color: u16) {
        let (max_col, max_page) = self.limits();
        let (c, p) = (self.cur_col, self.cur_page);
        if c < max_col && p < max_page {
            let (mut gc, mut gr) = if self.madctl & MADCTL_MV != 0 { (p, c) } else { (c, p) };
            if self.madctl & MADCTL_MX != 0 {
                gc = (GRAM_COLS as u16 - 1) - gc;
            }
            if self.madctl & MADCTL_MY != 0 {
                gr = (GRAM_ROWS as u16 - 1) - gr;
            }
            self.gram[usize::from(gr) * GRAM_COLS + usize::from(gc)] = color;
            self.generation += 1;
        }
        // Advance within the window: columns first, then pages, wrapping around.
        if self.cur_col >= self.col_end {
            self.cur_col = self.col_start;
            self.cur_page = if self.cur_page >= self.page_end { self.page_start } else { self.cur_page + 1 };
        } else {
            self.cur_col += 1;
        }
    }

    /// Color shown at a position of the glass (`px` along the sources, `py`
    /// along the gates, both in the panel's native portrait orientation),
    /// as 0x00RRGGBB, before backlight.
    pub fn panel_pixel(&self, px: usize, py: usize) -> u32 {
        if self.sleeping || !self.display_on {
            return 0;
        }
        let gc = if self.dfc[1] & DFC_SS != 0 { GRAM_COLS - 1 - px } else { px };
        let gr = if self.dfc[1] & DFC_GS != 0 { GRAM_ROWS - 1 - py } else { py };
        let mut v = self.gram[gr * GRAM_COLS + gc];
        if self.inverted {
            v = !v;
        }
        let (mut r, g, mut b) = (u32::from(v >> 11), u32::from((v >> 5) & 0x3F), u32::from(v & 0x1F));
        if self.madctl & MADCTL_BGR == 0 {
            // The panel's colour filter is BGR; without the BGR bit red and blue swap.
            std::mem::swap(&mut r, &mut b);
        }
        ((r * 255 / 31) << 16) | ((g * 255 / 63) << 8) | (b * 255 / 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(d: &mut Ili9486, c: u8, params: &[u16]) {
        d.write_byte(false, c);
        for p in params {
            for b in p.to_be_bytes() {
                d.write_byte(true, b);
            }
        }
        d.end_frame();
    }

    #[test]
    fn mv_window_write_lands_in_transposed_gram() {
        let mut d = Ili9486::new();
        cmd(&mut d, 0x36, &[0x28]); // MV | BGR
        cmd(&mut d, 0x11, &[]);
        cmd(&mut d, 0x29, &[]);
        // x 10..11, y 20..20
        cmd(&mut d, 0x2A, &[0, 10, 0, 11]);
        cmd(&mut d, 0x2B, &[0, 20, 0, 20]);
        cmd(&mut d, 0x2C, &[0xF800, 0x07E0]);
        assert_eq!(d.gram[10 * GRAM_COLS + 20], 0xF800);
        assert_eq!(d.gram[11 * GRAM_COLS + 20], 0x07E0);
        assert_eq!(d.panel_pixel(20, 10), 0xFF0000);
    }

    #[test]
    fn ramwr_wraps_inside_window() {
        let mut d = Ili9486::new();
        cmd(&mut d, 0x2A, &[0, 0, 0, 1]);
        cmd(&mut d, 0x2B, &[0, 0, 0, 1]);
        cmd(&mut d, 0x2C, &[1, 2, 3, 4, 5]);
        assert_eq!(d.gram[0], 5); // wrapped back to the window start
        assert_eq!(d.gram[1], 2);
        assert_eq!(d.gram[GRAM_COLS], 3);
        assert_eq!(d.gram[GRAM_COLS + 1], 4);
    }

    #[test]
    fn display_off_is_black() {
        let mut d = Ili9486::new();
        cmd(&mut d, 0x2C, &[0xFFFF]);
        assert_eq!(d.panel_pixel(0, 0), 0);
    }
}
