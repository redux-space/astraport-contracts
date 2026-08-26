#![cfg_attr(not(test), no_std)]
//! # AstraPort Fee Management Contract
//!
//! Flexible fee system supporting multiple fee models (flat, percentage, tiered)
//! with transparent accounting, revenue distribution to stakeholders, and
//! comprehensive reporting.

use soroban_sdk::{contract, contractimpl, symbol_short, Address, Env, Symbol};

pub mod engine;
pub mod records;
pub mod reporting;

use crate::engine::{
    apply_discount, apply_fee_cap, clamp_fee, compute_fee_from_structure, validate_fee_structure,
};
use crate::records::*;

// ---------------------------------------------------------------------------
// Storage helpers
// ---------------------------------------------------------------------------

fn get_admin(env: &Env) -> Address {
    env.storage().persistent().get(&FeeDataKey::Admin).unwrap()
}

fn put_admin(env: &Env, admin: &Address) {
    env.storage().persistent().set(&FeeDataKey::Admin, admin);
}

fn put_fee_structure(env: &Env, fs: &FeeStructure) {
    let key = FeeDataKey::FeeStructure(fs.fee_id.clone());
    env.storage().persistent().set(&key, fs);
}

fn get_fee_structure(env: &Env, fee_id: &Symbol) -> Option<FeeStructure> {
    let key = FeeDataKey::FeeStructure(fee_id.clone());
    env.storage().persistent().get(&key)
}

fn add_fee_id(env: &Env, fee_id: &Symbol) {
    let mut list: soroban_sdk::Vec<Symbol> = env
        .storage()
        .persistent()
        .get(&FeeDataKey::FeeIds)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env));
    for existing in list.iter() {
        if existing == *fee_id {
            return;
        }
    }
    list.push_back(fee_id.clone());
    env.storage().persistent().set(&FeeDataKey::FeeIds, &list);
}

