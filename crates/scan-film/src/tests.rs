use std::ops::RangeInclusive;

use crate::model::{FilmModel, GREY, MIN_GAMMA};
use crate::render::{
    BW_GAMMA, BW_GREY_DENSITY, Inverter, Mono, Printing, Rendering, Tone, Toning, auto_printing,
};
use crate::sheet::Datasheet;
use crate::sheets::kodak_e7022::SHEET;
use crate::sheets::{
    generic_c41, kodak_e4046_ektar_100, kodak_e4050_portra_400, kodak_e7023_ultramax_400,
};
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
fn black_and_white_negatives_print_grey_and_in_order() {
    let base = [0.7, 0.68, 0.66];
    let mono = Mono::default();
    let inverter = Inverter::black_and_white(base, &Printing::default(), &mono, None);
    assert!(inverter.is_mono() && !inverter.separable());
    // A grey card's density above base prints as a grey card does.
    let card: [f32; 3] = std::array::from_fn(|c| base[c] * 10f32.powf(-BW_GREY_DENSITY));
    let d = inverter.pixel(card);
    let grey = Inverter::new(model(), [0.5; 3], &Printing::default()).display_channel(GREY);
    for v in d {
        assert!((v - grey).abs() < 1e-4, "{d:?} vs {grey}");
    }
    // Denser negative, lighter print; one stop more is BW_GAMMA·log10(2) denser.
    let denser = card.map(|l| l * 10f32.powf(-BW_GAMMA * 2f32.log10()));
    let e = inverter.scene(0, denser[0]);
    assert!((e / GREY - 2.0).abs() < 1e-3, "{e}");
    assert!(inverter.pixel(denser)[1] > d[1]);
    // The color balance doesn't tint it.
    let tinted = Printing {
        balance_ev: [0.5, 0.0, -0.5],
        ..Printing::default()
    };
    let t = Inverter::black_and_white(base, &tinted, &mono, None).pixel(card);
    assert!((t[0] - t[2]).abs() < 1e-6);
}

#[test]
fn black_and_white_mixes_and_tones() {
    let base = [0.7; 3];
    // Only red counts: the other channels' light makes no difference.
    let red = Mono {
        mix: [2.0, 0.0, 0.0],
        ..Mono::default()
    };
    let inverter = Inverter::black_and_white(base, &Printing::default(), &red, None);
    let a = inverter.pixel([0.1, 0.3, 0.05]);
    let b = inverter.pixel([0.1, 0.02, 0.6]);
    assert!((a[0] - b[0]).abs() < 1e-6);
    // Sepia warms the midtones and leaves black and white.
    let sepia = Mono {
        toning: Toning::Sepia,
        toning_amount: 1.0,
        ..Mono::default()
    };
    let toned = Inverter::black_and_white(base, &Printing::default(), &sepia, None);
    let mid = toned.pixel([0.7 * 10f32.powf(-BW_GREY_DENSITY); 3]);
    assert!(mid[0] > mid[1] && mid[1] > mid[2], "{mid:?}");
    let white = toned.display([1e6; 3]);
    assert!(white.iter().all(|v| *v > 0.99), "{white:?}");
    for t in Toning::ALL {
        assert!(!t.label().is_empty());
    }
}

#[test]
fn slides_pass_their_light_through() {
    let inverter = Inverter::positive(&Printing::default());
    assert!(inverter.separable() && !inverter.is_mono());
    // A grey card on the slide, as sRGB shows it.
    let grey = crate::spectral::srgb_encode(f64::from(GREY)) as f32;
    let d = inverter.pixel([GREY; 3]);
    assert!(d.iter().all(|v| (v - grey).abs() < 1e-5), "{d:?}");
    // More light, lighter; exposure and balance as on a negative.
    assert!(inverter.pixel([0.5; 3])[0] > d[0]);
    let warm = Printing {
        exposure_ev: 1.0,
        balance_ev: [0.5, 0.0, -0.5],
        ..Printing::default()
    };
    let w = Inverter::positive(&warm);
    assert!((w.scene(1, 0.1) - 0.2).abs() < 1e-6);
    let p = w.pixel([GREY; 3]);
    assert!(p[0] > p[1] && p[1] > p[2]);
    // More contrast spreads light and dark from grey.
    let punchy = Inverter::positive(&Printing {
        contrast: 2.0,
        ..Printing::default()
    });
    assert!(punchy.pixel([0.05; 3])[0] < inverter.pixel([0.05; 3])[0]);
    assert!((punchy.pixel([GREY; 3])[0] - grey).abs() < 1e-5);
}

