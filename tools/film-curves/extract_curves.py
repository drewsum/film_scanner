"""Extract a film datasheet's curves from the vector graphics in its PDF, as Rust tables.

Kodak's technical datasheets draw their curves as vector paths, so the curves can be read
back exactly (to a fraction of the line width) instead of being traced off a picture.

Usage:

    python extract_curves.py list  SHEET.pdf PAGE           # every path on the page
    python extract_curves.py render SHEET.pdf PAGE OUT.png  # a picture of the page
    python extract_curves.py rust  CONFIG.json SHEET.pdf > ../../crates/scan-film/src/sheets/NAME.rs

`list` and `render` are for writing a config (see kodak_e7022.json and README.md); `rust`
writes the tables. The PDF's SHA-256 is checked against the config, so the tables can only
come from the edition the config was written for. Needs PyMuPDF (`pip install pymupdf`).
"""

import hashlib
import json
import sys

import fitz


def flatten(drawing, steps=24):
    """The points along a drawing's path: its lines' ends and points along its curves."""
    points = []
    for item in drawing["items"]:
        kind = item[0]
        if kind == "l":
            points += [item[1], item[2]]
        elif kind == "c":
            p0, c1, c2, p3 = item[1:5]
            for i in range(steps + 1):
                t = i / steps
                u = 1 - t
                points.append(
                    fitz.Point(
                        u**3 * p0.x + 3 * u * u * t * c1.x + 3 * u * t * t * c2.x + t**3 * p3.x,
                        u**3 * p0.y + 3 * u * u * t * c1.y + 3 * u * t * t * c2.y + t**3 * p3.y,
                    )
                )
        else:
            raise ValueError(f"unexpected path item {kind!r} in a curve")
    return points


class Axis:
    """Maps a page coordinate to a data value through two ticks: [[page, value], [page, value]]."""

    def __init__(self, ticks):
        (p0, v0), (p1, v1) = ticks
        self.scale = (v1 - v0) / (p1 - p0)
        self.p0, self.v0 = p0, v0

    def __call__(self, p):
        return self.v0 + (p - self.p0) * self.scale


def subpaths(drawing):
    """A drawing split where its path jumps: one drawing can hold several curves."""
    runs, current, last = [], [], None
    for item in drawing["items"]:
        start = item[1]
        if last is not None and abs(start.x - last.x) + abs(start.y - last.y) > 0.05:
            runs.append(current)
            current = []
        current.append(item)
        last = item[-1]
    runs.append(current)
    return [{"items": r} for r in runs]


def piece_points(drawings, spec):
    """The points of one piece of a curve. `spec` is a drawing's index, an inclusive range
    of consecutive drawings `[first, last]`, or `"index.run"`: one continuous run of a
    drawing that holds several (see `list --runs`)."""
    if isinstance(spec, list):
        return [p for i in range(spec[0], spec[1] + 1) for p in flatten(drawings[i])]
    if isinstance(spec, str):
        index, run = map(int, spec.split("."))
        return flatten(subpaths(drawings[index])[run])
    return flatten(drawings[spec])


def pieces(drawings, plot, name):
    """A curve as polylines in data units, one per piece, each sorted by x. A piece that is a
    list of specs is joined into one."""
    x, y = Axis(plot["x"]), Axis(plot["y"])
    runs = []
    for piece in plot["curves"][name]:
        specs = piece if isinstance(piece, list) and piece and not isinstance(piece[0], int) else [piece]
        pts = [(x(p.x), y(p.y)) for s in specs for p in piece_points(drawings, s)]
        pts.sort()
        runs.append(pts)
    return runs


def sample(runs, at, floor):
    """The curve at `at`: interpolated within a piece, None outside every piece. A piece
    ending on the plot's floor (the curve leaving the plot) counts as None past that end."""
    for pts in runs:
        if pts[0][0] - 1e-6 <= at <= pts[-1][0] + 1e-6:
            for (x0, y0), (x1, y1) in zip(pts, pts[1:]):
                if x0 - 1e-6 <= at <= x1 + 1e-6:
                    v = y0 if x1 == x0 else y0 + (y1 - y0) * (at - x0) / (x1 - x0)
                    if floor is not None and v <= floor + 0.01:
                        return None
                    return v
    return None


def grid(spec):
    start, step, end = spec["start"], spec["step"], spec["end"]
    n = int(round((end - start) / step)) + 1
    return [start + i * step for i in range(n)]