fn list_all_fee_ids(env: &Env) -> soroban_sdk::Vec<Symbol> {
    env.storage()
        .persistent()
        .get(&FeeDataKey::FeeIds)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

fn get_portfolio_fee_id(env: &Env, pid: &Symbol) -> Option<Symbol> {
    let key = FeeDataKey::PortfolioFee(pid.clone());
    env.storage().persistent().get(&key)
}

fn set_portfolio_fee_id(env: &Env, pid: &Symbol, fid: &Symbol) {
    let key = FeeDataKey::PortfolioFee(pid.clone());
    env.storage().persistent().set(&key, fid);
}

fn remove_portfolio_fee_id(env: &Env, pid: &Symbol) {
    let key = FeeDataKey::PortfolioFee(pid.clone());
    env.storage().persistent().remove(&key);
}

fn get_fee_history(env: &Env) -> soroban_sdk::Vec<FeeRecord> {
    env.storage()
        .persistent()
        .get(&FeeDataKey::FeeHistory)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

fn append_fee_record(env: &Env, record: &FeeRecord) {
    let mut h = get_fee_history(env);
    if h.len() >= MAX_HISTORY {
        h = h.slice(1..);
    }
    h.push_back(record.clone());
    env.storage().persistent().set(&FeeDataKey::FeeHistory, &h);
}

fn get_fee_waivers(env: &Env) -> soroban_sdk::Vec<FeeWaiver> {
    env.storage()
        .persistent()
        .get(&FeeDataKey::FeeWaivers)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

fn put_fee_waivers(env: &Env, w: &soroban_sdk::Vec<FeeWaiver>) {
    env.storage().persistent().set(&FeeDataKey::FeeWaivers, w);
}

fn get_revenue_recipients(env: &Env) -> soroban_sdk::Vec<RevenueRecipient> {
    env.storage()
        .persistent()
        .get(&FeeDataKey::RevenueRecipients)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

fn put_revenue_recipients(env: &Env, r: &soroban_sdk::Vec<RevenueRecipient>) {
    env.storage()
        .persistent()
        .set(&FeeDataKey::RevenueRecipients, r);
}

fn add_to_total_collected(env: &Env, amount: i128) {
    let cur: i128 = env
        .storage()
        .persistent()
        .get(&FeeDataKey::TotalCollected)
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&FeeDataKey::TotalCollected, &(cur + amount));
}

fn add_to_category_total(env: &Env, category: &FeeCategory, amount: i128) {
    let key = FeeDataKey::CategoryTotal(*category);
    let cur: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    env.storage().persistent().set(&key, &(cur + amount));
}

fn add_to_portfolio_total(env: &Env, portfolio_id: &Symbol, amount: i128) {
    let key = FeeDataKey::PortfolioTotal(portfolio_id.clone());
    let cur: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    env.storage().persistent().set(&key, &(cur + amount));
}

// ---------------------------------------------------------------------------
// Waiver matching
// ---------------------------------------------------------------------------

fn waiver_is_active(env: &Env, w: &FeeWaiver) -> bool {
    if w.expires_at == 0 {
        return true;
    }
    env.ledger().timestamp() < w.expires_at
}

fn waiver_same_target(a: &FeeWaiver, b: &FeeWaiver) -> bool {
    if a.has_address != b.has_address {
        return false;
    }
    if a.has_address && a.address != b.address {
        return false;
    }
    if a.has_portfolio != b.has_portfolio {
        return false;
    }
    if a.has_portfolio && a.portfolio_id != b.portfolio_id {
        return false;
    }
    true
}

fn resolve_waiver_for_portfolio(env: &Env, pid: &Symbol) -> (i128, bool) {
    for w in get_fee_waivers(env).iter() {
        if !waiver_is_active(env, &w) {
            continue;
        }
        if w.has_portfolio && w.portfolio_id == *pid {
            return (w.discount_bps, w.waived);
        }
    }
    (0, false)
}

fn resolve_waiver_for_collect(env: &Env, addr: &Address, pid: &Symbol) -> (i128, bool) {
    for w in get_fee_waivers(env).iter() {
        if !waiver_is_active(env, &w) {
            continue;
        }
        if w.has_address && w.address == *addr {
            return (w.discount_bps, w.waived);
        }
        if w.has_portfolio && w.portfolio_id == *pid {
            return (w.discount_bps, w.waived);
        }
    }
    (0, false)
}

// ---------------------------------------------------------------------------
// Revenue distribution
// ---------------------------------------------------------------------------

fn distribute_revenue(env: &Env, amount: i128) -> soroban_sdk::Vec<(Address, i128)> {
    let recips = get_revenue_recipients(env);
    let mut r = soroban_sdk::Vec::new(env);
    if recips.is_empty() || amount <= 0 {
        return r;
    }
    let mut total_shares: i128 = 0;
    for rp in recips.iter() {
        total_shares += rp.share_numerator as i128;
    }
    if total_shares == 0 {
        return r;
    }
    let mut distributed: i128 = 0;
    for rp in recips.iter() {
        let share = (rp.share_numerator as i128)
            .checked_mul(amount)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(env, FeeError::ArithmeticOverflow))
            / total_shares;
        distributed += share;
        r.push_back((rp.address.clone(), share));
    }
    let remainder = amount - distributed;
    if remainder > 0 && !r.is_empty() {
        let (first_addr, first_share) = r.get(0).unwrap();
        r.set(0, (first_addr, first_share + remainder));
    }
    r
}

// ===========================================================================
// Contract
// ===========================================================================

#[contract]
pub struct FeeManagementContract;

#[contractimpl]
impl FeeManagementContract {
    pub fn initialize(env: Env, admin: Address) -> Result<Symbol, FeeError> {
        let storage = env.storage().persistent();
        if storage.has(&FeeDataKey::Admin) {
            return Err(FeeError::AlreadyInitialized);
        }
        put_admin(&env, &admin);
        Ok(symbol_short!("ok"))
    }

    pub fn get_admin(env: Env) -> Address {
        get_admin(&env)
    }

    pub fn transfer_admin(env: Env, new_admin: Address) -> Symbol {
        get_admin(&env).require_auth();
        put_admin(&env, &new_admin);
        symbol_short!("ok")
    }

    pub fn set_fee_structure(
        env: Env,
        fee_id: Symbol,
        fee_type: FeeType,
        amount_bps: i128,
        tiered_entries: soroban_sdk::Vec<TierEntry>,
        category: FeeCategory,
        active: bool,
        fee_cap: Option<i128>,
    ) -> Symbol {
        get_admin(&env).require_auth();
        let fs = FeeStructure {
            fee_id: fee_id.clone(),
            fee_type,
            amount_bps,
            tiered_entries,
            category,
            active,
            fee_cap,
        };
        validate_fee_structure(&fs).unwrap_or_else(|e| soroban_sdk::panic_with_error!(&env, e));
        put_fee_structure(&env, &fs);
        add_fee_id(&env, &fee_id);
        symbol_short!("ok")
    }

    pub fn set_fee_structure_simple(
        env: Env,
        fee_id: Symbol,
        fee_type: FeeType,
        amount_bps: i128,
        tiered_entries: soroban_sdk::Vec<TierEntry>,
        active: bool,
    ) -> Symbol {
        Self::set_fee_structure(
            env,
            fee_id,
            fee_type,
            amount_bps,
            tiered_entries,
            FeeCategory::Custom,
            active,
            None,
        )
    }

    pub fn get_fee_structure(env: Env, fee_id: Symbol) -> Option<FeeStructure> {
        get_fee_structure(&env, &fee_id)
    }

    pub fn list_fee_structures(env: Env) -> soroban_sdk::Vec<Symbol> {
        list_all_fee_ids(&env)
    }

    pub fn set_fee_active(env: Env, fee_id: Symbol, active: bool) -> Symbol {
        get_admin(&env).require_auth();
        let mut fs = get_fee_structure(&env, &fee_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, FeeError::FeeNotFound));
        fs.active = active;
        put_fee_structure(&env, &fs);
        symbol_short!("ok")
    }

    pub fn set_fee_cap(env: Env, fee_id: Symbol, cap: Option<i128>) -> Symbol {
        get_admin(&env).require_auth();
        let mut fs = get_fee_structure(&env, &fee_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, FeeError::FeeNotFound));
        fs.fee_cap = cap;
        put_fee_structure(&env, &fs);
        symbol_short!("ok")
    }

    pub fn set_portfolio_fee(env: Env, portfolio_id: Symbol, fee_id: Symbol) -> Symbol {
        get_admin(&env).require_auth();
        let _ = get_fee_structure(&env, &fee_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, FeeError::FeeNotFound));
        set_portfolio_fee_id(&env, &portfolio_id, &fee_id);
        symbol_short!("ok")
    }

    pub fn get_portfolio_fee(env: Env, portfolio_id: Symbol) -> Option<Symbol> {
        get_portfolio_fee_id(&env, &portfolio_id)
    }

    pub fn remove_portfolio_fee(env: Env, portfolio_id: Symbol) -> Symbol {
        get_admin(&env).require_auth();
        remove_portfolio_fee_id(&env, &portfolio_id);
        symbol_short!("ok")
    }

    pub fn calculate_fee(env: Env, fee_id: Symbol, amount: i128) -> FeeCalculationResult {
        let fs = get_fee_structure(&env, &fee_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, FeeError::FeeNotFound));
        if !fs.active {
            soroban_sdk::panic_with_error!(&env, FeeError::FeeInactive);
        }
        let raw = compute_fee_from_structure(&env, &fs, amount);
        let clamped = clamp_fee(raw, amount);
        let capped = apply_fee_cap(clamped, fs.fee_cap);
        FeeCalculationResult {
            fee_id,
            category: fs.category,
            gross_amount: amount,
            raw_fee: raw,
            discount_bps: 0,
            fee_cap: fs.fee_cap,
            fee_amount: capped,
            waived: false,
        }
    }

    pub fn calculate_portfolio_fee(
        env: Env,
        portfolio_id: Symbol,
        fallback_fee_id: Symbol,
        amount: i128,
    ) -> FeeCalculationResult {
        let fee_id = get_portfolio_fee_id(&env, &portfolio_id).unwrap_or(fallback_fee_id);
        let fs = get_fee_structure(&env, &fee_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, FeeError::FeeNotFound));
        if !fs.active {
            soroban_sdk::panic_with_error!(&env, FeeError::FeeInactive);
        }
        let raw = compute_fee_from_structure(&env, &fs, amount);
        let clamped = clamp_fee(raw, amount);
        let capped = apply_fee_cap(clamped, fs.fee_cap);
        let (discount_bps, waived) = resolve_waiver_for_portfolio(&env, &portfolio_id);
        let final_fee = apply_discount(&env, capped, discount_bps, waived);
        FeeCalculationResult {
            fee_id,
            category: fs.category,
            gross_amount: amount,
            raw_fee: raw,
            discount_bps,
            fee_cap: fs.fee_cap,
            fee_amount: final_fee,
            waived,
        }
    }

    pub fn estimate_fee(
        env: Env,
        fee_id: Symbol,
        portfolio_id: Option<Symbol>,
        amount: i128,
    ) -> FeeCalculationResult {
        let resolved_id = match &portfolio_id {
            Some(p) => get_portfolio_fee_id(&env, p).unwrap_or(fee_id),
            None => fee_id,
        };
        let fs = get_fee_structure(&env, &resolved_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, FeeError::FeeNotFound));
        if !fs.active {
            soroban_sdk::panic_with_error!(&env, FeeError::FeeInactive);
        }
        let raw = compute_fee_from_structure(&env, &fs, amount);
        let clamped = clamp_fee(raw, amount);
        let capped = apply_fee_cap(clamped, fs.fee_cap);
        let (discount_bps, waived) = match &portfolio_id {
            Some(p) => resolve_waiver_for_portfolio(&env, p),
            None => (0, false),
        };
        let final_fee = apply_discount(&env, capped, discount_bps, waived);
        FeeCalculationResult {
            fee_id: resolved_id,
            category: fs.category,
            gross_amount: amount,
            raw_fee: raw,
            discount_bps,
            fee_cap: fs.fee_cap,
            fee_amount: final_fee,
            waived,
        }
    }

    pub fn collect_fee(
        env: Env,
        caller: Address,
        fee_id: Symbol,
        portfolio_id: Symbol,
        base_amount: i128,
    ) -> i128 {
        caller.require_auth();
        let fs = get_fee_structure(&env, &fee_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, FeeError::FeeNotFound));
        if !fs.active {
            soroban_sdk::panic_with_error!(&env, FeeError::FeeInactive);
        }
        let raw = compute_fee_from_structure(&env, &fs, base_amount);
        let clamped = clamp_fee(raw, base_amount);
        let capped = apply_fee_cap(clamped, fs.fee_cap);
        let (discount_bps, waived) = resolve_waiver_for_collect(&env, &caller, &portfolio_id);
        let final_fee = apply_discount(&env, capped, discount_bps, waived);
        append_fee_record(
            &env,
            &FeeRecord {
                fee_id: fee_id.clone(),
                category: fs.category,
                portfolio_id: portfolio_id.clone(),
                base_amount,
                fee_amount: final_fee,
                discount_bps,
                waived,
                timestamp: env.ledger().timestamp(),
                collector: caller,
            },
        );
        add_to_total_collected(&env, final_fee);
        add_to_category_total(&env, &fs.category, final_fee);
        add_to_portfolio_total(&env, &portfolio_id, final_fee);
        distribute_revenue(&env, final_fee);
        final_fee
    }

    pub fn collect_yield_fee(
        env: Env,
        caller: Address,
        portfolio_id: Symbol,
        yield_amount: i128,
    ) -> i128 {
        let fee_id = symbol_short!("YIELD");
        let _ = get_fee_structure(&env, &fee_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, FeeError::FeeNotFound));
        Self::collect_fee(env, caller, fee_id, portfolio_id, yield_amount)
    }

    pub fn collect_management_fee(
        env: Env,
        caller: Address,
        portfolio_id: Symbol,
        aum: i128,
    ) -> i128 {
        let fee_id = symbol_short!("MGMT");
        let _ = get_fee_structure(&env, &fee_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, FeeError::FeeNotFound));
        Self::collect_fee(env, caller, fee_id, portfolio_id, aum)
    }

    pub fn collect_rebalance_fee(
        env: Env,
        caller: Address,
        portfolio_id: Symbol,
        trade_amount: i128,
    ) -> i128 {
        let fee_id = symbol_short!("REBAL");
        let _ = get_fee_structure(&env, &fee_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, FeeError::FeeNotFound));
        Self::collect_fee(env, caller, fee_id, portfolio_id, trade_amount)
    }

    pub fn collect_trading_fee(
        env: Env,
        caller: Address,
        portfolio_id: Symbol,
        trade_amount: i128,
    ) -> i128 {
        let fee_id = symbol_short!("TRADE");
        let _ = get_fee_structure(&env, &fee_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, FeeError::FeeNotFound));
        Self::collect_fee(env, caller, fee_id, portfolio_id, trade_amount)
    }

    pub fn set_fee_waiver(
        env: Env,
        address: Option<Address>,
        portfolio_id: Option<Symbol>,
        discount_bps: i128,
        waived: bool,
        label: Option<Symbol>,
        expires_at: u64,
    ) -> Symbol {
        get_admin(&env).require_auth();
        if !(0..=BPS_DENOM).contains(&discount_bps) {
            soroban_sdk::panic_with_error!(&env, FeeError::InvalidFeeConfiguration);
        }
        let has_address = address.is_some();
        let has_portfolio = portfolio_id.is_some();
        let addr = match address {
            Some(a) => a,
            None => get_admin(&env),
        };
        let pid = match portfolio_id {
            Some(p) => p,
            None => sym_empty(),
        };
        let lbl = label.unwrap_or(sym_empty());
        let w = FeeWaiver {
            address: addr,
            has_address,
            portfolio_id: pid,
            has_portfolio,
            discount_bps,
            waived,
            label: lbl,
            expires_at,
        };
        let mut waivers = get_fee_waivers(&env);
        let mut found = false;
        let mut idx: u32 = 0;
        while idx < waivers.len() {
            let e = waivers.get(idx).unwrap();
            if waiver_same_target(&e, &w) {
                waivers.set(idx, w.clone());
                found = true;
                break;
            }
            idx += 1;
        }
        if !found {
            waivers.push_back(w);
        }
        put_fee_waivers(&env, &waivers);
        symbol_short!("ok")
    }

    pub fn remove_fee_waiver(
        env: Env,
        address: Option<Address>,
        portfolio_id: Option<Symbol>,
    ) -> Symbol {
        get_admin(&env).require_auth();
        let has_address = address.is_some();
        let has_portfolio = portfolio_id.is_some();
        let addr = match address {
            Some(a) => a,
            None => get_admin(&env),
        };
        let pid = match portfolio_id {
            Some(p) => p,
            None => sym_empty(),
        };
        let waivers = get_fee_waivers(&env);
        let mut new_waivers = soroban_sdk::Vec::new(&env);
        for w in waivers.iter() {
            let template = FeeWaiver {
                address: addr.clone(),
                has_address,
                portfolio_id: pid.clone(),
                has_portfolio,
                discount_bps: 0,
                waived: false,
                label: sym_empty(),
                expires_at: 0,
            };
            if !waiver_same_target(&w, &template) {
                new_waivers.push_back(w);
            }
        }
        put_fee_waivers(&env, &new_waivers);
        symbol_short!("ok")
    }

    pub fn list_fee_waivers(env: Env) -> soroban_sdk::Vec<FeeWaiver> {
        get_fee_waivers(&env)
    }

    pub fn set_revenue_recipients(
        env: Env,
        recipients: soroban_sdk::Vec<RevenueRecipient>,
    ) -> Symbol {
        get_admin(&env).require_auth();
        if recipients.len() > MAX_RECIPIENTS {
            soroban_sdk::panic_with_error!(&env, FeeError::TooManyRecipients);
        }
        put_revenue_recipients(&env, &recipients);
        symbol_short!("ok")
    }

    pub fn list_revenue_recipients(env: Env) -> soroban_sdk::Vec<RevenueRecipient> {
        get_revenue_recipients(&env)
    }

    pub fn distribute_revenue_amount(env: Env, amount: i128) -> soroban_sdk::Vec<(Address, i128)> {
        distribute_revenue(&env, amount)
    }

    pub fn get_total_collected(env: Env) -> i128 {
        env.storage()
            .persistent()
            .get(&FeeDataKey::TotalCollected)
            .unwrap_or(0)
    }

    pub fn get_category_total(env: Env, category: FeeCategory) -> i128 {
        env.storage()
            .persistent()
            .get(&FeeDataKey::CategoryTotal(category))
            .unwrap_or(0)
    }

    pub fn get_portfolio_total(env: Env, portfolio_id: Symbol) -> i128 {
        env.storage()
            .persistent()
            .get(&FeeDataKey::PortfolioTotal(portfolio_id))
            .unwrap_or(0)
    }

    pub fn get_fee_history(env: Env, max: u32) -> soroban_sdk::Vec<FeeRecord> {
        let h = get_fee_history(&env);
        if max == 0 || h.len() <= max {
            h
        } else {
            h.slice(h.len() - max..)
        }
    }

    pub fn get_fee_history_count(env: Env) -> u32 {
        get_fee_history(&env).len()
    }

    pub fn get_fee_history_by_portfolio(
        env: Env,
        portfolio_id: Symbol,
        max: u32,
    ) -> soroban_sdk::Vec<FeeRecord> {
        let h = get_fee_history(&env);
        let mut filtered = soroban_sdk::Vec::new(&env);
        for i in 0..h.len() {
            let r = h.get(i).unwrap();
            if r.portfolio_id == portfolio_id {
                filtered.push_back(r);
            }
            if max > 0 && filtered.len() >= max {
                break;
            }
        }
        filtered
    }

    pub fn get_fee_history_by_category(
        env: Env,
        category: FeeCategory,
        max: u32,
    ) -> soroban_sdk::Vec<FeeRecord> {
        let h = get_fee_history(&env);
        let mut filtered = soroban_sdk::Vec::new(&env);
        for i in 0..h.len() {
            let r = h.get(i).unwrap();
            if r.category == category {
                filtered.push_back(r);
            }
            if max > 0 && filtered.len() >= max {
                break;
            }
        }
        filtered
    }
}

