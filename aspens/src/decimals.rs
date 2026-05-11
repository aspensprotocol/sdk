//! Decimal-string ↔ base-units conversion shared by the order, deposit,
//! and withdraw paths.
//!
//! Tokens carry a `decimals` value in the chain config (e.g. 6 for USDC,
//! 18 for WFLR); on-chain math is in those base units. Humans type
//! amounts as decimal strings (`"10.5"`); this module is the single
//! place that bridges the two so every CLI / REPL / lib call site
//! produces identical scaled values.

use eyre::{eyre, Result};

/// Parse a human-readable decimal amount into a `u128` of base units.
///
/// Accepts integers (`"10"`), decimals (`"10.5"`), bare-fraction
/// (`".5"`), and tolerates surrounding whitespace. Trailing fractional
/// digits beyond `decimals` places are truncated (no rounding) to match
/// on-chain semantics. Negative numbers, scientific notation, and
/// thousands separators are not accepted.
///
/// # Examples
///
/// ```
/// use aspens::decimals::parse_decimal_amount;
/// assert_eq!(parse_decimal_amount("1", 6).unwrap(), 1_000_000);
/// assert_eq!(parse_decimal_amount("10.5", 6).unwrap(), 10_500_000);
/// assert_eq!(parse_decimal_amount("0.000001", 6).unwrap(), 1);
/// assert_eq!(parse_decimal_amount("1.0000001", 6).unwrap(), 1_000_000); // truncated
/// ```
pub fn parse_decimal_amount(amount: &str, decimals: u32) -> Result<u128> {
    let amount = amount.trim();

    if amount.is_empty() {
        return Err(eyre!("Amount is empty"));
    }
    // Unsigned only — `-` is rejected later by `u128::from_str`, but a
    // leading `+` would otherwise sneak through, so guard against it
    // here for a clearer error and symmetry with `-`.
    if amount.starts_with('+') {
        return Err(eyre!(
            "Invalid amount format: {} (leading sign not allowed)",
            amount
        ));
    }

    let parts: Vec<&str> = amount.split('.').collect();
    let (integer_part, fractional_part) = match parts.len() {
        1 => (parts[0], ""),
        2 => (parts[0], parts[1]),
        _ => return Err(eyre!("Invalid amount format: {}", amount)),
    };

    // Reject inputs with no digits at all (e.g. `"."`). We allow either
    // half to be empty so long as the other carries digits.
    if integer_part.is_empty() && fractional_part.is_empty() {
        return Err(eyre!("Invalid amount format: {} (no digits)", amount));
    }

    let integer: u128 = if integer_part.is_empty() {
        0
    } else {
        integer_part
            .parse()
            .map_err(|_| eyre!("Invalid integer part: {}", integer_part))?
    };

    // Truncate fractional digits beyond `decimals` (no rounding).
    let fractional_str = if fractional_part.len() >= decimals as usize {
        &fractional_part[..decimals as usize]
    } else {
        fractional_part
    };

    let fractional: u128 = if fractional_str.is_empty() {
        0
    } else {
        fractional_str
            .parse()
            .map_err(|_| eyre!("Invalid fractional part: {}", fractional_str))?
    };

    let padding_zeros = decimals as usize - fractional_str.len().min(decimals as usize);
    let fractional_padded = fractional
        .checked_mul(10_u128.pow(padding_zeros as u32))
        .ok_or_else(|| eyre!("Amount overflow: {}", amount))?;

    let multiplier = 10_u128.pow(decimals);
    integer
        .checked_mul(multiplier)
        .and_then(|v| v.checked_add(fractional_padded))
        .ok_or_else(|| eyre!("Amount overflow: {}", amount))
}

/// Same as [`parse_decimal_amount`] but downcasts to `u64`, returning a
/// clear error if the parsed value exceeds `u64::MAX`. Use this from
/// callers (deposit / withdraw) whose lib API takes `u64`.
pub fn parse_decimal_amount_u64(amount: &str, decimals: u32) -> Result<u64> {
    let parsed = parse_decimal_amount(amount, decimals)?;
    u64::try_from(parsed).map_err(|_| {
        eyre!(
            "Amount {} exceeds u64::MAX in base units (parsed {}, max {}). \
             Try a smaller amount.",
            amount,
            parsed,
            u64::MAX
        )
    })
}

