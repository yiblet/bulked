//! [`LineRange`]: the single owner of chunk line-range arithmetic.
//!
//! A chunk covers `len` lines starting at 1-indexed line `start`, both of which
//! are non-zero by construction. Every "which lines does this chunk touch?"
//! question — inclusive/exclusive end, overlap, adjacency — is answered here so
//! that `start + len - 1` never has to be written (or gotten wrong) anywhere else.

use std::fmt;
use std::num::NonZeroUsize;

/// A non-empty, 1-indexed range of lines: `start..=start + len - 1`.
///
/// Ordering is by `(start, len)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LineRange {
    start: NonZeroUsize,
    len: NonZeroUsize,
}

impl LineRange {
    /// Creates a range from an already-validated start line and length.
    pub const fn new(start: NonZeroUsize, len: NonZeroUsize) -> Self {
        Self { start, len }
    }

    /// Creates a range from plain integers; `None` if either `start` or `len` is zero.
    pub fn from_usize(start: usize, len: usize) -> Option<Self> {
        Some(Self::new(
            NonZeroUsize::new(start)?,
            NonZeroUsize::new(len)?,
        ))
    }

    /// The 1-indexed first line of the range.
    pub const fn start(self) -> usize {
        self.start.get()
    }

    /// The number of lines in the range (always `>= 1`).
    pub const fn len(self) -> usize {
        self.len.get()
    }

    /// The 1-indexed last line of the range (`start + len - 1`).
    pub const fn end_inclusive(self) -> usize {
        self.start.get() + self.len.get() - 1
    }

    /// The 1-indexed line just past the range (`start + len`).
    pub const fn end_exclusive(self) -> usize {
        self.start.get() + self.len.get()
    }

    /// `true` if the two ranges share at least one line. Symmetric.
    pub fn overlaps(self, other: Self) -> bool {
        self.start() <= other.end_inclusive() && other.start() <= self.end_inclusive()
    }

    /// `true` if one range ends on the line immediately before the other starts,
    /// i.e. they touch without overlapping. Symmetric.
    // Part of the LineRange contract (see the plan's Target Shape); no production
    // caller yet — apply only needs `overlaps`. Exercised by this module's tests.
    #[allow(dead_code)]
    pub fn is_adjacent_to(self, other: Self) -> bool {
        self.end_exclusive() == other.start() || other.end_exclusive() == self.start()
    }
}

impl fmt::Display for LineRange {
    /// Renders as `"5-7"` (inclusive ends), or just `"5"` for a single line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.len() == 1 {
            write!(f, "{}", self.start())
        } else {
            write!(f, "{}-{}", self.start(), self.end_inclusive())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lr(start: usize, len: usize) -> LineRange {
        LineRange::from_usize(start, len).expect("test ranges are non-zero")
    }

    #[test]
    fn test_line_range_from_usize_rejects_zero() {
        assert_eq!(LineRange::from_usize(0, 1), None);
        assert_eq!(LineRange::from_usize(1, 0), None);
        assert_eq!(LineRange::from_usize(0, 0), None);
        let r = LineRange::from_usize(1, 1).expect("(1,1) is a valid range");
        assert_eq!(r.start(), 1);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn test_line_range_ends() {
        let r = lr(5, 3);
        assert_eq!(r.end_inclusive(), 7);
        assert_eq!(r.end_exclusive(), 8);
        // A single line starts and ends on the same line.
        assert_eq!(lr(5, 1).end_inclusive(), 5);
        assert_eq!(lr(5, 1).end_exclusive(), 6);
    }

    #[test]
    fn test_line_range_overlaps_is_inclusive() {
        // (1,2) covers 1-2; (2,1) covers 2: they share line 2.
        assert!(lr(1, 2).overlaps(lr(2, 1)));
        assert!(lr(2, 1).overlaps(lr(1, 2)));
        // (3,1) starts right after (1,2) ends: no shared line.
        assert!(!lr(1, 2).overlaps(lr(3, 1)));
        assert!(!lr(3, 1).overlaps(lr(1, 2)));
        // A range overlaps itself, and containment counts.
        assert!(lr(4, 2).overlaps(lr(4, 2)));
        assert!(lr(1, 10).overlaps(lr(5, 1)));
    }

    #[test]
    fn test_line_range_adjacency() {
        assert!(lr(1, 2).is_adjacent_to(lr(3, 1)));
        assert!(lr(3, 1).is_adjacent_to(lr(1, 2)));
        assert!(!lr(1, 2).is_adjacent_to(lr(4, 1)));
        assert!(!lr(4, 1).is_adjacent_to(lr(1, 2)));
        // Overlapping ranges are not adjacent.
        assert!(!lr(1, 2).is_adjacent_to(lr(2, 1)));
    }

    #[test]
    fn test_line_range_display() {
        assert_eq!(lr(5, 3).to_string(), "5-7");
        assert_eq!(lr(5, 1).to_string(), "5");
    }
}
