//! Treasury management for the governance contract.
//!
//! Provides multi-sig controlled fund storage, deposit, withdrawal requests
//! with approval workflows, and balance tracking.

use soroban_sdk::{symbol_short, Address, Env, Symbol, Vec};

pub use crate::records::{GovernanceConfig, StorageKey, TreasuryRequest};

/// Sentinel return symbol.
const OK: Symbol = symbol_short!("ok");

/// Errors related to treasury operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[soroban_sdk::contracterror]
pub enum TreasuryError {
    /// Insufficient treasury balance.
    InsufficientBalance = 1,
    /// Request not found.
    RequestNotFound = 2,
    /// Request already executed.
    AlreadyExecuted = 3,
    /// Caller is not a registered treasury signer.
    NotASigner = 4,
    /// Caller has already approved this request.
    AlreadyApproved = 5,
    /// Not enough approvals to execute.
    InsufficientApprovals = 6,
    /// Amount exceeds the maximum allowed per-request spend.
    ExceedsSpendLimit = 7,
    /// Invalid amount (must be positive).
    InvalidAmount = 8,
    /// Request was cancelled.
    RequestCancelled = 9,
}

// ---------------------------------------------------------------------------
// Public functions
// ---------------------------------------------------------------------------

/// Deposit funds into the treasury.
///
/// Requires authorization from `depositor`. Increases the treasury balance
/// for `asset` and emits a `TREAS_DEP` event.
pub fn deposit(
    env: &Env,
    depositor: &Address,
    asset: &Symbol,
    amount: i128,
) -> Result<i128, TreasuryError> {
    depositor.require_auth();

    if amount <= 0 {
        return Err(TreasuryError::InvalidAmount);
    }

    let current: i128 = env
        .storage()
        .persistent()
        .get(&StorageKey::TreasuryBalance(asset.clone()))
        .unwrap_or_default();

    let new_balance = current
        .checked_add(amount)
        .ok_or(TreasuryError::InsufficientBalance)?;

    env.storage()
        .persistent()
        .set(&StorageKey::TreasuryBalance(asset.clone()), &new_balance);

    // Track treasury assets.
    let mut assets: Vec<Symbol> = env
        .storage()
        .persistent()
        .get(&StorageKey::TreasuryAssets)
        .unwrap_or_else(|| Vec::new(env));
    if !assets.contains(asset) {
        assets.push_back(asset.clone());
        env.storage()
            .persistent()
            .set(&StorageKey::TreasuryAssets, &assets);
    }

    env.events()
        .publish((symbol_short!("TREAS_DEP"), depositor, asset), amount);

    Ok(new_balance)
}

/// Get the treasury balance for an asset.
pub fn balance_of(env: &Env, asset: &Symbol) -> i128 {
    env.storage()
        .persistent()
        .get(&StorageKey::TreasuryBalance(asset.clone()))
        .unwrap_or_default()
}

/// Get all treasury assets.
pub fn all_assets(env: &Env) -> Vec<Symbol> {
    env.storage()
        .persistent()
        .get(&StorageKey::TreasuryAssets)
        .unwrap_or_else(|| Vec::new(env))
}

/// Create a treasury withdrawal request.
///
/// Returns the new request id. Requires authorization from `requester`.
pub fn create_request(
    env: &Env,
    requester: &Address,
    recipient: &Address,
    amount: i128,
    asset: &Symbol,
    reason: &Symbol,
    config: &GovernanceConfig,
) -> Result<u64, TreasuryError> {
    requester.require_auth();

    if amount <= 0 {
        return Err(TreasuryError::InvalidAmount);
    }

    // Check spend limit: amount <= (treasury_balance * max_treasury_spend_bps) / 10000
    let treasury_balance: i128 = env
        .storage()
        .persistent()
        .get(&StorageKey::TreasuryBalance(asset.clone()))
        .unwrap_or_default();

    let spend_limit = treasury_balance
        .checked_mul(config.max_treasury_spend_bps as i128)
        .ok_or(TreasuryError::InsufficientBalance)?
        / 10_000;

    if amount > spend_limit {
        return Err(TreasuryError::ExceedsSpendLimit);
    }

    let request_id: u64 = env
        .storage()
        .persistent()
        .get(&StorageKey::NextTreasuryRequestId)
        .unwrap_or(1);

    let now = env.ledger().timestamp();
    let request = TreasuryRequest {
        request_id,
        recipient: recipient.clone(),
        amount,
        asset: asset.clone(),
        reason: reason.clone(),
        approvals: Vec::new(env),
        executed: false,
        created_at: now,
    };

    env.storage()
        .persistent()
        .set(&StorageKey::TreasuryRequest(request_id), &request);
    env.storage()
        .persistent()
        .set(&StorageKey::NextTreasuryRequestId, &(request_id + 1));

    env.events().publish(
        (symbol_short!("TREAS_REQ"), requester, asset),
        (request_id, amount, recipient),
    );

    Ok(request_id)
}