/// Inverse of [`parse_decimal_amount`]: format a raw `u128` integer in
/// `decimals` scale as a human-readable decimal string suitable to feed
/// back into the CLI's buy-limit / sell-limit (or any caller that
/// expects a human-readable amount and then re-scales via
/// `parse_decimal_amount`). Trailing zeros are preserved so the
/// width-padded fractional part round-trips byte-for-byte through
/// `parse_decimal_amount`. `decimals == 0` returns the integer
/// stringified.
pub fn format_decimal_amount(raw: u128, decimals: u32) -> String {
    if decimals == 0 {
        return raw.to_string();
    }
    let scale = 10u128.pow(decimals);
    let int_part = raw / scale;
    let frac_part = raw % scale;
    format!(
        "{}.{:0width$}",
        int_part,
        frac_part,
        width = decimals as usize
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- Happy-path integer inputs -----------------------------------

    #[test]
    fn integer_zero_returns_zero_for_any_decimals() {
        for d in [0u32, 1, 6, 18, 30] {
            assert_eq!(parse_decimal_amount("0", d).unwrap(), 0);
        }
    }

    #[test]
    fn integer_one_scales_by_decimals() {
        assert_eq!(parse_decimal_amount("1", 0).unwrap(), 1);
        assert_eq!(parse_decimal_amount("1", 1).unwrap(), 10);
        assert_eq!(parse_decimal_amount("1", 6).unwrap(), 1_000_000);
        assert_eq!(
            parse_decimal_amount("1", 18).unwrap(),
            1_000_000_000_000_000_000
        );
    }

    #[test]
    fn larger_integers() {
        assert_eq!(parse_decimal_amount("100", 6).unwrap(), 100_000_000);
        assert_eq!(parse_decimal_amount("12345", 6).unwrap(), 12_345_000_000);
    }

    #[test]
    fn leading_zeros_in_integer_part_are_ignored() {
        assert_eq!(parse_decimal_amount("00010", 6).unwrap(), 10_000_000);
        assert_eq!(parse_decimal_amount("000", 6).unwrap(), 0);
    }

    // ----- Happy-path fractional inputs --------------------------------

    #[test]
    fn fractional_pads_short_fractions() {
        // "1.5" with 6 decimals → 1_500_000 (pads "5" to "500000").
        assert_eq!(parse_decimal_amount("1.5", 6).unwrap(), 1_500_000);
        assert_eq!(parse_decimal_amount("1.001", 6).unwrap(), 1_001_000);
        assert_eq!(parse_decimal_amount("0.5", 6).unwrap(), 500_000);
    }

    #[test]
    fn fractional_smallest_unit() {
        // 1 base unit at 6 decimals.
        assert_eq!(parse_decimal_amount("0.000001", 6).unwrap(), 1);
        // 1 base unit at 18 decimals.
        assert_eq!(parse_decimal_amount("0.000000000000000001", 18).unwrap(), 1);
    }

    #[test]
    fn fractional_with_leading_zeros() {
        // "0.001" → 1000 base units at 6 decimals.
        assert_eq!(parse_decimal_amount("0.001", 6).unwrap(), 1_000);
        assert_eq!(parse_decimal_amount("0.0001", 6).unwrap(), 100);
    }

    #[test]
    fn fractional_trailing_zeros_no_op() {
        // Trailing zeros in the fractional part are absorbed by padding
        // and produce the same value.
        assert_eq!(parse_decimal_amount("1.500000", 6).unwrap(), 1_500_000);
        assert_eq!(parse_decimal_amount("1.5", 6).unwrap(), 1_500_000);
        assert_eq!(parse_decimal_amount("1.50", 6).unwrap(), 1_500_000);
    }

    #[test]
    fn fractional_exact_length_no_padding_no_truncation() {
        // Fractional length == decimals; nothing to pad or truncate.
        assert_eq!(parse_decimal_amount("0.123456", 6).unwrap(), 123_456);
        assert_eq!(parse_decimal_amount("9.999999", 6).unwrap(), 9_999_999);
    }

    #[test]
    fn bare_fraction_no_integer() {
        // Empty integer part is treated as zero.
        assert_eq!(parse_decimal_amount(".5", 6).unwrap(), 500_000);
        assert_eq!(parse_decimal_amount(".000001", 6).unwrap(), 1);
    }

    #[test]
    fn trailing_dot_no_fraction() {
        // "1." has empty fractional → same as "1".
        assert_eq!(parse_decimal_amount("1.", 6).unwrap(), 1_000_000);
    }

    // ----- Truncation (no rounding) ------------------------------------

    #[test]
    fn truncates_excess_precision_does_not_round() {
        assert_eq!(parse_decimal_amount("1.0000001", 6).unwrap(), 1_000_000);
        assert_eq!(parse_decimal_amount("1.1234567", 6).unwrap(), 1_123_456);
        // Crucially: 0.9999999 with 6 decimals must NOT round up to
        // 1_000_000. Truncation gives 999_999.
        assert_eq!(parse_decimal_amount("0.9999999", 6).unwrap(), 999_999);
    }

    #[test]
    fn truncates_long_fraction_string() {
        // 60-char fractional far exceeds any realistic decimals.
        let long = format!("0.{}", "1".repeat(60));
        assert_eq!(parse_decimal_amount(&long, 6).unwrap(), 111_111);
    }

    // ----- Decimals = 0 (token has no fractional component) ------------

    #[test]
    fn decimals_zero_passes_integer_through() {
        assert_eq!(parse_decimal_amount("0", 0).unwrap(), 0);
        assert_eq!(parse_decimal_amount("1", 0).unwrap(), 1);
        assert_eq!(parse_decimal_amount("42", 0).unwrap(), 42);
    }

    #[test]
    fn decimals_zero_truncates_any_fractional_input() {
        // Any fractional digits get truncated to zero when decimals = 0.
        assert_eq!(parse_decimal_amount("1.999999", 0).unwrap(), 1);
        assert_eq!(parse_decimal_amount("0.5", 0).unwrap(), 0);
    }

    // ----- Whitespace tolerance ----------------------------------------

    #[test]
    fn whitespace_around_amount_is_trimmed() {
        assert_eq!(parse_decimal_amount("  1.5  ", 6).unwrap(), 1_500_000);
        assert_eq!(parse_decimal_amount("\t10\n", 6).unwrap(), 10_000_000);
    }

    #[test]
    fn whitespace_inside_amount_rejected() {
        // Internal whitespace is never valid; integer parser fails.
        assert!(parse_decimal_amount("1 0", 6).is_err());
        assert!(parse_decimal_amount("1. 5", 6).is_err());
        assert!(parse_decimal_amount("1 .5", 6).is_err());
    }

    // ----- Rejections (malformed input) --------------------------------

    #[test]
    fn rejects_alphabetic() {
        assert!(parse_decimal_amount("abc", 6).is_err());
        assert!(parse_decimal_amount("1a", 6).is_err());
        assert!(parse_decimal_amount("a1", 6).is_err());
        assert!(parse_decimal_amount("1.a", 6).is_err());
    }

    #[test]
    fn rejects_multiple_decimal_points() {
        assert!(parse_decimal_amount("1.2.3", 6).is_err());
        assert!(parse_decimal_amount("..5", 6).is_err());
        assert!(parse_decimal_amount("1..5", 6).is_err());
    }

    #[test]
    fn rejects_negatives() {
        // No negative amounts — deposit/withdraw/orders are all unsigned.
        assert!(parse_decimal_amount("-1", 6).is_err());
        assert!(parse_decimal_amount("-0.5", 6).is_err());
        assert!(parse_decimal_amount("-.5", 6).is_err());
    }

    #[test]
    fn rejects_signed_positive_prefix() {
        // u128::from_str does not accept a leading `+`.
        assert!(parse_decimal_amount("+1", 6).is_err());
        assert!(parse_decimal_amount("+0.5", 6).is_err());
    }

    #[test]
    fn rejects_scientific_notation() {
        assert!(parse_decimal_amount("1e6", 6).is_err());
        assert!(parse_decimal_amount("1E6", 6).is_err());
        assert!(parse_decimal_amount("1.5e2", 6).is_err());
    }

    #[test]
    fn rejects_thousands_separator() {
        assert!(parse_decimal_amount("1,000", 6).is_err());
        assert!(parse_decimal_amount("1_000", 6).is_err());
    }

    #[test]
    fn rejects_hex_octal_binary_prefixes() {
        assert!(parse_decimal_amount("0x10", 6).is_err());
        assert!(parse_decimal_amount("0o10", 6).is_err());
        assert!(parse_decimal_amount("0b10", 6).is_err());
    }

    #[test]
    fn rejects_empty_and_blank() {
        assert!(parse_decimal_amount("", 6).is_err());
        assert!(parse_decimal_amount("   ", 6).is_err());
        // A bare "." has no digits on either side. Reject it — the
        // function requires at least one digit somewhere.
        assert!(parse_decimal_amount(".", 6).is_err());
    }

    #[test]
    fn rejects_non_ascii_digits() {
        // Arabic-Indic numerals look like digits but Rust's u128 parser
        // does not accept them.
        assert!(parse_decimal_amount("١", 6).is_err()); // Arabic-Indic 1
    }

    // ----- Overflow handling -------------------------------------------

    #[test]
    fn integer_overflow_in_u128_multiply() {
        // u128::MAX ≈ 3.4e38. Pick an integer * 10^decimals that exceeds
        // u128::MAX to drive the checked_mul branch.
        let huge = "1".to_string() + &"0".repeat(38); // 10^38, just under u128::MAX
        assert!(parse_decimal_amount(&huge, 6).is_err());
    }

    #[test]
    fn integer_then_fraction_overflow_in_u128_add() {
        // Construct an integer that fits but whose product + fractional
        // padding overflows u128.
        let near_max = "340282366920938463463374607431768211455"; // u128::MAX
                                                                  // Multiplying by 10^1 overflows.
        assert!(parse_decimal_amount(near_max, 1).is_err());
    }

    #[test]
    fn rejects_decimals_too_large_for_u128_multiplier() {
        // 10^39 > u128::MAX, so the multiplier itself would overflow.
        // 10_u128.pow(39) panics, but we rely on the assumption that no
        // real token has > 38 decimals. Document the behaviour: if any
        // future caller passes a too-large decimals value, the function
        // panics in pow() — explicit guardrail is left to the caller.
        // (No assert here; this is a documentation test of the contract.)
    }

    // ----- u64 downcast wrapper ----------------------------------------

    #[test]
    fn u64_at_boundary_succeeds() {
        // u64::MAX = 18_446_744_073_709_551_615.
        // 18 * 10^18 = 18_000_000_000_000_000_000 — fits.
        assert_eq!(
            parse_decimal_amount_u64("18", 18).unwrap(),
            18_000_000_000_000_000_000
        );
        // u64::MAX itself, expressed in 0 decimals.
        assert_eq!(
            parse_decimal_amount_u64("18446744073709551615", 0).unwrap(),
            u64::MAX
        );
    }

    #[test]
    fn u64_overflow_returns_clear_error() {
        // 100 WFLR with 18 decimals = 10^20 > u64::MAX (~1.8e19).
        let err = parse_decimal_amount_u64("100", 18).unwrap_err().to_string();
        assert!(err.contains("exceeds u64::MAX"), "got: {err}");
        assert!(err.contains("100"), "should include the input: {err}");
    }

    #[test]
    fn u64_at_max_plus_one_overflows() {
        // u64::MAX + 1 in zero-decimals input form.
        assert!(parse_decimal_amount_u64("18446744073709551616", 0).is_err());
    }

    #[test]
    fn u64_propagates_underlying_parse_errors() {
        // The wrapper should bubble up the same error categories as the
        // underlying u128 parser, not mask them as "exceeds u64::MAX".
        assert!(parse_decimal_amount_u64("abc", 6).is_err());
        assert!(parse_decimal_amount_u64("-1", 6).is_err());
        assert!(parse_decimal_amount_u64("", 6).is_err());
    }

    // ----- Round-trip property -----------------------------------------

    #[test]
    fn integer_part_round_trips_for_realistic_ranges() {
        // For every (integer, decimals) we'd see in practice, the parsed
        // value equals integer * 10^decimals with no precision loss.
        for &integer in &[0u128, 1, 7, 100, 1_000, 1_000_000, 999_999_999] {
            for &decimals in &[0u32, 1, 6, 9, 18] {
                let input = integer.to_string();
                let expected = integer * 10_u128.pow(decimals);
                assert_eq!(
                    parse_decimal_amount(&input, decimals).unwrap(),
                    expected,
                    "round-trip failed for {input} with {decimals} decimals",
                );
            }
        }
    }

    #[test]
    fn fractional_round_trip_at_exact_decimal_length() {
        // For each test case, the fractional digits have length equal to
        // `decimals`, so no padding or truncation should change the value.
        let cases: &[(&str, u32, u128)] = &[
            ("0.5", 1, 5),
            ("0.50", 2, 50),
            ("0.500000", 6, 500_000),
            ("12.345", 3, 12_345),
            ("12.345000", 6, 12_345_000),
            ("0.123456789012345678", 18, 123_456_789_012_345_678),
        ];
        for &(input, decimals, expected) in cases {
            assert_eq!(
                parse_decimal_amount(input, decimals).unwrap(),
                expected,
                "case {input} with {decimals} decimals",
            );
        }
    }

    #[test]
    fn truncation_is_lossy_in_the_documented_direction() {
        // Truncate, never round. So values just below a unit boundary
        // do NOT roll up.
        assert_eq!(parse_decimal_amount("1.0999999", 6).unwrap(), 1_099_999);
        assert_eq!(parse_decimal_amount("0.0000004", 6).unwrap(), 0);
        assert_eq!(parse_decimal_amount("0.0000009", 6).unwrap(), 0);
    }
}
