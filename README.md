# film_scanner

Open-source film scanning software: an alternative to tools like SilverFast and VueScan for
scanning film negatives and slides on flatbed scanners.

The goal is one GUI that covers the whole workflow:

1. **Scan**: detect scanners, preview, and capture high-bit-depth scans, including infrared
   (dust/scratch removal) and multi-exposure passes
2. **Crop**: automatically detect frames on a strip, remove borders and straighten them
3. **Invert**: turn negatives into positives using film-stock profiles (orange-mask removal),
   and black-and-white negatives and slides
4. **Adjust**: edit each frame on its own: color, tone, levels, curves, selective color,
   grain reduction, sharpening, straightening, spot healing
5. **Export**: save the roll's frames into a folder, with the metadata
   [film_roll_publisher](https://github.com/drewsum/film_roll_publisher) uploads them with

A roll (the scan, its frames and every edit) can be saved and opened again later.

> **Status:** early development. Scanning, cropping, inverting color negatives, editing
> each frame and exporting the roll work. See the [roadmap issue](https://github.com/drewsum/film_scanner/issues/1).

## Scanner support

| Backend | Platform | Status |
|---------|----------|--------|
| WIA (Windows Image Acquisition) | Windows | Flatbed scanning. Tested with an Epson Perfection V500 |
| Mock "Virtual Film Scanner" | All | Synthetic film strips with an IR channel, for development |
| Native Epson V500 over USB | Windows | Flatbed and film, up to 6400 dpi, 16-bit, infrared and multi-exposure |
| SANE | Linux/macOS | Flatbed and film with any SANE driver (see below). Tested with SANE's simulated scanner |

The Epson V500's WIA driver only exposes the flatbed at up to 1200 dpi in 8-bit color, with
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
film adapter, by name; document feeders are skipped), color mode, bit depth, resolution and
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
   pixel, so there is no seam and no color shift.

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
2. **Film base.** The bare film between the perforations and the frames gives the color of
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

*Reverse order* numbers the frames from the other end of the film, for cameras that wind it
the other way. It stays on for later scans and detections, and each frame keeps its edits.

*Save frames as TIFF* saves each frame straightened, at the scan's bit depth, as
`<name>_01.tif`, `<name>_02.tif` and so on. It uses the cleaned and merged image when those
are on. TIFFs carry the scan's resolution, an sRGB profile, and the software and time
that made them; JPEGs carry the resolution and profile. Frames are cut with a Lanczos kernel, which keeps the grain a simpler one would
smooth away. A frame that isn't turned is copied pixel for pixel. Each frame's red and blue
are lined up with its green to a fraction of a pixel, measured from the frame itself. The
V500's color rows put them 0.2–0.5 px apart along the carriage's travel.

The **⟲ ⟳** buttons above the image turn the whole scan in quarter turns. Saved scans and
frames are turned the same way, and the app remembers the turn between sessions, since film
goes in the holder the same way each time.

Not yet covered: mounted slides, uncut slide film, medium format and half-frame. Their frames
can be added by hand.

## Inverting negatives: film-stock profiles

The **Invert** step turns color negatives into positives with a profile of the film stock,
like SilverFast's NegaFix. Profiles are built from the manufacturer's published data: each
layer's characteristic curve, read exactly from the vector graphics of the datasheet's PDF, is
run backwards to recover the light that exposed it. That is what keeps greys grey from the
shadows to the highlights, without auto-levels. The orange mask is removed by dividing out
the bare film base, which frame detection measures on every film scan.

| Film stock | Data |
|---|---|
| Kodak Kodacolor 200 | No datasheet exists (it is reported to be ColorPlus 200 renamed, which has none either); uses Kodak Gold 200's, E-7022 |
| CineStill 50D | None published; uses Kodak VISION3 50D's (H-1-5203t), the film it is reported to be without rem-jet |
| CineStill 400D | None published; uses Kodak VISION3 250D's (H-1-5207t), which it is reported to be built on |
| CineStill 800T | None published; uses Kodak VISION3 500T's (H-1-5219t), the film it is reported to be without rem-jet |
| Flic Film Elektra 100 | None published; uses Kodak AEROCOLOR IV 2460's (AS-2652), the aerial film it is reported to be. No orange mask |
| Fujifilm 200 | Its own datasheet, AF3-0261E (2022) |
| Fujifilm 400 | Its own datasheet, AF3-0262E (2023) |
| Harman Phoenix 200, Phoenix II 200 | None published: **generic**, a typical C-41 film averaged from four Kodak datasheets |
| Kodak Ektar 100 | Its own datasheet, E-4046 (2025) |
| Kodak Gold 100 | Its own datasheet, E-7022 (2007 edition) |
| Kodak Gold 200 | Its own datasheet, E-7022 (2016) |
| Kodak Portra 400 | Its own datasheet, E-4050 (2025) |
| Kodak UltraMax 400 | Its own datasheet, E-7023 (2016) |
| Reto Amber D100 | None published; uses Kodak VISION3 50D's (H-1-5203t), the film it is reported to be without rem-jet |
| Ilford HP5 Plus (black-and-white) | Its own datasheet, Ilford technical information (2010 edition) |

The motion picture films' curves are for ECN-2; developed in C-41, as CineStill and Reto
sell them, they have more contrast than the curves predict.

The step has the film stock, exposure and color balance in stops (like a photofinisher's
printer), contrast, and *Print* or *Colorimetric* color. *Auto* balances the roll as a
printer does: each frame's average is taken as grey, and the roll gets the median of the
frames' corrections. The view, *Save as TIFF* and *Save frames as TIFF* all give positives
(16-bit sRGB) while *Make positives* is on.

Two other kinds of film are inverted differently:

- **Black-and-white negatives** are inverted against the measured film base on their
  film's characteristic curve (Ilford HP5 Plus), or a generic black-and-white film's
  straight line for any other film. *Grey from* picks which of the scan's channels make
  the grey (all of them is the least noisy), and *Toning* tints the print sepia, selenium
  or blue.
- **Slides** have no profiles yet: they are printed as they are, with the printing's
  exposure, balance and contrast.

How profiles work, and how to add a stock, is in
[docs/film-profiles.md](docs/film-profiles.md). `cargo run -p scan-film --example profile`
prints a profile, and `cargo run --release -p scan-process --example negative_report --
SCAN.tif` measures a saved (uninverted) film scan against it.

## Editing frames

The **Adjust** step edits every frame on its own, with the tools SilverFast has for a
frame: Auto, grey, black and white pickers, histogram and levels, curves, color balance,
temperature and tint, brightness and contrast, highlights, shadows and clarity, saturation
and vibrance, selective color, grain and color noise reduction, unsharp masking, light
falloff and vignette, straightening (with a level tool), a film border, spot healing, and
turning and mirroring. Pick a frame from the strip along the bottom (or the list, or the
whole scan); the view shows it cut out, straightened and developed. *Before*, *Split* and
*Clipping* compare it with no edits and mark what is clipped.

The roll's printing in the Invert step stays the starting point. A frame's exposure and
color balance are added to it **in the film's scene light, before the print curve**, the
way a photofinisher corrects each frame's printer lights, so a frame's color correction
uses the film's profile just like the roll's does. Its *Auto* balances the frame as the
printer would on its own, then stretches it to full range. Newly detected frames get Auto
straight away. The rest edits the positive, starting from light sharpening that gives back
the fine grain cutting a frame straightened softens.

Each frame's edits are kept through a new scan of the same film (they follow the frame
detection finds in their place), can be undone (Ctrl+Z), copied to other frames or saved
as presets, and are saved with it by *Save frames as TIFF*. *Auto all* sets each frame
on its own; *Match roll* brings every frame to the roll's typical exposure and color
instead. Untick a frame in the strip to leave it out of exports. How it all works, and the
keyboard shortcuts, are in [docs/editing.md](docs/editing.md).

