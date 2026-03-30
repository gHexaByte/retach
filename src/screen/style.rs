/// Interned style identifier. Wraps a `u16` table index.
/// ID 0 is always the default (reset) style.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub struct StyleId(pub(super) u16);

impl StyleId {
    /// Table index.
    pub fn index(self) -> usize { self.0 as usize }
    /// True when this is the default style (index 0).
    pub fn is_default(self) -> bool { self.0 == 0 }
}

/// Underline style variant.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl UnderlineStyle {
    /// Convert from a raw SGR subparameter value.
    pub fn from_sgr(n: u8) -> Self {
        match n {
            0 => Self::None,
            1 => Self::Single,
            2 => Self::Double,
            3 => Self::Curly,
            4 => Self::Dotted,
            5 => Self::Dashed,
            _ => Self::Single, // unknown → single
        }
    }

    /// SGR subparameter value for this underline style.
    pub fn sgr_param(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Single => 1,
            Self::Double => 2,
            Self::Curly => 3,
            Self::Dotted => 4,
            Self::Dashed => 5,
        }
    }
}

/// SGR text attributes and foreground/background colors for a cell.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: UnderlineStyle,
    pub blink: bool,
    pub inverse: bool,
    pub strikethrough: bool,
    pub hidden: bool,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub underline_color: Option<Color>,
}

/// Terminal color, either a 256-color palette index or direct RGB.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    /// 256-color palette index (0-255).
    Indexed(u8),
    /// Direct 24-bit RGB color.
    Rgb(u8, u8, u8),
}

/// Write a u8 value as decimal ASCII digits into `out`.
fn write_u8(out: &mut Vec<u8>, n: u8) {
    if n >= 100 { out.push(b'0' + n / 100); }
    if n >= 10 { out.push(b'0' + (n / 10) % 10); }
    out.push(b'0' + n % 10);
}

/// Write a u16 value as decimal ASCII digits into `out`.
pub fn write_u16(out: &mut Vec<u8>, n: u16) {
    if n >= 10000 { out.push(b'0' + (n / 10000) as u8); }
    if n >= 1000 { out.push(b'0' + ((n / 1000) % 10) as u8); }
    if n >= 100 { out.push(b'0' + ((n / 100) % 10) as u8); }
    if n >= 10 { out.push(b'0' + ((n / 10) % 10) as u8); }
    out.push(b'0' + (n % 10) as u8);
}

impl Style {
    /// Return true if all attributes are at their default (reset) values.
    pub fn is_default(self) -> bool {
        self == Style::default()
    }

    /// Write SGR attribute parameters directly as bytes into `out`.
    /// Uses `;` separator. Caller is responsible for the `\x1b[` prefix and `m` suffix.
    fn write_sgr_to(self, out: &mut Vec<u8>, need_sep: &mut bool) {
        macro_rules! sep {
            ($out:expr, $need:expr) => {
                if *$need { $out.push(b';'); }
                *$need = true;
            };
        }
        if self.bold { sep!(out, need_sep); out.push(b'1'); }
        if self.dim { sep!(out, need_sep); out.push(b'2'); }
        if self.italic { sep!(out, need_sep); out.push(b'3'); }
        match self.underline {
            UnderlineStyle::None => {}
            UnderlineStyle::Single => { sep!(out, need_sep); out.push(b'4'); }
            other => {
                sep!(out, need_sep);
                out.push(b'4');
                out.push(b':');
                out.push(b'0' + other.sgr_param());
            }
        }
        if self.blink { sep!(out, need_sep); out.push(b'5'); }
        if self.inverse { sep!(out, need_sep); out.push(b'7'); }
        if self.hidden { sep!(out, need_sep); out.push(b'8'); }
        if self.strikethrough { sep!(out, need_sep); out.push(b'9'); }
        Self::write_color_to(out, self.fg, 30, 90, b"38", need_sep);
        Self::write_color_to(out, self.bg, 40, 100, b"48", need_sep);
        // Underline color (SGR 58)
        if let Some(ref color) = self.underline_color {
            sep!(out, need_sep);
            match color {
                Color::Indexed(c) => {
                    out.extend_from_slice(b"58;5;");
                    write_u8(out, *c);
                }
                Color::Rgb(r, g, b) => {
                    out.extend_from_slice(b"58;2;");
                    write_u8(out, *r);
                    out.push(b';');
                    write_u8(out, *g);
                    out.push(b';');
                    write_u8(out, *b);
                }
            }
        }
    }

