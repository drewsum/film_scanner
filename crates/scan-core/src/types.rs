use std::fmt;

use image::{DynamicImage, ImageBuffer, Luma};

use crate::ScanError;

/// 16-bit single-channel image, used for infrared passes.
pub type Gray16Image = ImageBuffer<Luma<u16>, Vec<u16>>;

pub const MM_PER_INCH: f32 = 25.4;

/// Backend-specific, stable identifier for a device.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeviceId(pub String);

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A device as discovered by a backend, before it is opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub manufacturer: String,
    pub model: String,
    /// Name of the backend that discovered this device (see [`crate::ScannerBackend::name`]).
    pub backend: &'static str,
}

impl DeviceInfo {
    pub fn display_name(&self) -> String {
        if self.manufacturer.is_empty() || self.model.starts_with(&self.manufacturer) {
            format!("{} ({})", self.model, self.backend)
        } else {
            format!("{} {} ({})", self.manufacturer, self.model, self.backend)
        }
    }
}

/// Where the scanner reads from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanSource {
    /// Reflective scanning of prints/documents on the glass.
    Flatbed,
    /// Transmissive scanning through the lid light / transparency unit (film).
    Transparency,
}

impl ScanSource {
    pub fn label(self) -> &'static str {
        match self {
            ScanSource::Flatbed => "Flatbed (reflective)",
            ScanSource::Transparency => "Film (transparency unit)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorMode {
    Color,
    Gray,
}

/// Bits per channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BitDepth {
    Eight,
    Sixteen,
}

impl BitDepth {
    pub fn bits(self) -> u32 {
        match self {
            BitDepth::Eight => 8,
            BitDepth::Sixteen => 16,
        }
    }
}

/// A rectangle on the scan bed, in millimetres from the top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScanArea {
    pub x_mm: f32,
    pub y_mm: f32,
    pub width_mm: f32,
    pub height_mm: f32,
}

impl ScanArea {
    pub fn full(width_mm: f32, height_mm: f32) -> Self {
        Self {
            x_mm: 0.0,
            y_mm: 0.0,
            width_mm,
            height_mm,
        }
    }

    /// Size in pixels at the given resolution (at least 1x1).
    pub fn pixel_size(&self, dpi: u32) -> (u32, u32) {
        let px = |mm: f32| ((mm / MM_PER_INCH * dpi as f32).round() as u32).max(1);
        (px(self.width_mm), px(self.height_mm))
    }
}

/// What a device can do for one particular source.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceCapabilities {
    pub source: ScanSource,
    /// Supported optical resolutions in DPI, ascending.
    pub resolutions: Vec<u32>,
    pub bit_depths: Vec<BitDepth>,
    pub color_modes: Vec<ColorMode>,
    /// Whether an infrared channel (for dust/scratch removal) can be captured.
    pub infrared: bool,
    /// Maximum number of exposure passes per scan (1 = no multi-exposure).
    pub max_exposure_passes: usize,
    /// Full scannable area for this source.
    pub max_area: ScanArea,
}

impl SourceCapabilities {
    /// The lowest resolution at or above `min_dpi`, or the highest available.
    pub fn preview_resolution(&self, min_dpi: u32) -> Option<u32> {
        self.resolutions
            .iter()
            .copied()
            .find(|&r| r >= min_dpi)
            .or_else(|| self.resolutions.last().copied())
    }
}

/// All capabilities of an opened device.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Capabilities {
    pub sources: Vec<SourceCapabilities>,
}

impl Capabilities {
    pub fn source(&self, source: ScanSource) -> Option<&SourceCapabilities> {
        self.sources.iter().find(|s| s.source == source)
    }
}

/// One exposure pass of a (possibly multi-exposure) scan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Exposure {
    /// Exposure offset in stops relative to the device's normal exposure.
    pub ev: f32,
}

impl Default for Exposure {
    fn default() -> Self {
        Self { ev: 0.0 }
    }
}

/// Everything needed to perform a scan.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanRequest {
    pub source: ScanSource,
    pub dpi: u32,
    pub bit_depth: BitDepth,
    pub color_mode: ColorMode,
    /// `None` scans the full area of the source.
    pub area: Option<ScanArea>,
    /// Capture an additional infrared pass.
    pub infrared: bool,
    /// Visible-light exposure passes; an empty list means one normal exposure.
    pub exposures: Vec<Exposure>,
}

impl ScanRequest {
    pub fn new(source: ScanSource, dpi: u32) -> Self {
        Self {
            source,
            dpi,
            bit_depth: BitDepth::Sixteen,
            color_mode: ColorMode::Color,
            area: None,
            infrared: false,
            exposures: Vec::new(),
        }
    }

    /// Exposure passes to perform, never empty.
    pub fn effective_exposures(&self) -> Vec<Exposure> {
        if self.exposures.is_empty() {
            vec![Exposure::default()]
        } else {
            self.exposures.clone()
        }
    }

