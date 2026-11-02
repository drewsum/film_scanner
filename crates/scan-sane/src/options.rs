//! SANE option descriptors as Rust values, and the driver interface built on them.

use scan_core::ScanError;

/// SANE's well-known option names (`saneopts.h`).
pub mod names {
    pub const SOURCE: &str = "source";
    pub const MODE: &str = "mode";
    pub const DEPTH: &str = "depth";
    pub const RESOLUTION: &str = "resolution";
    pub const PREVIEW: &str = "preview";
    pub const TL_X: &str = "tl-x";
    pub const TL_Y: &str = "tl-y";
    pub const BR_X: &str = "br-x";
    pub const BR_Y: &str = "br-y";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Int,
    Fixed,
    String,
    /// Buttons and groups, which hold no value.
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    None,
    Pixel,
    Mm,
    Dpi,
    Other,
}

/// The values an option accepts. Numbers are already scaled from `SANE_Fixed`.
#[derive(Clone, Debug, PartialEq)]
pub enum Constraint {
    None,
    Range { min: f64, max: f64, quant: f64 },
    Numbers(Vec<f64>),
    Strings(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct OptionDesc {
    /// Position in the driver's option list (option 0 is the option count).
    pub index: usize,
    pub name: String,
    pub kind: Kind,
    pub unit: Unit,
    /// Size of the value in bytes, as SANE reports it: 4 per word for numbers and booleans
    /// (more for arrays such as gamma tables), the buffer size for strings.
    pub size: usize,
    /// Settable by software and currently active.
    pub settable: bool,
    pub constraint: Constraint,
}

impl OptionDesc {
    /// The accepted strings, for a string list.
    pub fn strings(&self) -> &[String] {
        match &self.constraint {
            Constraint::Strings(s) => s,
            _ => &[],
        }
    }

    /// The accepted numbers, for a word list.
    pub fn numbers(&self) -> &[f64] {
        match &self.constraint {
            Constraint::Numbers(n) => n,
            _ => &[],
        }
    }

    /// The `(min, max)` of a range, or of a word list.
    pub fn bounds(&self) -> Option<(f64, f64)> {
        match &self.constraint {
            Constraint::Range { min, max, .. } => Some((*min, *max)),
            Constraint::Numbers(n) if !n.is_empty() => {
                let min = n.iter().copied().fold(f64::INFINITY, f64::min);
                let max = n.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                Some((min, max))
            }
            _ => None,
        }
    }

    /// `v` moved onto the nearest accepted value, so the driver doesn't reject it.
    pub fn nearest(&self, v: f64) -> f64 {
        match &self.constraint {
            Constraint::Range { min, max, quant } => {
                let v = v.clamp(*min, *max);
                if *quant > 0.0 {
                    let stepped = min + ((v - min) / quant).round() * quant;
                    if stepped > *max {
                        stepped - quant
                    } else {
                        stepped
                    }
                } else {
                    v
                }
            }
            Constraint::Numbers(n) => n
                .iter()
                .copied()
                .min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs()))
                .unwrap_or(v),
            _ => v,
        }
    }
}

/// A scalar option value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    /// For `Int` and `Fixed` options.
    Number(f64),
    String(String),
}

/// One frame's layout, from `sane_get_parameters`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parameters {
    pub format: Frame,
    pub last_frame: bool,
    pub bytes_per_line: usize,
    pub pixels_per_line: usize,
    /// `None` when the driver doesn't know the number of lines until the frame ends.
    pub lines: Option<usize>,
    pub depth: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    Gray,
    Rgb,
    Red,
    Green,
    Blue,
}

/// An open SANE device: libsane through [`crate::handle::SaneHandle`], or a fake in tests.
pub trait Driver: Send {
    fn options(&mut self) -> Result<Vec<OptionDesc>, ScanError>;

    /// Set a scalar option. Returns whether the other options may have changed.
    fn set(&mut self, option: &OptionDesc, value: &Value) -> Result<bool, ScanError>;

    fn start(&mut self) -> Result<(), ScanError>;

    fn parameters(&mut self) -> Result<Parameters, ScanError>;

    /// Read the next chunk of the frame (possibly empty); `None` at its end.
    fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, ScanError>;

    /// End the scan (also after the last frame, as SANE asks).
    fn cancel(&mut self);
}

/// A driver's options, looked up by name.
pub struct Options(pub Vec<OptionDesc>);

impl Options {
    /// The option `name`, if it exists and can be set now.
    pub fn get(&self, name: &str) -> Option<&OptionDesc> {
        self.0.iter().find(|o| o.settable && o.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(constraint: Constraint) -> OptionDesc {
        OptionDesc {
            index: 1,
            name: "x".into(),
            kind: Kind::Fixed,
            unit: Unit::Mm,
            size: 4,
            settable: true,
            constraint,
        }
    }

    #[test]
    fn nearest_follows_the_constraint() {
        let range = option(Constraint::Range {
            min: 1.0,
            max: 10.0,
            quant: 2.0,
        });
        assert_eq!(range.nearest(0.0), 1.0);
        assert_eq!(range.nearest(4.2), 5.0);
        assert_eq!(range.nearest(20.0), 9.0);
        assert_eq!(range.bounds(), Some((1.0, 10.0)));

        let list = option(Constraint::Numbers(vec![300.0, 600.0, 1200.0]));
        assert_eq!(list.nearest(800.0), 600.0);
        assert_eq!(list.bounds(), Some((300.0, 1200.0)));

        assert_eq!(option(Constraint::None).nearest(3.3), 3.3);
    }
}