## Saving a roll

*Save roll...* (Ctrl+S) saves the scan, its frames and every edit, so the roll can be
opened again later (*Open roll...*, Ctrl+O) to change a frame and export it again. A roll
file (`roll180.filmroll`) is text, and the scan is kept beside it as scanned, in the same
files *Save as TIFF* writes (`roll180.tif`, and `roll180_ir.tif` and so on): merging and
dust removal are done again when it is opened. Saving to the same roll again writes only
the roll file.

## Exporting a roll

The **Export** step saves every frame, each with its own edits and turn, into one folder
as `roll180_01.jpg`, `roll180_02.jpg`, ... in the Crop step's frame order: *File names* sets
how they start (`roll` by default), and shows the names it will give.
JPEG (with a quality) and 8-bit PNG are what
[film_roll_publisher](https://github.com/drewsum/film_roll_publisher) uploads; 16-bit TIFF
is there for archiving. Every file is tagged with an sRGB color profile, so viewers and
Immich know its colors. Frames unticked in the strip are left out, and the rest numbered
without gaps. Files already in the folder are pointed out before they're replaced.

Tick *Write metadata.yml for film_roll_publisher* to save the roll's details next to the
frames, in the publisher's format 0:

- **Roll:** name (the Immich album), description, date captured, date scanned (today by
  default), public, and GPS.
- **Film:** filled in from the Invert step's film profile (Kodak, Kodacolor, 200), and
  editable for film without one; development notes.
- **Camera:** make, model, lens, f-number, focal length, flash and filters. These are
  remembered for the next roll, with the folder and format.

The app checks what the publisher checks before writing anything: the required fields,
MM/DD/YYYY dates, a whole-number ISO, and one f-number and focal length rather than a
range. Then run `rollpublisher` in the export folder. Uploading from the app itself is
[issue #23](https://github.com/drewsum/film_scanner/issues/23).

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
- `crates/scan-film`: film-stock profiles from manufacturers' datasheets, and the model that
  inverts negatives with them (no dependencies)
- `crates/scan-process`: processing of scans, independent of the scanner: multi-exposure
  merging, infrared dust and scratch removal, frame detection and cropping, inverting
  negatives image by image, and editing frames
- `tools/film-curves`: reads a datasheet's curves out of its PDF into `scan-film`'s tables
- `crates/scan-wia`: Windows WIA backend
- `crates/scan-epson`: native USB backend for the Epson Perfection V500 (Windows)
- `crates/scan-sane`: SANE backend (Linux, macOS)
- `crates/app`: the `film-scanner` egui application, the Export step's `metadata.yml`,
  and roll files

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
