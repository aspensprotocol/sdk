//! Curve-agnostic wallet abstraction for signing.
//!
//! Wraps EVM (secp256k1, via Alloy) and Solana (Ed25519, via solana-sdk)
//! keys behind a single interface so call sites don't need to branch on
//! curve type. Solana support is gated behind the `solana` feature.

use alloy::primitives::B256;
use alloy::signers::{local::PrivateKeySigner, Signer};
use eyre::{eyre, Result};

#[cfg(feature = "solana")]
use solana_sdk::signature::{Keypair, Signer as SolanaSigner};

/// Cryptographic curve used by a wallet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveType {
    /// secp256k1 ECDSA — EVM-compatible chains
    Secp256k1,
    /// Ed25519 EdDSA — Solana-compatible chains. Constructing/using a wallet
    /// of this curve requires the `solana` feature.
    Ed25519,
}

/// A wallet that can sign messages on EVM (always) or Solana (with `solana`
/// feature) chains.
pub enum Wallet {
    Evm(PrivateKeySigner),
    #[cfg(feature = "solana")]
    Solana(Box<Keypair>),
}

impl Wallet {
    /// Load an EVM wallet from a hex private key (with or without `0x` prefix).
    pub fn from_evm_hex(hex_key: &str) -> Result<Self> {
        let signer: PrivateKeySigner = hex_key
            .parse()
            .map_err(|e| eyre!("invalid EVM private key: {}", e))?;
        Ok(Wallet::Evm(signer))
    }

    /// Load a Solana wallet from a base58-encoded keypair string
    /// (the standard `solana-keygen` output format).
    #[cfg(feature = "solana")]
    pub fn from_solana_base58(b58: &str) -> Result<Self> {
        let bytes = bs58::decode(b58.trim())
            .into_vec()
            .map_err(|e| eyre!("invalid base58 keypair: {}", e))?;
        if bytes.len() != 64 {
            return Err(eyre!(
                "Solana keypair must be 64 bytes, got {}",
                bytes.len()
            ));
        }
        let keypair = Keypair::try_from(bytes.as_slice())
            .map_err(|e| eyre!("invalid Solana keypair bytes: {}", e))?;
        Ok(Wallet::Solana(Box::new(keypair)))
    }

    /// Load a Solana wallet from a JSON byte array (alternate `solana-keygen` format,
    /// e.g. `[12,34,56,...]` — 64 bytes).
    #[cfg(feature = "solana")]
    pub fn from_solana_json(json: &str) -> Result<Self> {
        let bytes: Vec<u8> =
            serde_json::from_str(json).map_err(|e| eyre!("invalid Solana keypair JSON: {}", e))?;
        if bytes.len() != 64 {
            return Err(eyre!(
                "Solana keypair must be 64 bytes, got {}",
                bytes.len()
            ));
        }
        let keypair = Keypair::try_from(bytes.as_slice())
            .map_err(|e| eyre!("invalid Solana keypair bytes: {}", e))?;
        Ok(Wallet::Solana(Box::new(keypair)))
    }

    /// Return the wallet's curve type.
    pub fn curve(&self) -> CurveType {
        match self {
            Wallet::Evm(_) => CurveType::Secp256k1,
            #[cfg(feature = "solana")]
            Wallet::Solana(_) => CurveType::Ed25519,
        }
    }

    /// Return the wallet's address as a string.
    /// - EVM: checksummed `0x`-prefixed hex
    /// - Solana: base58-encoded 32-byte public key
    pub fn address(&self) -> String {
        match self {
            Wallet::Evm(s) => s.address().to_checksum(None),
            #[cfg(feature = "solana")]
            Wallet::Solana(kp) => kp.pubkey().to_string(),
        }
    }

    /// Sign an arbitrary message and return the raw signature bytes.
    /// - EVM: 65-byte ECDSA signature (r || s || v)
    /// - Solana: 64-byte Ed25519 signature
    pub async fn sign_message(&self, msg: &[u8]) -> Result<Vec<u8>> {
        match self {
            Wallet::Evm(s) => {
                let sig = s.sign_message(msg).await?;
                Ok(sig.as_bytes().to_vec())
            }
            #[cfg(feature = "solana")]
            Wallet::Solana(kp) => {
                let sig = kp.sign_message(msg);
                Ok(sig.as_ref().to_vec())
            }
        }
    }

    /// Sign a 32-byte EIP-712 digest. Only valid for EVM wallets.
    pub async fn sign_eip712_digest(&self, digest: B256) -> Result<Vec<u8>> {
        match self {
            Wallet::Evm(s) => {
                let sig = s.sign_hash(&digest).await?;
                Ok(sig.as_bytes().to_vec())
            }
            #[cfg(feature = "solana")]
            Wallet::Solana(_) => Err(eyre!(
                "EIP-712 digest signing is not supported for Ed25519 wallets"
            )),
        }
    }

    /// Borrow as an EVM signer, if this is an EVM wallet.
    pub fn as_evm(&self) -> Option<&PrivateKeySigner> {
        match self {
            Wallet::Evm(s) => Some(s),
            #[cfg(feature = "solana")]
            Wallet::Solana(_) => None,
        }
    }

    /// Borrow as a Solana keypair, if this is a Solana wallet.
    #[cfg(feature = "solana")]
    pub fn as_solana(&self) -> Option<&Keypair> {
        match self {
            Wallet::Evm(_) => None,
            Wallet::Solana(kp) => Some(kp),
        }
    }
}

