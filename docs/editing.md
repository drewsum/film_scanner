# Editing frames: the Adjust step

The Adjust step edits each frame on its own, after the Invert step has turned the roll into
positives. The editing code is in `crates/scan-process/src/adjust.rs`, the app's side in
`crates/app/src/edit.rs` and `state.rs`.

## What is per roll and what is per frame

- **Per roll (Invert step):** the film's type and stock, the film base, and the
  *printing*: exposure, color balance, contrast and Print or Colorimetric color. *Auto*
  there sets the roll's printing from all its frames.
- **Per frame (Adjust step):** everything below. Each detected frame has its own
  `Adjustments`, and so does the whole scan (used by *Save as TIFF*). The defaults change
  nothing.

## The order of the edits

Before anything else, the frame's **geometry** decides what is cut from the scan
(`adjust::framed`): its rectangle grown by its *border* on every side (to keep the film's
edge, rebate and sprocket holes), then turned by its *straightening* and shrunk, keeping
its shape, just enough that no corner is empty. Then its **spots** are healed: each is a
circle on the scanner bed (so it stays put if the frame is moved or straightened), filled
in from just outside it, the way dust removal fills its defects.

`adjust::develop` then makes the frame's positive. These are the steps, in order:

1. **Exposure, color balance, temperature and tint, in scene light.** They are added to the
   roll's printing (`Adjustments::printing`), and the frame is inverted with the sum. The
   offsets work on the film's scene light, through its characteristic curves, before the
   print tone curve. That is how a photofinisher corrects one frame's printer lights, and
   it keeps a frame's correction as accurate as the roll's: a frame balanced this way stays
   neutral from the shadows to the highlights. Temperature moves red and blue opposite
   ways (half a stop each at full), and tint moves green against both; neither changes
   the overall level. On a positive (a flatbed scan, or film left uninverted) the same
   offsets are gains on the scan's linear light.
2. **Grain and color noise reduction.** A guided filter (He, Sun and Tang, 2010) smooths
   areas that vary less than the strength allows and keeps edges that vary more. It works
   separately on brightness (the grain) and on color, the two chroma axes over twice the
   radius (blotchy color noise).
3. **Light falloff:** the edges and corners brightened, up to twice in the corners, for a
   scanner's or lens's light falling off.
4. **Highlights and shadows.** Each pixel is scaled by how light its surroundings are: a
   guided filter of brightness about 3% of the frame across, which follows strong edges,
   so an edge between a dark and a light area isn't haloed. Shadows lift dark
   surroundings (up to 2.6 times at black), highlights darken light ones (down to 0.4
   times at white). Scaling keeps the contrast within an area.
5. **Tone**, one table per channel: levels (the channel's own, then RGB), curves (likewise),
   brightness, then contrast.
   - **Levels:** input black and white, a midtone gamma (above 1 lightens), and output
     black and white, on 0–255.
   - **Curves:** a monotone cubic spline (Fritsch–Carlson) through the points. It never
     overshoots between them, so a flat stretch stays flat.
   - **Brightness** raises or lowers the midtones and keeps black and white.
   - **Contrast** blends towards a sigmoid S-curve about the middle, or towards its
     inverse, and keeps black, white and the middle.
6. **Clarity:** local contrast. Each pixel's difference in brightness from its
   surroundings (a guided filter about 1.2% of the frame across) is added again, up to
   twice, most in the midtones and not at all at black or white. The guided filter keeps
   strong edges out of the difference, so they get no halos.
7. **Saturation, vibrance and selective color.** Colors are split into their mean and a
   chroma vector, whose angle is the hue (red 0°, yellow 60°, and so on). Saturation
   scales the chroma, so greys stay grey. Vibrance scales it more where it is low, and
   less on skin (orange-red hues). Selective color has six hue ranges (reds, yellows,
   greens, cyans, blues, magentas). Each range is a raised cosine 120° wide, and the
   ranges add up to exactly 1 at every hue, so neighbouring colors blend smoothly. For
   each range, hue turns the chroma, saturation scales it, and lightness moves the color
   towards white or black. Hue and lightness fade out towards grey.
8. **Vignette:** the corners darkened or lightened, for effect.
9. **Sharpening.** An unsharp mask of brightness alone, added equally to every channel, so
   it adds no color fringes. A soft threshold leaves differences smaller than the threshold
   alone, so grain isn't sharpened.
10. **Orientation.** The frame is mirrored first if asked, then turned: the scan's own turn
    and then the frame's.

Noise reduction and sharpening are sized in **pixels of the saved frame**. The view works
on a preview at most 2048 pixels on its longer side, and scales their radii to it, so it
shows what is saved. Zoom in to judge sharpening. Highlights, shadows and clarity are
sized by the frame instead, so they look the same at any size.

## Black-and-white negatives and slides

A **black-and-white negative** is inverted against its measured film base on its film's
characteristic curve when the film has a profile (Ilford HP5 Plus), or otherwise on a
generic black-and-white film's straight line (a grey card at density 0.75 above base,
gamma 0.62), each channel on its own, then mixed to grey by
*Grey from*, printed through the same tone curve as color film, and toned: each channel's
print value is raised to a power, so black and white stay and the midtones take the tint.
Color balance, temperature and tint don't apply. A **slide** is printed as it is: the
light through it, with the printing's exposure and balance, is the scene's, and the
printing's contrast steepens or flattens it about grey. Slides have no profiles yet.

