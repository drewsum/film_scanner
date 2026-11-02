use scan_core::ScanArea;
use scan_core::image::{ImageBuffer, Rgb};
use scan_film::stock;

use super::*;

fn image(w: u32, h: u32, f: impl Fn(u32, u32) -> [f32; 3]) -> Rgb16Image {
    ImageBuffer::from_fn(w, h, |x, y| Rgb(f(x, y).map(to_u16)))
}

fn px(image: &Rgb16Image, x: u32, y: u32) -> [f32; 3] {
    image.get_pixel(x, y).0.map(|v| f32::from(v) / 65535.0)
}

/// A ramp across, of a different color down each band.
fn test_card() -> Rgb16Image {
    image(256, 60, |x, y| {
        let v = x as f32 / 255.0;
        match y / 10 {
            0 => [v, v, v],
            1 => [v, 0.3 * v, 0.2 * v],
            2 => [0.9 * v, 0.8 * v, 0.1],
            3 => [0.2 * v, v, 0.3 * v],
            4 => [0.1, 0.5 * v, v],
            _ => [0.8 * v, 0.2, 0.7 * v],
        }
    })
}

#[test]
fn default_adjustments_change_nothing() {
    let card = test_card();
    let adj = Adjustments::default();
    assert!(adj.is_identity() && !adj.edits_positive());
    assert_eq!(apply(card.clone(), &adj, 1.0), card);
    let scan = DynamicImage::ImageRgb16(card.clone());
    assert_eq!(develop(&scan, 2.2, None, &adj, 1.0), card);
}

#[test]
fn levels_stretch_and_bend_midtones() {
    let l = Levels {
        black: 0.1,
        white: 0.9,
        ..Levels::default()
    };
    assert_eq!(l.apply(0.05), 0.0);
    assert!((l.apply(0.5) - 0.5).abs() < 1e-6);
    assert_eq!(l.apply(0.95), 1.0);
    let lighter = Levels {
        gamma: 2.0,
        ..Levels::default()
    };
    assert!((lighter.apply(0.25) - 0.5).abs() < 1e-6);
    let out = Levels {
        out_black: 0.2,
        out_white: 0.8,
        ..Levels::default()
    };
    assert!((out.apply(0.0) - 0.2).abs() < 1e-6 && (out.apply(1.0) - 0.8).abs() < 1e-6);
}

#[test]
fn channel_levels_move_only_their_channel() {
    let mut adj = Adjustments::default();
    adj.levels[2].gamma = 1.5;
    let out = apply(test_card(), &adj, 1.0);
    let (a, b) = (px(&test_card(), 128, 5), px(&out, 128, 5));
    assert!((a[0] - b[0]).abs() < 1e-4 && (a[2] - b[2]).abs() < 1e-4);
    assert!(b[1] > a[1] + 0.1, "{a:?} -> {b:?}");
}

#[test]
fn curves_pass_through_their_points_without_overshoot() {
    let mut c = Curve::default();
    assert!(c.is_identity());
    assert!((c.eval(0.3) - 0.3).abs() < 1e-6);
    let i = c.insert(0.25, 0.4).unwrap();
    c.insert(0.75, 0.8).unwrap();
    assert_eq!(c.insert(0.251, 0.1), None, "too close to another point");
    for p in c.points() {
        assert!((c.eval(p[0]) - p[1]).abs() < 1e-5);
    }
    // Monotone points give a monotone curve.
    let mut last = 0.0;
    for k in 0..=1000 {
        let y = c.eval(k as f32 / 1000.0);
        assert!(y >= last - 1e-6);
        last = y;
    }
    // A point can't be moved past its neighbours; ends stay.
    c.set(i, 0.9, 0.5);
    assert!(c.points()[i][0] < 0.75);
    c.remove(0);
    c.remove(0);
    c.remove(0);
    assert_eq!(c.points().len(), 2);
    // A flat stretch stays flat: no bump between equal points.
    let flat = Curve::new([[0.0, 0.0], [0.3, 0.5], [0.7, 0.5], [1.0, 1.0]]);
    for k in 30..=70 {
        assert!((flat.eval(k as f32 / 100.0) - 0.5).abs() < 1e-5);
    }
}

