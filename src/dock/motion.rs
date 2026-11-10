//! Short, interruptible collection-item transitions.

use std::time::{Duration, Instant};

pub const ENTER_MS: u64 = 220;
pub const MOVE_MS: u64 = 240;
pub const EXIT_MS: u64 = 190;
const ENTER_SCALE: f32 = 0.45;

pub fn zoom(opacity: f32) -> f32 {
    ENTER_SCALE + (1.0 - ENTER_SCALE) * opacity.clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Copy)]
pub struct Motion {
    pub from: f32,
    pub to: f32,
    pub opacity_from: f32,
    pub opacity_to: f32,
    pub started: Instant,
    pub duration: Duration,
}

impl Motion {
    pub fn progress(self, now: Instant) -> (f32, bool) {
        let t = (now.duration_since(self.started).as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0);
        (1.0 - (1.0 - t).powi(3), t < 1.0)
    }

    pub fn value(self, now: Instant) -> (f32, f32, bool) {
        let (eased, active) = self.progress(now);
        (
            self.from + (self.to - self.from) * eased,
            self.opacity_from + (self.opacity_to - self.opacity_from) * eased,
            active,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_reaches_both_identity_position_and_opacity_endpoints() {
        let start = Instant::now();
        let motion = Motion {
            from: 3.0,
            to: 1.0,
            opacity_from: 0.0,
            opacity_to: 1.0,
            started: start,
            duration: Duration::from_millis(MOVE_MS),
        };
        assert_eq!(motion.value(start), (3.0, 0.0, true));
        assert_eq!(motion.value(start + Duration::from_millis(MOVE_MS)), (1.0, 1.0, false));
    }

    #[test]
    fn transition_midpoint_eases_toward_target_without_overshoot() {
        let start = Instant::now();
        let motion = Motion {
            from: 0.0,
            to: 2.0,
            opacity_from: 1.0,
            opacity_to: 0.0,
            started: start,
            duration: Duration::from_millis(MOVE_MS),
        };
        let (position, opacity, active) = motion.value(start + Duration::from_millis(MOVE_MS / 2));
        assert!(position > 1.0 && position < 2.0);
        assert!(opacity < 0.5 && opacity > 0.0);
        assert!(active);
    }

    #[test]
    fn zoom_follows_opacity_and_returns_to_full_size() {
        assert_eq!(zoom(0.0), ENTER_SCALE);
        assert!(zoom(0.5) > ENTER_SCALE && zoom(0.5) < 1.0);
        assert_eq!(zoom(1.0), 1.0);
    }
}
