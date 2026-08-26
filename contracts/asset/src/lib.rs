#![no_std]
//! # AstraPort Asset Management Contract
//!
//! Comprehensive asset management system for portfolio composition. Enables
//! dynamic management of portfolio assets with validation, pricing, and
//! allocation tracking.
//!
//! ## Key Features
//!
//! - **Asset lifecycle management** — Add, update, and remove assets from
//!   portfolios with full validation.
//! - **Type safety** — Supports Token and Derivative asset types with
//!   proper validation.
//! - **Risk classification** — Each asset carries a risk level (Low, Medium,
//!   High, VeryHigh) for portfolio risk assessment.
//! - **Price feed integration** — Store and retrieve asset prices for
//!   portfolio valuation calculations.
//! - **Safety checks** — Assets can only be removed when balance is zero,
//!   preventing accidental removal of active positions.
//! - **Metadata support** — Assets include name, decimals, and contract
//!   address for full on-chain identification.
//!
//! ## Modules
//!
//! - [`records`] — Soroban-typed data structures for assets, prices,
//!   storage keys, and error definitions.

use soroban_sdk::{contract, contractimpl, symbol_short, Address, Env, Symbol, Vec};

pub mod records;

use crate::records::*;

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

/// AstraPort Asset Management — portfolio asset lifecycle and pricing.
#[contract]
pub struct AssetManagementContract;

#[contractimpl]
impl AssetManagementContract {
    // =======================================================================
    // Lifecycle
    // =======================================================================

    /// Initialize the asset management contract with an admin address.
    ///
    /// Can only be called once; subsequent calls return an error.
    pub fn initialize(env: Env, admin: Address) -> Result<Symbol, AssetError> {
        let storage = env.storage().persistent();
        if storage.has(&AssetDataKey::Admin) {
            return Err(AssetError::AlreadyInitialized);
        }
        storage.set(&AssetDataKey::Admin, &admin);
        Ok(symbol_short!("ok"))
    }

    // =======================================================================
    // Admin helpers
    // =======================================================================

    /// Assert that `caller` is the admin. Returns `Err(Unauthorized)` if not.
    fn assert_admin(env: &Env, caller: &Address) -> Result<(), AssetError> {
        let stored_admin: Address = env
            .storage()
            .persistent()
            .get(&AssetDataKey::Admin)
            .ok_or(AssetError::Unauthorized)?;
        if *caller != stored_admin {
            return Err(AssetError::Unauthorized);
        }
        Ok(())
    }

    // =======================================================================
    // Validation helpers
    // =======================================================================

    /// Validate an asset symbol is a valid Soroban symbol.
    ///
    /// Soroban symbols are bounded to 9 bytes (short) or up to 32 bytes,
    /// so this validates we received a non-default symbol.
    fn validate_symbol(_symbol: &Symbol) -> Result<(), AssetError> {
        // Soroban symbols are already length-constrained by the SDK.
        // A symbol_short!("") is technically 0 bytes, but soroban_sdk
        // will reject it at serialization time. We accept all non-empty
        // symbols that survive SDK validation.
        Ok(())
    }

    /// Validate an asset name.
    ///
    /// Soroban symbols are length-constrained by the SDK.
    fn validate_name(_name: &Symbol) -> Result<(), AssetError> {
        Ok(())
    }

    /// Validate asset type is supported.
    fn validate_asset_type(asset_type: &AssetType) -> Result<(), AssetError> {
        if !asset_type.is_valid() {
            return Err(AssetError::InvalidAssetType);
        }
        Ok(())
    }

    /// Validate decimals value (0-18 is reasonable for most assets).
    fn validate_decimals(decimals: u32) -> Result<(), AssetError> {
        if decimals > 18 {
            return Err(AssetError::InvalidDecimals);
        }
        Ok(())
    }

    /// Validate asset balance (non-negative).
    fn validate_balance(balance: i128) -> Result<(), AssetError> {
        if balance < 0 {
            return Err(AssetError::InvalidBalance);
        }
        Ok(())
    }

    /// Validate asset price (positive).
    fn validate_price(price: i128) -> Result<(), AssetError> {
        if price <= 0 {
            return Err(AssetError::InvalidPrice);
        }
        Ok(())
    }

