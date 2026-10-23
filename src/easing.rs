//! Easing curves: cubic Béziers from (0, 0) to (1, 1), like CSS `cubic-bezier()` and
//! Hyprland's `bezier`. x is the fraction of the duration elapsed, y the fraction of the
//! change done.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Curve {
    /// The two control points. x values stay within 0..=1 so time never runs backwards.
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

impl Curve {
    pub const LINEAR: Curve = Curve::new(0.0, 0.0, 1.0, 1.0);
    /// CSS's `ease-out`.
    pub const EASE_OUT: Curve = Curve::new(0.0, 0.0, 0.58, 1.0);

    pub const fn new(x1: f64, y1: f64, x2: f64, y2: f64) -> Curve {
        Curve { x1, y1, x2, y2 }
    }

    /// How far along the change is at `progress` (0..=1) of the duration.
    pub fn at(&self, progress: f64) -> f64 {
        let x = progress.clamp(0.0, 1.0);
        if x == 0.0 || x == 1.0 {
            return x;
        }
        let t = self.solve_t(x);
        sample(self.y1, self.y2, t)
    }

    /// The curve is defined by a parameter t, not by x, so find the t where the curve's
    /// x equals `x`. Newton's method converges in a few steps; bisection is the fallback
    /// where the slope is too flat for Newton to be reliable.
    fn solve_t(&self, x: f64) -> f64 {
        let mut t = x;
        for _ in 0..8 {
            let error = sample(self.x1, self.x2, t) - x;
            if error.abs() < 1e-7 {
                return t;
            }
            let slope = slope(self.x1, self.x2, t);
            if slope.abs() < 1e-6 {
                break;
            }
            t -= error / slope;
        }
        // x(t) only rises from 0 to 1, so halving the interval always finds it.
        let (mut low, mut high) = (0.0, 1.0);
        t = x;
        for _ in 0..50 {
            let value = sample(self.x1, self.x2, t);
            if (value - x).abs() < 1e-7 {
                break;
            }
            if value < x {
                low = t;
            } else {
                high = t;
            }
            t = (low + high) / 2.0;
        }
        t
    }
}

/// One coordinate of the Bézier at t, with the end points fixed at 0 and 1:
/// 3(1-t)²t·p1 + 3(1-t)t²·p2 + t³, written as a polynomial in t.
fn sample(p1: f64, p2: f64, t: f64) -> f64 {
    let c = 3.0 * p1;
    let b = 3.0 * (p2 - p1) - c;
    let a = 1.0 - c - b;
    ((a * t + b) * t + c) * t
}

/// The derivative of `sample` with respect to t.
fn slope(p1: f64, p2: f64, t: f64) -> f64 {
    let c = 3.0 * p1;
    let b = 3.0 * (p2 - p1) - c;
    let a = 1.0 - c - b;
    (3.0 * a * t + 2.0 * b) * t + c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn linear_is_the_identity() {
        for x in [0.0, 0.1, 0.5, 0.9, 1.0] {
            assert!(close(Curve::LINEAR.at(x), x), "{x}");
        }
    }

    #[test]
    fn ends_are_fixed_and_progress_is_clamped() {
        let curve = Curve::new(0.15, 0.0, 0.1, 1.0);
        assert_eq!(curve.at(0.0), 0.0);
        assert_eq!(curve.at(1.0), 1.0);
        assert_eq!(curve.at(-1.0), 0.0);
        assert_eq!(curve.at(2.0), 1.0);
    }

    #[test]
    fn ease_out_matches_sampled_values() {
        // Reference values for cubic-bezier(0, 0, 0.58, 1), found by sampling the curve
        // densely and picking the point closest to each x.
        assert!(
            close(Curve::EASE_OUT.at(0.25), 0.3781),
            "{}",
            Curve::EASE_OUT.at(0.25)
        );
        assert!(
            close(Curve::EASE_OUT.at(0.5), 0.6846),
            "{}",
            Curve::EASE_OUT.at(0.5)
        );
    }

    #[test]
    fn steep_curves_still_rise_monotonically() {
        // Hyprland's "quick": almost all of the change happens early.
        let quick = Curve::new(0.15, 0.0, 0.1, 1.0);
        let mut last = 0.0;
        for i in 1..=100 {
            let y = quick.at(f64::from(i) / 100.0);
            assert!(y >= last - 1e-9, "{i}: {y} < {last}");
            last = y;
        }
    }

    #[test]
    fn overshooting_curves_go_past_one() {
        // y may leave 0..=1 for a bounce; only x is restricted.
        let back = Curve::new(0.3, 1.5, 0.7, 1.5);
        assert!(back.at(0.6) > 1.0);
    }
}
