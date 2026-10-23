use crate::chain_client::named_chain_for;
use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U160, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use eyre::Result;
use url::Url;

use crate::commands::config::config_pb::GetConfigResponse;
use crate::evm::rpc::{IERC20, MidribV3};
use crate::wallet::{Architecture, CurveType, Wallet};

/// Minimum gas balance required for transactions (0.0001 ETH = 100000 gwei)
const MIN_GAS_BALANCE: u128 = 100_000_000_000_000; // 0.0001 ETH in wei

/// Deposit tokens using a curve-agnostic wallet.
///
/// Branches on `chain.architecture` (closed; unknown is an error):
/// - **EVM**: MidribV3 `approve` + `deposit`, or `depositNative` with value
/// - **Solana**: the Midrib program's `deposit` instruction (wrapping SOL
///   first for native SOL)
/// - **Tron**: the same MidribV3 calls as native Tron transactions — USDT
///   `approve` + `deposit`, TRX `depositNative` with `call_value` — with the
///   allowance read over the node's `/jsonrpc`
pub async fn call_deposit_from_config_with_wallet(
    network: String,
    token_symbol: String,
    amount: u128,
    wallet: &Wallet,
    config: GetConfigResponse,
) -> Result<()> {
    // Look up chain to determine the dispatch path
    let chain_for_arch = config
        .get_chain(&network)
        .ok_or_else(|| eyre::eyre!("Chain '{}' not found in configuration", network))?;

    match crate::wallet::chain_architecture(chain_for_arch)? {
        Architecture::Solana => {
            // Solana SPL token amounts are natively u64 — downcast (checked) at the
            // boundary, since `deposit_ix` takes a u64 (DEC-1: u128 upstream, u64 on Solana).
            let spl_amount: u64 = amount.try_into().map_err(|_| {
                eyre::eyre!(
                    "amount {amount} exceeds the SPL token u64 max on Solana chain '{network}'"
                )
            })?;
            return solana_deposit(chain_for_arch, &token_symbol, spl_amount, wallet).await;
        }
        Architecture::Tron => {
            return tron_deposit(chain_for_arch, &token_symbol, amount, wallet).await;
        }
        Architecture::Evm => {}
    }

    // EVM path requires an EVM wallet
    if wallet.curve() != CurveType::Secp256k1 {
        return Err(eyre::eyre!(
            "EVM chain '{}' requires a secp256k1 wallet, got {:?}",
            network,
            wallet.curve()
        ));
    }
    let signer = wallet
        .as_evm()
        .ok_or_else(|| eyre::eyre!("expected EVM wallet for chain '{}'", network))?
        .clone();

    call_deposit_from_config_evm(network, token_symbol, amount, signer, config).await
}

