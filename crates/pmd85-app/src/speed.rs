//! Emulation speed control and real-time pacing.
//!
//! The machine must be stepped at 50 Hz of *emulated* time, but the wall
//! clock is the only valid reference at 1x speed. [`frames_due`] is the
//! pure pacing kernel: given the wall-clock time of the next emulated
//! frame, it computes how many frames to run now. Turbo (unthrottled)
//! bypasses the wall clock entirely and runs frames until a compute
//! budget runs out.

use std::time::{Duration, Instant};

/// Emulated frame period (50 Hz video).
pub const FRAME_PERIOD_SECS: f64 = 0.020;
/// Emulated frames per second.
pub const FRAMES_PER_SEC: f64 = 50.0;

/// Fraction of each render frame turbo spends emulating; the rest is
/// left for the UI so it stays interactive while turbo runs.
pub const TURBO_DUTY_CYCLE: f64 = 0.85;
/// Bounds for the adaptive per-render-frame turbo budget.
pub const TURBO_BUDGET_MIN: Duration = Duration::from_millis(1);
pub const TURBO_BUDGET_MAX: Duration = Duration::from_millis(50);
/// Budget for the first render frame of a turbo engagement, before
/// the render cadence has been measured.
pub const TURBO_BUDGET_INIT: Duration = Duration::from_millis(8);

/// Per-render-frame turbo emulation budget for a measured render
/// interval: a [`TURBO_DUTY_CYCLE`] fraction of it, clamped.
///
/// A fixed budget cannot work: the render loop is vsync-bound (a 60 Hz
/// display allows ~16.6 ms per render frame), so a fixed 8 ms would
/// cap turbo at under half the CPU — slower than a high speed
/// multiplier, which runs every frame the wall clock owes it with no
/// budget at all.
pub fn turbo_budget(interval: Duration) -> Duration {
    Duration::from_secs_f64(interval.as_secs_f64() * TURBO_DUTY_CYCLE)
        .clamp(TURBO_BUDGET_MIN, TURBO_BUDGET_MAX)
}

/// Available speed presets (multipliers of real time).
pub const SPEED_PRESETS: &[f64] = &[0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 25.0, 50.0];

/// Allowed speed multiplier range, inclusive (0.25x to 50x real time).
pub const SPEED_MIN: f64 = 0.25;
pub const SPEED_MAX: f64 = 50.0;

/// How many emulated frames to run now, and the wall-clock time the
/// *next* emulated frame is due.
///
/// `next_due` is the deadline of the next *unrun* frame: a frame whose
/// deadline is exactly `now` is due and runs. `now` and `next_due` are
/// seconds since an arbitrary common epoch. `multiplier` is the speed
/// relative to real time (1.0 = real time). Behind-schedule catch-up
/// is bounded so that a long stall (window drag, resize) does not
/// fast-forward the machine in one giant burst; beyond the bound the
/// pacer resynchronizes to `now`.
pub fn frames_due(now: f64, next_due: f64, multiplier: f64) -> (u32, f64) {
    assert!(
        multiplier.is_finite() && multiplier > 0.0,
        "bad multiplier {multiplier}"
    );
    let period = FRAME_PERIOD_SECS / multiplier;
    let mut frames: u32 = 0;
    let mut due = next_due;
    // Bound the burst: at high multipliers a whole wall-frame worth of
    // emulated frames is legitimately owed, so scale the cap with the
    // speed; never less than the 1x catch-up bound.
    let max_catch_up = ((FRAMES_PER_SEC / 60.0 * 2.0 * multiplier).ceil() as u32).max(6);
    while now >= due && frames < max_catch_up {
        frames += 1;
        due += period;
    }
    if now >= due {
        // Too far behind to catch up gracefully: resynchronize.
        due = now;
    }
    (frames, due)
}

/// Wall-clock state of the emulator's frame pacing.
///
/// Holds the wall-clock time at which the next emulated frame is due
/// (see [`frames_due`]); turbo mode ignores it.
#[derive(Debug)]
pub struct Pacer {
    /// Wall-clock time of the next emulated frame.
    next_due: Instant,
    /// Seconds-epoch counterpart of `next_due`, for the pure kernel.
    epoch: Instant,
    /// Test-only: when set, [`Self::take_frames`] returns this many
    /// frames on every call, ignoring the wall clock.
    #[cfg(test)]
    forced: Option<u32>,
}

impl Default for Pacer {
    fn default() -> Self {
        Self::new()
    }
}

impl Pacer {
    pub fn new() -> Self {
        let epoch = Instant::now();
        Pacer {
            next_due: epoch,
            epoch,
            #[cfg(test)]
            forced: None,
        }
    }

