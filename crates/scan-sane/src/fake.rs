//! A fake SANE driver for tests: a flatbed, a transparency unit with its own resolutions
//! and geometry, and a document feeder.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use scan_core::{MM_PER_INCH, ScanError};

use crate::options::{Constraint, Driver, Frame, Kind, OptionDesc, Parameters, Unit, Value};

/// The sample the fake sends for pixel `(x, y)`, channel `c`, at `depth` bits.
pub fn expected_pixel(x: u32, y: u32, c: usize, depth: u32) -> u16 {
    let v = x * 7 + y * 13 + c as u32 * 50;
    if depth == 8 {
        (v % 256) as u16
    } else {
        (v * 31 % 65536) as u16
    }
}

pub struct Fake {
    pub sources: Vec<String>,
    /// Send color as separate red, green and blue frames.
    pub three_pass: bool,
    /// Report -1 lines, as drivers that don't know the length do.
    pub unknown_lines: bool,
    /// Extra bytes at the end of every line.
    pub padding: usize,
    values: HashMap<String, Value>,
    log: Arc<Mutex<Vec<String>>>,
    frames: Vec<Frame>,
    frame: Option<(Parameters, Vec<u8>, usize)>,
}

impl Fake {
    pub fn new() -> Self {
        let values = [
            ("source", Value::String("Flatbed".into())),
            ("mode", Value::String("Lineart".into())),
            ("depth", Value::Number(1.0)),
            ("resolution", Value::Number(75.0)),
            ("preview", Value::Bool(true)),
            ("tl-x", Value::Number(0.0)),
            ("tl-y", Value::Number(0.0)),
            ("br-x", Value::Number(10.0)),
            ("br-y", Value::Number(10.0)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        Self {
            sources: vec![
                "Flatbed".into(),
                "Transparency Unit".into(),
                "Automatic Document Feeder".into(),
            ],
            three_pass: false,
            unknown_lines: false,
            padding: 0,
            values,
            log: Arc::default(),
            frames: Vec::new(),
            frame: None,
        }
    }

    /// Every option set ("name=value") and "cancel", in order.
    pub fn log(&self) -> Arc<Mutex<Vec<String>>> {
        self.log.clone()
    }

    fn number(&self, name: &str) -> f64 {
        match self.values.get(name) {
            Some(Value::Number(n)) => *n,
            _ => 0.0,
        }
    }

    fn string(&self, name: &str) -> &str {
        match self.values.get(name) {
            Some(Value::String(s)) => s,
            _ => "",
        }
    }

    fn film(&self) -> bool {
        self.string("source") == "Transparency Unit"
    }

    fn start_frame(&mut self, format: Frame) {
        let res = self.number("resolution");
        let px = |a: &str, b: &str| {
            ((self.number(b) - self.number(a)) / MM_PER_INCH as f64 * res).round() as usize
        };
        let (width, height) = (px("tl-x", "br-x"), px("tl-y", "br-y"));
        let depth = self.number("depth") as u32;
        let channels: &[usize] = match format {
            Frame::Rgb => &[0, 1, 2],
            Frame::Gray | Frame::Red => &[0],
            Frame::Green => &[1],
            Frame::Blue => &[2],
        };
        let bytes_per_line = width * channels.len() * depth as usize / 8 + self.padding;
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                for &c in channels {
                    let v = expected_pixel(x as u32, y as u32, c, depth);
                    if depth == 8 {
                        data.push(v as u8);
                    } else {
                        data.extend_from_slice(&v.to_ne_bytes());
                    }
                }
            }
            data.extend(std::iter::repeat_n(0xee, self.padding));
        }
        let params = Parameters {
            format,
            last_frame: self.frames.is_empty(),
            bytes_per_line,
            pixels_per_line: width,
            lines: (!self.unknown_lines).then_some(height),
            depth,
        };
        self.frame = Some((params, data, 0));
    }
}

fn option(index: usize, name: &str, kind: Kind, unit: Unit, constraint: Constraint) -> OptionDesc {
    OptionDesc {
        index,
        name: name.into(),
        kind,
        unit,
        size: if kind == Kind::String { 32 } else { 4 },
        settable: true,
        constraint,
    }
}

