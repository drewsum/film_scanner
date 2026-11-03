//! Roll files: a scan's frames and every edit, saved beside the scan's TIFFs so a roll can
//! be opened again later, to change a frame and export it again.
//!
//! A roll file (`roll180.filmroll`) is RON text. Next to it the scan is kept as scanned, in
//! the same files as *Save as TIFF* writes (`roll180.tif`, `roll180_ev+1.0.tif`,
//! `roll180_ir.tif`): merging and dust removal are done again when it is opened.

use std::path::{Path, PathBuf};

use scan_core::image::DynamicImage;
use scan_core::{Exposure, ScanArea, ScanPass, ScanResult, ScanSource};
use scan_film::{Mono, Printing, Rendering, Toning};
use scan_process::FrameRect;
use scan_process::adjust::Adjustments;
use serde::{Deserialize, Serialize};

use crate::export::RollMetadata;
use crate::history::History;
use crate::state::{AppState, CropFrame, FilmBase, FilmType};
use crate::worker::ScanKind;

/// A roll file's extension.
pub const EXTENSION: &str = "filmroll";
/// The version of the file's contents this writes; files of later versions aren't read.
const FORMAT: u32 = 1;

/// Everything a roll file holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Roll {
    pub format: u32,
    pub scan: ScanFiles,
    #[serde(default)]
    pub film_base: Option<SavedBase>,
    #[serde(default)]
    pub frames: Vec<SavedFrame>,
    #[serde(default)]
    pub frames_edited: bool,
    #[serde(default)]
    pub strips: usize,
    #[serde(default)]
    pub reversed: bool,
    #[serde(default)]
    pub quarter_turns: u8,
    #[serde(default)]
    pub scan_adjust: Adjustments,
    pub invert: SavedInvert,
    #[serde(default = "yes")]
    pub merge_exposures: bool,
    #[serde(default = "yes")]
    pub remove_dust: bool,
    #[serde(default = "half")]
    pub dust_strength: f32,
    #[serde(default)]
    pub metadata: RollMetadata,
}

fn yes() -> bool {
    true
}

fn half() -> f32 {
    0.5
}

