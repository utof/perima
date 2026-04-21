//! Hybrid Logical Clock (HLC) for CRDT ordering.
//!
//! Per architecture audit §4.8, perima's schema reserves an `hlc`
//! column on every CRDT-eligible mutable row. This module provides
//! the generation + packing primitive; populating the column is
//! writer-side work (Batch B+).
//!
//! # Layout
//!
//! A packed HLC is a non-negative `i64`:
//!
//! - bits 0..=47  — millisecond timestamp (48-bit mask; 47 bits of
//!   positive ms = ~4460 years from Unix epoch)
//! - bits 48..=62 — same-ms monotonic counter (15 bits, max 32767)
//! - bit  63      — always 0 (i64 sign bit; enforces non-negative)
//!
//! This layout encodes `Ord` over `(ms, counter)` directly as `Ord`
//! over the packed `i64`. The counter is stored as `u16` but capped
//! at `HLC_MAX_COUNTER = 2^15 - 1` so shifting it into bits 48-62
//! never touches bit 63.
//!
//! # Monotonicity
//!
//! `Hlc::now()` is monotonic even under wall-clock step-back (NTP
//! adjustment, manual clock change). If `SystemTime::now()` returns
//! a ms `<= last`, the helper reuses `last` and bumps the counter.
//! If the counter saturates within one ms, the returned HLC advances
//! `ms` by 1 to preserve the total order.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Hybrid Logical Clock value. Monotonically non-decreasing per
/// process via [`Hlc::now`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hlc {
    /// Milliseconds since the Unix epoch (48 low bits of packed form;
    /// capped at `HLC_MAX_MS`).
    pub ms: u64,
    /// Same-ms tiebreak counter (bits 48-62 of packed form; capped at
    /// `HLC_MAX_COUNTER`).
    pub counter: u16,
}

/// Maximum `ms` representable in the packed form (2^47 - 1 ≈ 4460
/// years from Unix epoch).
pub const HLC_MAX_MS: u64 = (1u64 << 47) - 1;

/// Maximum `counter` representable in the packed form (2^15 - 1).
/// Capped at 15 bits so that packing never sets the i64 sign bit.
pub const HLC_MAX_COUNTER: u16 = (1u16 << 15) - 1;

/// Shared mutable state for [`Hlc::now`]. Per-process; across
/// processes the counter resets but `ms` typically advances between
/// restarts. For the single-writer model this is acceptable; v2+
/// multi-device sync can persist last-hlc in `device_config`.
static HLC_STATE: Mutex<Hlc> = Mutex::new(Hlc { ms: 0, counter: 0 });

impl Hlc {
    /// Generate a new HLC, monotonically non-decreasing relative to
    /// every prior call in the same process.
    ///
    /// # Saturation (v1 acceptable limit)
    ///
    /// If the internal state reaches `ms == HLC_MAX_MS` AND
    /// `counter == HLC_MAX_COUNTER`, further calls will reset the
    /// counter to 0 at the same `ms`, breaking strict `Ord`
    /// monotonicity. Unreachable in practice (~year 6429 from epoch)
    /// but documented here. Multi-device sync in v2 will persist
    /// `last_hlc` in `device_config` and detect this explicitly.
    /// # Panics
    ///
    /// Panics if the internal HLC mutex is poisoned (only possible if a
    /// prior call panicked while holding the lock, which is unreachable
    /// in normal operation).
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // WHY: d.as_millis() is u128 but clamped below HLC_MAX_MS (2^47-1 < 2^64).
    pub fn now() -> Self {
        let wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
            .min(HLC_MAX_MS);

        let mut state = HLC_STATE.lock().expect("HLC mutex poisoned");
        if wall_ms > state.ms {
            *state = Self {
                ms: wall_ms,
                counter: 0,
            };
        } else if state.counter >= HLC_MAX_COUNTER {
            // Counter saturated within one ms — advance ms to
            // preserve total order. At HLC_MAX_MS this saturates
            // (documented above).
            *state = Self {
                ms: state.ms.saturating_add(1).min(HLC_MAX_MS),
                counter: 0,
            };
        } else {
            state.counter += 1;
        }
        *state
    }

