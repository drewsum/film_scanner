//! How long a scan has left, from how fast its progress has moved over the last while.
//!
//! A scan's progress doesn't move evenly: each pass calibrates first, standing still for a
//! while and then jumping ahead, before reading at a steady pace. So the rate is measured
//! over a recent window, and afresh after any jump.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How far back the rate is measured.
const WINDOW: Duration = Duration::from_secs(30);
/// How long the progress must have been measured before there's an estimate.
const MIN_SPAN: Duration = Duration::from_secs(5);
/// A step this large is a stage finishing (the calibration), not reading.
const JUMP: f32 = 0.02;

/// Progress updates of the running scan, as (time, fraction done).
#[derive(Debug, Default)]
pub struct Eta {
    samples: VecDeque<(Instant, f32)>,
}

impl Eta {
    /// Forget the last scan's progress.
    pub fn reset(&mut self) {
        self.samples.clear();
    }

    /// Note that the scan was `fraction` done at `now`.
    pub fn update(&mut self, now: Instant, fraction: f32) {
        if self
            .samples
            .back()
            .is_some_and(|&(_, p)| fraction - p > JUMP)
        {
            self.samples.clear();
        }
        self.samples.push_back((now, fraction));
        // Keep the latest sample from before the window, so it spans all of it.
        if let Some(start) = now.checked_sub(WINDOW) {
            while self.samples.len() > 2 && self.samples[1].0 <= start {
                self.samples.pop_front();
            }
        }
    }

    /// The time the scan has left at `now`, if it has moved long enough to tell.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        let &(start, from) = self.samples.front()?;
        let &(_, done) = self.samples.back()?;
        // Measured up to now, so the estimate grows while the progress stands still.
        let span = now.checked_duration_since(start)?;
        if span < MIN_SPAN || done <= from {
            return None;
        }
        let rate = (done - from) / span.as_secs_f32();
        Some(Duration::from_secs_f32((1.0 - done).max(0.0) / rate))
    }
}

/// `remaining` for the status bar, such as "about 12 min left".
pub fn describe(remaining: Duration) -> String {
    let minutes = remaining.as_secs().div_ceil(60);
    match minutes {
        0 | 1 => "under a minute left".into(),
        2..60 => format!("about {minutes} min left"),
        _ => format!("about {} h {:02} min left", minutes / 60, minutes % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn steady_progress_gives_the_time_left() {
        let t0 = Instant::now();
        let mut eta = Eta::default();
        // 1% every 2 s: 25% done after 50 s, 150 s to go.
        for i in 0..=25 {
            eta.update(t0 + secs(2 * i), i as f32 / 100.0);
        }
        let left = eta.remaining(t0 + secs(50)).unwrap();
        assert!(left.abs_diff(secs(150)) < secs(1), "{left:?}");
    }

    #[test]
    fn nothing_until_the_progress_has_moved_for_a_while() {
        let t0 = Instant::now();
        let mut eta = Eta::default();
        assert_eq!(eta.remaining(t0), None);
        eta.update(t0, 0.0);
        eta.update(t0 + secs(2), 0.01);
        assert_eq!(eta.remaining(t0 + secs(2)), None);
        eta.update(t0 + secs(4), 0.0);
        assert_eq!(eta.remaining(t0 + secs(6)), None, "no progress made");
    }

    #[test]
    fn calibration_jumps_start_the_measure_afresh() {
        let t0 = Instant::now();
        let mut eta = Eta::default();
        eta.update(t0, 0.0);
        eta.update(t0 + secs(1), 0.05);
        // Calibrated after a minute; then reading at 1% a minute.
        eta.update(t0 + secs(60), 0.15);
        for i in 1..=10 {
            eta.update(t0 + secs(60 + 6 * i), 0.15 + 0.001 * i as f32);
        }
        let left = eta.remaining(t0 + secs(120)).unwrap();
        // 84% to go at 1% a minute.
        assert!(left.abs_diff(secs(84 * 60)) < secs(30), "{left:?}");
    }

    #[test]
    fn follows_a_change_of_pace() {
        let t0 = Instant::now();
        let mut eta = Eta::default();
        // 1% a second for 40 s, then 1% every 2 s.
        for i in 0..=40 {
            eta.update(t0 + secs(i), i as f32 * 0.01);
        }
        for i in 1..=30 {
            eta.update(t0 + secs(40 + 2 * i), 0.4 + i as f32 * 0.01);
        }
        // Only the slower pace is in the window: 30% to go at 0.5% a second.
        let left = eta.remaining(t0 + secs(100)).unwrap();
        assert!(left.abs_diff(secs(60)) < secs(3), "{left:?}");
    }

    #[test]
    fn grows_while_the_progress_stands_still() {
        let t0 = Instant::now();
        let mut eta = Eta::default();
        for i in 0..=10 {
            eta.update(t0 + secs(i), i as f32 * 0.01);
        }
        let moving = eta.remaining(t0 + secs(10)).unwrap();
        let stalled = eta.remaining(t0 + secs(20)).unwrap();
        assert!(stalled > moving, "{moving:?} then {stalled:?}");
    }

    #[test]
    fn describes_the_time_left() {
        assert_eq!(describe(secs(20)), "under a minute left");
        assert_eq!(describe(secs(60)), "under a minute left");
        assert_eq!(describe(secs(61)), "about 2 min left");
        assert_eq!(describe(secs(59 * 60)), "about 59 min left");
        assert_eq!(describe(secs(65 * 60)), "about 1 h 05 min left");
    }
}
