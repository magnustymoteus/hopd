//! Turns held movement keys into pointer displacement. Pure logic, no Wayland.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Down,
    Up,
    Right,
}

impl Direction {
    fn index(self) -> usize {
        self as usize
    }
}

/// How fast the pointer moves, in pixels per second.
#[derive(Debug, Clone, Copy)]
pub struct Speed {
    /// Speed the moment a movement key is pressed.
    pub initial: f64,
    /// Added for every second movement keys stay held.
    pub acceleration: f64,
    /// Upper limit.
    pub max: f64,
}

impl Speed {
    /// Speed after movement keys have been held for `held`.
    pub fn after(&self, held: Duration) -> f64 {
        (self.initial + self.acceleration * held.as_secs_f64()).min(self.max)
    }
}

/// Which movement keys are currently held.
#[derive(Debug, Default)]
pub struct HeldKeys([bool; 4]);

impl HeldKeys {
    pub fn set(&mut self, direction: Direction, held: bool) {
        self.0[direction.index()] = held;
    }

    pub fn any(&self) -> bool {
        self.0.iter().any(|&held| held)
    }

    /// Unit vector of the combined held directions (screen coordinates: +y is down),
    /// or (0, 0) when nothing is held or opposite keys cancel out.
    fn heading(&self) -> (f64, f64) {
        let [left, down, up, right] = self.0.map(f64::from);
        let (x, y) = (right - left, down - up);
        let length = x.hypot(y);
        if length == 0.0 {
            (0.0, 0.0)
        } else {
            (x / length, y / length)
        }
    }

    /// Displacement for one tick: `speed` pixels per second for `elapsed` time.
    pub fn displacement(&self, speed: f64, elapsed: Duration) -> (f64, f64) {
        let (x, y) = self.heading();
        let distance = speed * elapsed.as_secs_f64();
        (x * distance, y * distance)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDirection {
    Down,
    Up,
}

/// Which scroll keys are currently held.
#[derive(Debug, Default)]
pub struct ScrollKeys([bool; 2]);

impl ScrollKeys {
    pub fn set(&mut self, direction: ScrollDirection, held: bool) {
        self.0[direction as usize] = held;
    }

    pub fn any(&self) -> bool {
        self.0.iter().any(|&held| held)
    }

    /// Vertical scroll amount for one tick: `speed` units per second for `elapsed` time.
    /// Positive scrolls down, matching wl_pointer's vertical axis.
    pub fn delta(&self, speed: f64, elapsed: Duration) -> f64 {
        let [down, up] = self.0.map(f64::from);
        (down - up) * speed * elapsed.as_secs_f64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrolling(directions: &[ScrollDirection]) -> ScrollKeys {
        let mut keys = ScrollKeys::default();
        for &direction in directions {
            keys.set(direction, true);
        }
        keys
    }

    #[test]
    fn scroll_down_is_positive_and_up_is_negative() {
        let one_second = Duration::from_secs(1);
        assert_eq!(
            scrolling(&[ScrollDirection::Down]).delta(300.0, one_second),
            300.0
        );
        assert_eq!(
            scrolling(&[ScrollDirection::Up]).delta(300.0, one_second),
            -300.0
        );
    }

    #[test]
    fn scroll_scales_with_elapsed_time() {
        let keys = scrolling(&[ScrollDirection::Down]);
        assert_eq!(keys.delta(300.0, Duration::from_millis(100)), 30.0);
    }

    #[test]
    fn opposite_scroll_keys_cancel() {
        let keys = scrolling(&[ScrollDirection::Down, ScrollDirection::Up]);
        assert_eq!(keys.delta(300.0, Duration::from_secs(1)), 0.0);
        assert!(keys.any());
        assert!(!ScrollKeys::default().any());
    }

    const SPEED: Speed = Speed {
        initial: 220.0,
        acceleration: 700.0,
        max: 1600.0,
    };

    fn held(directions: &[Direction]) -> HeldKeys {
        let mut keys = HeldKeys::default();
        for &direction in directions {
            keys.set(direction, true);
        }
        keys
    }

    fn assert_close(actual: (f64, f64), expected: (f64, f64)) {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(
            close(actual.0, expected.0) && close(actual.1, expected.1),
            "{actual:?} != {expected:?}"
        );
    }

    #[test]
    fn speed_grows_linearly_then_caps() {
        assert_eq!(SPEED.after(Duration::ZERO), 220.0);
        assert_eq!(SPEED.after(Duration::from_secs(1)), 920.0);
        assert_eq!(SPEED.after(Duration::from_secs(10)), 1600.0);
    }

    #[test]
    fn single_key_moves_along_its_axis() {
        let one_second = Duration::from_secs(1);
        assert_close(
            held(&[Direction::Right]).displacement(100.0, one_second),
            (100.0, 0.0),
        );
        assert_close(
            held(&[Direction::Left]).displacement(100.0, one_second),
            (-100.0, 0.0),
        );
        assert_close(
            held(&[Direction::Down]).displacement(100.0, one_second),
            (0.0, 100.0),
        );
        assert_close(
            held(&[Direction::Up]).displacement(100.0, one_second),
            (0.0, -100.0),
        );
    }

    #[test]
    fn distance_scales_with_elapsed_time() {
        let keys = held(&[Direction::Right]);
        assert_close(
            keys.displacement(120.0, Duration::from_millis(500)),
            (60.0, 0.0),
        );
    }

    #[test]
    fn diagonal_moves_at_the_same_speed_as_straight() {
        let (x, y) =
            held(&[Direction::Right, Direction::Up]).displacement(100.0, Duration::from_secs(1));
        assert!((x.hypot(y) - 100.0).abs() < 1e-9);
        assert!(x > 0.0 && y < 0.0);
    }

    #[test]
    fn opposite_keys_cancel() {
        let keys = held(&[Direction::Left, Direction::Right]);
        assert_close(keys.displacement(100.0, Duration::from_secs(1)), (0.0, 0.0));
        assert!(keys.any());
    }

    #[test]
    fn nothing_held() {
        let keys = HeldKeys::default();
        assert!(!keys.any());
        assert_close(keys.displacement(100.0, Duration::from_secs(1)), (0.0, 0.0));
    }
}
