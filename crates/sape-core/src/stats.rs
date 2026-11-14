//! Frame-time statistics shared by the in-game monitor and the offline analyzer.
//!
//! Definitions (fixed so that results are comparable between runs):
//! * **avg FPS** = frames / total seconds (NOT the mean of per-frame FPS).
//! * **1% low FPS** = 1000 / mean(frame time of the slowest 1% of frames).
//! * **0.1% low FPS** = same with the slowest 0.1%.
//! * **pXX** = nearest-rank percentile of frame time in milliseconds.

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FrameSummary {
    pub frames: usize,
    pub total_ms: f64,
    pub avg_ms: f64,
    pub avg_fps: f64,
    pub stddev_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub p999_ms: f64,
    pub low_1pct_fps: f64,
    pub low_01pct_fps: f64,
}

/// Summarises frame times (milliseconds). Non-finite / non-positive samples are ignored.
/// `scratch` is reused to avoid allocating in the game's render thread.
pub fn summarize(frame_ms: &[f32], scratch: &mut Vec<f32>) -> FrameSummary {
    scratch.clear();
    scratch.extend(
        frame_ms
            .iter()
            .copied()
            .filter(|v| v.is_finite() && *v > 0.0),
    );
    let n = scratch.len();
    if n == 0 {
        return FrameSummary::default();
    }
    scratch.sort_unstable_by(|a, b| a.total_cmp(b));

    let total: f64 = scratch.iter().map(|&v| v as f64).sum();
    let avg = total / n as f64;
    let var = scratch
        .iter()
        .map(|&v| (v as f64 - avg).powi(2))
        .sum::<f64>()
        / n as f64;

    FrameSummary {
        frames: n,
        total_ms: total,
        avg_ms: avg,
        avg_fps: n as f64 * 1000.0 / total,
        stddev_ms: var.sqrt(),
        min_ms: scratch[0] as f64,
        max_ms: scratch[n - 1] as f64,
        p50_ms: percentile_sorted(scratch, 50.0),
        p95_ms: percentile_sorted(scratch, 95.0),
        p99_ms: percentile_sorted(scratch, 99.0),
        p999_ms: percentile_sorted(scratch, 99.9),
        low_1pct_fps: low_fps_sorted(scratch, 0.01),
        low_01pct_fps: low_fps_sorted(scratch, 0.001),
    }
}

/// Nearest-rank percentile of an ascending-sorted, non-empty slice.
pub fn percentile_sorted(sorted: &[f32], p: f64) -> f64 {
    debug_assert!(!sorted.is_empty());
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1] as f64
}

/// FPS equivalent of the mean of the slowest `fraction` of frames (at least one frame).
pub fn low_fps_sorted(sorted: &[f32], fraction: f64) -> f64 {
    let n = sorted.len();
    let k = ((n as f64 * fraction).ceil() as usize).clamp(1, n);
    let worst = &sorted[n - k..];
    let mean = worst.iter().map(|&v| v as f64).sum::<f64>() / k as f64;
    if mean > 0.0 {
        1000.0 / mean
    } else {
        0.0
    }
}

/// Rolling median over the last `N` frames, used for spike detection.
/// Fixed-size, allocation-free after construction.
pub struct RollingMedian<const N: usize> {
    ring: [f32; N],
    scratch: [f32; N],
    len: usize,
    pos: usize,
}

impl<const N: usize> Default for RollingMedian<N> {
    fn default() -> Self {
        Self {
            ring: [0.0; N],
            scratch: [0.0; N],
            len: 0,
            pos: 0,
        }
    }
}

impl<const N: usize> RollingMedian<N> {
    pub fn push(&mut self, v: f32) {
        self.ring[self.pos] = v;
        self.pos = (self.pos + 1) % N;
        self.len = (self.len + 1).min(N);
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn median(&mut self) -> Option<f32> {
        if self.len == 0 {
            return None;
        }
        let s = &mut self.scratch[..self.len];
        s.copy_from_slice(&self.ring[..self.len]);
        let mid = self.len / 2;
        let (_, m, _) = s.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
        Some(*m)
    }
}

/// Spike classifier: a frame is a spike if it exceeds both an absolute floor and
/// `factor` times the rolling median of preceding frames. Requires a warm-up of
/// `MIN_HISTORY` frames so loading screens do not flood the log.
pub struct SpikeDetector {
    median: RollingMedian<120>,
    pub factor: f32,
    pub min_ms: f32,
}

impl SpikeDetector {
    pub const MIN_HISTORY: usize = 30;

    pub fn new(factor: f32, min_ms: f32) -> Self {
        Self {
            median: RollingMedian::default(),
            factor,
            min_ms,
        }
    }

    /// Classifies `ms` against the history *before* adding it, then records it.
    /// Returns `Some(median)` if the frame is a spike.
    pub fn observe(&mut self, ms: f32) -> Option<f32> {
        let verdict = if self.median.len() >= Self::MIN_HISTORY {
            self.median
                .median()
                .filter(|&m| ms > self.min_ms && ms > self.factor * m)
        } else {
            None
        };
        self.median.push(ms);
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_of_constant_frames() {
        let v = vec![16.0f32; 100];
        let s = summarize(&v, &mut Vec::new());
        assert_eq!(s.frames, 100);
        assert!((s.avg_fps - 62.5).abs() < 1e-9);
        assert!((s.low_1pct_fps - 62.5).abs() < 1e-9);
        assert_eq!(s.stddev_ms, 0.0);
    }

    #[test]
    fn one_percent_low_uses_worst_frames() {
        // 99 frames at 10 ms + 1 frame at 100 ms
        let mut v = vec![10.0f32; 99];
        v.push(100.0);
        let s = summarize(&v, &mut Vec::new());
        assert!((s.low_1pct_fps - 10.0).abs() < 1e-9);
        assert_eq!(s.max_ms, 100.0);
        assert_eq!(s.p50_ms, 10.0);
        assert_eq!(s.p99_ms, 10.0);
        assert_eq!(s.p999_ms, 100.0);
        // avg fps = 100 frames / 1.09 s
        assert!((s.avg_fps - 100.0 / 1.09).abs() < 1e-6);
    }

    #[test]
    fn ignores_invalid_samples() {
        let s = summarize(&[f32::NAN, -1.0, 0.0, 20.0], &mut Vec::new());
        assert_eq!(s.frames, 1);
        assert_eq!(summarize(&[], &mut Vec::new()).frames, 0);
    }

    #[test]
    fn rolling_median_wraps() {
        let mut m = RollingMedian::<3>::default();
        assert_eq!(m.median(), None);
        for v in [1.0, 2.0, 3.0, 100.0, 100.0] {
            m.push(v);
        }
        assert_eq!(m.median(), Some(100.0)); // window = [100, 100, 3]
    }

    #[test]
    fn spike_detector() {
        let mut d = SpikeDetector::new(2.5, 25.0);
        for _ in 0..SpikeDetector::MIN_HISTORY {
            assert_eq!(d.observe(16.0), None);
        }
        assert_eq!(d.observe(30.0), None, "below factor*median (40ms)");
        assert_eq!(d.observe(50.0), Some(16.0));
        let mut fast = SpikeDetector::new(2.5, 25.0);
        for _ in 0..SpikeDetector::MIN_HISTORY {
            fast.observe(4.0);
        }
        assert_eq!(fast.observe(20.0), None, "below absolute floor");
    }
}
