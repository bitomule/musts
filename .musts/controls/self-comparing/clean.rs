//! Control: the near-miss. Same shape as `violating.rs` down to the
//! binding name, but the two sides of the assertion are different
//! expressions, so the question must stay quiet.
//!
//! The near-miss is the point. A clean control that shares nothing with
//! the violating one only proves the question can tell source code from
//! an empty file.

pub fn total(items: &[u32]) -> u32 {
    items.iter().sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_items() {
        let total = total(&[1, 2, 3]);
        let total_expected = 6;
        assert_eq!(total, total_expected);
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(total(&[]), 0);
    }
}