#[test]
fn brightness_and_contrast_keep_black_white_and_grey() {
    for (brightness, contrast) in [(0.5, 0.0), (-0.5, 0.0), (0.0, 0.8), (0.0, -0.8)] {
        let adj = Adjustments {
            brightness,
            contrast,
            ..Adjustments::default()
        };
        let t = tone_tables(&adj);
        assert_eq!((t[0][0], t[0][65535]), (0, 65535));
        let quarter = f32::from(t[1][16384]) / 65535.0;
        let mid = f32::from(t[1][32768]) / 65535.0;
        if brightness > 0.0 {
            assert!(mid > 0.6);
        } else if brightness < 0.0 {
            assert!(mid < 0.4);
        } else {
            assert!((mid - 0.5).abs() < 0.002);
            assert_eq!(quarter < 0.25, contrast > 0.0, "{contrast}: {quarter}");
        }
    }
}

#[test]
fn saturation_scales_chroma_about_grey() {
    let card = test_card();
    let grey = apply(
        card.clone(),
        &Adjustments {
            saturation: -1.0,
            ..Adjustments::default()
        },
        1.0,
    );
    for (x, y) in [(200, 15), (128, 35), (60, 55)] {
        let (a, b) = (px(&card, x, y), px(&grey, x, y));
        assert!((b[0] - b[1]).abs() < 1e-4 && (b[1] - b[2]).abs() < 1e-4);
        assert!((a.iter().sum::<f32>() - b.iter().sum::<f32>()).abs() < 1e-3);
    }
    // Greys are left alone.
    let more = apply(
        card.clone(),
        &Adjustments {
            saturation: 0.5,
            ..Adjustments::default()
        },
        1.0,
    );
    assert_eq!(px(&more, 100, 5), px(&card, 100, 5));
    let chroma = |p: [f32; 3]| opponent(p)[1].hypot(opponent(p)[2]);
    let (a, b) = (px(&card, 128, 15), px(&more, 128, 15));
    assert!((chroma(b) / chroma(a) - 1.5).abs() < 0.01);
}

#[test]
fn hue_weights_share_every_hue() {
    for h in 0..360 {
        let w = hue_weights(h as f32);
        assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-5, "{h}");
    }
    assert_eq!(hue_weights(120.0)[2], 1.0);
    assert_eq!(hue_weights(-60.0)[5], 1.0);
}

#[test]
fn selective_color_moves_its_hues_only() {
    let mut adj = Adjustments::default();
    // Blues: lighter and less saturated.
    adj.selective[4] = HueAdjust {
        hue_deg: 0.0,
        saturation: -0.5,
        lightness: 0.3,
    };
    let blue = [0.1, 0.2, 0.8];
    let red = [0.8, 0.15, 0.1];
    let grey = [0.4, 0.4, 0.4];
    let s = adj.selective;
    let (b, r, g) = (
        recolor(blue, 0.0, 0.0, &s),
        recolor(red, 0.0, 0.0, &s),
        recolor(grey, 0.0, 0.0, &s),
    );
    assert!(b.iter().sum::<f32>() > blue.iter().sum::<f32>());
    assert!(b[2] - b[0] < blue[2] - blue[0]);
    for c in 0..3 {
        assert!((r[c] - red[c]).abs() < 1e-5 && (g[c] - grey[c]).abs() < 1e-5);
    }
    // A hue shift turns reds towards yellow.
    let mut s = [HueAdjust::default(); 6];
    s[0].hue_deg = 30.0;
    let shifted = recolor(red, 0.0, 0.0, &s);
    assert!(shifted[1] > red[1] + 0.1 && shifted[0] < red[0]);
}

