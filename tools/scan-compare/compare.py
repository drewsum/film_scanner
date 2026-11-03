"""Compare scans of the same frame: sharpness along each axis, color registration, tone and
grain, with the TIFFs' color profile and resolution tags.

Usage:

    python compare.py SCAN.tif [SCAN.tif ...]

Each file gets one row per measure. The sharpness and registration measures use a square from
the middle of the frame, so the files needn't be cropped alike; the tone measures use the
whole frame, so they should be. Values are on 0–1 of full scale in each file's own encoding,
so files in different color spaces (sRGB, Adobe RGB) compare only roughly on tone.

- `hf x/y`: high-frequency energy (0.3–0.49 cycles per pixel) along x over along y, of
  brightness and of each channel. Film grain is round, so where grain dominates a sharp scan
  is near 1, but a soft scan's high frequencies are the scene's edges, which are rarely the
  same both ways: compare files of the same frame only.
- `grain corr`: how alike neighbouring pixels' grain is along x and y, per channel, in the
  flattest mid-tone tiles (the image less a 3 px blur). Independent of the scene: raw scans
  from the V500 at 1,600 dpi are about 0.2; smoothing raises it (a bilinear resample to about
  0.5), sharpening lowers it (SilverFast's are about 0).
- `R-G`, `B-G`: each channel's shift from green along x and y, in pixels.
- `finest`: brightness energy around 0.49 cycles per pixel, relative to the first file.
- `p1`, `p99`: the 1st and 99th percentiles of each channel (black and white points).
- `contrast`: the standard deviation of brightness; `local`: of brightness less a 20 px blur.
- `chroma`: mean of max − min channel. `grain`: the 2nd percentile of 8 × 8 block standard
  deviations, the noise of the flattest areas.

Needs NumPy, SciPy, tifffile and Pillow (`pip install numpy scipy tifffile pillow`).
"""

import io
import sys

import numpy as np
import tifffile
from PIL import ImageCms
from scipy.ndimage import gaussian_filter

# Side of the square from the middle of the frame used for sharpness and registration.
SIDE = 800
# High-frequency band, in cycles per pixel.
BAND = (0.3, 0.49)
# Largest channel shift looked for, in pixels.
MAX_SHIFT = 6


def load(path):
    """The image as floats in 0–1, with its profile's description and resolution tags."""
    with tifffile.TiffFile(path) as tif:
        page = tif.pages[0]
        image = page.asarray()
        tags = page.tags
        profile = "none"
        if "InterColorProfile" in tags:
            icc = bytes(tags["InterColorProfile"].value)
            profile = ImageCms.getProfileDescription(ImageCms.ImageCmsProfile(io.BytesIO(icc)))
        res = "none"
        if "XResolution" in tags:
            num, den = tags["XResolution"].value
            unit = tags["ResolutionUnit"].value if "ResolutionUnit" in tags else 2
            res = f"{num / den:g} {['', 'none', 'dpi', 'dpcm'][int(unit)]}".strip()
    scale = np.iinfo(image.dtype).max
    return image[..., :3].astype(np.float64) / scale, image.dtype, profile.strip(), res


def brightness(image):
    return image @ np.array([0.2126, 0.7152, 0.0722])


