# film_scanner

Open-source film scanning software: an alternative to tools like SilverFast and VueScan for
scanning film negatives and slides on flatbed scanners.

The goal is one GUI that covers the whole workflow:

1. **Scan**: detect scanners, preview, and capture high-bit-depth scans, including infrared
   (dust/scratch removal) and multi-exposure passes
2. **Crop**: automatically detect frames on a strip, remove borders and straighten them
3. **Invert**: turn negatives into positives using film-stock profiles (orange-mask removal)
4. **Adjust**: colour correction: white balance, levels, curves
5. **Export**: batch export with metadata

> **Status:** early development. Scanner communication and the GUI shell work; Crop, Invert,
> Adjust and Export are placeholders. See the [roadmap issue](https://github.com/drewsum/film_scanner/issues/1).

## Scanner support

| Backend | Platform | Status |
|---------|----------|--------|
| WIA (Windows Image Acquisition) | Windows | Flatbed scanning. Tested with an Epson Perfection V500 |
| Mock "Virtual Film Scanner" | All | Synthetic film strips with an IR channel, for development |
| SANE | Linux/macOS | Planned |
| Native Epson ESC/I over USB | All | Planned, needed for the V500's film adapter, 48-bit and IR |

The Epson V500's WIA driver only exposes the flatbed at up to 1200 dpi in 8-bit colour, with
no transparency unit and no infrared. Full film support for that scanner needs a native backend.

## Dust and scratch removal

Film scans with the infrared channel turned on can have their dust and scratches removed, as
Digital ICE and SilverFast iSRD do. The infrared is also saved as `<name>_ir.tif`. The
code is in `crates/scan-process`.

Film dyes are nearly transparent to infrared, so dust, hairs and scratches show as dark
specks on an otherwise flat infrared image. The defect mask is built like this:

1. Areas that are mostly dark in infrared over a few millimetres are taken for the film
   holder and skipped.
2. Each pixel is compared with the mean infrared within about 0.4 mm of it, leaving out
   pixels that are themselves dark. A pixel more than a threshold darker is a defect. The
   *Strength* slider sets the threshold, from 35% darker (0) to 10% darker (1).
3. The cyan dye of some films absorbs a little infrared, so frame detail shows faintly. A line
   fitted to infrared against the red channel, leaving out outliers such as dust, measures how
   much. The infrared is corrected by that much before step 2.
4. The mask is grown by a pixel or two, to cover the soft edges of each defect.

Masked pixels are then filled from their surroundings, working inwards from the edge of each
defect, and smoothed. *Show repaired areas in red* displays the mask. *Save as TIFF* saves
the cleaned image while removal is on; the infrared and any extra exposures are saved as
scanned.

Known limits:

- Silver-based black-and-white film and Kodachrome block infrared, so their infrared shows
  the image itself. Turn removal off for them.
- Repairs are smooth, without grain. That's invisible on dust specks, but may show on large
  defects.
- Defects right at the edge of the film strip, next to the holder, are not detected.
- Scratches into the emulsion that don't block infrared aren't found: infrared only shows
  what blocks light, such as dust, hairs and scratches on the film base.

## Building

Requires Rust 1.95+ (`rustup` recommended). On Windows, the MSVC toolchain and the Visual
Studio C++ Build Tools are needed.

On Linux, install the GUI dependencies first:

```sh
sudo apt-get install pkg-config libgtk-3-dev libxkbcommon-dev libxcb-render0-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev libwayland-dev
```

Then:

```sh
cargo run --release -p film-scanner
```

### Windows program

To build a standalone `dist\Film Scanner.exe` that you can launch by double-clicking it, run
this from PowerShell. The two switches add Start Menu and Desktop shortcuts:

```powershell
.\scripts\build-windows.ps1 -StartMenu -Desktop
```

The release program opens without a console window. With a V500 attached, it asks for
administrator rights once at launch.

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Hardware tests talk to a real scanner and are ignored by default. To run them with a WIA
scanner attached:

```sh
cargo test -p scan-wia -- --ignored --nocapture
# FILM_SCANNER_TEST_DEVICE=V500 selects a device by name; FILM_SCANNER_TEST_SOURCE=film
# scans from the transparency unit where available.
```

### Layout

- `crates/scan-core`: backend-agnostic scanner traits and types (capabilities, scan
  requests with IR and multi-exposure passes), the mock backend, and TIFF export
- `crates/scan-process`: processing of scans, independent of the scanner: infrared dust
  and scratch removal
- `crates/scan-wia`: Windows WIA backend
- `crates/app`: the `film-scanner` egui application

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
