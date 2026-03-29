/// Restart intensity tracking (OTP-style).
///
/// Tracks restart timestamps in a ring buffer. If more than `max_restarts`
/// restarts occur within `window` duration, the intensity is exceeded and
/// the supervisor should escalate to its parent.
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct RestartIntensity {
    max_restarts: u32,
    window: Duration,
    timestamps: VecDeque<Instant>,
}

impl RestartIntensity {
    pub fn new(max_restarts: u32, window: Duration) -> Self {
        Self {
            max_restarts,
            window,
            timestamps: VecDeque::with_capacity(max_restarts as usize + 1),
        }
    }

    /// Record a restart. Returns `true` if intensity is exceeded
    /// (too many restarts within the window).
    pub fn record_restart(&mut self) -> bool {
        self.record_restart_at(Instant::now())
    }

    /// Record a restart at a specific time (for testing).
    pub fn record_restart_at(&mut self, now: Instant) -> bool {
        self.timestamps.push_back(now);

        // Evict timestamps older than the window
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        while self.timestamps.front().is_some_and(|&t| t < cutoff) {
            self.timestamps.pop_front();
        }

        // Exceeded if count > max_restarts
        self.timestamps.len() > self.max_restarts as usize
    }

    /// Reset the intensity counter (e.g. after a stable period).
    pub fn reset(&mut self) {
        self.timestamps.clear();
    }

    /// Current number of restarts within the window.
    pub fn current_count(&self) -> usize {
        self.timestamps.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_under_threshold() {
        let mut intensity = RestartIntensity::new(3, Duration::from_secs(60));
        let now = Instant::now();

        assert!(!intensity.record_restart_at(now));
        assert!(!intensity.record_restart_at(now));
        assert!(!intensity.record_restart_at(now));
        assert_eq!(intensity.current_count(), 3);
    }

    #[test]
    fn test_exceeds_threshold() {
        let mut intensity = RestartIntensity::new(3, Duration::from_secs(60));
        let now = Instant::now();

        assert!(!intensity.record_restart_at(now));
        assert!(!intensity.record_restart_at(now));
        assert!(!intensity.record_restart_at(now));
        // 4th restart within the window should exceed
        assert!(intensity.record_restart_at(now));
    }

    #[test]
    fn test_old_restarts_evicted() {
        let mut intensity = RestartIntensity::new(2, Duration::from_secs(10));
        let t0 = Instant::now();

        assert!(!intensity.record_restart_at(t0));
        assert!(!intensity.record_restart_at(t0));

        // 15 seconds later, old restarts should be evicted
        let t1 = t0 + Duration::from_secs(15);
        assert!(!intensity.record_restart_at(t1));
        assert_eq!(intensity.current_count(), 1);
    }

    #[test]
    fn test_reset() {
        let mut intensity = RestartIntensity::new(2, Duration::from_secs(60));
        let now = Instant::now();

        intensity.record_restart_at(now);
        intensity.record_restart_at(now);
        assert_eq!(intensity.current_count(), 2);

        intensity.reset();
        assert_eq!(intensity.current_count(), 0);
    }
}
