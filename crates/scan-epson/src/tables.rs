//! Tables uploaded with `1e 84`, generated the way Epson's driver builds them (see
//! `docs/epson-v500`). Each upload is an 8-byte header (type, an address, the length as u24 LE
//! at 5) followed by the table.

/// Gamma of the driver's tone curves.
const GAMMA: f64 = 1.8;

/// The 8-bit tone curve (type 3, one per channel): 255 · x^(1/1.8), rounded. Matches the
/// driver's exactly.
pub fn gamma8() -> [u8; 256] {
    std::array::from_fn(|i| (255.0 * (i as f64 / 255.0).powf(1.0 / GAMMA)).round() as u8)
}

/// The 12-bit tone curve (type 2, 4,096 u16 LE per channel): the 8-bit curve widened to 12 bits
/// at every 16th entry, linear in between, and flat at the top. The driver's curve has the same
/// knots; between them it differs from this by at most 2 of 4,095 in 4 % of the entries.
pub fn gamma12() -> Vec<u8> {
    let knots = gamma8().map(|v| u32::from(v) << 4 | u32::from(v) >> 4);
    (0..4096u32)
        .flat_map(|k| {
            let j = (k / 16) as usize;
            let (a, b) = (knots[j], knots[(j + 1).min(255)]);
            let v = if j == 255 {
                4095
            } else {
                a + (b - a) * (k % 16) / 16
            };
            (v as u16).to_le_bytes()
        })
        .collect()
}

/// The tone curve as a function on 0–1, for applying it on the host to 16-bit reads.
pub fn tone(x: f64) -> f64 {
    x.clamp(0.0, 1.0).powf(1.0 / GAMMA)
}

/// Flat tone curves (8- and 12-bit), as SilverFast uploads them for 16-bit reads: the
/// scanner then returns linear data.
pub fn linear8() -> [u8; 256] {
    std::array::from_fn(|i| i as u8)
}

pub fn linear12() -> Vec<u8> {
    (0..4096u16).flat_map(u16::to_le_bytes).collect()
}

