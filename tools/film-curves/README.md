# film-curves

Reads the curves of a film datasheet out of the vector graphics of its PDF, into the tables
of `crates/scan-film/src/sheets/`. One JSON config per datasheet edition says which paths are
which curves and how the axes are calibrated; the PDF's SHA-256 pins the edition.

```sh
pip install pymupdf
python extract_curves.py render SHEET.pdf PAGE out.png   # look at the page
python extract_curves.py list SHEET.pdf PAGE             # its paths, ticks and runs
python extract_curves.py overlay CONFIG.json SHEET.pdf out    # check: the samples drawn on the pages
python extract_curves.py rust CONFIG.json SHEET.pdf > ../../crates/scan-film/src/sheets/NAME.rs
python extract_curves.py generic generic_c41.json PDF_DIR > ../../crates/scan-film/src/sheets/generic_c41.rs
cargo fmt
```

| Config | Datasheet | Used by |
|---|---|---|
| `kodak_e7022.json` | KODAK GOLD 200, E-7022, February 2016 | Gold 200, Kodacolor 200 |
| `kodak_e7022_2007_gold_100.json` | KODAK GOLD 100 and 200, E-7022, February 2007 | Gold 100 |
| `kodak_e4046_ektar_100.json` | KODAK PROFESSIONAL EKTAR 100, E-4046, January 2025 | Ektar 100 |
| `kodak_e4050_portra_400.json` | KODAK PROFESSIONAL PORTRA 400, E-4050, January 2025 | Portra 400 |
| `kodak_e7023_ultramax_400.json` | KODAK ULTRAMAX 400, E-7023, February 2016 | UltraMax 400 |
| `kodak_h1_5203_vision3_50d.json` | KODAK VISION3 50D 5203, H-1-5203t, July 2015 | CineStill 50D, Reto Amber D100 |
| `kodak_h1_5207_vision3_250d.json` | KODAK VISION3 250D 5207, H-1-5207t, April 2009 | CineStill 400D |
| `kodak_h1_5219_vision3_500t.json` | KODAK VISION3 500T 5219, H-1-5219t, July 2015 | CineStill 800T |
| `kodak_as2652_aerocolor_iv_2460.json` | KODAK AEROCOLOR IV 2460, AS-2652, October 2009 | Flic Film Elektra 100 |
| `fujifilm_af3_0261e_fujifilm_200.json` | FUJIFILM 200, AF3-0261E, 9 February 2022 | Fujifilm 200 |
| `fujifilm_af3_0262e_fujifilm_400.json` | FUJIFILM 400, AF3-0262E, 1 April 2023 | Fujifilm 400 |
| `ilford_hp5_plus_2010.json` | ILFORD HP5 PLUS, technical information, April 2010 | HP5 Plus (black-and-white) |
| `generic_c41.json` | the mean of Gold 200, Ektar 100, Portra 400 and UltraMax 400 | Harman Phoenix 200, Phoenix II 200 |

The PDFs are the manufacturers' and are not kept here; each config has the URL to fetch.
The config format, the checks to make on a new sheet and how to add a stock are in
[docs/film-profiles.md](../../docs/film-profiles.md).
