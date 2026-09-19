//! Control: this file really does break `self_comparing`. If the question
//! does not fire here it is broken, whatever it answers on real history.
//!
//! Not compiled by anything — it is a file for a question to read.

pub fn total(items: &[u32]) -> u32 {
    items.iter().sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_items() {
        let total = total(&[1, 2, 3]);
        assert_eq!(total, total);
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(total(&[]), 0);
    }
}
