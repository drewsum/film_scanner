//! Film stocks' characteristic curves (#27), drawn from their datasheets: in the film stock
//! lists' tooltips, and in the Invert step's sidebar for the stock chosen.
//!
//! The curves are normalized so stocks compare: density above each layer's D-min (so a
//! color negative's orange mask doesn't lift its curves apart), against exposure in stops
//! from the datasheet's Log H ref, where a grey card exposed at the film's speed lands. The
//! axes are the same for every stock, wide enough for all of them.
//!
//! The drawing is an SVG template like the other tooltip drawings ([`crate::tooltip_art`]),
//! with the same colour tokens, so it matches the theme.

use std::fmt::Write as _;
use std::sync::OnceLock;

use eframe::egui::{self, Response, Ui, WidgetText};
use scan_film::sheet::Curve;
use scan_film::stock::{self, FilmStock, MonoStock};

use crate::tooltip_art;

const WIDTH: f32 = 300.0;
const HEIGHT: f32 = 180.0;
/// The plot's left, top, right and bottom edges.
const PLOT: [f32; 4] = [44.0, 12.0, 290.0, 136.0];
const LOG2: f32 = std::f32::consts::LOG10_2;

/// A stock with datasheet curves.
#[derive(Clone, Copy)]
pub enum Stock {
    Color(&'static FilmStock),
    Mono(&'static MonoStock),
}

impl Stock {
    fn id(self) -> &'static str {
        match self {
            Stock::Color(s) => s.id,
            Stock::Mono(s) => s.id,
        }
    }

    /// Each curve, with its colour token, and the Log H ref they are measured from. A sheet
    /// that gives one curve for all three layers (as Harman Phoenix's does) has it drawn
    /// once, as a black-and-white film's is.
    fn curves(self) -> (Vec<(&'static Curve, &'static str)>, f32) {
        match self {
            Stock::Color(s) => {
                let [r, g, b] = &s.sheet.characteristic;
                let curves = if r.values == g.values && g.values == b.values {
                    vec![(r, "fg")]
                } else {
                    vec![(r, "red"), (g, "green"), (b, "blue")]
                };
                (curves, s.sheet.log_h_ref)
            }
            Stock::Mono(s) => (vec![(&s.sheet.characteristic, "fg")], s.sheet.log_h_ref),
        }
    }
}

/// Every stock.
fn every() -> impl Iterator<Item = Stock> {
    let color = stock::all().iter().map(|&s| Stock::Color(s));
    color.chain(stock::mono_all().iter().map(|&s| Stock::Mono(s)))
}

/// The axes every stock is drawn on: log exposure from Log H ref, and density above D-min,
/// each rounded out to a half.
fn axes() -> &'static ([f32; 2], [f32; 2]) {
    static AXES: OnceLock<([f32; 2], [f32; 2])> = OnceLock::new();
    AXES.get_or_init(|| {
        let (mut x, mut top) = ([0.0f32, 0.0f32], 0.0f32);
        for s in every() {
            let (curves, log_h_ref) = s.curves();
            for (c, _) in curves {
                x[0] = x[0].min(c.start - log_h_ref);
                x[1] = x[1].max(c.end() - log_h_ref);
                let d_min = c.values[0];
                top = c.values.iter().fold(top, |t, v| t.max(v - d_min));
            }
        }
        let out = |v: f32, f: fn(f32) -> f32| f(v * 2.0) / 2.0;
        (
            [out(x[0], f32::floor), out(x[1], f32::ceil)],
            [0.0, out(top, f32::ceil)],
        )
    })
}