## Auto and the pickers

- **Auto** (per frame):
  - On a negative it gives the frame the printing a photofinisher's printer would give it
    on its own: the frame's average log exposure is taken as grey (`scan_film::auto_printing`
    on that frame alone). That printing is stored as offsets from the roll's.
  - A positive is balanced to grey the same way, in linear light, and keeps its exposure.
  - Then master levels stretch the frame to full range, clipping 0.1% at each end. One
    stretch is used for all three channels, so the balance just set is kept.
  - The whole scan of a negative keeps the roll's printing (the Invert step's Auto).
- **Grey picker:** click something that should be neutral. The balance changes so that
  point's scene light (or linear light, on a positive) is equal in all three channels,
  keeping its level. It averages a 5 × 5 pixel area.
- **Black and white pickers:** set each channel's levels black or white point to the
  clicked color, before levels. That makes the point black (or white) and neutral.
- **Auto all** runs Auto on every frame, each on its own.
- **Match roll** pulls every frame to the roll's typical frame instead: each frame's
  average color, measured through the film at neutral printing, is moved to the median
  frame's. Frames shot in the same light then print alike, rather than each being made
  neutral on its own (which tints a frame of green lawn magenta). Temperature, tint and
  every other edit are kept.

## Tools on the view

- **Heal** (H): click a speck to fill it in, right-click a healed one to undo it. The
  brush should cover the whole speck, as it fills from what is just outside it. For dust
  and hairs the infrared pass missed, and for film infrared can't see through
  (black-and-white silver film, Kodachrome).
- **Level** (L): drag along a horizon or an upright, and the frame is straightened along
  it (whichever of level or upright the line is nearer).
- **Before** (B) shows the frame with none of its Adjust edits; **Split** (S) shows it
  before on the left and after on the right of a line you can drag; **Clipping** (C) marks
  pixels clipped to white in any channel in red, and to black in blue.

## Frames and their edits

- Selecting a frame in the strip along the bottom, or in the list (or in the Crop step)
  shows it. Frames with edits are marked `*`, and skipped ones `-`. Untick a frame in the
  strip (or press X) to leave it out of exports and saves; the rest are numbered without
  gaps.
- A new scan of the same film detects the frames again. Each new frame takes the edits of
  the old frame whose middle is nearest it, within half the frame's shorter side, so edits
  made on a preview carry over to the final scan. Frames edited by hand in the Crop step
  are kept as they are, with their edits.
- *Copy*, *Paste* and *Paste to all* copy every edit but the frame's geometry: its turn,
  mirroring, straightening and healed spots, which belong to it. *Reset* undoes every
  edit but those. **Presets** save a frame's edits (without its geometry) under a name,
  kept between sessions, to apply to a frame or the whole roll.
- **Undo** and **Redo** (Ctrl+Z, Ctrl+Y) step through each frame's own changes; a slider
  dragged, or a curve point moved, is one change.
- *Save frames as TIFF* saves each frame with its own edits, as 16-bit sRGB, tagged with an
  sRGB profile. *Save as TIFF* saves the whole scan with the whole scan's edits. The whole
  scan has no turn of its own, so its infrared and extra exposure files stay aligned with
  it.

## Keyboard shortcuts

| Keys | In the Adjust step |
|---|---|
| PgUp, PgDn (or ←, → when no slider has focus) | Previous and next frame (also in Export) |
| Ctrl+Z, Ctrl+Y (or Ctrl+Shift+Z) | Undo, redo |
| Ctrl+C, Ctrl+V | Copy and paste edits |
| A | Auto |
| B, S, C | Before, split, clipping |
| G, H, L | Grey picker, heal, level |
| [ and ] | Turn anticlockwise, clockwise |
| X | Export the frame or skip it |
| Esc | Put the tool down |
| Ctrl+S, Ctrl+O | Save the roll, open a roll (any step) |
