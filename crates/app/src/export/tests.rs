use yaml_rust2::{Yaml, YamlLoader};

use super::*;

fn filled() -> RollMetadata {
    RollMetadata {
        name: "Spring Walk".into(),
        description: "A short \"test\" roll:\nline two, \\ and ünïcode".into(),
        date_captured: "04/15/2024".into(),
        date_scanned: "04/20/2024".into(),
        public: true,
        gps_latitude: "41.8781".into(),
        gps_longitude: " -87.6298 ".into(),
        manufacturer: "Kodak".into(),
        stock: "Gold".into(),
        iso: "200".into(),
        development_notes: "Lab processed.".into(),
        camera_make: "Nikon".into(),
        camera_model: "FM2".into(),
        lens_make: "Nikon".into(),
        lens_model: "2030960".into(),
        f_number: "f/8".into(),
        focal_length: "50mm".into(),
        flash: "No".into(),
        filters: "# UV: yes".into(),
    }
}

fn parse(yaml: &str) -> Yaml {
    YamlLoader::load_from_str(yaml).unwrap().remove(0)
}

#[test]
fn metadata_reads_back_as_typed() {
    let meta = filled();
    assert!(meta.validate().is_empty(), "{:?}", meta.validate());
    let doc = parse(&meta.to_yaml());
    assert_eq!(doc["format"].as_i64(), Some(0));
    let roll = &doc["roll"];
    assert_eq!(roll["name"].as_str(), Some("Spring Walk"));
    assert_eq!(
        roll["description"].as_str(),
        Some(meta.description.as_str())
    );
    assert_eq!(roll["date_captured"].as_str(), Some("04/15/2024"));
    assert_eq!(roll["date_scanned"].as_str(), Some("04/20/2024"));
    assert_eq!(roll["public"].as_bool(), Some(true));
    assert_eq!(roll["gps_latitude"].as_f64(), Some(41.8781));
    assert_eq!(roll["gps_longitude"].as_f64(), Some(-87.6298));
    let film = &doc["film"];
    assert_eq!(film["stock"].as_str(), Some("Gold"));
    assert_eq!(film["photographic_sensitivity"].as_i64(), Some(200));
    let camera = &doc["camera"];
    // Numbers typed as text stay text: the publisher relies on lens models being strings.
    assert_eq!(camera["lens_model"].as_str(), Some("2030960"));
    assert_eq!(camera["f_number"].as_str(), Some("f/8"));
    assert_eq!(camera["filters"].as_str(), Some("# UV: yes"));
}

#[test]
fn empty_optional_fields_are_left_out() {
    let meta = RollMetadata {
        date_scanned: String::new(),
        description: String::new(),
        gps_latitude: String::new(),
        gps_longitude: String::new(),
        development_notes: String::new(),
        lens_make: String::new(),
        lens_model: String::new(),
        f_number: String::new(),
        focal_length: String::new(),
        flash: String::new(),
        filters: String::new(),
        ..filled()
    };
    assert!(meta.validate().is_empty());
    let doc = parse(&meta.to_yaml());
    let roll = doc["roll"].as_hash().unwrap();
    let keys: Vec<_> = roll.keys().filter_map(Yaml::as_str).collect();
    // The publisher builds the album description on it, so it is always there.
    assert_eq!(keys, ["name", "description", "date_captured", "public"]);
    assert!(doc["film"]["development_notes"].is_badvalue());
    let camera = doc["camera"].as_hash().unwrap();
    assert_eq!(camera.len(), 2);
}

#[test]
fn problems_the_publisher_would_refuse_are_found() {
    assert_eq!(
        RollMetadata::default().validate().len(),
        7,
        "the required fields"
    );
    let meta = RollMetadata {
        date_captured: "2024-04-15".into(),
        date_scanned: "13/01/2024".into(),
        iso: "ISO 200".into(),
        f_number: "f/3.5-5.6".into(),
        focal_length: "28~80mm".into(),
        gps_latitude: "91".into(),
        ..filled()
    };
    let problems = meta.validate();
    assert_eq!(problems.len(), 6, "{problems:?}");
    assert!(problems[0].starts_with("Date captured"));
    assert!(problems.iter().any(|p| p.contains("is a range")));
    let only_lat = RollMetadata {
        gps_longitude: String::new(),
        ..filled()
    };
    assert_eq!(only_lat.validate().len(), 1);
    // Bare numbers are fine.
    let bare = RollMetadata {
        f_number: "F4".into(),
        focal_length: "35".into(),
        ..filled()
    };
    assert!(bare.validate().is_empty());
}

#[test]
fn film_is_filled_from_the_profile() {
    let mut meta = RollMetadata::default();
    let gold = scan_film::stock::by_id("kodak-gold-200").unwrap();
    meta.fill_film(gold.manufacturer, gold.name, gold.iso);
    assert_eq!(
        (&*meta.manufacturer, &*meta.stock, &*meta.iso),
        ("Kodak", "Gold", "200")
    );
}

#[test]
fn frames_are_numbered_to_sort_in_order() {
    let settings = ExportSettings::default();
    let dir = Path::new("out");
    assert_eq!(settings.frame_path(dir, 0, 12), dir.join("roll_01.jpg"));
    assert_eq!(settings.frame_path(dir, 11, 12), dir.join("roll_12.jpg"));
    assert_eq!(settings.frame_path(dir, 4, 120), dir.join("roll_005.jpg"));
    let roll180 = ExportSettings {
        prefix: " roll180 ".into(),
        ..settings.clone()
    };
    assert_eq!(roll180.frame_path(dir, 0, 36), dir.join("roll180_01.jpg"));
    assert_eq!(roll180.prefix_problem(), None);
    let bad = ExportSettings {
        prefix: "roll/180:".into(),
        ..settings.clone()
    };
    assert_eq!(bad.prefix_problem().unwrap(), "File names can't contain /:");
    let settings = ExportSettings {
        prefix: "  ".into(),
        format: FrameFormat::Png,
        ..settings
    };
    assert_eq!(settings.frame_path(dir, 0, 1), dir.join("frame_01.png"));
}

#[test]
fn today_is_a_valid_date() {
    assert!(parse_date(&today()).is_some(), "{}", today());
}