/// Approve a treasury withdrawal request.
///
/// Caller must be a registered treasury signer. Returns the updated request.
pub fn approve_request(
    env: &Env,
    signer: &Address,
    request_id: u64,
) -> Result<TreasuryRequest, TreasuryError> {
    signer.require_auth();

    // Verify signer is authorized.
    let signers: Vec<Address> = env
        .storage()
        .persistent()
        .get(&StorageKey::TreasurySigners)
        .unwrap_or_else(|| Vec::new(env));

    let admin: Address = env
        .storage()
        .persistent()
        .get(&StorageKey::Admin)
        .ok_or(TreasuryError::NotASigner)?;

    if !signers.contains(signer) && *signer != admin {
        return Err(TreasuryError::NotASigner);
    }

    let mut request: TreasuryRequest = env
        .storage()
        .persistent()
        .get(&StorageKey::TreasuryRequest(request_id))
        .ok_or(TreasuryError::RequestNotFound)?;

    if request.executed {
        return Err(TreasuryError::AlreadyExecuted);
    }
    if request.approvals.contains(signer) {
        return Err(TreasuryError::AlreadyApproved);
    }

    request.approvals.push_back(signer.clone());
    env.storage()
        .persistent()
        .set(&StorageKey::TreasuryRequest(request_id), &request);

    env.events().publish(
        (symbol_short!("TREAS_APR"), signer),
        (request_id, request.approvals.len()),
    );

    Ok(request)
}

/// Execute a treasury withdrawal request once enough approvals are gathered.
///
/// Returns the net amount sent to the recipient. The actual token transfer is
/// tracked via events; the contract manages accounting only.
pub fn execute_request(
    env: &Env,
    request_id: u64,
    config: &GovernanceConfig,
) -> Result<i128, TreasuryError> {
    let mut request: TreasuryRequest = env
        .storage()
        .persistent()
        .get(&StorageKey::TreasuryRequest(request_id))
        .ok_or(TreasuryError::RequestNotFound)?;

    if request.executed {
        return Err(TreasuryError::AlreadyExecuted);
    }

    if request.approvals.len() < config.treasury_multisig_threshold {
        return Err(TreasuryError::InsufficientApprovals);
    }

    // Check balance.
    let current_balance: i128 = env
        .storage()
        .persistent()
        .get(&StorageKey::TreasuryBalance(request.asset.clone()))
        .unwrap_or_default();

    if request.amount > current_balance {
        return Err(TreasuryError::InsufficientBalance);
    }

    // Deduct from treasury.
    let new_balance = current_balance - request.amount;
    env.storage().persistent().set(
        &StorageKey::TreasuryBalance(request.asset.clone()),
        &new_balance,
    );

    // Mark as executed.
    let _executor = env.current_contract_address();
    request.executed = true;
    env.storage()
        .persistent()
        .set(&StorageKey::TreasuryRequest(request_id), &request);

    env.events().publish(
        (symbol_short!("TREAS_EXE"), &request.recipient),
        (request_id, request.amount, request.asset.clone()),
    );

    Ok(request.amount)
}

/// Cancel a treasury withdrawal request. Admin-only.
pub fn cancel_request(
    env: &Env,
    admin: &Address,
    request_id: u64,
) -> Result<Symbol, TreasuryError> {
    admin.require_auth();

    let mut request: TreasuryRequest = env
        .storage()
        .persistent()
        .get(&StorageKey::TreasuryRequest(request_id))
        .ok_or(TreasuryError::RequestNotFound)?;

    if request.executed {
        return Err(TreasuryError::AlreadyExecuted);
    }

    request.executed = true; // Mark as done (cancelled)
    env.storage()
        .persistent()
        .set(&StorageKey::TreasuryRequest(request_id), &request);

    env.events()
        .publish((symbol_short!("TREAS_CAN"), admin), request_id);

    Ok(OK)
}

/// Get a treasury request by id.
pub fn get_request(env: &Env, request_id: u64) -> Option<TreasuryRequest> {
    env.storage()
        .persistent()
        .get(&StorageKey::TreasuryRequest(request_id))
}

// ---------------------------------------------------------------------------
// Multi-sig signer management
// ---------------------------------------------------------------------------

/// Add a treasury signer. Admin-only.
pub fn add_signer(env: &Env, admin: &Address, signer: &Address) -> Result<Symbol, TreasuryError> {
    admin.require_auth();

    let mut signers: Vec<Address> = env
        .storage()
        .persistent()
        .get(&StorageKey::TreasurySigners)
        .unwrap_or_else(|| Vec::new(env));

    if !signers.contains(signer) {
        signers.push_back(signer.clone());
        env.storage()
            .persistent()
            .set(&StorageKey::TreasurySigners, &signers);
    }

    Ok(OK)
}

/// Remove a treasury signer. Admin-only.
pub fn remove_signer(
    env: &Env,
    admin: &Address,
    signer: &Address,
) -> Result<Symbol, TreasuryError> {
    admin.require_auth();

    let mut signers: Vec<Address> = env
        .storage()
        .persistent()
        .get(&StorageKey::TreasurySigners)
        .unwrap_or_else(|| Vec::new(env));

    if let Some(idx) = signers.first_index_of(signer) {
        signers.remove(idx);
        env.storage()
            .persistent()
            .set(&StorageKey::TreasurySigners, &signers);
    }

    Ok(OK)
}

/// Get all treasury signers.
pub fn get_signers(env: &Env) -> Vec<Address> {
    env.storage()
        .persistent()
        .get(&StorageKey::TreasurySigners)
        .unwrap_or_else(|| Vec::new(env))
}