/// Solana deposit — builds and submits the user-signed Midrib `deposit`
/// instruction. Requires the `solana` feature.
#[cfg(feature = "solana")]
async fn solana_deposit(
    chain: &crate::commands::config::config_pb::Chain,
    token_symbol: &str,
    amount: u64,
    wallet: &Wallet,
) -> Result<()> {
    use solana_sdk::pubkey::Pubkey;
    use std::str::FromStr;

    let token = chain.tokens.get(token_symbol).ok_or_else(|| {
        eyre::eyre!(
            "Token '{}' not found on Solana chain '{}'",
            token_symbol,
            chain.network
        )
    })?;

    let keypair = wallet.as_solana().ok_or_else(|| {
        eyre::eyre!(
            "Solana chain '{}' requires an Ed25519 wallet (TRADER_PRIVKEY_SOLANA)",
            chain.network
        )
    })?;

    let (program_id, instance) = crate::solana::client::resolve_program_and_instance(chain)?;
    // Before building or signing anything: a missing or dead endpoint fails here, politely.
    let rpc_url = crate::chain_client::checked_chain_rpc_url(chain).await?;
    let user = solana_sdk::signer::Signer::pubkey(keypair);
    let mint = Pubkey::from_str(&token.address)
        .map_err(|e| eyre::eyre!("invalid Solana mint '{}': {}", token.address, e))?;
    let user_ata = crate::solana::derive_associated_token_account(&user, &mint);

    tracing::info!(
        "Solana deposit: {} {} on {} (program={}, instance={}, ata={})",
        amount,
        token_symbol,
        chain.network,
        program_id,
        instance,
        user_ata
    );

    let ix = crate::solana::deposit_ix(&program_id, &instance, &user, &mint, &user_ata, amount)?;

    // Native-SOL deposit (WSOL mint): wrap in the SAME transaction —
    // create the WSOL ATA (idempotent), move `amount` lamports into it,
    // `SyncNative` so the token balance reflects them, then deposit.
    if crate::solana::is_wsol_mint(&token.address) {
        use solana_client::nonblocking::rpc_client::RpcClient;

        // The wrap spends `amount` lamports from the fee-payer itself; fail
        // fast with a clear error instead of an opaque simulation failure.
        // Headroom: tx fee + possible ATA rent (~0.002 SOL).
        const WRAP_FEE_HEADROOM: u64 = 3_000_000;
        let rpc = RpcClient::new(rpc_url.clone());
        let lamports = rpc.get_balance(&user).await.unwrap_or(0);
        let required = amount.saturating_add(WRAP_FEE_HEADROOM);
        if lamports < required {
            return Err(eyre::eyre!(
                "insufficient SOL: wallet {user} has {lamports} lamports, needs \
                 {required} (deposit amount + ~0.003 SOL fee/rent headroom)"
            ));
        }

        let ata_ix = crate::solana::create_idempotent_ata_ix(&user, &user, &mint, &user_ata);
        let wrap_ix = crate::solana::system_transfer_ix(&user, &user_ata, amount);
        let sync_ix = crate::solana::sync_native_ix(&user_ata);
        let ixs = [ata_ix, wrap_ix, sync_ix, ix];
        let sig = crate::solana::client::submit_user_signed_multi(&rpc_url, keypair, &ixs).await?;
        tracing::info!("Solana native (wrapped-SOL) deposit confirmed: {}", sig);
        return Ok(());
    }

    let sig = crate::solana::client::submit_user_signed(&rpc_url, keypair, ix).await?;
    tracing::info!("Solana deposit confirmed: {}", sig);
    Ok(())
}

#[cfg(not(feature = "solana"))]
async fn solana_deposit(
    chain: &crate::commands::config::config_pb::Chain,
    _token_symbol: &str,
    _amount: u64,
    _wallet: &Wallet,
) -> Result<()> {
    Err(eyre::eyre!(
        "chain '{}' is Solana but the `solana` feature is disabled",
        chain.network
    ))
}

/// Tron deposit: resolve and probe the node, then [`tron_deposit_at`].
#[cfg(feature = "tron")]
async fn tron_deposit(
    chain: &crate::commands::config::config_pb::Chain,
    token_symbol: &str,
    amount: u128,
    wallet: &Wallet,
) -> Result<()> {
    // Before building or signing anything: a missing or dead endpoint fails
    // here, politely (the probe is `eth_chainId` at `/jsonrpc`).
    let base = crate::chain_client::checked_chain_rpc_url(chain).await?;
    let http = crate::tron::client::TronHttp::for_network(&base, &chain.network)?;
    tron_deposit_at(
        &http,
        chain,
        token_symbol,
        amount,
        wallet,
        &crate::tron::client::SendConfig::default(),
    )
    .await
}

#[cfg(not(feature = "tron"))]
async fn tron_deposit(
    chain: &crate::commands::config::config_pb::Chain,
    _token_symbol: &str,
    _amount: u128,
    _wallet: &Wallet,
) -> Result<()> {
    Err(eyre::eyre!(
        "chain '{}' is Tron but the `tron` feature is disabled",
        chain.network
    ))
}

