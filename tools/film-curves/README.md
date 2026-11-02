# film-curves

Reads the curves of a film datasheet out of the vector graphics of its PDF, into the tables
of `crates/scan-film/src/sheets/`. One JSON config per datasheet edition says which paths are
which curves and how the axes are calibrated; the PDF's SHA-256 pins the edition.

```sh
pip install pymupdf
python extract_curves.py render SHEET.pdf PAGE out.png   # look at the page
python extract_curves.py list SHEET.pdf PAGE             # its paths, ticks and runs
python extract_curves.py rust CONFIG.json SHEET.pdf > ../../crates/scan-film/src/sheets/NAME.rs
cargo fmt
```

| Config | Datasheet | Used by |
|---|---|---|
| `kodak_e7022.json` | KODAK GOLD 200, E-7022, February 2016 | Gold 200, Kodacolor 200 |
| `kodak_e7022_2007_gold_100.json` | KODAK GOLD 100 and 200, E-7022, February 2007 | Gold 100 |

The PDFs are the manufacturers' and are not kept here; each config has the URL to fetch.
The config format, the checks to make on a new sheet and how to add a stock are in
[docs/film-profiles.md](../../docs/film-profiles.md).
