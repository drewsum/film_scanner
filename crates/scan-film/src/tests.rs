use crate::model::{FilmModel, GREY, MIN_GAMMA};
use crate::render::{Inverter, Printing, Rendering, Tone, auto_printing};
use crate::sheets::kodak_e7022::SHEET;
use crate::stock;

fn model() -> &'static FilmModel {
    stock::by_id("kodak-kodacolor-200").unwrap().model()
}

/// A negative's light per channel on a scan, for scene light `e`, whose bare base lets
/// `base` through.
fn scanned(model: &FilmModel, base: [f32; 3], e: [f32; 3]) -> [f32; 3] {
    let d = model.negative(e);
    std::array::from_fn(|c| base[c] * 10f32.powf(-d[c]))
}

#[test]
fn extracted_curves_agree_with_kodaks_exposure_check() {
    // E-7022, "Judging negative exposures": what a normally exposed grey card, and the
    // lightest step of a paper grey scale (about 90% reflectance, five times the card),
    // read through the Status M red filter.
    let checks = [
        ("kodak-kodacolor-200", 0.85..=1.05, 1.25..=1.45),
        ("kodak-gold-200", 0.85..=1.05, 1.25..=1.45),
        ("kodak-gold-100", 0.90..=1.10, 1.30..=1.50),
    ];
    for (id, card_range, white_range) in checks {
        let sheet = stock::by_id(id).unwrap().sheet;
        let red = &sheet.characteristic[0];
        let card = red.at(sheet.log_h_ref);
        let white = red.at(sheet.log_h_ref + (0.9f32 / GREY).log10());
        assert!(card_range.contains(&card), "{id}: grey card {card}");
        assert!(white_range.contains(&white), "{id}: paper white {white}");
    }
    assert_eq!(
        stock::all().len(),
        3,
        "a new stock needs its exposure check here"
    );
}

#[test]
fn layers_peak_where_their_colours_are() {
    for stock in stock::all() {
        let sheet = stock.sheet;
        let peak = |c: usize| {
            let s = &sheet.log_sensitivity[c];
            (0..s.values.len())
                .filter_map(|i| Some((s.values[i]?, s.start_nm + i as f32 * s.step_nm)))
                .max_by(|a, b| a.0.total_cmp(&b.0))
                .unwrap()
                .1
        };
        assert!((630.0..=670.0).contains(&peak(0)), "red {}", peak(0));
        assert!((530.0..=570.0).contains(&peak(1)), "green {}", peak(1));
        assert!((430.0..=480.0).contains(&peak(2)), "blue {}", peak(2));
        // D-min is the orange mask: blue densest, red least.
        let d = sheet.d_min();
        assert!(d[2] > d[1] && d[1] > d[0], "{}: {d:?}", stock.id);
    }
}

#[test]
fn kodacolor_uses_gold_200s_published_curves() {
    let kodacolor = stock::by_id("kodak-kodacolor-200").unwrap();
    let sheet = kodacolor.sheet;
    assert_eq!(
        (sheet.film, sheet.publication, sheet.edition),
        (SHEET.film, "E-7022", "February 2016")
    );
}

#[test]
fn layers_invert_everywhere() {
    for (stock, c) in stock::all()
        .iter()
        .flat_map(|s| (0..3).map(move |c| (s, c)))
    {
        let layer = stock.model().layer(c);
        let mut last = f32::NEG_INFINITY;
        for i in 0..=600 {
            let log_h = -5.0 + i as f32 * 0.01;
            let d = layer.density(log_h);
            assert!(
                d - last >= MIN_GAMMA * 0.01 * 0.99,
                "layer {c} flat at {log_h}"
            );
            last = d;
            let back = layer.log_exposure(d);
            assert!(
                (back - log_h).abs() < 1e-3,
                "layer {c}: {log_h} -> {d} -> {back}"
            );
        }
    }
}

#[test]
fn negatives_invert_to_the_scene() {
    let model = model();
    // Above the lowest exposure the datasheet plots (5 stops under a grey card or so).
    for e in [0.006, 0.02, 0.09, 0.18, 0.5, 0.9, 3.0, 20.0] {
        for c in 0..3 {
            let d = model.negative([e; 3])[c];
            let back = model.exposure(c, d);
            assert!((back / e - 1.0).abs() < 1e-3, "{e} -> {d} -> {back}");
        }
    }
    // A grey card is a midscale density, and more light is more density.
    let card = model.negative([GREY; 3]);
    assert!(card.iter().all(|&d| (0.5..0.9).contains(&d)), "{card:?}");
    let white = model.negative([0.9; 3]);
    assert!((0..3).all(|c| white[c] > card[c] + 0.25), "{white:?}");
}