    /// Frames to run at `multiplier` right now.
    pub fn take_frames(&mut self, multiplier: f64) -> u32 {
        #[cfg(test)]
        if let Some(frames) = self.forced {
            return frames;
        }
        let now = self.epoch.elapsed().as_secs_f64();
        let due = self.next_due.duration_since(self.epoch).as_secs_f64();
        let (frames, new_due) = frames_due(now, due, multiplier);
        self.next_due = self.epoch + Duration::from_secs_f64(new_due);
        frames
    }

    /// Drop accumulated debt (after a pause, turbo run, or machine
    /// reconfiguration) without fast-forwarding.
    pub fn resync(&mut self) {
        self.next_due = Instant::now();
    }

    /// Wall-clock time the next emulated frame is due (turbo ignores it).
    pub fn next_due(&self) -> Instant {
        self.next_due
    }

    /// Test-only: pin the emulator to `frames` frames per
    /// [`Self::take_frames`] call, ignoring the wall clock, so tests
    /// can run frame-paced flows (boots, auto-typed loads) without
    /// real-time sleeps.
    #[cfg(test)]
    pub fn force_frames(&mut self, frames: u32) {
        self.forced = Some(frames);
    }
}

/// Adaptive turbo pacing: measures the render cadence and hands out
/// the emulation budget for each render frame.
///
/// The budget follows the measured render interval (smoothed, since
/// vsync jitter must not swing it): turbo spends
/// [`TURBO_DUTY_CYCLE`] of each render frame emulating. If a frame
/// overshoots (a missed vsync), the measured interval grows and so
/// does the next budget, converging on the fastest render cadence the
/// host can sustain while keeping the UI alive. [`Self::reset`] is
/// called whenever turbo is not engaged, so a fresh engagement starts
/// from [`TURBO_BUDGET_INIT`] and adapts within a few frames.
#[derive(Debug)]
pub struct Turbo {
    /// Exponentially smoothed interval between turbo render frames;
    /// `None` until the first interval has been measured.
    interval: Option<Duration>,
    /// Render time of the previous [`Self::budget`] call.
    last: Option<Instant>,
}

impl Default for Turbo {
    fn default() -> Self {
        Self::new()
    }
}

impl Turbo {
    pub fn new() -> Self {
        Turbo {
            interval: None,
            last: None,
        }
    }

    /// Emulation budget for this render frame; call once per render
    /// frame while turbo is engaged.
    pub fn budget(&mut self) -> Duration {
        let now = Instant::now();
        let sample = self.last.replace(now).map(|prev| now.duration_since(prev));
        match (sample, self.interval) {
            (Some(sample), Some(interval)) => {
                let smoothed = interval.mul_f64(0.7) + sample.mul_f64(0.3);
                self.interval = Some(smoothed);
                turbo_budget(smoothed)
            }
            // First measured interval: adopt it without smoothing.
            (Some(sample), None) => {
                self.interval = Some(sample);
                turbo_budget(sample)
            }
            // First frame of an engagement: cadence unknown yet.
            (None, _) => TURBO_BUDGET_INIT,
        }
    }

