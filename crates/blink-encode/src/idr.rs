//! When to force an IDR frame.
//!
//! NVENC runs with an infinite GOP, so it never inserts keyframes on its own;
//! this scheduler decides. Capture is variable-rate (unchanged frames are
//! skipped and the rate is decimated), so a frame-count GOP would stretch to
//! tens of seconds on an idle desktop. The ring needs an IDR at least once per
//! second of PTS, so the interval is measured in timestamps.

use blink_ring::media::Timestamp;

/// Tracks the last IDR and any pending request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdrScheduler {
    /// Force an IDR once this much PTS has passed since the last one.
    interval: Timestamp,
    /// PTS of the last IDR the encoder produced, or `None` before the first
    /// one (and after a reconfigure).
    last_idr: Option<Timestamp>,
    /// An explicit request, kept until an IDR is produced.
    requested: bool,
}

impl IdrScheduler {
    /// A scheduler that forces an IDR on the first frame and then at least
    /// once per second of PTS.
    ///
    /// The interval is one second minus half a frame, so a frame landing a
    /// hair before the one-second mark (timestamp jitter) is still made an
    /// IDR instead of pushing the GOP to almost two seconds.
    pub(crate) const fn new(frame_duration: Timestamp) -> Self {
        Self {
            interval: Timestamp(Timestamp::TICKS_PER_SECOND - frame_duration.0 / 2),
            last_idr: None,
            requested: false,
        }
    }

    /// Asks for the next frame to be an IDR.
    pub(crate) const fn request(&mut self) {
        self.requested = true;
    }

    /// Forgets the last IDR so the next frame is one (after a reconfigure).
    pub(crate) const fn reset(&mut self) {
        self.last_idr = None;
    }

    /// Whether a frame at `pts` must be an IDR.
    ///
    /// A `pts` earlier than the last IDR (a clock step backwards) does not
    /// force one; the interval restarts from the next IDR.
    pub(crate) fn due(&self, pts: Timestamp) -> bool {
        if self.requested {
            return true;
        }
        self.last_idr
            .is_none_or(|last| pts.0.saturating_sub(last.0) >= self.interval.0)
    }

    /// Records the picture type the encoder actually produced for `pts`.
    pub(crate) const fn observe(&mut self, pts: Timestamp, idr: bool) {
        if idr {
            self.last_idr = Some(pts);
            self.requested = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME_60: Timestamp = Timestamp(166_666);

    fn ms(millis: i64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    /// Runs `pts` through the scheduler as the encoder would, assuming NVENC
    /// honours every forced IDR and inserts none of its own.
    fn idr_frames(scheduler: &mut IdrScheduler, pts: &[Timestamp]) -> Vec<Timestamp> {
        let mut idrs = Vec::new();
        for &frame in pts {
            let idr = scheduler.due(frame);
            scheduler.observe(frame, idr);
            if idr {
                idrs.push(frame);
            }
        }
        idrs
    }

    #[test]
    fn first_frame_is_an_idr() {
        let scheduler = IdrScheduler::new(FRAME_60);
        assert!(scheduler.due(ms(5)));
    }

    #[test]
    fn constant_rate_gets_one_idr_per_second() {
        let mut scheduler = IdrScheduler::new(FRAME_60);
        let pts: Vec<Timestamp> = (0..181).map(|n| Timestamp(n * FRAME_60.0)).collect();
        let idrs = idr_frames(&mut scheduler, &pts);
        // Frames 0, 60, 120, 180 (60 frames of 166 666 ticks is 0.99999 s,
        // within half a frame of one second).
        assert_eq!(
            idrs,
            [
                Timestamp(0),
                Timestamp(60 * FRAME_60.0),
                Timestamp(120 * FRAME_60.0),
                Timestamp(180 * FRAME_60.0)
            ]
        );
    }

    #[test]
    fn idle_variable_rate_still_gets_an_idr_every_second() {
        // An idle desktop updating twice a second: a 60-frame GOP would be
        // 30 s long. By timestamp, every second gets an IDR.
        let mut scheduler = IdrScheduler::new(FRAME_60);
        let pts: Vec<Timestamp> = (0..7).map(|n| ms(n * 500)).collect();
        let idrs = idr_frames(&mut scheduler, &pts);
        assert_eq!(idrs, [ms(0), ms(1000), ms(2000), ms(3000)]);
    }

    #[test]
    fn static_screen_reports_due_for_a_reencode() {
        let mut scheduler = IdrScheduler::new(FRAME_60);
        scheduler.observe(ms(0), true);
        assert!(!scheduler.due(ms(900)));
        assert!(scheduler.due(ms(1000)));
    }

    #[test]
    fn request_forces_the_next_frame_until_an_idr_is_produced() {
        let mut scheduler = IdrScheduler::new(FRAME_60);
        scheduler.observe(ms(0), true);
        scheduler.request();
        assert!(scheduler.due(ms(17)));
        // The driver did not produce one (or the encode failed): still due.
        scheduler.observe(ms(17), false);
        assert!(scheduler.due(ms(33)));
        scheduler.observe(ms(33), true);
        assert!(!scheduler.due(ms(50)));
    }

    #[test]
    fn reset_forces_the_next_frame() {
        let mut scheduler = IdrScheduler::new(FRAME_60);
        scheduler.observe(ms(0), true);
        scheduler.reset();
        assert!(scheduler.due(ms(17)));
    }

    #[test]
    fn idrs_the_encoder_inserts_itself_restart_the_interval() {
        let mut scheduler = IdrScheduler::new(FRAME_60);
        scheduler.observe(ms(0), true);
        scheduler.observe(ms(600), true);
        assert!(!scheduler.due(ms(1000)));
        assert!(scheduler.due(ms(1600)));
    }

    #[test]
    fn a_backwards_clock_step_does_not_force_an_idr() {
        let mut scheduler = IdrScheduler::new(FRAME_60);
        scheduler.observe(ms(5000), true);
        assert!(!scheduler.due(ms(4000)));
    }
}
