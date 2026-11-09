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
| 30, 31 | SilverFast, one frame at 400 and 800 ppi | [Film](#film-transparency-unit) |
| 33 | the same frame at "3,200 ppi" | read at 1,600 dpi; SilverFast scales up |
| 35, 36 | 1,600 ppi, a frame on the other strip and one further along | x and y positions |
| 38 | 800 ppi with iSRD | color pass at 200 dpi hardware, then infrared at 800 |
| 39 | 400 ppi with multi-exposure | two whole sessions; the second exposes 2× |
| 40 | Epson Scan, one frame at 1,200 dpi (48-bit, no corrections) | its own 1,200 dpi mode |
| 41, 43 | the same frame at 3,200 and 2,400 dpi | the same read, 3,200 × 2,400 |
| 42 | 3,200 dpi with Digital ICE | infrared pass at 3,200 × 2,400 |
| 44 | 3,200 dpi, a frame on the other strip | x position |
| 45 | 6,400 dpi | a 6,400 dpi mode |

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
| 8 | upper bits: horizontal subsampling, `b_` → ÷8/3, `a_` → ÷4/3, `1_` → ÷2, `0_` → ÷1. Bit 0: **8-bit samples** (set in all of WIA's reads); clear, the read returns 16-bit little-endian samples | units per pixel; a native 16-bit read with `a0`/`b0` returns twice the bytes of `a1`/`b1` |
| 9, 10 | resolution: **hardware dpi = 2400 × b9 / b10** | `01 0c` 200, `01 06` 400, `01 03` 800, `02 03` 1600, `01 01` 2400 |
| 11 | `82`/`83` for image reads (SilverFast's 16-bit ones too), `40` white read, `00` dark and masked-strip reads | open: the low bit |
| 15 | varies 0x10–0x60 between otherwise equal scans | open |
| 16 | `2e` in captures 02–09, `00` from 15 on; `1e` for a 400 dpi read at the top of the bed; `80` in SilverFast's reads and calibration reads | open; not the bit depth: 16-bit reads with and without `80` are alike |
| 18 | lines per data block: as many as fit in 64 KiB (11 at 1,920 px of RGB) | full-width captures |

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

**`1e 22` (12 bytes):** u16 at 0 grows with resolution (3,840 at 200 dpi, 3,264 at 400, 5,376 at
800, 10,848 at 1,600 on the flatbed). The same value leads `1e 21` (26 bytes) and follows `04 00`
in `1e 46`. SilverFast's film scans leave it 0 and set u16 at 6 instead: the exposure (doubling it
doubles the signal), searched per session, see [Film](#film-transparency-unit).

**`1e 57` (42 bytes), the scan record:** u32 LE fields: hardware dpi (twice), the area's x, y,
width and height in pixels at that resolution, `0x0813`, `0x0300` + the resolution divisor, then
`1e 41` byte 16 at byte 33. The next session reads it back with `1e 53`. SilverFast writes its
own; it doesn't affect the scan.

**`1e 84` (table upload):** an 8-byte header, then the table, acked once. Byte 0 is the type,
bytes 1–4 an address and bytes 5–7 the table's length (u24 LE). `scan_epson::tables` generates
them all:

| Type | Table | Content |
|---|---|---|
| 3 | 8-bit tone curve, one per channel (256 B) | `round(255 · x^(1/1.8))`, exactly the driver's |
| 2 | 12-bit tone curve, one per channel (u16 × 4,096) | knots every 16 entries are the 8-bit curve widened to 12 bits (`v << 4 \| v >> 4`), linear in between, 4,095 from 4,080 on. The driver's interpolation differs by at most 2 in 4 % of the entries |
| 5 | motor acceleration (u16 step periods) | constant acceleration from the first period to the last over the table's length: 2,500 → 250 (256 entries) for a short move, 2,500 → 160 (512) for a long one, 2,500 → 320 / 1,249 → 272 (512) for the image read at 200 / 400 dpi, one entry (160 / 136) for the white read. Within one tick of the driver's |
| 6 | shading, see [Flatbed calibration](#flatbed-calibration) | |
| 7 | 256 B, constant in every session (Epson's and SilverFast's) | `00 00 00 00 28 00 c0 31 c8 00 c0 31 90 01 00 10`, then `ff` |

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
   base. The white base is the dimmest pixel's white level under the glass (the first 8.5 in
   of the read; past it the read falls off the calibration strip, and those pixels would halve
   the base).

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

`1e 43` (12 bytes) is `00 80 00 80 00 80 84 7d 77 7e df 7f` for calibration reads and
`84 7d 77 7e df 7f` twice for image reads. SilverFast sends the same values, so they are fixed
rather than per-device. `1e 87 00` returns 10 bytes that differ between sessions.

### Native scans

`epson-scan` scans any flatbed area at up to 400 dpi without Epson's software. It generates the
whole session Epson's driver runs (`scan_epson::session`):

1. Open: `1e 85`, `1e 53`, `1e e1 0b 00 25 61 00 00`, `1b 40`, `1b 13` (identity), `1e a1`,
   `1e 65`, the 8-bit tone curves.
2. `1e 57` (the scan record), lamp on (`1e a2 01`), `1e 87 00`, `1e 86`.
3. [Calibration](#flatbed-calibration): the scanner's offset calibration, `1e 11 04`, `1e 21`,
   the type-7 table, then the masked-strip, white and dark reads.
4. The 12-bit tone curves, the carriage move (motor table, `1e 01`, `1e 05`).
5. The image read: `1e 22`, `1e 41`, `1e 42`, `1e 43`, the shading table, the motor table,
   `1e 01`, `1e 47`. Then `1e a1`, `1e 65`, `1e 85`.

`1b 03` is polled until bit `0x40` (busy) clears after each read and move. Like the driver, a
session reads the full width and the area is cropped when decoding:

```sh
Stop-Service stisvc -Force     # admin; Start-Service stisvc afterwards
cargo run -p scan-epson --bin epson-scan -- --dpi 300 --area 40,60,30,20 --out scan.png
```

`--bits 16` makes a 16-bit read: `1e 41` byte 8 bit 0 cleared, and flat 8- and 12-bit tone
curves uploaded instead of the gamma ones, as SilverFast does, so the samples are linear. (The
flat 12-bit curve is the identity widened like the gamma one, `v << 4 | v >> 4` at the knots, so
256 maps to 257: exactly SilverFast's.)
`scan_epson::decode::flatbed_rgb16` applies the 1.8 tone curve on the host, so 8- and 16-bit
scans of the same area match (mean absolute difference 1.25 of 255 on hardware, two scans
apart) and the 16-bit one has levels between the 8-bit steps. `--simulate` runs the session against `scan_epson::sim::SimulatedScanner` instead. From the
`1e 57` record on, a session's commands match Epson's driver in full-width captures 24, 25, 28
and 29 (checked by `cargo test -p scan-epson -- --ignored` when the captures are present), except
for the per-session calibration values, the tables' small differences above, and the carriage
move's step count. On hardware, against WIA's scans of the same areas (mean absolute difference,
0–255): 1.85 at 75 dpi (80 × 50 mm), 2.42 at 300 dpi (30 × 20 mm, 1 px offset), 3.96 at 150 dpi
at the top of the bed (1 line offset).

In the app, the V500 appears as **EPSON Perfection V500 (epson)** (`scan_epson::EpsonBackend`):
flatbed, 8- or 16-bit color, 50–400 dpi, any area of the 215.9 × 297.18 mm bed; and film,
8- or 16-bit, 100–1,600 dpi, with an optional second pass at +1 EV. The app relaunches
itself elevated once at startup when a V500 is attached (`--no-elevate` skips this). Each scan
stops `stisvc` and starts it again afterwards, so WIA stays usable between scans. A scanner that
was just powered on gets its firmware from Epson Scan's `esfw7c.bin` first. A cancel is honoured
only before calibration: after that the session runs to the end, because no way to abort a read
is known and the carriage has to get home. The ignored test
`cargo test -p scan-epson --test hardware -- --ignored` scans the live scanner (as admin).

The older template mode (`--template CAPTURE.pcap`) replays a full-width capture of Epson's
driver with the area patched in. Templates are captures 24/28 (200 dpi hardware, top/lower move
profile) and 25/29 (400 dpi) of WIA scans of `0,0,215.9,20` and `0,30,215.9,20` mm. They come from
your own machine's captures and are not committed.

### Film (transparency unit)

SilverFast's film sessions (captures 30–39) use the same commands as a flatbed session, with
their own constants and searches. `scan_epson::film` generates them; outside the searches its
commands match SilverFast's in captures 30, 31, 33, 35 and 36 write for write, except for the
calibration values and the lines per block (checked by
`cargo test -p scan-epson -- --ignored` when the captures are present).

1. **Open** as for the flatbed, but `1e e1 0b 00 31 61 00 00`, flat 8-bit tone curves, no
   `1e 87`/`1e 46`. The `1e 57` record holds the area in pixels at hardware resolution, then
   `0x011012`, `0x0300` + `12`/`24`/`45` (400/800/1,600 dpi) and `0x8000`.
2. **Film lamp** `1e a2 02`; `1e 25 00`; a fast move of 932 steps; `1e 25 02`, `1e 11 03`,
   `1e 31` gains `1e 1e 1e`, `1e 21 80 1f…`, the type-7 table. `1e 25` picks what the motor
   drives: `00` the CCD carriage, `02` the transparency unit's lamp, `03` both. `1e 86` reads
   the two positions (u16 CCD, u16 lamp), so the window read below sweeps the lamp past the
   parked CCD.
3. **Calibration window.** A 150-line 8-bit read at 200 dpi (`1e 41 8f 0c 0f 0e 96 00 …`, x in the
   gap between the strips) crosses a bright window in the holder. The carriage then moves back
   276 steps (`1e 01` mode `ff 01 ff 01 30 00`: bit `0x20` of byte 10 reverses), 12 fewer for
   each line the window's lower edge falls after line 91 (11 and 12 have it at 91.5 and move
   264). `1e 8a` gets `0x400` + that move, then `00 77`. All calibration reads happen here.
4. A 4-line 16-bit read at gain `01 01 01`, saturated (its purpose is unknown); `1e 25 03`,
   `1e 87 00`.
5. **Exposure and gains**, from 4-line 8-bit reads of the window across the film area's width.
   Gain (`1e 31` bytes 8–10) is proportional to 1 / (72 − code): SilverFast's steps all fit
   that within 1 %. SilverFast starts at `1c 17 13` and exposure 1,500 (4,000 at 1,600 dpi),
   scales the exposure to bring the brightest channel to 243, balances the gains, and ends with
   about 0.85 of that exposure and each channel at a mean of about 206. `scan_epson::film` gets
   there directly: one read, then the exposure and gains from the model, refined once. On
   hardware it lands within 4 % of SilverFast's exposure and one gain step of its gains. A
   normal exposure is at most half of `1e 21` (`00 0f` up to 800 dpi, `f8 1c` at 1,600, sent
   before the search; at 1,600 `1e 11 02` too).
6. **Offsets** (`1e 31` bytes 0–5), against 4-line 16-bit reads of the masked strip with the lamp
   off (`1e a2 03`; `1e 22` byte 9 = 4). Each channel steps up while its dark level is below
   1,024 and down while above 1,536, from `80` by `40`, `20`, `10`, `8`, `4`, until all are in
   range. This reproduces SilverFast's sequences exactly (`80 c0 a0 90 88`, then `8c` or not).
7. **White and dark reads** at the area's x range: an 8-line masked-strip read for the levels,
   then a 32-line white read (`02 03`, `40`) through the window and a 64-line dark read, as on
   the flatbed. The shading table covers the read's x units (before subsampling).
8. **Flat 12-bit tone curves, the move and the image read.** `1e 41`
   `… 00 00 SS RR RR 83 80 00 00 20 80 a0 BB`, 16-bit; `1e 43` neutral for both halves.

| Hardware dpi | `1e 41` 9–10, byte 8 | `1e 11` | x units/in, film area origin | Read motor: ramp, `1e 01` mode | Fast-move lead |
|---|---|---|---|---|---|
| 400 | `01 06`, `10` (÷2) | `03` | 800, 2,365 | 1,249 → 320, `7e 00 ff 00 ff 00 02 00` | 750 |
| 800 | `01 03`, `00` | `03` | 800, 2,365 | 1,250 → 640, the same | 750 |
| 1,600 | `02 03`, `00` | `02` | 1,600, 4,730 | 309 flat, `7e 00 00 00 00 00 08 00` | 494 |
| 1,200 | `01 02`, `a0` (÷4/3) | `02` | 1,600, 4,730 | 234 flat, `80 00 00 00 00 00 08 00` | 496 |
| 3,200 × 2,400 | `01 01`, `00` | `01` | 1,600 (left edge), about 4,770 | 510 flat, `20 00 00 00 00 00 08 00` | 400 |
| 6,400 | `08 03`, `00` | `00` | 1,600 (left edge), about 4,819 | 1,592 flat, `1e 00 00 00 00 00 08 00` | 398 |

The last three are Epson Scan's (captures 40–45); SilverFast doesn't use them. `1e 11` sets the
CCD's horizontal sampling: 6,400 (`00`), 3,200 (`01`), 1,600 (`02`), 800 (`03`) or 533⅓ (`04`,
the flatbed) pixels per inch, and byte 8 subsamples from there. The film area is 2.7 × 9.33
in (SilverFast's preview, 1,080 × 3,732 px at 400 dpi). An area at x px (hardware resolution)
starts at origin + x px; its width is rounded up to 16 px. The move from the window is
2,400 × y in − lead steps (fast: `ff 01 ff 01 10 00`, the 2,500 → 222 table). Shorter moves run
at one speed (a one-entry table of 96, mode `00 00 00 00 18 00`) and land 1,024 steps further
on: 274 steps for y = 0 at 400 dpi (capture 11). That offset is inferred for 800 and 1,600 dpi;
on hardware, scans at y = 3 mm land where the preview has them, to within 0.25 mm. The read
needs `2400 / hw × lines` steps plus a lead-in, which is `1e 01` byte 4 (126 in SilverFast's
reads; 128, 32 and 30 in Epson Scan's at 1,200, 2,400 and 6,400 dpi). A steady read's
fast-move lead is always 368 steps more than its lead-in. SilverFast's lines per block reach
256 KiB at 1,600 dpi; 64 KiB blocks work as well.

**Above 1,600 dpi** the x units split: `1e 41`'s left edge stays at 1,600 units per inch from
the origin, while the width (end − start) counts pixels at the CCD's resolution. Epson Scan's
search reads cover the film area as 4,786–13,426 (8,640 px = 2.7 in at 3,200) and
4,820–22,100 (17,280 px at 6,400). Epson Scan's 3,200 dpi is the 3,200 × 2,400 read with the
lines scaled up (its 2,400 sends the very same commands), so a true 3,200 dpi scan is a 6,400
read halved: about 3× the time of 2,400 (81 s against 26 s for a frame). The `1e 57` record
gives x and y resolutions separately (`3200, 2400`). At 6,400 dpi the image read's `1e 41`
byte 11 is `80`, not `83`. The line time is `1e 21`, and every white read's motor period is
the line time over 24. Epson Scan reads 2 × 63 skew lines at 2,400 dpi and 2 × 211 at 6,400
where 0.025 in would give 60 and 160; still to be checked on hardware.

Captures 41 and 44 put the 3,200 × 2,400 origin 19 units apart (4,784 and 4,765), while Epson
Scan's own images of one frame at 1,200, 2,400, 3,200 and 6,400 dpi line up within 2 px at
1,200 dpi. USBPcap's snap length cuts Epson Scan's 256 KiB data reads to 64 KB, so the
captures hold the commands but not whole images.

**Multi-exposure** (capture 39) is a second whole session whose image read and its dark read use
twice the exposure; the white read keeps the normal one, so the second pass comes out a stop
brighter. `FilmSession::with_exposure` does the same (up to `1e 21`).

```sh
cargo run -p scan-epson --bin epson-scan -- --film --dpi 1600 --bits 16 --area 3,40,24,36 --out frame.png
```

On hardware, native scans of the frames of captures 31 (800 dpi) and 33 (1,600 dpi), against
SilverFast's reads of them through the same decoding: mean absolute difference 2.33 and 2.41 of
255, within a line and a pixel of each other, channel means within 3 %. A +1 EV pass is 1.99×
the normal one in linear terms.

### Infrared

iSRD (captures 13 and 38) runs a whole second session after the color one, with the infrared
LED: lamp `06` for the calibration reads, `05` off for the dark reads and `04` for the image read.

1. Opening as for color, then `1e 25 02` and a fast move of **535 steps of the lamp alone**
   (`1e 86` goes from `00 00 00 00` to `00 00 17 06`): the LED sits that far along the lamp
   unit from the visible lamp. Without the move the window reads are dark. Then `1e 25 00` and
   the CCD's 932-step move, and the window read and move back as for color (no `1e 8a`, no
   saturated read).
2. `1e 11 01` reads at 3,200 px per inch across: a gain search, then a 490-line read during which
   neither unit moves, between lamp and CCD moves that cancel out. SilverFast probably measures
   the LED's alignment from it: its infrared read starts 8 px earlier than the color one in
   capture 38, and 1 later but 16 wider in capture 13. `scan_epson::film` leaves it out and
   shifts its infrared read by a fixed 18/2,400 in (6 px at 800 dpi). That is measured on this
   V500, and puts the two planes within a pixel of each other in x and 2 px (at 1,600 dpi) in y.
3. `1e 25 03`, `1e 11 03` (`02` at 1,600 dpi). Exposure `1e 22` is zero throughout: the line
   time `1e 21` sets it (`60 18` at 800 dpi, `a8 21` at 1,600). Gains start at 0 and aim for a
   window mean of about 210 (`0f 16 14` at 800 dpi). The offsets, white and dark reads follow
   as for color, with the LED left on for the masked-strip reads.
4. Flat tone curves, the move (its lead is 494, the color lead + 256 at 800 dpi) and the
   image read, at one motor speed: `1e 01` mode `7e 00 00 00 00 00 08 00` with a one-entry
   period table (130 at 800 dpi, 359 at 1,600; a line takes period × 16 × steps per line, the
   line time).

The image read is an ordinary 3-channel 16-bit read. All three CCD rows see the infrared, with
the same line skew as in color, so `decode::infrared` aligns and averages them into one linear
plane. Dust and scratches are sharp dark specks on a bright, low-contrast image (the dyes are
nearly transparent to infrared). That gives the defect mask for dust removal.

```sh
cargo run -p scan-epson --bin epson-scan -- --film --ir --dpi 800 --area 5,40,24,36 --out ir.png
```

**Positions.** Moves are relative, and `1e 86` counts a move's steps plus twice its ramp (a
fast move of 932 steps on the 512-entry table reads 1,956). Only a session's image read takes
both units home again. So a session that stops partway leaves them out, and the next one's
window read misses the window. A film session therefore starts by moving each unit back by
what `1e 86` reports. The firmware homes on its own sensor only after its upload at power-on.

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
- `1e a2` + 1 byte selects the light source (see [Infrared](#infrared)).
- `1e 31` (12 bytes) sets per-channel analog front-end values: three u16 at 0 (offset; SilverFast
  binary-searches them, `80 → c0 → a0 → 90 → 88 → 8c`, against dark reads), and three bytes at
  8 (gain, e.g. `22 16 16`).
- `1e 41` byte 8 is `0x10` (÷2, 16-bit) in capture 11. Its preview
  is 1,088 px × 3,752 lines of 16-bit LE interleaved RGB at hardware divisor 6 (400 dpi), sent in
  261,120-byte (40-line) blocks. The x range in `1e 41` is 2× the pixel width at that
  resolution (800 units per inch, see [Film](#film-transparency-unit)).