def middle(image):
    h, w = image.shape[:2]
    return image[h // 2 - SIDE // 2 : h // 2 + SIDE // 2, w // 2 - SIDE // 2 : w // 2 + SIDE // 2]


def spectrum(plane):
    window = np.outer(np.hanning(SIDE), np.hanning(SIDE))
    f = np.fft.fftshift(np.fft.fft2((plane - plane.mean()) * window))
    return np.abs(f) ** 2


def axis_ratio(plane):
    """High-frequency energy along x over along y, from bands about each axis."""
    p = spectrum(plane)
    c = SIDE // 2
    lo, hi = (int(f * SIDE) for f in BAND)
    along_x = p[c - 5 : c + 6, c + lo : c + hi].mean()
    along_y = p[c + lo : c + hi, c - 5 : c + 6].mean()
    return along_x / along_y


def finest(plane):
    """Mean energy in a ring around 0.49 cycles per pixel."""
    p = spectrum(plane)
    yy, xx = np.indices(p.shape)
    r = np.hypot(yy - SIDE // 2, xx - SIDE // 2) / SIDE
    return p[(r > 0.47) & (r < 0.5)].mean()


def shift(ref, other, axis):
    """How far `other` is shifted from `ref` along `axis`, by correlating their gradients."""
    a = np.diff(ref, axis=axis)
    b = np.diff(other, axis=axis)
    shifts = range(-MAX_SHIFT, MAX_SHIFT + 1)
    corr = [np.sum(a * np.roll(b, s, axis=axis)) for s in shifts]
    i = int(np.argmax(corr))
    frac = 0.0
    if 0 < i < len(corr) - 1:
        y0, y1, y2 = corr[i - 1 : i + 2]
        frac = 0.5 * (y0 - y2) / (y0 - 2 * y1 + y2)
    return -(shifts[i] + frac)


def grain(plane, block=8):
    h, w = (s // block * block for s in plane.shape)
    blocks = plane[:h, :w].reshape(h // block, block, w // block, block).swapaxes(1, 2)
    return np.percentile(blocks.reshape(-1, block * block).std(axis=1), 2)


def grain_correlation(image, tile=32):
    """Neighbour correlation of each channel's grain along x and y, in flat mid-tone tiles."""
    out = []
    for c in range(3):
        plane = image[..., c]
        low = gaussian_filter(plane, 3)
        residual = plane - low
        h, w = (s // tile * tile for s in plane.shape)

        def tiles(a):
            return a[:h, :w].reshape(h // tile, tile, w // tile, tile).swapaxes(1, 2)

        gy, gx = np.gradient(low)
        slope = tiles(np.hypot(gx, gy)).mean(axis=(2, 3))
        level = tiles(low).mean(axis=(2, 3))
        flat = (slope < np.percentile(slope, 15)) & (level > 0.15) & (level < 0.85)
        picked = tiles(residual)[flat]
        if len(picked) == 0:
            out.append(f"{'RGB'[c]} -")
            continue
        cx = np.mean([np.corrcoef(t[:, :-1].ravel(), t[:, 1:].ravel())[0, 1] for t in picked])
        cy = np.mean([np.corrcoef(t[:-1].ravel(), t[1:].ravel())[0, 1] for t in picked])
        out.append(f"{'RGB'[c]} x {cx:.2f} y {cy:.2f}")
    return " | ".join(out)


def measure(path):
    image, dtype, profile, res = load(path)
    y = brightness(image)
    mid = middle(image)
    mid_y = brightness(mid)
    h, w = image.shape[:2]
    return {
        "size": f"{w}x{h} {dtype}",
        "profile": profile,
        "resolution": res,
        "hf x/y": " ".join(
            [f"Y {axis_ratio(mid_y):.2f}"]
            + [f"{'RGB'[c]} {axis_ratio(mid[..., c]):.2f}" for c in range(3)]
        ),
        "grain corr": grain_correlation(image),
        "R-G": "x {:+.2f} y {:+.2f}".format(
            shift(mid[..., 1], mid[..., 0], 1), shift(mid[..., 1], mid[..., 0], 0)
        ),
        "B-G": "x {:+.2f} y {:+.2f}".format(
            shift(mid[..., 1], mid[..., 2], 1), shift(mid[..., 1], mid[..., 2], 0)
        ),
        "_finest": finest(mid_y),
        "p1": " ".join(f"{np.percentile(image[..., c], 1):.3f}" for c in range(3)),
        "p99": " ".join(f"{np.percentile(image[..., c], 99):.3f}" for c in range(3)),
        "contrast": f"{y.std():.3f}",
        "local": f"{(mid_y - gaussian_filter(mid_y, 20)).std():.4f}",
        "chroma": f"{(image.max(axis=-1) - image.min(axis=-1)).mean():.3f}",
        "grain": f"{grain(mid_y):.4f}",
    }


def main(paths):
    rows = [measure(p) for p in paths]
    base = rows[0]["_finest"]
    for row in rows:
        row["finest"] = f"{row.pop('_finest') / base:.2f}"
    names = [p.replace("\\", "/").rsplit("/", 1)[-1] for p in paths]
    width = max(len(n) for n in names)
    for key in rows[0]:
        print(f"{key}:")
        for name, row in zip(names, rows):
            print(f"  {name:<{width}}  {row[key]}")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    main(sys.argv[1:])
