//! Graphics in tooltips (#27): a tooltip about something easier to see than to read (a
//! levels curve, sharpening's overshoot, the four frame orders) shows a small drawing of it
//! under its text.
//!
//! The drawings live in one place, `assets/graphics/`, as SVG templates built into the
//! program. Colours in a template are tokens filled from the GUI's visuals when the tooltip
//! shows, so a drawing matches the light or dark theme, on a background a shade darker than
//! the tooltip's:
//!
//! * `{bg}` the background, `{fg}` text and lines, `{muted}` axes and guides, `{grid}` faint
//!   fills, `{accent}` emphasis, `{warn}` a warning tint.
//! * `{red}`, `{green}`, `{blue}`, `{cyan}`, `{magenta}`, `{yellow}`, `{amber}`: colours that
//!   are what the drawing shows (a balance slider's ends), the same in both themes.
//! * Any other token names a part of the drawing a tooltip can point at: drawn in `{accent}`
//!   when the tooltip names it in `highlight`, else in `{fg}`. One drawing serves several
//!   controls that way (levels' black, midtones and white).
//!
//! egui_extras' SVG loader renders a drawing at the screen's pixels per point, once per
//! theme and highlight. A drawing that can't be found or sized is left out: the tooltip is
//! its text alone.

use std::hash::{Hash, Hasher};

use eframe::egui::{self, Color32, Response, Ui, Vec2, Visuals, WidgetText};

/// Every drawing, by name: `assets/graphics/<name>.svg`.
pub const DRAWINGS: &[(&str, &str)] = &[
    ("balance", include_str!("../assets/graphics/balance.svg")),
    ("border", include_str!("../assets/graphics/border.svg")),
    (
        "brightness",
        include_str!("../assets/graphics/brightness.svg"),
    ),
    ("clarity", include_str!("../assets/graphics/clarity.svg")),
    ("clipping", include_str!("../assets/graphics/clipping.svg")),
    ("compare", include_str!("../assets/graphics/compare.svg")),
    ("contrast", include_str!("../assets/graphics/contrast.svg")),
    ("dust", include_str!("../assets/graphics/dust.svg")),
    ("falloff", include_str!("../assets/graphics/falloff.svg")),
    (
        "film_base",
        include_str!("../assets/graphics/film_base.svg"),
    ),
    (
        "frame_order",
        include_str!("../assets/graphics/frame_order.svg"),
    ),
    ("heal", include_str!("../assets/graphics/heal.svg")),
    (
        "hue_shift",
        include_str!("../assets/graphics/hue_shift.svg"),
    ),
    ("levels", include_str!("../assets/graphics/levels.svg")),
    (
        "multi_exposure",
        include_str!("../assets/graphics/multi_exposure.svg"),
    ),
    ("noise", include_str!("../assets/graphics/noise.svg")),
    ("sharpen", include_str!("../assets/graphics/sharpen.svg")),
    (
        "straighten",
        include_str!("../assets/graphics/straighten.svg"),
    ),
    (
        "tone_range",
        include_str!("../assets/graphics/tone_range.svg"),
    ),
    (
        "white_balance",
        include_str!("../assets/graphics/white_balance.svg"),
    ),
];

/// The SVG template `name`.
pub fn drawing(name: &str) -> Option<&'static str> {
    DRAWINGS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, svg)| *svg)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

fn hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// The tokens that aren't parts, from `visuals`.
pub fn palette(visuals: &Visuals) -> Vec<(&'static str, String)> {
    let window = visuals.window_fill;
    let bg = mix(
        window,
        Color32::BLACK,
        if visuals.dark_mode { 0.3 } else { 0.06 },
    );
    let fg = visuals.text_color();
    vec![
        ("bg", hex(bg)),
        ("fg", hex(fg)),
        ("muted", hex(mix(bg, fg, 0.55))),
        ("grid", hex(mix(bg, fg, 0.15))),
        ("accent", hex(visuals.hyperlink_color)),
        ("warn", hex(mix(bg, Color32::from_rgb(220, 90, 60), 0.35))),
        // As the histogram and curves draw the channels, and Clipping marks clipped pixels.
        ("red", hex(Color32::from_rgb(235, 80, 70))),
        ("green", hex(Color32::from_rgb(90, 200, 90))),
        ("blue", hex(Color32::from_rgb(90, 140, 245))),
        ("cyan", hex(Color32::from_rgb(60, 200, 210))),
        ("magenta", hex(Color32::from_rgb(215, 80, 190))),
        ("yellow", hex(Color32::from_rgb(230, 200, 50))),
        ("amber", hex(Color32::from_rgb(240, 160, 60))),
    ]
}

