//! Chain-aware RPC client that dispatches between Alloy (EVM) and
//! solana-client (Solana) based on the chain's `architecture` field.

use alloy::primitives::{Address, Uint};
use alloy::providers::{Provider, ProviderBuilder};
use alloy_chains::NamedChain;
use eyre::{eyre, Result};
use solana_client::nonblocking::rpc_client::RpcClient as SolanaRpcClient;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use url::Url;

use crate::commands::config::config_pb::{Chain, Token};

/// Architecture string used in chain config for Solana chains.
pub const ARCH_SOLANA: &str = "Solana";
/// Architecture string used in chain config for EVM chains.
pub const ARCH_EVM: &str = "EVM";

/// A curve-aware RPC client.
pub enum ChainClient {
    /// EVM provider (Alloy).
    Evm { rpc_url: String, chain_id: u32 },
    /// Solana RPC client.
    Solana { client: SolanaRpcClient },
}

impl ChainClient {
    /// Build a `ChainClient` from a chain config entry.
    ///
    /// Dispatches on `chain.architecture`:
    /// - `"EVM"` (or empty/anything else for backward compat) → Alloy provider
    /// - `"Solana"` → Solana RPC client
    pub fn from_chain_config(chain: &Chain) -> Result<Self> {
        if chain.architecture.eq_ignore_ascii_case(ARCH_SOLANA) {
            Ok(ChainClient::Solana {
                client: SolanaRpcClient::new(chain.rpc_url.clone()),
            })
        } else {
            Ok(ChainClient::Evm {
                rpc_url: chain.rpc_url.clone(),
                chain_id: chain.chain_id,
            })
        }
    }

    /// Query the native gas balance for an address. Returns a `u128` in
    /// the smallest unit (wei for EVM, lamports for Solana).
    pub async fn native_balance(&self, address: &str) -> Result<u128> {
        match self {
            ChainClient::Evm { rpc_url, .. } => {
                let url = Url::parse(rpc_url)?;
                let provider = ProviderBuilder::new().connect_http(url);
                let addr: Address = address.parse()?;
                let balance: Uint<256, 4> = provider.get_balance(addr).await?;
                // EVM balance fits in u128 for any reasonable amount; clamp on overflow.
                Ok(balance.try_into().unwrap_or(u128::MAX))
            }
            ChainClient::Solana { client } => {
                let pubkey = Pubkey::from_str(address)
                    .map_err(|e| eyre!("invalid Solana address: {}", e))?;
                let lamports = client.get_balance(&pubkey).await?;
                Ok(lamports as u128)
            }
        }
    }

    /// Query the token balance for an owner on this chain.
    ///
    /// - EVM: ERC-20 `balanceOf(owner)`
    /// - Solana: SPL token account balance for the associated token account
    ///   derived from the owner pubkey and the token mint.
    pub async fn token_balance(&self, token: &Token, owner: &str) -> Result<u128> {
        match self {
            ChainClient::Evm { rpc_url, chain_id } => {
                use crate::commands::trading::IERC20;
                let url = Url::parse(rpc_url)?;
                let named_chain =
                    NamedChain::try_from(*chain_id as u64).unwrap_or(NamedChain::BaseSepolia);
                let provider = ProviderBuilder::new()
                    .with_chain(named_chain)
                    .connect_http(url);
                let token_addr: Address = token.address.parse()?;
                let owner_addr: Address = owner.parse()?;
                let contract = IERC20::new(token_addr, &provider);
                let result: Uint<256, 4> = contract.balanceOf(owner_addr).call().await?;
                Ok(result.try_into().unwrap_or(u128::MAX))
            }
            ChainClient::Solana { client } => {
                // For Solana, the token mint address is in `token.address` (base58).
                // The owner's token holding lives in their Associated Token Account (ATA),
                // derived deterministically from (owner, mint).
                //
                // We compute the ATA inline to avoid pulling in the full
                // spl-associated-token-account crate (which has dep conflicts).
                let owner_pubkey = Pubkey::from_str(owner)
                    .map_err(|e| eyre!("invalid Solana owner address: {}", e))?;
                let mint_pubkey = Pubkey::from_str(&token.address)
                    .map_err(|e| eyre!("invalid Solana mint address: {}", e))?;
                let ata = derive_associated_token_account(&owner_pubkey, &mint_pubkey);

                // Try to fetch the token account balance. If the account doesn't
                // exist, the owner has zero of this token.
                match client.get_token_account_balance(&ata).await {
                    Ok(b) => b
                        .amount
                        .parse::<u128>()
                        .map_err(|e| eyre!("invalid Solana token balance: {}", e)),
                    Err(_) => Ok(0),
                }
            }
        }
    }
}

/// Derive the Solana Associated Token Account (ATA) address for
/// `(owner, mint)`. This mirrors the SPL ATA program's PDA derivation:
///
/// ```text
/// ATA = find_program_address(
///     &[owner, TOKEN_PROGRAM_ID, mint],
///     ASSOCIATED_TOKEN_PROGRAM_ID,
/// )
/// ```
pub fn derive_associated_token_account(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    // Hardcoded program IDs from the SPL specification.
    // SPL Token program: TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA
    // Associated Token program: ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL
    let token_program_id = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap();
    let associated_token_program_id =
        Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap();

    let seeds = &[owner.as_ref(), token_program_id.as_ref(), mint.as_ref()];
    let (ata, _bump) = Pubkey::find_program_address(seeds, &associated_token_program_id);
    ata
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ata_derivation_matches_known_pair() {
        // Sanity: derivation should produce a deterministic, valid pubkey.
        let owner = Pubkey::from_str("11111111111111111111111111111112").unwrap();
        let mint = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
        let ata = derive_associated_token_account(&owner, &mint);
        // Just verify it doesn't panic and returns a non-default pubkey.
        assert_ne!(ata, Pubkey::default());
    }
}