/// The Tron deposit against an explicit node: the MidribV3 calls the EVM path
/// makes, sent as native Tron transactions by [`send_tron_tx_with`].
///
/// - TRX (the native sentinel): `depositNative()` with `amount` sun as
///   `call_value`. No approve.
/// - A TRC-20 (USDT): the allowance is read over `/jsonrpc`; when it is short
///   of `amount` + 1000 (the EVM path's margin), `approve(instance, amount +
///   1000)` is sent to the token and must succeed first; then
///   `deposit(token, amount)` to the instance.
///
/// [`send_tron_tx_with`]: crate::tron::client::send_tron_tx_with
#[cfg(feature = "tron")]
pub(crate) async fn tron_deposit_at(
    http: &crate::tron::client::TronHttp,
    chain: &crate::commands::config::config_pb::Chain,
    token_symbol: &str,
    amount: u128,
    wallet: &Wallet,
    send: &crate::tron::client::SendConfig,
) -> Result<()> {
    use crate::tron::client::{
        APPROVE_FEE_LIMIT_SUN, DEPOSIT_FEE_LIMIT_SUN, DEPOSIT_NATIVE_FEE_LIMIT_SUN,
        send_tron_tx_with,
    };
    use alloy::sol_types::SolCall;

    let token = chain.tokens.get(token_symbol).ok_or_else(|| {
        eyre::eyre!(
            "Token '{}' not found on Tron chain '{}'",
            token_symbol,
            chain.network
        )
    })?;
    let owner = wallet
        .as_evm()
        .ok_or_else(|| {
            eyre::eyre!(
                "Tron chain '{}' requires a secp256k1 wallet (TRADER_PRIVKEY)",
                chain.network
            )
        })?
        .address();
    let contract: Address = chain
        .trade_contract
        .as_ref()
        .ok_or_else(|| eyre::eyre!("Trade contract not found for chain '{}'", chain.network))?
        .address
        .parse()?;

    if crate::evm::is_native_token(&token.address) {
        let call_value: i64 = amount.try_into().map_err(|_| {
            eyre::eyre!("amount {amount} sun exceeds what a Tron call_value can carry")
        })?;
        tracing::info!("Tron depositNative of {amount} sun to {contract:#x}");
        let data = MidribV3::depositNativeCall {}.abi_encode();
        let receipt = send_tron_tx_with(
            http,
            wallet,
            contract,
            &data,
            call_value,
            DEPOSIT_NATIVE_FEE_LIMIT_SUN,
            send,
        )
        .await?;
        tracing::info!(
            "Tron native deposit confirmed: {} (block {})",
            hex::encode(receipt.txid),
            receipt.block_number
        );
        return Ok(());
    }

    let token_addr: Address = token.address.parse()?;
    let reads = ProviderBuilder::new()
        .connect_http(Url::parse(&crate::tron::client::jsonrpc_url(http.base())?)?);
    let allowance = IERC20::new(token_addr, &reads)
        .allowance(owner, contract)
        .call()
        .await?;
    let allowance_amount = U256::from(amount.saturating_add(1000));
    if allowance < allowance_amount {
        tracing::info!("Tron allowance {allowance} is short; approving {allowance_amount}");
        let data = IERC20::approveCall {
            spender: contract,
            amount: allowance_amount,
        }
        .abi_encode();
        let receipt = send_tron_tx_with(
            http,
            wallet,
            token_addr,
            &data,
            0,
            APPROVE_FEE_LIMIT_SUN,
            send,
        )
        .await?;
        tracing::info!("Tron approve confirmed: {}", hex::encode(receipt.txid));
    }

    let data = MidribV3::depositCall {
        _token: token_addr,
        _amount: U160::from(amount),
    }
    .abi_encode();
    let receipt = send_tron_tx_with(
        http,
        wallet,
        contract,
        &data,
        0,
        DEPOSIT_FEE_LIMIT_SUN,
        send,
    )
    .await?;
    tracing::info!(
        "Tron deposit confirmed: {} (block {})",
        hex::encode(receipt.txid),
        receipt.block_number
    );
    Ok(())
}

