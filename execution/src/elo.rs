use crate::fixed::Decimal;

// K-factor determines how much ratings change after each match
// Higher K = more volatile ratings, lower K = more stable ratings
const K: i32 = 32;

// Scaling factor for the tanh-based sigmoid approximation
// Standard ELO uses the formula: 1 / (1 + 10^(-diff/400))
// We approximate this with: 0.5 * (1 - tanh(diff/TANH_SCALE))
// The value 347 is empirically chosen to make the tanh approximation
// match the standard ELO probabilities:
// - 400 point difference: 90.93% vs 90.91% (standard)
// - 200 point difference: 76.00% vs 75.97% (standard)
// - 100 point difference: 64.02% vs 64.01% (standard)
const TANH_SCALE: i32 = 347;

// Calculate the damage dealt by a player, taking into account overkill
fn calculate_damage_dealt(health: i32, max_health: i32) -> i32 {
    assert!(health <= max_health);
    if health >= 0 {
        max_health - health
    } else {
        max_health + health.abs()
    }
}

/// Updates the ratings of two players based on their remaining health.
///
/// # Arguments
/// * `rating_a` - The current `u16` rating of player A.
/// * `health_a` - The remaining health of player A (can be negative to represent overkill).
/// * `max_health_a` - The maximum health of player A.
/// * `rating_b` - The current `u16` rating of player B.
/// * `health_b` - The remaining health of player B (can be negative to represent overkill).
/// * `max_health_b` - The maximum health of player B.
///
/// # Returns
/// A tuple `(u16, u16)` containing the new ratings for player A and player B.
pub fn update(
    rating_a: u16,
    health_a: i16,
    max_health_a: u8,
    rating_b: u16,
    health_b: i16,
    max_health_b: u8,
) -> (u16, u16) {
    // Convert ratings to fixed-point
    let rating_a_fp = Decimal::from_u16(rating_a);
    let rating_b_fp = Decimal::from_u16(rating_b);

    // Work with regular integers for health calculations
    let health_a_i32 = health_a as i32;
    let max_health_a_i32 = max_health_a as i32;
    let health_b_i32 = health_b as i32;
    let max_health_b_i32 = max_health_b as i32;

    // Calculate expected score (returns Fixed value 0.0 to 1.0)
    let expected_a = calculate_expected_score(rating_a_fp, rating_b_fp);

    // Calculate actual score based on health remaining
    // Score is proportional to damage dealt (including overkill)
    let damage_dealt_by_a = calculate_damage_dealt(health_b_i32, max_health_b_i32);
    let damage_dealt_by_b = calculate_damage_dealt(health_a_i32, max_health_a_i32);

    // If neither player does anything, total damage will be 0
    let total_damage = damage_dealt_by_a + damage_dealt_by_b;
    if total_damage == 0 {
        return (rating_a, rating_b);
    }

    // Actual score for A is their proportion of total damage dealt
    let actual_a = Decimal::from_int(damage_dealt_by_a).div_int(total_damage);

    // Calculate rating changes (zero-sum)
    let k_fp = Decimal::from_int(K);
    let change_a = k_fp.mul(actual_a - expected_a);
    let change_b = -change_a;

    // Apply changes to ratings
    let new_rating_a_fp = rating_a_fp + change_a;
    let new_rating_b_fp = rating_b_fp + change_b;

    // Convert back to u16 with clamping
    let new_rating_a = new_rating_a_fp.to_u16_rounded();
    let new_rating_b = new_rating_b_fp.to_u16_rounded();
    (new_rating_a, new_rating_b)
}

