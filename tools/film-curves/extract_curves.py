"""Extract a film datasheet's curves from the vector graphics in its PDF, as Rust tables.

Kodak's technical datasheets draw their curves as vector paths, so the curves can be read
back exactly (to a fraction of the line width) instead of being traced off a picture.

Usage:

    python extract_curves.py list  SHEET.pdf PAGE           # every path on the page
    python extract_curves.py render SHEET.pdf PAGE OUT.png  # a picture of the page
    python extract_curves.py rust  CONFIG.json SHEET.pdf > ../../crates/scan-film/src/sheets/NAME.rs
    python extract_curves.py overlay CONFIG.json SHEET.pdf OUT   # the samples drawn on the pages
    python extract_curves.py generic CONFIG.json PDF_DIR > ../../crates/scan-film/src/sheets/NAME.rs

`list` and `render` are for writing a config (see kodak_e7022.json and README.md); `rust`
writes the tables, and `overlay` draws what it read over the sheet's pages to check it.
`generic` averages several sheets into a typical film's, for films that publish no curves.
The PDF's SHA-256 is checked against the config, so the tables can only come from the
edition the config was written for. Needs PyMuPDF (`pip install pymupdf`).
"""

import hashlib
import json
import os
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

    def page(self, v):
        """The page coordinate of value `v`."""
        return self.p0 + (v - self.v0) / self.scale


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


def load(config_path, pdf_path):
    """The config, and the PDF it describes: refused if it is any other edition."""
    config = json.load(open(config_path, encoding="utf-8"))
    digest = hashlib.sha256(open(pdf_path, "rb").read()).hexdigest()
    if digest != config["source"]["sha256"]:
        sys.exit(f"{pdf_path} is not the edition {config_path} describes (sha256 {digest})")
    return config, fitz.open(pdf_path), digest


class Sheet:
    """A datasheet's plots, read from its PDF. A plot is on the config's `page` unless it
    gives its own."""

    def __init__(self, config, doc):
        self.config, self.doc, self.drawings = config, doc, {}

    def page_of(self, plot_name):
        return self.config["plots"][plot_name].get("page", self.config["page"])

    def plot_drawings(self, plot_name):
        page = self.page_of(plot_name)
        if page not in self.drawings:
            self.drawings[page] = self.doc[page].get_drawings()
        return self.drawings[page]

    def curve(self, plot_name, name):
        """A characteristic curve on its sample grid: every sample must have a value."""
        plot = self.config["plots"][plot_name]
        runs = pieces(self.plot_drawings(plot_name), plot, name)
        xs = grid(plot["sample"])
        ys = [sample(runs, x, None) for x in xs]
        missing = [x for x, y in zip(xs, ys) if y is None]
        if missing:
            sys.exit(f"{plot_name}/{name} has no value at {missing}: shorten its sample range")
        return xs, ys

    def spectrum(self, plot_name, name):
        """A spectral curve on its sample grid, None where the plot shows nothing."""
        plot = self.config["plots"][plot_name]
        runs = pieces(self.plot_drawings(plot_name), plot, name)
        xs = grid(plot["sample"])
        return xs, [sample(runs, x, plot.get("floor")) for x in xs]


def fmt(v):
    return "None" if v is None else f"Some({v:.4f})"


def curve_lines(xs, ys, step, indent):
    pad = " " * indent
    lines = [f"{pad}Curve {{", f"{pad}    start: {xs[0]:.2f},", f"{pad}    step: {step},"]
    lines.append(f"{pad}    values: &[")
    for i in range(0, len(ys), 8):
        lines.append(pad + "        " + " ".join(f"{y:.4f}," for y in ys[i : i + 8]))
    lines += [f"{pad}    ],", f"{pad}}}"]
    return lines


def spectrum_lines(xs, ys, step, indent):
    pad = " " * indent
    lines = [f"{pad}Spectrum {{", f"{pad}    start_nm: {xs[0]:.1f},", f"{pad}    step_nm: {step:.1f},"]
    lines.append(f"{pad}    values: &[")
    per_line = 6 if indent < 8 else 5
    for i in range(0, len(ys), per_line):
        lines.append(pad + "        " + " ".join(f"{fmt(y)}," for y in ys[i : i + per_line]))
    lines += [f"{pad}    ],", f"{pad}}}"]
    return lines


def title(src):
    return f"{src['film']}, {src.get('publisher', 'KODAK')} Publication {src['publication']} ({src['edition']})"


