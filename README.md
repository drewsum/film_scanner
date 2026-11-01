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

> **Status:** early development. Scanner communication and the GUI shell work; Crop, Invert,
> Adjust and Export are placeholders. See the [roadmap issue](https://github.com/drewsum/film_scanner/issues/1).

## Scanner support

| Backend | Platform | Status |
|---------|----------|--------|
| WIA (Windows Image Acquisition) | Windows | Flatbed scanning. Tested with an Epson Perfection V500 |
| Mock "Virtual Film Scanner" | All | Synthetic film strips with an IR channel, for development |
| SANE | Linux/macOS | Planned |
| Native Epson ESC/I over USB | All | Planned, needed for the V500's film adapter, 48-bit and IR |

The Epson V500's WIA driver only exposes the flatbed at up to 1200 dpi in 8-bit colour, with
no transparency unit and no infrared. Full film support for that scanner needs a native backend.

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
- `crates/scan-wia`: Windows WIA backend
- `crates/app`: the `film-scanner` egui application

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