/// Calculate expected score using a smooth sigmoid approximation
/// This provides a deterministic approximation of the Elo formula
/// using a tanh-based function that avoids exponentials
/// Returns a Fixed value from 0.0 to 1.0 representing probability
fn calculate_expected_score(rating_a: Decimal, rating_b: Decimal) -> Decimal {
    // Use tanh approximation for sigmoid: 0.5 * (1 - tanh(x))
    // where x is scaled appropriately for Elo ratings
    let rating_diff = (rating_b - rating_a).to_int_rounded();

    // Apply the tanh scaling factor
    let x_fp = Decimal::from_int(rating_diff).div_int(TANH_SCALE);

    // Approximate tanh using a rational function
    // tanh(x) ≈ x * (27 + x^2) / (27 + 9*x^2)

    // x^2
    let x_squared = x_fp.mul(x_fp);

    // Calculate numerator: x * (27 + x^2)
    let twenty_seven = Decimal::from_int(27);
    let numerator = x_fp.mul(twenty_seven + x_squared);

    // Calculate denominator: 27 + 9*x^2
    let nine = Decimal::from_int(9);
    let denominator = twenty_seven + nine.mul(x_squared);

    // tanh_x = numerator / denominator
    let tanh_x = numerator.div(denominator);

    // Convert to expected score: 0.5 * (1 - tanh(x))
    let one = Decimal::from_int(1);
    let half = Decimal::from_frac(1, 2);
    let expected = half.mul(one - tanh_x);

    // Clamp to reasonable bounds to handle extreme rating differences
    // Between 0.01 and 0.99
    let min_expected = Decimal::from_frac(1, 100);
    let max_expected = Decimal::from_frac(99, 100);
    expected.clamp(min_expected, max_expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_win() {
        // Test decisive win (A wins with full health, B loses all)
        let (new_a, new_b) = update(1200, 100, 100, 1200, 0, 100);
        assert!(new_a > 1200);
        assert!(new_b < 1200);
        assert_eq!(new_a + new_b, 2400); // Total rating conserved

        // Test close win (A wins with little health left)
        let (new_a_close, new_b_close) = update(1200, 10, 100, 1200, 0, 100);
        assert!(new_a_close > 1200);
        assert!(new_b_close < 1200);
        // Close win should give less rating change than decisive win
        assert!(new_a_close - 1200 < new_a - 1200);

        // Test underdog decisive win
        let (new_a, new_b) = update(1000, 80, 80, 1400, 0, 80);
        assert!(new_a > 1000);
        assert!(new_b < 1400);
        let gain_a = new_a - 1000;
        let loss_b = 1400 - new_b;
        assert_eq!(gain_a, loss_b); // Zero-sum
        assert!(gain_a > 16); // Underdog gains more
    }

    #[test]
    fn test_loss() {
        // Test decisive loss (A loses all health, B keeps full)
        let (new_a, new_b) = update(1200, 0, 100, 1200, 100, 100);
        assert!(new_a < 1200);
        assert!(new_b > 1200);
        assert_eq!(new_a + new_b, 2400); // Total rating conserved

        // Test favorite decisive loss
        let (new_a, new_b) = update(1400, 0, 100, 1000, 100, 100);
        assert!(new_a < 1400);
        assert!(new_b > 1000);
        let loss_a = 1400 - new_a;
        let gain_b = new_b - 1000;
        assert_eq!(loss_a, gain_b); // Zero-sum
        assert!(loss_a > 16); // Favorite loses more when upset
    }

    #[test]
    fn test_draw() {
        // Test exact draw (both lose all health)
        let (new_a, new_b) = update(1200, 0, 100, 1200, 0, 100);
        assert_eq!(new_a, 1200);
        assert_eq!(new_b, 1200);

        // Test near-draw with equal damage dealt
        let (new_a, new_b) = update(1200, 20, 100, 1200, 20, 100);
        // Should be very close to original ratings
        assert!((new_a as i32 - 1200).abs() <= 1);
        assert!((new_b as i32 - 1200).abs() <= 1);

        // Test draw-like result with different ratings
        let (new_a, new_b) = update(1000, 50, 100, 1400, 50, 100);
        assert!(new_a > 1000); // Lower rated gains
        assert!(new_b < 1400); // Higher rated loses
        assert_eq!((new_a as i32 - 1000), -(new_b as i32 - 1400)); // Zero-sum
    }

    #[test]
    fn test_bounds() {
        // Test minimum rating bound
        let (new_a, _) = update(0, 0, 100, 1500, 100, 100);
        assert_eq!(new_a, 0);

        // Test maximum rating bound
        let (new_a, _) = update(u16::MAX, 100, 100, 1000, 0, 100);
        assert_eq!(new_a, u16::MAX);
    }

    #[test]
    fn test_expected_scores() {
        // Test expected score calculations with same ratings
        let expected = calculate_expected_score(Decimal::from_int(1200), Decimal::from_int(1200));
        assert!((expected.raw() - 5000).abs() < 100); // Should be ~0.5

        // Test with rating advantage
        let expected = calculate_expected_score(Decimal::from_int(1400), Decimal::from_int(1000));
        assert!(expected.raw() > 9000); // Strong favorite (>0.9)

        // Test with rating disadvantage
        let expected = calculate_expected_score(Decimal::from_int(1000), Decimal::from_int(1400));
        assert!(expected.raw() < 1000); // Strong underdog (<0.1)
    }

    #[test]
    fn test_damage_dealt() {
        assert_eq!(calculate_damage_dealt(100, 100), 0);
        assert_eq!(calculate_damage_dealt(0, 100), 100);
        assert_eq!(calculate_damage_dealt(0, 0), 0);
    }

    #[test]
    #[should_panic]
    fn test_damage_dealt_panic() {
        calculate_damage_dealt(100, 0);
    }

    #[test]
    fn test_overkill() {
        // Test overkill damage counting in a more realistic scenario
        // Both players deal damage, but B deals overkill
        let (new_a_overkill, new_b_overkill) = update(1200, 20, 100, 1200, -30, 100);
        // A took 80 damage, B took 130 damage (30 overkill)

        // Compare with non-overkill scenario (B at exactly 0)
        let (new_a_normal, new_b_normal) = update(1200, 20, 100, 1200, 0, 100);
        // A took 80 damage, B took 100 damage (no overkill)

        // With overkill, A dealt more total damage (130 vs 100), so should gain more rating
        assert!(new_a_overkill > new_a_normal);
        assert!(new_b_overkill < new_b_normal);

        // Test with both players dealing overkill
        let (new_a, new_b) = update(1200, -30, 100, 1200, -50, 100);
        // A dealt 150 damage, B dealt 130 damage
        // A should gain rating (dealt more damage)
        assert!(new_a > 1200);
        assert!(new_b < 1200);

        // Test where overkill reverses the outcome
        let (new_a_slight, _new_b_slight) = update(1200, 5, 100, 1200, 0, 100);
        let (new_a_massive, _new_b_massive) = update(1200, 5, 100, 1200, -100, 100);
        // In slight win: A dealt 100, B dealt 95 (A wins by small margin)
        // In massive overkill: A dealt 200, B dealt 95 (A dominates)
        assert!(new_a_massive - 1200 > new_a_slight - 1200);
    }
}