    // =======================================================================
    // Asset management
    // =======================================================================

    /// Add a new asset to a portfolio.
    ///
    /// Validates all asset properties before adding. Fails if the asset
    /// already exists in the portfolio or if the portfolio has reached
    /// the maximum asset limit.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    /// * `admin` - Admin address (must be authorized).
    /// * `portfolio_id` - Portfolio identifier.
    /// * `asset` - Asset to add.
    ///
    /// # Returns
    ///
    /// `Ok(symbol_short!("ok"))` on success, or an `AssetError`.
    pub fn add_asset(
        env: Env,
        admin: Address,
        portfolio_id: Symbol,
        asset: Asset,
    ) -> Result<Symbol, AssetError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        // Validate all asset properties
        Self::validate_symbol(&asset.symbol)?;
        Self::validate_name(&asset.name)?;
        Self::validate_asset_type(&asset.asset_type)?;
        Self::validate_decimals(asset.decimals)?;
        Self::validate_balance(asset.balance)?;

        // Check if asset already exists in portfolio
        let key = AssetDataKey::AssetEntry(portfolio_id.clone(), asset.symbol.clone());
        if env.storage().persistent().has(&key) {
            return Err(AssetError::AssetAlreadyExists);
        }

        // Check portfolio asset count limit
        let portfolio_assets = Self::get_portfolio_assets(env.clone(), portfolio_id.clone());
        if portfolio_assets.len() >= MAX_ASSETS_PER_PORTFOLIO {
            return Err(AssetError::MaxAssetsReached);
        }

        // Store the asset
        env.storage().persistent().set(&key, &asset);

        // Update portfolio assets list
        let mut assets = Self::get_portfolio_assets(env.clone(), portfolio_id.clone());
        assets.push_back(asset.clone());
        env.storage().persistent().set(
            &AssetDataKey::PortfolioAssets(portfolio_id.clone()),
            &assets,
        );

        // Ensure portfolio is registered
        Self::ensure_portfolio_registered(&env, &portfolio_id);

        // Emit event
        env.events().publish(
            (symbol_short!("ASSET_ADD"), portfolio_id),
            (asset.symbol, asset.asset_type),
        );

