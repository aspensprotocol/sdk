//! Shared user-facing error formatter for the Aspens CLI binaries.
//!
//! Inspects an `eyre::Report` and matches the lowercased message text
//! against a battery of known failure modes (transport, auth, config
//! lookups, gas, balance, signing). Each branch returns a multi-line
//! message that explains what went wrong and what the user can try
//! next, with the original error appended as the "Underlying error"
//! footer. If no branch matches, a generic fallback is returned.
//!
//! The branches and their order are the union of what `aspens-cli`,
//! `aspens-repl`, and `aspens-admin` historically each carried. The
//! cli/repl branches dominate for trading-path errors; the admin
//! branches dominate for JWT / auth / config-mutation errors. Most
//! branches are inert for binaries they don't apply to (e.g. cli never
//! sees `"invalid token"`), so leaving them in the shared helper is
//! cheap and avoids drift.

use crate::BinaryContext;

/// Analyze an error and return a user-friendly message with hints.
///
/// `context` is a short verb-phrase describing the operation that
/// failed (e.g. `"send buy order"`, `"fetch balance"`). It's
/// interpolated into the first line as `"Failed to {context}: ..."`.
pub fn format_error(err: &eyre::Report, context: &str, ctx: &BinaryContext) -> String {
    let err_string = err.to_string().to_lowercase();
    let root_cause = err.root_cause().to_string().to_lowercase();
    let name = ctx.name;

    let with_underlying = |msg: String| -> String { format!("{msg}\n\nUnderlying error: {err}") };

    // -- Transport / network ---------------------------------------------

    if err_string.contains("failed to connect")
        || err_string.contains("connection refused")
        || root_cause.contains("connection refused")
    {
        return with_underlying(format!(
            "Failed to {context}: Could not connect to the server\n\n\
             Possible causes:\n\
             - The Aspens server is not running\n\
             - The server URL is incorrect\n\
             - A firewall is blocking the connection\n\n\
             Hints:\n\
             - Check that the server is running\n\
             - Verify the stack URL with '{name} status'\n\
             - Check ASPENS_MARKET_STACK_URL in your .env file"
        ));
    }

    if err_string.contains("dns error")
        || err_string.contains("no such host")
        || err_string.contains("name or service not known")
        || root_cause.contains("dns")
    {
        return with_underlying(format!(
            "Failed to {context}: Could not resolve server hostname\n\n\
             Possible causes:\n\
             - The server hostname is incorrect\n\
             - DNS is not configured properly\n\
             - No internet connection\n\n\
             Hints:\n\
             - Verify the stack URL is correct\n\
             - Check your internet connection\n\
             - Try using an IP address instead of hostname"
        ));
    }

    if err_string.contains("tls")
        || err_string.contains("ssl")
        || err_string.contains("certificate")
        || root_cause.contains("certificate")
    {
        return with_underlying(format!(
            "Failed to {context}: TLS/SSL error\n\n\
             Possible causes:\n\
             - The server's SSL certificate is invalid or expired\n\
             - Certificate chain is incomplete\n\
             - Using HTTP URL for HTTPS server or vice versa\n\n\
             Hints:\n\
             - Verify you're using the correct protocol (http:// vs https://)\n\
             - For local development, use http://localhost:50051\n\
             - For remote servers, use https://"
        ));
    }

    if err_string.contains("compression flag")
        || err_string.contains("protocol error")
        || err_string.contains("invalid compression")
    {
        return with_underlying(format!(
            "Failed to {context}: Protocol mismatch\n\n\
             Possible causes:\n\
             - Using HTTP to connect to an HTTPS server\n\
             - Using HTTPS to connect to an HTTP server\n\
             - The server is not a gRPC endpoint\n\n\
             Hints:\n\
             - For remote servers, use https://\n\
             - For local development, use http://\n\
             - Verify ASPENS_MARKET_STACK_URL in your .env file"
        ));
    }

    if err_string.contains("timeout") || err_string.contains("timed out") {
        return with_underlying(format!(
            "Failed to {context}: Request timed out\n\n\
             Possible causes:\n\
             - The server is overloaded or unresponsive\n\
             - Network latency is too high\n\
             - The operation is taking longer than expected\n\n\
             Hints:\n\
             - Try again in a few moments\n\
             - Check server status with '{name} status'\n\
             - Verify network connectivity"
        ));
    }

    // -- Auth / admin (only fires for admin flows) -----------------------

    if err_string.contains("unauthenticated")
        || err_string.contains("unauthorized")
        || err_string.contains("401")
        || err_string.contains("invalid token")
        || err_string.contains("token expired")
    {
        return with_underlying(format!(
            "Failed to {context}: Authentication failed\n\n\
             Possible causes:\n\
             - JWT token is missing, invalid, or expired\n\
             - You don't have admin privileges\n\n\
             Hints:\n\
             - Run '{name} login' to get a fresh JWT token\n\
             - Set ASPENS_JWT in your .env file or use --jwt flag\n\
             - Verify {privkey} is set correctly",
            privkey = ctx.privkey_env_var,
        ));
    }

    if err_string.contains("not authorized as an admin")
        || err_string.contains("address is not authorized")
    {
        return with_underlying(format!(
            "Failed to {context}: Address is not authorized as admin\n\n\
             The wallet address derived from {privkey} is not registered as an admin\n\
             on this Aspens server.\n\n\
             Possible causes:\n\
             - Using the wrong private key (not the admin wallet)\n\
             - The admin address was changed on the server\n\
             - This is a fresh server and admin hasn't been initialized\n\n\
             Hints:\n\
             - Run '{name} admin-public-key' to see your wallet address\n\
             - Compare with the registered admin address on the server\n\
             - If this is a new server, use '{name} init-admin --address <your-address>'\n\
             - Check that {privkey} in .env matches the expected admin wallet",
            privkey = ctx.privkey_env_var,
        ));
    }

    if err_string.contains("permission denied")
        || err_string.contains("forbidden")
        || err_string.contains("403")
    {
        return with_underlying(format!(
            "Failed to {context}: Permission denied\n\n\
             Possible causes:\n\
             - Your account doesn't have admin privileges\n\
             - The operation requires a different permission level\n\n\
             Hints:\n\
             - Verify you are using the correct admin wallet\n\
             - Contact the system administrator"
        ));
    }

    if err_string.contains("admin already") || err_string.contains("already initialized") {
        return with_underlying(format!(
            "Failed to {context}: Admin has already been initialized\n\n\
             Hints:\n\
             - Use '{name} login' to authenticate with the existing admin\n\
             - Use '{name} update-admin' to change the admin address (requires auth)"
        ));
    }

    // -- Config / lookup errors ------------------------------------------

    // Specific resource not-found branches first (chain/token/market),
    // then a generic 404 fallback for admin-side mutations.
    if err_string.contains("chain not found")
        || err_string.contains("network not found")
        || (err_string.contains("not found") && err_string.contains("chain"))
    {
        return with_underlying(format!(
            "Failed to {context}: Chain/network not found\n\n\
             Hints:\n\
             - Check available chains with '{name} config'\n\
             - Verify the network name is spelled correctly\n\
             - The chain may not be configured on this server"
        ));
    }

    if err_string.contains("token not found")
        || (err_string.contains("not found") && err_string.contains("token"))
    {
        return with_underlying(format!(
            "Failed to {context}: Token not found\n\n\
             Hints:\n\
             - Check available tokens with '{name} config'\n\
             - Verify the token symbol is spelled correctly (case-sensitive)\n\
             - The token may not be configured on this chain"
        ));
    }

    if err_string.contains("market not found")
        || (err_string.contains("not found") && err_string.contains("market"))
    {
        return with_underlying(format!(
            "Failed to {context}: Market not found\n\n\
             Hints:\n\
             - Check available markets with '{name} config'\n\
             - Verify the market ID is correct\n\
             - Markets are identified by their full ID (e.g., chain_id::token::chain_id::token)"
        ));
    }

    if err_string.contains("already exists") || err_string.contains("duplicate") {
        return with_underlying(format!(
            "Failed to {context}: Resource already exists\n\n\
             Hints:\n\
             - Use the appropriate delete command first if you want to replace it\n\
             - Check existing configuration with '{name} config'"
        ));
    }

    if err_string.contains("not found") || err_string.contains("404") {
        return with_underlying(format!(
            "Failed to {context}: Resource not found\n\n\
             Hints:\n\
             - Verify the resource name/ID is correct\n\
             - Check existing configuration with '{name} config'\n\
             - The resource may have been deleted"
        ));
    }

    // -- Trading / on-chain errors ---------------------------------------

    if err_string.contains("insufficient gas") || err_string.contains("insufficient funds for gas")
    {
        return with_underlying(format!(
            "Failed to {context}: Insufficient gas for transaction fees\n\n\
             Your wallet needs native tokens (ETH, FLR, etc.) to pay for gas.\n\n\
             Hints:\n\
             - Fund your wallet with native tokens on the target chain\n\
             - For testnets, use a faucet to get free test tokens:\n\
               - Base Sepolia: https://www.alchemy.com/faucets/base-sepolia\n\
               - Flare Coston2: https://faucet.flare.network"
        ));
    }

    if err_string.contains("insufficient")
        || err_string.contains("not enough")
        || err_string.contains("balance too low")
    {
        return with_underlying(format!(
            "Failed to {context}: Insufficient balance\n\n\
             Hints:\n\
             - Check your balances with '{name} balance'\n\
             - For trading: ensure you have deposited tokens first\n\
             - For deposits: ensure your wallet has enough tokens"
        ));
    }

    if err_string.contains("invalid string length") {
        return with_underlying(format!(
            "Failed to {context}: Invalid amount format\n\n\
             The server rejected the order due to an invalid amount format.\n\n\
             Possible causes:\n\
             - Amount or price is too small or has too few digits\n\
             - Values need to be in the correct decimal format\n\n\
             Hints:\n\
             - Use decimal notation for amounts (e.g., '1.5' instead of '1')\n\
             - Check '{name} config' to see the market's pairDecimals setting\n\
             - For market with pairDecimals=4: '1' becomes '10000', '0.5' becomes '5000'"
        ));
    }

    if err_string.contains("transaction")
        || err_string.contains("revert")
        || err_string.contains("execution reverted")
    {
        return with_underlying(format!(
            "Failed to {context}: Transaction failed\n\n\
             Possible causes:\n\
             - Insufficient token balance or allowance\n\
             - Contract execution reverted\n\
             - Gas estimation failed\n\n\
             Hints:\n\
             - Check your wallet balance\n\
             - Verify you have approved the contract to spend tokens\n\
             - Try with a smaller amount"
        ));
    }

    // -- Signing key / address format ------------------------------------

    if err_string.contains("invalid address") || err_string.contains("invalid checksum") {
        return with_underlying(format!(
            "Failed to {context}: Invalid Ethereum address format\n\n\
             Hints:\n\
             - Ensure the address starts with '0x'\n\
             - Verify the address is 42 characters long (including '0x')\n\
             - Use a checksummed address format"
        ));
    }

    if err_string.contains("invalid private key")
        || err_string.contains("privkey")
        || err_string.contains("secret key")
        || err_string.contains("hex decode")
    {
        return with_underlying(format!(
            "Failed to {context}: Invalid private key\n\n\
             Hints:\n\
             - Ensure {privkey} is set correctly in your .env file\n\
             - The private key should be a 64-character hex string\n\
             - Do not include the '0x' prefix",
            privkey = ctx.privkey_env_var,
        ));
    }

    // -- Generic fallback ------------------------------------------------

    format!(
        "Failed to {context}\n\n\
         Hints:\n\
         - Check server status with '{name} status'\n\
         - Verify your configuration in .env file\n\
         - Use -v flag for more detailed output\n\n\
         Underlying error: {err}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(msg: &'static str) -> eyre::Report {
        eyre::eyre!(msg)
    }

    #[test]
    fn connection_refused_emits_actionable_hint() {
        let e = report("transport error: Failed to connect: Connection refused");
        let out = format_error(&e, "fetch balance", &BinaryContext::TRADER_CLI);
        assert!(out.contains("Could not connect"));
        assert!(out.contains("'aspens-cli status'"));
        assert!(out.contains("Underlying error:"));
    }

    #[test]
    fn binary_name_interpolates_per_context() {
        let e = report("Failed to connect");
        let cli = format_error(&e, "ping", &BinaryContext::TRADER_CLI);
        let admin = format_error(&e, "ping", &BinaryContext::ADMIN);
        assert!(cli.contains("'aspens-cli status'"));
        assert!(admin.contains("'aspens-admin status'"));
        assert!(!cli.contains("aspens-admin"));
        assert!(!admin.contains("aspens-cli"));
    }

    #[test]
    fn admin_auth_branch_mentions_admin_privkey_env() {
        let e = report("rpc error: Unauthenticated: invalid token");
        let out = format_error(&e, "set chain", &BinaryContext::ADMIN);
        assert!(out.contains("Authentication failed"));
        assert!(
            out.contains("ADMIN_PRIVKEY"),
            "admin auth branch must surface ADMIN_PRIVKEY env var: {out}"
        );
        assert!(out.contains("'aspens-admin login'"));
    }

    #[test]
    fn trader_privkey_branch_mentions_trader_env() {
        let e = report("Invalid private key: hex decode failed");
        let out = format_error(&e, "deposit", &BinaryContext::TRADER_CLI);
        assert!(out.contains("Invalid private key"));
        assert!(
            out.contains("TRADER_PRIVKEY"),
            "trader binary surfaces TRADER_PRIVKEY: {out}"
        );
    }

    #[test]
    fn chain_not_found_picks_specific_branch_not_generic_404() {
        // The generic "not found" branch must not preempt the more
        // helpful chain-specific one. Branch ordering protects this.
        let e = report("Chain 'flare-mainnet' not found in configuration");
        let out = format_error(&e, "withdraw", &BinaryContext::TRADER_CLI);
        assert!(out.contains("Chain/network not found"));
        assert!(out.contains("Check available chains"));
    }

    #[test]
    fn timeout_branch_includes_binary_name_in_hint() {
        let e = report("operation timed out after 30s");
        let out = format_error(&e, "fetch config", &BinaryContext::TRADER_REPL);
        assert!(out.contains("Request timed out"));
        assert!(out.contains("'aspens-repl status'"));
    }

    #[test]
    fn fallback_includes_underlying_error_text() {
        let e = report("something exotic and unmatched");
        let out = format_error(&e, "do thing", &BinaryContext::TRADER_CLI);
        assert!(out.contains("Failed to do thing"));
        assert!(out.contains("something exotic and unmatched"));
        assert!(out.contains("Use -v flag"));
    }
}