    /// Write SGR color parameters directly as bytes.
    fn write_color_to(out: &mut Vec<u8>, color: Option<Color>, base: u8, bright_base: u8, extended: &[u8], need_sep: &mut bool) {
        match color {
            Some(Color::Indexed(c)) if c < 8 => {
                if *need_sep { out.push(b';'); }
                *need_sep = true;
                write_u8(out, base + c);
            }
            Some(Color::Indexed(c)) if c < 16 => {
                if *need_sep { out.push(b';'); }
                *need_sep = true;
                write_u8(out, bright_base + c - 8);
            }
            Some(Color::Indexed(c)) => {
                if *need_sep { out.push(b';'); }
                *need_sep = true;
                out.extend_from_slice(extended);
                out.extend_from_slice(b";5;");
                write_u8(out, c);
            }
            Some(Color::Rgb(r, g, b)) => {
                if *need_sep { out.push(b';'); }
                *need_sep = true;
                out.extend_from_slice(extended);
                out.extend_from_slice(b";2;");
                write_u8(out, r);
                out.push(b';');
                write_u8(out, g);
                out.push(b';');
                write_u8(out, b);
            }
            None => {}
        }
    }

    /// Render this style as an SGR sequence (no reset prefix).
    /// Returns empty Vec for default style.
    #[cfg(test)]
    pub fn to_sgr(self) -> Vec<u8> {
        if self.is_default() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(24);
        out.extend_from_slice(b"\x1b[");
        let mut need_sep = false;
        self.write_sgr_to(&mut out, &mut need_sep);
        out.push(b'm');
        out
    }

    /// Render this style as a combined reset+set SGR: `\x1b[0;1;31m` instead of
    /// separate `\x1b[0m` + `\x1b[31m`. Always includes reset (param 0).
    /// For default style returns just `\x1b[0m`.
    pub fn to_sgr_with_reset(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(24);
        self.write_sgr_with_reset_to(&mut out);
        out
    }

    /// Write combined reset+set SGR directly into `out`, avoiding intermediate allocation.
    /// For default style writes just `\x1b[0m`.
    pub fn write_sgr_with_reset_to(self, out: &mut Vec<u8>) {
        if self.is_default() {
            out.extend_from_slice(b"\x1b[0m");
            return;
        }
        out.extend_from_slice(b"\x1b[0");
        let mut need_sep = true;
        self.write_sgr_to(out, &mut need_sep);
        out.push(b'm');
    }

    /// Apply SGR parameters to this style (accumulates)
    pub fn apply_sgr(&mut self, params: &[Vec<u16>]) {
        if params.is_empty() {
            *self = Style::default();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            let p = params[i].first().copied().unwrap_or(0);
            match p {
                0 => *self = Style::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => {
                    // Check for subparams: 4:0 (none), 4:1 (single), 4:2 (double), 4:3 (curly), etc.
                    if params[i].len() > 1 {
                        self.underline = UnderlineStyle::from_sgr(params[i][1] as u8);
                    } else {
                        self.underline = UnderlineStyle::Single;
                    }
                }
                5 | 6 => self.blink = true,
                7 => self.inverse = true,
                8 => self.hidden = true,
                9 => self.strikethrough = true,
                21 => self.underline = UnderlineStyle::Double, // double underline
                22 => { self.bold = false; self.dim = false; }
                23 => self.italic = false,
                24 => self.underline = UnderlineStyle::None,
                25 => self.blink = false,
                27 => self.inverse = false,
                28 => self.hidden = false,
                29 => self.strikethrough = false,
                30..=37 => self.fg = Some(Color::Indexed((p - 30) as u8)),
                38 => {
                    if let Some(color) = parse_extended_color(params, &mut i) {
                        self.fg = Some(color);
                    }
                }
                39 => self.fg = None,
                40..=47 => self.bg = Some(Color::Indexed((p - 40) as u8)),
                48 => {
                    if let Some(color) = parse_extended_color(params, &mut i) {
                        self.bg = Some(color);
                    }
                }
                49 => self.bg = None,
                58 => {
                    if let Some(color) = parse_extended_color(params, &mut i) {
                        self.underline_color = Some(color);
                    }
                }
                59 => self.underline_color = None,
                90..=97 => self.fg = Some(Color::Indexed((p - 90 + 8) as u8)),
                100..=107 => self.bg = Some(Color::Indexed((p - 100 + 8) as u8)),
                _ => {}
            }
            i += 1;
        }
    }
}

