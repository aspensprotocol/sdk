//! Authentication module for admin operations
//!
//! This module provides EIP-712 signature-based authentication to obtain
//! JWT tokens for admin operations on the Aspens platform.

pub mod auth_pb {
    include!("../../../proto/generated/xyz.aspens.arborter_auth.v1.rs");
}

use alloy::primitives::{keccak256, Address, B256, U256};
use alloy::signers::{local::PrivateKeySigner, Signer};
use auth_pb::auth_service_client::AuthServiceClient;
use auth_pb::{AuthRequest, AuthResponse, InitializeAdminRequest, InitializeAdminResponse};
use eyre::Result;
use std::time::{SystemTime, UNIX_EPOCH};

/// EIP-712 domain separator for Arborter authentication
const EIP712_DOMAIN_NAME: &str = "Arborter";
const EIP712_DOMAIN_VERSION: &str = "1";

/// Authentication response containing JWT token and metadata
#[derive(Debug, Clone)]
pub struct AuthToken {
    /// JWT token for authenticated requests
    pub jwt_token: String,
    /// Unix timestamp when the token expires (in seconds)
    pub expires_at: u64,
    /// The address that was authenticated
    pub address: String,
}

impl From<AuthResponse> for AuthToken {
    fn from(response: AuthResponse) -> Self {
        Self {
            jwt_token: response.jwt_token,
            expires_at: response.expires_at,
            address: response.address,
        }
    }
}

impl From<InitializeAdminResponse> for AuthToken {
    fn from(response: InitializeAdminResponse) -> Self {
        Self {
            jwt_token: response.jwt_token,
            expires_at: response.expires_at,
            address: response.address,
        }
    }
}

/// Initialize the first admin on a fresh Aspens stack
///
/// This can only be called once when no admin exists. It sets up
/// the initial admin and returns a JWT token for that admin.
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `address` - The Ethereum address to set as admin
pub async fn initialize_admin(url: String, address: String) -> Result<AuthToken> {
    let channel = tonic::transport::Channel::from_shared(url)?
        .connect()
        .await?;

    let mut client = AuthServiceClient::new(channel);

    let request = tonic::Request::new(InitializeAdminRequest { address });

    let response = client.initialize_admin(request).await?;

    Ok(response.into_inner().into())
}

/// Authenticate with EIP-712 signature to obtain a JWT token
///
/// This function creates an EIP-712 typed data structure, signs it with
/// the provided private key, and calls the authentication endpoint.
///
/// # Arguments
/// * `url` - The Aspens stack gRPC URL
/// * `private_key` - The private key (hex string, with or without 0x prefix)
/// * `chain_id` - The chain ID for EIP-712 domain (optional, defaults to 1)
pub async fn authenticate_with_signature(
    url: String,
    private_key: String,
    chain_id: Option<u64>,
) -> Result<AuthToken> {
    let signer: PrivateKeySigner = private_key.parse()?;
    let address = signer.address();

    // Generate timestamp and nonce
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let nonce = generate_nonce();

    // Create and sign the EIP-712 message
    let signature = sign_auth_message(&signer, address, timestamp, &nonce, chain_id).await?;

    // Connect to gRPC service
    let channel = tonic::transport::Channel::from_shared(url)?
        .connect()
        .await?;

    let mut client = AuthServiceClient::new(channel);

    let request = tonic::Request::new(AuthRequest {
        address: address.to_checksum(None),
        timestamp,
        nonce,
        signature,
    });

    let response = client.authenticate_with_signature(request).await?;

    Ok(response.into_inner().into())
}

/// Generate a random nonce for authentication
fn generate_nonce() -> String {
    use std::time::Instant;
    // Use a combination of timestamp and random-ish data
    let instant = Instant::now();
    let nanos = instant.elapsed().as_nanos();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}{:x}", timestamp, nanos)
}