/// A step edge with noise: (noise, flat-area variance, edge contrast).
fn noisy_step() -> Rgb16Image {
    // A fixed pseudo-random pattern.
    let noise = |x: u32, y: u32, c: u32| {
        let mut h =
            x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663) ^ c.wrapping_mul(83_492_791);
        h ^= h >> 13;
        h = h.wrapping_mul(0x5bd1_e995);
        h ^= h >> 15;
        (h % 1000) as f32 / 1000.0 - 0.5
    };
    image(120, 80, |x, y| {
        let base = if x < 60 { 0.3 } else { 0.7 };
        std::array::from_fn(|c| base + 0.04 * noise(x, y, c as u32))
    })
}

fn variance(image: &Rgb16Image, xs: std::ops::Range<u32>) -> f32 {
    let mut v = Vec::new();
    for y in 10..70 {
        for x in xs.clone() {
            v.push(px(image, x, y)[1]);
        }
    }
    let m = v.iter().sum::<f32>() / v.len() as f32;
    v.iter().map(|a| (a - m).powi(2)).sum::<f32>() / v.len() as f32
}

#[test]
fn noise_reduction_smooths_flat_areas_and_keeps_edges() {
    let step = noisy_step();
    let adj = Adjustments {
        noise: NoiseReduction {
            grain: 1.0,
            color: 1.0,
            radius_px: 3.0,
        },
        ..Adjustments::default()
    };
    let out = apply(step.clone(), &adj, 1.0);
    assert!(variance(&out, 10..50) < variance(&step, 10..50) / 4.0);
    // The edge stays a sharp step.
    let (a, b) = (px(&out, 58, 40)[1], px(&out, 61, 40)[1]);
    assert!(a < 0.36 && b > 0.64, "{a} {b}");
}

#[test]
fn sharpening_steepens_edges_and_spares_noise_under_the_threshold() {
    let edge = image(80, 20, |x, _| [if x < 40 { 0.3 } else { 0.7 }; 3]);
    let adj = Adjustments {
        sharpen: Sharpen {
            amount: 1.0,
            radius_px: 1.5,
            threshold: 0.0,
        },
        ..Adjustments::default()
    };
    let out = apply(edge.clone(), &adj, 1.0);
    assert!(px(&out, 38, 10)[0] < 0.29 && px(&out, 41, 10)[0] > 0.71);
    assert_eq!(px(&out, 5, 10), px(&edge, 5, 10), "flat areas unchanged");
    assert!(
        (px(&out, 41, 10)[0] - px(&out, 41, 10)[2]).abs() < 1e-6,
        "no color"
    );

    let step = noisy_step();
    let thresholded = Adjustments {
        sharpen: Sharpen {
            amount: 2.0,
            radius_px: 1.0,
            threshold: 0.05,
        },
        ..Adjustments::default()
    };
    let out = apply(step.clone(), &thresholded, 1.0);
    assert!(variance(&out, 10..50) < variance(&step, 10..50) * 1.05);
}

#[test]
fn spatial_steps_scale_with_the_preview() {
    // A preview at half size with half the radius matches the full image halved.
    let edge = |w: u32| image(w, 8, move |x, _| [if x < w / 2 { 0.2 } else { 0.8 }; 3]);
    let adj = Adjustments {
        sharpen: Sharpen {
            amount: 1.0,
            radius_px: 4.0,
            threshold: 0.0,
        },
        ..Adjustments::default()
    };
    let full = apply(edge(200), &adj, 1.0);
    let half = apply(edge(100), &adj, 0.5);
    let overshoot = |i: &Rgb16Image| px(i, i.width() / 2, 4)[0];
    assert!((overshoot(&full) - overshoot(&half)).abs() < 0.03);
}

