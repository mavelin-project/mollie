//! Format specifiers of interpolated values: `"${value:>8.2}"`.

use std::fmt;

/// Alignment of a value padded to a width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FormatAlign {
    /// `<`
    Left,
    /// `>`
    Right,
    /// `^`
    Center,
}

/// How a number is written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FormatKind {
    #[default]
    Display,
    /// `x`
    Hex,
    /// `X`
    UpperHex,
    /// `b`
    Binary,
}

/// `[<|>|^][0][width][.precision][x|X|b]`, e.g. `>8`, `04`, `.2` or `x`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FormatSpec {
    /// Alignment in the width. By default, numbers are aligned to the right
    /// and other values to the left.
    pub align: Option<FormatAlign>,
    /// Pads numbers with zeros after their sign.
    pub zero: bool,
    /// Minimum width in characters.
    pub width: Option<u16>,
    /// Digits after the decimal point.
    pub precision: Option<u16>,
    pub kind: FormatKind,
}

impl FormatSpec {
    /// Parses a specifier (the part after `:`). Returns `None` if it's
    /// malformed.
    pub fn parse(spec: &str) -> Option<Self> {
        let mut rest = spec;
        let mut result = Self::default();

        let mut take = |prefix: char| rest.strip_prefix(prefix).map(|stripped| rest = stripped).is_some();

        result.align = if take('<') {
            Some(FormatAlign::Left)
        } else if take('>') {
            Some(FormatAlign::Right)
        } else if take('^') {
            Some(FormatAlign::Center)
        } else {
            None
        };

        result.zero = take('0');

        let digits = |text: &str| text.find(|character: char| !character.is_ascii_digit()).unwrap_or(text.len());

        let width = digits(rest);

        if width > 0 {
            result.width = Some(rest[..width].parse().ok()?);
            rest = &rest[width..];
        }

        if let Some(stripped) = rest.strip_prefix('.') {
            let precision = digits(stripped);

            if precision == 0 {
                return None;
            }

            result.precision = Some(stripped[..precision].parse().ok()?);
            rest = &stripped[precision..];
        }

        result.kind = match rest {
            "" => FormatKind::Display,
            "x" => FormatKind::Hex,
            "X" => FormatKind::UpperHex,
            "b" => FormatKind::Binary,
            _ => return None,
        };

        Some(result)
    }

    /// The specifier as a number, passed to functions formatting values at
    /// run time.
    pub fn pack(self) -> u64 {
        let align = match self.align {
            None => 0,
            Some(FormatAlign::Left) => 1,
            Some(FormatAlign::Right) => 2,
            Some(FormatAlign::Center) => 3,
        };
        let kind = match self.kind {
            FormatKind::Display => 0,
            FormatKind::Hex => 1,
            FormatKind::UpperHex => 2,
            FormatKind::Binary => 3,
        };

        align
            | u64::from(self.zero) << 2
            | kind << 3
            | u64::from(self.width.is_some()) << 5
            | u64::from(self.precision.is_some()) << 6
            | u64::from(self.width.unwrap_or(0)) << 16
            | u64::from(self.precision.unwrap_or(0)) << 32
    }

    /// The specifier packed by [`FormatSpec::pack`].
    pub fn unpack(packed: u64) -> Self {
        let bits = |shift: u32, mask: u64| (packed >> shift) & mask;
        // Width and precision are 16 bits each.
        let half = |shift: u32| u16::try_from(bits(shift, 0xFFFF)).unwrap_or(u16::MAX);

        Self {
            align: match bits(0, 0b11) {
                1 => Some(FormatAlign::Left),
                2 => Some(FormatAlign::Right),
                3 => Some(FormatAlign::Center),
                _ => None,
            },
            zero: bits(2, 1) == 1,
            width: (bits(5, 1) == 1).then(|| half(16)),
            precision: (bits(6, 1) == 1).then(|| half(32)),
            kind: match bits(3, 0b11) {
                1 => FormatKind::Hex,
                2 => FormatKind::UpperHex,
                3 => FormatKind::Binary,
                _ => FormatKind::Display,
            },
        }
    }

    /// Pads `text` (a formatted value) to the width. `number` tells whether
    /// it's a number, which is aligned to the right and may be padded with
    /// zeros after its sign.
    pub fn pad(&self, text: &str, number: bool) -> String {
        let length = text.chars().count();
        let width = usize::from(self.width.unwrap_or(0));

        if length >= width {
            return text.to_owned();
        }

        let padding = width - length;

        if self.zero && number {
            let (sign, digits) = text.strip_prefix('-').map_or(("", text), |digits| ("-", digits));

            return format!("{sign}{}{digits}", "0".repeat(padding));
        }

        let (before, after) = match self.align.unwrap_or(if number { FormatAlign::Right } else { FormatAlign::Left }) {
            FormatAlign::Left => (0, padding),
            FormatAlign::Right => (padding, 0),
            FormatAlign::Center => (padding / 2, padding - padding / 2),
        };

        format!("{}{text}{}", " ".repeat(before), " ".repeat(after))
    }
}

impl fmt::Display for FormatSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.align {
            Some(FormatAlign::Left) => f.write_str("<")?,
            Some(FormatAlign::Right) => f.write_str(">")?,
            Some(FormatAlign::Center) => f.write_str("^")?,
            None => (),
        }

        if self.zero {
            f.write_str("0")?;
        }

        if let Some(width) = self.width {
            write!(f, "{width}")?;
        }

        if let Some(precision) = self.precision {
            write!(f, ".{precision}")?;
        }

        f.write_str(match self.kind {
            FormatKind::Display => "",
            FormatKind::Hex => "x",
            FormatKind::UpperHex => "X",
            FormatKind::Binary => "b",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{FormatAlign, FormatKind, FormatSpec};

    #[test]
    fn parses_specifiers() {
        assert_eq!(
            FormatSpec::parse(">08.2"),
            Some(FormatSpec {
                align: Some(FormatAlign::Right),
                zero: true,
                width: Some(8),
                precision: Some(2),
                kind: FormatKind::Display,
            })
        );
        assert_eq!(FormatSpec::parse("x").map(|spec| spec.kind), Some(FormatKind::Hex));
        assert_eq!(FormatSpec::parse("4").and_then(|spec| spec.width), Some(4));
        assert_eq!(FormatSpec::parse("."), None);
        assert_eq!(FormatSpec::parse("2x8"), None);
        assert_eq!(FormatSpec::parse("99999999"), None);
    }

    #[test]
    fn packs_specifiers() {
        for spec in ["<10", "^7.3", "08X", "b", ".0", "65535"] {
            let parsed = FormatSpec::parse(spec).unwrap();

            assert_eq!(FormatSpec::unpack(parsed.pack()), parsed);
            assert_eq!(parsed.to_string(), spec);
        }
    }

    #[test]
    fn pads_values() {
        let spec = |text| FormatSpec::parse(text).unwrap();

        assert_eq!(spec("5").pad("ab", false), "ab   ");
        assert_eq!(spec("5").pad("12", true), "   12");
        assert_eq!(spec("^6").pad("ab", false), "  ab  ");
        assert_eq!(spec("05").pad("-12", true), "-0012");
    }
}