    /// Check the request against a device's capabilities, returning the source capabilities
    /// and resolved scan area on success.
    pub fn validate<'c>(
        &self,
        caps: &'c Capabilities,
    ) -> Result<(&'c SourceCapabilities, ScanArea), ScanError> {
        let invalid = |msg: String| Err(ScanError::InvalidRequest(msg));
        let Some(src) = caps.source(self.source) else {
            return invalid(format!("source {:?} not available", self.source));
        };
        if !src.resolutions.contains(&self.dpi) {
            return invalid(format!(
                "{} dpi not supported for {:?}",
                self.dpi, self.source
            ));
        }
        if !src.bit_depths.contains(&self.bit_depth) {
            return invalid(format!("{}-bit not supported", self.bit_depth.bits()));
        }
        if !src.color_modes.contains(&self.color_mode) {
            return invalid(format!("color mode {:?} not supported", self.color_mode));
        }
        if self.infrared && !src.infrared {
            return invalid("infrared not supported by this source".into());
        }
        let passes = self.effective_exposures().len();
        if passes > src.max_exposure_passes {
            return invalid(format!(
                "{passes} exposure passes requested, device supports {}",
                src.max_exposure_passes
            ));
        }
        let area = self.area.unwrap_or(src.max_area);
        let max = src.max_area;
        let eps = 0.01;
        if area.width_mm <= 0.0
            || area.height_mm <= 0.0
            || area.x_mm < 0.0
            || area.y_mm < 0.0
            || area.x_mm + area.width_mm > max.width_mm + eps
            || area.y_mm + area.height_mm > max.height_mm + eps
        {
            return invalid(format!("scan area {area:?} outside {max:?}"));
        }
        Ok((src, area))
    }
}

/// The visible-light image captured by one exposure pass.
#[derive(Clone, Debug)]
pub struct ScanPass {
    pub exposure: Exposure,
    /// `ImageRgb16`/`ImageLuma16` for 16-bit scans, `ImageRgb8`/`ImageLuma8` for 8-bit.
    pub image: DynamicImage,
}

/// The output of a scan.
#[derive(Clone, Debug)]
pub struct ScanResult {
    pub source: ScanSource,
    pub dpi: u32,
    pub area: ScanArea,
    pub passes: Vec<ScanPass>,
    pub infrared: Option<Gray16Image>,
}

impl ScanResult {
    /// The first (normal exposure) pass.
    pub fn primary(&self) -> &DynamicImage {
        &self.passes[0].image
    }

    pub fn dimensions(&self) -> (u32, u32) {
        let img = self.primary();
        (img.width(), img.height())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Capabilities {
        Capabilities {
            sources: vec![SourceCapabilities {
                source: ScanSource::Transparency,
                resolutions: vec![300, 600, 1200],
                bit_depths: vec![BitDepth::Eight, BitDepth::Sixteen],
                color_modes: vec![ColorMode::Color],
                infrared: false,
                max_exposure_passes: 1,
                max_area: ScanArea::full(50.0, 100.0),
            }],
        }
    }

    #[test]
    fn valid_request_resolves_full_area() {
        let req = ScanRequest::new(ScanSource::Transparency, 600);
        let (_, area) = req.validate(&caps()).unwrap();
        assert_eq!(area, ScanArea::full(50.0, 100.0));
    }

    #[test]
    fn rejects_unsupported_settings() {
        let caps = caps();
        let base = ScanRequest::new(ScanSource::Transparency, 600);
        let cases = [
            ScanRequest {
                source: ScanSource::Flatbed,
                ..base.clone()
            },
            ScanRequest {
                dpi: 601,
                ..base.clone()
            },
            ScanRequest {
                color_mode: ColorMode::Gray,
                ..base.clone()
            },
            ScanRequest {
                infrared: true,
                ..base.clone()
            },
            ScanRequest {
                exposures: vec![Exposure::default(); 2],
                ..base.clone()
            },
            ScanRequest {
                area: Some(ScanArea {
                    x_mm: 10.0,
                    ..ScanArea::full(45.0, 10.0)
                }),
                ..base
            },
        ];
        for req in cases {
            assert!(
                matches!(req.validate(&caps), Err(ScanError::InvalidRequest(_))),
                "expected rejection: {req:?}"
            );
        }
    }

    #[test]
    fn pixel_size_rounds_from_mm() {
        let area = ScanArea::full(25.4, 50.8);
        assert_eq!(area.pixel_size(300), (300, 600));
        assert_eq!(ScanArea::full(0.0001, 0.0001).pixel_size(300), (1, 1));
    }

    #[test]
    fn preview_resolution_picks_lowest_suitable() {
        let caps = caps();
        let src = caps.source(ScanSource::Transparency).unwrap();
        assert_eq!(src.preview_resolution(200), Some(300));
        assert_eq!(src.preview_resolution(5000), Some(1200));
    }

    #[test]
    fn display_name_avoids_duplicate_manufacturer() {
        let mut info = DeviceInfo {
            id: DeviceId("x".into()),
            manufacturer: "EPSON".into(),
            model: "EPSON Perfection V500".into(),
            backend: "wia",
        };
        assert_eq!(info.display_name(), "EPSON Perfection V500 (wia)");
        info.model = "Perfection V500".into();
        assert_eq!(info.display_name(), "EPSON Perfection V500 (wia)");
    }
}