def header(w, src, config_path, notes, digest):
    w(f"//! {title(src)}: its curves,")
    w("//! read from the vector graphics of the datasheet's PDF.")
    w("//!")
    w(f"//! Generated by `tools/film-curves/extract_curves.py` from `{os.path.basename(config_path)}`; do")
    w("//! not edit by hand. Source PDF:")
    w(f"//! <{src['url']}>")
    w(f"//! (SHA-256 `{digest}`).")
    w("//!")
    for line in notes:
        w(f"//! {line}" if line else "//!")
    w("")


def source_fields(w, src, log_h_ref):
    w(f"    film: {json.dumps(src['film'])},")
    w(f"    publication: {json.dumps(src['publication'])},")
    w(f"    edition: {json.dumps(src['edition'])},")
    w(f"    url: {json.dumps(src['url'])},")
    w(f"    log_h_ref: {log_h_ref},")


def color_fields(w, out, curves, spectra, dye, step, exposure, density):
    """The fields of a color Datasheet: `curves` maps red/green/blue to (xs, ys), `spectra`
    cyan/magenta/yellow and `dye` minimum/midscale; `step` each plot's sample step."""
    w(f"    // Status M density against log exposure (lux-seconds) to {exposure}: red, green, blue.")
    w("    characteristic: [")
    for name in ["red", "green", "blue"]:
        lines = curve_lines(*curves[name], step["characteristic"], 8)
        out.extend(l + ("," if l.endswith("}") else "") for l in lines)
    w("    ],")
    w(f"    // Log sensitivity (1 / erg per cm², for a density {density} above D-min) of the cyan-,")
    w("    // magenta- and yellow-forming layers: red, green, blue. None below the plot (< 0).")
    w("    log_sensitivity: [")
    for name in ["cyan", "magenta", "yellow"]:
        lines = spectrum_lines(*spectra[name], step["sensitivity"], 8)
        out.extend(l + ("," if l.endswith("}") else "") for l in lines)
    w("    ],")
    w("    // Diffuse spectral density of the processed film's D-min, and of a midscale neutral.")
    for field, name in [("dye_density_min", "minimum"), ("dye_density_midscale", "midscale")]:
        lines = spectrum_lines(*dye[name], step["dye_density"], 4)
        lines[0] = f"    {field}: " + lines[0].strip()
        lines[-1] += ","
        out.extend(lines)


def steps(config):
    return {name: plot["sample"]["step"] for name, plot in config["plots"].items()}


def rust(config_path, pdf_path):
    config, doc, digest = load(config_path, pdf_path)
    sheet = Sheet(config, doc)
    src = config["source"]
    out = []
    w = out.append
    header(w, src, config_path, config["notes"], digest)
    if config.get("kind") == "mono":
        w("use crate::sheet::{Curve, MonoDatasheet};")
        w("")
        w("pub const SHEET: MonoDatasheet = MonoDatasheet {")
        source_fields(w, src, config["log_h_ref"])
        w(f"    // {config['densitometry']} density against {config['exposure_axis']}.")
        lines = curve_lines(*sheet.curve("characteristic", "neutral"), steps(config)["characteristic"], 4)
        lines[0] = "    characteristic: " + lines[0].strip()
        lines[-1] += ","
        out.extend(lines)
    else:
        w("use crate::sheet::{Curve, Datasheet, Spectrum};")
        w("")
        w("pub const SHEET: Datasheet = Datasheet {")
        source_fields(w, src, config["log_h_ref"])
        color_fields(
            w,
            out,
            {n: sheet.curve("characteristic", n) for n in ["red", "green", "blue"]},
            {n: sheet.spectrum("sensitivity", n) for n in ["cyan", "magenta", "yellow"]},
            {n: sheet.spectrum("dye_density", n) for n in ["minimum", "midscale"]},
            steps(config),
            config.get("exposure", "daylight"),
            config.get("sensitivity_density", 0.2),
        )
    w("};")
    print("\n".join(out))