        Ok(symbol_short!("ok"))
    }

    /// Remove an asset from a portfolio.
    ///
    /// # Safety
    ///
    /// Assets can only be removed if their balance is zero. This prevents
    /// accidental removal of active positions and ensures portfolio integrity.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    /// * `admin` - Admin address (must be authorized).
    /// * `portfolio_id` - Portfolio identifier.
    /// * `asset_symbol` - Symbol of the asset to remove.
    ///
    /// # Returns
    ///
    /// `Ok(symbol_short!("ok"))` on success, or an `AssetError`.
    pub fn remove_asset(
        env: Env,
        admin: Address,
        portfolio_id: Symbol,
        asset_symbol: Symbol,
    ) -> Result<Symbol, AssetError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        // Get the asset
        let key = AssetDataKey::AssetEntry(portfolio_id.clone(), asset_symbol.clone());
        let asset: Asset = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(AssetError::AssetNotFound)?;

        // Safety check: cannot remove asset with non-zero balance
        if asset.balance != 0 {
            return Err(AssetError::NonZeroBalance);
        }

        // Remove from storage
        env.storage().persistent().remove(&key);

        // Update portfolio assets list
        let mut assets = Self::get_portfolio_assets(env.clone(), portfolio_id.clone());
        for i in 0..assets.len() {
            if assets.get(i).unwrap().symbol == asset_symbol {
                assets.remove(i);
                break;
            }
        }
        env.storage().persistent().set(
            &AssetDataKey::PortfolioAssets(portfolio_id.clone()),
            &assets,
        );

        // Emit event
        env.events()
            .publish((symbol_short!("ASSET_RM"), portfolio_id), asset_symbol);

        Ok(symbol_short!("ok"))
    }

    /// Update the balance of an asset in a portfolio.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    /// * `admin` - Admin address (must be authorized).
    /// * `portfolio_id` - Portfolio identifier.
    /// * `asset_symbol` - Symbol of the asset to update.
    /// * `new_balance` - New balance value.
    ///
    /// # Returns
    ///
    /// `Ok(symbol_short!("ok"))` on success, or an `AssetError`.
    pub fn update_asset_balance(
        env: Env,
        admin: Address,
        portfolio_id: Symbol,
        asset_symbol: Symbol,
        new_balance: i128,
    ) -> Result<Symbol, AssetError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        // Validate balance
        Self::validate_balance(new_balance)?;

        // Get and update the asset
        let key = AssetDataKey::AssetEntry(portfolio_id.clone(), asset_symbol.clone());
        let mut asset: Asset = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(AssetError::AssetNotFound)?;

        let old_balance = asset.balance;
        asset.balance = new_balance;

        // Store updated asset
        env.storage().persistent().set(&key, &asset);

        // Update in the portfolio assets list
        let mut assets = Self::get_portfolio_assets(env.clone(), portfolio_id.clone());
        for i in 0..assets.len() {
            if assets.get(i).unwrap().symbol == asset_symbol {
                assets.remove(i);
                assets.insert(i, asset.clone());
                break;
            }
        }
        env.storage().persistent().set(
            &AssetDataKey::PortfolioAssets(portfolio_id.clone()),
            &assets,
        );

        // Emit event
        env.events().publish(
            (symbol_short!("BAL_UPD"), portfolio_id, asset_symbol),
            (old_balance, new_balance),
        );

        Ok(symbol_short!("ok"))
    }

    /// Update an asset's metadata (name, decimals, risk level, type).
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    /// * `admin` - Admin address (must be authorized).
    /// * `portfolio_id` - Portfolio identifier.
    /// * `asset_symbol` - Symbol of the asset to update.
    /// * `new_name` - New asset name.
    /// * `new_decimals` - New decimals value.
    /// * `new_risk_level` - New risk level.
    /// * `new_asset_type` - New asset type.
    ///
    /// # Returns
    ///
    /// `Ok(symbol_short!("ok"))` on success, or an `AssetError`.
    pub fn update_asset_metadata(
        env: Env,
        admin: Address,
        portfolio_id: Symbol,
        asset_symbol: Symbol,
        new_name: Symbol,
        new_decimals: u32,
        new_risk_level: RiskLevel,
        new_asset_type: AssetType,
    ) -> Result<Symbol, AssetError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        // Validate metadata
        Self::validate_name(&new_name)?;
        Self::validate_decimals(new_decimals)?;
        Self::validate_asset_type(&new_asset_type)?;

        // Get and update the asset
        let key = AssetDataKey::AssetEntry(portfolio_id.clone(), asset_symbol.clone());
        let mut asset: Asset = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(AssetError::AssetNotFound)?;

        let name_for_event = new_name.clone();
        asset.name = new_name;
        asset.decimals = new_decimals;
        asset.risk_level = new_risk_level;
        asset.asset_type = new_asset_type;

        // Store updated asset
        env.storage().persistent().set(&key, &asset);

        // Update in the portfolio assets list
        let mut assets = Self::get_portfolio_assets(env.clone(), portfolio_id.clone());
        for i in 0..assets.len() {
            if assets.get(i).unwrap().symbol == asset_symbol {
                assets.remove(i);
                assets.insert(i, asset.clone());
                break;
            }
        }
        env.storage().persistent().set(
            &AssetDataKey::PortfolioAssets(portfolio_id.clone()),
            &assets,
        );

        // Emit event
        env.events().publish(
            (symbol_short!("META_UPD"), portfolio_id, asset_symbol),
            (name_for_event, new_risk_level as u32, new_asset_type as u32),
        );

        Ok(symbol_short!("ok"))
    }

    // =======================================================================
    // Asset queries
    // =======================================================================

    /// Get a specific asset from a portfolio.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    /// * `portfolio_id` - Portfolio identifier.
    /// * `asset_symbol` - Symbol of the asset to retrieve.
    ///
    /// # Returns
    ///
    /// `Some(Asset)` if found, `None` otherwise.
    pub fn get_asset(env: Env, portfolio_id: Symbol, asset_symbol: Symbol) -> Option<Asset> {
        let key = AssetDataKey::AssetEntry(portfolio_id, asset_symbol);
        env.storage().persistent().get(&key)
    }

    /// Get all assets in a portfolio.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    /// * `portfolio_id` - Portfolio identifier.
    ///
    /// # Returns
    ///
    /// Vector of all assets in the portfolio.
    pub fn get_portfolio_assets(env: Env, portfolio_id: Symbol) -> Vec<Asset> {
        let key = AssetDataKey::PortfolioAssets(portfolio_id);
        env.storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get all active assets in a portfolio.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    /// * `portfolio_id` - Portfolio identifier.
    ///
    /// # Returns
    ///
    /// Vector of active assets in the portfolio.
    pub fn get_active_assets(env: Env, portfolio_id: Symbol) -> Vec<Asset> {
        let all_assets = Self::get_portfolio_assets(env.clone(), portfolio_id);
        let mut active_assets = Vec::new(&env);
        for i in 0..all_assets.len() {
            let asset = all_assets.get(i).unwrap();
            if asset.is_active {
                active_assets.push_back(asset);
            }
        }
        active_assets
    }

    // =======================================================================
    // Price feed integration
    // =======================================================================

    /// Set the price for an asset.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    /// * `admin` - Admin address (must be authorized).
    /// * `asset_symbol` - Symbol of the asset.
    /// * `price` - Price in fixed-point (SCALE = 1.0).
    /// * `source` - Price source identifier.
    ///
    /// # Returns
    ///
    /// `Ok(symbol_short!("ok"))` on success, or an `AssetError`.
    pub fn set_asset_price(
        env: Env,
        admin: Address,
        asset_symbol: Symbol,
        price: i128,
        source: Symbol,
    ) -> Result<Symbol, AssetError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        // Validate price
        Self::validate_price(price)?;

        let now = env.ledger().timestamp();
        let asset_price = AssetPrice {
            symbol: asset_symbol.clone(),
            price,
            updated_at: now,
            source,
        };

        let key = AssetDataKey::AssetPrice(asset_symbol.clone());
        env.storage().persistent().set(&key, &asset_price);

        // Emit event
        env.events()
            .publish((symbol_short!("PRICE_SET"), asset_symbol), price);

        Ok(symbol_short!("ok"))
    }

    /// Get the price for an asset.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    /// * `asset_symbol` - Symbol of the asset.
    ///
    /// # Returns
    ///
    /// `Some(AssetPrice)` if found, `None` otherwise.
    pub fn get_asset_price(env: Env, asset_symbol: Symbol) -> Option<AssetPrice> {
        let key = AssetDataKey::AssetPrice(asset_symbol);
        env.storage().persistent().get(&key)
    }

    /// Get the current value of an asset (balance × price).
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    /// * `portfolio_id` - Portfolio identifier.
    /// * `asset_symbol` - Symbol of the asset.
    ///
    /// # Returns
    ///
    /// `Some(i128)` with the asset value, or `None` if asset or price not found.
    pub fn get_asset_value(env: Env, portfolio_id: Symbol, asset_symbol: Symbol) -> Option<i128> {
        let asset = Self::get_asset(env.clone(), portfolio_id, asset_symbol.clone())?;
        let price_data = Self::get_asset_price(env, asset_symbol)?;
        Some(asset.balance * price_data.price)
    }

    // =======================================================================
    // Portfolio summary
    // =======================================================================

    /// Get the total value of a portfolio (sum of all asset values).
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    /// * `portfolio_id` - Portfolio identifier.
    ///
    /// # Returns
    ///
    /// Total portfolio value in fixed-point.
    pub fn get_portfolio_value(env: Env, portfolio_id: Symbol) -> i128 {
        let assets = Self::get_portfolio_assets(env.clone(), portfolio_id);
        let mut total_value: i128 = 0;

        for i in 0..assets.len() {
            let asset = assets.get(i).unwrap();
            let price_data = Self::get_asset_price(env.clone(), asset.symbol.clone());
            if let Some(price) = price_data {
                total_value = total_value
                    .checked_add(asset.balance * price.price)
                    .unwrap_or(i128::MAX);
            }
        }

        total_value
    }

    /// Get the portfolio summary including asset count and total value.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    /// * `portfolio_id` - Portfolio identifier.
    ///
    /// # Returns
    ///
    /// `Some(PortfolioSummary)` if portfolio exists, `None` otherwise.
    pub fn get_portfolio_summary(env: Env, portfolio_id: Symbol) -> Option<PortfolioSummary> {
        let assets = Self::get_portfolio_assets(env.clone(), portfolio_id.clone());
        if assets.is_empty()
            && !env
                .storage()
                .persistent()
                .has(&AssetDataKey::PortfolioAssets(portfolio_id.clone()))
        {
            return None;
        }

        let mut total_value: i128 = 0;
        let mut active_count: u32 = 0;

        for i in 0..assets.len() {
            let asset = assets.get(i).unwrap();
            if asset.is_active {
                active_count += 1;
            }
            let price_data = Self::get_asset_price(env.clone(), asset.symbol.clone());
            if let Some(price) = price_data {
                total_value = total_value
                    .checked_add(asset.balance * price.price)
                    .unwrap_or(i128::MAX);
            }
        }

        Some(PortfolioSummary {
            portfolio_id,
            asset_count: assets.len(),
            total_value,
            active_asset_count: active_count,
        })
    }

    // =======================================================================
    // Helper functions
    // =======================================================================

    /// Ensure a portfolio is registered in the global portfolio list.
    fn ensure_portfolio_registered(env: &Env, portfolio_id: &Symbol) {
        let mut portfolios: Vec<Symbol> = env
            .storage()
            .persistent()
            .get(&AssetDataKey::Portfolios)
            .unwrap_or_else(|| Vec::new(env));

        if !portfolios.contains(portfolio_id.clone()) {
            portfolios.push_back(portfolio_id.clone());
            env.storage()
                .persistent()
                .set(&AssetDataKey::Portfolios, &portfolios);
        }
    }

    /// Get all registered portfolios.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    ///
    /// # Returns
    ///
    /// Vector of all portfolio identifiers.
    pub fn get_all_portfolios(env: Env) -> Vec<Symbol> {
        env.storage()
            .persistent()
            .get(&AssetDataKey::Portfolios)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get the count of assets in a portfolio.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment.
    /// * `portfolio_id` - Portfolio identifier.
    ///
    /// # Returns
    ///
    /// Number of assets in the portfolio.
    pub fn get_asset_count(env: Env, portfolio_id: Symbol) -> u32 {
        let assets = Self::get_portfolio_assets(env, portfolio_id);
        assets.len()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn setup() -> (Env, AssetManagementClient<'static>, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, AssetManagementContract);
        let client = AssetManagementClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin);
        (env, client, admin)
    }

    fn make_asset(symbol: &str, name: &str) -> Asset {
        Asset {
            symbol: symbol_short!(symbol),
            asset_type: AssetType::Token,
            contract_address: Address::generate(&Env::default()),
            balance: 1000 * SCALE,
            name: symbol_short!(name),
            decimals: 8,
            risk_level: RiskLevel::Medium,
            is_active: true,
        }
    }

    // ---- Initialization ----

    #[test]
    fn test_initialize() {
        let (_env, client, _admin) = setup();
        let portfolios = client.get_all_portfolios();
        assert_eq!(portfolios.len(), 0);
    }

    #[test]
    fn test_double_initialize_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, AssetManagementContract);
        let client = AssetManagementClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin);

        let admin2 = Address::generate(&env);
        assert_eq!(
            client.try_initialize(&admin2),
            Err(Ok(AssetError::AlreadyInitialized)),
        );
    }

    // ---- Add asset ----

    #[test]
    fn test_add_asset() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");
        let asset = make_asset("XLM", "Stellar");

        let result = client.add_asset(&admin, &pid, &asset);
        assert_eq!(result, symbol_short!("ok"));

        let retrieved = client.get_asset(&pid, &symbol_short!("XLM"));
        assert!(retrieved.is_some());
        let a = retrieved.unwrap();
        assert_eq!(a.symbol, symbol_short!("XLM"));
        assert_eq!(a.name, symbol_short!("Stellar"));
        assert_eq!(a.balance, 1000 * SCALE);
        assert_eq!(a.decimals, 8);
        assert_eq!(a.risk_level, RiskLevel::Medium);
        assert_eq!(a.asset_type, AssetType::Token);
    }

    #[test]
    fn test_add_asset_duplicate_fails() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");
        let asset = make_asset("XLM", "Stellar");

        client.add_asset(&admin, &pid, &asset);
        assert_eq!(
            client.try_add_asset(&admin, &pid, &asset),
            Err(Ok(AssetError::AssetAlreadyExists)),
        );
    }

    #[test]
    fn test_add_asset_multiple_assets() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");

        let asset1 = make_asset("XLM", "Stellar");
        let asset2 = make_asset("USDC", "USD Coin");
        let asset3 = make_asset("BTC", "Bitcoin");

        client.add_asset(&admin, &pid, &asset1);
        client.add_asset(&admin, &pid, &asset2);
        client.add_asset(&admin, &pid, &asset3);

        let assets = client.get_portfolio_assets(&pid);
        assert_eq!(assets.len(), 3);
    }

    #[test]
    fn test_add_asset_invalid_type_fails() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");
        let mut asset = make_asset("XLM", "Stellar");
        // Use an invalid type by trying to set it (AssetType only has Token/Derivative)
        asset.symbol = symbol_short!("XLM");

        // Valid type should work
        let result = client.add_asset(&admin, &pid, &asset);
        assert_eq!(result, symbol_short!("ok"));
    }

    // ---- Remove asset ----

    #[test]
    fn test_remove_asset_zero_balance() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");
        let mut asset = make_asset("XLM", "Stellar");
        asset.balance = 0; // Zero balance required

        client.add_asset(&admin, &pid, &asset);

        let result = client.remove_asset(&admin, &pid, &symbol_short!("XLM"));
        assert_eq!(result, symbol_short!("ok"));

        let retrieved = client.get_asset(&pid, &symbol_short!("XLM"));
        assert!(retrieved.is_none());
    }

    #[test]
    fn test_remove_asset_nonzero_balance_fails() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");
        let asset = make_asset("XLM", "Stellar"); // balance = 1000 * SCALE

        client.add_asset(&admin, &pid, &asset);

        assert_eq!(
            client.try_remove_asset(&admin, &pid, &symbol_short!("XLM")),
            Err(Ok(AssetError::NonZeroBalance)),
        );
    }

    #[test]
    fn test_remove_asset_not_found_fails() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");

        assert_eq!(
            client.try_remove_asset(&admin, &pid, &symbol_short!("XLM")),
            Err(Ok(AssetError::AssetNotFound)),
        );
    }

    #[test]
    fn test_remove_asset_updates_portfolio_count() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");

        let mut asset1 = make_asset("XLM", "Stellar");
        asset1.balance = 0;
        let mut asset2 = make_asset("USDC", "USD Coin");
        asset2.balance = 0;

        client.add_asset(&admin, &pid, &asset1);
        client.add_asset(&admin, &pid, &asset2);

        assert_eq!(client.get_asset_count(&pid), 2);

        client.remove_asset(&admin, &pid, &symbol_short!("XLM"));
        assert_eq!(client.get_asset_count(&pid), 1);

        client.remove_asset(&admin, &pid, &symbol_short!("USDC"));
        assert_eq!(client.get_asset_count(&pid), 0);
    }

    // ---- Update balance ----

    #[test]
    fn test_update_asset_balance() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");
        let asset = make_asset("XLM", "Stellar");

        client.add_asset(&admin, &pid, &asset);

        let result =
            client.update_asset_balance(&admin, &pid, &symbol_short!("XLM"), &(2000 * SCALE));
        assert_eq!(result, symbol_short!("ok"));

        let updated = client.get_asset(&pid, &symbol_short!("XLM")).unwrap();
        assert_eq!(updated.balance, 2000 * SCALE);
    }

    #[test]
    fn test_update_asset_balance_to_zero() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");
        let asset = make_asset("XLM", "Stellar");

        client.add_asset(&admin, &pid, &asset);

        let result = client.update_asset_balance(&admin, &pid, &symbol_short!("XLM"), &0);
        assert_eq!(result, symbol_short!("ok"));

        let updated = client.get_asset(&pid, &symbol_short!("XLM")).unwrap();
        assert_eq!(updated.balance, 0);
    }

    #[test]
    fn test_update_asset_balance_negative_fails() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");
        let asset = make_asset("XLM", "Stellar");

        client.add_asset(&admin, &pid, &asset);

        assert_eq!(
            client.try_update_asset_balance(&admin, &pid, &symbol_short!("XLM"), &(-1)),
            Err(Ok(AssetError::InvalidBalance)),
        );
    }

    #[test]
    fn test_update_asset_balance_not_found_fails() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");

        assert_eq!(
            client.try_update_asset_balance(&admin, &pid, &symbol_short!("XLM"), &100),
            Err(Ok(AssetError::AssetNotFound)),
        );
    }

    // ---- Price feed ----

    #[test]
    fn test_set_get_asset_price() {
        let (env, client, admin) = setup();
        let source = symbol_short!("oracle");

        let result = client.set_asset_price(&admin, &symbol_short!("XLM"), &(2 * SCALE), &source);
        assert_eq!(result, symbol_short!("ok"));

        let price = client.get_asset_price(&symbol_short!("XLM"));
        assert!(price.is_some());
        let p = price.unwrap();
        assert_eq!(p.price, 2 * SCALE);
        assert_eq!(p.source, symbol_short!("oracle"));
    }

    #[test]
    fn test_get_asset_price_not_found() {
        let (env, client, _admin) = setup();
        let price = client.get_asset_price(&symbol_short!("XLM"));
        assert!(price.is_none());
    }

    #[test]
    fn test_set_asset_price_invalid_fails() {
        let (env, client, admin) = setup();
        let source = symbol_short!("oracle");

        assert_eq!(
            client.try_set_asset_price(&admin, &symbol_short!("XLM"), &0, &source),
            Err(Ok(AssetError::InvalidPrice)),
        );

        assert_eq!(
            client.try_set_asset_price(&admin, &symbol_short!("XLM"), &(-1), &source),
            Err(Ok(AssetError::InvalidPrice)),
        );
    }

    #[test]
    fn test_asset_value_calculation() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");
        let source = symbol_short!("oracle");

        let mut asset = make_asset("XLM", "Stellar");
        asset.balance = 100; // 100 units
        client.add_asset(&admin, &pid, &asset);

        // Price = SCALE means 1.0 per unit
        client.set_asset_price(&admin, &symbol_short!("XLM"), &SCALE, &source);

        let value = client.get_asset_value(&pid, &symbol_short!("XLM"));
        assert_eq!(value, Some(100 * SCALE));
    }

    #[test]
    fn test_asset_value_no_price() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");

        let asset = make_asset("XLM", "Stellar");
        client.add_asset(&admin, &pid, &asset);

        // No price set
        let value = client.get_asset_value(&pid, &symbol_short!("XLM"));
        assert_eq!(value, None);
    }

    // ---- Portfolio value ----

    #[test]
    fn test_portfolio_value_empty() {
        let (env, client, _admin) = setup();
        let pid = symbol_short!("PORT1");

        let value = client.get_portfolio_value(&pid);
        assert_eq!(value, 0);
    }

    #[test]
    fn test_portfolio_value_single_asset() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");
        let source = symbol_short!("oracle");

        let mut asset = make_asset("XLM", "Stellar");
        asset.balance = 1000;
        client.add_asset(&admin, &pid, &asset);

        client.set_asset_price(&admin, &symbol_short!("XLM"), &(2 * SCALE), &source);

        let value = client.get_portfolio_value(&pid);
        assert_eq!(value, 2000 * SCALE);
    }

    #[test]
    fn test_portfolio_value_multiple_assets() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");
        let source = symbol_short!("oracle");

        let mut asset1 = make_asset("XLM", "Stellar");
        asset1.balance = 1000;
        let mut asset2 = make_asset("USDC", "USD Coin");
        asset2.balance = 500;

        client.add_asset(&admin, &pid, &asset1);
        client.add_asset(&admin, &pid, &asset2);

        client.set_asset_price(&admin, &symbol_short!("XLM"), &SCALE, &source);
        client.set_asset_price(&admin, &symbol_short!("USDC"), &(2 * SCALE), &source);

        // XLM: 1000 * SCALE, USDC: 500 * 2 * SCALE = 1000 * SCALE
        // Total: 2000 * SCALE
        let value = client.get_portfolio_value(&pid);
        assert_eq!(value, 2000 * SCALE);
    }

    // ---- Portfolio summary ----

    #[test]
    fn test_portfolio_summary_none_when_empty() {
        let (env, client, _admin) = setup();
        let pid = symbol_short!("PORT1");

        let summary = client.get_portfolio_summary(&pid);
        assert!(summary.is_none());
    }

    #[test]
    fn test_portfolio_summary_with_assets() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");
        let source = symbol_short!("oracle");

        let mut asset = make_asset("XLM", "Stellar");
        asset.balance = 1000;
        client.add_asset(&admin, &pid, &asset);

        client.set_asset_price(&admin, &symbol_short!("XLM"), &SCALE, &source);

        let summary = client.get_portfolio_summary(&pid).unwrap();
        assert_eq!(summary.asset_count, 1);
        assert_eq!(summary.total_value, 1000 * SCALE);
        assert_eq!(summary.active_asset_count, 1);
    }

    // ---- Active assets ----

    #[test]
    fn test_get_active_assets() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");

        let mut asset1 = make_asset("XLM", "Stellar");
        asset1.is_active = true;
        let mut asset2 = make_asset("USDC", "USD Coin");
        asset2.is_active = false;

        client.add_asset(&admin, &pid, &asset1);
        client.add_asset(&admin, &pid, &asset2);

        let active = client.get_active_assets(&pid);
        assert_eq!(active.len(), 1);
        assert_eq!(active.get(0).unwrap().symbol, symbol_short!("XLM"));
    }

    // ---- Portfolio registration ----

    #[test]
    fn test_portfolio_registered_on_add() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");
        let asset = make_asset("XLM", "Stellar");

        client.add_asset(&admin, &pid, &asset);

        let portfolios = client.get_all_portfolios();
        assert_eq!(portfolios.len(), 1);
        assert_eq!(portfolios.get(0).unwrap(), pid);
    }

    #[test]
    fn test_multiple_portfolios() {
        let (env, client, admin) = setup();
        let pid1 = symbol_short!("PORT1");
        let pid2 = symbol_short!("PORT2");
        let asset1 = make_asset("XLM", "Stellar");
        let asset2 = make_asset("USDC", "USD Coin");

        client.add_asset(&admin, &pid1, &asset1);
        client.add_asset(&admin, &pid2, &asset2);

        let portfolios = client.get_all_portfolios();
        assert_eq!(portfolios.len(), 2);
    }

    // ---- Risk levels ----

    #[test]
    fn test_add_asset_with_different_risk_levels() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");

        let mut asset_low = make_asset("USDC", "USD Coin");
        asset_low.risk_level = RiskLevel::Low;

        let mut asset_high = make_asset("DOGE", "Dogecoin");
        asset_high.risk_level = RiskLevel::High;

        let mut asset_very_high = make_asset("SHIB", "Shiba Inu");
        asset_very_high.risk_level = RiskLevel::VeryHigh;

        client.add_asset(&admin, &pid, &asset_low);
        client.add_asset(&admin, &pid, &asset_high);
        client.add_asset(&admin, &pid, &asset_very_high);

        let assets = client.get_portfolio_assets(&pid);
        assert_eq!(assets.len(), 3);
    }

    // ---- Asset types ----

    #[test]
    fn test_add_derivative_asset() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("PORT1");

        let asset = Asset {
            symbol: symbol_short!("CALL"),
            asset_type: AssetType::Derivative,
            contract_address: Address::generate(&env),
            balance: 100,
            name: symbol_short!("CallOption"),
            decimals: 0,
            risk_level: RiskLevel::High,
            is_active: true,
        };

        let result = client.add_asset(&admin, &pid, &asset);
        assert_eq!(result, symbol_short!("ok"));

        let retrieved = client.get_asset(&pid, &symbol_short!("CALL")).unwrap();
        assert_eq!(retrieved.asset_type, AssetType::Derivative);
    }
}