#[test]
fn greys_print_grey_through_any_base() {
    // Every stock, with two scanners' worth of base colour and level: the base is divided
    // out.
    let bases = [[0.56, 0.22, 0.10], [0.9, 0.7, 0.4]];
    for (model, base) in stock::all()
        .iter()
        .flat_map(|s| bases.map(|b| (s.model(), b)))
    {
        let inverter = Inverter::new(model, base, &Printing::default());
        for e in [0.01, 0.05, GREY, 0.5, 0.9, 2.0] {
            let rgb = inverter.pixel(scanned(model, base, [e; 3]));
            let spread = rgb.iter().cloned().fold(f32::MIN, f32::max)
                - rgb.iter().cloned().fold(f32::MAX, f32::min);
            assert!(spread < 1e-3, "{e}: {rgb:?}");
        }
        // A grey card prints as sRGB 0.18: 118 of 255.
        let card = inverter.pixel(scanned(model, base, [GREY; 3]));
        assert!((card[1] * 255.0 - 118.0).abs() < 1.0, "{card:?}");
        // The bare base prints black, a highlight near white.
        assert!(inverter.pixel(base).iter().all(|&v| v < 0.01));
        let white = inverter.pixel(scanned(model, base, [4.0; 3]));
        assert!(white.iter().all(|&v| v > 0.9), "{white:?}");
    }
}

#[test]
fn printing_moves_exposure_and_colour() {
    let model = model();
    let base = [0.5, 0.2, 0.1];
    let card = scanned(model, base, [GREY; 3]);
    let plain = Inverter::new(model, base, &Printing::default());
    let brighter = Inverter::new(
        model,
        base,
        &Printing {
            exposure_ev: 1.0,
            ..Printing::default()
        },
    );
    // One stop more is twice the scene light.
    for (c, &light) in card.iter().enumerate() {
        let ratio = brighter.scene(c, light) / plain.scene(c, light);
        assert!((ratio - 2.0).abs() < 1e-3);
    }
    let warmer = Inverter::new(
        model,
        base,
        &Printing {
            balance_ev: [0.5, 0.0, -0.5],
            ..Printing::default()
        },
    );
    let rgb = warmer.pixel(card);
    assert!(rgb[0] > rgb[1] && rgb[1] > rgb[2], "{rgb:?}");
}

#[test]
fn colorimetric_matrix_keeps_white_and_is_plausible() {
    let m = model().colorimetric();
    for row in m {
        assert!((row.iter().sum::<f64>() - 1.0).abs() < 0.01, "{m:?}");
    }
    // Each layer feeds its own colour most.
    for (i, row) in m.iter().enumerate() {
        for (j, &v) in row.iter().enumerate() {
            if i != j {
                assert!(row[i] > v.abs(), "{m:?}");
            }
        }
    }
    let base = [0.5, 0.2, 0.1];
    let inverter = Inverter::new(
        model(),
        base,
        &Printing {
            rendering: Rendering::Colorimetric,
            ..Printing::default()
        },
    );
    let grey = inverter.pixel(scanned(model(), base, [GREY; 3]));
    assert!((grey[0] - grey[2]).abs() < 0.01, "{grey:?}");
}

#[test]
fn tone_keeps_grey_and_climbs() {
    for contrast in [0.8, 1.4, 2.2] {
        let tone = Tone::new(contrast);
        assert!((tone.apply(GREY) - GREY).abs() < 1e-5);
        let mut last = 0.0;
        for i in 1..200 {
            let v = tone.apply(i as f32 * 0.05);
            assert!(v > last && v < 1.0);
            last = v;
        }
    }
}

#[test]
fn auto_printing_takes_the_rolls_typical_frame() {
    let log = |e: [f32; 3]| e.map(f32::log10);
    // A roll shot a stop under, under tungsten-ish light (red up, blue down), with one
    // frame of green lawn.
    let cast = [1.4, 1.0, 0.6];
    let mut frames: Vec<[f32; 3]> = (0..5)
        .map(|i| log(cast.map(|c| c * 0.09 * (1.0 + 0.05 * i as f32))))
        .collect();
    frames.push(log([0.05, 0.2, 0.04]));
    let printing = auto_printing(&frames, &Printing::default());
    let balanced: [f32; 3] = std::array::from_fn(|c| {
        cast[c] * 0.09 * 2f32.powf(printing.exposure_ev + printing.balance_ev[c])
    });
    let spread = balanced[0] / balanced[2];
    assert!((spread - 1.0).abs() < 0.03, "{printing:?} {balanced:?}");
    assert!((printing.exposure_ev - 1.0).abs() < 0.2, "{printing:?}");
    assert_eq!(
        auto_printing(&[], &Printing::default()),
        Printing::default()
    );
}

#[test]
fn stocks_are_found_by_id() {
    let stock = stock::default_stock();
    assert_eq!(stock.display_name(), "Kodak Kodacolor 200");
    assert!(stock::by_id(stock.id).is_some());
    assert!(stock::by_id("nonesuch").is_none());
}
