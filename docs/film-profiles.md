# Film-stock profiles

How film_scanner turns a scan of a colour negative into a positive, what a film-stock profile
is made of, and how to add one. The code is in `crates/scan-film`; the image-level parts are in
`crates/scan-process/src/negative.rs`.

The rule for every profile: **its numbers come from the manufacturer's published data, read
exactly, and every assumption is written down in the stock's module.** Nothing is tuned by eye.

## What a colour negative is

A colour negative has three layers, sensitive to red, green and blue light. Each forms a dye
in proportion to the log of its exposure: the red-sensitive layer cyan dye, the green magenta,
the blue yellow. Unexposed film is not clear: the base, fog and the **orange mask** (coloured
couplers that correct the dyes' unwanted absorptions) give it a minimum density, D-min,
densest in blue.

A manufacturer's technical datasheet describes all of this with three sets of curves:

| Curves | What they say | What we use them for |
|---|---|---|
| **Characteristic** (density vs. log exposure, one per layer, Status M) | How much dye each layer forms for each exposure: toe, straight line (gamma ≈ 0.55–0.65), shoulder. **Log H ref** marks a normally exposed grey card. | Run backwards, they turn the negative's densities back into scene light. This is the heart of the profile. |
| **Spectral sensitivity** (log sensitivity vs. wavelength, per layer) | Which colours of light each layer records. | The *Colorimetric* rendering: a 3 × 3 matrix fitted from them (see below). |
| **Spectral dye density** (D-min and a midscale neutral) | The colour of the mask and of the dyes. | Kept for calibrating scanners to Status M later. |

**Status M** is the densitometry made to see a negative the way colour print paper does, one
dye per channel. The characteristic curves are measured in it, so they say what a print would
see.

## The inversion, step by step

1. **Film base.** Frame detection finds the strips and measures the light through the bare
   film between the frames and the perforations (`FrameDetection::base`), on every film scan,
   in linear light. If no strips are found, the clearest unclipped film on the scan is used
   instead, and the app says it is an estimate. The base is never taken from the datasheet:
   it varies with processing and with the scanner.
2. **Density above base.** For each channel, `d = log10(base / light)`. This removes the
   orange mask and the scanner's own colour balance in one step.
3. **Back through the film.** `d` plus the datasheet's D-min is a Status M density on the
   layer's characteristic curve, which gives the log exposure that made it. Relative to Log H
   ref that is scene light `e`, with a grey card at 0.18. Each layer has its own curve and
   gamma, which is what keeps greys grey from the shadows to the highlights. Without this,
   simply inverting and stretching each channel gives colour casts that change with
   brightness (crossover).
   - The curves are made strictly increasing: a dip just above D-min (the blue curve of many
     negatives has one, from the masking couplers) is held level, and every part of the curve
     climbs at least `MIN_GAMMA` (0.05). The toe is very flat, and inverting it exactly would
     multiply scanner noise without bound; this caps the gain at 20×.
   - Past the ends of the plotted curve it continues with the end's own slope.
4. **Printing.** Exposure and colour balance are offsets in stops on each layer's scene light,
   like a photofinisher's printer lights. *Auto* sets them as a printer does ("integrate to
   grey"): each frame's average log exposure is taken as grey; the roll gets the **median**
   of the frames' corrections, so one frame of lawn or sea doesn't tint the rest. The
   exposure brings the typical frame's log average to a grey card.
5. **Colour.**
   - *Print* (default): each layer drives its own channel, as when the negative is printed
     on colour paper, which sees one dye per layer. This is the film's own colour, as its maker
     designed it to print.
   - *Colorimetric*: a 3 × 3 matrix from layer exposures to linear sRGB, fitted by least
     squares (with white held to white) over 186 smooth surface spectra of every hue lit by
     daylight. Each surface's layer exposures come from the datasheet's spectral
     sensitivities, and its target colour from the CIE 1931 observer, adapted to D65 with
     Bradford. The film's chemistry already boosts saturation (interimage effects), so this
     can look more saturated than a print.
6. **Tone.** Display light `e^p / (e^p + k^p)`, with `k` set so a grey card stays 0.18; `p` is
   the Contrast slider. It rolls the highlights off towards white and the shadows towards
   black, as a print does. Then sRGB encoding, at 16 bits.

The view inverts a display-sized copy of the scan, so the sliders are live. Saving inverts at
full resolution: the whole scan, or each frame after it is cut out and straightened.

## Reading a datasheet: `tools/film-curves`

Kodak's datasheets draw their curves as vector paths in the PDF, so they can be read back to a
fraction of the line width instead of being traced off a picture.
`tools/film-curves/extract_curves.py` does this from a JSON config per datasheet edition:

- `source`: the film, publication, edition, URL and the PDF's SHA-256. The script refuses a
  PDF with any other hash, so the tables can only come from the edition the config describes.
- `notes`: what the sheet says about its conditions, and anything we had to interpret.
- `page`, `log_h_ref`.
- `plots`: for each of `characteristic`, `sensitivity` and `dye_density`:
  - `x` and `y`: two `[page coordinate, value]` pairs per axis, taken from **tick marks**.
  - `curves`: which paths are which curve. A path is given as its index, a range
    `[first, last]` of consecutive paths (Kodak draws the sensitivity curves in short
    pieces), or `"index.run"` for one continuous run of a path that holds several curves.
    A curve that the plot breaks off (cyan's sensitivity dips below the axis between its blue
    bump and its main peak) is a list of pieces.
  - `floor` (sensitivity): values on the plot's floor mean "below the plot" and become
    `None`.
  - `sample`: the grid the curve is written on (0.05 log H; 5 nm).

The datasheet PDFs themselves are Kodak's and are not in the repository; the config's URL and
hash say exactly which file to fetch.

### Checks to make on a new sheet

- **The axes:** the ticks used must agree with the plot frame (the extractor's `list` command
  prints every straight line and frame), to a few hundredths of a point.
- **The sheet's own numbers:** Kodak's "Judging negative exposures" table gives the red
  density of a normally exposed grey card and of a white paper grey-scale step. The extracted
  red curve at Log H ref, and 0.70 above it, must fall in those ranges. This is a test in
  `crates/scan-film/src/tests.rs`, one row per stock.
- **Another edition**, if there is one: reading the same film from a different layout must
  give the same curves. Gold 200's 2007 and 2016 editions agree to 0.003.
- `cargo run -p scan-film --example profile STOCK_ID` prints the profile: density by stops
  from a grey card, D-min, gamma, the colorimetric matrix and how well it fits.

## Adding a stock

1. **Find the data.** Prefer the film's own technical datasheet, latest edition. If the film
   has none, find the closest documented film, and write down why it is the closest and what
   evidence there is (packaging, edge print, reviews of the two side by side).
2. **Write the config** in `tools/film-curves/`: run `extract_curves.py render SHEET.pdf PAGE
   out.png` to see the page, and `list SHEET.pdf PAGE` to find the paths and ticks. Read
   carefully what the sheet prints: Gold 100's Log H ref is printed "0.84" without its minus
   sign.
3. **Generate the tables:**
   `python tools/film-curves/extract_curves.py rust tools/film-curves/CONFIG.json SHEET.pdf > crates/scan-film/src/sheets/NAME.rs`,
   then `cargo fmt`. Add the module to `crates/scan-film/src/sheets.rs`.
4. **Write the stock** in `crates/scan-film/src/stocks/`, with a module comment that says
   where every number comes from, what is measured instead of assumed, and what differences
   to watch for. `id` is kept in users' settings: never change it once released. Add it to
   `ALL` in `stocks.rs`.
5. **Add its exposure check** to `extracted_curves_agree_with_kodaks_exposure_check`, run the
   tests, and look at the `profile` example's output.
6. **Try it on real film** if you have some, and record what the app shows for the film base
   density next to the datasheet's D-min.

## Known limits

- **The scanner is taken to see like Status M.** On the V500 the red and blue film base
  densities match the datasheet's D-min (0.264 and 0.976 against 0.251 and 0.991 on
  Kodacolor 200), but green reads lower (0.461 against 0.657): its green channel sees the
  mask differently. A real roll showed no density scale error to correct, and less than
  0.15 stop of crossover, within what its scenes explain (see the Kodacolor 200 module).
  Calibrating each scanner's channels to Status M with a step wedge or IT8 target, which
  would measure this properly, is a follow-up.

To check a stock on real film: save an uninverted 16-bit scan (untick *Invert colour
negative*, then *Save as TIFF*) and run
`cargo run --release -p scan-process --example negative_report -- SCAN.tif 400 1.8 STOCK_ID`.
It prints the film base against D-min, each frame's densities above base against the
stock's grey card and white, each frame's colour offsets, and how much they change with
level (crossover).
- **Print paper isn't modelled:** the tone curve is print-like, not a particular paper's.
- **Colour negative only:** black-and-white and slide film need their own models.