/// Sign an authentication message using EIP-712 typed data
///
/// The typed data structure matches the server's expected format:
/// - Domain: { name: "Aspens", version: "1", chainId: <chain_id> }
/// - Message: { address, timestamp, nonce }
async fn sign_auth_message(
    signer: &PrivateKeySigner,
    address: Address,
    timestamp: u64,
    nonce: &str,
    chain_id: Option<u64>,
) -> Result<String> {
    let chain_id = chain_id.unwrap_or(1);

    // Compute domain separator
    let domain_separator = compute_domain_separator(chain_id);

    // Compute struct hash for the authentication message
    let struct_hash = compute_auth_struct_hash(address, timestamp, nonce);

    // Compute final EIP-712 hash: keccak256("\x19\x01" || domainSeparator || structHash)
    let mut digest_input = Vec::with_capacity(66);
    digest_input.extend_from_slice(&[0x19, 0x01]);
    digest_input.extend_from_slice(domain_separator.as_slice());
    digest_input.extend_from_slice(struct_hash.as_slice());

    let digest = keccak256(&digest_input);

    // Sign the digest
    let signature = signer.sign_hash(&digest).await?;

    // Return as hex string with 0x prefix
    Ok(format!("0x{}", hex::encode(signature.as_bytes())))
}

/// Compute EIP-712 domain separator
///
/// domainSeparator = keccak256(
///     keccak256("EIP712Domain(string name,string version,uint256 chainId)") ||
///     keccak256(name) ||
///     keccak256(version) ||
///     chainId
/// )
fn compute_domain_separator(chain_id: u64) -> B256 {
    let type_hash = keccak256(b"EIP712Domain(string name,string version,uint256 chainId)");
    let name_hash = keccak256(EIP712_DOMAIN_NAME.as_bytes());
    let version_hash = keccak256(EIP712_DOMAIN_VERSION.as_bytes());

    let mut encoded = Vec::with_capacity(128);
    encoded.extend_from_slice(type_hash.as_slice());
    encoded.extend_from_slice(name_hash.as_slice());
    encoded.extend_from_slice(version_hash.as_slice());
    encoded.extend_from_slice(&U256::from(chain_id).to_be_bytes::<32>());

    keccak256(&encoded)
}

/// Compute struct hash for authentication message
///
/// structHash = keccak256(
///     keccak256("AuthRequest(address address,uint64 timestamp,string nonce)") ||
///     address ||
///     timestamp ||
///     keccak256(nonce)
/// )
fn compute_auth_struct_hash(address: Address, timestamp: u64, nonce: &str) -> B256 {
    let type_hash = keccak256(b"AuthRequest(address address,uint64 timestamp,string nonce)");
    let nonce_hash = keccak256(nonce.as_bytes());

    let mut encoded = Vec::with_capacity(128);
    encoded.extend_from_slice(type_hash.as_slice());
    // Address is left-padded to 32 bytes
    encoded.extend_from_slice(&[0u8; 12]);
    encoded.extend_from_slice(address.as_slice());
    // timestamp is uint64, but still encoded as 32 bytes (left-padded)
    encoded.extend_from_slice(&[0u8; 24]);
    encoded.extend_from_slice(&timestamp.to_be_bytes());
    encoded.extend_from_slice(nonce_hash.as_slice());

    keccak256(&encoded)
}

/// Check if a JWT token is still valid based on its expiry time
pub fn is_token_valid(expires_at: u64) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Add a 30 second buffer for clock skew
    expires_at > now + 30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nonce_generation() {
        let nonce1 = generate_nonce();
        let nonce2 = generate_nonce();
        // Nonces should be non-empty
        assert!(!nonce1.is_empty());
        assert!(!nonce2.is_empty());
    }

    #[test]
    fn test_domain_separator() {
        // Just verify it computes without panicking
        let separator = compute_domain_separator(1);
        assert!(!separator.is_zero());
    }

    #[test]
    fn test_token_validity() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Token expiring in 1 hour should be valid
        assert!(is_token_valid(now + 3600));

        // Token expired 1 minute ago should be invalid
        assert!(!is_token_valid(now - 60));

        // Token expiring in 10 seconds should be invalid (30 second buffer)
        assert!(!is_token_valid(now + 10));
    }
}
