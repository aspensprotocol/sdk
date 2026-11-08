//! Owner-signed reads of the caller's own orders.
//!
//! Two signatures authorize a read (the arborter side is `read_auth.rs`):
//!
//! 1. A **read delegation**, signed once per owner wallet over a fixed text
//!    (`delegation_text`) that names the venue, the owner, an Ed25519
//!    delegate key and a validity window of at most 24 hours. It is signed the
//!    way a withdraw request is: EIP-191 on EVM, the TRON message prefix on
//!    Tron, raw Ed25519 on Solana (`super::sign_withdraw_request`).
//! 2. A **query signature**, made silently by the in-memory `DelegateKey`
//!    over `prost`'s encoding of the `MyOrdersQuery` actually sent. Each
//!    query carries a millisecond timestamp and a fresh 16-byte random nonce.
//!
//! A `ReadSession` holds 1 to 4 delegations, all for one delegate key, so a
//! user with wallets on several chains signs each once and every query covers
//! all of them. When the caller passes the wallets, a delegation with less than
//! five minutes left is re-signed before the query goes out.
//!
//! A Tron owner is always sent as lowercase `0x` hex; the `T…` form is refused
//! by the venue.

use std::collections::HashMap;
use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signer as _, SigningKey};
use eyre::{Result, eyre};

use super::arborter_pb::arborter_service_client::ArborterServiceClient;
pub use super::arborter_pb::{
    MyOrder, MyOrdersQuery, MyOrdersRequest, MyOrdersResponse, ReadDelegation,
};
use crate::client::AspensClient;
use crate::commands::config::config_pb::GetConfigResponse;
use crate::grpc::create_channel;
use crate::wallet::{Architecture, CurveType, Wallet, chain_architecture};

/// The longest validity window a delegation may name.
pub const MAX_DELEGATION_TTL: Duration = Duration::from_secs(86_400);
/// A delegation with less than this long left is re-signed before a query.
pub const REDELEGATE_BELOW: Duration = Duration::from_secs(300);
/// The most delegations one request may carry.
pub const MAX_DELEGATIONS: usize = 4;
/// The most order ids one `GetMyOrders` call may carry; longer lists are chunked.
pub const MAX_IDS_PER_CALL: usize = 100;
/// The page size requested from `ListMyOrders` (the venue's maximum).
const LIST_PAGE_LIMIT: u32 = 500;
/// A query nonce is exactly this long.
const NONCE_LEN: usize = 16;
/// A re-signed delegation is valid for at least this long, so a delegation
/// first signed for a minute is not re-signed on every query.
const MIN_REDELEGATION_TTL: Duration = Duration::from_secs(600);

/// An Ed25519 delegate key, held in memory only. It signs queries; the owner's
/// wallet never does after the delegation is made.
pub struct DelegateKey(SigningKey);

impl std::fmt::Debug for DelegateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DelegateKey({})", hex::encode(self.pubkey()))
    }
}

impl DelegateKey {
    /// A fresh key from the OS random source.
    pub fn generate() -> Result<Self> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| eyre!("generating delegate key: {e}"))?;
        Ok(Self(SigningKey::from_bytes(&seed)))
    }

    /// The key with this 32-byte seed.
    pub fn from_bytes(seed: &[u8; 32]) -> Self {
        Self(SigningKey::from_bytes(seed))
    }

    /// The 32-byte public key a delegation names.
    pub fn pubkey(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.0.sign(msg).to_bytes().to_vec()
    }
}

/// The delegations in force and the delegate key that signs queries under them.
#[derive(Debug)]
pub struct ReadSession {
    delegations: Vec<ReadDelegation>,
    key: DelegateKey,
}

impl ReadSession {
    /// A session over `delegations` (1 to 4), each of which must name `key`.
    pub fn new(delegations: Vec<ReadDelegation>, key: DelegateKey) -> Result<Self> {
        if delegations.is_empty() || delegations.len() > MAX_DELEGATIONS {
            return Err(eyre!(
                "a read session needs 1 to {MAX_DELEGATIONS} delegations, got {}",
                delegations.len()
            ));
        }
        let pk = key.pubkey();
        if delegations
            .iter()
            .any(|d| d.delegate_pubkey.as_slice() != pk.as_slice())
        {
            return Err(eyre!(
                "every delegation must name the session's delegate key"
            ));
        }
        Ok(Self { delegations, key })
    }

    /// The delegations currently in the session.
    pub fn delegations(&self) -> &[ReadDelegation] {
        &self.delegations
    }
}

/// The text an owner signs to delegate read access. LF line endings, no
/// trailing newline. `owner` is the address exactly as carried in
/// `ReadDelegation.owner`; the delegate key is lowercase hex with no `0x`.
pub(crate) fn delegation_text(
    venue_id: &str,
    owner: &str,
    delegate: &[u8; 32],
    issued: u64,
    expires: u64,
) -> String {
    format!(
        "Aspens read-only delegation\n\
         Venue: {venue_id}\n\
         Owner: {owner}\n\
         Delegate: {}\n\
         Scope: order-status\n\
         Issued: {issued}\n\
         Expires: {expires}",
        hex::encode(delegate)
    )
}

fn now() -> Result<Duration> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| eyre!("system clock is before the Unix epoch: {e}"))
}

pub(crate) fn now_ms() -> Result<u64> {
    u64::try_from(now()?.as_millis()).map_err(|_| eyre!("system clock out of range"))
}

/// The venue id is 32 lowercase hex characters (`Configuration.venue_id`).
fn check_venue_id(venue_id: &str) -> Result<()> {
    let ok = venue_id.len() == 32
        && venue_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if ok {
        Ok(())
    } else {
        Err(eyre!(
            "venue id must be 32 lowercase hex characters, got {venue_id:?}"
        ))
    }
}

