//! Print what a film stock's profile is made of, to check a new or changed profile:
//!
//!     cargo run -p scan-film --example profile [STOCK_ID]

use scan_film::model::{GREY, sensitivities, training_surfaces};
use scan_film::spectral::{self, SAMPLES, wavelengths};
use scan_film::stock;

fn main() {
    let id = std::env::args().nth(1);
    let stock = match &id {
        Some(id) => stock::by_id(id).unwrap_or_else(|| {
            let ids: Vec<_> = stock::all().iter().map(|s| s.id).collect();
            panic!("no stock {id}; there are {ids:?}")
        }),
        None => stock::default_stock(),
    };
    let sheet = stock.sheet;
    let model = stock.model();
    println!("{} ({})", stock.display_name(), stock.id);
    println!(
        "  data: {}, publication {} ({})",
        sheet.film, sheet.publication, sheet.edition
    );
    println!("  Log H ref {:+.2}", sheet.log_h_ref);

    let names = ["red", "green", "blue"];
    let d_min = model.d_min();
    println!("\nTone, Status M density (above D-min) by exposure in stops from a grey card:");
    print!("  {:>6}", "stops");
    for n in names {
        print!(" {n:>7}");
    }
    println!();
    for stops in [-6, -4, -3, -2, -1, 0, 1, 2, 3, 4, 6] {
        let e = GREY * 2f32.powi(stops);
        let d = model.negative([e; 3]);
        print!("  {stops:>+6}");
        for v in d {
            print!(" {v:>7.3}");
        }
        println!();
    }
    print!("  D-min ");
    for v in d_min {
        print!(" {v:>7.3}");
    }
    println!();
    let gamma: Vec<f32> = (0..3)
        .map(|c| {
            let l = model.layer(c);
            (l.density(sheet.log_h_ref + 0.5) - l.density(sheet.log_h_ref - 0.5)) / 1.0
        })
        .collect();
    println!("  gamma over one log unit around Log H ref: {gamma:.3?}");

    let m = model.colorimetric();
    println!("\nColorimetric matrix, layer exposures (R, G, B) to linear sRGB:");
    for row in m {
        println!("  [{:>7.3} {:>7.3} {:>7.3}]", row[0], row[1], row[2]);
    }

    // How well the film's layers, with that matrix, match the observer over the surfaces.
    let sens = sensitivities(sheet);
    let light: Vec<f64> = wavelengths().map(spectral::daylight).collect();
    let white: [f64; 3] =
        std::array::from_fn(|c| (0..SAMPLES).map(|i| sens[c][i] * light[i]).sum());
    let mut white_power = [0.0; SAMPLES];
    white_power.copy_from_slice(&light);
    let white_xyz = spectral::xyz_of(&white_power);
    let to_srgb = spectral::mul(
        &spectral::XYZ_TO_SRGB,
        &spectral::bradford(white_xyz.map(|v| v / white_xyz[1]), spectral::D65),
    );
    let (mut err, mut n) = (0.0, 0);
    let mut worst = (0.0, 0);
    for (k, r) in training_surfaces().iter().enumerate() {
        let mut power = [0.0; SAMPLES];
        for i in 0..SAMPLES {
            power[i] = light[i] * r[i];
        }
        let e: [f64; 3] = std::array::from_fn(|c| {
            (0..SAMPLES).map(|i| sens[c][i] * power[i]).sum::<f64>() / white[c]
        });
        let target =
            spectral::mul_vec(&to_srgb, spectral::xyz_of(&power).map(|v| v / white_xyz[1]));
        let got = spectral::mul_vec(m, e);
        let d = (0..3)
            .map(|c| (got[c] - target[c]).powi(2))
            .sum::<f64>()
            .sqrt();
        err += d * d;
        n += 1;
        if d > worst.0 {
            worst = (d, k);
        }
    }
    println!(
        "  fit over {n} surfaces: rms error {:.4} (linear sRGB), worst {:.4} (surface {})",
        (err / n as f64).sqrt(),
        worst.0,
        worst.1
    );
    println!("\nProvenance: {}", stock.provenance);
}