/// Parse extended color (38;5;N or 38;2;R;G;B) from SGR params
fn parse_extended_color(params: &[Vec<u16>], i: &mut usize) -> Option<Color> {
    // Check for colon-separated subparams first (e.g., 38:5:N or 38:2:R:G:B)
    if params[*i].len() > 1 {
        let sub = &params[*i];
        if sub.len() >= 3 && sub[1] == 5 {
            return Some(Color::Indexed(sub[2] as u8));
        }
        if sub[1] == 2 {
            if sub.len() >= 6 {
                // 38:2:CS:R:G:B (with color space ID)
                return Some(Color::Rgb(sub[3] as u8, sub[4] as u8, sub[5] as u8));
            } else if sub.len() >= 5 {
                // 38:2:R:G:B (without color space ID)
                return Some(Color::Rgb(sub[2] as u8, sub[3] as u8, sub[4] as u8));
            }
        }
        return None;
    }
    // Semicolon-separated: look at next params
    if *i + 1 < params.len() {
        let mode = params[*i + 1].first().copied().unwrap_or(0);
        if mode == 5 && *i + 2 < params.len() {
            let c = params[*i + 2].first().copied().unwrap_or(0);
            *i += 2;
            return Some(Color::Indexed(c as u8));
        }
        if mode == 2 && *i + 4 < params.len() {
            let r = params[*i + 2].first().copied().unwrap_or(0);
            let g = params[*i + 3].first().copied().unwrap_or(0);
            let b = params[*i + 4].first().copied().unwrap_or(0);
            *i += 4;
            return Some(Color::Rgb(r as u8, g as u8, b as u8));
        }
    }
    None
}

/// Interning table for cell styles. Index 0 is always the default style.
/// Dead slots are tracked in a free list and reused by `intern()` after GC.
#[derive(Clone, Debug)]
pub struct StyleTable {
    styles: Vec<Style>,
    index: std::collections::HashMap<Style, u16>,
    free_slots: Vec<u16>,
}

impl StyleTable {
    pub fn new() -> Self {
        Self {
            styles: vec![Style::default()],
            index: std::collections::HashMap::new(),
            free_slots: Vec::new(),
        }
    }

    /// Intern a style, returning its ID. Default style always returns `StyleId(0)`.
    /// Reuses free slots from GC before growing. If the table is full
    /// (65536 entries) and no free slots remain, returns `StyleId(0)` (degradation).
    pub fn intern(&mut self, style: Style) -> StyleId {
        if style.is_default() { return StyleId(0); }
        if let Some(&id) = self.index.get(&style) {
            return StyleId(id);
        }
        if let Some(id) = self.free_slots.pop() {
            self.styles[id as usize] = style;
            self.index.insert(style, id);
            return StyleId(id);
        }
        if self.styles.len() > u16::MAX as usize {
            return StyleId(0); // table full — fall back to default style
        }
        let id = self.styles.len() as u16;
        self.styles.push(style);
        self.index.insert(style, id);
        StyleId(id)
    }

    /// Look up a style by ID.
    #[inline]
    pub fn get(&self, id: StyleId) -> Style {
        self.styles.get(id.index()).copied().unwrap_or_default()
    }