#[test]
fn gains_and_printing_add_to_the_roll() {
    let adj = Adjustments {
        exposure_ev: 0.5,
        balance_ev: [0.2, 0.0, -0.1],
        ..Adjustments::default()
    };
    let roll = Printing {
        exposure_ev: 1.0,
        balance_ev: [0.1, 0.1, -0.2],
        ..Printing::default()
    };
    let p = adj.printing(&roll);
    assert_eq!(p.exposure_ev, 1.5);
    assert!((p.balance_ev[0] - 0.3).abs() < 1e-6 && (p.balance_ev[2] + 0.3).abs() < 1e-6);
    assert_eq!(p.contrast, roll.contrast);
    let g = adj.gains();
    assert!((g[1] - 2f32.sqrt()).abs() < 1e-5);

    // On a positive the gains are on linear light: one stop doubles it.
    let grey = DynamicImage::ImageRgb16(image(4, 4, |_, _| [0.25f32.powf(1.0 / 2.2); 3]));
    let brighter = Adjustments {
        exposure_ev: 1.0,
        ..Adjustments::default()
    };
    let out = develop(&grey, 2.2, None, &brighter, 1.0);
    assert!((px(&out, 0, 0)[0].powf(2.2) - 0.5).abs() < 0.002);
}

#[test]
fn a_frame_balance_works_in_the_film_scene_light() {
    // A negative inverted with a frame's balance is the same as inverting with the roll's
    // printing changed by it: the per-frame offset happens before the print curve.
    let model = stock::default_stock().model();
    let base = [0.6, 0.35, 0.12];
    let neg = DynamicImage::ImageRgb16(image(16, 4, |x, _| {
        let d = model.negative([0.02 + 0.05 * x as f32; 3]);
        std::array::from_fn(|c| (base[c] * 10f32.powf(-d[c])).powf(1.0 / 2.2))
    }));
    let adj = Adjustments {
        balance_ev: [0.0, 0.0, 0.5],
        ..Adjustments::default()
    };
    let roll = Printing::default();
    let inverter = Inverter::new(model, base, &adj.printing(&roll));
    let out = develop(&neg, 2.2, Some(&inverter), &adj, 1.0);
    let neutral = invert(&neg, 2.2, &Inverter::new(model, base, &roll));
    for x in [3, 8, 12] {
        let (a, b) = (px(&neutral, x, 0), px(&out, x, 0));
        assert!((a[0] - a[2]).abs() < 0.02, "neutral grey {a:?}");
        assert!(b[2] > b[0] + 0.03 && (b[0] - a[0]).abs() < 1e-3, "{b:?}");
    }
}

#[test]
fn neutralize_makes_the_light_grey() {
    let e = [0.2, 0.1, 0.05];
    let d = neutralize(e);
    let out: Vec<f32> = (0..3).map(|c| e[c] * 2f32.powf(d[c])).collect();
    assert!((out[0] - out[1]).abs() < 1e-6 && (out[1] - out[2]).abs() < 1e-6);
    assert!(d.iter().sum::<f32>().abs() < 1e-6, "level kept");
}

#[test]
fn auto_levels_stretch_the_range_keeping_balance() {
    let narrow = image(100, 10, |x, _| {
        let v = 0.2 + 0.5 * x as f32 / 99.0;
        [v, v * 0.9, v * 0.8]
    });
    let l = auto_levels(&histogram(&narrow), 0.001);
    // Black by the darkest channel (blue), white by the lightest (red).
    assert!((l.black - 0.16).abs() < 0.01, "{l:?}");
    assert!((l.white - 0.70).abs() < 0.01, "{l:?}");
    // An image already full range is left alone.
    let full = image(256, 4, |x, _| [x as f32 / 255.0; 3]);
    let l = auto_levels(&histogram(&full), 0.001);
    assert!(l.black < 0.01 && l.white > 0.99);
}

#[test]
fn histograms_count_every_channel() {
    let card = image(
        10,
        10,
        |x, _| if x < 5 { [1.0, 0.0, 0.0] } else { [0.0; 3] },
    );
    let h = histogram(&card);
    assert_eq!((h.bins[1][255], h.bins[1][0], h.bins[2][0]), (50, 50, 100));
    assert_eq!(h.bins[MASTER][54], 50, "red's brightness is 0.2126");
}

