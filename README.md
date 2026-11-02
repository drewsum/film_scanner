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

> **Status:** early development. Scanning and cropping work; Invert, Adjust and Export are
> placeholders. See the [roadmap issue](https://github.com/drewsum/film_scanner/issues/1).

## Scanner support

| Backend | Platform | Status |
|---------|----------|--------|
| WIA (Windows Image Acquisition) | Windows | Flatbed scanning. Tested with an Epson Perfection V500 |
| Mock "Virtual Film Scanner" | All | Synthetic film strips with an IR channel, for development |
| Native Epson V500 over USB | Windows | Flatbed and film, up to 6400 dpi, 16-bit, infrared and multi-exposure |
| SANE | Linux/macOS | Flatbed and film with any SANE driver (see below). Tested with SANE's simulated scanner |

The Epson V500's WIA driver only exposes the flatbed at up to 1200 dpi in 8-bit colour, with
no transparency unit and no infrared, hence the native backend.

## SANE (Linux and macOS)

[SANE](http://www.sane-project.org/) drives most flatbed scanners on Linux, and on macOS
through Homebrew or MacPorts. Install it, check your scanner shows up in `scanimage -L`, and
it appears in the app's device list:

```sh
sudo apt-get install libsane1 sane-utils   # Debian/Ubuntu
brew install sane-backends                  # macOS
```

`libsane` is loaded when the app starts, so it isn't needed to build, and without it the app
simply lists no SANE scanners. Set `FILM_SCANNER_LIBSANE` to a path to load it from
somewhere unusual.

The backend reads SANE's standard options: the source (flatbed, or a transparency unit or
film adapter, by name; document feeders are skipped), colour mode, bit depth, resolution and
scan area. SANE has no standard options for infrared or exposure, so SANE scans are a single
pass without infrared. The code is in `crates/scan-sane`.

## Multi-exposure

*Extra +1 EV pass* in the Scan panel scans film twice, the second time a stop brighter, and
merges the two, as SilverFast's Multi-Exposure does. Little light gets through the densest
parts of a frame (a negative's highlights, a slide's shadows), so the scanner's noise covers
them; the brighter pass lifts them clear of it but clips the thin parts. The code is in
`crates/scan-process/src/hdr.rs`.

The merge works in linear light (the scan's tone curve is undone first):

1. Each pass is a separate scan, so the brighter one may land a little off. The shift whose
   gradients correlate best with the first pass's is found, to a fraction of a pixel, leaving
   out edges near clipping.
2. The brighter pass's gain is measured per channel, with a line fitted to its values against
   the first pass's where neither is noisy or clipped. It is rarely exactly 2x.
3. Where the brighter pass is well short of clipping, it replaces the first, scaled back by its
   gain. Its weight fades out smoothly as it nears clipping, the same for all channels of a
   pixel, so there is no seam and no colour shift.

The Result panel shows the measured exposure and how much of the image came from the brighter
pass; *Merge exposures* turns the merge off to compare. Dust removal and *Save as TIFF* use the
merged image while it is on; the brighter pass is also saved as scanned, as
`<name>_ev+1.0.tif`.

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

## Frames: detection, cropping and straightening

After a film scan, the **Crop** step finds the frames on it. The code is in
`crates/scan-process/src/frames.rs`. It is built for strips of 35 mm negatives in a holder,
such as the V500's two strips of six:

1. **Strips.** Film lets light through and the holder doesn't, so the strips are the large
   bright areas of the scan. Their shape says whether they run down or across the scan. Their
   long edges are found every millimetre and fitted with lines, which give each strip's
   exact angle. That angle straightens its frames.
2. **Film base.** The bare film between the perforations and the frames gives the colour of
   unexposed film, which is also what lies between frames.
3. **Frames.** Cameras advance the film by a nearly fixed distance (38 mm on 35 mm film), so
   a row of frames at one spacing is fitted to the whole strip: the one whose gaps look most
   like bare film and whose frames least. Each frame edge is then moved to where the scan
   shows it nearby.

Dark and underexposed frames are the hard case, since their edges can look just like the film
around them. They still get their place from the spacing of the frames around them, and are
marked **?** to check. Frames as bare as the film base, such as the unexposed end of a roll,
are left out. Strips can be short, and there can be fewer than 12 frames.

Every frame can be edited. Drag a frame to move it, its handles to resize it, or the round
handle to turn it. The side panel sets the size and angle exactly, and adds or deletes frames.
*Detect frames* starts over. Frames are kept in millimetres on the scanner bed, so frames
found on a preview fit the full scan of the same film. Once edited, a new scan doesn't
replace them.

*Save frames as TIFF* saves each frame straightened, at the scan's bit depth, as
`<name>_01.tif`, `<name>_02.tif` and so on. It uses the cleaned and merged image when those
are on.

The **⟲ ⟳** buttons above the image turn the whole scan in quarter turns. Saved scans and
frames are turned the same way, and the app remembers the turn between sessions, since film
goes in the holder the same way each time.

Not yet covered: mounted slides, uncut slide film, medium format and half-frame. Their frames
can be added by hand.

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
- `crates/scan-process`: processing of scans, independent of the scanner: multi-exposure
  merging, infrared dust and scratch removal, and frame detection and cropping
- `crates/scan-wia`: Windows WIA backend
- `crates/scan-epson`: native USB backend for the Epson Perfection V500 (Windows)
- `crates/scan-sane`: SANE backend (Linux, macOS)
- `crates/app`: the `film-scanner` egui application

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