def rust(config_path, pdf_path):
    config = json.load(open(config_path))
    data = open(pdf_path, "rb").read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != config["source"]["sha256"]:
        sys.exit(f"{pdf_path} is not the edition {config_path} describes (sha256 {digest})")
    page = fitz.open(pdf_path)[config["page"]]
    drawings = page.get_drawings()
    src = config["source"]

    out = []
    w = out.append
    w(f"//! {src['film']}, KODAK Publication {src['publication']} ({src['edition']}): its curves,")
    w("//! read from the vector graphics of the datasheet's PDF.")
    w("//!")
    w(f"//! Generated by `tools/film-curves/extract_curves.py` from `{config_path.split('/')[-1]}`; do")
    w("//! not edit by hand. Source PDF:")
    w(f"//! <{src['url']}>")
    w(f"//! (SHA-256 `{digest}`).")
    w("//!")
    for line in config["notes"]:
        w(f"//! {line}" if line else "//!")
    w("")
    w("use crate::sheet::{Curve, Datasheet, Spectrum};")
    w("")

    def fmt(v):
        return "None" if v is None else f"Some({v:.4f})"

    def curve(plot_name, name, indent):
        plot = config["plots"][plot_name]
        runs = pieces(drawings, plot, name)
        xs = grid(plot["sample"])
        ys = [sample(runs, x, None) for x in xs]
        missing = [x for x, y in zip(xs, ys) if y is None]
        if missing:
            sys.exit(f"{plot_name}/{name} has no value at {missing}: shorten its sample range")
        pad = " " * indent
        lines = [f"{pad}Curve {{", f"{pad}    start: {xs[0]:.2f},", f"{pad}    step: {plot['sample']['step']},"]
        lines.append(f"{pad}    values: &[")
        for i in range(0, len(ys), 8):
            lines.append(pad + "        " + " ".join(f"{y:.4f}," for y in ys[i : i + 8]))
        lines += [f"{pad}    ],", f"{pad}}}"]
        return lines

    def spectrum(plot_name, name, indent):
        plot = config["plots"][plot_name]
        runs = pieces(drawings, plot, name)
        xs = grid(plot["sample"])
        ys = [sample(runs, x, plot.get("floor")) for x in xs]
        pad = " " * indent
        lines = [f"{pad}Spectrum {{", f"{pad}    start_nm: {xs[0]:.1f},", f"{pad}    step_nm: {plot['sample']['step']:.1f},"]
        lines.append(f"{pad}    values: &[")
        per_line = 6 if indent < 8 else 5
        for i in range(0, len(ys), per_line):
            lines.append(pad + "        " + " ".join(f"{fmt(y)}," for y in ys[i : i + per_line]))
        lines += [f"{pad}    ],", f"{pad}}}"]
        return lines

    w(f"pub const SHEET: Datasheet = Datasheet {{")
    w(f"    film: {json.dumps(src['film'])},")
    w(f"    publication: {json.dumps(src['publication'])},")
    w(f"    edition: {json.dumps(src['edition'])},")
    w(f"    url: {json.dumps(src['url'])},")
    w(f"    log_h_ref: {config['log_h_ref']},")
    w("    // Status M density against log exposure (lux-seconds) to daylight: red, green, blue.")
    w("    characteristic: [")
    for name in ["red", "green", "blue"]:
        out.extend(l + ("," if l.endswith("}") else "") for l in curve("characteristic", name, 8))
    w("    ],")
    w("    // Log sensitivity (1 / erg per cm², for a density 0.2 above D-min) of the cyan-,")
    w("    // magenta- and yellow-forming layers: red, green, blue. None below the plot (< 0).")
    w("    log_sensitivity: [")
    for name in ["cyan", "magenta", "yellow"]:
        out.extend(l + ("," if l.endswith("}") else "") for l in spectrum("sensitivity", name, 8))
    w("    ],")
    w("    // Diffuse spectral density of the processed film's D-min, and of a midscale neutral.")
    lines = spectrum("dye_density", "minimum", 4)
    lines[0] = "    dye_density_min: " + lines[0].strip()
    lines[-1] += ","
    out.extend(lines)
    lines = spectrum("dye_density", "midscale", 4)
    lines[0] = "    dye_density_midscale: " + lines[0].strip()
    lines[-1] += ","
    out.extend(lines)
    w("};")
    print("\n".join(out))


def list_paths(pdf_path, page_no):
    """Every path on the page: its bounds, the ends of its straight lines (ticks, grid
    lines), and the bounds of each continuous run when it has several."""
    page = fitz.open(pdf_path)[int(page_no)]
    for i, d in enumerate(page.get_drawings()):
        r = d["rect"]
        kinds = "".join(sorted({it[0] for it in d["items"]}))
        print(
            f"{i:4} {kinds:4} items={len(d['items']):4} width={d.get('width')} "
            f"rect=({r.x0:.3f}, {r.y0:.3f})-({r.x1:.3f}, {r.y1:.3f})"
        )
        if all(it[0] == "l" for it in d["items"]) and len(d["items"]) <= 30:
            for it in d["items"]:
                a, b = it[1:3]
                print(f"       line ({a.x:.3f}, {a.y:.3f}) - ({b.x:.3f}, {b.y:.3f})")
        runs = subpaths(d)
        if len(runs) > 1 and kinds != "re":
            for k, run in enumerate(runs):
                pts = flatten(run)
                xs, ys = [p.x for p in pts], [p.y for p in pts]
                print(
                    f"       run {i}.{k}: x {min(xs):.1f}-{max(xs):.1f}, "
                    f"y {min(ys):.1f}-{max(ys):.1f}"
                )


def render(pdf_path, page_no, out):
    fitz.open(pdf_path)[int(page_no)].get_pixmap(dpi=150).save(out)


if __name__ == "__main__":
    # The tables are Rust source: UTF-8 with Unix line ends on every platform.
    sys.stdout.reconfigure(encoding="utf-8", newline="\n")
    command, *args = sys.argv[1:]
    {"list": list_paths, "render": render, "rust": rust}[command](*args)