#[test]
fn orientation_mirrors_then_turns() {
    let img = DynamicImage::ImageRgb16(image(3, 2, |x, y| [x as f32 / 2.0, y as f32, 0.0]));
    let o = orient(&img, 1, true).to_rgb16();
    assert_eq!(o.dimensions(), (2, 3));
    // Mirrored, the top-left is the old top-right (x = 2); a quarter turn clockwise puts
    // the top-left at the top right.
    assert_eq!(o.get_pixel(1, 0).0[0], 65535);
    assert_eq!(o.get_pixel(1, 0).0[1], 0);
    assert!(matches!(orient(&img, 0, false), Cow::Borrowed(_)));
}

#[test]
fn positives_balance_from_their_log_means() {
    let img = DynamicImage::ImageRgb16(image(20, 20, |_, _| [0.5, 0.4, 0.3]));
    let m = log_means(&img, 1.0);
    let d = neutralize(m.map(f32::exp2));
    let fixed: Vec<f32> = (0..3).map(|c| m[c] + d[c]).collect();
    assert!((fixed[0] - fixed[2]).abs() < 1e-5);
}

#[test]
fn temperature_and_tint_move_the_balance_keeping_the_level() {
    let warm = Adjustments {
        temperature: 1.0,
        ..Adjustments::default()
    };
    let [r, g, b] = warm.color_ev();
    assert!((r - TEMPERATURE_EV).abs() < 1e-6 && g == 0.0 && (b + TEMPERATURE_EV).abs() < 1e-6);
    let magenta = Adjustments {
        tint: 0.5,
        balance_ev: [0.1, 0.0, 0.0],
        ..Adjustments::default()
    };
    let e = magenta.color_ev();
    assert!(e[1] < 0.0 && e[0] > 0.1 && e[2] > 0.0);
    // Neither changes the overall level.
    let mean = |e: [f32; 3]| e.iter().sum::<f32>() / 3.0;
    assert!((mean(e) - 0.1 / 3.0).abs() < 1e-6);
    let printing = warm.printing(&Printing::default());
    assert_eq!(printing.balance_ev, warm.color_ev());
    let g = warm.gains();
    assert!(g[0] > 1.0 && g[1] == 1.0 && g[2] < 1.0);
}

#[test]
fn borders_grow_the_frame_and_straightening_keeps_it_inside() {
    let rect = FrameRect {
        cx_mm: 50.0,
        cy_mm: 40.0,
        width_mm: 36.0,
        height_mm: 24.0,
        angle_deg: 1.0,
    };
    assert_eq!(framed(&rect, &Adjustments::default()), rect);
    let border = Adjustments {
        border_mm: 1.5,
        ..Adjustments::default()
    };
    let grown = framed(&rect, &border);
    assert_eq!((grown.width_mm, grown.height_mm), (39.0, 27.0));
    assert!(border.reframes());

    let level = Adjustments {
        straighten_deg: 3.0,
        ..border
    };
    let turned = framed(&rect, &level);
    assert!((turned.angle_deg - (1.0 - 3.0)).abs() < 1e-6);
    // Smaller, the same shape, and every corner inside the grown frame.
    assert!(turned.width_mm < 39.0);
    assert!((turned.width_mm / turned.height_mm - 39.0 / 27.0).abs() < 1e-4);
    for (x, y) in turned.corners() {
        let (u, v) = grown.to_local(x, y);
        assert!(u.abs() <= 19.5 + 1e-3 && v.abs() <= 13.5 + 1e-3, "{u}, {v}");
    }
}