/// The red density of a grey card exposed at a sheet's Log H ref, and of the lightest step
/// of a paper grey scale (about 90% reflectance, five times the card).
fn card_and_white(sheet: &Datasheet) -> (f32, f32) {
    let red = &sheet.characteristic[0];
    (
        red.at(sheet.log_h_ref),
        red.at(sheet.log_h_ref + (0.9f32 / GREY).log10()),
    )
}

#[test]
fn extracted_curves_agree_with_the_sheets_exposure_checks() {
    // Each sheet's "Judging negative exposures": what a normally exposed grey card, and
    // the lightest step of a paper grey scale, read through the Status M red filter. The
    // last column is how far outside them the sheet's own curves may be: E-4046 and E-4050
    // print the same check word for word, and their curves sit 0.02 to 0.09 denser (see
    // the stocks' modules); AF3-0262E's Log H ref is assumed, and lands 0.01 off its check; E-4L's is assumed
    // too, and lands within its check.
    let checks = [
        ("kodak-kodacolor-200", 0.85..=1.05, Some(1.25..=1.45), 0.0),
        ("kodak-gold-200", 0.85..=1.05, Some(1.25..=1.45), 0.0),
        ("kodak-gold-100", 0.90..=1.10, Some(1.30..=1.50), 0.0),
        ("kodak-ektar-100", 0.77..=0.87, Some(1.13..=1.23), 0.09),
        ("kodak-portra-400", 0.77..=0.87, Some(1.13..=1.23), 0.05),
        ("kodak-pro-image-100", 1.03..=1.23, Some(1.43..=1.63), 0.0),
        ("kodak-ultramax-400", 0.80..=1.00, Some(1.20..=1.40), 0.0),
        ("fujifilm-400", 0.75..=0.95, None, 0.02),
    ];
    let within = |range: &RangeInclusive<f32>, v: f32, slack: f32| {
        (*range.start() - slack..=*range.end() + slack).contains(&v)
    };
    for (id, card_range, white_range, slack) in &checks {
        let (card, white) = card_and_white(stock::by_id(id).unwrap().sheet);
        assert!(within(card_range, card, *slack), "{id}: grey card {card}");
        if let Some(range) = white_range {
            assert!(within(range, white, *slack), "{id}: paper white {white}");
        }
    }
    // Sheets without such a check: Log H ref within a third of a stop of where Kodak's
    // still-film sheets put a grey card for the sheet's own speed S, log10(0.8 / S) + 1.26
    // (Gold 200: -1.14 at 200; Portra 400: -1.44 at 400).
    let by_speed = [
        ("cinestill-50d", 50.0),
        ("reto-amber-d100", 50.0),
        ("cinestill-400d", 250.0),
        ("cinestill-800t", 500.0),
        ("flic-film-elektra-100", 100.0),
        ("fujifilm-200", 200.0),
        ("harman-phoenix-200", 200.0),
        ("harman-phoenix-ii-200", 200.0),
    ];
    for (id, speed) in by_speed {
        let sheet = stock::by_id(id).unwrap().sheet;
        let expected = (0.8f32 / speed).log10() + 1.26;
        assert!(
            (sheet.log_h_ref - expected).abs() < 0.1,
            "{id}: Log H ref {} against {expected}",
            sheet.log_h_ref
        );
        let (card, white) = card_and_white(sheet);
        assert!((0.8..=1.05).contains(&card), "{id}: grey card {card}");
        assert!(white > card + 0.3, "{id}: paper white {white}");
    }
    for stock in stock::all() {
        assert!(
            checks.iter().any(|c| c.0 == stock.id) || by_speed.iter().any(|c| c.0 == stock.id),
            "{} needs its exposure check here",
            stock.id
        );
    }
}