    /// Number of occupied (live) slots, excluding free slots.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.styles.len() - self.free_slots.len()
    }

    /// Total number of allocated slots (including free slots).
    pub fn capacity(&self) -> usize {
        self.styles.len()
    }

    /// True if the table has reached maximum capacity and no free slots remain.
    pub fn is_full(&self) -> bool {
        self.styles.len() > u16::MAX as usize && self.free_slots.is_empty()
    }

    /// Reclaim dead slots: any style ID not marked as live in the bitvec
    /// is removed from the index and added to the free list.
    pub fn reclaim(&mut self, live: &[bool]) {
        self.free_slots.clear();
        for id in 1..self.styles.len() {
            if id < live.len() && !live[id] {
                self.index.remove(&self.styles[id]);
                self.free_slots.push(id as u16);
            }
        }
    }

    /// Reset the table to its initial state (only the default style).
    /// Called on RIS (full terminal reset) to reclaim memory from
    /// accumulated styles. Existing cells referencing old style IDs
    /// will be blanked by the RIS handler anyway.
    pub fn reset(&mut self) {
        self.styles.clear();
        self.styles.push(Style::default());
        self.index.clear();
        self.free_slots.clear();
    }
}

impl Default for StyleTable {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_round_trip_default() {
        let style = Style::default();
        assert!(style.to_sgr().is_empty());
    }

    #[test]
    fn sgr_round_trip_bold() {
        let mut style = Style::default();
        style.bold = true;
        let sgr = style.to_sgr();
        assert_eq!(sgr, b"\x1b[1m");

        let mut parsed = Style::default();
        parsed.apply_sgr(&[vec![1]]);
        assert_eq!(parsed, style);
    }

    #[test]
    fn sgr_round_trip_fg_indexed() {
        let mut style = Style::default();
        style.fg = Some(Color::Indexed(1));
        let sgr = style.to_sgr();
        assert_eq!(sgr, b"\x1b[31m");

        let mut parsed = Style::default();
        parsed.apply_sgr(&[vec![31]]);
        assert_eq!(parsed, style);
    }

    #[test]
    fn sgr_round_trip_256_color() {
        let mut style = Style::default();
        style.fg = Some(Color::Indexed(200));
        let sgr = style.to_sgr();
        assert_eq!(sgr, b"\x1b[38;5;200m");

        let mut parsed = Style::default();
        parsed.apply_sgr(&[vec![38], vec![5], vec![200]]);
        assert_eq!(parsed, style);
    }

    #[test]
    fn sgr_round_trip_rgb() {
        let mut style = Style::default();
        style.fg = Some(Color::Rgb(100, 150, 200));
        let sgr = style.to_sgr();
        assert_eq!(sgr, b"\x1b[38;2;100;150;200m");

        let mut parsed = Style::default();
        parsed.apply_sgr(&[vec![38], vec![2], vec![100], vec![150], vec![200]]);
        assert_eq!(parsed, style);
    }

    #[test]
    fn sgr_reset() {
        let mut style = Style::default();
        style.bold = true;
        style.fg = Some(Color::Indexed(1));
        style.apply_sgr(&[vec![0]]);
        assert_eq!(style, Style::default());
    }

    #[test]
    fn sgr_colon_separated_subparams() {
        let mut style = Style::default();
        // 38:5:200 as colon-separated subparams
        style.apply_sgr(&[vec![38, 5, 200]]);
        assert_eq!(style.fg, Some(Color::Indexed(200)));
    }

    // --- New tests ---

    #[test]
    fn sgr_underline_variants() {
        // double underline
        let mut s = Style::default();
        s.underline = UnderlineStyle::Double;
        let sgr = s.to_sgr();
        assert_eq!(sgr, b"\x1b[4:2m", "double underline should use 4:2");

        // curly underline
        s.underline = UnderlineStyle::Curly;
        let sgr = s.to_sgr();
        assert_eq!(sgr, b"\x1b[4:3m", "curly underline should use 4:3");

        // dotted underline
        s.underline = UnderlineStyle::Dotted;
        let sgr = s.to_sgr();
        assert_eq!(sgr, b"\x1b[4:4m", "dotted underline should use 4:4");

        // dashed underline
        s.underline = UnderlineStyle::Dashed;
        let sgr = s.to_sgr();
        assert_eq!(sgr, b"\x1b[4:5m", "dashed underline should use 4:5");
    }

