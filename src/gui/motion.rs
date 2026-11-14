//! Interruptible sidebar motion. Each new target starts at the current visual width.
use std::time::{Duration, Instant};

pub const SIDEBAR_DURATION: Duration = Duration::from_millis(180);
pub const PAGE_DURATION: Duration = Duration::from_millis(150);

pub struct SidebarMotion {
    from: f32,
    to: f32,
    started: Instant,
}

impl SidebarMotion {
    pub fn new(width: f32) -> Self {
        Self {
            from: width,
            to: width,
            started: Instant::now(),
        }
    }

    pub fn value(&self, now: Instant) -> f32 {
        let progress = (now.saturating_duration_since(self.started).as_secs_f32()
            / SIDEBAR_DURATION.as_secs_f32())
        .clamp(0., 1.);
        let eased = 1. - (1. - progress).powi(5);
        self.from + (self.to - self.from) * eased
    }

    pub fn retarget(&mut self, width: f32, now: Instant) {
        self.from = self.value(now);
        self.to = width;
        self.started = now;
    }

    pub fn active(&self, now: Instant) -> bool {
        self.from != self.to && now.saturating_duration_since(self.started) < SIDEBAR_DURATION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reversing_mid_transition_preserves_width_and_reaches_the_new_target() {
        let mut motion = SidebarMotion::new(232.);
        let start = Instant::now();
        motion.retarget(0., start);
        let midway = start + Duration::from_millis(60);
        let width = motion.value(midway);
        assert!(width > 0. && width < 232.);
        motion.retarget(232., midway);
        assert_eq!(motion.value(midway), width);
        assert!(motion.value(midway + Duration::from_millis(20)) > width);
        assert_eq!(motion.value(midway + SIDEBAR_DURATION), 232.);
        assert!(!motion.active(midway + SIDEBAR_DURATION));
    }
}