#[test]
fn spots_are_found_on_the_cut_and_healed() {
    let rect = FrameRect {
        cx_mm: 20.0,
        cy_mm: 10.0,
        width_mm: 10.0,
        height_mm: 8.0,
        angle_deg: 90.0,
    };
    // Its own right is down the bed: a spot 2 mm below the middle is 2 mm right of it.
    let spot = Spot {
        x_mm: 20.0,
        y_mm: 12.0,
        radius_mm: 0.5,
    };
    let area = ScanArea::full(40.0, 30.0);
    let [x, y, r] = spot_pixels(&[spot], Some(&rect), &area, 10.0)[0];
    assert!((x - 70.0).abs() < 1e-3 && (y - 40.0).abs() < 1e-3 && r == 5.0);
    let [x, y, _] = spot_pixels(&[spot], None, &area, 2.0)[0];
    assert!((x - 40.0).abs() < 1e-3 && (y - 24.0).abs() < 1e-3);

    let mut img = image(100, 80, |_, _| [0.5, 0.4, 0.3]);
    for yy in 36..44 {
        for xx in 66..74 {
            img.put_pixel(xx, yy, Rgb([0, 0, 0]));
        }
    }
    let dusty = DynamicImage::ImageRgb16(img);
    let healed = heal(&dusty, &[[70.0, 40.0, 6.0]]).to_rgb16();
    let p = px(&healed, 70, 40);
    assert!(
        (p[0] - 0.5).abs() < 0.01 && (p[2] - 0.3).abs() < 0.01,
        "{p:?}"
    );
    // Nothing to heal, or off the image: unchanged and not copied.
    assert!(matches!(heal(&dusty, &[]), Cow::Borrowed(_)));
    assert!(matches!(
        heal(&dusty, &[[500.0, 5.0, 3.0]]),
        Cow::Borrowed(_)
    ));
}

/// Dark on the left, light on the right, each with a little texture.
fn two_tone() -> Rgb16Image {
    image(200, 100, |x, y| {
        let t = if (x / 4 + y / 4) % 2 == 0 { 0.02 } else { 0.0 };
        let v = if x < 100 { 0.12 } else { 0.85 } + t;
        [v; 3]
    })
}

#[test]
fn shadows_and_highlights_work_on_their_own_areas() {
    let card = two_tone();
    let lifted = apply(
        card.clone(),
        &Adjustments {
            shadows: 1.0,
            ..Adjustments::default()
        },
        1.0,
    );
    let (dark, light) = (px(&card, 30, 50)[0], px(&card, 170, 50)[0]);
    assert!(
        px(&lifted, 30, 50)[0] > dark * 1.6,
        "{:?}",
        px(&lifted, 30, 50)
    );
    assert!((px(&lifted, 170, 50)[0] - light).abs() < 0.01);
    let calmed = apply(
        card.clone(),
        &Adjustments {
            highlights: -1.0,
            ..Adjustments::default()
        },
        1.0,
    );
    assert!(px(&calmed, 170, 50)[0] < light * 0.8);
    assert!((px(&calmed, 30, 50)[0] - dark).abs() < 0.01);
    // The texture within an area is kept: scaled with it, not flattened.
    let step = |img: &Rgb16Image| px(img, 30, 50)[0] - px(img, 34, 50)[0];
    assert!(step(&lifted).abs() > step(&card).abs());
}

#[test]
fn clarity_steepens_local_contrast_and_leaves_flat_areas() {
    // A fine texture on midgrey, and a flat band.
    let card = image(200, 100, |x, y| {
        let v = if y < 50 {
            0.5 + 0.05 * (x as f32 * 0.8).sin()
        } else {
            0.5
        };
        [v; 3]
    });
    let spread = |img: &Rgb16Image| {
        let row: Vec<f32> = (40..160).map(|x| px(img, x, 25)[1]).collect();
        row.iter().cloned().fold(f32::MIN, f32::max) - row.iter().cloned().fold(f32::MAX, f32::min)
    };
    let with = |amount| {
        apply(
            card.clone(),
            &Adjustments {
                clarity: amount,
                ..Adjustments::default()
            },
            1.0,
        )
    };
    let (crisp, soft) = (with(1.0), with(-1.0));
    assert!(spread(&crisp) > spread(&card) * 1.5);
    assert!(spread(&soft) < spread(&card) * 0.5);
    assert!((px(&crisp, 100, 90)[0] - 0.5).abs() < 0.002);
}