#[test]
fn layers_peak_where_their_colors_are() {
    // Films without an orange mask: Aerocolor (Elektra 100), whose sheet says so, and
    // Harman's, on the generic profile.
    let unmasked = [
        "flic-film-elektra-100",
        "harman-phoenix-200",
        "harman-phoenix-ii-200",
    ];
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
        let id = stock.id;
        assert!((630.0..=670.0).contains(&peak(0)), "{id}: red {}", peak(0));
        assert!(
            (530.0..=570.0).contains(&peak(1)),
            "{id}: green {}",
            peak(1)
        );
        // Tungsten film's blue layer peaks lower, where tungsten light has least.
        assert!((395.0..=480.0).contains(&peak(2)), "{id}: blue {}", peak(2));
        let d = sheet.d_min();
        if unmasked.contains(&id) {
            // Without a mask, the base is near neutral.
            assert!(d[2] - d[0] < 0.2, "{id}: {d:?}");
        } else {
            // D-min is the orange mask: blue densest, red least.
            assert!(d[2] > d[1] && d[1] > d[0], "{id}: {d:?}");
        }
    }
}

#[test]
fn the_generic_film_is_the_kodak_sheets_typical_curve() {
    let generic = &generic_c41::SHEET;
    let members = [
        &SHEET,
        &kodak_e4046_ektar_100::SHEET,
        &kodak_e4050_portra_400::SHEET,
        &kodak_e7023_ultramax_400::SHEET,
    ];
    let g = &generic.characteristic[0];
    for c in 1..3 {
        assert_eq!(generic.characteristic[c].values, g.values);
    }
    // Its density, stop by stop from a grey card's, is the members' mean.
    for stops in [-5, -2, 2, 5] {
        let rel = stops as f32 * std::f32::consts::LOG10_2;
        let mine = g.at(generic.log_h_ref + rel) - g.at(generic.log_h_ref);
        let mean = members
            .iter()
            .flat_map(|s| {
                s.characteristic
                    .iter()
                    .map(move |c| c.at(s.log_h_ref + rel) - c.at(s.log_h_ref))
            })
            .sum::<f32>()
            / 12.0;
        assert!((mine - mean).abs() < 2e-3, "{stops}: {mine} vs {mean}");
    }
}

#[test]
fn black_and_white_film_inverts_on_its_curve() {
    let hp5 = stock::mono_by_id("ilford-hp5-plus").unwrap();
    assert_eq!(hp5.display_name(), "Ilford HP5 Plus");
    assert!(stock::mono_by_id("nonesuch").is_none());
    let film = hp5.model();
    // Base plus fog as printed, and the film runs back to the scene.
    assert!((film.d_min() - 0.17).abs() < 0.01, "{}", film.d_min());
    for e in [0.01, 0.05, GREY, 0.9, 5.0] {
        let d = film.negative(e);
        assert!((film.exposure(d) / e - 1.0).abs() < 1e-3, "{e} -> {d}");
    }
    let card = film.negative(GREY);
    assert!((0.5..0.75).contains(&card), "{card}");
    // HP5's gamma on its straight line, about 0.65.
    let gamma = (film.negative(GREY * 4.0) - card) / 4f32.log10();
    assert!((0.55..0.75).contains(&gamma), "{gamma}");
    // A grey card on the film prints as a grey card does.
    let base = [0.7, 0.68, 0.66];
    let inverter =
        Inverter::black_and_white(base, &Printing::default(), &Mono::default(), Some(film));
    assert!(inverter.is_mono());
    let lights: [f32; 3] = std::array::from_fn(|c| base[c] * 10f32.powf(-card));
    let grey = Inverter::new(model(), [0.5; 3], &Printing::default()).display_channel(GREY);
    for v in inverter.pixel(lights) {
        assert!((v - grey).abs() < 1e-3, "{v} vs {grey}");
    }
}

#[test]
fn every_stock_has_its_own_id() {
    let mut ids: Vec<&str> = stock::all().iter().map(|s| s.id).collect();
    ids.extend(stock::mono_all().iter().map(|s| s.id));
    let n = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), n);
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
    // Every stock, with two scanners' worth of base color and level: the base is divided
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
fn printing_moves_exposure_and_color() {
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
    // Each layer feeds its own color most.
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
