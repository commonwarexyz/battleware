// Scaling factor for fixed-point arithmetic
// Using 10000 for easy decimal representation (4 decimal places)
pub const SCALE: i32 = 10000;
pub const HALF_SCALE: i64 = SCALE as i64 / 2;

/// Fixed-point number with 4 decimal places of precision
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Decimal(i64);

impl Decimal {
    /// Create from an integer value
    pub fn from_int(value: i32) -> Self {
        Decimal(value as i64 * SCALE as i64)
    }

    /// Create from a u16 value
    pub fn from_u16(value: u16) -> Self {
        Decimal(value as i64 * SCALE as i64)
    }

    /// Create from a fraction (numerator / denominator)
    pub fn from_frac(numerator: i32, denominator: i32) -> Self {
        let num = Decimal::from_int(numerator);
        let den = Decimal::from_int(denominator);
        num.div(den)
    }

    /// Convert to integer with rounding
    pub fn to_int_rounded(self) -> i32 {
        if self.0 >= 0 {
            ((self.0 + HALF_SCALE) / SCALE as i64) as i32
        } else {
            ((self.0 - HALF_SCALE) / SCALE as i64) as i32
        }
    }

    /// Convert to u16 with rounding and clamping
    pub fn to_u16_rounded(self) -> u16 {
        self.to_int_rounded().clamp(0, u16::MAX as i32) as u16
    }

    /// Get the raw scaled value
    #[cfg(test)]
    pub fn raw(self) -> i64 {
        self.0
    }

    /// Divide by an integer
    pub fn div_int(self, other: i32) -> Self {
        Decimal(self.0 / other as i64)
    }

    /// Multiply two fixed-point numbers
    pub fn mul(self, other: Self) -> Self {
        Decimal((self.0 * other.0) / SCALE as i64)
    }

    /// Divide two fixed-point numbers
    pub fn div(self, other: Self) -> Self {
        Decimal((self.0 * SCALE as i64) / other.0)
    }
}

impl std::ops::Add for Decimal {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Decimal(self.0 + other.0)
    }
}

impl std::ops::Sub for Decimal {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Decimal(self.0 - other.0)
    }
}

impl std::ops::Neg for Decimal {
    type Output = Self;
    fn neg(self) -> Self {
        Decimal(-self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_int() {
        let f = Decimal::from_int(5);
        assert_eq!(f.raw(), 50000);

        let f = Decimal::from_int(-3);
        assert_eq!(f.raw(), -30000);
    }

    #[test]
    fn test_from_frac() {
        // Test simple fractions
        let half = Decimal::from_frac(1, 2);
        assert_eq!(half.raw(), 5000); // 0.5 * 10000

        let quarter = Decimal::from_frac(1, 4);
        assert_eq!(quarter.raw(), 2500); // 0.25 * 10000

        let three_quarters = Decimal::from_frac(3, 4);
        assert_eq!(three_quarters.raw(), 7500); // 0.75 * 10000

        // Test negative fractions
        let neg_half = Decimal::from_frac(-1, 2);
        assert_eq!(neg_half.raw(), -5000);

        let neg_third = Decimal::from_frac(-1, 3);
        assert_eq!(neg_third.raw(), -3333); // Truncated

        // Test larger fractions
        let five_halves = Decimal::from_frac(5, 2);
        assert_eq!(five_halves.raw(), 25000); // 2.5 * 10000
    }

    #[test]
    fn test_from_u16() {
        let f = Decimal::from_u16(1200);
        assert_eq!(f.raw(), 12000000);

        let f = Decimal::from_u16(0);
        assert_eq!(f.raw(), 0);

        let f = Decimal::from_u16(u16::MAX);
        assert_eq!(f.raw(), u16::MAX as i64 * SCALE as i64);
    }

    #[test]
    fn test_to_int_rounded() {
        // Test positive rounding
        let f = Decimal::from_frac(15499, 10000);
        assert_eq!(f.to_int_rounded(), 2); // rounds up

        let f = Decimal::from_frac(15000, 10000);
        assert_eq!(f.to_int_rounded(), 2); // rounds up at exactly .5

        let f = Decimal::from_frac(14999, 10000);
        assert_eq!(f.to_int_rounded(), 1); // rounds down

        // Test negative rounding
        let f = Decimal::from_frac(-15499, 10000);
        assert_eq!(f.to_int_rounded(), -2); // rounds down (away from zero)

        let f = Decimal::from_frac(-15000, 10000);
        assert_eq!(f.to_int_rounded(), -2); // rounds down at exactly -.5

        let f = Decimal::from_frac(-14999, 10000);
        assert_eq!(f.to_int_rounded(), -1); // rounds up (toward zero)
    }

    #[test]
    fn test_to_u16_rounded() {
        let f = Decimal::from_u16(1200);
        assert_eq!(f.to_u16_rounded(), 1200);

        // Test clamping at 0
        let f = Decimal::from_int(-10000);
        assert_eq!(f.to_u16_rounded(), 0);

        // Test clamping at u16::MAX
        let f = Decimal::from_int(70000);
        assert_eq!(f.to_u16_rounded(), u16::MAX);
    }

    #[test]
    fn test_arithmetic() {
        let a = Decimal::from_int(10);
        let b = Decimal::from_int(3);

        // Test addition
        let sum = a + b;
        assert_eq!(sum.to_int_rounded(), 13);

        // Test subtraction
        let diff = a - b;
        assert_eq!(diff.to_int_rounded(), 7);

        // Test negation
        let neg = -a;
        assert_eq!(neg.to_int_rounded(), -10);
    }

    #[test]
    fn test_multiplication() {
        let a = Decimal::from_int(10);
        let b = Decimal::from_int(3);

        // Test multiplication
        let prod = a.mul(b);
        assert_eq!(prod.to_int_rounded(), 30);

        // Test fractional multiplication
        let half = Decimal::from_frac(1, 2);
        let result = a.mul(half);
        assert_eq!(result.to_int_rounded(), 5);
    }

    #[test]
    fn test_division() {
        let a = Decimal::from_int(10);
        let b = Decimal::from_int(4);

        // Test division by Fixed
        let quot = a.div(b);
        assert_eq!(quot.raw(), 25000); // 2.5 * 10000

        // Test division by integer
        let quot = a.div_int(4);
        assert_eq!(quot.raw(), 25000); // 2.5 * 10000
    }

    #[test]
    fn test_comparison() {
        let a = Decimal::from_int(10);
        let b = Decimal::from_int(5);
        let c = Decimal::from_int(10);

        assert!(a > b);
        assert!(b < a);
        assert_eq!(a, c);
        assert!(a >= c);
        assert!(a <= c);
    }
}
