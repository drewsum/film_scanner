use scan_core::mock::MockBackend;
use scan_core::{NoProgress, ScanRequest, ScannerBackend};
use scan_process::adjust::Spot;

use super::*;
use crate::worker::Event;

/// A state with a mock film scan (two passes and infrared) whose frames were found.
fn scanned() -> AppState {
    let mut dev = MockBackend.open(&MockBackend::device_info()).unwrap();
    let mut request = ScanRequest::new(ScanSource::Transparency, 75);
    request.infrared = true;
    request.exposures = vec![Exposure::default(), Exposure { ev: 1.0 }];
    let result = dev.scan(&request, &mut NoProgress).unwrap();
    let detection = scan_process::detect_frames(
        result.primary(),
        result.gamma,
        result.dpi,
        result.area,
        &Default::default(),
    );
    let mut state = AppState::default();
    state.apply(Event::ScanDone {
        kind: ScanKind::Final,
        result: Ok(result),
    });
    state.apply(Event::FramesDetected {
        generation: state.result_generation,
        detection,
        estimated_base: None,
    });
    assert_eq!(state.crop.frames.len(), 12);
    state.activity = crate::state::Activity::Idle;
    state
}

#[test]
fn rolls_save_and_open_with_every_edit() {
    let mut state = scanned();
    let a = state.adjustments_mut(Some(2));
    a.exposure_ev = 0.4;
    a.straighten_deg = 1.2;
    a.spots.push(Spot {
        x_mm: 10.0,
        y_mm: 20.0,
        radius_mm: 0.4,
    });
    a.curves[0].insert(0.5, 0.6);
    state.crop.frames[5].skip = true;
    state.crop.reversed = true;
    state.quarter_turns = 3;
    state.invert.film_type = FilmType::BlackAndWhite;
    state.invert.mono.toning = Toning::Sepia;
    state.invert.printing.contrast = 1.7;
    state.export.metadata.name = "Halloween".into();
    state.dust.strength = 0.8;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roll180.filmroll");
    let roll = Roll::of(&state, &path).unwrap();
    // Its scan's files are named as Save as TIFF names them.
    let written =
        scan_core::export::save_tiff(state.result.as_ref().unwrap(), &roll.scan_path(&path))
            .unwrap();
    let names: Vec<_> = written
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    let mut expected = vec![roll.scan.primary.clone()];
    expected.extend(roll.scan.passes.iter().map(|(_, n)| n.clone()));
    expected.extend(roll.scan.infrared.clone());
    assert_eq!(names, expected);
    assert_eq!(names[1], "roll180_ev+1.0.tif");
    std::fs::write(&path, roll.to_text()).unwrap();

    let (opened, result) = Roll::open(&path).unwrap();
    assert_eq!(opened, roll);
    let original = state.result.as_ref().unwrap();
    assert_eq!(result.primary(), original.primary());
    assert_eq!(result.passes[1].exposure.ev, 1.0);
    assert_eq!(result.infrared, original.infrared);
    assert_eq!(
        (result.dpi, result.area, result.gamma),
        (original.dpi, original.area, original.gamma)
    );

    let mut back = AppState::default();
    opened.restore(&mut back, result, path.clone());
    assert_eq!(back.crop.frames.len(), 12);
    for (a, b) in back.crop.frames.iter().zip(&state.crop.frames) {
        assert_eq!((a.rect, &a.adjust, a.skip), (b.rect, &b.adjust, b.skip));
    }
    assert!(back.crop.reversed && back.quarter_turns == 3);
    assert_eq!(back.invert.film_type, FilmType::BlackAndWhite);
    assert_eq!(back.invert.mono.toning, Toning::Sepia);
    assert_eq!(back.invert.printing.contrast, 1.7);
    assert_eq!(back.export.metadata.name, "Halloween");
    assert_eq!(back.dust.strength, 0.8);
    // The film base came with it, so the frames aren't looked for again, and the film is
    // ready to print.
    assert_eq!(
        back.current_film_base(),
        state.current_film_base().map(|b| FilmBase {
            generation: back.result_generation,
            ..b
        })
    );
    assert!(back.film().is_some());
    assert_eq!(back.roll_file, Some((path, back.result_generation)));
    // Merging and dust removal are done again.
    assert!(matches!(
        back.update_processing(),
        Some(crate::worker::Command::MergeExposures { .. })
    ));
}

#[test]
fn saving_again_skips_the_scan_already_saved() {
    let mut state = scanned();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.filmroll");
    let Some(crate::worker::Command::SaveRoll { scan: Some(_), .. }) =
        state.start_save_roll(dir.path().join("r"))
    else {
        panic!("expected the scan saved with the roll");
    };
    state.activity = crate::state::Activity::Idle;
    state.apply(Event::RollSaved {
        path: path.clone(),
        generation: state.result_generation,
        result: Ok(vec![path.clone()]),
    });
    // Its file isn't there in this test, so it is written again...
    assert!(matches!(
        state.start_save_roll(path.clone()),
        Some(crate::worker::Command::SaveRoll { scan: Some(_), .. })
    ));
    state.activity = crate::state::Activity::Idle;
    // ...but once it is, only the roll file is.
    std::fs::write(dir.path().join("r.tif"), b"").unwrap();
    assert!(matches!(
        state.start_save_roll(path),
        Some(crate::worker::Command::SaveRoll { scan: None, .. })
    ));
}

#[test]
fn newer_or_broken_roll_files_are_refused() {
    let state = scanned();
    let mut roll = Roll::of(&state, Path::new("x.filmroll")).unwrap();
    roll.format = FORMAT + 1;
    let err = Roll::from_text(&roll.to_text()).unwrap_err();
    assert!(err.contains("newer"), "{err}");
    assert!(Roll::from_text("(format: 1").is_err());
    // A missing scan is named.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gone.filmroll");
    roll.format = FORMAT;
    std::fs::write(&path, roll.to_text()).unwrap();
    let err = Roll::open(&path).unwrap_err();
    assert!(err.contains("x.tif"), "{err}");
}
