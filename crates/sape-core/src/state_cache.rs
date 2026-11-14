//! Shadow copy of Direct3D 9 device state used to detect (and optionally skip)
//! redundant `Set*` calls.
//!
//! Correctness model: an entry is only "known" after a `Set*` call that we saw
//! succeed on the owning thread outside of state-block recording. Anything that
//! can change device state behind our back (Reset, `IDirect3DStateBlock9::Apply`,
//! calls from another thread, failed calls) must invalidate the cache. When in
//! doubt the entry is unknown and the call is passed through, so the worst case
//! of a bug in invalidation coverage is a *missed* optimization, never a skipped
//! call - except for state changes made through paths we do not intercept at
//! all, which are listed in `docs/optimizations/0001-redundant-state-filter.md`.

/// Fixed-size cache of `u32` state values with O(1) invalidate-all, using
/// per-entry generation stamps.
pub struct StateCache<const N: usize> {
    values: [u32; N],
    stamps: [u32; N],
    generation: u32,
}

impl<const N: usize> Default for StateCache<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> StateCache<N> {
    pub const CAPACITY: usize = N;

    /// Usable in `static` initialisers. Generation starts at 1 so the
    /// zero-initialised stamps are all invalid.
    pub const fn new() -> Self {
        Self {
            values: [0; N],
            stamps: [0; N],
            generation: 1,
        }
    }

    /// True if `index` is known to currently hold `value`.
    #[inline]
    pub fn is_redundant(&self, index: usize, value: u32) -> bool {
        index < N && self.stamps[index] == self.generation && self.values[index] == value
    }

    /// Records a value that the device has accepted.
    #[inline]
    pub fn record(&mut self, index: usize, value: u32) {
        if index < N {
            self.values[index] = value;
            self.stamps[index] = self.generation;
        }
    }

    #[inline]
    pub fn invalidate(&mut self, index: usize) {
        if index < N {
            self.stamps[index] = 0;
        }
    }

    pub fn invalidate_all(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            // Wrapped after 2^32 invalidations: stamps from the previous epoch
            // could alias, so clear them explicitly.
            self.stamps = [0; N];
            self.generation = 1;
        }
    }

    pub fn known_entries(&self) -> usize {
        self.stamps
            .iter()
            .filter(|&&s| s == self.generation)
            .count()
    }
}

/// Outcome of routing one `Set*` call through the filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Call the real device method; afterwards report the HRESULT via `commit`.
    PassThrough { track: bool },
    /// The call is redundant and filtering is active: return D3D_OK without calling.
    Skip,
}

/// Counters for one state category.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FilterCounters {
    pub calls: u32,
    pub redundant: u32,
    pub skipped: u32,
}

impl FilterCounters {
    pub const ZERO: Self = Self {
        calls: 0,
        redundant: 0,
        skipped: 0,
    };

    /// Returns the current values and resets them.
    pub fn take(&mut self) -> Self {
        core::mem::replace(self, Self::ZERO)
    }
}

/// Decides what to do with one call. `tracking` is false while recording a state
/// block or on a foreign thread: such calls are passed through untouched.
#[inline]
pub fn decide<const N: usize>(
    cache: &StateCache<N>,
    index: usize,
    value: u32,
    tracking: bool,
    filtering: bool,
    counters: &mut FilterCounters,
) -> Decision {
    counters.calls = counters.calls.wrapping_add(1);
    if !tracking || index >= N {
        return Decision::PassThrough { track: false };
    }
    if cache.is_redundant(index, value) {
        counters.redundant = counters.redundant.wrapping_add(1);
        if filtering {
            counters.skipped = counters.skipped.wrapping_add(1);
            return Decision::Skip;
        }
    }
    Decision::PassThrough { track: true }
}

/// Updates the cache after a passed-through call returned `hresult`.
#[inline]
pub fn commit<const N: usize>(cache: &mut StateCache<N>, index: usize, value: u32, hresult: i32) {
    if hresult >= 0 {
        cache.record(index, value);
    } else {
        cache.invalidate(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_until_recorded() {
        let mut c = StateCache::<8>::default();
        assert!(
            !c.is_redundant(3, 0),
            "zero-initialised values must not count as known"
        );
        c.record(3, 7);
        assert!(c.is_redundant(3, 7));
        assert!(!c.is_redundant(3, 8));
        assert!(!c.is_redundant(100, 7), "out of range is never redundant");
    }

    #[test]
    fn invalidate_all_is_o1_and_complete() {
        let mut c = StateCache::<8>::default();
        for i in 0..8 {
            c.record(i, i as u32);
        }
        assert_eq!(c.known_entries(), 8);
        c.invalidate_all();
        assert_eq!(c.known_entries(), 0);
        c.record(1, 1);
        assert_eq!(c.known_entries(), 1);
    }

    #[test]
    fn generation_wrap_clears_stamps() {
        let mut c = StateCache::<2>::default();
        c.record(0, 5);
        c.generation = u32::MAX;
        c.stamps[0] = u32::MAX;
        c.invalidate_all();
        assert_eq!(c.generation, 1);
        assert!(!c.is_redundant(0, 5));
    }

    #[test]
    fn decide_and_commit_flow() {
        let mut c = StateCache::<4>::default();
        let mut k = FilterCounters::default();

        // First call: unknown -> pass through, then commit success.
        assert_eq!(
            decide(&c, 1, 9, true, true, &mut k),
            Decision::PassThrough { track: true }
        );
        commit(&mut c, 1, 9, 0);
        // Same value again -> skip when filtering.
        assert_eq!(decide(&c, 1, 9, true, true, &mut k), Decision::Skip);
        // Analysis mode: counted as redundant but still passed through.
        assert_eq!(
            decide(&c, 1, 9, true, false, &mut k),
            Decision::PassThrough { track: true }
        );
        assert_eq!(
            k,
            FilterCounters {
                calls: 3,
                redundant: 2,
                skipped: 1
            }
        );
    }

    #[test]
    fn failed_call_invalidates_entry() {
        let mut c = StateCache::<4>::default();
        c.record(2, 1);
        commit(&mut c, 2, 5, -2005530516); // D3DERR_INVALIDCALL
        assert!(!c.is_redundant(2, 1));
        assert!(!c.is_redundant(2, 5));
    }

    #[test]
    fn untracked_calls_never_skip_or_touch_cache() {
        let mut c = StateCache::<4>::default();
        c.record(0, 1);
        let mut k = FilterCounters::default();
        // e.g. during BeginStateBlock/EndStateBlock recording
        assert_eq!(
            decide(&c, 0, 1, false, true, &mut k),
            Decision::PassThrough { track: false }
        );
        assert_eq!(k.redundant, 0);
    }
}
