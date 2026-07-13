//! [`Clock`] implementation backed by the system clock.

use crate::ports::Clock;
use std::time::{Duration, Instant};

/// Real wall-clock and monotonic time.
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// A clock whose monotonic origin is the moment of construction.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_iso8601(&self) -> String {
        // Truncate to whole seconds: this feeds page titles, where
        // sub-second precision is noise.
        jiff::Timestamp::now()
            .round(jiff::Unit::Second)
            .unwrap_or_else(|_| jiff::Timestamp::now())
            .to_string()
    }

    fn monotonic(&self) -> Duration {
        self.origin.elapsed()
    }
}