    /// Forget the measured cadence: turbo disengaged (or the machine
    /// paused), so the next engagement starts fresh.
    pub fn reset(&mut self) {
        self.interval = None;
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `next_due` is the deadline of the next *unrun* frame: at
    // `now == next_due` that frame is due and runs. All frame
    // deadlines before it have already been consumed.

    #[test]
    fn no_frames_before_deadline() {
        // 10 ms short of the next deadline at 1x.
        let (frames, due) = frames_due(10.010, 10.020, 1.0);
        assert_eq!(frames, 0);
        assert_eq!(due, 10.020);
    }

    #[test]
    fn frame_runs_at_its_deadline() {
        // The pending frame (due at 10.010) runs; the next one is
        // due at 10.030.
        let (frames, due) = frames_due(10.025, 10.010, 1.0);
        assert_eq!(frames, 1);
        assert!((due - 10.030).abs() < 1e-9);
    }

    #[test]
    fn consecutive_deadlines_all_run() {
        // 25 ms past the deadline at 1x: the frames due at 10.000,
        // 10.020 both run; the next is due at 10.040.
        let (frames, due) = frames_due(10.025, 10.0, 1.0);
        assert_eq!(frames, 2);
        assert!((due - 10.040).abs() < 1e-9);
    }

    #[test]
    fn slow_speed_stretches_the_period() {
        // At 0.5x a frame is due every 40 ms.
        // 30 ms past the deadline: only the one pending frame runs.
        let (frames, due) = frames_due(10.030, 10.0, 0.5);
        assert_eq!(frames, 1);
        assert!((due - 10.040).abs() < 1e-9);
        // 10 ms short of the next deadline: nothing due.
        let (frames, due) = frames_due(10.030, 10.040, 0.5);
        assert_eq!(frames, 0);
        assert_eq!(due, 10.040);
    }

    #[test]
    fn fast_speed_shrinks_the_period() {
        // At 2x a frame is due every 10 ms; 25 ms past the deadline
        // owes three frames (10.000, 10.010, 10.020), the fourth at
        // 10.030.
        let (frames, due) = frames_due(10.025, 10.0, 2.0);
        assert_eq!(frames, 3);
        assert!((due - 10.030).abs() < 1e-9);
    }

    #[test]
    fn catch_up_is_bounded_at_1x() {
        // A full second behind at 1x: only the catch-up bound of frames
        // run now, and the pacer resynchronizes.
        let (frames, due) = frames_due(11.0, 10.0, 1.0);
        assert_eq!(frames, 6);
        assert_eq!(due, 11.0);
    }

    #[test]
    fn catch_up_scales_with_speed() {
        // 100 ms behind at 10x (period 2 ms): 50 frames owed, bound is
        // ceil(50/60*2*10) = 17, then resync.
        let (frames, due) = frames_due(10.100, 10.0, 10.0);
        assert_eq!(frames, 17);
        assert_eq!(due, 10.100);
        // A full second behind at 10x: same bound, resync.
        let (frames, due) = frames_due(11.0, 10.0, 10.0);
        assert_eq!(frames, 17);
        assert_eq!(due, 11.0);
    }

    #[test]
    #[should_panic]
    fn rejects_bad_multiplier() {
        frames_due(0.0, 0.0, 0.0);
    }

    #[test]
    fn turbo_budget_tracks_the_render_interval() {
        // A 60 Hz render cadence: most of the frame is spent
        // emulating, a fraction is left for the UI.
        let budget = turbo_budget(Duration::from_secs_f64(1.0 / 60.0));
        assert!((budget.as_secs_f64() - (1.0 / 60.0) * TURBO_DUTY_CYCLE).abs() < 1e-9);
        // A missed vsync (30 Hz) grows the budget proportionally, so
        // turbo still fills the frame instead of stalling at a fixed
        // cap.
        let budget = turbo_budget(Duration::from_secs_f64(1.0 / 30.0));
        assert!((budget.as_secs_f64() - (1.0 / 30.0) * TURBO_DUTY_CYCLE).abs() < 1e-9);
    }

    #[test]
    fn turbo_budget_is_clamped() {
        // Far below any render cadence: keep a floor so at least some
        // frames run.
        assert_eq!(turbo_budget(Duration::ZERO), TURBO_BUDGET_MIN);
        // A throttled/occluded window: do not let one render frame
        // monopolize the process.
        assert_eq!(
            turbo_budget(Duration::from_secs(10)),
            TURBO_BUDGET_MAX
        );
    }

    #[test]
    fn turbo_starts_from_the_initial_budget() {
        let mut turbo = Turbo::new();
        // First frame of an engagement: cadence unknown.
        assert_eq!(turbo.budget(), TURBO_BUDGET_INIT);
        // Resetting (turbo off, machine paused) forgets the cadence.
        turbo.reset();
        assert_eq!(turbo.budget(), TURBO_BUDGET_INIT);
    }

    #[test]
    fn turbo_adapts_to_a_slow_cadence() {
        let mut turbo = Turbo::new();
        turbo.budget();
        // Frame pacing far slower than a real redraw: the measured
        // interval grows, and with it the budget (up to the clamp).
        std::thread::sleep(Duration::from_millis(30));
        let first = turbo.budget();
        assert!(first > TURBO_BUDGET_INIT);
        std::thread::sleep(Duration::from_millis(30));
        let second = turbo.budget();
        assert!(second >= TURBO_BUDGET_MIN && second <= TURBO_BUDGET_MAX);
    }

    #[test]
    fn pacer_tracks_wall_clock() {
        let mut pacer = Pacer::new();
        // Freshly created: the first frame is due immediately (the
        // machine starts running without a startup delay).
        let f = pacer.take_frames(1.0);
        assert_eq!(f, 1);
        // Before the next 20 ms deadline: nothing more is owed.
        let f = pacer.take_frames(1.0);
        assert_eq!(f, 0);
        // After sleeping past one frame period at 1x, a frame is owed.
        std::thread::sleep(Duration::from_millis(21));
        let f = pacer.take_frames(1.0);
        assert_eq!(f, 1);
    }
}