/// The SVG template of `stock`'s curves.
pub fn svg(stock: Stock) -> String {
    let ([x0, x1], [y0, y1]) = *axes();
    let [left, top, right, bottom] = PLOT;
    let px = |log_h: f32| left + (log_h - x0) / (x1 - x0) * (right - left);
    let py = |d: f32| bottom - (d - y0) / (y1 - y0) * (bottom - top);
    let mut s = String::new();
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" viewBox="0 0 {WIDTH} {HEIGHT}">
  <defs><clipPath id="plot"><rect x="{left}" y="{top}" width="{w}" height="{h}"/></clipPath></defs>
  <rect x="0" y="0" width="{WIDTH}" height="{HEIGHT}" rx="8" fill="{{bg}}"/>
  <g font-family="sans-serif" font-size="10">
    <rect x="{left}" y="{top}" width="{w}" height="{h}" fill="{{grid}}"/>
"#,
        w = right - left,
        h = bottom - top,
    );
    // Density every half, exposure every two stops.
    let mut d = y0;
    while d <= y1 + 1e-3 {
        let y = py(d);
        let _ = writeln!(
            s,
            r#"    <path d="M{left},{y:.1} H{right}" stroke="{{bg}}" stroke-width="1"/><text x="{tx}" y="{ty:.1}" fill="{{muted}}" text-anchor="end">{d:.1}</text>"#,
            tx = left - 4.0,
            ty = y + 3.5,
        );
        d += 0.5;
    }
    let first = (x0 / LOG2 / 2.0).ceil() as i32 * 2;
    let mut stops = first;
    while stops as f32 * LOG2 <= x1 + 1e-3 {
        let x = px(stops as f32 * LOG2);
        let dash = if stops == 0 {
            r#" stroke-dasharray="4,3" stroke="{muted}""#
        } else {
            r#" stroke="{bg}""#
        };
        let _ = writeln!(
            s,
            r#"    <path d="M{x:.1},{top} V{bottom}"{dash} stroke-width="1"/><text x="{x:.1}" y="{ty}" fill="{{muted}}" text-anchor="middle">{label}</text>"#,
            label = if stops == 0 {
                "0".to_owned()
            } else {
                format!("{stops:+}")
            },
            ty = bottom + 12.0,
        );
        stops += 2;
    }
    let (curves, log_h_ref) = stock.curves();
    let mut d_mins = Vec::new();
    s.push_str("    <g clip-path=\"url(#plot)\" fill=\"none\" stroke-width=\"2\">\n");
    for (curve, color) in &curves {
        let d_min = curve.values[0];
        d_mins.push(format!("{d_min:.2}"));
        let mut path = String::new();
        for (i, v) in curve.values.iter().enumerate() {
            let cmd = if i == 0 { 'M' } else { 'L' };
            let _ = write!(
                path,
                "{cmd}{:.1},{:.1} ",
                px(curve.x(i) - log_h_ref),
                py(v - d_min)
            );
        }
        let _ = writeln!(
            s,
            r#"      <path d="{}" stroke="{{{color}}}"/>"#,
            path.trim_end()
        );
    }
    s.push_str("    </g>\n");
    let _ = write!(
        s,
        r#"    <text x="{tx}" y="{ty}" fill="{{fg}}" font-size="11">D-min {}</text>
    <text x="{cx}" y="{cy}" fill="{{muted}}" text-anchor="middle">stops from a grey card's exposure</text>
    <text x="12" y="{my}" fill="{{muted}}" text-anchor="middle" transform="rotate(-90 12 {my})">density above D-min</text>
  </g>
</svg>
"#,
        d_mins.join(" / "),
        tx = left + 6.0,
        ty = top + 14.0,
        cx = (left + right) / 2.0,
        cy = HEIGHT - 8.0,
        my = (top + bottom) / 2.0,
    );
    s
}

/// The tooltip: `text`, with `stock`'s curves under it.
pub fn show(ui: &mut Ui, text: impl Into<WidgetText>, stock: Stock) {
    tooltip_art::show_svg(ui, text.into(), stock.id(), &svg(stock), &[]);
}

/// [`Response::on_hover_text`] of `text`, with `stock`'s curves.
pub fn on_hover(response: Response, text: &str, stock: Stock) -> Response {
    response.on_hover_ui(|ui| show(ui, text, stock))
}

/// `stock`'s curves, as wide as `ui` has room for, up to their own size.
pub fn panel(ui: &mut Ui, stock: Stock) {
    let width = ui.available_width().min(WIDTH);
    let size = egui::vec2(width, width * HEIGHT / WIDTH);
    let image = tooltip_art::image(ui, stock.id(), &svg(stock), &[]);
    ui.add(image.fit_to_exact_size(size));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_axes_hold_every_stock() {
        let ([x0, x1], [y0, y1]) = *axes();
        assert!(x0 < -1.5 && x1 > 1.5, "{x0}..{x1}");
        assert!(y0 == 0.0 && (1.5..4.0).contains(&y1), "{y0}..{y1}");
    }

    #[test]
    fn every_stock_draws_and_renders() {
        let colors = tooltip_art::palette(&egui::Visuals::dark());
        for stock in every() {
            let template = svg(stock);
            let size = tooltip_art::size(&template).unwrap();
            let filled = tooltip_art::fill(&template, &colors, &[]);
            assert!(
                tooltip_art::tokens(&filled).next().is_none(),
                "{}",
                stock.id()
            );
            let image = egui_extras::image::load_svg_bytes_with_size(
                filled.as_bytes(),
                egui::load::SizeHint::Scale(1.0.into()),
                &resvg::usvg::Options::default(),
            )
            .unwrap_or_else(|e| panic!("{} doesn't render: {e}", stock.id()));
            assert_eq!(image.size, [size.x as usize, size.y as usize]);
        }
    }

    #[test]
    fn a_color_stock_has_three_curves_a_mono_one_one() {
        let curves = |s: &str| s.matches("<path d=\"M").count();
        let grid = curves(&svg(Stock::Mono(stock::mono_all()[0])));
        let color = curves(&svg(Stock::Color(stock::default_stock())));
        assert_eq!(color, grid + 2);
    }

    /// Renders every stock's curves in both themes to PNGs in `TOOLTIP_ART_OUT`:
    /// `cargo test -p film-scanner stock_curves::tests::preview -- --ignored`.
    #[test]
    #[ignore = "writes PNGs to look at"]
    fn preview() {
        let out = std::path::PathBuf::from(std::env::var("TOOLTIP_ART_OUT").unwrap());
        let mut options = resvg::usvg::Options::default();
        options.fontdb_mut().load_system_fonts();
        for (theme, visuals) in [
            ("dark", egui::Visuals::dark()),
            ("light", egui::Visuals::light()),
        ] {
            let colors = tooltip_art::palette(&visuals);
            for stock in every() {
                let filled = tooltip_art::fill(&svg(stock), &colors, &[]);
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
                    .save(out.join(format!("stock-{}-{theme}.png", stock.id())))
                    .unwrap();
            }
        }
    }
}