/// The owner address as `ReadDelegation.owner` carries it: checksummed `0x`
/// hex on EVM, lowercase `0x` hex on Tron (never `T…`), base58 on Solana.
fn owner_for(wallet: &Wallet, architecture: Architecture) -> Result<String> {
    match (architecture, wallet.curve()) {
        (Architecture::Evm, CurveType::Secp256k1) | (Architecture::Solana, CurveType::Ed25519) => {
            Ok(wallet.address())
        }
        (Architecture::Tron, CurveType::Secp256k1) => {
            let signer = wallet
                .as_evm()
                .ok_or_else(|| eyre!("a Tron owner needs a secp256k1 (EVM) wallet"))?;
            Ok(format!("{:#x}", signer.address()))
        }
        (arch, curve) => Err(eyre!(
            "a {curve:?} wallet cannot own an account on a {arch:?} chain"
        )),
    }
}

/// Whether `owner` (as carried in a delegation) is `wallet`'s account.
fn owns(wallet: &Wallet, architecture: Architecture, owner: &str) -> bool {
    match owner_for(wallet, architecture) {
        Ok(mine) => match architecture {
            Architecture::Solana => mine == owner,
            Architecture::Evm | Architecture::Tron => mine.eq_ignore_ascii_case(owner),
        },
        Err(_) => false,
    }
}

/// Sign a read delegation for `key`, valid for `ttl`.
///
/// `owner_network` is a configured network name and `architecture` its chain
/// family; together they select the signature scheme. `ttl` must be positive
/// and at most 24 hours.
pub async fn create_read_delegation_with_wallet(
    wallet: &Wallet,
    owner_network: &str,
    architecture: Architecture,
    venue_id: &str,
    key: &DelegateKey,
    ttl: Duration,
) -> Result<ReadDelegation> {
    if ttl > MAX_DELEGATION_TTL {
        return Err(eyre!(
            "a read delegation lasts at most {} s, asked for {} s",
            MAX_DELEGATION_TTL.as_secs(),
            ttl.as_secs()
        ));
    }
    create_at(
        wallet,
        owner_network,
        architecture,
        venue_id,
        key,
        ttl.as_secs(),
        now()?.as_secs(),
    )
    .await
}

pub(crate) async fn create_at(
    wallet: &Wallet,
    owner_network: &str,
    architecture: Architecture,
    venue_id: &str,
    key: &DelegateKey,
    ttl_s: u64,
    issued: u64,
) -> Result<ReadDelegation> {
    check_venue_id(venue_id)?;
    if ttl_s == 0 || ttl_s > MAX_DELEGATION_TTL.as_secs() {
        return Err(eyre!(
            "a read delegation lasts 1 to {} s, asked for {ttl_s} s",
            MAX_DELEGATION_TTL.as_secs()
        ));
    }
    let owner = owner_for(wallet, architecture)?;
    let expires = issued + ttl_s;
    let delegate = key.pubkey();
    let text = delegation_text(venue_id, &owner, &delegate, issued, expires);
    let signature = super::sign_withdraw_request(wallet, architecture, &text).await?;
    Ok(ReadDelegation {
        owner_network: owner_network.to_string(),
        owner,
        delegate_pubkey: delegate.to_vec(),
        issued,
        expires,
        signature,
    })
}

pub(crate) fn build_request_at(
    session: &ReadSession,
    mut query: MyOrdersQuery,
    now_ms: u64,
    nonce: [u8; NONCE_LEN],
) -> MyOrdersRequest {
    use prost::Message;
    query.timestamp_ms = now_ms;
    query.nonce = nonce.to_vec();
    // Signed over exactly the bytes prost produces for the query being sent;
    // the venue re-encodes the decoded query and verifies against that.
    let query_signature = session.key.sign(&query.encode_to_vec());
    MyOrdersRequest {
        delegations: session.delegations.clone(),
        query: Some(query),
        query_signature,
    }
}

/// Authorize `query` under the session: stamp it with the current time and a
/// fresh 16-byte random nonce (replacing any set on `query`) and sign it with
/// the delegate key.
pub fn build_my_orders_request(
    session: &ReadSession,
    query: MyOrdersQuery,
) -> Result<MyOrdersRequest> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|e| eyre!("generating query nonce: {e}"))?;
    Ok(build_request_at(session, query, now_ms()?, nonce))
}

/// Re-sign every delegation with less than [`REDELEGATE_BELOW`] left, for
/// those whose owner is one of `wallets`. A delegation no wallet here owns is
/// left alone. Returns how many were re-signed.
///
/// The new window is the old one's length, but at least ten minutes and at
/// most 24 hours.
pub(crate) async fn refresh_delegations(
    session: &mut ReadSession,
    wallets: &[&Wallet],
    venue_id: &str,
    architecture_of: impl Fn(&str) -> Result<Architecture>,
    now_s: u64,
) -> Result<usize> {
    let mut refreshed = 0;
    for i in 0..session.delegations.len() {
        let d = session.delegations[i].clone();
        if d.expires.saturating_sub(now_s) >= REDELEGATE_BELOW.as_secs() {
            continue;
        }
        let architecture = architecture_of(&d.owner_network)?;
        let Some(wallet) = wallets.iter().find(|w| owns(w, architecture, &d.owner)) else {
            continue;
        };
        let ttl = d
            .expires
            .saturating_sub(d.issued)
            .clamp(MIN_REDELEGATION_TTL.as_secs(), MAX_DELEGATION_TTL.as_secs());
        session.delegations[i] = create_at(
            wallet,
            &d.owner_network,
            architecture,
            venue_id,
            &session.key,
            ttl,
            now_s,
        )
        .await?;
        refreshed += 1;
    }
    Ok(refreshed)
}

