//! Config loading, startup checks and limit resolution.
//!
//! The config file holds no secrets. Trader keys live in the env file it names,
//! which must exist and be mode 0600.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use aspens::commands::config::config_pb::GetConfigResponse;
use aspens::decimals::parse_decimal_amount;
use eyre::{Result, WrapErr, eyre};
use serde::Deserialize;

const DEFAULT_STATE_DIR: &str = "~/.aspens-mcp";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub stack_url: String,
    #[serde(default)]
    pub env_file: Option<PathBuf>,
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    #[serde(default)]
    pub allow_withdraw: bool,
    #[serde(default)]
    pub settle_address_allowlist: Vec<String>,
    #[serde(default)]
    pub deposits: DepositsConfig,
    #[serde(default)]
    pub venue: Option<VenuePin>,
    #[serde(default)]
    pub markets: BTreeMap<String, MarketLimitsText>,
}

fn default_state_dir() -> PathBuf {
    PathBuf::from(DEFAULT_STATE_DIR)
}

/// Per-market limits as written in the config: decimal strings in human units.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketLimitsText {
    pub max_order_qty: String,
    pub max_daily_qty: String,
    pub max_order_quote: String,
    pub max_daily_quote: String,
    pub max_deviation_bps: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DepositsConfig {
    pub enabled: bool,
    /// Keyed `"<network>:<token-address>"`.
    pub max_per_call: BTreeMap<String, String>,
}

impl Default for DepositsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_per_call: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VenuePin {
    #[serde(default)]
    pub expected_contracts: BTreeMap<String, String>,
    #[serde(default)]
    pub attestation_mrtd: Option<String>,
}

/// Resolved limits: every amount a `u128` in the market's pair-decimal units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketLimits {
    pub pair_decimals: u32,
    pub max_order_qty: u128,
    pub max_daily_qty: u128,
    pub max_order_quote: u128,
    pub max_daily_quote: u128,
    pub max_deviation_bps: u32,
}

/// Which trader curves have a key configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPresence {
    pub evm: bool,
    pub solana: bool,
}

/// Expand a leading `~/` (or a bare `~`) using `$HOME`.
pub fn expand_tilde(p: &Path) -> PathBuf {
    let Some(s) = p.to_str() else {
        return p.to_path_buf();
    };
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    if s == "~" {
        return home().unwrap_or_else(|| p.to_path_buf());
    }
    match (s.strip_prefix("~/"), home()) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => p.to_path_buf(),
    }
}

/// Read, parse and validate the config file, and check the env file it names.
pub fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("reading config {}", path.display()))?;
    let mut table: toml::Table = text
        .parse()
        .wrap_err_with(|| format!("parsing config {}", path.display()))?;

    // Markets are parsed one by one so an error names the exact TOML key.
    let markets_raw = table.remove("markets");
    let mut cfg: Config = table
        .try_into()
        .wrap_err_with(|| format!("parsing config {}", path.display()))?;
    if let Some(markets_raw) = markets_raw {
        let toml::Value::Table(markets_raw) = markets_raw else {
            return Err(eyre!(
                "markets must be a table of [markets.\"<id>\"] blocks"
            ));
        };
        for (id, value) in markets_raw {
            let limits = MarketLimitsText::deserialize(value).map_err(|e| {
                let msg = e.message();
                match msg
                    .strip_prefix("missing field `")
                    .and_then(|m| m.strip_suffix('`'))
                {
                    Some(field) => eyre!("markets.\"{id}\".{field} is required but missing"),
                    None => eyre!("markets.\"{id}\": {msg}"),
                }
            })?;
            cfg.markets.insert(id, limits);
        }
    }

    cfg.state_dir = expand_tilde(&cfg.state_dir);
    if let Some(env_file) = &cfg.env_file {
        let env_file = expand_tilde(env_file);
        check_env_file(&env_file)?;
        cfg.env_file = Some(env_file);
    }
    Ok(cfg)
}

fn check_env_file(path: &Path) -> Result<()> {
    let meta = std::fs::metadata(path)
        .map_err(|e| eyre!("env_file {} is set but cannot be read: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(eyre!(
                "env_file {} has mode {mode:04o}; it holds trader keys and must be 0600 (chmod 600 it)",
                path.display()
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    Ok(())
}

/// Parse a KEY=VALUE env file (comments and blank lines skipped, one pair
/// of surrounding quotes stripped). Local on purpose: `AspensClient` loads
/// env files by mutating the process environment.
fn read_env_file(path: &Path) -> Result<HashMap<String, String>> {
    let text = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("reading env_file {}", path.display()))?;
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let mut v = v.trim();
        for q in ['"', '\''] {
            if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
                v = &v[1..v.len() - 1];
                break;
            }
        }
        out.insert(k.trim().to_string(), v.to_string());
    }
    Ok(out)
}

