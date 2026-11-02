//! An sRGB ICC profile, to embed in saved frames so viewers (and Immich) know their colors
//! are sRGB rather than having to assume it.
//!
//! It is built here, from the sRGB standard (IEC 61966-2-1), rather than copied from
//! anyone's profile: a version 2 display profile, with the sRGB primaries adapted to the
//! profile connection space's D50 white (Bradford) and the sRGB tone curve as a table.

use std::sync::OnceLock;

/// D50, the profile connection space's white, in XYZ.
const D50: [f64; 3] = [0.9642, 1.0, 0.8249];
/// The sRGB primaries' XYZ, adapted from D65 to D50 with Bradford, as the ICC's own sRGB
/// profiles give them: red, green, blue.
const PRIMARIES: [[f64; 3]; 3] = [
    [0.436_074_7, 0.222_504_5, 0.013_932_2],
    [0.385_064_9, 0.716_878_6, 0.097_104_5],
    [0.143_080_4, 0.060_616_9, 0.714_173_3],
];
/// Entries in the tone curve's table.
const CURVE_POINTS: usize = 1024;

/// The profile's bytes, built once.
pub fn srgb() -> &'static [u8] {
    static PROFILE: OnceLock<Vec<u8>> = OnceLock::new();
    PROFILE.get_or_init(build)
}

fn build() -> Vec<u8> {
    let mut desc = Vec::new();
    desc.extend_from_slice(b"desc\0\0\0\0");
    let name = b"sRGB IEC61966-2.1 (Film Scanner)\0";
    desc.extend_from_slice(&(name.len() as u32).to_be_bytes());
    desc.extend_from_slice(name);
    // No Unicode or ScriptCode description: their lengths are zero, the ScriptCode's
    // 67 bytes are there regardless.
    desc.extend_from_slice(&[0; 4 + 4 + 2 + 1 + 67]);

    let mut cprt = b"text\0\0\0\0".to_vec();
    cprt.extend_from_slice(b"No copyright, use freely\0");

    let xyz = |v: [f64; 3]| {
        let mut t = b"XYZ \0\0\0\0".to_vec();
        for c in v {
            t.extend_from_slice(&s15_fixed16(c));
        }
        t
    };

    let mut curve = b"curv\0\0\0\0".to_vec();
    curve.extend_from_slice(&(CURVE_POINTS as u32).to_be_bytes());
    for i in 0..CURVE_POINTS {
        let v = srgb_decode(i as f64 / (CURVE_POINTS - 1) as f64);
        curve.extend_from_slice(&((v * 65535.0).round() as u16).to_be_bytes());
    }

    let mut tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", desc),
        (b"cprt", cprt),
        (b"wtpt", xyz(D50)),
        (b"rXYZ", xyz(PRIMARIES[0])),
        (b"gXYZ", xyz(PRIMARIES[1])),
        (b"bXYZ", xyz(PRIMARIES[2])),
        (b"rTRC", curve),
    ];
    for t in &mut tags {
        // Every tag starts on a four-byte boundary.
        while t.1.len() % 4 != 0 {
            t.1.push(0);
        }
    }

    // The three tone curves are the same: one copy, which they all point at.
    let table_len = 4 + 12 * (tags.len() + 2);
    let mut offset = 128 + table_len;
    let mut table = Vec::new();
    let mut data = Vec::new();
    table.extend_from_slice(&((tags.len() + 2) as u32).to_be_bytes());
    let mut curve_at = (0, 0);
    for (sig, bytes) in &tags {
        table.extend_from_slice(*sig);
        table.extend_from_slice(&(offset as u32).to_be_bytes());
        table.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        if *sig == b"rTRC" {
            curve_at = (offset, bytes.len());
        }
        data.extend_from_slice(bytes);
        offset += bytes.len();
    }
    for sig in [b"gTRC", b"bTRC"] {
        table.extend_from_slice(sig);
        table.extend_from_slice(&(curve_at.0 as u32).to_be_bytes());
        table.extend_from_slice(&(curve_at.1 as u32).to_be_bytes());
    }

    let size = 128 + table.len() + data.len();
    let mut header = Vec::with_capacity(128);
    header.extend_from_slice(&(size as u32).to_be_bytes());
    header.extend_from_slice(&[0; 4]); // Preferred CMM: none.
    header.extend_from_slice(&[2, 0x10, 0, 0]); // Version 2.1.
    header.extend_from_slice(b"mntr");
    header.extend_from_slice(b"RGB ");
    header.extend_from_slice(b"XYZ ");
    for v in [2026u16, 11, 1, 0, 0, 0] {
        header.extend_from_slice(&v.to_be_bytes());
    }
    header.extend_from_slice(b"acsp");
    // Platform, flags, manufacturer, model, attributes (8), rendering intent: none or 0.
    header.extend_from_slice(&[0; 4 + 4 + 4 + 4 + 8 + 4]);
    for c in D50 {
        header.extend_from_slice(&s15_fixed16(c));
    }
    header.extend_from_slice(&[0; 4]); // Creator.
    header.resize(128, 0);

    let mut profile = header;
    profile.extend_from_slice(&table);
    profile.extend_from_slice(&data);
    debug_assert_eq!(profile.len(), size);
    profile
}

/// The ICC's signed 15.16 fixed point.
fn s15_fixed16(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

/// Linear light of an sRGB-encoded value, both 0-1.
fn srgb_decode(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(p: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(p[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn the_profile_is_well_formed() {
        let p = srgb();
        assert_eq!(u32_at(p, 0) as usize, p.len());
        assert_eq!(&p[36..40], b"acsp");
        assert_eq!(
            (&p[12..16], &p[16..20], &p[20..24]),
            (&b"mntr"[..], &b"RGB "[..], &b"XYZ "[..])
        );
        let count = u32_at(p, 128) as usize;
        assert_eq!(count, 9);
        let mut sigs = Vec::new();
        for k in 0..count {
            let at = 132 + 12 * k;
            let (offset, size) = (u32_at(p, at + 4) as usize, u32_at(p, at + 8) as usize);
            assert!(offset % 4 == 0 && offset + size <= p.len());
            sigs.push(&p[at..at + 4]);
            // Each tag's data starts with its type.
            let kind = &p[offset..offset + 4];
            assert!([&b"desc"[..], b"text", b"XYZ ", b"curv"].contains(&kind));
        }
        for sig in [
            b"desc", b"wtpt", b"rXYZ", b"gXYZ", b"bXYZ", b"rTRC", b"gTRC", b"bTRC",
        ] {
            assert!(sigs.contains(&&sig[..]), "{sig:?}");
        }
    }

    #[test]
    fn the_primaries_add_up_to_the_white() {
        for c in 0..3 {
            let sum: f64 = PRIMARIES.iter().map(|p| p[c]).sum();
            assert!((sum - D50[c]).abs() < 1e-3, "{c}: {sum}");
        }
        assert_eq!(s15_fixed16(1.0), [0, 1, 0, 0]);
        assert!((srgb_decode(0.5) - 0.214).abs() < 1e-3);
    }
}