    #[test]
    fn sgr_bright_colors() {
        // Bright fg: indices 8-15 → codes 90-97
        let mut s = Style::default();
        s.fg = Some(Color::Indexed(8));
        assert_eq!(s.to_sgr(), b"\x1b[90m");

        s.fg = Some(Color::Indexed(15));
        assert_eq!(s.to_sgr(), b"\x1b[97m");

        // Bright bg: indices 8-15 → codes 100-107
        s.fg = None;
        s.bg = Some(Color::Indexed(8));
        assert_eq!(s.to_sgr(), b"\x1b[100m");

        s.bg = Some(Color::Indexed(15));
        assert_eq!(s.to_sgr(), b"\x1b[107m");
    }

    #[test]
    fn sgr_all_attributes_combined() {
        let s = Style {
            bold: true,
            dim: true,
            italic: true,
            underline: UnderlineStyle::Single,
            blink: true,
            inverse: true,
            strikethrough: true,
            hidden: true,
            fg: Some(Color::Indexed(1)),
            bg: Some(Color::Indexed(4)),
            underline_color: None,
        };
        let sgr = s.to_sgr();
        let text = String::from_utf8_lossy(&sgr);
        // All attribute codes should be present
        assert!(text.contains("1;"), "bold");
        assert!(text.contains("2;"), "dim");
        assert!(text.contains("3;"), "italic");
        assert!(text.contains(";4;"), "underline");
        assert!(text.contains(";5;"), "blink");
        assert!(text.contains(";7;"), "inverse");
        assert!(text.contains(";9;"), "strikethrough");
        assert!(text.contains(";8;"), "hidden");
        assert!(text.contains("31"), "red fg");
        assert!(text.contains("44"), "blue bg");
    }

    #[test]
    fn sgr_dim_attribute() {
        let mut s = Style::default();
        s.dim = true;
        assert_eq!(s.to_sgr(), b"\x1b[2m");
    }

    #[test]
    fn sgr_inverse_attribute() {
        let mut s = Style::default();
        s.inverse = true;
        assert_eq!(s.to_sgr(), b"\x1b[7m");
    }

    #[test]
    fn sgr_blink_attribute() {
        let mut s = Style::default();
        s.blink = true;
        assert_eq!(s.to_sgr(), b"\x1b[5m");
    }

    #[test]
    fn sgr_strikethrough_attribute() {
        let mut s = Style::default();
        s.strikethrough = true;
        assert_eq!(s.to_sgr(), b"\x1b[9m");
    }

    #[test]
    fn sgr_with_reset_default() {
        let s = Style::default();
        assert_eq!(s.to_sgr_with_reset(), b"\x1b[0m");
    }

    #[test]
    fn sgr_with_reset_styled() {
        let mut s = Style::default();
        s.bold = true;
        s.fg = Some(Color::Indexed(1)); // red
        let sgr = s.to_sgr_with_reset();
        assert_eq!(sgr, b"\x1b[0;1;31m", "reset+bold+red");
    }

    // --- StyleTable GC tests ---

    #[test]
    fn style_table_len_and_capacity() {
        let mut table = StyleTable::new();
        assert_eq!(table.len(), 1); // default style
        assert_eq!(table.capacity(), 1);

        let s1 = Style { bold: true, ..Style::default() };
        table.intern(s1);
        assert_eq!(table.len(), 2);
        assert_eq!(table.capacity(), 2);
    }