/// A key is present when its variable exists and is non-empty after trimming.
/// At least one curve must have a key.
pub fn check_keys(env: &HashMap<String, String>) -> Result<KeyPresence> {
    let has = |k: &str| env.get(k).is_some_and(|v| !v.trim().is_empty());
    let presence = KeyPresence {
        evm: has("TRADER_PRIVKEY"),
        solana: has("TRADER_PRIVKEY_SOLANA"),
    };
    if !presence.evm && !presence.solana {
        return Err(eyre!(
            "no trader key: set TRADER_PRIVKEY and/or TRADER_PRIVKEY_SOLANA"
        ));
    }
    Ok(presence)
}

/// Check the keys the process would see: the inherited environment, with the
/// env file's values taking precedence.
pub fn startup_keys(cfg: &Config) -> Result<KeyPresence> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    if let Some(path) = &cfg.env_file {
        env.extend(read_env_file(path)?);
    }
    check_keys(&env)
}

/// Parse a human decimal amount, rejecting more fractional digits than
/// `decimals` (the SDK's `parse_decimal_amount` silently truncates them).
pub fn parse_amount(s: &str, decimals: u32) -> Result<u128> {
    if let Some((_, frac)) = s.trim().split_once('.')
        && frac.len() as u64 > u64::from(decimals)
    {
        return Err(eyre!("too many decimal places: {s} (max {decimals})"));
    }
    parse_decimal_amount(s, decimals)
}

