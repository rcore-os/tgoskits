#![no_std]

/// Add two numbers.
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

/// Subtract `b` from `a`.
pub fn sub(a: i64, b: i64) -> i64 {
    a - b
}

/// Clamp a value within a range `[lo, hi]`.
///
/// Returns `lo` if `val < lo`, `hi` if `val > hi`, otherwise `val`.
pub fn clamp(val: i64, lo: i64, hi: i64) -> i64 {
    if val < lo {
        lo
    } else if val > hi {
        hi
    } else {
        val
    }
}

/// Compute the greatest common divisor using Euclidean algorithm.
pub fn gcd(a: u64, b: u64) -> u64 {
    let mut a = a;
    let mut b = b;
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(2, 3), 5);
        assert_eq!(add(-1, 1), 0);
    }

    #[test]
    fn test_sub() {
        assert_eq!(sub(5, 3), 2);
        assert_eq!(sub(0, 1), -1);
    }

    #[test]
    fn test_clamp() {
        assert_eq!(clamp(5, 0, 10), 5);
        assert_eq!(clamp(-1, 0, 10), 0);
        assert_eq!(clamp(15, 0, 10), 10);
    }

    #[test]
    fn test_gcd() {
        assert_eq!(gcd(12, 8), 4);
        assert_eq!(gcd(7, 0), 7);
        assert_eq!(gcd(0, 0), 0);
    }
}