def generic(config_path, pdf_dir):
    """A typical film's sheet averaged from the sheets the config lists, each read from its
    own config and its PDF in `pdf_dir`, for films that publish no curves.

    - Characteristic: every layer of every sheet as density above its D-min, against log
      exposure from its Log H ref, averaged into one curve. Each layer of the typical film
      is that curve, from the same D-min: the sheets' mean red D-min.
    - Spectral sensitivity and dye density: each curve's mean over the sheets, where every
      sheet has a value.
    """
    config = json.load(open(config_path, encoding="utf-8"))
    log_h_ref = config["log_h_ref"]
    char, sens, dye = config["characteristic"], config["sensitivity"], config["dye_density"]
    here = os.path.dirname(config_path)
    members = []
    for m in config["average_of"]:
        mconfig, doc, _ = load(os.path.join(here, m["config"]), os.path.join(pdf_dir, m["pdf"]))
        members.append((mconfig, Sheet(mconfig, doc)))

    def at(xs, ys, x):
        t = (x - xs[0]) / (xs[1] - xs[0])
        if t < -1e-6 or t > len(xs) - 1 + 1e-6:
            sys.exit(f"log H {x:.2f} is past the end of a sheet's curve: narrow the grid")
        i = min(max(int(t), 0), len(xs) - 2)
        f = t - i
        return ys[i] * (1 - f) + ys[i + 1] * f

    rel = grid(char)
    above, d_mins = [], []
    for mconfig, sheet in members:
        for name in ["red", "green", "blue"]:
            xs, ys = sheet.curve("characteristic", name)
            above.append([at(xs, ys, mconfig["log_h_ref"] + r) - ys[0] for r in rel])
        d_mins.append(sheet.curve("characteristic", "red")[1][0])
    d_min = sum(d_mins) / len(d_mins)
    typical = [d_min + sum(c[i] for c in above) / len(above) for i in range(len(rel))]
    log_h = [log_h_ref + r for r in rel]

    def mean_spectra(plot_name, names, spec):
        nms = grid(spec)
        out = {}
        for name in names:
            curves = []
            for _, sheet in members:
                xs, ys = sheet.spectrum(plot_name, name)
                curves.append({round(x, 3): y for x, y in zip(xs, ys)})
            values = []
            for nm in nms:
                v = [c.get(round(nm, 3)) for c in curves]
                values.append(None if None in v else sum(v) / len(v))
            out[name] = (nms, values)
        return out

    src = config["source"]
    out = []
    w = out.append
    w(f"//! {src['film']}: for films whose makers publish no curves. Averaged from these")
    w("//! datasheets, each read from the vector graphics of its PDF by its own config:")
    w("//!")
    for (mconfig, _), m in zip(members, config["average_of"]):
        w(f"//! - {title(mconfig['source'])}, `{m['config']}`")
    w("//!")
    w(f"//! Generated by `tools/film-curves/extract_curves.py generic` from")
    w(f"//! `{os.path.basename(config_path)}`; do not edit by hand.")
    w("//!")
    for line in config["notes"]:
        w(f"//! {line}" if line else "//!")
    w("")
    w("use crate::sheet::{Curve, Datasheet, Spectrum};")
    w("")
    w("pub const SHEET: Datasheet = Datasheet {")
    source_fields(w, src, log_h_ref)
    color_fields(
        w,
        out,
        {n: (log_h, typical) for n in ["red", "green", "blue"]},
        mean_spectra("sensitivity", ["cyan", "magenta", "yellow"], sens),
        mean_spectra("dye_density", ["minimum", "midscale"], dye),
        {"characteristic": char["step"], "sensitivity": sens["step"], "dye_density": dye["step"]},
        "daylight",
        0.2,
    )
    w("};")
    print("\n".join(out))


def overlay(config_path, pdf_path, out_prefix):
    """Each page with plots, with the samples `rust` would write drawn over it as dots, to
    check the extraction against the printed curves: OUT_PREFIX-PAGE.png."""
    config, doc, _ = load(config_path, pdf_path)
    sheet = Sheet(config, doc)
    colors = {
        "red": (1, 0, 0), "green": (0, 0.7, 0), "blue": (0, 0, 1), "neutral": (1, 0, 0),
        "cyan": (0, 0.7, 0.7), "magenta": (0.8, 0, 0.8), "yellow": (0.9, 0.6, 0),
        "minimum": (1, 0, 0), "midscale": (0, 0, 1),
    }
    pages = set()
    for plot_name, plot in config["plots"].items():
        page_no = sheet.page_of(plot_name)
        pages.add(page_no)
        x, y = Axis(plot["x"]), Axis(plot["y"])
        for name in plot["curves"]:
            read = sheet.curve if plot_name == "characteristic" else sheet.spectrum
            for vx, vy in zip(*read(plot_name, name)):
                if vy is not None:
                    c = colors.get(name, (1, 0, 0))
                    doc[page_no].draw_circle(fitz.Point(x.page(vx), y.page(vy)), 0.8, color=c, fill=c)
    for p in sorted(pages):
        doc[p].get_pixmap(dpi=200).save(f"{out_prefix}-{p}.png")


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
        if not all(it[0] in "lc" for it in d["items"]):
            continue
        runs = subpaths(d)
        if len(runs) > 1:
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
    commands = {"list": list_paths, "render": render, "rust": rust, "generic": generic, "overlay": overlay}
    commands[command](*args)