/// The scan's files, by name in the roll file's folder, and what the scan was.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScanFiles {
    pub primary: String,
    /// Each further exposure pass: its exposure, in stops, and file.
    #[serde(default)]
    pub passes: Vec<(f32, String)>,
    #[serde(default)]
    pub infrared: Option<String>,
    /// `film` or `flatbed`.
    pub source: String,
    pub dpi: u32,
    /// Left, top, width and height on the bed, in millimetres.
    pub area: [f32; 4],
    pub gamma: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedBase {
    pub light: [f32; 3],
    pub measured: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedFrame {
    pub rect: FrameRect,
    #[serde(default)]
    pub adjust: Adjustments,
    #[serde(default)]
    pub skip: bool,
    #[serde(default)]
    pub check: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedInvert {
    pub enabled: bool,
    /// A [`FilmType::key`].
    pub film_type: String,
    /// A film stock's id.
    pub stock: String,
    /// A black-and-white stock's id, empty for the generic curve.
    #[serde(default)]
    pub mono_stock: String,
    pub exposure_ev: f32,
    pub balance_ev: [f32; 3],
    pub contrast: f32,
    /// `print` or `colorimetric`.
    pub rendering: String,
    pub mono_mix: [f32; 3],
    /// A [`Toning::label`].
    pub toning: String,
    pub toning_amount: f32,
}

impl Roll {
    /// The roll file of `state`'s result, to be saved as `path`, its scan's files named
    /// after it as [`scan_core::export::save_tiff`] names them.
    pub fn of(state: &AppState, path: &Path) -> Option<Self> {
        let result = state.result.as_ref()?;
        let stem = path
            .file_stem()
            .map_or("roll".into(), |s| s.to_string_lossy().into_owned());
        let a = result.area;
        let invert = &state.invert;
        let p = &invert.printing;
        Some(Self {
            format: FORMAT,
            scan: ScanFiles {
                primary: format!("{stem}.tif"),
                passes: result.passes[1..]
                    .iter()
                    .map(|pass| {
                        let ev = pass.exposure.ev;
                        (ev, format!("{stem}_ev{ev:+.1}.tif"))
                    })
                    .collect(),
                infrared: result.infrared.as_ref().map(|_| format!("{stem}_ir.tif")),
                source: match result.source {
                    ScanSource::Transparency => "film",
                    ScanSource::Flatbed => "flatbed",
                }
                .into(),
                dpi: result.dpi,
                area: [a.x_mm, a.y_mm, a.width_mm, a.height_mm],
                gamma: result.gamma,
            },
            film_base: state.current_film_base().map(|b| SavedBase {
                light: b.light,
                measured: b.measured,
            }),
            frames: state
                .crop
                .frames
                .iter()
                .map(|f| SavedFrame {
                    rect: f.rect,
                    adjust: f.adjust.clone(),
                    skip: f.skip,
                    check: f.check,
                })
                .collect(),
            frames_edited: state.crop.edited,
            strips: state.crop.strips,
            reversed: state.crop.reversed,
            quarter_turns: state.quarter_turns,
            scan_adjust: state.scan_adjust.clone(),
            invert: SavedInvert {
                enabled: invert.enabled,
                film_type: invert.film_type.key().into(),
                stock: invert.stock.id.into(),
                mono_stock: invert.mono_stock.map_or("", |s| s.id).into(),
                exposure_ev: p.exposure_ev,
                balance_ev: p.balance_ev,
                contrast: p.contrast,
                rendering: match p.rendering {
                    Rendering::Print => "print",
                    Rendering::Colorimetric => "colorimetric",
                }
                .into(),
                mono_mix: invert.mono.mix,
                toning: invert.mono.toning.label().into(),
                toning_amount: invert.mono.toning_amount,
            },
            merge_exposures: state.merge_exposures,
            remove_dust: state.dust.enabled,
            dust_strength: state.dust.strength,
            metadata: state.export.metadata.clone(),
        })
    }

    pub fn to_text(&self) -> String {
        let config = ron::ser::PrettyConfig::new().depth_limit(4);
        ron::ser::to_string_pretty(self, config).expect("a roll is always serializable")
    }

    pub fn from_text(text: &str) -> Result<Self, String> {
        let roll: Roll = ron::from_str(text).map_err(|e| format!("not a roll file: {e}"))?;
        if roll.format > FORMAT {
            return Err(format!(
                "made by a newer Film Scanner (format {}, this reads {FORMAT})",
                roll.format
            ));
        }
        Ok(roll)
    }

    /// Read the roll file at `path` and the scan beside it.
    pub fn open(path: &Path) -> Result<(Self, ScanResult), String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let roll = Self::from_text(&text)?;
        let dir = path.parent().unwrap_or(Path::new("."));
        let load = |name: &str| -> Result<DynamicImage, String> {
            let file = dir.join(name);
            scan_core::export::open_scan(&file).map_err(|e| format!("{}: {e}", file.display()))
        };
        let s = &roll.scan;
        let mut passes = vec![ScanPass {
            exposure: Exposure::default(),
            image: load(&s.primary)?,
        }];
        for (ev, name) in &s.passes {
            passes.push(ScanPass {
                exposure: Exposure { ev: *ev },
                image: load(name)?,
            });
        }
        let size = (passes[0].image.width(), passes[0].image.height());
        if passes
            .iter()
            .any(|p| (p.image.width(), p.image.height()) != size)
        {
            return Err("the scan's passes aren't the same size".into());
        }
        let infrared = match &s.infrared {
            Some(name) => Some(load(name)?.to_luma16()),
            None => None,
        };
        let result = ScanResult {
            source: if s.source == "flatbed" {
                ScanSource::Flatbed
            } else {
                ScanSource::Transparency
            },
            dpi: s.dpi,
            area: ScanArea {
                x_mm: s.area[0],
                y_mm: s.area[1],
                width_mm: s.area[2],
                height_mm: s.area[3],
            },
            passes,
            infrared,
            gamma: s.gamma,
        };
        Ok((roll, result))
    }

    /// The scan's primary file, beside the roll file at `path`.
    pub fn scan_path(&self, path: &Path) -> PathBuf {
        path.with_file_name(&self.scan.primary)
    }

    /// Make `result` (this roll's scan) `state`'s, with this roll's frames and edits, as
    /// opened from `path`.
    pub fn restore(self, state: &mut AppState, result: ScanResult, path: PathBuf) {
        let generation = state.result_generation + 1;
        state.result = Some(std::sync::Arc::new(result));
        state.result_kind = Some(ScanKind::Final);
        state.result_generation = generation;
        state.merged = None;
        state.cleaned = None;
        state.film_base = self.film_base.map(|b| FilmBase {
            generation,
            light: b.light,
            measured: b.measured,
        });
        state.crop.frames = self
            .frames
            .into_iter()
            .map(|f| CropFrame {
                rect: f.rect,
                check: f.check,
                adjust: f.adjust,
                skip: f.skip,
                history: History::default(),
            })
            .collect();
        state.crop.selected = None;
        // The frames are kept as they were; the film base too, if it was known.
        state.crop.edited = self.frames_edited || state.film_base.is_none();
        state.crop.detected = state.film_base.map(|_| generation);
        state.crop.strips = self.strips;
        state.crop.reversed = self.reversed;
        state.quarter_turns = self.quarter_turns % 4;
        state.scan_adjust = self.scan_adjust;
        state.scan_history = History::default();
        let i = self.invert;
        let invert = &mut state.invert;
        invert.enabled = i.enabled;
        invert.film_type = FilmType::from_key(&i.film_type).unwrap_or_default();
        if let Some(stock) = scan_film::stock::by_id(&i.stock) {
            invert.stock = stock;
        }
        invert.mono_stock = scan_film::stock::mono_by_id(&i.mono_stock);
        invert.printing = Printing {
            exposure_ev: i.exposure_ev,
            balance_ev: i.balance_ev,
            contrast: i.contrast,
            rendering: if i.rendering == "colorimetric" {
                Rendering::Colorimetric
            } else {
                Rendering::Print
            },
        };
        invert.mono = Mono {
            mix: i.mono_mix,
            toning: Toning::ALL
                .into_iter()
                .find(|t| t.label() == i.toning)
                .unwrap_or_default(),
            toning_amount: i.toning_amount,
        };
        state.merge_exposures = self.merge_exposures;
        state.dust.enabled = self.remove_dust;
        state.dust.strength = self.dust_strength;
        state.export.metadata = self.metadata;
        state.export.film_from = state.invert.profile_film().map(|p| p.id);
        state.roll_file = Some((path, generation));
        state.forget_settled();
    }
}

#[cfg(test)]
mod tests;