/// Original EVM deposit logic — kept private and called from the wallet-aware
/// dispatcher above.
async fn call_deposit_from_config_evm(
    network: String,
    token_symbol: String,
    amount: u128,
    signer: PrivateKeySigner,
    config: GetConfigResponse,
) -> Result<()> {
    // Look up chain info
    let chain = config.get_chain(&network).ok_or_else(|| {
        let available_chains = config
            .config
            .as_ref()
            .map(|c| {
                c.chains
                    .iter()
                    .map(|ch| ch.network.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        eyre::eyre!(
            "Chain '{}' not found in configuration. Available chains: {}",
            network,
            available_chains
        )
    })?;

    // Look up token info
    let token = config.get_token(&network, &token_symbol).ok_or_else(|| {
        let available_tokens = chain
            .tokens
            .keys()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        eyre::eyre!(
            "Token '{}' not found on chain '{}'. Available tokens: {}",
            token_symbol,
            network,
            available_tokens
        )
    })?;

    // Get trade contract address
    let contract_address = chain
        .trade_contract
        .as_ref()
        .ok_or_else(|| {
            eyre::eyre!(
                "Trade contract not found for chain '{}'. Please ensure the contract is deployed.",
                network
            )
        })?
        .address
        .clone();

    // alloy's chain metadata, when alloy knows the id. The hand-written match
    // this replaces listed nine chains and then ERRORED on anything else, so a
    // chain the venue happily lists but alloy has no variant for -- HyperEVM
    // testnet 998 -- could not deposit or withdraw at all. See
    // [`named_chain_for`]: an unnamed chain is a normal EVM chain, it just gets
    // no metadata. The RPC reports its own id and the tx is signed here.
    let named_chain = named_chain_for(chain.chain_id as u64);

    // Before building or signing anything: a missing or dead endpoint fails here, politely.
    let rpc_url = crate::chain_client::checked_chain_rpc_url(chain).await?;

    tracing::info!(
        "Depositing {} {} on {} (chain_id: {}, rpc: {})",
        amount,
        token_symbol,
        network,
        chain.chain_id,
        crate::chain_client::redact_rpc_url(&rpc_url)
    );

    // Perform the deposit
    let native = crate::evm::is_native_token(&token.address);
    let allowance_amount = U256::from(amount.saturating_add(1000));
    let deposit_amount = U160::from(amount);
    let contract_addr: Address = contract_address.parse()?;
    let token_addr: Address = token.address.parse()?;
    let signer_address = signer.address();
    let wallet = EthereumWallet::new(signer);
    let rpc_url = Url::parse(&rpc_url)?;

    // Set up the provider
    let provider = match named_chain {
        Some(named) => ProviderBuilder::new()
            .with_chain(named)
            .wallet(wallet)
            .connect_http(rpc_url)
            .erased(),
        None => ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(rpc_url)
            .erased(),
    };

    // Check the wallet balance before attempting any transactions. A native
    // deposit spends the deposit amount ITSELF from the gas balance, so it
    // must cover amount + gas headroom, not just gas.
    let gas_balance = provider.get_balance(signer_address).await?;
    tracing::info!("Gas balance: {} wei", gas_balance);

    let required = if native {
        U256::from(amount).saturating_add(U256::from(MIN_GAS_BALANCE))
    } else {
        U256::from(MIN_GAS_BALANCE)
    };
    if gas_balance < required {
        let balance_eth = gas_balance.to::<u128>() as f64 / 1e18;
        return Err(eyre::eyre!(
            "insufficient native balance: wallet has {:.6}, needs {} wei \
            ({}). Fund your wallet ({}) on {}.",
            balance_eth,
            required,
            if native {
                "deposit amount + 0.0001 gas headroom"
            } else {
                "0.0001 gas headroom"
            },
            signer_address,
            network
        ));
    }

    // Get an instance of the contract
    let contract = MidribV3::new(contract_addr, &provider);

    // Native-asset deposit: no ERC-20 approve — the value rides the call.
    if native {
        tracing::info!("Attempting NATIVE deposit of {amount} wei to contract {contract_addr}");
        let deposit_tx = contract.depositNative().value(U256::from(amount));
        match deposit_tx.estimate_gas().await {
            Ok(gas_estimate) => {
                tracing::info!("Gas estimate for depositNative: {gas_estimate:?}");
            }
            Err(e) => {
                tracing::error!("Failed to estimate gas for depositNative: {e:?}");
                return Err(e.into());
            }
        }
        let result = deposit_tx.send().await?;
        tracing::info!("Native deposit transaction sent: {result:?}");
        let receipt = result.with_required_confirmations(1).watch().await?;
        tracing::info!("Native deposit transaction hash: {receipt:?}");
        return Ok(());
    }

    let erc20 = IERC20::new(token_addr, &provider);
    // Get the allowance
    let allowance_result = erc20
        .allowance(signer_address, contract_addr)
        .call()
        .await?;

    tracing::info!("Get allowance result: {allowance_result:?}");

    // Only set allowance if current allowance is insufficient
    if allowance_result < allowance_amount {
        tracing::info!(
            "Current allowance insufficient, approving {} tokens",
            allowance_amount
        );
        let approve_result = erc20
            .approve(contract_addr, allowance_amount)
            .send()
            .await?
            .watch()
            .await?;
        tracing::info!("Set allowance result: {approve_result:?}");
    } else {
        tracing::info!("Sufficient allowance already set: {}", allowance_result);
    }

    // Call the contract function
    tracing::info!("Attempting deposit of {deposit_amount} tokens to contract {contract_addr}");

    let deposit_tx = contract.deposit(token_addr, deposit_amount);

    // Try to estimate gas first to see if the transaction would succeed
    match deposit_tx.estimate_gas().await {
        Ok(gas_estimate) => {
            tracing::info!("Gas estimate for deposit: {gas_estimate:?}");
        }
        Err(e) => {
            tracing::error!("Failed to estimate gas for deposit: {e:?}");
            return Err(e.into());
        }
    }

    let result = deposit_tx.send().await?;

    tracing::info!("Deposit transaction sent: {result:?}");

    let receipt = result.with_required_confirmations(1).watch().await?;

    tracing::info!("Deposit transaction hash: {receipt:?}");

    Ok(())
}

#[cfg(all(test, feature = "tron"))]
mod tron_tests {
    use super::*;
    use crate::tron::client::mock::*;
    use crate::tron::client::{
        APPROVE_FEE_LIMIT_SUN, DEPOSIT_FEE_LIMIT_SUN, DEPOSIT_NATIVE_FEE_LIMIT_SUN, TronHttp,
    };
    use alloy::sol_types::SolCall;
    use wiremock::MockServer;

    fn word(hex20: &str) -> String {
        format!("{:0>64}", hex20.trim_start_matches("0x"))
    }

    fn amount_word(v: u128) -> String {
        format!("{v:064x}")
    }

    async fn deposit(server: &MockServer, symbol: &str, amount: u128) -> Result<()> {
        tron_deposit_at(
            &TronHttp::new(&server.uri()).unwrap(),
            &nile_chain(),
            symbol,
            amount,
            &wallet(),
            &fast(),
        )
        .await
    }

    /// TRX rides the call as `call_value` on `depositNative()` (selector
    /// `db6b5246`, as in the Nile fixture). There is no approve, and no
    /// allowance is read.
    #[tokio::test]
    async fn tron_native_deposit_sets_call_value_no_approve() {
        let server = MockServer::start().await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("SUCCESS", 1)).await;
        deposit(&server, "TRX", 10_000_000).await.unwrap();

        let sent = broadcasts(&server).await;
        assert_eq!(sent.len(), 1, "one transaction, no approve");
        let tx = decode_sent(&sent[0]);
        assert_eq!(
            hex::encode(bytes_of(&tx.trigger, 2)),
            format!("41{}", MIDRIB.trim_start_matches("0x"))
        );
        assert_eq!(hex::encode(bytes_of(&tx.trigger, 4)), "db6b5246");
        assert_eq!(field(&tx.trigger, 3), Some(Field::Varint(10_000_000)));
        assert_eq!(
            varint_of(&fields(&tx.raw), 18) as i64,
            DEPOSIT_NATIVE_FEE_LIMIT_SUN
        );
        assert!(bodies(&server, "/jsonrpc").await.is_empty());
    }

    /// USDT: the allowance is read over `/jsonrpc`; with none, `approve` goes
    /// to the token first (the EVM path's amount + 1000), then `deposit(token,
    /// amount)` to the instance. Neither carries a call value.
    #[tokio::test]
    async fn tron_usdt_deposit_approves_then_deposits() {
        let server = MockServer::start().await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("SUCCESS", 0)).await;
        mount_jsonrpc(
            &server,
            "0x0000000000000000000000000000000000000000000000000000000000000000",
        )
        .await;
        deposit(&server, "USDT", 50_000_000).await.unwrap();

        let sent = broadcasts(&server).await;
        assert_eq!(sent.len(), 2, "approve, then deposit");
        let approve = decode_sent(&sent[0]);
        assert_eq!(
            hex::encode(bytes_of(&approve.trigger, 2)),
            format!("41{}", USDT.trim_start_matches("0x"))
        );
        assert_eq!(
            hex::encode(bytes_of(&approve.trigger, 4)),
            format!("095ea7b3{}{}", word(MIDRIB), amount_word(50_001_000))
        );
        assert_eq!(field(&approve.trigger, 3), None);
        assert_eq!(
            varint_of(&fields(&approve.raw), 18) as i64,
            APPROVE_FEE_LIMIT_SUN
        );

        let dep = decode_sent(&sent[1]);
        assert_eq!(
            hex::encode(bytes_of(&dep.trigger, 2)),
            format!("41{}", MIDRIB.trim_start_matches("0x"))
        );
        assert_eq!(
            hex::encode(bytes_of(&dep.trigger, 4)),
            format!(
                "{}{}{}",
                hex::encode(MidribV3::depositCall::SELECTOR),
                word(USDT),
                amount_word(50_000_000)
            )
        );
        assert_eq!(field(&dep.trigger, 3), None);
        assert_eq!(
            varint_of(&fields(&dep.raw), 18) as i64,
            DEPOSIT_FEE_LIMIT_SUN
        );
        let reads = bodies(&server, "/jsonrpc").await;
        assert_eq!(reads.len(), 1, "{reads:?}");
        assert_eq!(reads[0]["method"], "eth_call");
    }

    /// An allowance that already covers the deposit means no approve.
    #[tokio::test]
    async fn tron_usdt_deposit_skips_a_covered_approve() {
        let server = MockServer::start().await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("SUCCESS", 0)).await;
        mount_jsonrpc(
            &server,
            "0x00000000000000000000000000000000000000000000000000000000ffffffff",
        )
        .await;
        deposit(&server, "USDT", 50_000_000).await.unwrap();
        let sent = broadcasts(&server).await;
        assert_eq!(sent.len(), 1);
        assert_eq!(
            hex::encode(bytes_of(&decode_sent(&sent[0]).trigger, 2)),
            format!("41{}", MIDRIB.trim_start_matches("0x"))
        );
    }

    /// A failed approve stops the deposit: nothing more is sent.
    #[tokio::test]
    async fn tron_failed_approve_sends_no_deposit() {
        let server = MockServer::start().await;
        mount_node(&server).await;
        mount_broadcast_ok(&server).await;
        mount_tx_info(&server, TxInfoEcho::new("REVERT", 0)).await;
        mount_jsonrpc(
            &server,
            "0x0000000000000000000000000000000000000000000000000000000000000000",
        )
        .await;
        let err = deposit(&server, "USDT", 50_000_000).await.unwrap_err();
        assert!(err.to_string().contains("REVERT"), "{err}");
        assert_eq!(broadcasts(&server).await.len(), 1);
    }
}