/// Load a trader wallet from environment variables based on the requested curve.
///
/// - `Secp256k1`: reads `TRADER_PRIVKEY` (hex)
/// - `Ed25519`: reads `TRADER_PRIVKEY_SOLANA` (base58 keypair) — requires the
///   `solana` feature
pub fn load_trader_wallet(curve: CurveType) -> Result<Wallet> {
    match curve {
        CurveType::Secp256k1 => {
            let key = std::env::var("TRADER_PRIVKEY")
                .map_err(|_| eyre!("TRADER_PRIVKEY not set in environment"))?;
            Wallet::from_evm_hex(&key)
        }
        CurveType::Ed25519 => {
            #[cfg(feature = "solana")]
            {
                let key = std::env::var("TRADER_PRIVKEY_SOLANA")
                    .map_err(|_| eyre!("TRADER_PRIVKEY_SOLANA not set in environment"))?;
                Wallet::from_solana_base58(&key).or_else(|_| Wallet::from_solana_json(&key))
            }
            #[cfg(not(feature = "solana"))]
            {
                Err(eyre!(
                    "Ed25519/Solana wallets require the `solana` feature to be enabled"
                ))
            }
        }
    }
}

/// Load an admin wallet from environment variables based on the requested curve.
///
/// - `Secp256k1`: reads `ADMIN_PRIVKEY` (hex)
/// - `Ed25519`: reads `ADMIN_PRIVKEY_SOLANA` (base58 keypair) — requires the
///   `solana` feature
pub fn load_admin_wallet(curve: CurveType) -> Result<Wallet> {
    match curve {
        CurveType::Secp256k1 => {
            let key = std::env::var("ADMIN_PRIVKEY")
                .map_err(|_| eyre!("ADMIN_PRIVKEY not set in environment"))?;
            Wallet::from_evm_hex(&key)
        }
        CurveType::Ed25519 => {
            #[cfg(feature = "solana")]
            {
                let key = std::env::var("ADMIN_PRIVKEY_SOLANA")
                    .map_err(|_| eyre!("ADMIN_PRIVKEY_SOLANA not set in environment"))?;
                Wallet::from_solana_base58(&key).or_else(|_| Wallet::from_solana_json(&key))
            }
            #[cfg(not(feature = "solana"))]
            {
                Err(eyre!(
                    "Ed25519/Solana wallets require the `solana` feature to be enabled"
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Anvil test key #0
    const TEST_EVM_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    #[cfg(feature = "solana")]
    fn fresh_solana_keypair_b58() -> String {
        let kp = Keypair::new();
        bs58::encode(kp.to_bytes()).into_string()
    }

    #[test]
    fn evm_wallet_address_is_hex() {
        let w = Wallet::from_evm_hex(TEST_EVM_KEY).unwrap();
        let addr = w.address();
        assert!(addr.starts_with("0x"));
        assert_eq!(addr.len(), 42);
        assert_eq!(w.curve(), CurveType::Secp256k1);
    }

    #[cfg(feature = "solana")]
    #[test]
    fn solana_wallet_address_is_base58() {
        let b58 = fresh_solana_keypair_b58();
        let w = Wallet::from_solana_base58(&b58).unwrap();
        let addr = w.address();
        assert!(!addr.is_empty());
        assert!(!addr.starts_with("0x"));
        assert!(addr.len() >= 32 && addr.len() <= 44);
        for c in addr.chars() {
            assert!(c.is_ascii_alphanumeric());
            assert!(c != '0' && c != 'O' && c != 'I' && c != 'l');
        }
        assert_eq!(w.curve(), CurveType::Ed25519);
    }

    #[tokio::test]
    async fn evm_sign_message_is_65_bytes() {
        let w = Wallet::from_evm_hex(TEST_EVM_KEY).unwrap();
        let sig = w.sign_message(b"hello").await.unwrap();
        assert_eq!(sig.len(), 65, "EVM signature should be 65 bytes");
    }

    #[cfg(feature = "solana")]
    #[tokio::test]
    async fn solana_sign_message_is_64_bytes() {
        let w = Wallet::from_solana_base58(&fresh_solana_keypair_b58()).unwrap();
        let sig = w.sign_message(b"hello").await.unwrap();
        assert_eq!(sig.len(), 64, "Ed25519 signature should be 64 bytes");
    }

    #[cfg(feature = "solana")]
    #[tokio::test]
    async fn solana_eip712_returns_error() {
        let w = Wallet::from_solana_base58(&fresh_solana_keypair_b58()).unwrap();
        let digest = B256::ZERO;
        assert!(w.sign_eip712_digest(digest).await.is_err());
    }

    #[cfg(feature = "solana")]
    #[test]
    fn solana_wallet_rejects_short_key() {
        let short = bs58::encode(vec![0u8; 32]).into_string();
        assert!(Wallet::from_solana_base58(&short).is_err());
    }

    #[test]
    fn evm_wallet_rejects_invalid_hex() {
        assert!(Wallet::from_evm_hex("not-hex").is_err());
    }

    #[cfg(not(feature = "solana"))]
    #[test]
    fn ed25519_load_errors_without_feature() {
        assert!(load_trader_wallet(CurveType::Ed25519).is_err());
    }
}
