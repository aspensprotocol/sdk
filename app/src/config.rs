use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
pub struct TradeContract {
    #[serde(rename = "contract_id", skip_serializing_if = "Option::is_none")]
    pub contract_id: Option<String>,
    pub address: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Token {
    pub name: String,
    pub symbol: String,
    #[serde(rename = "token_id", skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
    pub address: String,
    pub decimals: u8,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Chain {
    pub architecture: String,
    #[serde(rename = "canonical_name")]
    pub canonical_name: String,
    pub network: String,
    #[serde(rename = "chain_id")]
    pub chain_id: u64,
    #[serde(rename = "contract_owner_address")]
    pub contract_owner_address: String,
    #[serde(rename = "explorer_url")]
    pub explorer_url: String,
    #[serde(rename = "rpc_url")]
    pub rpc_url: String,
    #[serde(rename = "service_address")]
    pub service_address: String,
    #[serde(rename = "trade_contract")]
    pub trade_contract: TradeContract,
    pub tokens: HashMap<String, Token>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Market {
    pub slug: String,
    pub name: String,
    #[serde(rename = "base_chain_network")]
    pub base_chain_network: String,
    #[serde(rename = "base_chain_token_symbol")]
    pub base_chain_token_symbol: String,
    #[serde(rename = "quote_chain_network")]
    pub quote_chain_network: String,
    #[serde(rename = "quote_chain_token_symbol")]
    pub quote_chain_token_symbol: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub chains: Vec<Chain>,
    pub markets: Vec<Market>,
}

impl Config {
    pub fn from_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path)?;

        // Determine file type based on extension
        let config = match path.extension().and_then(|ext| ext.to_str()) {
            Some("json") => serde_json::from_str(&contents)?,
            Some("toml") => toml::from_str(&contents)?,
            Some(ext) => anyhow::bail!("Unsupported file extension: {}", ext),
            None => anyhow::bail!("No file extension found"),
        };

        Ok(config)
    }

    pub fn get_chain(&self, network: &str) -> Option<&Chain> {
        self.chains.iter().find(|chain| chain.network == network)
    }

    pub fn get_token(&self, network: &str, symbol: &str) -> Option<&Token> {
        self.get_chain(network)
            .and_then(|chain| chain.tokens.get(symbol))
    }

    pub fn get_market(&self, slug: &str) -> Option<&Market> {
        self.markets.iter().find(|market| market.slug == slug)
    }

    pub fn get_market_by_tokens(
        &self,
        base_network: &str,
        base_symbol: &str,
        quote_network: &str,
        quote_symbol: &str,
    ) -> Option<&Market> {
        self.markets.iter().find(|market| {
            market.base_chain_network == base_network
                && market.base_chain_token_symbol == base_symbol
                && market.quote_chain_network == quote_network
                && market.quote_chain_token_symbol == quote_symbol
        })
    }

    pub fn get_chain_by_id(&self, chain_id: u64) -> Option<&Chain> {
        self.chains.iter().find(|chain| chain.chain_id == chain_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_config_parsing() {
        let config = Config::from_file("test/config.json").unwrap();
        verify_config(&config);
    }

    #[test]
    fn test_toml_config_parsing() {
        let config = Config::from_file("test/config.toml").unwrap();
        verify_config(&config);
    }

    fn verify_config(config: &Config) {
        // Test chain retrieval
        let anvil1 = config.get_chain("anvil-1").unwrap();
        assert_eq!(anvil1.chain_id, 84531);
        assert_eq!(anvil1.rpc_url, "http://localhost:8545");
        
        // Test token retrieval
        let usdc = config.get_token("anvil-1", "USDC").unwrap();
        assert_eq!(usdc.symbol, "USDC");
        assert_eq!(usdc.name, "USD Coin");
        assert_eq!(usdc.decimals, 6);
        
        // Test market retrieval
        let market = config.get_market("A1USDC-A2USDT").unwrap();
        assert_eq!(market.base_chain_network, "anvil-1");
        assert_eq!(market.base_chain_token_symbol, "USDC");
        assert_eq!(market.quote_chain_network, "anvil-2");
        assert_eq!(market.quote_chain_token_symbol, "USDT");
        
        // Test market lookup by tokens
        let market = config.get_market_by_tokens(
            "anvil-1",
            "USDC",
            "anvil-2",
            "USDT",
        );
        assert!(market.is_some());
        assert_eq!(market.unwrap().slug, "A1USDC-A2USDT");
    }
} 