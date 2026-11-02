//! The Export step: where and how a roll's frames are saved, and the `metadata.yml` that
//! [film_roll_publisher](https://github.com/drewsum/film_roll_publisher) reads to upload them.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use jiff::civil::Date;
use scan_film::FilmStock;

/// The file the publisher reads the roll's metadata from, in the folder it uploads.
pub const METADATA_FILE: &str = "metadata.yml";

/// The publisher's date format, MM/DD/YYYY.
const DATE_FORMAT: &str = "%m/%d/%Y";

/// The format exported frames are saved in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameFormat {
    /// 8-bit JPEG, at a quality from 1 to 100.
    Jpeg(u8),
    /// 8-bit PNG.
    Png,
    /// 16-bit TIFF, for archiving. The publisher doesn't upload these.
    Tiff,
}

impl FrameFormat {
    pub fn extension(self) -> &'static str {
        match self {
            FrameFormat::Jpeg(_) => "jpg",
            FrameFormat::Png => "png",
            FrameFormat::Tiff => "tif",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FrameFormat::Jpeg(_) => "JPEG",
            FrameFormat::Png => "PNG",
            FrameFormat::Tiff => "TIFF (16-bit)",
        }
    }

    /// Whether the publisher picks these files up.
    pub fn publishable(self) -> bool {
        self != FrameFormat::Tiff
    }
}

/// The Export step's settings.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportSettings {
    pub folder: Option<PathBuf>,
    /// Frames are saved as `<prefix>_01`, `<prefix>_02`, ...
    pub prefix: String,
    pub format: FrameFormat,
    /// Write [`METADATA_FILE`] next to the frames.
    pub write_metadata: bool,
    pub metadata: RollMetadata,
    /// The film profile the film fields were last filled in from: a change of profile
    /// fills them in again.
    pub film_from: Option<&'static str>,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            folder: None,
            prefix: "roll".into(),
            format: FrameFormat::Jpeg(95),
            write_metadata: true,
            metadata: RollMetadata {
                date_scanned: today(),
                ..RollMetadata::default()
            },
            film_from: None,
        }
    }
}

impl ExportSettings {
    /// Why the file names can't start with the prefix: it has characters Windows doesn't
    /// allow in file names (or that would make it a path).
    pub fn prefix_problem(&self) -> Option<String> {
        let bad: String = self
            .prefix
            .chars()
            .filter(|c| r#"\/:*?"<>|"#.contains(*c) || c.is_control())
            .collect();
        (!bad.is_empty()).then(|| format!("File names can't contain {bad}"))
    }

    /// Where the `n`th frame (from 0) of `count` is saved: numbered from 01, with enough
    /// digits that the files sort in frame order, as the publisher uploads them.
    pub fn frame_path(&self, folder: &Path, n: usize, count: usize) -> PathBuf {
        let digits = count.to_string().len().max(2);
        let prefix = match self.prefix.trim() {
            "" => "frame",
            p => p,
        };
        folder.join(format!(
            "{prefix}_{:0digits$}.{}",
            n + 1,
            self.format.extension()
        ))
    }
}

/// A roll's metadata, as the publisher's format 0 has it. Everything is kept as typed, and
/// checked by [`RollMetadata::validate`] before it is written.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RollMetadata {
    pub name: String,
    pub description: String,
    /// MM/DD/YYYY.
    pub date_captured: String,
    /// MM/DD/YYYY; empty is left out.
    pub date_scanned: String,
    pub public: bool,
    pub gps_latitude: String,
    pub gps_longitude: String,

    pub manufacturer: String,
    pub stock: String,
    pub iso: String,
    pub development_notes: String,

    pub camera_make: String,
    pub camera_model: String,
    pub lens_make: String,
    pub lens_model: String,
    pub f_number: String,
    pub focal_length: String,
    pub flash: String,
    pub filters: String,
}

impl RollMetadata {
    /// The camera fields, by their names in the file: kept between sessions, since they
    /// change less often than the roll.
    pub fn camera_fields(&mut self) -> [(&'static str, &mut String); 8] {
        [
            ("camera_make", &mut self.camera_make),
            ("camera_model", &mut self.camera_model),
            ("lens_make", &mut self.lens_make),
            ("lens_model", &mut self.lens_model),
            ("f_number", &mut self.f_number),
            ("focal_length", &mut self.focal_length),
            ("flash", &mut self.flash),
            ("filters", &mut self.filters),
        ]
    }

    /// Fill in the film from `stock`'s profile. The publisher names films by stock and
    /// speed apart (`Kodak Gold 200`: stock `Gold`, speed 200), so a speed at the end of
    /// the profile's name is left off the stock.
    pub fn fill_film(&mut self, stock: &FilmStock) {
        let speed = format!(" {}", stock.iso);
        self.manufacturer = stock.manufacturer.into();
        self.stock = stock.name.strip_suffix(&speed).unwrap_or(stock.name).into();
        self.iso = stock.iso.to_string();
    }

    /// Every problem the publisher would refuse the file for, or mangle: none means it
    /// can be written.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut require = |value: &str, what: &str| {
            if value.trim().is_empty() {
                problems.push(format!("{what} is required"));
            }
        };
        require(&self.name, "Roll name");
        require(&self.date_captured, "Date captured");
        require(&self.manufacturer, "Film manufacturer");
        require(&self.stock, "Film stock");
        require(&self.iso, "ISO");
        require(&self.camera_make, "Camera make");
        require(&self.camera_model, "Camera model");