    #[test]
    fn style_table_reclaim_frees_dead_slots() {
        let mut table = StyleTable::new();
        let s1 = Style { bold: true, ..Style::default() };
        let s2 = Style { italic: true, ..Style::default() };
        let s3 = Style { dim: true, ..Style::default() };
        let id1 = table.intern(s1);
        let id2 = table.intern(s2);
        let id3 = table.intern(s3);
        assert_eq!(table.len(), 4); // default + 3
        assert_eq!(table.capacity(), 4);

        // Mark s1 and s3 as live, s2 as dead
        let mut live = vec![false; table.capacity()];
        live[0] = true;
        live[id1.index()] = true;
        live[id3.index()] = true;
        table.reclaim(&live);

        assert_eq!(table.len(), 3); // default + s1 + s3
        assert_eq!(table.capacity(), 4); // Vec didn't shrink

        // New intern should reuse s2's slot
        let s4 = Style { blink: true, ..Style::default() };
        let id4 = table.intern(s4);
        assert_eq!(id4, id2, "should reuse freed slot");
        assert_eq!(table.get(id4), s4);
        assert_eq!(table.len(), 4);
    }

    #[test]
    fn style_table_reclaim_all_dead() {
        let mut table = StyleTable::new();
        for i in 0..10u8 {
            table.intern(Style { fg: Some(Color::Indexed(i)), ..Style::default() });
        }
        assert_eq!(table.len(), 11); // default + 10

        // Only default is live
        let mut live = vec![false; table.capacity()];
        live[0] = true;
        table.reclaim(&live);

        assert_eq!(table.len(), 1); // only default
        // All 10 slots are free for reuse
        let s = Style { bold: true, ..Style::default() };
        let id = table.intern(s);
        let raw = id.index();
        assert!(raw >= 1 && raw <= 10, "should reuse a freed slot, got {}", raw);
    }

    #[test]
    fn style_table_reclaim_none_dead() {
        let mut table = StyleTable::new();
        let s1 = Style { bold: true, ..Style::default() };
        table.intern(s1);

        let live = vec![true; table.capacity()];
        table.reclaim(&live);

        assert_eq!(table.len(), 2); // nothing reclaimed
    }

    #[test]
    fn style_table_is_full() {
        let table = StyleTable::new();
        assert!(!table.is_full());
        // Can't practically fill 65536 entries in a unit test,
        // but we can verify the logic by checking the condition
    }

    #[test]
    fn style_table_reset_clears_free_slots() {
        let mut table = StyleTable::new();
        let s1 = Style { bold: true, ..Style::default() };
        table.intern(s1);

        let mut live = vec![false; table.capacity()];
        live[0] = true;
        table.reclaim(&live);
        assert_eq!(table.len(), 1);

        table.reset();
        assert_eq!(table.len(), 1);
        assert_eq!(table.capacity(), 1);
    }

    // --- Underline color (SGR 58/59) tests ---

    #[test]
    fn underline_color_indexed() {
        let mut style = Style::default();
        style.apply_sgr(&[vec![58], vec![5], vec![196]]);
        assert_eq!(style.underline_color, Some(Color::Indexed(196)));
    }

    #[test]
    fn underline_color_rgb() {
        let mut style = Style::default();
        style.apply_sgr(&[vec![58], vec![2], vec![255], vec![128], vec![0]]);
        assert_eq!(style.underline_color, Some(Color::Rgb(255, 128, 0)));
    }

    #[test]
    fn underline_color_reset() {
        let mut style = Style::default();
        style.apply_sgr(&[vec![58], vec![5], vec![196]]);
        assert!(style.underline_color.is_some());
        style.apply_sgr(&[vec![59]]);
        assert_eq!(style.underline_color, None);
    }

    #[test]
    fn underline_color_colon_separated() {
        let mut style = Style::default();
        style.apply_sgr(&[vec![58, 5, 196]]);
        assert_eq!(style.underline_color, Some(Color::Indexed(196)));
    }

    #[test]
    fn underline_color_emitted_in_sgr() {
        let mut style = Style::default();
        style.underline_color = Some(Color::Indexed(196));
        let sgr = style.to_sgr();
        let s = String::from_utf8_lossy(&sgr);
        assert!(s.contains("58;5;196"), "SGR should contain underline color, got: {}", s);
    }
}
