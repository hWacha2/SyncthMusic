//! Hybrid Logical Clock.
//!
//! Нужен вместо `SystemTime`, потому что часы устройств расходятся, а
//! разрешение конфликтов должно быть детерминированным и причинно-корректным.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Hlc {
    /// Физическое время в миллисекундах (максимум из локального и наблюдённого).
    pub ms: u64,
    /// Логический счётчик для событий внутри одной миллисекунды.
    pub counter: u32,
}

impl Hlc {
    pub fn new(ms: u64, counter: u32) -> Self {
        Self { ms, counter }
    }

    /// Текущее физическое время в мс. Устойчиво к часам до 1970 года.
    pub fn wall_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Локальное событие.
    pub fn tick(&mut self) -> Hlc {
        let now = Self::wall_ms();
        if now > self.ms {
            self.ms = now;
            self.counter = 0;
        } else {
            // Часы не сдвинулись или ушли назад — спасаемся счётчиком.
            self.counter = self.counter.saturating_add(1);
        }
        *self
    }

    /// Событие после получения удалённого HLC.
    pub fn observe(&mut self, remote: Hlc) -> Hlc {
        let now = Self::wall_ms();
        let max_ms = now.max(self.ms).max(remote.ms);
        if max_ms == self.ms && max_ms == remote.ms {
            self.counter = self.counter.max(remote.counter).saturating_add(1);
        } else if max_ms == self.ms {
            self.counter = self.counter.saturating_add(1);
        } else if max_ms == remote.ms {
            self.counter = remote.counter.saturating_add(1);
        } else {
            self.counter = 0;
        }
        self.ms = max_ms;
        *self
    }

    /// Полный порядок: время, затем счётчик.
    pub fn cmp_hlc(&self, other: &Hlc) -> Ordering {
        (self.ms, self.counter).cmp(&(other.ms, other.counter))
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp_hlc(other)
    }
}

impl std::fmt::Display for Hlc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.ms, self.counter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_are_monotonic_even_if_wall_clock_stalls() {
        let mut clock = Hlc::new(1_000, 0);
        let a = clock.tick();
        let b = clock.tick();
        let c = clock.tick();
        assert!(a < b && b < c, "HLC must be strictly increasing");
    }

    #[test]
    fn observe_advances_past_remote() {
        let mut clock = Hlc::new(100, 0);
        let remote = Hlc::new(10_000, 5);
        let after = clock.observe(remote);
        assert!(after > remote);
    }

    #[test]
    fn wall_clock_going_backwards_does_not_break_order() {
        let mut clock = Hlc::new(u64::MAX / 2, 0);
        let first = clock.tick();
        let second = clock.tick();
        assert!(second > first);
    }
}
