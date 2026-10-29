//! Hints: points on screen, each with a label to type. Pane mode puts an even grid of them
//! over a window (Wayland doesn't let clients see inside other windows, so there are no
//! buttons to label); window mode puts one at the centre of each window.

#[derive(Debug, Clone, PartialEq)]
pub struct Hint {
    pub label: Vec<char>,
    /// Position in pixels, in whatever coordinates the points were given in.
    pub x: f64,
    pub y: f64,
}

/// What a typed character did.
#[derive(Debug, PartialEq)]
pub enum Typed {
    /// Fewer labels match now.
    Narrowed,
    /// The character completed the label of the hint at `index`, counting in the order
    /// the points were given.
    Picked { index: usize, x: f64, y: f64 },
    /// No label continues with it, so it was dropped.
    Ignored,
}

pub struct Hints {
    hints: Vec<Hint>,
    typed: Vec<char>,
}

impl Hints {
    /// Hints `spacing` pixels apart, centred in a `width` × `height` window, row by row.
    pub fn grid(width: f64, height: f64, spacing: f64, chars: &[char]) -> Hints {
        let xs = axis(width, spacing);
        let ys = axis(height, spacing);
        let points = ys.iter().flat_map(|&y| xs.iter().map(move |&x| (x, y)));
        Hints::at(points.collect(), chars)
    }

    /// A hint at each point, labelled in order. Labels all have the same length, the
    /// shortest that gives every point its own label, so no label is the start of another.
    pub fn at(points: Vec<(f64, f64)>, chars: &[char]) -> Hints {
        let hints = labels(points.len(), chars)
            .zip(points)
            .map(|(label, (x, y))| Hint { label, x, y })
            .collect();
        Hints {
            hints,
            typed: Vec::new(),
        }
    }

    pub fn typed(&self) -> &[char] {
        &self.typed
    }

    /// The hints whose labels start with what has been typed.
    pub fn visible(&self) -> impl Iterator<Item = &Hint> {
        self.hints
            .iter()
            .filter(|hint| hint.label.starts_with(&self.typed))
    }

    pub fn push(&mut self, c: char) -> Typed {
        self.typed.push(c);
        // Copied out, so the borrow of `self` ends before `typed` may change again.
        let first = self
            .hints
            .iter()
            .enumerate()
            .find(|(_, hint)| hint.label.starts_with(&self.typed))
            .map(|(index, hint)| (index, hint.label.len(), hint.x, hint.y));
        match first {
            None => {
                self.typed.pop();
                Typed::Ignored
            }
            Some((index, length, x, y)) if length == self.typed.len() => {
                Typed::Picked { index, x, y }
            }
            Some(_) => Typed::Narrowed,
        }
    }

    /// Takes back the last typed character, if any.
    pub fn pop(&mut self) {
        self.typed.pop();
    }
}

/// Positions along one axis: as many as fit `spacing` apart, centred in `length`.
fn axis(length: f64, spacing: f64) -> Vec<f64> {
    let count = ((length / spacing).floor() as usize).max(1);
    let start = (length - (count - 1) as f64 * spacing) / 2.0;
    (0..count).map(|i| start + i as f64 * spacing).collect()
}

/// The first `count` labels of the shortest length that has enough of them, in counting
/// order: aa, ab, ..., ba, ...
fn labels(count: usize, chars: &[char]) -> impl Iterator<Item = Vec<char>> {
    let base = chars.len();
    let mut length = 1;
    while base.pow(length) < count {
        length += 1;
    }
    (0..count).map(move |mut n| {
        let mut label = vec![chars[0]; length as usize];
        for slot in label.iter_mut().rev() {
            *slot = chars[n % base];
            n /= base;
        }
        label
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(label: &[char]) -> String {
        label.iter().collect()
    }

    #[test]
    fn points_are_centred_and_spaced() {
        assert_eq!(axis(200.0, 60.0), [40.0, 100.0, 160.0]);
        // A window smaller than the spacing still gets one point, in its middle.
        assert_eq!(axis(30.0, 60.0), [15.0]);
    }

    #[test]
    fn labels_use_the_shortest_length_that_fits() {
        let chars = ['a', 'b', 'c'];
        let short: Vec<String> = labels(3, &chars).map(|l| text(&l)).collect();
        assert_eq!(short, ["a", "b", "c"]);
        let long: Vec<String> = labels(5, &chars).map(|l| text(&l)).collect();
        assert_eq!(long, ["aa", "ab", "ac", "ba", "bb"]);
    }

    #[test]
    fn hints_cover_the_window_row_by_row() {
        let hints = Hints::grid(120.0, 120.0, 60.0, &['a', 'b', 'c', 'd']);
        let all: Vec<(String, f64, f64)> = hints
            .visible()
            .map(|h| (text(&h.label), h.x, h.y))
            .collect();
        assert_eq!(
            all,
            [
                ("a".into(), 30.0, 30.0),
                ("b".into(), 90.0, 30.0),
                ("c".into(), 30.0, 90.0),
                ("d".into(), 90.0, 90.0),
            ]
        );
    }

    #[test]
    fn typing_narrows_then_picks() {
        // 5 points with 2 chars: labels aaa, aab, aba, abb, baa.
        let mut hints = Hints::grid(300.0, 60.0, 60.0, &['a', 'b']);
        assert_eq!(hints.push('a'), Typed::Narrowed);
        assert_eq!(hints.visible().count(), 4);
        assert_eq!(hints.push('b'), Typed::Narrowed);
        assert_eq!(
            hints.push('a'),
            Typed::Picked {
                index: 2,
                x: 150.0,
                y: 30.0
            }
        );
    }

    #[test]
    fn hints_can_go_at_any_points() {
        let mut hints = Hints::at(vec![(500.0, 20.0), (-30.0, 7.5)], &['a', 'b', 'c']);
        let all: Vec<(String, f64, f64)> = hints
            .visible()
            .map(|h| (text(&h.label), h.x, h.y))
            .collect();
        assert_eq!(all, [("a".into(), 500.0, 20.0), ("b".into(), -30.0, 7.5)]);
        assert_eq!(
            hints.push('b'),
            Typed::Picked {
                index: 1,
                x: -30.0,
                y: 7.5
            }
        );
    }

    #[test]
    fn no_points_means_nothing_to_type() {
        let mut hints = Hints::at(Vec::new(), &['a', 'b']);
        assert_eq!(hints.visible().count(), 0);
        assert_eq!(hints.push('a'), Typed::Ignored);
    }

    #[test]
    fn characters_no_label_continues_with_are_ignored() {
        let mut hints = Hints::grid(300.0, 60.0, 60.0, &['a', 'b']);
        assert_eq!(hints.push('b'), Typed::Narrowed);
        assert_eq!(hints.push('b'), Typed::Ignored);
        assert_eq!(hints.push('z'), Typed::Ignored);
        assert_eq!(hints.typed(), ['b']);
    }

    #[test]
    fn backspace_widens_again() {
        let mut hints = Hints::grid(300.0, 60.0, 60.0, &['a', 'b']);
        hints.push('a');
        hints.push('b');
        hints.pop();
        assert_eq!(hints.typed(), ['a']);
        assert_eq!(hints.visible().count(), 4);
        hints.pop();
        hints.pop();
        assert_eq!(hints.visible().count(), 5);
    }
}
