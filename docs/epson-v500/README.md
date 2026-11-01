# Epson Perfection V500: native protocol notes

Working notes from reverse-engineering the V500 (`04b8:0130`, sold in Japan as the GT-X770).
Tracking issue: [#3](https://github.com/drewsum/film_scanner/issues/3).

## Why not ESC/I

Epson's SANE backend (`epkowa`) lists the V500 as needing a non-free "interpreter" plugin.
The plugin translates ESC/I into the scanner's own protocol and uploads firmware after
power-on. Bytes written to the bulk pipe show the scanner rejects most ESC/I commands:

| Sent | Reply | ESC/I meaning |
|------|-------|---------------|
| `1b 40` | `06` or `00` | `ESC @` initialise |
| `1b 49` | `15` (NAK) | `ESC I` identify |
| `1b 53` | `15` | `ESC S` |
| `1b 66` | `15` | `ESC f` extended status |
| `1b 46` | `15` | `ESC F` status |
| `1c 49` | `00` | `FS I` |
| `1b 03` | `81 00` / `18 81` | none |

Replies change from run to run, so a read may pick up bytes left over from earlier
commands. Don't treat these as semantics until captures confirm them.

## Epson's Windows driver (installed with Epson Scan)

| File | Role |
|------|------|
| `C:\Windows\System32\esfw7c.bin` (65 793 bytes) | Firmware uploaded to the scanner |
| `C:\Windows\SysWOW64\esint7c.dll` | 32-bit interpreter (ESC/I to native) |
| `C:\Windows\System32\esxwia7c.dll` | WIA minidriver |
| `C:\Windows\twain_32\escndv\perv500.ds` | Epson Scan TWAIN data source (film, 48-bit, ICE) |

These are proprietary and must never be committed. A backend can load the firmware from
the user's installed driver, the same way SANE does.

Windows binds the scanner to `usbscan.sys`, which exposes the device as `\\.\Usbscan0`.
`ReadFile`/`WriteFile` on that file use the bulk IN/OUT pipes, so we can talk to the scanner
while Epson's driver stays installed. `IOCTL_SET_TIMEOUT` is `0x8000202C`. A read that times
out fails with `0x800703E3` (operation aborted), not a timeout error.

**The WIA service must be stopped for native access.** While `stisvc` runs, Epson's WIA driver
polls the scanner with `1e 85` (apparently the button state) about once a second, forever. Those
polls interleave with our commands and can swallow our replies, which leaves the scanner
mid-transfer. Stop it from an admin shell (`Stop-Service stisvc -Force`). It is trigger-started
when a scanner arrives, so to power-cycle without it, also set it to `Disabled`, and set it
back to `Automatic` afterwards.

## Tools

```sh
# List usbscan devices, then send raw transfers and print the replies
cargo run -p scan-epson --bin epson-probe -- --list
cargo run -p scan-epson --bin epson-probe -- 1b40

# Capture with USBPcap (admin shell). Find the root hub the scanner is on with
# `USBPcapCMD.exe` (interactive), then:
"C:\Program Files\USBPcap\USBPcapCMD.exe" -d \\.\USBPcap1 -o capture.pcap -A

# Print the conversation and dump the raw image stream
cargo run -p scan-epson --bin epson-trace -- capture.pcap --dump-data scan.bin
```

Captures hold only scanner traffic, but don't commit large ones. Keep the decoded logs.

## Firmware upload

At power-on the scanner has no firmware: `1b 03` answers `28 00` (bit `0x20` of byte 0 set).
Once firmware runs it answers e.g. `18 81` and identifies as `EPSON GT-X770 1.08` (`1b 13`).
The firmware stays loaded until power-off: restarting the driver or reopening the device does
not clear it. Epson's driver uploads `esfw7c.bin` as soon as the scanner enumerates:

| Host sends | Scanner | Notes |
|---|---|---|
| `1b 06` | `06` | start upload |
| `01 00 01 00` | | u32 LE length = image + 1 checksum byte (file offset `0x4c`) |
| image | | 65,536 bytes from file offset `0x100` (offset at `0x40`), sent as 65,520 + 16 |
| `cc` | `06` | low byte of the image's byte sum |
| `1b 16` | `06` | |
| `80` | `06` | boot; the scanner is silent for ~5 s, then status clears `0x20` |

The file is a 256-byte header (`EDLA` magic, vendor id `04b8` at `0x4a`) plus the image and one
trailing byte that is not sent. `epson-firmware` does the upload from the user's own Epson Scan
install:

```sh
cargo run -p scan-epson --bin epson-firmware            # skips if firmware is running
```

Note: USBPcap's 65,535-byte snap length truncates the 65,520-byte write in a capture. The
firmware file fills the gap.

## Captures

Captures (in `target/captures`, not committed) are WIA scans made with
`cargo test -p scan-wia --test hardware configured_scan -- --ignored`, 8-bit flatbed:

| # | Settings | Notes |
|---|---|---|
| 01 | WIA preview, 75 dpi | |
| 02–09 | 75/150/300/600 dpi, 50×50 / 100×50 / 50×100 / offset 20,30 mm, grey | one setting changed at a time |
| 10 | power-on, then a 75 dpi scan | firmware upload |
| 15–23 | 50/100/200/240/400/800/1,200 dpi and y offsets 30 and 150 mm | WIA offers only flatbed, 8-bit, ≤ 1,200 dpi |
| 11 | SilverFast preview, film holder with two 35 mm negative strips | its own calibration |
| 12 | SilverFast zoom preview of one frame at 1,600 ppi (48-bit, no multi-exposure) | 1,600 × 2,482 px; `1e 41` bytes 9–10 = `02 03` |
| 13a | as 12, a second run | |
| 13 | as 12 with iSRD (infrared dust removal) | separate IR pass, below |

### Scan parameters

From the final read of each capture (WIA flatbed 50–1,200 dpi in captures 02–09 and 15–23;
SilverFast film in 11–14). Sequence for one read:

```text
1e 22 <12>   1e 41 <22>   1e 42 <18>   1e 43 <12>   [1e 84 tables]   1e 01 <12>   1e 47 → data
```

**`1e 41`, scan geometry and mode (22 bytes)**

| Bytes | Meaning | Evidence |
|---|---|---|
| 0–1, 2–3 | x start, x end (u16 LE) | units below |
| 4–5 | line count (u16 LE) | equals the lines received |
| 8 | horizontal subsampling / mode: `b1` → ÷8/3, `a1` → ÷4/3, `10` → ÷2, `00`/`01` → ÷1 | units per pixel |
| 9, 10 | resolution: **hardware dpi = 2400 × b9 / b10** | `01 0c` 200, `01 06` 400, `01 03` 800, `02 03` 1600, `01 01` 2400 |
| 11 | `82`/`83` for image reads, `40` shading, `00` dark | open: the low bit |
| 15 | varies 0x10–0x60 between otherwise equal scans | open |
| 16 | `2e` in captures 02–09, `00` from 15 on | open; maybe lamp/state |
| 18 | varies with resolution and area (`1c`, `12`, `38`, `44`, …) | open; maybe lines per block |

x units are pixels at max(hardware dpi, 533⅓) per inch; for the flatbed, x = 0 mm is about 143
units at 533⅓/in (0.27 in from the edge). The driver asks for more width than requested and
crops. It downsamples to the requested dpi from the next hardware resolution up.

**`1e 01`, motor move (12 bytes):** u32 LE steps at 0, at 2,400 steps per inch. Sent with `1e 05`
it moves the carriage to the area before the read. Just before `1e 47` it is the scan length:
`2400 / hardware dpi × lines` plus 120 (200 dpi) or 126 (400 dpi) steps.

The move has two motor profiles, each with its own acceleration table (uploaded with
`1e 84 05 00 00 01 02 00 {02|04} 00` just before) and `1e 01` bytes 6–9 (`ff 00 ff 00` or
`ff 01 ff 01`). They end with different internal lead-ins, so the step count depends on the profile:

| Hardware dpi | Short profile (top of the bed) | Long profile |
|---|---|---|
| 200 | 194 + 2400 × y_in | 2400 × y_in − 315 |
| 400 | 444 + 2400 × y_in | 2400 × y_in − 69 |

Mixing them (a long move with the short profile's table) lands the read 0.2 in early.

**Lines:** a read of h px at the requested dpi needs `⌊h × hw/dpi⌋ + 2 × skew` lines, where
skew = 0.025 in × hardware dpi. Output row j takes blue from raw line j, green from j + skew and
red from j + 2 × skew.

**`1e 22` (12 bytes):** u16 at 0 grows with resolution (3,840 at 200/400 dpi, 5,376 at 800,
10,848 at 1,600 on the flatbed). SilverFast's film scans leave it 0 and set u16 at 6 instead.
Probably exposure time per lamp; open.

**`1e 84` (table upload):** an 8-byte header, then the table. Byte 0 is the type and bytes 5–7
the table's length (u24 LE). Type 2: three 8,192-byte gamma curves (u16 × 4,096); type 3: three
256-byte curves; type 5: motor acceleration; type 6: shading; type 7: 256 bytes, constant.

### Flatbed calibration

Diffing two WIA sessions that differ only in y position (24 and 28) shows what changes from run to
run: the carriage move, the `1e 57` "last scan" record (read back by the next session's `1e 53`),
and the calibration below. Everything else is constant for a given geometry.

1. **Analog offsets on the scanner.** `1e 46` (`04 00 00 0f 00…`) starts the scanner's own
   calibration; `1e 04` polls it (`02` busy, `01` done) and `1e 92` returns the result, 12 bytes
   (e.g. `86 00 86 00 8c 00 00 00 34 30 31 00`). The driver runs it twice and sends the second
   result unchanged as the `1e 31` parameters.
2. **Masked-strip read** (x 21–37, 4 lines, 16-bit): the CCD's black level. Its per-channel means
   go into the next `1e 42`.
3. **White read** of the calibration strip at the home position: 32 lines, 16-bit, at the image
   read's x range, `1e a2 01` (lamp on). The `1e 42` before it is
   `00 00 00 00 00 00 Dr Dr Dg Dg Db Db` (u16 LE) with D from step 2; the scanner subtracts
   those.
4. **Dark read**: 16 lines with the lamp off (`1e a2 00`, `1e 22` byte 9 = 1, `1e 42` zero).
5. **Image read.** `1e 42` holds a white base per channel (u16 × 3), then each channel's dark base
   twice (u16 × 6). The shading table (`1e 84` type 6) has one u16 per pixel and channel,
   pixel-interleaved. Its upper 10 bits are the pixel's white level above the white base in
   64-count steps. Its lower 6 bits are the pixel's dark level above the dark base. The driver
   smooths the dark profile (a ~65 px box filter matches) and takes its minimum as the dark
   base. The white base is the dimmest lit pixel's white level.

`scan_epson::calibration` computes all of this from the scanner's own reads. In capture 24, the
table's upper field correlates 0.9999 with the white read and the lower field is within ~2
counts of the smoothed dark read. The driver's white levels carry some noise this read doesn't
explain (±2 steps), so the table isn't byte-identical. On hardware, against WIA's scan of the same
area (mean absolute difference, 0–255):

| Calibration (`--calibration`) | 75 dpi, 80 × 50 mm | 300 dpi, 30 × 20 mm |
|---|---|---|
| `live`: computed from the session's reads | 1.81 | 3.52 |
| `recorded`: the template's values | 1.86 | 3.57 |
| `flat`: shading table flattened | 2.82, 2–14 % brighter, varying across the area | |

`1e 43` (12 bytes) stays `84 7d 77 7e df 7f` for image reads; it is probably the CCD's
per-channel gain from the scanner's EEPROM (`1e 87` returns 10 bytes of what looks like device
data).

### Native scans from templates

`epson-scan` scans any flatbed area at up to 400 dpi without Epson's software running. It uses
full-width captures of Epson's driver as templates for the lamp, gamma and motor commands. It
redoes the [calibration](#flatbed-calibration) from the scanner's own reads and patches the
carriage move, line count and scan length. It reads the full width and crops when decoding:

```sh
Stop-Service stisvc -Force     # admin; Start-Service stisvc afterwards
cargo run -p scan-epson --bin epson-scan -- \
  --template target/captures/24_75dpi_fullwidth.pcap \
  --template target/captures/28_75dpi_fullwidth_y30.pcap \
  --dpi 75 --area 40,60,80,50 --out scan.png
```

Templates are captures 24/28 (200 dpi hardware, top/lower move profile) and 25/29 (400 dpi) of
WIA scans of `0,0,215.9,20` and `0,30,215.9,20` mm. Checked against WIA's own scans of the same
areas: no offset at 75 dpi, at most 1 px at 300 dpi, mean difference 2–4 levels out of 255.
Templates come from your own machine's captures and are not committed.

### Infrared (capture 13)

iSRD adds two reads after the colour pass, with the same `1e 41` geometry except for a slightly
wider x range:

1. `1e a2 06`, `1e 11 01`: a 490-line strip at another resolution (`01 01`), probably
   calibration for the infrared lamp.
2. `1e a2 04`, `1e 11 02`: the infrared pass. It is an ordinary 3-channel 16-bit read (1,520 ×
   2,346 px here). All three CCD rows see the infrared, with the same line skew as in colour,
   so align and average them for one IR plane.

Dust and scratches are sharp dark specks on a bright, low-contrast image (the dyes are nearly
transparent to infrared). That gives the defect mask for dust removal.

So `1e a2` selects the light. Epson's flatbed sessions use `01` (flatbed lamp) and `00` (off, for
the dark read). SilverFast's film sessions use `02` (film unit, visible) and `03` (off, dark
reads), and for infrared `06` (calibration), `05` and `04` (the infrared pass).

SilverFast drives the scanner itself, with the same command set as Epson's driver. Its captures
are an independent reference: commands both programs send are probably required.

Findings so far:

- Commands are `1b xx` or `1e xx`. The scanner acks with `06` (NAK `15`), then the host sends the
  parameter block, which is acked again.
- The scan area is set by `1e 41` (22 bytes: x start/end and line count as u16 LE, resolution
  divisor of 2400 dpi at byte 10) and `1e 01` (motor steps, u32 LE at 0).
- `1e 47` starts a data read. Data arrives in blocks, each followed by an 8-byte trailer
  `02 <u32 LE block length> 00 80 00`. When the block isn't a multiple of 512 bytes, the
  trailer shares the last USB transfer with the block's final bytes (e.g. 128 + 8). Don't drop
  short transfers: `scan_epson::data::BlockAssembler` splits blocks at their trailers.
- R/G/B line skew: at 1,600 dpi, red leads green by 40 lines and blue trails by 40, i.e. 0.025 in
  each way.
- Final image data is pixel-interleaved 8-bit RGB, wider than requested and with R/G/B line
  skew. The driver crops, resamples and realigns it.
- Calibration (dark/white reads, shading uploads with `1e 84`) is done by the host, except for
  the analog offsets (see [Flatbed calibration](#flatbed-calibration)).
- `1e a2` + 1 byte selects the light source (see [Infrared](#infrared-capture-13)).
- `1e 31` (12 bytes) sets per-channel analog front-end values: three u16 at 0 (offset; SilverFast
  binary-searches them, `80 → c0 → a0 → 90 → 88 → 8c`, against dark reads), and three bytes at
  8 (gain, e.g. `22 16 16`).
- `1e 41` byte 8 is `0x10` for 16-bit-per-channel data (`0x00` for 8-bit). Capture 11's preview
  is 1,088 px × 3,720 lines of 16-bit LE interleaved RGB at hardware divisor 6 (400 dpi), sent in
  261,120-byte (40-line) blocks. The x range in `1e 41` is 2× the pixel width at that
  resolution.
