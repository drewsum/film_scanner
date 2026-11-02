# Editing frames: the Adjust step

The Adjust step edits each frame on its own, after the Invert step has turned the roll into
positives. The editing code is in `crates/scan-process/src/adjust.rs`, the app's side in
`crates/app/src/edit.rs` and `state.rs`.

## What is per roll and what is per frame

- **Per roll (Invert step):** the film stock, the film base, and the *printing*: exposure,
  color balance, contrast and Print or Colorimetric color. *Auto* there sets the roll's
  printing from all its frames.
- **Per frame (Adjust step):** everything below. Each detected frame has its own
  `Adjustments`, and so does the whole scan (used by *Save as TIFF*). The defaults change
  nothing.

## The order of the edits

`adjust::develop` makes a frame's positive. These are the steps, in order:

1. **Exposure and color balance, in scene light.** They are added to the roll's printing
   (`Adjustments::printing`), and the frame is inverted with the sum. The offsets work on
   the film's scene light, through its characteristic curves, before the print tone curve.
   That is how a photofinisher corrects one frame's printer lights, and it keeps a frame's
   correction as accurate as the roll's: a frame balanced this way stays neutral from the
   shadows to the highlights. On a positive (a flatbed scan, or film left uninverted) the
   same offsets are gains on the scan's linear light.
2. **Grain and color noise reduction.** A guided filter (He, Sun and Tang, 2010) smooths
   areas that vary less than the strength allows and keeps edges that vary more. It works
   separately on brightness (the grain) and on color, the two chroma axes over twice the
   radius (blotchy color noise).
3. **Tone**, one table per channel: levels (the channel's own, then RGB), curves (likewise),
   brightness, then contrast.
   - **Levels:** input black and white, a midtone gamma (above 1 lightens), and output
     black and white, on 0–255.
   - **Curves:** a monotone cubic spline (Fritsch–Carlson) through the points. It never
     overshoots between them, so a flat stretch stays flat.
   - **Brightness** raises or lowers the midtones and keeps black and white.
   - **Contrast** blends towards a sigmoid S-curve about the middle, or towards its
     inverse, and keeps black, white and the middle.
4. **Saturation and selective color.** Colors are split into their mean and a chroma
   vector, whose angle is the hue (red 0°, yellow 60°, and so on). Saturation scales the
   chroma, so greys stay grey. Selective color has six hue ranges (reds, yellows, greens,
   cyans, blues, magentas). Each range is a raised cosine 120° wide, and the ranges add up
   to exactly 1 at every hue, so neighbouring colors blend smoothly. For each range, hue
   turns the chroma, saturation scales it, and lightness moves the color towards white or
   black. Hue and lightness fade out towards grey.
5. **Sharpening.** An unsharp mask of brightness alone, added equally to every channel, so
   it adds no color fringes. A soft threshold leaves differences smaller than the threshold
   alone, so grain isn't sharpened.
6. **Orientation.** The frame is mirrored first if asked, then turned: the scan's own turn
   and then the frame's.

Noise reduction and sharpening are sized in **pixels of the saved frame**. The view works
on a preview at most 2048 pixels on its longer side, and scales their radii to it, so it
shows what is saved. Zoom in to judge sharpening.

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

## Frames and their edits

- Selecting a frame in the list (or in the Crop step) shows it. Frames with edits are
  marked `*`.
- A new scan of the same film detects the frames again. Each new frame takes the edits of
  the old frame whose middle is nearest it, within half the frame's shorter side, so edits
  made on a preview carry over to the final scan. Frames edited by hand in the Crop step
  are kept as they are, with their edits.
- *Copy*, *Paste* and *Paste to all* copy every edit but the turn and mirror, which belong
  to the frame. *Reset* undoes every edit but those.
- *Before* shows the frame with none of its Adjust edits: inverted with the roll's printing
  alone.
- *Save frames as TIFF* saves each frame with its own edits, as 16-bit sRGB. *Save as TIFF*
  saves the whole scan with the whole scan's edits. The whole scan has no turn of its own,
  so its infrared and extra exposure files stay aligned with it.