/// A motor acceleration table (type 5): `len` step periods (u16 LE, in the scanner's ticks)
/// under constant acceleration, from `start` for the first step to `end` from the last on.
/// Matches the driver's tables to within one tick.
pub fn motor_ramp(start: u16, end: u16, len: usize) -> Vec<u8> {
    let (start, end) = (f64::from(start), f64::from(end));
    let steps = len.saturating_sub(1).max(1) as f64;
    let speed_end = 1.0 / end;
    // Constant acceleration a from speed s0 reaches `speed_end` after `steps` steps:
    // s(x)² = s0² + 2·a·x. Pick s0 so the first step takes `start` (plus half a tick, the
    // driver's tables fall within a tick of that).
    let accel = |s0: f64| (speed_end * speed_end - s0 * s0) / (2.0 * steps);
    let time_to = |s0: f64, x: f64| {
        let a = accel(s0);
        (-s0 + (s0 * s0 + 2.0 * a * x).sqrt()) / a
    };
    let first = start + 0.5;
    let (mut lo, mut hi) = (0.1 / start, 1.0 / start);
    for _ in 0..100 {
        let mid = (lo + hi) / 2.0;
        // A faster start makes the first step shorter.
        if time_to(mid, 1.0) > first {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let s0 = (lo + hi) / 2.0;
    (0..len)
        .flat_map(|n| {
            let x = n as f64;
            let period = if start <= end || x >= steps {
                end
            } else {
                (time_to(s0, x + 1.0) - time_to(s0, x)).floor().max(end)
            };
            (period as u16).to_le_bytes()
        })
        .collect()
}

/// The 8-byte `1e 84` header for a table of `kind` at `address`, `len` bytes long.
pub fn header(kind: u8, address: [u8; 4], len: usize) -> [u8; 8] {
    let n = (len as u32).to_le_bytes();
    [
        kind, address[0], address[1], address[2], address[3], n[0], n[1], n[2],
    ]
}

pub const GAMMA8: u8 = 3;
pub const GAMMA12: u8 = 2;
pub const MOTOR: u8 = 5;
pub const SHADING: u8 = 6;
pub const TYPE7: u8 = 7;

/// Addresses the driver uploads each table to: per channel for the tone curves.
pub const GAMMA8_ADDRESS: [[u8; 4]; 3] = [
    [0x00, 0xfc, 0x1f, 0x02],
    [0x00, 0xfd, 0x1f, 0x02],
    [0x00, 0xfe, 0x1f, 0x02],
];
pub const GAMMA12_ADDRESS: [[u8; 4]; 3] = [
    [0x00, 0x00, 0x38, 0x00],
    [0x00, 0x20, 0x38, 0x00],
    [0x00, 0x40, 0x38, 0x00],
];
pub const MOTOR_ADDRESS: [u8; 4] = [0x00, 0x00, 0x01, 0x02];
pub const SHADING_ADDRESS: [u8; 4] = [0x00, 0x08, 0x01, 0x02];
pub const TYPE7_ADDRESS: [u8; 4] = [0x00, 0x00, 0x00, 0x00];

/// The type-7 table, constant in every session captured (Epson's and SilverFast's): 16 bytes,
/// then `ff` to 256. Its meaning is unknown.
pub fn type7() -> [u8; 256] {
    const HEAD: [u8; 16] = [
        0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0xc0, 0x31, 0xc8, 0x00, 0xc0, 0x31, 0x90, 0x01, 0x00,
        0x10,
    ];
    std::array::from_fn(|i| HEAD.get(i).copied().unwrap_or(0xff))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16s(b: &[u8]) -> Vec<u16> {
        b.as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect()
    }

    #[test]
    fn tone_curves() {
        let g = gamma8();
        // The driver's first and last entries.
        assert_eq!(g[..12], [0, 12, 17, 22, 25, 29, 32, 35, 37, 40, 42, 44]);
        assert_eq!(g[250..], [252, 253, 253, 254, 254, 255]);
        let g = u16s(&gamma12());
        assert_eq!(g.len(), 4096);
        assert_eq!(g[..4], [0, 12, 24, 36]);
        // Knots: the 8-bit values widened (12 → 192, 17 → 273, 53 → 851).
        assert_eq!((g[16], g[32], g[240]), (192, 273, 851));
        assert_eq!(g[4080..], [4095; 16]);
        assert!(g.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(u16s(&linear12())[4095], 4095);
        assert_eq!((tone(0.0), tone(1.0)), (0.0, 1.0));
        assert_eq!((255.0 * tone(1.0 / 255.0)).round(), 12.0);
    }

    #[test]
    fn motor_ramps_match_driver() {
        // First and last entries of tables from captures 24, 25 and 28.
        for (start, end, len, head) in [
            (2500, 250, 256, [2500, 2119, 1872, 1696]),
            (2500, 320, 512, [2500, 2364, 2249, 2149]),
            (1249, 272, 512, [1249, 1226, 1203, 1182]),
            (2500, 160, 512, [2500, 2054, 1785, 1601]),
        ] {
            let t = u16s(&motor_ramp(start, end, len));
            assert_eq!(t.len(), len);
            for (got, want) in t.iter().zip(head) {
                assert!(got.abs_diff(want) <= 1, "{start}->{end}: {got} vs {want}");
            }
            assert_eq!(*t.last().unwrap(), end);
            assert!(t.windows(2).all(|w| w[0] >= w[1]));
        }
        assert_eq!(u16s(&motor_ramp(160, 160, 1)), [160]);
    }

    #[test]
    fn headers() {
        assert_eq!(
            header(GAMMA12, GAMMA12_ADDRESS[1], 8192),
            [0x02, 0x00, 0x20, 0x38, 0x00, 0x00, 0x20, 0x00]
        );
        assert_eq!(
            header(MOTOR, MOTOR_ADDRESS, 2),
            [0x05, 0x00, 0x00, 0x01, 0x02, 0x02, 0x00, 0x00]
        );
        assert_eq!(type7()[4..8], [0x28, 0x00, 0xc0, 0x31]);
    }
}