    /// Pack into a non-negative `i64` suitable for `SQLite` `INTEGER`
    /// (stored as a raw integer column). Sign bit is always 0 because
    /// `ms` is masked to 48 bits and `counter` is masked to 15 bits
    /// via [`HLC_MAX_COUNTER`] before shifting into bits 48-62.
    #[must_use]
    #[allow(clippy::cast_possible_wrap)] // WHY: ms_bits masked to 48 bits + counter_bits capped to 15 bits in 48-62; bit 63 always 0, so i64 is non-negative.
    pub fn pack(&self) -> i64 {
        let ms_bits = self.ms & ((1u64 << 48) - 1);
        let counter_bits = u64::from(self.counter & HLC_MAX_COUNTER) << 48;
        (ms_bits | counter_bits) as i64
    }

    /// Inverse of [`Hlc::pack`]. Accepts any `i64` produced by `pack`;
    /// behaviour on values with bit-63 set is undefined (packed HLCs
    /// are always non-negative).
    #[must_use]
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)] // WHY: packed values from pack() are always non-negative i64 with known bit layout.
    pub fn unpack(packed: i64) -> Self {
        let bits = packed as u64;
        let ms = bits & ((1u64 << 48) - 1);
        let counter = ((bits >> 48) & u64::from(HLC_MAX_COUNTER)) as u16;
        Self { ms, counter }
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)] // WHY: tests clamp ms below HLC_MAX_MS before casting u128 → u64.
mod tests {
    use super::*;

    // WHY: the 4 state-mutating tests (monotonicity, clock-backward,
    // counter-tiebreak, counter-overflow) all read or write the
    // process-global `HLC_STATE`. Default test parallelism causes
    // cross-test state pollution. Serialize via TEST_LOCK. The pure
    // pack/unpack round-trip test doesn't need the lock.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn pack_unpack_round_trip_across_ranges() {
        for (ms, counter) in [
            (0u64, 0u16),
            (1_700_000_000_000u64, 0u16),
            (1_700_000_000_000u64, 42u16),
            (HLC_MAX_MS, HLC_MAX_COUNTER),
            (HLC_MAX_MS - 1, 0u16),
            (0u64, HLC_MAX_COUNTER),
        ] {
            let h = Hlc { ms, counter };
            assert_eq!(
                Hlc::unpack(h.pack()),
                h,
                "round-trip failed for ({ms}, {counter})"
            );
            assert!(
                h.pack() >= 0,
                "pack produced negative i64 for ({ms}, {counter})"
            );
        }
    }

    #[test]
    fn now_is_monotonically_non_decreasing_under_rapid_calls() {
        let _guard = TEST_LOCK.lock().expect("test lock poisoned");
        let mut last = Hlc::now();
        for _ in 0..10_000 {
            let curr = Hlc::now();
            assert!(curr >= last, "HLC regressed: {curr:?} < {last:?}");
            last = curr;
        }
    }

    #[test]
    fn clock_backward_safety_via_state_reuse() {
        let _guard = TEST_LOCK.lock().expect("test lock poisoned");
        // Seed HLC_STATE to a future ms, then generate. The new HLC
        // must not regress to a prior ms even if wall clock is behind.
        {
            let mut state = HLC_STATE.lock().expect("mutex");
            *state = Hlc {
                ms: state.ms.max(1_900_000_000_000),
                counter: 0,
            };
        }
        let prior = *HLC_STATE.lock().expect("mutex");
        let h = Hlc::now();
        assert!(h >= prior, "HLC regressed below forced-future state");
    }

    #[test]
    fn counter_tiebreak_within_same_ms() {
        let _guard = TEST_LOCK.lock().expect("test lock poisoned");
        // Pin state to a fixed ms and call now() multiple times; the
        // counter should advance within the same ms unless the wall
        // clock happens to tick past it mid-test.
        {
            let mut state = HLC_STATE.lock().expect("mutex");
            *state = Hlc {
                ms: state.ms.max(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() as u64),
                ),
                counter: 0,
            };
        }
        let a = Hlc::now();
        let b = Hlc::now();
        let c = Hlc::now();
        assert!(
            a <= b && b <= c,
            "non-monotonic within-ms: {a:?} {b:?} {c:?}"
        );
    }

    #[test]
    fn counter_overflow_within_same_ms_advances_ms() {
        let _guard = TEST_LOCK.lock().expect("test lock poisoned");
        // Force counter to saturation; next call must advance ms.
        let start_ms;
        {
            let mut state = HLC_STATE.lock().expect("mutex");
            start_ms = state.ms.max(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64),
            );
            *state = Hlc {
                ms: start_ms,
                counter: HLC_MAX_COUNTER,
            };
        }
        let h = Hlc::now();
        assert!(
            h.ms > start_ms || (h.ms == start_ms && h.counter == 0),
            "overflow did not advance ms or reset counter: start_ms={start_ms} got={h:?}"
        );
    }
}
