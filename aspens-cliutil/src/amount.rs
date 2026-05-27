//! Token-amount resolution shared by aspens-cli and aspens-repl.

use aspens::commands::config::config_pb::GetConfigResponse;
use aspens::decimals::parse_decimal_amount_u64;
use eyre::Result;

/// Look up `token_symbol` on `network` in the server config and parse
/// `amount` (a human-readable decimal string like `"1.5"`) against the
/// token's `decimals`. Returns the value in base units.
///
/// Used by `deposit` / `withdraw` flows in both `aspens-cli` and
/// `aspens-repl`. The error hint mentions the generic `config`
/// command, which is the subcommand name in both binaries.
pub fn resolve_token_amount(
    config: &GetConfigResponse,
    network: &str,
    token_symbol: &str,
    amount: &str,
) -> Result<u64> {
    let token = config.get_token(network, token_symbol).ok_or_else(|| {
        eyre::eyre!(
            "Token '{}' not found on chain '{}'. \
             Run `config` to see available tokens.",
            token_symbol,
            network
        )
    })?;
    parse_decimal_amount_u64(amount, token.decimals)
        .map_err(|e| eyre::eyre!("Invalid amount '{}' for {}: {}", amount, token_symbol, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aspens::commands::config::config_pb::{Chain, Configuration, GetConfigResponse, Token};
    use std::collections::HashMap;

    fn config_with_token(network: &str, symbol: &str, decimals: u32) -> GetConfigResponse {
        let mut tokens = HashMap::new();
        tokens.insert(
            symbol.to_string(),
            Token {
                symbol: symbol.to_string(),
                decimals,
                ..Default::default()
            },
        );
        let chain = Chain {
            network: network.to_string(),
            tokens,
            ..Default::default()
        };
        GetConfigResponse {
            config: Some(Configuration {
                chains: vec![chain],
                ..Default::default()
            }),
        }
    }

    #[test]
    fn resolves_with_correct_decimals() {
        let cfg = config_with_token("base-sepolia", "USDC", 6);
        let got = resolve_token_amount(&cfg, "base-sepolia", "USDC", "10").unwrap();
        assert_eq!(got, 10_000_000);
    }

    #[test]
    fn human_readable_fractions_scale_to_base_units() {
        let cfg = config_with_token("base-sepolia", "USDC", 6);
        let got = resolve_token_amount(&cfg, "base-sepolia", "USDC", "0.5").unwrap();
        assert_eq!(got, 500_000);
    }

    #[test]
    fn unknown_token_returns_actionable_error() {
        let cfg = config_with_token("base-sepolia", "USDC", 6);
        let err = resolve_token_amount(&cfg, "base-sepolia", "USDT", "1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("USDT"), "error names the missing token: {err}");
        assert!(
            err.contains("config"),
            "error points the user at the config command: {err}"
        );
    }

    #[test]
    fn invalid_amount_returns_actionable_error() {
        let cfg = config_with_token("base-sepolia", "USDC", 6);
        let err = resolve_token_amount(&cfg, "base-sepolia", "USDC", "not-a-number")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Invalid amount"), "error: {err}");
        assert!(err.contains("USDC"), "error names the token: {err}");
    }
}
