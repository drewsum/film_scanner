//! Spectral and color-space arithmetic: the CIE 1931 observer, daylight, linear sRGB.

/// A 3 × 3 matrix, rows first.
pub type Mat3 = [[f64; 3]; 3];

/// Wavelengths the spectral integrals are taken over: the visible range every datasheet
/// plots (the sensitivity of film below 380 nm is cut off by any camera lens).
pub const START_NM: f64 = 380.0;
pub const STEP_NM: f64 = 5.0;
pub const SAMPLES: usize = 65;

pub fn wavelengths() -> impl Iterator<Item = f64> {
    (0..SAMPLES).map(|i| START_NM + i as f64 * STEP_NM)
}

/// The CIE 1931 2° standard observer's color-matching functions x̄, ȳ, z̄ at `nm`.
///
/// This is the multi-lobe analytic fit of Wyman, Sloan and Shirley, "Simple Analytic
/// Approximations to the CIE XYZ Color Matching Functions", Journal of Computer Graphics
/// Techniques 2(2), 2013, which follows the tabulated functions to within about 1% of their
/// peaks: far closer than the datasheet curves it is used with are known. A formula keeps
/// the CIE's tables (licensed CC BY-SA) out of this repository.
pub fn cie_1931(nm: f64) -> [f64; 3] {
    // A Gaussian with different widths on either side of its peak.
    let g = |mu: f64, below: f64, above: f64| {
        let s = if nm < mu { below } else { above };
        (-0.5 * ((nm - mu) / s).powi(2)).exp()
    };
    [
        1.056 * g(599.8, 37.9, 31.0) + 0.362 * g(442.0, 16.0, 26.7) - 0.065 * g(501.1, 20.4, 26.2),
        0.821 * g(568.8, 46.9, 40.5) + 0.286 * g(530.9, 16.3, 31.1),
        1.217 * g(437.0, 11.8, 36.0) + 0.681 * g(459.0, 26.0, 13.8),
    ]
}

/// Relative spectral power of a black body at `kelvin`, by Planck's law (any scale).
pub fn black_body(nm: f64, kelvin: f64) -> f64 {
    // c2 = hc / k in nm·K.
    const C2: f64 = 1.438_776_877e7;
    let l = nm * 1e-3;
    1.0 / (l.powi(5) * ((C2 / (nm * kelvin)).exp() - 1.0))
}

/// The light color film is balanced for and its datasheets expose with: photographic
/// daylight, about 5,500 K. Taken as a black body, which the film's three broad
/// sensitivities can hardly tell from CIE D55.
pub const DAYLIGHT_K: f64 = 5500.0;

pub fn daylight(nm: f64) -> f64 {
    black_body(nm, DAYLIGHT_K)
}

/// The XYZ of a spectrum given at [`wavelengths`] (as spectral power), unnormalised.
pub fn xyz_of(power: &[f64; SAMPLES]) -> [f64; 3] {
    let mut xyz = [0.0; 3];
    for (p, nm) in power.iter().zip(wavelengths()) {
        let cmf = cie_1931(nm);
        for c in 0..3 {
            xyz[c] += p * cmf[c] * STEP_NM;
        }
    }
    xyz
}

/// CIE XYZ to linear sRGB (IEC 61966-2-1), for a D65 white of Y = 1.
pub const XYZ_TO_SRGB: Mat3 = [
    [3.2406, -1.5372, -0.4986],
    [-0.9689, 1.8758, 0.0415],
    [0.0557, -0.2040, 1.0570],
];

/// The D65 white point, Y = 1.
pub const D65: [f64; 3] = [0.95047, 1.0, 1.08883];

/// The Bradford cone response matrix, for chromatic adaptation.
const BRADFORD: Mat3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// The Bradford adaptation from white `from` to white `to` (both XYZ).
pub fn bradford(from: [f64; 3], to: [f64; 3]) -> Mat3 {
    let a = mul_vec(&BRADFORD, from);
    let b = mul_vec(&BRADFORD, to);
    let scale = [
        [b[0] / a[0], 0.0, 0.0],
        [0.0, b[1] / a[1], 0.0],
        [0.0, 0.0, b[2] / a[2]],
    ];
    mul(&inverse(&BRADFORD), &mul(&scale, &BRADFORD))
}

/// The sRGB transfer function, from linear light (0–1) to the encoded value.
pub fn srgb_encode(linear: f64) -> f64 {
    if linear <= 0.003_130_8 {
        12.92 * linear.max(0.0)
    } else {
        1.055 * linear.min(1.0).powf(1.0 / 2.4) - 0.055
    }
}

pub fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

pub fn mul_vec(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| (0..3).map(|k| m[i][k] * v[k]).sum())
}

pub fn determinant(m: &Mat3) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// The inverse of `m`, which must not be singular.
pub fn inverse(m: &Mat3) -> Mat3 {
    let d = determinant(m);
    let c =
        |r0: usize, c0: usize, r1: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    [
        [c(1, 1, 2, 2) / d, -c(0, 1, 2, 2) / d, c(0, 1, 1, 2) / d],
        [-c(1, 0, 2, 2) / d, c(0, 0, 2, 2) / d, -c(0, 0, 1, 2) / d],
        [c(1, 0, 2, 1) / d, -c(0, 0, 2, 1) / d, c(0, 0, 1, 1) / d],
    ]
}

pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_follows_the_cie_tables() {
        // CIE 1931 2° values at a few wavelengths.
        let table = [
            (450.0, [0.3362, 0.0380, 1.7721]),
            (500.0, [0.0049, 0.3230, 0.2720]),
            (550.0, [0.4334, 0.9950, 0.0087]),
            (600.0, [1.0622, 0.6310, 0.0008]),
            (650.0, [0.2835, 0.1070, 0.0000]),
        ];
        for (nm, expected) in table {
            let got = cie_1931(nm);
            for c in 0..3 {
                assert!(
                    (got[c] - expected[c]).abs() < 0.025,
                    "{nm} nm: {got:?} vs {expected:?}"
                );
            }
        }
    }

    #[test]
    fn daylight_is_near_d55() {
        let power = std::array::from_fn(|i| daylight(START_NM + i as f64 * STEP_NM));
        let [x, y, z] = xyz_of(&power);
        let (cx, cy) = (x / (x + y + z), y / (x + y + z));
        // D55 is (0.3324, 0.3474); the black body sits a little below the daylight locus.
        assert!(
            (cx - 0.3324).abs() < 0.005 && (cy - 0.3474).abs() < 0.01,
            "{cx} {cy}"
        );
    }

    #[test]
    fn white_is_white_in_srgb() {
        let rgb = mul_vec(&XYZ_TO_SRGB, D65);
        assert!(rgb.iter().all(|v| (v - 1.0).abs() < 1e-3), "{rgb:?}");
        let adapt = bradford([0.9642, 1.0, 0.8251], D65);
        let w = mul_vec(&adapt, [0.9642, 1.0, 0.8251]);
        assert!((0..3).all(|c| (w[c] - D65[c]).abs() < 1e-9), "{w:?}");
    }

    #[test]
    fn inverse_inverts() {
        let m = [[2.0, 1.0, 0.5], [0.1, 3.0, -1.0], [0.0, 0.4, 1.5]];
        let p = mul(&m, &inverse(&m));
        for i in 0..3 {
            for j in 0..3 {
                assert!((p[i][j] - IDENTITY[i][j]).abs() < 1e-12);
            }
        }
    }
}
