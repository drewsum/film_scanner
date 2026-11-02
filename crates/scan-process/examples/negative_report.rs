//! Measure a saved, uninverted film scan: its film base, and each frame's densities above
//! the base, to compare with a film stock's characteristic curves.
//!
//!     cargo run --release -p scan-process --example negative_report -- SCAN.tif [DPI] [GAMMA] [STOCK_ID]
//!
//! DPI defaults to 400 and GAMMA to 1.8 (the V500's encoding); the scan is taken to cover the
//! V500's film area from its corner. Densities are percentiles over each frame: the 1st is
//! the frame's thinnest film (its shadows), the 99th its densest (its highlights). For the
//! film's curves, a grey card normally exposed and a white surface sit at the densities the
//! `profile` example of `scan-film` prints for 0 and about +2.3 stops.

use scan_core::ScanArea;
use scan_core::image::{self, DynamicImage};
use scan_film::{Printing, stock};
use scan_process::{DetectParams, detect_frames, frame_levels};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args
        .first()
        .expect("usage: negative_report SCAN.tif [DPI] [GAMMA] [STOCK_ID]");
    let dpi: u32 = args.get(1).map_or(400, |a| a.parse().expect("DPI"));
    let gamma: f32 = args.get(2).map_or(1.8, |a| a.parse().expect("GAMMA"));
    let stock = args.get(3).map_or(stock::default_stock(), |id| {
        stock::by_id(id).expect("stock id")
    });
    let img = image::open(path).expect("readable image");
    let area = ScanArea::full(
        img.width() as f32 / dpi as f32 * scan_core::MM_PER_INCH,
        img.height() as f32 / dpi as f32 * scan_core::MM_PER_INCH,
    );
    let detection = detect_frames(&img, gamma, dpi, area, &DetectParams::default());
    let base = detection
        .base
        .expect("no film strips found to measure the base on");
    let model = stock.model();
    let d_min = model.d_min();
    println!("{path}: {} x {} px", img.width(), img.height());
    println!(
        "{} frames on {} strips; film base density {:.3} / {:.3} / {:.3}, datasheet D-min {:.3} / {:.3} / {:.3}",
        detection.frames.len(),
        detection.strips,
        -base[0].log10(),
        -base[1].log10(),
        -base[2].log10(),
        d_min[0],
        d_min[1],
        d_min[2]
    );

    let rgb = match &img {
        DynamicImage::ImageRgb16(i) => i.clone(),
        other => other.to_rgb16(),
    };
    let decode = |v: u16| (f32::from(v) / 65535.0).powf(gamma);
    let px_per_mm = dpi as f32 / scan_core::MM_PER_INCH;
    println!("\nDensity above base, red / green / blue, by percentile of each frame:");
    println!("frame        1%                 50%                99%");
    let mut all: [Vec<f32>; 3] = Default::default();
    for (n, frame) in detection.frames.iter().enumerate() {
        let r = frame.rect;
        let mut d: [Vec<f32>; 3] = Default::default();
        let step = 0.25;
        let (nu, nv) = ((r.width_mm / step) as i32, (r.height_mm / step) as i32);
        for j in 0..nv {
            for i in 0..nu {
                let (x, y) = r.from_local(
                    -r.width_mm / 2.0 + (i as f32 + 0.5) * step,
                    -r.height_mm / 2.0 + (j as f32 + 0.5) * step,
                );
                let (px, py) = ((x * px_per_mm) as u32, (y * px_per_mm) as u32);
                if px >= rgb.width() || py >= rgb.height() {
                    continue;
                }
                let p = rgb.get_pixel(px, py).0;
                for c in 0..3 {
                    let v = (base[c] / decode(p[c]).max(1e-6)).log10();
                    d[c].push(v);
                }
            }
        }
        for c in 0..3 {
            all[c].extend_from_slice(&d[c]);
        }
        println!("{:>4}   {}", n + 1, row(d));
    }
    println!("roll   {}", row(all));

    let card = model.negative([0.18; 3]);
    let white = model.negative([0.9; 3]);
    println!(
        "\n{}: a grey card exposed normally is {:.2} / {:.2} / {:.2} above base, a white surface {:.2} / {:.2} / {:.2}",
        stock.display_name(),
        card[0],
        card[1],
        card[2],
        white[0],
        white[1],
        white[2]
    );
    let rects: Vec<_> = detection.frames.iter().map(|f| f.rect).collect();
    let levels = frame_levels(&img, gamma, dpi, area, &rects, model, base);
    // A channel whose densities the scanner scales differently from Status M shows as a
    // colour offset that changes with the frame's level ("crossover"); offsets that don't
    // follow the level are the scenes' own colour.
    println!("\nEach frame at neutral printing: level (mean log10 scene light) and colour offsets");
    println!("frame   level    red  green   blue");
    for (n, l) in levels.iter().enumerate() {
        let mean = (l[0] + l[1] + l[2]) / 3.0;
        println!(
            "{:>4}   {:+.2}   {:+.3} {:+.3} {:+.3}",
            n + 1,
            mean,
            l[0] - mean,
            l[1] - mean,
            l[2] - mean
        );
    }
    if levels.len() >= 3 {
        let mean: Vec<f32> = levels.iter().map(|l| (l[0] + l[1] + l[2]) / 3.0).collect();
        let m = mean.iter().sum::<f32>() / mean.len() as f32;
        let var: f32 = mean.iter().map(|x| (x - m).powi(2)).sum();
        let slopes: Vec<f32> = (0..3)
            .map(|c| {
                let cov: f32 = levels
                    .iter()
                    .zip(&mean)
                    .map(|(l, x)| (x - m) * (l[c] - x))
                    .sum();
                cov / var
            })
            .collect();
        println!(
            "offset per unit of level: red {:+.3}, green {:+.3}, blue {:+.3} (0 = no crossover)",
            slopes[0], slopes[1], slopes[2]
        );
    }
    let auto = scan_film::auto_printing(&levels, &Printing::default());
    println!(
        "Auto printing: exposure {:+.2} EV, balance {:+.2} / {:+.2} / {:+.2}",
        auto.exposure_ev, auto.balance_ev[0], auto.balance_ev[1], auto.balance_ev[2]
    );
}

/// The 1st, 50th and 99th percentiles of each channel's densities.
fn row(mut d: [Vec<f32>; 3]) -> String {
    for v in &mut d {
        v.sort_by(f32::total_cmp);
    }
    let q = |v: &[f32], f: f32| match v.len() {
        0 => f32::NAN,
        n => v[((n - 1) as f32 * f).round() as usize],
    };
    [0.01, 0.5, 0.99]
        .map(|f| format!("{:.2} {:.2} {:.2}", q(&d[0], f), q(&d[1], f), q(&d[2], f)))
        .join("     ")
}