// ===========================================================================
// Unit Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{symbol_short, testutils::Address as _, Address, Env};

    fn setup() -> (Env, FeeManagementContractClient<'static>, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, FeeManagementContract);
        let client = FeeManagementContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin);
        (env, client, admin)
    }

    #[test]
    fn test_initialize() {
        let (_env, client, admin) = setup();
        assert_eq!(client.get_admin(), admin);
    }

    #[test]
    fn test_double_initialize_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, FeeManagementContract);
        let client = FeeManagementContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let result = client.try_initialize(&admin);
        assert!(result.is_ok());
        let admin2 = Address::generate(&env);
        let result2 = client.try_initialize(&admin2);
        assert_eq!(result2, Err(Ok(FeeError::AlreadyInitialized)));
    }

    #[test]
    fn test_set_get_fee_structure() {
        let (_env, client, _admin) = setup();
        let fid = symbol_short!("TEST");
        client.set_fee_structure_simple(
            &fid,
            &FeeType::Percentage,
            &250,
            &soroban_sdk::Vec::new(&_env),
            &true,
        );
        let fs = client.get_fee_structure(&fid).unwrap();
        assert_eq!(fs.fee_type, FeeType::Percentage);
        assert_eq!(fs.amount_bps, 250);
        assert!(fs.active);
    }

    #[test]
    fn test_set_fee_structure_with_category_and_cap() {
        let (env, client, _admin) = setup();
        let fid = symbol_short!("REBAL");
        client.set_fee_structure(
            &fid,
            &FeeType::Percentage,
            &200,
            &soroban_sdk::Vec::new(&env),
            &FeeCategory::Rebalancing,
            &true,
            &Some(50_000),
        );
        let fs = client.get_fee_structure(&fid).unwrap();
        assert_eq!(fs.category, FeeCategory::Rebalancing);
        assert_eq!(fs.fee_cap, Some(50_000));
    }

    #[test]
    fn test_fee_calculation_percentage() {
        let (env, client, _admin) = setup();
        let fid = symbol_short!("PCT");
        client.set_fee_structure_simple(
            &fid,
            &FeeType::Percentage,
            &250,
            &soroban_sdk::Vec::new(&env),
            &true,
        );
        let result = client.calculate_fee(&fid, &10_000_000);
        assert_eq!(result.fee_amount, 250_000);
    }

    #[test]
    fn test_fee_calculation_flat() {
        let (env, client, _admin) = setup();
        let fid = symbol_short!("FLAT");
        client.set_fee_structure_simple(
            &fid,
            &FeeType::Flat,
            &1_000,
            &soroban_sdk::Vec::new(&env),
            &true,
        );
        let r1 = client.calculate_fee(&fid, &500);
        assert_eq!(r1.fee_amount, 500); // clamped
        let r2 = client.calculate_fee(&fid, &5_000);
        assert_eq!(r2.fee_amount, 1_000);
    }

    #[test]
    fn test_fee_calculation_tiered() {
        let (env, client, _admin) = setup();
        let mut tiers = soroban_sdk::Vec::new(&env);
        tiers.push_back(TierEntry {
            threshold: 0,
            fee_bps: 50,
        });
        tiers.push_back(TierEntry {
            threshold: 10_000_000,
            fee_bps: 30,
        });
        tiers.push_back(TierEntry {
            threshold: 100_000_000,
            fee_bps: 15,
        });
        let fid = symbol_short!("TIER");
        client.set_fee_structure_simple(&fid, &FeeType::Tiered, &0, &tiers, &true);
        assert_eq!(client.calculate_fee(&fid, &5_000_000).fee_amount, 25_000);
        assert_eq!(client.calculate_fee(&fid, &50_000_000).fee_amount, 150_000);
        assert_eq!(client.calculate_fee(&fid, &200_000_000).fee_amount, 300_000);
    }

    #[test]
    fn test_fee_calculation_with_cap() {
        let (env, client, _admin) = setup();
        let fid = symbol_short!("CAP");
        client.set_fee_structure(
            &fid,
            &FeeType::Percentage,
            &200,
            &soroban_sdk::Vec::new(&env),
            &FeeCategory::Trading,
            &true,
            &Some(50_000),
        );
        let result = client.calculate_fee(&fid, &100_000_000);
        assert_eq!(result.fee_amount, 50_000);
        assert_eq!(result.fee_cap, Some(50_000));
    }

    #[test]
    fn test_fee_structure_list() {
        let (env, client, _admin) = setup();
        let f1 = symbol_short!("F1");
        let f2 = symbol_short!("F2");
        client.set_fee_structure_simple(
            &f1,
            &FeeType::Percentage,
            &100,
            &soroban_sdk::Vec::new(&env),
            &true,
        );
        client.set_fee_structure_simple(
            &f2,
            &FeeType::Flat,
            &500,
            &soroban_sdk::Vec::new(&env),
            &true,
        );
        let list = client.list_fee_structures();
        assert_eq!(list.len(), 2);
        client.set_fee_active(&f1, &false);
        let fs = client.get_fee_structure(&f1).unwrap();
        assert!(!fs.active);
    }

    #[test]
    fn test_set_get_remove_portfolio_fee() {
        let (env, client, _admin) = setup();
        let fid = symbol_short!("PF");
        client.set_fee_structure_simple(
            &fid,
            &FeeType::Percentage,
            &100,
            &soroban_sdk::Vec::new(&env),
            &true,
        );
        let pid = symbol_short!("PORT1");
        client.set_portfolio_fee(&pid, &fid);
        assert_eq!(client.get_portfolio_fee(&pid), Some(fid.clone()));
        client.remove_portfolio_fee(&pid);
        assert_eq!(client.get_portfolio_fee(&pid), None);
    }

    #[test]
    fn test_calculate_portfolio_fee() {
        let (env, client, _admin) = setup();
        let fid = symbol_short!("PF");
        client.set_fee_structure_simple(
            &fid,
            &FeeType::Percentage,
            &200,
            &soroban_sdk::Vec::new(&env),
            &true,
        );
        let pid = symbol_short!("PORT1");
        client.set_portfolio_fee(&pid, &fid);
        let result = client.calculate_portfolio_fee(&pid, &symbol_short!("FALL"), &10_000_000);
        assert_eq!(result.fee_amount, 200_000);
    }

    #[test]
    fn test_waiver_discount() {
        let (env, client, _admin) = setup();
        let fid = symbol_short!("WD");
        client.set_fee_structure_simple(
            &fid,
            &FeeType::Percentage,
            &200,
            &soroban_sdk::Vec::new(&env),
            &true,
        );
        let pid = symbol_short!("WDP");
        client.set_portfolio_fee(&pid, &fid);
        client.set_fee_waiver(&None, &Some(pid.clone()), &5000, &false, &None, &0);
        let result = client.calculate_portfolio_fee(&pid, &symbol_short!("X"), &10_000_000);
        assert_eq!(result.fee_amount, 100_000);
        assert_eq!(result.discount_bps, 5000);
    }

    #[test]
    fn test_waiver_full_waive() {
        let (env, client, _admin) = setup();
        let fid = symbol_short!("FW");
        client.set_fee_structure_simple(
            &fid,
            &FeeType::Percentage,
            &200,
            &soroban_sdk::Vec::new(&env),
            &true,
        );
        let pid = symbol_short!("FWD");
        client.set_portfolio_fee(&pid, &fid);
        client.set_fee_waiver(&None, &Some(pid.clone()), &0, &true, &None, &0);
        let result = client.calculate_portfolio_fee(&pid, &symbol_short!("X"), &10_000_000);
        assert_eq!(result.fee_amount, 0);
        assert!(result.waived);
    }

    #[test]
    fn test_collect_fee_updates_totals() {
        let (env, client, _admin) = setup();
        let fid = symbol_short!("COL");
        client.set_fee_structure_simple(
            &fid,
            &FeeType::Percentage,
            &250,
            &soroban_sdk::Vec::new(&env),
            &true,
        );
        let pid = symbol_short!("P1");
        let caller = Address::generate(&env);
        let fee = client.collect_fee(&caller, &fid, &pid, &10_000_000);
        assert_eq!(fee, 250_000);
        assert_eq!(client.get_total_collected(), 250_000);
        assert_eq!(client.get_portfolio_total(&pid), 250_000);
        assert_eq!(client.get_fee_history_count(), 1);
    }

    #[test]
    fn test_collect_multiple_fees() {
        let (env, client, _admin) = setup();
        let fid = symbol_short!("CM");
        client.set_fee_structure_simple(
            &fid,
            &FeeType::Percentage,
            &200,
            &soroban_sdk::Vec::new(&env),
            &true,
        );
        let p1 = symbol_short!("P1");
        let p2 = symbol_short!("P2");
        let caller = Address::generate(&env);
        client.collect_fee(&caller, &fid, &p1, &10_000_000);
        client.collect_fee(&caller, &fid, &p2, &5_000_000);
        assert_eq!(client.get_total_collected(), 300_000);
        assert_eq!(client.get_portfolio_total(&p1), 200_000);
        assert_eq!(client.get_portfolio_total(&p2), 100_000);
        assert_eq!(client.get_fee_history_count(), 2);
    }

    #[test]
    fn test_revenue_distribution() {
        let (env, client, _admin) = setup();
        let t1 = Address::generate(&env);
        let t2 = Address::generate(&env);
        let mut recipients = soroban_sdk::Vec::new(&env);
        recipients.push_back(RevenueRecipient {
            address: t1.clone(),
            share_numerator: 70,
            label: symbol_short!("treasury"),
        });
        recipients.push_back(RevenueRecipient {
            address: t2.clone(),
            share_numerator: 30,
            label: symbol_short!("dev"),
        });
        client.set_revenue_recipients(&recipients);
        let dist = client.distribute_revenue_amount(&251_000);
        assert_eq!(dist.len(), 2);
        assert_eq!(dist.get(0).unwrap().1, 175_700);
        assert_eq!(dist.get(1).unwrap().1, 75_300);
    }

    #[test]
    fn test_revenue_distribution_empty() {
        let (_env, client, _admin) = setup();
        let dist = client.distribute_revenue_amount(&10_000);
        assert_eq!(dist.len(), 0);
    }

    #[test]
    fn test_fee_history_by_portfolio() {
        let (env, client, _admin) = setup();
        let fid = symbol_short!("FH");
        client.set_fee_structure_simple(
            &fid,
            &FeeType::Percentage,
            &100,
            &soroban_sdk::Vec::new(&env),
            &true,
        );
        let p1 = symbol_short!("P1");
        let p2 = symbol_short!("P2");
        let caller = Address::generate(&env);
        client.collect_fee(&caller, &fid, &p1, &1_000_000);
        client.collect_fee(&caller, &fid, &p2, &2_000_000);
        client.collect_fee(&caller, &fid, &p1, &3_000_000);
        assert_eq!(client.get_fee_history_by_portfolio(&p1, &0).len(), 2);
        assert_eq!(client.get_fee_history_by_portfolio(&p2, &0).len(), 1);
    }

    #[test]
    fn test_fee_history_limit() {
        let (env, client, _admin) = setup();
        let fid = symbol_short!("FL");
        client.set_fee_structure_simple(
            &fid,
            &FeeType::Percentage,
            &100,
            &soroban_sdk::Vec::new(&env),
            &true,
        );
        let pid = symbol_short!("P1");
        let caller = Address::generate(&env);
        for _ in 0..5 {
            client.collect_fee(&caller, &fid, &pid, &1_000_000);
        }
        assert_eq!(client.get_fee_history(&0).len(), 5);
        assert_eq!(client.get_fee_history(&3).len(), 3);
    }

    #[test]
    fn test_zero_fee_portfolio() {
        let (env, client, _admin) = setup();
        let zero_id = symbol_short!("ZERO");
        client.set_fee_structure_simple(
            &zero_id,
            &FeeType::Percentage,
            &0,
            &soroban_sdk::Vec::new(&env),
            &true,
        );
        let grant = symbol_short!("GRANT");
        client.set_portfolio_fee(&grant, &zero_id);
        let result = client.calculate_portfolio_fee(&grant, &symbol_short!("X"), &10_000_000);
        assert_eq!(result.fee_amount, 0);
    }

    #[test]
    fn test_error_codes() {
        assert_eq!(FeeError::FeeNotFound as u32, 1);
        assert_eq!(FeeError::FeeInactive as u32, 2);
        assert_eq!(FeeError::InvalidFeeConfiguration as u32, 3);
        assert_eq!(FeeError::ArithmeticOverflow as u32, 4);
        assert_eq!(FeeError::FeeWaiverNotFound as u32, 5);
        assert_eq!(FeeError::TooManyRecipients as u32, 6);
        assert_eq!(FeeError::AlreadyInitialized as u32, 7);
        assert_eq!(FeeError::Unauthorized as u32, 8);
    }
}
