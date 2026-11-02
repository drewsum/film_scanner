//! Line fits that ignore outliers.

/// Rounds of fitting: the first uses every point, later ones only those near the last fit.
const ROUNDS: usize = 3;
/// Fewer points than this aren't fitted.
const MIN_POINTS: f64 = 100.0;

/// A fitted line `y = offset + slope · x`, and how far from it the points it kept lie.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Line {
    pub slope: f64,
    pub offset: f64,
    /// Three standard deviations of the kept points' residuals, but at least the fit's
    /// `min_spread`.
    pub spread: f64,
}

impl Line {
    pub fn at(&self, x: f64) -> f64 {
        self.offset + self.slope * x
    }

    /// Whether the point is within [`Line::spread`] of the line.
    pub fn keeps(&self, (x, y): (f64, f64)) -> bool {
        (y - self.at(x)).abs() <= self.spread
    }
}

/// A least-squares line through `points`, refitted to the points within three standard
/// deviations (but at least `min_spread`) of the last fit. `None` with too few points, or if
/// their `x` hardly varies.
pub(crate) fn trimmed_line(points: &[(f64, f64)], min_spread: f64) -> Option<Line> {
    trimmed_line_of(points, min_spread, MIN_POINTS)
}

/// [`trimmed_line`] of as few as `min_points` points.
pub(crate) fn trimmed_line_of(
    points: &[(f64, f64)],
    min_spread: f64,
    min_points: f64,
) -> Option<Line> {
    let mut line: Option<Line> = None;
    for _ in 0..ROUNDS {
        let near = |&&p: &&(f64, f64)| line.is_none_or(|l| l.keeps(p));
        let (mut n, mut sx, mut sy, mut sxx, mut sxy) = (0f64, 0f64, 0f64, 0f64, 0f64);
        for &(x, y) in points.iter().filter(near) {
            n += 1.0;
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
        }
        if n < min_points {
            return None;
        }
        let (mx, my) = (sx / n, sy / n);
        let var = sxx / n - mx * mx;
        if var < 1e-6 * (1.0 + mx * mx) {
            return None;
        }
        let slope = (sxy / n - mx * my) / var;
        let offset = my - slope * mx;
        let residual: f64 = points
            .iter()
            .filter(near)
            .map(|&(x, y)| (y - (offset + slope * x)).powi(2))
            .sum();
        line = Some(Line {
            slope,
            offset,
            spread: (3.0 * (residual / n).sqrt()).max(min_spread),
        });
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outliers_are_left_out() {
        // y = 0.1 + 2x with a little noise, and a tenth of the points far off.
        let points: Vec<_> = (0..1000)
            .map(|i| {
                let x = i as f64 / 1000.0;
                let noise = ((i * 7919) % 13) as f64 / 13.0 - 0.5;
                let y = 0.1 + 2.0 * x + 0.01 * noise;
                (x, if i % 10 == 0 { y + 1.0 } else { y })
            })
            .collect();
        let line = trimmed_line(&points, 0.001).unwrap();
        assert!((line.slope - 2.0).abs() < 0.005, "{line:?}");
        assert!((line.offset - 0.1).abs() < 0.005, "{line:?}");
        assert!(!line.keeps(points[0]) && line.keeps(points[1]));
    }

    #[test]
    fn too_few_or_flat_points_give_no_line() {
        assert_eq!(trimmed_line(&[(0.0, 0.0), (1.0, 1.0)], 0.01), None);
        assert_eq!(trimmed_line(&[(0.5, 0.0); 500], 0.01), None);
    }
}