#[test]
fn vibrance_lifts_muted_colors_most_and_spares_grey() {
    let muted = [0.45, 0.52, 0.56];
    let vivid = [0.9, 0.2, 0.6];
    let chroma = |p: [f32; 3]| {
        let [_, u, v] = opponent(p);
        u.hypot(v)
    };
    let gain = |p| chroma(recolor(p, 0.0, 1.0, &Default::default())) / chroma(p);
    assert!(gain(muted) > 1.5, "{}", gain(muted));
    assert!(gain(vivid) < 1.2, "{}", gain(vivid));
    let grey = [0.4; 3];
    let out = recolor(grey, 0.0, 1.0, &Default::default());
    assert!(out.iter().all(|v| (v - 0.4).abs() < 1e-5));
    // Skin (orange-red) is lifted less than a muted green of the same chroma.
    let skin = [0.6, 0.5, 0.45];
    let green = [0.5, 0.58, 0.5];
    assert!(gain(skin) < gain(green), "{} {}", gain(skin), gain(green));
}

#[test]
fn falloff_and_vignette_change_the_corners_not_the_middle() {
    let card = image(100, 60, |_, _| [0.5; 3]);
    let with = |a: Adjustments| apply(card.clone(), &a, 1.0);
    let lit = with(Adjustments {
        falloff: 0.5,
        ..Adjustments::default()
    });
    let dark = with(Adjustments {
        vignette: -1.0,
        ..Adjustments::default()
    });
    let light = with(Adjustments {
        vignette: 1.0,
        ..Adjustments::default()
    });
    assert!(px(&lit, 0, 0)[0] > 0.7);
    assert!(px(&dark, 0, 0)[0] < 0.25);
    assert!(px(&light, 99, 59)[0] > 0.75);
    for img in [&lit, &dark, &light] {
        assert!((px(img, 50, 30)[0] - 0.5).abs() < 0.005);
    }
}

#[test]
fn new_edits_count_as_edits() {
    let edits = [
        Adjustments {
            highlights: 0.1,
            ..Adjustments::default()
        },
        Adjustments {
            shadows: 0.1,
            ..Adjustments::default()
        },
        Adjustments {
            clarity: 0.1,
            ..Adjustments::default()
        },
        Adjustments {
            vibrance: 0.1,
            ..Adjustments::default()
        },
        Adjustments {
            falloff: 0.1,
            ..Adjustments::default()
        },
        Adjustments {
            vignette: 0.1,
            ..Adjustments::default()
        },
    ];
    for a in edits {
        assert!(a.edits_positive(), "{a:?}");
    }
    let geometry = Adjustments {
        straighten_deg: 1.0,
        ..Adjustments::default()
    };
    assert!(!geometry.edits_positive() && geometry.reframes());
}

#[test]
fn blurs_match_their_definitions() {
    let (w, h) = (37, 23);
    let src: Vec<f32> = (0..w * h)
        .map(|i| ((i * 7919) % 101) as f32 / 100.0)
        .collect();
    for r in [1, 3, 30] {
        let fast = box_blur(&src, w, h, r);
        for y in 0..h {
            for x in 0..w {
                let (mut sum, mut n) = (0.0, 0.0);
                for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                    for xx in x.saturating_sub(r)..(x + r + 1).min(w) {
                        sum += src[yy * w + xx];
                        n += 1.0;
                    }
                }
                assert!(
                    (fast[y * w + x] - sum / n).abs() < 1e-5,
                    "r {r} at {x}, {y}"
                );
            }
        }
    }
    // A Gaussian keeps a constant, and the mean of anything.
    let flat = gaussian_blur(&vec![0.4; w * h], w, h, 2.0);
    assert!(flat.iter().all(|v| (v - 0.4).abs() < 1e-5));
    let blurred = gaussian_blur(&src, w, h, 1.2);
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
    assert!((mean(&blurred) - mean(&src)).abs() < 0.01);
}