/// The tokens in `svg`, in order, each as often as it is used.
pub fn tokens(svg: &str) -> impl Iterator<Item = &str> {
    svg.split('{').skip(1).filter_map(|s| {
        let end = s.find('}')?;
        let key = &s[..end];
        key.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
            .then_some(key)
    })
}

/// `svg` with its colour tokens replaced.
pub fn fill(svg: &str, colors: &[(&str, String)], highlight: &[&str]) -> String {
    let accent = &colors
        .iter()
        .find(|(k, _)| *k == "accent")
        .expect("a base token")
        .1;
    let fg = &colors
        .iter()
        .find(|(k, _)| *k == "fg")
        .expect("a base token")
        .1;
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let key = after.find('}').map(|end| &after[..end]);
        match key.filter(|k| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
            Some(key) => {
                let color = colors
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, c)| c)
                    .unwrap_or(if highlight.contains(&key) { accent } else { fg });
                out.push_str(color);
                rest = &after[key.len() + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The size `svg` is drawn at, from the `width` and `height` on its `<svg>` element.
pub fn size(svg: &str) -> Option<Vec2> {
    let start = svg.find("<svg")?;
    let tag = &svg[start..start + svg[start..].find('>')?];
    let attr = |name: &str| -> Option<f32> {
        let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
        tag[at..at + tag[at..].find('"')?].parse().ok()
    };
    Some(Vec2::new(attr("width")?, attr("height")?))
}

/// The tooltip: `text`, with the drawing `name` under it, the parts named in `highlight`
/// picked out.
fn show(ui: &mut Ui, text: WidgetText, name: &str, highlight: &[&str]) {
    let Some((svg, size)) = drawing(name).and_then(|svg| Some((svg, size(svg)?))) else {
        debug_assert!(false, "no tooltip drawing {name}");
        ui.label(text);
        return;
    };
    debug_assert!(
        highlight.iter().all(|p| tokens(svg).any(|t| t == *p)),
        "tooltip drawing {name} has no part among {highlight:?}"
    );
    ui.set_max_width(ui.spacing().tooltip_width.max(size.x));
    ui.label(text);
    let svg = fill(svg, &palette(ui.visuals()), highlight);
    // A new name for each theme and highlight, so each is rendered once and kept.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    svg.hash(&mut hasher);
    let uri = format!("bytes://tooltip-art/{name}-{:016x}.svg", hasher.finish());
    ui.add_space(4.0);
    ui.add(egui::Image::from_bytes(uri, svg.into_bytes()).fit_to_exact_size(size));
}

/// [`Response::on_hover_text`], with a drawing.
pub trait ArtTip {
    /// A tooltip of `text` with the drawing `name` under it, the parts named in `highlight`
    /// drawn in the accent colour.
    fn on_hover_art(self, text: impl Into<WidgetText>, name: &str, highlight: &[&str]) -> Self;
}

impl ArtTip for Response {
    fn on_hover_art(self, text: impl Into<WidgetText>, name: &str, highlight: &[&str]) -> Self {
        let text = text.into();
        self.on_hover_ui(|ui| show(ui, text, name, highlight))
    }
}

/// A tooltip for a helper that makes the control: its text, and maybe a drawing.
#[derive(Clone, Copy)]
pub struct Tip<'a> {
    text: &'a str,
    art: Option<(&'a str, &'a [&'a str])>,
}

/// A tooltip of `text` with the drawing `name`, the parts named in `highlight` lit.
pub fn art<'a>(text: &'a str, name: &'a str, highlight: &'a [&'a str]) -> Tip<'a> {
    Tip {
        text,
        art: Some((name, highlight)),
    }
}

impl<'a> From<&'a str> for Tip<'a> {
    fn from(text: &'a str) -> Self {
        Tip { text, art: None }
    }
}

impl Tip<'_> {
    /// `response` with this tooltip.
    pub fn on(self, response: Response) -> Response {
        match self.art {
            Some((name, highlight)) => response.on_hover_art(self.text, name, highlight),
            None => response.on_hover_text(self.text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &[&str] = &[
        "bg", "fg", "muted", "grid", "accent", "warn", "red", "green", "blue", "cyan", "magenta",
        "yellow", "amber",
    ];

    #[test]
    fn every_drawing_is_listed_and_every_listed_one_exists() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/graphics");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                (p.extension()? == "svg").then(|| p.file_stem().unwrap().to_string_lossy().into())
            })
            .collect();
        on_disk.sort();
        let listed: Vec<&str> = DRAWINGS.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            on_disk, listed,
            "DRAWINGS is assets/graphics, in name order"
        );
    }

    #[test]
    fn every_drawing_fills_sizes_and_renders() {
        let colors = palette(&Visuals::dark());
        for (name, svg) in DRAWINGS {
            let size = size(svg).unwrap_or_else(|| panic!("{name} has no width and height"));
            assert!(
                (200.0..=340.0).contains(&size.x) && (90.0..=200.0).contains(&size.y),
                "{name} is {size:?}"
            );
            assert!(
                svg.contains("font-family=\"sans-serif\""),
                "{name}: sans-serif text"
            );
            let parts: Vec<&str> = tokens(svg).filter(|t| !BASE.contains(t)).collect();
            let filled = fill(svg, &colors, &parts);
            assert!(tokens(&filled).next().is_none(), "{name} has tokens left");
            let image = egui_extras::image::load_svg_bytes_with_size(
                filled.as_bytes(),
                egui::load::SizeHint::Scale(2.0.into()),
                &resvg::usvg::Options::default(),
            )
            .unwrap_or_else(|e| panic!("{name} doesn't render: {e}"));
            assert_eq!(
                image.size,
                [size.x as usize * 2, size.y as usize * 2],
                "{name}"
            );
        }
    }

    #[test]
    fn parts_are_lit_only_when_highlighted() {
        let colors = palette(&Visuals::light());
        let get = |k: &str| colors.iter().find(|(n, _)| *n == k).unwrap().1.clone();
        let svg = r#"<a fill="{bg}"/><b fill="{black}"/><c fill="{white}"/>{ not a token }"#;
        let filled = fill(svg, &colors, &["white"]);
        assert_eq!(
            filled,
            format!(
                r#"<a fill="{}"/><b fill="{}"/><c fill="{}"/>{{ not a token }}"#,
                get("bg"),
                get("fg"),
                get("accent")
            )
        );
    }

    #[test]
    fn the_themes_differ() {
        let dark = palette(&Visuals::dark());
        let light = palette(&Visuals::light());
        for key in ["bg", "fg", "muted", "grid"] {
            let d = &dark.iter().find(|(k, _)| *k == key).unwrap().1;
            let l = &light.iter().find(|(k, _)| *k == key).unwrap().1;
            assert_ne!(d, l, "{key}");
        }
    }

    /// Renders every drawing, in both themes with its first part lit, to PNGs in
    /// `TOOLTIP_ART_OUT` to look at: `cargo test -p film-scanner preview -- --ignored`.
    #[test]
    #[ignore = "writes PNGs to look at"]
    fn preview() {
        let out = std::path::PathBuf::from(std::env::var("TOOLTIP_ART_OUT").unwrap());
        let mut options = resvg::usvg::Options::default();
        options.fontdb_mut().load_system_fonts();
        for (theme, visuals) in [("dark", Visuals::dark()), ("light", Visuals::light())] {
            let colors = palette(&visuals);
            for (name, svg) in DRAWINGS {
                let first: Vec<&str> = tokens(svg).filter(|t| !BASE.contains(t)).take(1).collect();
                let filled = fill(svg, &colors, &first);
                let image = egui_extras::image::load_svg_bytes_with_size(
                    filled.as_bytes(),
                    egui::load::SizeHint::Scale(2.0.into()),
                    &options,
                )
                .unwrap();
                let [w, h] = image.size;
                let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                scan_core::image::RgbaImage::from_raw(w as u32, h as u32, rgba)
                    .unwrap()
                    .save(out.join(format!("{name}-{theme}.png")))
                    .unwrap();
            }
        }
    }

    #[test]
    fn every_drawing_is_used_by_a_tooltip() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let code: String = ["ui.rs", "edit.rs"]
            .iter()
            .map(|f| std::fs::read_to_string(src.join(f)).unwrap())
            .collect();
        for (name, _) in DRAWINGS {
            assert!(
                code.contains(&format!("\"{name}\"")),
                "no tooltip shows the drawing {name}"
            );
        }
    }
}