/// Resolve configured limits against the venue's markets. Exact id match only.
pub fn resolve_markets(
    cfg: &Config,
    venue: &GetConfigResponse,
) -> Result<BTreeMap<String, MarketLimits>> {
    let mut out = BTreeMap::new();
    for (id, text) in &cfg.markets {
        let market = venue
            .get_market_by_id(id)
            .ok_or_else(|| eyre!("markets.\"{id}\": no such market in the venue config"))?;
        let pair_decimals = u32::try_from(market.pair_decimals)
            .map_err(|_| eyre!("markets.\"{id}\": venue pair_decimals is negative"))?;
        let amount = |field: &str, value: &str| {
            parse_amount(value, pair_decimals)
                .wrap_err_with(|| format!("markets.\"{id}\".{field} = {value:?}"))
        };
        out.insert(
            id.clone(),
            MarketLimits {
                pair_decimals,
                max_order_qty: amount("max_order_qty", &text.max_order_qty)?,
                max_daily_qty: amount("max_daily_qty", &text.max_daily_qty)?,
                max_order_quote: amount("max_order_quote", &text.max_order_quote)?,
                max_daily_quote: amount("max_daily_quote", &text.max_daily_quote)?,
                max_deviation_bps: text.max_deviation_bps,
            },
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aspens::commands::config::config_pb::{Configuration, Market};

    const MARKET: &str = "base::0xaaa::quote::0xbbb";

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    fn market_block(drop: Option<&str>, extra: &str) -> String {
        let lines = [
            ("max_order_qty", "max_order_qty = \"1.5\""),
            ("max_daily_qty", "max_daily_qty = \"7\""),
            ("max_order_quote", "max_order_quote = \"25\""),
            ("max_daily_quote", "max_daily_quote = \"250\""),
            ("max_deviation_bps", "max_deviation_bps = 100"),
        ];
        let mut s = format!("[markets.\"{MARKET}\"]\n");
        for (k, l) in lines {
            if Some(k) != drop {
                s.push_str(l);
                s.push('\n');
            }
        }
        s.push_str(extra);
        s
    }

    fn venue() -> GetConfigResponse {
        GetConfigResponse {
            config: Some(Configuration {
                markets: vec![Market {
                    market_id: MARKET.to_string(),
                    base_chain_token_decimals: 18,
                    quote_chain_token_decimals: 8,
                    pair_decimals: 6,
                    ..Default::default()
                }],
                ..Default::default()
            }),
        }
    }

    fn cfg_with(markets: &str) -> Config {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            dir.path(),
            "c.toml",
            &format!("stack_url = \"https://x\"\n{markets}"),
        );
        load(&p).unwrap()
    }

    #[test]
    fn defaults_apply() {
        let c = cfg_with("");
        assert!(!c.allow_withdraw);
        assert!(c.deposits.enabled);
        assert!(c.env_file.is_none());
        assert!(c.state_dir.ends_with(".aspens-mcp"));
        assert!(!c.state_dir.starts_with("~"));
    }

    #[test]
    fn missing_env_file_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.env");
        let p = write(
            dir.path(),
            "c.toml",
            &format!(
                "stack_url = \"https://x\"\nenv_file = \"{}\"\n",
                missing.display()
            ),
        );
        let err = load(&p).unwrap_err().to_string();
        assert!(err.contains(&missing.display().to_string()), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn world_readable_env_file_refuses() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let env = write(dir.path(), "k.env", "TRADER_PRIVKEY=abc\n");
        std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o644)).unwrap();
        let p = write(
            dir.path(),
            "c.toml",
            &format!(
                "stack_url = \"https://x\"\nenv_file = \"{}\"\n",
                env.display()
            ),
        );
        let err = load(&p).unwrap_err().to_string();
        assert!(err.contains("0600"), "{err}");

        std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load(&p).is_ok());
    }

    #[test]
    fn missing_limit_field_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            "stack_url = \"https://x\"\n{}",
            market_block(Some("max_daily_quote"), "")
        );
        let p = write(dir.path(), "c.toml", &body);
        let err = load(&p).unwrap_err().to_string();
        assert!(
            err.contains(&format!("markets.\"{MARKET}\".max_daily_quote")),
            "{err}"
        );
    }

    #[test]
    fn unknown_key_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            "stack_url = \"https://x\"\n{}",
            market_block(None, "max_price = \"1\"\n")
        );
        let p = write(dir.path(), "c.toml", &body);
        let err = load(&p).unwrap_err().to_string();
        assert!(err.contains("max_price"), "{err}");

        let p = write(
            dir.path(),
            "d.toml",
            "stack_url = \"https://x\"\nbogus = 1\n",
        );
        assert!(load(&p).is_err());
    }

    #[test]
    fn resolve_scales_by_pair_decimals() {
        let c = cfg_with(&market_block(None, ""));
        let r = resolve_markets(&c, &venue()).unwrap();
        let l = &r[MARKET];
        assert_eq!(l.pair_decimals, 6);
        assert_eq!(l.max_order_qty, 1_500_000);
        assert_eq!(l.max_daily_qty, 7_000_000);
        assert_eq!(l.max_order_quote, 25_000_000);
        assert_eq!(l.max_daily_quote, 250_000_000);
        assert_eq!(l.max_deviation_bps, 100);
    }

    #[test]
    fn resolve_rejects_market_absent_from_venue() {
        let c = cfg_with(&market_block(None, ""));
        let mut v = venue();
        v.config.as_mut().unwrap().markets[0].market_id = MARKET.to_uppercase();
        let err = resolve_markets(&c, &v).unwrap_err().to_string();
        assert!(err.contains(MARKET), "{err}");
    }

    #[test]
    fn resolve_names_key_on_unparseable_amount() {
        let mut c = cfg_with(&market_block(None, ""));
        c.markets.get_mut(MARKET).unwrap().max_daily_qty = "ten".into();
        let err = format!("{:#}", resolve_markets(&c, &venue()).unwrap_err());
        assert!(
            err.contains(&format!("markets.\"{MARKET}\".max_daily_qty")),
            "{err}"
        );
    }

    #[test]
    fn parse_amount_rejects_excess_precision() {
        let err = parse_amount("1.1234567", 6).unwrap_err().to_string();
        assert!(err.contains("max 6"), "{err}");
    }

    #[test]
    fn parse_amount_accepts_exact_precision() {
        assert_eq!(parse_amount("1.123456", 6).unwrap(), 1_123_456);
        assert_eq!(parse_amount("1", 6).unwrap(), 1_000_000);
    }

    #[test]
    fn resolve_refuses_limit_with_excess_decimals() {
        let mut c = cfg_with(&market_block(None, ""));
        c.markets.get_mut(MARKET).unwrap().max_daily_quote = "1.1234567".into();
        let err = format!("{:#}", resolve_markets(&c, &venue()).unwrap_err());
        assert!(
            err.contains(&format!("markets.\"{MARKET}\".max_daily_quote")),
            "{err}"
        );
        assert!(err.contains("max 6"), "{err}");
    }

    #[test]
    fn startup_names_missing_trader_key() {
        let dir = tempfile::tempdir().unwrap();
        let env = write(
            dir.path(),
            "k.env",
            "# nothing\nRPC_URL=http://x\nTRADER_PRIVKEY=  \n",
        );
        let parsed = read_env_file(&env).unwrap();
        let err = check_keys(&parsed).unwrap_err().to_string();
        assert_eq!(
            err,
            "no trader key: set TRADER_PRIVKEY and/or TRADER_PRIVKEY_SOLANA"
        );
    }

    #[test]
    fn check_keys_reports_each_curve() {
        let mut env = HashMap::new();
        env.insert("TRADER_PRIVKEY_SOLANA".to_string(), " k ".to_string());
        assert_eq!(
            check_keys(&env).unwrap(),
            KeyPresence {
                evm: false,
                solana: true
            }
        );
        env.insert("TRADER_PRIVKEY".to_string(), "\"abc\"".to_string());
        assert_eq!(
            check_keys(&env).unwrap(),
            KeyPresence {
                evm: true,
                solana: true
            }
        );
    }

    #[test]
    fn tilde_expands_with_home_only_as_prefix() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            expand_tilde(Path::new("~/x/y")),
            PathBuf::from(home).join("x/y")
        );
        assert_eq!(expand_tilde(Path::new("/a/~/b")), PathBuf::from("/a/~/b"));
    }
}