        for (value, what) in [
            (&self.date_captured, "Date captured"),
            (&self.date_scanned, "Date scanned"),
        ] {
            let value = value.trim();
            if !value.is_empty() && parse_date(value).is_none() {
                problems.push(format!("{what} \"{value}\" isn't a date like 07/29/2026"));
            }
        }
        let iso = self.iso.trim();
        if !iso.is_empty() && !iso.parse::<u32>().is_ok_and(|v| v > 0) {
            problems.push(format!("ISO \"{iso}\" isn't a whole number, like 200"));
        }
        if let Err(p) = single_value(&self.f_number, "F-number", &["f/", "f"], "", "f/8") {
            problems.push(p);
        }
        if let Err(p) = single_value(&self.focal_length, "Focal length", &[], "mm", "50mm") {
            problems.push(p);
        }
        match (self.gps_latitude.trim(), self.gps_longitude.trim()) {
            ("", "") => {}
            ("", _) | (_, "") => problems.push("GPS needs both latitude and longitude".into()),
            (lat, long) => {
                for (value, what, limit) in [(lat, "latitude", 90.0), (long, "longitude", 180.0)] {
                    if !value.parse::<f64>().is_ok_and(|v| v.abs() <= limit) {
                        problems.push(format!(
                            "GPS {what} \"{value}\" isn't a number from -{limit} to {limit}"
                        ));
                    }
                }
            }
        }
        problems
    }

    /// The publisher's `metadata.yml`. Text is quoted so YAML reads it back exactly as
    /// typed, and optional fields left empty are left out.
    pub fn to_yaml(&self) -> String {
        let mut out = String::from(
            "# Roll metadata for film_roll_publisher \
             (https://github.com/drewsum/film_roll_publisher),\n# written by Film Scanner.\n\
             format: 0\n",
        );
        let text = |out: &mut String, key: &str, value: &str, always: bool| {
            let value = value.trim();
            if always || !value.is_empty() {
                let _ = writeln!(out, "  {key}: {}", quoted(value));
            }
        };
        out.push_str("roll:\n");
        text(&mut out, "name", &self.name, true);
        // The publisher's album description starts with it, so it must be there.
        text(&mut out, "description", &self.description, true);
        text(&mut out, "date_captured", &self.date_captured, true);
        text(&mut out, "date_scanned", &self.date_scanned, false);
        let _ = writeln!(out, "  public: {}", self.public);
        if let (Ok(lat), Ok(long)) = (
            self.gps_latitude.trim().parse::<f64>(),
            self.gps_longitude.trim().parse::<f64>(),
        ) {
            let _ = writeln!(out, "  gps_latitude: {lat}\n  gps_longitude: {long}");
        }

        out.push_str("film:\n");
        text(&mut out, "manufacturer", &self.manufacturer, true);
        text(&mut out, "stock", &self.stock, true);
        let _ = writeln!(out, "  photographic_sensitivity: {}", self.iso.trim());
        text(
            &mut out,
            "development_notes",
            &self.development_notes,
            false,
        );

        out.push_str("camera:\n");
        for (key, value) in self.clone().camera_fields() {
            text(
                &mut out,
                key,
                value,
                key == "camera_make" || key == "camera_model",
            );
        }
        out
    }
}

/// Today's date in the publisher's format.
pub fn today() -> String {
    jiff::Zoned::now().date().strftime(DATE_FORMAT).to_string()
}

fn parse_date(value: &str) -> Option<Date> {
    Date::strptime(DATE_FORMAT, value).ok()
}

/// Check an optional single number like `f/8` or `50mm`: the publisher refuses ranges like
/// `f/3.5-5.6`, since EXIF holds one value.
fn single_value(
    value: &str,
    what: &str,
    prefixes: &[&str],
    suffix: &str,
    example: &str,
) -> Result<(), String> {
    let raw = value.trim();
    if raw.is_empty() {
        return Ok(());
    }
    if raw.contains(['~', '-']) {
        return Err(format!(
            "{what} \"{raw}\" is a range: enter one value (the widest), like {example}"
        ));
    }
    let lower = raw.to_lowercase();
    let mut number = lower.as_str();
    for prefix in prefixes {
        if let Some(rest) = number.strip_prefix(prefix) {
            number = rest;
            break;
        }
    }
    let number = number.strip_suffix(suffix).unwrap_or(number).trim();
    match number.parse::<f64>() {
        Ok(v) if v > 0.0 => Ok(()),
        _ => Err(format!("{what} \"{raw}\" isn't a number like {example}")),
    }
}

/// `value` as a double-quoted YAML string.
fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests;