/// `venue_id` and a network-to-architecture map, from the venue config.
fn venue_context(config: &GetConfigResponse) -> Result<(String, HashMap<String, Architecture>)> {
    let config = config
        .config
        .as_ref()
        .ok_or_else(|| eyre!("the venue returned no configuration"))?;
    check_venue_id(&config.venue_id)?;
    let mut archs = HashMap::new();
    for chain in &config.chains {
        archs.insert(chain.network.clone(), chain_architecture(chain)?);
    }
    Ok((config.venue_id.clone(), archs))
}

async fn prepare(
    config: &GetConfigResponse,
    session: &mut ReadSession,
    wallets: &[&Wallet],
) -> Result<()> {
    let (venue_id, archs) = venue_context(config)?;
    refresh_delegations(
        session,
        wallets,
        &venue_id,
        |network| {
            archs
                .get(network)
                .copied()
                .ok_or_else(|| eyre!("delegation names unknown network '{network}'"))
        },
        now()?.as_secs(),
    )
    .await?;
    Ok(())
}

/// Open a read session for `key` covering every account `wallets` own on
/// this venue: one delegation, valid for `ttl`, per chain family (EVM, Tron,
/// Solana) that has both a chain in `config` and a wallet of its curve. Each
/// delegation names the first configured network of its family.
///
/// An EVM key owns its EVM and its Tron account through two delegations; the
/// venue verifies each under its own scheme. Errors when no wallet owns an
/// account on any configured chain.
pub async fn open_read_session_with_wallets(
    config: &GetConfigResponse,
    wallets: &[&Wallet],
    key: DelegateKey,
    ttl: Duration,
) -> Result<ReadSession> {
    let cfg = config
        .config
        .as_ref()
        .ok_or_else(|| eyre!("the venue returned no configuration"))?;
    let mut families: Vec<Architecture> = Vec::new();
    let mut delegations = Vec::new();
    for chain in &cfg.chains {
        let architecture = chain_architecture(chain)?;
        if families.contains(&architecture) {
            continue;
        }
        let Some(wallet) = wallets.iter().find(|w| w.curve() == architecture.curve()) else {
            continue;
        };
        families.push(architecture);
        delegations.push(
            create_read_delegation_with_wallet(
                wallet,
                &chain.network,
                architecture,
                &cfg.venue_id,
                &key,
                ttl,
            )
            .await?,
        );
    }
    if delegations.is_empty() {
        return Err(eyre!(
            "no wallet given owns an account on any chain this venue lists"
        ));
    }
    ReadSession::new(delegations, key)
}

/// Fetch the caller's orders by id, in the order given (duplicates dropped).
/// More than 100 ids go out as several calls and are merged by order id. An id
/// the venue has no record of for these owners comes back with `found: false`.
///
/// When `wallets` holds the delegations' owners, a delegation with less than
/// five minutes left is re-signed first.
pub async fn get_my_orders_with_wallet(
    client: &AspensClient,
    session: &mut ReadSession,
    wallets: &[&Wallet],
    ids: &[[u8; 32]],
) -> Result<Vec<MyOrder>> {
    let config = client.get_config().await?;
    get_my_orders_from_config_with_wallet(
        client.stack_url().as_str(),
        &config,
        session,
        wallets,
        ids,
    )
    .await
}

/// [`get_my_orders_with_wallet`] against the venue at `url`, whose
/// configuration the caller already holds.
pub async fn get_my_orders_from_config_with_wallet(
    url: &str,
    config: &GetConfigResponse,
    session: &mut ReadSession,
    wallets: &[&Wallet],
    ids: &[[u8; 32]],
) -> Result<Vec<MyOrder>> {
    prepare(config, session, wallets).await?;
    let rpc = ArborterServiceClient::new(create_channel(url).await?);
    get_orders_via(session, ids, |req| {
        let mut rpc = rpc.clone();
        async move {
            Ok(rpc
                .get_my_orders(tonic::Request::new(req))
                .await?
                .into_inner())
        }
    })
    .await
}

/// List the caller's orders, optionally for one market, following the venue's
/// cursor until the last page. `include_closed` adds orders that have closed.
///
/// Re-delegates like [`get_my_orders_with_wallet`].
pub async fn list_my_orders_with_wallet(
    client: &AspensClient,
    session: &mut ReadSession,
    wallets: &[&Wallet],
    market: Option<&str>,
    include_closed: bool,
) -> Result<Vec<MyOrder>> {
    let config = client.get_config().await?;
    list_my_orders_from_config_with_wallet(
        client.stack_url().as_str(),
        &config,
        session,
        wallets,
        market,
        include_closed,
    )
    .await
}

/// [`list_my_orders_with_wallet`] against the venue at `url`, whose
/// configuration the caller already holds.
pub async fn list_my_orders_from_config_with_wallet(
    url: &str,
    config: &GetConfigResponse,
    session: &mut ReadSession,
    wallets: &[&Wallet],
    market: Option<&str>,
    include_closed: bool,
) -> Result<Vec<MyOrder>> {
    prepare(config, session, wallets).await?;
    let rpc = ArborterServiceClient::new(create_channel(url).await?);
    list_orders_via(session, market, include_closed, |req| {
        let mut rpc = rpc.clone();
        async move {
            Ok(rpc
                .list_my_orders(tonic::Request::new(req))
                .await?
                .into_inner())
        }
    })
    .await
}

/// The snake_case name of a `MyOrderState` value; `unspecified` for one this
/// SDK does not know.
pub fn my_order_state_name(state: i32) -> &'static str {
    use super::arborter_pb::MyOrderState as S;
    match S::try_from(state) {
        Ok(S::Open) => "open",
        Ok(S::PartiallyFilled) => "partially_filled",
        Ok(S::Filled) => "filled",
        Ok(S::Cancelled) => "cancelled",
        Ok(S::Expired) => "expired",
        Ok(S::DroppedByRestart) => "dropped_by_restart",
        Ok(S::Traded) => "traded",
        Ok(S::Unspecified) | Err(_) => "unspecified",
    }
}

