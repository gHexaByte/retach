/// Underline style variant.
#[derive(Copy, Clone, Debug, Default, PartialEq, Hash)]
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
#[derive(Copy, Clone, Debug, Default, PartialEq, Hash)]
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
}

/// Terminal color, either a 256-color palette index or direct RGB.
#[derive(Copy, Clone, Debug, PartialEq, Hash)]
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
        if self.is_default() {
            return b"\x1b[0m".to_vec();
        }
        let mut out = Vec::with_capacity(24);
        out.extend_from_slice(b"\x1b[0");
        let mut need_sep = true;
        self.write_sgr_to(&mut out, &mut need_sep);
        out.push(b'm');
        out
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
                90..=97 => self.fg = Some(Color::Indexed((p - 90 + 8) as u8)),
                100..=107 => self.bg = Some(Color::Indexed((p - 100 + 8) as u8)),
                _ => {}
            }
            i += 1;
        }
    }
}

/// Parse extended color (38;5;N or 38;2;R;G;B) from SGR params
pub fn parse_extended_color(params: &[Vec<u16>], i: &mut usize) -> Option<Color> {
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
}