impl Driver for Fake {
    fn options(&mut self) -> Result<Vec<OptionDesc>, ScanError> {
        let strings = |s: &[&str]| Constraint::Strings(s.iter().map(|s| s.to_string()).collect());
        let mm = |min: f64, max: f64| Constraint::Range {
            min,
            max,
            quant: 0.0,
        };
        let film = self.film();
        let (x, y) = if film {
            ((80.0, 130.0), (20.0, 240.0))
        } else {
            ((0.0, 216.0), (0.0, 297.0))
        };
        let resolution = if film {
            Constraint::Range {
                min: 400.0,
                max: 6400.0,
                quant: 400.0,
            }
        } else {
            Constraint::Numbers(vec![75.0, 150.0, 300.0, 600.0, 1200.0])
        };
        Ok(vec![
            option(1, "", Kind::Other, Unit::None, Constraint::None),
            option(
                2,
                "source",
                Kind::String,
                Unit::None,
                Constraint::Strings(self.sources.clone()),
            ),
            option(
                3,
                "mode",
                Kind::String,
                Unit::None,
                strings(&["Lineart", "Gray", "Color"]),
            ),
            option(
                4,
                "depth",
                Kind::Int,
                Unit::None,
                Constraint::Numbers(vec![1.0, 8.0, 16.0]),
            ),
            option(5, "resolution", Kind::Int, Unit::Dpi, resolution),
            option(6, "preview", Kind::Bool, Unit::None, Constraint::None),
            option(7, "tl-x", Kind::Fixed, Unit::Mm, mm(x.0, x.1)),
            option(8, "tl-y", Kind::Fixed, Unit::Mm, mm(y.0, y.1)),
            option(9, "br-x", Kind::Fixed, Unit::Mm, mm(x.0, x.1)),
            option(10, "br-y", Kind::Fixed, Unit::Mm, mm(y.0, y.1)),
            OptionDesc {
                settable: false,
                ..option(11, "gamma-table", Kind::Int, Unit::None, Constraint::None)
            },
        ])
    }

    fn set(&mut self, option: &OptionDesc, value: &Value) -> Result<bool, ScanError> {
        let shown = match value {
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => ((n * 1000.0).round() / 1000.0).to_string(),
            Value::String(s) => s.clone(),
        };
        self.log
            .lock()
            .unwrap()
            .push(format!("{}={shown}", option.name));
        let reload = option.name == "source" && self.values.get("source") != Some(value);
        self.values.insert(option.name.clone(), value.clone());
        Ok(reload)
    }

    fn start(&mut self) -> Result<(), ScanError> {
        if self.frames.is_empty() {
            self.frames = match self.string("mode") {
                "Color" if self.three_pass => vec![Frame::Blue, Frame::Green, Frame::Red],
                "Color" => vec![Frame::Rgb],
                "Gray" => vec![Frame::Gray],
                _ => return Err(ScanError::Backend("fake scans color or grey".into())),
            };
        }
        // Frames are popped from the end: red, green, then blue.
        let format = self.frames.pop().unwrap();
        self.start_frame(format);
        Ok(())
    }

    fn parameters(&mut self) -> Result<Parameters, ScanError> {
        self.frame
            .as_ref()
            .map(|f| f.0)
            .ok_or_else(|| ScanError::Backend("not scanning".into()))
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, ScanError> {
        let Some((_, data, pos)) = &mut self.frame else {
            return Err(ScanError::Backend("not scanning".into()));
        };
        if *pos == data.len() {
            return Ok(None);
        }
        // Uneven chunks, to cross line boundaries.
        let n = (data.len() - *pos).min(buf.len()).min(10_007);
        buf[..n].copy_from_slice(&data[*pos..*pos + n]);
        *pos += n;
        Ok(Some(n))
    }

    fn cancel(&mut self) {
        self.frames.clear();
        self.frame = None;
        self.log.lock().unwrap().push("cancel".into());
    }
}