fn hex_id(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// `o` as JSON, with ids as `0x` hex and amounts as the venue's pair-unit
/// strings. An order the venue did not find is its id and `found: false`.
pub fn my_order_json(o: &MyOrder) -> serde_json::Value {
    use super::arborter_pb::{FillRole, MyOrderKind, Side};
    use serde_json::json;
    if !o.found {
        return json!({ "orderId": hex_id(&o.order_id), "found": false });
    }
    let side = match Side::try_from(o.side) {
        Ok(Side::Bid) => "buy",
        Ok(Side::Ask) => "sell",
        _ => "unspecified",
    };
    let kind = match MyOrderKind::try_from(o.kind) {
        Ok(MyOrderKind::Limit) => "limit",
        Ok(MyOrderKind::Market) => "market",
        Ok(MyOrderKind::Discretionary) => "discretionary",
        _ => "unspecified",
    };
    let fills: Vec<serde_json::Value> = o
        .fills
        .iter()
        .map(|f| {
            let role = match FillRole::try_from(f.role) {
                Ok(FillRole::Maker) => "maker",
                Ok(FillRole::Taker) => "taker",
                _ => "unspecified",
            };
            json!({
                "fillId": hex_id(&f.fill_id),
                "price": f.price,
                "qty": f.qty,
                "matchTsMs": f.match_ts_ms,
                "role": role,
            })
        })
        .collect();
    json!({
        "orderId": hex_id(&o.order_id),
        "found": true,
        "marketId": o.market_id,
        "side": side,
        "kind": kind,
        "hidden": o.is_hidden,
        "postOnly": o.is_post_only,
        "price": o.price,
        "initialQtyKnown": o.initial_qty_known,
        "initialQty": o.initial_qty,
        "filledQty": o.filled_qty,
        "remainingQty": o.remaining_qty,
        "state": my_order_state_name(o.state),
        "createdAtMs": o.created_at_ms,
        "closedAtMs": o.closed_at_ms,
        "baseAddress": o.base_address,
        "quoteAddress": o.quote_address,
        "fills": fills,
    })
}

pub(crate) async fn get_orders_via<F, Fut>(
    session: &ReadSession,
    ids: &[[u8; 32]],
    mut send: F,
) -> Result<Vec<MyOrder>>
where
    F: FnMut(MyOrdersRequest) -> Fut,
    Fut: Future<Output = Result<MyOrdersResponse>>,
{
    let mut seen = std::collections::HashSet::new();
    let unique: Vec<[u8; 32]> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    if unique.is_empty() {
        return Err(eyre!("no order ids to fetch"));
    }
    // Keyed by id: the venue matches results to ids, not to positions.
    let mut by_id: HashMap<Vec<u8>, MyOrder> = HashMap::with_capacity(unique.len());
    for chunk in unique.chunks(MAX_IDS_PER_CALL) {
        let query = MyOrdersQuery {
            order_ids: chunk.iter().map(|id| id.to_vec()).collect(),
            ..Default::default()
        };
        let resp = send(build_my_orders_request(session, query)?).await?;
        for order in resp.orders {
            by_id.insert(order.order_id.clone(), order);
        }
    }
    Ok(unique
        .iter()
        .filter_map(|id| by_id.remove(id.as_slice()))
        .collect())
}

pub(crate) async fn list_orders_via<F, Fut>(
    session: &ReadSession,
    market: Option<&str>,
    include_closed: bool,
    mut send: F,
) -> Result<Vec<MyOrder>>
where
    F: FnMut(MyOrdersRequest) -> Fut,
    Fut: Future<Output = Result<MyOrdersResponse>>,
{
    let mut out = Vec::new();
    let mut cursor: Vec<u8> = Vec::new();
    loop {
        let query = MyOrdersQuery {
            market_id: market.unwrap_or_default().to_string(),
            include_closed,
            cursor: cursor.clone(),
            limit: LIST_PAGE_LIMIT,
            ..Default::default()
        };
        let resp = send(build_my_orders_request(session, query)?).await?;
        out.extend(resp.orders);
        if resp.next_cursor.is_empty() {
            return Ok(out);
        }
        if resp.next_cursor == cursor {
            return Err(eyre!("the venue returned the same page cursor twice"));
        }
        cursor = resp.next_cursor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::Wallet;
    use alloy::primitives::{B256, Signature, keccak256};
    use ed25519_dalek::{Signature as EdSignature, SigningKey, VerifyingKey};
    use std::sync::Mutex;

    const ANVIL_3_KEY: &str = "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a";
    const ANVIL_3_ADDR: &str = "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC";
    const ANVIL_3_ADDR_LOWER: &str = "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc";
    const DELEGATE_SEED: [u8; 32] = [0x42; 32];
    const SOLANA_OWNER_SEED: [u8; 32] = [0x5a; 32];
    const VENUE: &str = "0123456789abcdef0123456789abcdef";
    const NOW_S: u64 = 1_762_560_000;
    const NOW_MS: u64 = NOW_S * 1000;

    /// Copied from arborter `read_auth.rs` (`DELEGATION_TEXT_GOLDEN_HEX`), pinned
    /// there by the `delegation_text_golden` test: venue `VENUE`, owner
    /// `ANVIL_3_ADDR`, delegate = pubkey of seed `[0x42; 32]`, issued
    /// `1762560000`, expires `1762646400`.
    const DELEGATION_TEXT_GOLDEN_HEX: &str = "417370656e7320726561642d6f6e6c792064656c65676174696f6e0a56656e75653a2030313233343536373839616263646566303132333435363738396162636465660a4f776e65723a203078334334344364446442366139303066613262353835646432393965303364313246413432393342430a44656c65676174653a20323135326638643139623739316432343435333234326531356632656162366362376366666137623661356564333030393739363065303639383831646231320a53636f70653a206f726465722d7374617475730a4973737565643a20313736323536303030300a457870697265733a2031373632363436343030";

    /// Copied from arborter `read_auth.rs` (`MY_ORDERS_QUERY_GOLDEN_HEX`), the
    /// prost encoding of its `golden_query()` (asserted there by
    /// `query_signature_over_prost_bytes`).
    const MY_ORDERS_QUERY_GOLDEN_HEX: &str = "0880808d86a6331210000102030405060708090a0b0c0d0e0f1a2011111111111111111111111111111111111111111111111111111111111111111a20222222222222222222222222222222222222222222222222222222222222222222086d61726b65742d372801320363757238fa01";

    fn key() -> DelegateKey {
        DelegateKey::from_bytes(&DELEGATE_SEED)
    }

    fn evm_wallet() -> Wallet {
        Wallet::from_evm_hex(ANVIL_3_KEY).unwrap()
    }

    fn solana_wallet() -> Wallet {
        let sk = SigningKey::from_bytes(&SOLANA_OWNER_SEED);
        let mut kp = sk.to_bytes().to_vec();
        kp.extend_from_slice(&sk.verifying_key().to_bytes());
        Wallet::from_solana_base58(&bs58::encode(kp).into_string()).unwrap()
    }

    fn text_of(d: &ReadDelegation) -> String {
        let delegate: [u8; 32] = d.delegate_pubkey.as_slice().try_into().unwrap();
        delegation_text(VENUE, &d.owner, &delegate, d.issued, d.expires)
    }

    fn tron_digest(text: &str) -> B256 {
        let mut pre = b"\x19TRON Signed Message:\n".to_vec();
        pre.extend_from_slice(text.len().to_string().as_bytes());
        pre.extend_from_slice(text.as_bytes());
        keccak256(pre)
    }

    #[test]
    fn delegation_text_matches_arborter_golden() {
        let text = delegation_text(VENUE, ANVIL_3_ADDR, &key().pubkey(), NOW_S, NOW_S + 86_400);
        assert_eq!(
            hex::encode(key().pubkey()),
            "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12"
        );
        assert_eq!(hex::encode(text.as_bytes()), DELEGATION_TEXT_GOLDEN_HEX);
        assert!(!text.ends_with('\n') && !text.contains('\r'));
    }

    #[test]
    fn query_bytes_match_arborter_golden() {
        use prost::Message;
        // arborter `golden_query()`
        let q = MyOrdersQuery {
            timestamp_ms: NOW_MS,
            nonce: (0u8..16).collect(),
            order_ids: vec![vec![0x11; 32], vec![0x22; 32]],
            market_id: "market-7".to_string(),
            include_closed: true,
            cursor: b"cur".to_vec(),
            limit: 250,
        };
        assert_eq!(hex::encode(q.encode_to_vec()), MY_ORDERS_QUERY_GOLDEN_HEX);

        // The request builder signs exactly prost's bytes for the query it
        // sends, and the delegate signature verifies strictly.
        let session = ReadSession::new(vec![delegation_stub()], key()).unwrap();
        let nonce: [u8; 16] = std::array::from_fn(|i| i as u8);
        let req = build_request_at(&session, q.clone(), NOW_MS, nonce);
        assert_eq!(req.query.as_ref().unwrap(), &q);
        let vk = VerifyingKey::from_bytes(&key().pubkey()).unwrap();
        let sig = EdSignature::from_slice(&req.query_signature).unwrap();
        vk.verify_strict(&req.query.as_ref().unwrap().encode_to_vec(), &sig)
            .unwrap();
        // ...and the signed bytes are the golden ones, not merely self-consistent.
        vk.verify_strict(&hex::decode(MY_ORDERS_QUERY_GOLDEN_HEX).unwrap(), &sig)
            .unwrap();
    }

    fn delegation_stub() -> ReadDelegation {
        ReadDelegation {
            owner_network: "base-sepolia".into(),
            owner: ANVIL_3_ADDR.into(),
            delegate_pubkey: key().pubkey().to_vec(),
            issued: NOW_S,
            expires: NOW_S + 3600,
            signature: vec![0; 65],
        }
    }

    /// Arborter `handlers/order_id.rs::derive` pins
    /// `0xc6d8846d…774f2d` in `sdk_parity_known_good_vector` for these inputs
    /// (limit bid: 1.5 base at 2.0, pair 12 dp, base 18 dp, quote 6 dp, so
    /// input 3_000_000 and output 1.5e18).
    #[test]
    fn order_id_derive_matches_server_golden() {
        let user = hex::decode("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266").unwrap();
        let id = crate::orders::derive_order_id(
            &user,
            1_723_000_000_000,
            990_002,
            990_001,
            b"0x00000000000000000000000000000000000000c1",
            b"0x00000000000000000000000000000000000000b1",
            3_000_000,
            1_500_000_000_000_000_000,
        );
        assert_eq!(
            hex::encode(id),
            "c6d8846de54b5b1b2513fa5fb8b535ea48bbe2835c17187e6564443aaa774f2d"
        );
    }

    #[tokio::test]
    async fn evm_delegation_signs_text_with_eip191() {
        let d = create_at(
            &evm_wallet(),
            "base-sepolia",
            Architecture::Evm,
            VENUE,
            &key(),
            3600,
            NOW_S,
        )
        .await
        .unwrap();
        assert_eq!(d.owner, ANVIL_3_ADDR);
        assert_eq!((d.issued, d.expires), (NOW_S, NOW_S + 3600));
        assert_eq!(d.delegate_pubkey, key().pubkey().to_vec());
        let text = text_of(&d);
        let mut pre = b"\x19Ethereum Signed Message:\n".to_vec();
        pre.extend_from_slice(text.len().to_string().as_bytes());
        pre.extend_from_slice(text.as_bytes());
        let got = Signature::from_raw(&d.signature)
            .unwrap()
            .recover_address_from_prehash(&keccak256(pre))
            .unwrap();
        assert_eq!(got.to_checksum(None), ANVIL_3_ADDR);
    }

    #[cfg(feature = "tron")]
    #[tokio::test]
    async fn tron_owner_sent_as_hex() {
        let d = create_at(
            &evm_wallet(),
            "tron-nile",
            Architecture::Tron,
            VENUE,
            &key(),
            3600,
            NOW_S,
        )
        .await
        .unwrap();
        // 0x-hex, lowercase; never the base58 `T...` form.
        assert_eq!(d.owner, ANVIL_3_ADDR_LOWER);
        assert!(d.owner.starts_with("0x") && !d.owner.starts_with('T'));
        // Signed under the TRON prefix over the text as is, not EIP-191.
        let text = text_of(&d);
        let sig = Signature::from_raw(&d.signature).unwrap();
        assert_eq!(
            sig.recover_address_from_prehash(&tron_digest(&text))
                .unwrap()
                .to_checksum(None),
            ANVIL_3_ADDR
        );
    }

    #[tokio::test]
    async fn solana_delegation_signs_text_raw_ed25519() {
        let w = solana_wallet();
        let d = create_at(
            &w,
            "solana-devnet",
            Architecture::Solana,
            VENUE,
            &key(),
            3600,
            NOW_S,
        )
        .await
        .unwrap();
        assert_eq!(d.owner, w.address());
        let vk = VerifyingKey::from_bytes(
            &SigningKey::from_bytes(&SOLANA_OWNER_SEED)
                .verifying_key()
                .to_bytes(),
        )
        .unwrap();
        vk.verify_strict(
            text_of(&d).as_bytes(),
            &EdSignature::from_slice(&d.signature).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn wallet_and_architecture_must_agree() {
        let e = create_at(
            &solana_wallet(),
            "tron-nile",
            Architecture::Tron,
            VENUE,
            &key(),
            3600,
            NOW_S,
        )
        .await;
        assert!(e.is_err());
        let e = create_at(
            &evm_wallet(),
            "solana-devnet",
            Architecture::Solana,
            VENUE,
            &key(),
            3600,
            NOW_S,
        )
        .await;
        assert!(e.is_err());
    }

    #[tokio::test]
    async fn ttl_over_24h_refused() {
        let w = evm_wallet();
        let ok = create_read_delegation_with_wallet(
            &w,
            "base-sepolia",
            Architecture::Evm,
            VENUE,
            &key(),
            Duration::from_secs(86_400),
        )
        .await;
        assert!(ok.is_ok());
        for ttl in [Duration::from_secs(86_401), Duration::ZERO] {
            let e = create_read_delegation_with_wallet(
                &w,
                "base-sepolia",
                Architecture::Evm,
                VENUE,
                &key(),
                ttl,
            )
            .await;
            assert!(e.is_err(), "{ttl:?} must be refused");
        }
    }

    #[tokio::test]
    async fn bad_venue_id_refused() {
        for v in [
            "",
            "0123",
            "0123456789ABCDEF0123456789abcdef",
            "g123456789abcdef0123456789abcdef",
        ] {
            let e = create_at(
                &evm_wallet(),
                "base-sepolia",
                Architecture::Evm,
                v,
                &key(),
                60,
                NOW_S,
            )
            .await;
            assert!(e.is_err(), "{v:?}");
        }
    }

    #[test]
    fn request_has_fresh_timestamp_and_unique_16_byte_nonce() {
        let session = ReadSession::new(vec![delegation_stub()], key()).unwrap();
        let a = build_my_orders_request(&session, MyOrdersQuery::default()).unwrap();
        let b = build_my_orders_request(&session, MyOrdersQuery::default()).unwrap();
        let (qa, qb) = (a.query.unwrap(), b.query.unwrap());
        assert_eq!(qa.nonce.len(), 16);
        assert_ne!(qa.nonce, qb.nonce);
        let now = now_ms().unwrap();
        assert!(now.abs_diff(qa.timestamp_ms) < 5_000);
    }

    #[test]
    fn session_rejects_bad_delegation_sets() {
        assert!(ReadSession::new(vec![], key()).is_err());
        assert!(ReadSession::new(vec![delegation_stub(); 5], key()).is_err());
        assert!(ReadSession::new(vec![delegation_stub(); 4], key()).is_ok());
        let other = DelegateKey::from_bytes(&[0x43; 32]);
        assert!(ReadSession::new(vec![delegation_stub()], other).is_err());
    }

    fn chain(network: &str, architecture: &str) -> crate::commands::config::config_pb::Chain {
        crate::commands::config::config_pb::Chain {
            network: network.into(),
            architecture: architecture.into(),
            ..Default::default()
        }
    }

    fn venue_cfg(chains: Vec<crate::commands::config::config_pb::Chain>) -> GetConfigResponse {
        GetConfigResponse {
            config: Some(crate::commands::config::config_pb::Configuration {
                chains,
                venue_id: VENUE.into(),
                ..Default::default()
            }),
        }
    }

    /// One delegation per chain family a wallet can own, named for the first
    /// network of that family, all for the session's key.
    #[cfg(feature = "tron")]
    #[tokio::test]
    async fn session_opens_one_delegation_per_owned_family() {
        let cfg = venue_cfg(vec![
            chain("base-sepolia", "evm"),
            chain("arb-sepolia", "evm"),
            chain("solana-devnet", "solana"),
            chain("tron-nile", "tron"),
        ]);
        let evm = evm_wallet();
        let sol = solana_wallet();
        let both =
            open_read_session_with_wallets(&cfg, &[&evm, &sol], key(), Duration::from_secs(7_200))
                .await
                .unwrap();
        let got: Vec<(&str, &str)> = both
            .delegations()
            .iter()
            .map(|d| (d.owner_network.as_str(), d.owner.as_str()))
            .collect();
        let sol_addr = sol.address();
        assert_eq!(
            got,
            vec![
                ("base-sepolia", ANVIL_3_ADDR),
                ("solana-devnet", sol_addr.as_str()),
                ("tron-nile", ANVIL_3_ADDR_LOWER),
            ]
        );
        for d in both.delegations() {
            assert_eq!(d.delegate_pubkey, key().pubkey().to_vec());
            assert_eq!(d.expires - d.issued, 7_200);
        }

        // Only an EVM key: its EVM and Tron accounts, nothing for Solana.
        let evm_only =
            open_read_session_with_wallets(&cfg, &[&evm], key(), Duration::from_secs(600))
                .await
                .unwrap();
        let nets: Vec<&str> = evm_only
            .delegations()
            .iter()
            .map(|d| d.owner_network.as_str())
            .collect();
        assert_eq!(nets, vec!["base-sepolia", "tron-nile"]);

        // No wallet owns anything here: refused, not an empty session.
        let sol_cfg = venue_cfg(vec![chain("solana-devnet", "solana")]);
        let e = open_read_session_with_wallets(&sol_cfg, &[&evm], key(), Duration::from_secs(600))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("no wallet"), "{e}");
    }

    #[test]
    fn my_order_json_names_states_in_snake_case() {
        use super::super::arborter_pb::MyOrderState as S;
        let cases = [
            (S::Unspecified, "unspecified"),
            (S::Open, "open"),
            (S::PartiallyFilled, "partially_filled"),
            (S::Filled, "filled"),
            (S::Cancelled, "cancelled"),
            (S::Expired, "expired"),
            (S::DroppedByRestart, "dropped_by_restart"),
            (S::Traded, "traded"),
        ];
        for (state, name) in cases {
            assert_eq!(my_order_state_name(state as i32), name);
        }
        assert_eq!(my_order_state_name(99), "unspecified");
    }

    #[test]
    fn my_order_json_carries_every_field() {
        use super::super::arborter_pb::{FillRole, MyFill, MyOrderKind, MyOrderState, Side};
        let o = MyOrder {
            order_id: vec![0xab; 32],
            found: true,
            market_id: "m-1".into(),
            side: Side::Ask as i32,
            kind: MyOrderKind::Limit as i32,
            is_hidden: true,
            is_post_only: false,
            price: "2500".into(),
            initial_qty_known: true,
            initial_qty: "1200".into(),
            filled_qty: "450".into(),
            remaining_qty: "750".into(),
            state: MyOrderState::Cancelled as i32,
            created_at_ms: 1_000,
            closed_at_ms: 2_000,
            base_address: "0xbase".into(),
            quote_address: "0xquote".into(),
            fills: vec![MyFill {
                fill_id: vec![0xcd; 32],
                price: "2490".into(),
                qty: "450".into(),
                match_ts_ms: 1_500,
                role: FillRole::Taker as i32,
            }],
        };
        let j = my_order_json(&o);
        assert_eq!(j["orderId"], format!("0x{}", "ab".repeat(32)));
        assert_eq!(j["found"], true);
        assert_eq!(j["side"], "sell");
        assert_eq!(j["kind"], "limit");
        assert_eq!(j["hidden"], true);
        assert_eq!(j["state"], "cancelled");
        assert_eq!(j["initialQty"], "1200");
        assert_eq!(j["filledQty"], "450");
        assert_eq!(j["remainingQty"], "750");
        assert_eq!(j["closedAtMs"], 2_000);
        assert_eq!(j["fills"][0]["role"], "taker");
        assert_eq!(j["fills"][0]["price"], "2490");
        assert_eq!(j["fills"][0]["fillId"], format!("0x{}", "cd".repeat(32)));

        // Not found: the id and `found: false`, nothing else.
        let nf = my_order_json(&MyOrder {
            order_id: vec![0x01; 32],
            ..Default::default()
        });
        assert_eq!(
            nf,
            serde_json::json!({ "orderId": format!("0x{}", "01".repeat(32)), "found": false })
        );
    }

    fn ids(n: usize) -> Vec<[u8; 32]> {
        (0..n)
            .map(|i| {
                let mut id = [0u8; 32];
                id[..8].copy_from_slice(&(i as u64 + 1).to_le_bytes());
                id
            })
            .collect()
    }

    fn found(id: &[u8]) -> MyOrder {
        MyOrder {
            order_id: id.to_vec(),
            found: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn ids_chunked_at_100() {
        let session = ReadSession::new(vec![delegation_stub()], key()).unwrap();
        let all = ids(250);
        let sizes = Mutex::new(Vec::new());
        let nonces = Mutex::new(Vec::new());
        let out = get_orders_via(&session, &all, |req| {
            let q = req.query.clone().unwrap();
            sizes.lock().unwrap().push(q.order_ids.len());
            nonces.lock().unwrap().push(q.nonce.clone());
            // Answer by id, in REVERSE order: results must merge by id, not position.
            let orders = q.order_ids.iter().rev().map(|i| found(i)).collect();
            async move {
                Ok(MyOrdersResponse {
                    orders,
                    next_cursor: vec![],
                })
            }
        })
        .await
        .unwrap();
        assert_eq!(*sizes.lock().unwrap(), vec![100, 100, 50]);
        let n = nonces.lock().unwrap();
        assert!(
            n[0] != n[1] && n[1] != n[2] && n[0] != n[2],
            "nonce per chunk"
        );
        let got: Vec<Vec<u8>> = out.iter().map(|o| o.order_id.clone()).collect();
        let want: Vec<Vec<u8>> = all.iter().map(|i| i.to_vec()).collect();
        assert_eq!(got, want, "merged back into request order, by id");
    }

    #[tokio::test]
    async fn get_with_no_ids_or_duplicates() {
        let session = ReadSession::new(vec![delegation_stub()], key()).unwrap();
        let none: Vec<[u8; 32]> = vec![];
        let e = get_orders_via(&session, &none, |_| async {
            Ok(MyOrdersResponse::default())
        })
        .await;
        assert!(e.is_err());
        // Duplicates are sent once.
        let one = ids(1);
        let dup = vec![one[0], one[0]];
        let sent = Mutex::new(0usize);
        let out = get_orders_via(&session, &dup, |req| {
            *sent.lock().unwrap() += req.query.as_ref().unwrap().order_ids.len();
            let orders = req
                .query
                .unwrap()
                .order_ids
                .iter()
                .map(|i| found(i))
                .collect();
            async move {
                Ok(MyOrdersResponse {
                    orders,
                    next_cursor: vec![],
                })
            }
        })
        .await
        .unwrap();
        assert_eq!((*sent.lock().unwrap(), out.len()), (1, 1));
    }

    #[tokio::test]
    async fn list_pages_to_completion() {
        let session = ReadSession::new(vec![delegation_stub()], key()).unwrap();
        let cursors = Mutex::new(Vec::new());
        let out = list_orders_via(&session, Some("m-1"), true, |req| {
            let q = req.query.unwrap();
            assert_eq!((q.market_id.as_str(), q.include_closed), ("m-1", true));
            assert!(q.order_ids.is_empty());
            cursors.lock().unwrap().push(q.cursor.clone());
            let (orders, next) = match q.cursor.as_slice() {
                b"" => (vec![found(&[1; 32])], b"p2".to_vec()),
                b"p2" => (vec![found(&[2; 32]), found(&[3; 32])], b"p3".to_vec()),
                _ => (vec![found(&[4; 32])], vec![]),
            };
            async move {
                Ok(MyOrdersResponse {
                    orders,
                    next_cursor: next,
                })
            }
        })
        .await
        .unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(
            *cursors.lock().unwrap(),
            vec![b"".to_vec(), b"p2".to_vec(), b"p3".to_vec()]
        );
    }

    #[tokio::test]
    async fn list_stuck_cursor_is_an_error() {
        let session = ReadSession::new(vec![delegation_stub()], key()).unwrap();
        let e = list_orders_via(&session, None, false, |_| async {
            Ok(MyOrdersResponse {
                orders: vec![],
                next_cursor: b"same".to_vec(),
            })
        })
        .await;
        assert!(e.is_err());
    }

    fn arch_of(network: &str) -> eyre::Result<Architecture> {
        match network {
            "base-sepolia" => Ok(Architecture::Evm),
            "tron-nile" => Ok(Architecture::Tron),
            "solana-devnet" => Ok(Architecture::Solana),
            n => Err(eyre::eyre!("unknown network {n}")),
        }
    }

    #[tokio::test]
    async fn redelegates_under_5_min() {
        let evm = evm_wallet();
        let sol = solana_wallet();
        let fresh = create_at(
            &evm,
            "base-sepolia",
            Architecture::Evm,
            VENUE,
            &key(),
            3600,
            NOW_S,
        )
        .await
        .unwrap();
        // 299 s left: under the threshold.
        let stale_sol = create_at(
            &sol,
            "solana-devnet",
            Architecture::Solana,
            VENUE,
            &key(),
            3600,
            NOW_S - 3600 + 299,
        )
        .await
        .unwrap();
        let mut session = ReadSession::new(vec![fresh.clone(), stale_sol.clone()], key()).unwrap();
        let n = refresh_delegations(&mut session, &[&evm, &sol], VENUE, arch_of, NOW_S)
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(session.delegations[0], fresh, "300 s+ left is left alone");
        let d = &session.delegations[1];
        assert_eq!(
            (d.issued, d.owner.as_str()),
            (NOW_S, stale_sol.owner.as_str())
        );
        assert!(d.expires - NOW_S >= 600 && d.expires - NOW_S <= 86_400);
        assert_ne!(d.signature, stale_sol.signature);

        // Exactly 300 s left is not "less than 5 minutes".
        let edge = create_at(
            &evm,
            "base-sepolia",
            Architecture::Evm,
            VENUE,
            &key(),
            3600,
            NOW_S - 3600 + 300,
        )
        .await
        .unwrap();
        let mut s2 = ReadSession::new(vec![edge.clone()], key()).unwrap();
        assert_eq!(
            refresh_delegations(&mut s2, &[&evm], VENUE, arch_of, NOW_S)
                .await
                .unwrap(),
            0
        );
        assert_eq!(s2.delegations[0], edge);
    }

    #[tokio::test]
    async fn no_wallet_for_a_delegation_leaves_it_alone() {
        let evm = evm_wallet();
        let stale = create_at(
            &evm,
            "base-sepolia",
            Architecture::Evm,
            VENUE,
            &key(),
            3600,
            NOW_S - 3590,
        )
        .await
        .unwrap();
        let mut session = ReadSession::new(vec![stale.clone()], key()).unwrap();
        let sol = solana_wallet();
        let n = refresh_delegations(&mut session, &[&sol], VENUE, arch_of, NOW_S)
            .await
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(session.delegations[0], stale);
    }

    #[tokio::test]
    async fn redelegation_matches_tron_owner_case_insensitively() {
        let evm = evm_wallet();
        let stale = create_at(
            &evm,
            "tron-nile",
            Architecture::Tron,
            VENUE,
            &key(),
            3600,
            NOW_S - 3590,
        )
        .await
        .unwrap();
        let mut session = ReadSession::new(vec![stale], key()).unwrap();
        let n = refresh_delegations(&mut session, &[&evm], VENUE, arch_of, NOW_S)
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(session.delegations[0].owner, ANVIL_3_ADDR_LOWER);
    }
}
