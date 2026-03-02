/// SGR text attributes and foreground/background colors for a cell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: u8, // 0=none, 1=single, 2=double, 3=curly, 4=dotted, 5=dashed
    pub blink: bool,
    pub inverse: bool,
    pub strikethrough: bool,
    pub hidden: bool,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
}

/// Terminal color, either a 256-color palette index or direct RGB.
#[derive(Clone, Debug, PartialEq)]
pub enum Color {
    /// 256-color palette index (0-255).
    Indexed(u8),
    /// Direct 24-bit RGB color.
    Rgb(u8, u8, u8),
}

impl Style {
    /// Return true if all attributes are at their default (reset) values.
    pub fn is_default(&self) -> bool {
        *self == Style::default()
    }

    /// Render this style as an SGR escape sequence
    pub fn to_sgr(&self) -> Vec<u8> {
        if self.is_default() {
            return Vec::new();
        }
        let mut params: Vec<String> = Vec::new();
        if self.bold { params.push("1".into()); }
        if self.dim { params.push("2".into()); }
        if self.italic { params.push("3".into()); }
        match self.underline {
            0 => {}
            1 => params.push("4".into()),
            n => params.push(format!("4:{}", n)),
        }
        if self.blink { params.push("5".into()); }
        if self.inverse { params.push("7".into()); }
        if self.strikethrough { params.push("9".into()); }
        if self.hidden { params.push("8".into()); }
        match &self.fg {
            Some(Color::Indexed(c)) if *c < 8 => params.push(format!("{}", 30 + c)),
            Some(Color::Indexed(c)) if *c < 16 => params.push(format!("{}", 90 + c - 8)),
            Some(Color::Indexed(c)) => { params.push("38".into()); params.push("5".into()); params.push(c.to_string()); }
            Some(Color::Rgb(r, g, b)) => { params.push("38".into()); params.push("2".into()); params.push(r.to_string()); params.push(g.to_string()); params.push(b.to_string()); }
            None => {}
        }
        match &self.bg {
            Some(Color::Indexed(c)) if *c < 8 => params.push(format!("{}", 40 + c)),
            Some(Color::Indexed(c)) if *c < 16 => params.push(format!("{}", 100 + c - 8)),
            Some(Color::Indexed(c)) => { params.push("48".into()); params.push("5".into()); params.push(c.to_string()); }
            Some(Color::Rgb(r, g, b)) => { params.push("48".into()); params.push("2".into()); params.push(r.to_string()); params.push(g.to_string()); params.push(b.to_string()); }
            None => {}
        }
        if params.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"\x1b[");
        out.extend_from_slice(params.join(";").as_bytes());
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
                        self.underline = params[i][1] as u8;
                    } else {
                        self.underline = 1;
                    }
                }
                5 | 6 => self.blink = true,
                7 => self.inverse = true,
                8 => self.hidden = true,
                9 => self.strikethrough = true,
                21 => self.underline = 2, // double underline
                22 => { self.bold = false; self.dim = false; }
                23 => self.italic = false,
                24 => self.underline = 0,
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
}
