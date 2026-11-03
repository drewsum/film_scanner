//! Cut one frame out of a saved film scan after each processing stage, to see what each does
//! to the image (issue #28): the scan as read, merged with its longer exposure passes, and
//! with dust removed by its infrared pass.
//!
//!     cargo run --release -p scan-process --example stages -- OUT_DIR SCAN.tif DPI GAMMA \
//!         CX,CY,W,H,ANGLE [--pass EV,PASS.tif ...] [--ir IR.tif] [--dust STRENGTH]
//!
//! The frame is in millimetres on the scanner bed, as a roll file's `rect` gives it; the scan
//! is taken to cover the V500's film area from its corner. Writes `read.tif`, `merged.tif`
//! and `cleaned.tif` (as far as the passes given allow), uninverted and in the scan's
//! encoding, and `aligned.tif`: the last of them with its color channels lined up.

use scan_core::export::open_scan;
use scan_core::image::DynamicImage;
use scan_core::{Exposure, MM_PER_INCH, ScanArea, ScanPass};
use scan_process::{
    DustParams, FrameRect, channel_shifts, crop_frame, crop_frame_aligned, merge_exposures,
    remove_dust,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: stages OUT_DIR SCAN.tif DPI GAMMA CX,CY,W,H,ANGLE \
                 [--pass EV,PASS.tif ...] [--ir IR.tif] [--dust STRENGTH]";
    let [out, scan, dpi, gamma, rect] = &args.get(..5).expect(usage) else {
        unreachable!()
    };
    let dpi: u32 = dpi.parse().expect("DPI");
    let gamma: f32 = gamma.parse().expect("GAMMA");
    let r: Vec<f32> = rect
        .split(',')
        .map(|v| v.parse().expect("CX,CY,W,H,ANGLE"))
        .collect();
    let frame = FrameRect {
        cx_mm: r[0],
        cy_mm: r[1],
        width_mm: r[2],
        height_mm: r[3],
        angle_deg: r[4],
    };
    let (mut passes, mut ir, mut strength) = (Vec::new(), None, 0.5);
    let mut rest = args[5..].iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().expect(usage);
        match flag.as_str() {
            "--pass" => {
                let (ev, path) = value.split_once(',').expect("EV,PASS.tif");
                passes.push((ev.parse::<f32>().expect("EV"), path.to_string()));
            }
            "--ir" => ir = Some(value.clone()),
            "--dust" => strength = value.parse().expect("STRENGTH"),
            _ => panic!("{usage}"),
        }
    }

    let read = open_scan(scan.as_ref()).expect("readable scan");
    let area = ScanArea::full(
        read.width() as f32 / dpi as f32 * MM_PER_INCH,
        read.height() as f32 / dpi as f32 * MM_PER_INCH,
    );
    let save = |name: &str, image: &DynamicImage| {
        let path = format!("{out}/{name}.tif");
        crop_frame(image, dpi, area, &frame)
            .save(&path)
            .expect("writable output");
        println!("wrote {path}");
    };
    save("read", &read);

    let mut current = read.clone();
    if !passes.is_empty() {
        let mut all = vec![ScanPass {
            exposure: Exposure::default(),
            image: read,
        }];
        for (ev, path) in &passes {
            all.push(ScanPass {
                exposure: Exposure { ev: *ev },
                image: open_scan(path.as_ref()).expect("readable pass"),
            });
        }
        let merge = merge_exposures(&all, gamma).expect("mergeable passes");
        for p in &merge.passes {
            println!("pass {p:?}");
        }
        current = merge.image;
        save("merged", &current);
    }
    if let Some(ir) = ir {
        let ir = open_scan(ir.as_ref())
            .expect("readable infrared")
            .to_luma16();
        let params = DustParams::for_scan(dpi, strength);
        let cleaned = remove_dust(&current, &ir, &params).expect("dust removal");
        println!("dust {:?}", cleaned.report);
        current = cleaned.image;
        save("cleaned", &current);
    }
    let t = std::time::Instant::now();
    let shifts = channel_shifts(&current, dpi, area, &frame);
    println!("channel shifts {shifts:?} in {:?}", t.elapsed());
    let path = format!("{out}/aligned.tif");
    crop_frame_aligned(&current, dpi, area, &frame, &shifts)
        .save(&path)
        .expect("writable output");
    println!("wrote {path}");
}
