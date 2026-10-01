#![no_std]
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, Symbol,
};

// ============================================================================
// Storage Key Symbols
// ============================================================================

const ADMIN: Symbol = symbol_short!("ADMIN");
const USER_IDS: Symbol = symbol_short!("USRIDS");
const SCORES: Symbol = symbol_short!("SCORES");
const BADGES: Symbol = symbol_short!("BADGES");
const USR_BID: Symbol = symbol_short!("USR_BID");
const STATS: Symbol = symbol_short!("STATS");
const CHALLENGES: Symbol = symbol_short!("CHALS");
const CHAL_IDS: Symbol = symbol_short!("CHAL_ID");
const CHAL_PRT: Symbol = symbol_short!("CHAL_PT");
const RWD_POOL: Symbol = symbol_short!("RWD_PL");
const RWD_DIST: Symbol = symbol_short!("RWD_DI");
const BADGE_DEFS: Symbol = symbol_short!("BDG_DEF");
const BADGE_DIDS: Symbol = symbol_short!("BDG_DID");

// ============================================================================
// Limits & Constants
// ============================================================================

const MAX_LEADERBOARD_RETURN: u32 = 100;
const MAX_USERS_RETURN: u32 = 500;
#[allow(dead_code)]
const BPS_DENOM: i128 = 10_000;

// Score component weights (sum to 100)
const TRADE_SCORE_MAX: i128 = 30;
const ROI_SCORE_MAX: i128 = 30;
const STREAK_SCORE_MAX: i128 = 15;
const LEARN_SCORE_MAX: i128 = 15;
const COMMUNITY_SCORE_MAX: i128 = 10;

// Tier thresholds (out of 100)
const TIER_SILVER: i128 = 25;
const TIER_GOLD: i128 = 50;
const TIER_PLATINUM: i128 = 75;

// Reward amounts per tier (virtual tokens)
const REWARD_BRONZE: i128 = 10;
const REWARD_SILVER: i128 = 25;
const REWARD_GOLD: i128 = 50;
const REWARD_PLATINUM: i128 = 100;

// ============================================================================
// Errors
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[contracterror]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    UserAlreadyRegistered = 3,
    UserNotFound = 4,
    BadgeAlreadyEarned = 5,
    BadgeNotFound = 6,
    ChallengeNotFound = 7,
    ChallengeAlreadyEnded = 8,
    ChallengeNotStarted = 9,
    ChallengeAlreadyJoined = 10,
    InvalidChallengeParams = 11,
    InsufficientRewardPool = 12,
    ArithmeticOverflow = 13,
    AdminRequired = 14,
    BadgeDefinitionNotFound = 15,
}

// ============================================================================
// Types
// ============================================================================

/// Progression tiers for players based on composite score.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd)]
#[contracttype]
pub enum ProgressionTier {
    Bronze = 0,
    Silver = 1,
    Gold = 2,
    Platinum = 3,
}

/// Achievement badge types that can be earned by players.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[contracttype]
pub enum BadgeType {
    FirstTrade,
    Trade10,
    Trade50,
    Trade100,
    PositiveRoi,
    Roi50,
    Roi100,
    Streak3,
    Streak5,
    Streak10,
    Learn1,
    Learn5,
    LearnAll,
    Community1,
    ChallengeWin,
    BronzeTier,
    SilverTier,
    GoldTier,
    PlatinumTier,
}

/// A badge definition that can be configured by admin.
#[derive(Clone, Debug, PartialEq, Eq)]
#[contracttype]
pub struct BadgeDefinition {
    pub badge_id: Symbol,
    pub badge_type: BadgeType,
    pub name: Symbol,
    pub description: Symbol,
    pub reward_amount: i128,
    pub active: bool,
}

/// A badge record earned by a user.
#[derive(Clone, Debug, PartialEq, Eq)]
#[contracttype]
pub struct BadgeRecord {
    pub badge_id: Symbol,
    pub user: Address,
    pub earned_at: u64,
    pub score_at_earn: i128,
}

/// Comprehensive player statistics.
#[derive(Clone, Debug, PartialEq, Eq)]
#[contracttype]
pub struct PlayerStats {
    pub user: Address,
    pub trade_count: u32,
    pub successful_trades: u32,
    pub best_roi_bps: i128,
    pub current_streak: u32,
    pub best_streak: u32,
    pub modules_completed: u32,
    pub total_modules: u32,
    pub community_actions: u32,
    pub challenges_won: u32,
    pub tier: ProgressionTier,
    pub total_score: i128,
    pub last_active: u64,
}

/// A leaderboard entry for display.
#[derive(Clone, Debug, PartialEq, Eq)]
#[contracttype]
pub struct LeaderboardEntry {
    pub rank: u32,
    pub user: Address,
    pub score: i128,
    pub tier: ProgressionTier,
    pub trade_count: u32,
    pub best_roi_bps: i128,
    pub streak: u32,
}

/// Paginated leaderboard result.
#[derive(Clone, Debug, PartialEq, Eq)]
#[contracttype]
pub struct LeaderboardPage {
    pub entries: soroban_sdk::Vec<LeaderboardEntry>,
    pub total_players: u32,
    pub page_offset: u32,
    pub page_limit: u32,
}

/// Sort metric for leaderboard queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[contracttype]
pub enum SortMetric {
    Score,
    TradeCount,
    Roi,
    Streak,
}

/// Time window filter for leaderboards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[contracttype]
pub enum TimeWindow {
    AllTime,
    Daily,
    Weekly,
    Monthly,
}

/// A time-limited challenge campaign.
#[derive(Clone, Debug, PartialEq, Eq)]
#[contracttype]
pub struct Challenge {
    pub challenge_id: Symbol,
    pub name: Symbol,
    pub description: Symbol,
    pub target_metric: SortMetric,
    pub target_value: i128,
    pub reward_amount: i128,
    pub start_time: u64,
    pub end_time: u64,
    pub active: bool,
}

/// User's participation record in a challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
#[contracttype]
pub struct ChallengeEntry {
    pub challenge_id: Symbol,
    pub user: Address,
    pub current_value: i128,
    pub completed: bool,
    pub completed_at: u64,
}

// ============================================================================
// Storage Helpers
// ============================================================================

fn is_initialized(env: &Env) -> bool {
    env.storage().instance().has(&ADMIN)
}

fn get_admin(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&ADMIN)
        .unwrap_or_else(|| soroban_sdk::panic_with_error!(env, Error::NotInitialized))
}

fn put_admin(env: &Env, admin: &Address) {
    env.storage().instance().set(&ADMIN, admin);
}

fn get_user_ids(env: &Env) -> soroban_sdk::Vec<Address> {
    env.storage()
        .persistent()
        .get(&USER_IDS)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

fn put_user_ids(env: &Env, ids: &soroban_sdk::Vec<Address>) {
    env.storage().persistent().set(&USER_IDS, ids);
}

fn add_user_id(env: &Env, user: &Address) {
    let mut ids = get_user_ids(env);
    // Check for duplicates
    for existing in ids.iter() {
        if existing == *user {
            return;
        }
    }
    ids.push_back(user.clone());
    put_user_ids(env, &ids);
}

fn get_score(env: &Env, user: &Address) -> i128 {
    let key = (SCORES, user);
    env.storage().persistent().get(&key).unwrap_or(0)
}

fn put_score(env: &Env, user: &Address, score: i128) {
    let key = (SCORES, user);
    env.storage().persistent().set(&key, &score);
}

fn get_player_stats(env: &Env, user: &Address) -> Option<PlayerStats> {
    let key = (STATS, user);
    env.storage().persistent().get(&key)
}

fn put_player_stats(env: &Env, stats: &PlayerStats) {
    let key = (STATS, &stats.user);
    env.storage().persistent().set(&key, stats);
}

fn get_user_badge_ids(env: &Env, user: &Address) -> soroban_sdk::Vec<Symbol> {
    let key = (USR_BID, user);
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

fn put_user_badge_ids(env: &Env, user: &Address, ids: &soroban_sdk::Vec<Symbol>) {
    let key = (USR_BID, user);
    env.storage().persistent().set(&key, ids);
}

fn get_badge_record(env: &Env, user: &Address, badge_id: &Symbol) -> Option<BadgeRecord> {
    let key = (BADGES, user, badge_id);
    env.storage().persistent().get(&key)
}

fn put_badge_record(env: &Env, record: &BadgeRecord) {
    let key = (BADGES, &record.user, &record.badge_id);
    env.storage().persistent().set(&key, record);
}

fn get_badge_definitions(env: &Env) -> soroban_sdk::Vec<BadgeDefinition> {
    env.storage()
        .persistent()
        .get(&BADGE_DEFS)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

fn put_badge_definitions(env: &Env, defs: &soroban_sdk::Vec<BadgeDefinition>) {
    env.storage().persistent().set(&BADGE_DEFS, defs);
}

#[allow(dead_code)]
fn get_badge_def_ids(env: &Env) -> soroban_sdk::Vec<Symbol> {
    env.storage()
        .persistent()
        .get(&BADGE_DIDS)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

fn put_badge_def_ids(env: &Env, ids: &soroban_sdk::Vec<Symbol>) {
    env.storage().persistent().set(&BADGE_DIDS, ids);
}

fn get_challenge(env: &Env, challenge_id: &Symbol) -> Option<Challenge> {
    let key = (CHALLENGES, challenge_id);
    env.storage().persistent().get(&key)
}

fn put_challenge(env: &Env, challenge: &Challenge) {
    let key = (CHALLENGES, &challenge.challenge_id);
    env.storage().persistent().set(&key, challenge);
}

fn get_challenge_ids(env: &Env) -> soroban_sdk::Vec<Symbol> {
    env.storage()
        .persistent()
        .get(&CHAL_IDS)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

fn put_challenge_ids(env: &Env, ids: &soroban_sdk::Vec<Symbol>) {
    env.storage().persistent().set(&CHAL_IDS, ids);
}

fn get_challenge_entry(env: &Env, challenge_id: &Symbol, user: &Address) -> Option<ChallengeEntry> {
    let key = (CHAL_PRT, challenge_id, user);
    env.storage().persistent().get(&key)
}

fn put_challenge_entry(env: &Env, entry: &ChallengeEntry) {
    let key = (CHAL_PRT, &entry.challenge_id, &entry.user);
    env.storage().persistent().set(&key, entry);
}

fn get_reward_pool(env: &Env) -> i128 {
    env.storage().instance().get(&RWD_POOL).unwrap_or(0)
}

fn put_reward_pool(env: &Env, amount: i128) {
    env.storage().instance().set(&RWD_POOL, &amount);
}

fn get_reward_distributed(env: &Env, user: &Address) -> i128 {
    let key = (RWD_DIST, user);
    env.storage().persistent().get(&key).unwrap_or(0)
}

fn put_reward_distributed(env: &Env, user: &Address, amount: i128) {
    let key = (RWD_DIST, user);
    env.storage().persistent().set(&key, &amount);
}

fn add_to_reward_distributed(env: &Env, user: &Address, amount: i128) {
    let current = get_reward_distributed(env, user);
    put_reward_distributed(env, user, current + amount);
}

// ============================================================================
// Score Computation
// ============================================================================

/// Compute individual score components from player stats.
fn compute_trade_score(trade_count: u32) -> i128 {
    let score = (trade_count as i128) * 2;
    if score > TRADE_SCORE_MAX {
        TRADE_SCORE_MAX
    } else {
        score
    }
}

fn compute_roi_score(roi_bps: i128) -> i128 {
    if roi_bps <= 0 {
        return 0;
    }
    // Scale: 100% ROI (10000 bps) = 30 points max
    let score = roi_bps * ROI_SCORE_MAX / 10_000;
    if score > ROI_SCORE_MAX {
        ROI_SCORE_MAX
    } else {
        score
    }
}

fn compute_streak_score(streak: u32) -> i128 {
    let score = (streak as i128) * 3;
    if score > STREAK_SCORE_MAX {
        STREAK_SCORE_MAX
    } else {
        score
    }
}

fn compute_learn_score(modules_completed: u32, total_modules: u32) -> i128 {
    if total_modules == 0 {
        return 0;
    }
    let score = (modules_completed as i128) * LEARN_SCORE_MAX / (total_modules as i128);
    if score > LEARN_SCORE_MAX {
        LEARN_SCORE_MAX
    } else {
        score
    }
}

fn compute_community_score(actions: u32) -> i128 {
    let score = (actions as i128) * 2;
    if score > COMMUNITY_SCORE_MAX {
        COMMUNITY_SCORE_MAX
    } else {
        score
    }
}

/// Compute total composite score (0-100).
fn compute_total_score(stats: &PlayerStats) -> i128 {
    let trade = compute_trade_score(stats.successful_trades);
    let roi = compute_roi_score(stats.best_roi_bps);
    let streak = compute_streak_score(stats.best_streak);
    let learn = compute_learn_score(stats.modules_completed, stats.total_modules);
    let community = compute_community_score(stats.community_actions);

    trade + roi + streak + learn + community
}

/// Determine tier from score.
fn score_to_tier(score: i128) -> ProgressionTier {
    if score >= TIER_PLATINUM {
        ProgressionTier::Platinum
    } else if score >= TIER_GOLD {
        ProgressionTier::Gold
    } else if score >= TIER_SILVER {
        ProgressionTier::Silver
    } else {
        ProgressionTier::Bronze
    }
}

/// Get reward amount for a tier.
fn tier_reward(tier: &ProgressionTier) -> i128 {
    match tier {
        ProgressionTier::Bronze => REWARD_BRONZE,
        ProgressionTier::Silver => REWARD_SILVER,
        ProgressionTier::Gold => REWARD_GOLD,
        ProgressionTier::Platinum => REWARD_PLATINUM,
    }
}

// ============================================================================
// Badge Evaluation
// ============================================================================

/// Evaluate which badges a player should earn based on their stats.
/// Returns a Vec of badge_ids that should be newly issued.
fn evaluate_badges(env: &Env, user: &Address, stats: &PlayerStats) -> soroban_sdk::Vec<Symbol> {
    let earned = get_user_badge_ids(env, user);
    let mut new_badges = soroban_sdk::Vec::new(env);

    let checks: &[(Symbol, bool)] = &[
        (symbol_short!("1ST_TRD"), stats.trade_count >= 1),
        (symbol_short!("TRD_10"), stats.successful_trades >= 10),
        (symbol_short!("TRD_50"), stats.successful_trades >= 50),
        (symbol_short!("TRD_100"), stats.successful_trades >= 100),
        (symbol_short!("POS_ROI"), stats.best_roi_bps > 0),
        (symbol_short!("ROI_50"), stats.best_roi_bps >= 5_000),
        (symbol_short!("ROI_100"), stats.best_roi_bps >= 10_000),
        (symbol_short!("STRK_3"), stats.best_streak >= 3),
        (symbol_short!("STRK_5"), stats.best_streak >= 5),
        (symbol_short!("STRK10"), stats.best_streak >= 10),
        (symbol_short!("LRN_1"), stats.modules_completed >= 1),
        (symbol_short!("LRN_5"), stats.modules_completed >= 5),
        (
            symbol_short!("LRN_ALL"),
            stats.total_modules > 0 && stats.modules_completed >= stats.total_modules,
        ),
        (symbol_short!("COM_1"), stats.community_actions >= 1),
        (
            symbol_short!("BRZ_TIR"),
            stats.tier == ProgressionTier::Bronze && stats.total_score < TIER_SILVER,
        ),
        (
            symbol_short!("SLV_TIR"),
            stats.tier == ProgressionTier::Silver,
        ),
        (
            symbol_short!("GLD_TIR"),
            stats.tier == ProgressionTier::Gold || stats.tier == ProgressionTier::Platinum,
        ),
        (
            symbol_short!("PLT_TIR"),
            stats.tier == ProgressionTier::Platinum,
        ),
    ];

    for (badge_id, condition) in checks.iter() {
        if *condition && !badge_already_earned(&earned, badge_id) {
            new_badges.push_back(badge_id.clone());
        }
    }

    new_badges
}

fn badge_already_earned(earned: &soroban_sdk::Vec<Symbol>, badge_id: &Symbol) -> bool {
    for id in earned.iter() {
        if id == *badge_id {
            return true;
        }
    }
    false
}

// ============================================================================
// Default Badge Definitions
// ============================================================================

fn create_default_badge_defs(env: &Env) -> soroban_sdk::Vec<BadgeDefinition> {
    let mut defs = soroban_sdk::Vec::new(env);

    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("1ST_TRD"),
        badge_type: BadgeType::FirstTrade,
        name: symbol_short!("FirstTrd"),
        description: symbol_short!("1st_trd"),
        reward_amount: 5,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("TRD_10"),
        badge_type: BadgeType::Trade10,
        name: symbol_short!("10_Trades"),
        description: symbol_short!("10_trds"),
        reward_amount: 10,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("TRD_50"),
        badge_type: BadgeType::Trade50,
        name: symbol_short!("50_Trades"),
        description: symbol_short!("50_trds"),
        reward_amount: 25,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("TRD100"),
        badge_type: BadgeType::Trade100,
        name: symbol_short!("100_Trds"),
        description: symbol_short!("100_trds"),
        reward_amount: 50,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("POS_ROI"),
        badge_type: BadgeType::PositiveRoi,
        name: symbol_short!("Pos_ROI"),
        description: symbol_short!("Pos_ROI"),
        reward_amount: 10,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("ROI_50"),
        badge_type: BadgeType::Roi50,
        name: symbol_short!("ROI_50"),
        description: symbol_short!("50pct_ROI"),
        reward_amount: 30,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("ROI100"),
        badge_type: BadgeType::Roi100,
        name: symbol_short!("ROI_100"),
        description: symbol_short!("100pct"),
        reward_amount: 50,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("STRK_3"),
        badge_type: BadgeType::Streak3,
        name: symbol_short!("Strk_3"),
        description: symbol_short!("3win"),
        reward_amount: 10,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("STRK_5"),
        badge_type: BadgeType::Streak5,
        name: symbol_short!("Strk_5"),
        description: symbol_short!("5win"),
        reward_amount: 20,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("STRK10"),
        badge_type: BadgeType::Streak10,
        name: symbol_short!("Strk_10"),
        description: symbol_short!("10win"),
        reward_amount: 40,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("LRN_1"),
        badge_type: BadgeType::Learn1,
        name: symbol_short!("LRN_1"),
        description: symbol_short!("1_module"),
        reward_amount: 5,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("LRN_5"),
        badge_type: BadgeType::Learn5,
        name: symbol_short!("LRN_5"),
        description: symbol_short!("5_modules"),
        reward_amount: 15,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("LRN_ALL"),
        badge_type: BadgeType::LearnAll,
        name: symbol_short!("LRN_ALL"),
        description: symbol_short!("All_mods"),
        reward_amount: 50,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("COM_1"),
        badge_type: BadgeType::Community1,
        name: symbol_short!("COM_1"),
        description: symbol_short!("1st_com"),
        reward_amount: 5,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("BRZ_TIR"),
        badge_type: BadgeType::BronzeTier,
        name: symbol_short!("Bronze"),
        description: symbol_short!("Brz_tier"),
        reward_amount: 10,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("SLV_TIR"),
        badge_type: BadgeType::SilverTier,
        name: symbol_short!("Silver"),
        description: symbol_short!("Slv_tier"),
        reward_amount: 25,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("GLD_TIR"),
        badge_type: BadgeType::GoldTier,
        name: symbol_short!("Gold"),
        description: symbol_short!("Gld_tier"),
        reward_amount: 50,
        active: true,
    });
    defs.push_back(BadgeDefinition {
        badge_id: symbol_short!("PLT_TIR"),
        badge_type: BadgeType::PlatinumTier,
        name: symbol_short!("Platinum"),
        description: symbol_short!("Plt_tier"),
        reward_amount: 100,
        active: true,
    });

    defs
}

// ============================================================================
// Leaderboard Sort Helper
// ============================================================================

/// Build a sorted leaderboard page from all user stats.
fn build_leaderboard(
    env: &Env,
    metric: SortMetric,
    _window: TimeWindow,
    offset: u32,
    limit: u32,
) -> LeaderboardPage {
    let user_ids = get_user_ids(env);
    let _total = user_ids.len();

    // Collect all entries with their sort keys
    let mut entries: soroban_sdk::Vec<(Address, i128, i128, u32, i128, u32)> =
        soroban_sdk::Vec::new(env);
    for user in user_ids.iter() {
        let stats = get_player_stats(env, &user);
        if let Some(s) = stats {
            let sort_key = match metric {
                SortMetric::Score => s.total_score,
                SortMetric::TradeCount => s.successful_trades as i128,
                SortMetric::Roi => s.best_roi_bps,
                SortMetric::Streak => s.best_streak as i128,
            };
            entries.push_back((
                user,
                sort_key,
                s.total_score,
                s.successful_trades,
                s.best_roi_bps,
                s.best_streak,
            ));
        }
    }

    // Simple insertion sort (fine for typical leaderboard sizes)
    let len = entries.len();
    let mut i: u32 = 1;
    while i < len {
        let key_entry = entries.get(i).unwrap();
        let mut j = i;
        while j > 0 {
            let prev = entries.get(j - 1).unwrap();
            // Sort descending
            if key_entry.1 > prev.1 {
                entries.set(j, prev);
                entries.set(j - 1, key_entry.clone());
                j -= 1;
            } else {
                break;
            }
        }
        i += 1;
    }

    let total_players = entries.len();
    let actual_offset = if offset >= total_players {
        total_players
    } else {
        offset
    };
    let actual_limit = if limit > MAX_LEADERBOARD_RETURN {
        MAX_LEADERBOARD_RETURN
    } else {
        limit
    };

    let mut result = soroban_sdk::Vec::new(env);
    let mut rank = actual_offset + 1;
    let mut idx = actual_offset;
    let end = if actual_offset + actual_limit > total_players {
        total_players
    } else {
        actual_offset + actual_limit
    };

    while idx < end {
        let e = entries.get(idx).unwrap();
        let tier = score_to_tier(e.2);
        result.push_back(LeaderboardEntry {
            rank,
            user: e.0,
            score: e.2,
            tier,
            trade_count: e.3,
            best_roi_bps: e.4,
            streak: e.5,
        });
        rank += 1;
        idx += 1;
    }

    LeaderboardPage {
        entries: result,
        total_players,
        page_offset: actual_offset,
        page_limit: actual_limit,
    }
}

// ============================================================================
// Contract
// ============================================================================

/// GamificationEngine contract for SwapTrade
/// Manages achievement badges, leaderboards, streaks, challenges,
/// rewards, and progression tiers.
#[contract]
pub struct GamificationEngine;

#[contractimpl]
impl GamificationEngine {
    // -----------------------------------------------------------------------
    // Initialization
    // -----------------------------------------------------------------------

    /// Initialize the gamification engine.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `admin` - Address of the contract administrator
    /// * `total_modules` - Total number of learning modules in the system
    ///
    /// # Returns
    /// Success symbol if initialization succeeds
    pub fn initialize(env: Env, admin: Address, total_modules: u32) -> Symbol {
        if is_initialized(&env) {
            soroban_sdk::panic_with_error!(&env, Error::AlreadyInitialized);
        }
        put_admin(&env, &admin);
        put_reward_pool(&env, 1_000_000);

        // Store total modules for learning score computation
        let key = symbol_short!("TOT_MOD");
        env.storage().instance().set(&key, &total_modules);

        // Initialize default badge definitions
        let defs = create_default_badge_defs(&env);
        let mut def_ids = soroban_sdk::Vec::new(&env);
        for def in defs.iter() {
            def_ids.push_back(def.badge_id.clone());
        }
        put_badge_definitions(&env, &defs);
        put_badge_def_ids(&env, &def_ids);

        symbol_short!("ok")
    }

    // -----------------------------------------------------------------------
    // User Management
    // -----------------------------------------------------------------------

    /// Register a new user in the gamification system.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `user` - Address of the user to register
    ///
    /// # Returns
    /// Success symbol
    pub fn register_user(env: Env, user: Address) -> Symbol {
        if !is_initialized(&env) {
            soroban_sdk::panic_with_error!(&env, Error::NotInitialized);
        }
        user.require_auth();

        // Check not already registered
        let existing = get_player_stats(&env, &user);
        if existing.is_some() {
            soroban_sdk::panic_with_error!(&env, Error::UserAlreadyRegistered);
        }

        let total_modules_key = symbol_short!("TOT_MOD");
        let total_modules: u32 = env
            .storage()
            .instance()
            .get(&total_modules_key)
            .unwrap_or(0);

        let stats = PlayerStats {
            user: user.clone(),
            trade_count: 0,
            successful_trades: 0,
            best_roi_bps: 0,
            current_streak: 0,
            best_streak: 0,
            modules_completed: 0,
            total_modules,
            community_actions: 0,
            challenges_won: 0,
            tier: ProgressionTier::Bronze,
            total_score: 0,
            last_active: env.ledger().timestamp(),
        };
        put_player_stats(&env, &stats);
        add_user_id(&env, &user);

        symbol_short!("ok")
    }

    // -----------------------------------------------------------------------
    // Trade Tracking
    // -----------------------------------------------------------------------

    /// Record a completed trade for a user. If the trade was profitable
    /// (roi_bps > 0), the streak increments; otherwise it resets.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `user` - Address of the trader
    /// * `roi_bps` - Return on investment in basis points (e.g. 500 = 5%)
    /// * `successful` - Whether the trade was profitable
    ///
    /// # Returns
    /// Updated score
    pub fn record_trade(env: Env, user: Address, roi_bps: i128, successful: bool) -> i128 {
        user.require_auth();
        let mut stats = get_player_stats(&env, &user)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::UserNotFound));

        stats.trade_count += 1;
        if successful {
            stats.successful_trades += 1;
            stats.current_streak += 1;
            if stats.current_streak > stats.best_streak {
                stats.best_streak = stats.current_streak;
            }
        } else {
            stats.current_streak = 0;
        }

        if roi_bps > stats.best_roi_bps {
            stats.best_roi_bps = roi_bps;
        }

        stats.last_active = env.ledger().timestamp();
        stats.total_score = compute_total_score(&stats);
        stats.tier = score_to_tier(stats.total_score);

        put_player_stats(&env, &stats);
        put_score(&env, &user, stats.total_score);

        // Emit trade recorded event
        env.events().publish(
            (symbol_short!("TRADE"), &user),
            (roi_bps, successful, stats.total_score),
        );

        stats.total_score
    }

    /// Record multiple trades in a batch for efficiency.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `user` - Address of the trader
    /// * `rois` - Vec of (roi_bps, successful) tuples
    ///
    /// # Returns
    /// Updated score
    pub fn record_trades_batch(
        env: Env,
        user: Address,
        rois: soroban_sdk::Vec<(i128, bool)>,
    ) -> i128 {
        user.require_auth();
        let mut stats = get_player_stats(&env, &user)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::UserNotFound));

        for trade in rois.iter() {
            stats.trade_count += 1;
            if trade.1 {
                // successful
                stats.successful_trades += 1;
                stats.current_streak += 1;
                if stats.current_streak > stats.best_streak {
                    stats.best_streak = stats.current_streak;
                }
            } else {
                stats.current_streak = 0;
            }
            if trade.0 > stats.best_roi_bps {
                stats.best_roi_bps = trade.0;
            }
        }

        stats.last_active = env.ledger().timestamp();
        stats.total_score = compute_total_score(&stats);
        stats.tier = score_to_tier(stats.total_score);

        put_player_stats(&env, &stats);
        put_score(&env, &user, stats.total_score);

        stats.total_score
    }

    // -----------------------------------------------------------------------
    // Learning Progress
    // -----------------------------------------------------------------------

    /// Record completion of a learning module.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `user` - Address of the learner
    ///
    /// # Returns
    /// Updated score
    pub fn complete_learning_module(env: Env, user: Address) -> i128 {
        user.require_auth();
        let mut stats = get_player_stats(&env, &user)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::UserNotFound));

        if stats.modules_completed < stats.total_modules {
            stats.modules_completed += 1;
        }

        stats.last_active = env.ledger().timestamp();
        stats.total_score = compute_total_score(&stats);
        stats.tier = score_to_tier(stats.total_score);

        put_player_stats(&env, &stats);
        put_score(&env, &user, stats.total_score);

        env.events().publish(
            (symbol_short!("LEARN"), &user),
            (stats.modules_completed, stats.total_score),
        );

        stats.total_score
    }

    // -----------------------------------------------------------------------
    // Community Participation
    // -----------------------------------------------------------------------

    /// Record a community action (e.g., sharing, helping others, governance).
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `user` - Address of the participant
    ///
    /// # Returns
    /// Updated score
    pub fn record_community_action(env: Env, user: Address) -> i128 {
        user.require_auth();
        let mut stats = get_player_stats(&env, &user)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::UserNotFound));

        stats.community_actions += 1;
        stats.last_active = env.ledger().timestamp();
        stats.total_score = compute_total_score(&stats);
        stats.tier = score_to_tier(stats.total_score);

        put_player_stats(&env, &stats);
        put_score(&env, &user, stats.total_score);

        env.events().publish(
            (symbol_short!("COMM"), &user),
            (stats.community_actions, stats.total_score),
        );

        stats.total_score
    }

    // -----------------------------------------------------------------------
    // Badge System
    // -----------------------------------------------------------------------

    /// Check and issue any pending badges for a user based on their current
    /// stats. This is the main entry point for badge evaluation.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `user` - Address of the user
    ///
    /// # Returns
    /// Vec of newly earned badge IDs
    pub fn check_and_issue_badges(env: Env, user: Address) -> soroban_sdk::Vec<Symbol> {
        let stats = get_player_stats(&env, &user)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::UserNotFound));

        let new_badge_ids = evaluate_badges(&env, &user, &stats);
        let mut earned_ids = get_user_badge_ids(&env, &user);
        let badge_defs = get_badge_definitions(&env);
        let timestamp = env.ledger().timestamp();

        for badge_id in new_badge_ids.iter() {
            // Find the badge definition for reward amount
            let reward = find_badge_reward(&badge_defs, &badge_id);

            let record = BadgeRecord {
                badge_id: badge_id.clone(),
                user: user.clone(),
                earned_at: timestamp,
                score_at_earn: stats.total_score,
            };
            put_badge_record(&env, &record);
            earned_ids.push_back(badge_id.clone());

            // Distribute badge reward
            if reward > 0 {
                let pool = get_reward_pool(&env);
                if pool >= reward {
                    put_reward_pool(&env, pool - reward);
                    add_to_reward_distributed(&env, &user, reward);
                }
            }

            env.events()
                .publish((symbol_short!("BADGE"), &user), (&badge_id, reward));
        }

        put_user_badge_ids(&env, &user, &earned_ids);
        earned_ids
    }

    /// Manually issue a specific badge to a user (admin-only).
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `admin` - Admin address
    /// * `user` - Target user
    /// * `badge_id` - Badge to issue
    ///
    /// # Returns
    /// Success symbol
    pub fn issue_badge(env: Env, admin: Address, user: Address, badge_id: Symbol) -> Symbol {
        admin.require_auth();
        if admin != get_admin(&env) {
            soroban_sdk::panic_with_error!(&env, Error::AdminRequired);
        }

        let earned = get_user_badge_ids(&env, &user);
        if badge_already_earned(&earned, &badge_id) {
            soroban_sdk::panic_with_error!(&env, Error::BadgeAlreadyEarned);
        }

        let stats = get_player_stats(&env, &user)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::UserNotFound));

        let badge_defs = get_badge_definitions(&env);
        let reward = find_badge_reward(&badge_defs, &badge_id);

        let record = BadgeRecord {
            badge_id: badge_id.clone(),
            user: user.clone(),
            earned_at: env.ledger().timestamp(),
            score_at_earn: stats.total_score,
        };
        put_badge_record(&env, &record);

        let mut new_earned = earned;
        new_earned.push_back(badge_id.clone());
        put_user_badge_ids(&env, &user, &new_earned);

        if reward > 0 {
            let pool = get_reward_pool(&env);
            if pool >= reward {
                put_reward_pool(&env, pool - reward);
                add_to_reward_distributed(&env, &user, reward);
            }
        }

        env.events()
            .publish((symbol_short!("BDG_IS"), &user), (&badge_id, reward));

        symbol_short!("ok")
    }

    /// Get a badge record for a specific user and badge.
    pub fn get_badge_record(env: Env, user: Address, badge_id: Symbol) -> Option<BadgeRecord> {
        get_badge_record(&env, &user, &badge_id)
    }

    /// Get all badge IDs earned by a user.
    pub fn get_user_badges(env: Env, user: Address) -> soroban_sdk::Vec<Symbol> {
        get_user_badge_ids(&env, &user)
    }

    /// Get all badge definitions.
    pub fn get_badge_definitions_list(env: Env) -> soroban_sdk::Vec<BadgeDefinition> {
        get_badge_definitions(&env)
    }

    // -----------------------------------------------------------------------
    // Player Stats & Score
    // -----------------------------------------------------------------------

    /// Get comprehensive player statistics.
    pub fn get_player_stats(env: Env, user: Address) -> Option<PlayerStats> {
        get_player_stats(&env, &user)
    }

    /// Get a player's composite score.
    pub fn get_score(env: Env, user: Address) -> i128 {
        get_score(&env, &user)
    }

    /// Get a player's progression tier.
    pub fn get_tier(env: Env, user: Address) -> ProgressionTier {
        let stats = get_player_stats(&env, &user);
        match stats {
            Some(s) => s.tier,
            None => ProgressionTier::Bronze,
        }
    }

    /// Compute score components breakdown for a user.
    pub fn get_score_breakdown(env: Env, user: Address) -> (i128, i128, i128, i128, i128, i128) {
        let stats = get_player_stats(&env, &user)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::UserNotFound));

        let trade = compute_trade_score(stats.successful_trades);
        let roi = compute_roi_score(stats.best_roi_bps);
        let streak = compute_streak_score(stats.best_streak);
        let learn = compute_learn_score(stats.modules_completed, stats.total_modules);
        let community = compute_community_score(stats.community_actions);
        let total = trade + roi + streak + learn + community;

        (trade, roi, streak, learn, community, total)
    }

    // -----------------------------------------------------------------------
    // Leaderboard
    // -----------------------------------------------------------------------

    /// Get the global leaderboard sorted by score.
    pub fn get_leaderboard(
        env: Env,
        metric: SortMetric,
        window: TimeWindow,
        offset: u32,
        limit: u32,
    ) -> LeaderboardPage {
        build_leaderboard(&env, metric, window, offset, limit)
    }

    /// Get the rank of a specific user.
    pub fn get_user_rank(env: Env, user: Address, metric: SortMetric) -> Option<u32> {
        let page = build_leaderboard(&env, metric, TimeWindow::AllTime, 0, MAX_USERS_RETURN);
        for entry in page.entries.iter() {
            if entry.user == user {
                return Some(entry.rank);
            }
        }
        None
    }

    /// Get top N players by score.
    pub fn get_top_players(env: Env, n: u32) -> soroban_sdk::Vec<LeaderboardEntry> {
        let page = build_leaderboard(
            &env,
            SortMetric::Score,
            TimeWindow::AllTime,
            0,
            if n > MAX_LEADERBOARD_RETURN {
                MAX_LEADERBOARD_RETURN
            } else {
                n
            },
        );
        page.entries
    }

    /// Get total number of registered players.
    pub fn get_total_players(env: Env) -> u32 {
        get_user_ids(&env).len()
    }

    // -----------------------------------------------------------------------
    // Streak Management
    // -----------------------------------------------------------------------

    /// Get a user's current and best streak.
    pub fn get_streak(env: Env, user: Address) -> (u32, u32) {
        let stats = get_player_stats(&env, &user)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::UserNotFound));
        (stats.current_streak, stats.best_streak)
    }

    // -----------------------------------------------------------------------
    // Challenge Campaigns
    // -----------------------------------------------------------------------

    /// Create a new challenge campaign (admin only).
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `admin` - Admin address
    /// * `challenge_id` - Unique identifier for the challenge
    /// * `name` - Display name
    /// * `description` - Challenge description
    /// * `target_metric` - Which metric to track
    /// * `target_value` - Target value to reach
    /// * `reward_amount` - Reward for completion
    /// * `start_time` - Challenge start timestamp
    /// * `end_time` - Challenge end timestamp
    ///
    /// # Returns
    /// Success symbol
    pub fn create_challenge(
        env: Env,
        admin: Address,
        challenge_id: Symbol,
        name: Symbol,
        description: Symbol,
        target_metric: SortMetric,
        target_value: i128,
        reward_amount: i128,
        start_time: u64,
        end_time: u64,
    ) -> Symbol {
        admin.require_auth();
        if admin != get_admin(&env) {
            soroban_sdk::panic_with_error!(&env, Error::AdminRequired);
        }

        if start_time >= end_time || target_value <= 0 || reward_amount <= 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidChallengeParams);
        }

        let challenge = Challenge {
            challenge_id: challenge_id.clone(),
            name,
            description,
            target_metric,
            target_value,
            reward_amount,
            start_time,
            end_time,
            active: true,
        };

        put_challenge(&env, &challenge);

        let mut ids = get_challenge_ids(&env);
        ids.push_back(challenge_id);
        put_challenge_ids(&env, &ids);

        symbol_short!("ok")
    }

    /// Join a challenge campaign.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `user` - User joining
    /// * `challenge_id` - Challenge to join
    ///
    /// # Returns
    /// Success symbol
    pub fn join_challenge(env: Env, user: Address, challenge_id: Symbol) -> Symbol {
        user.require_auth();

        let challenge = get_challenge(&env, &challenge_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ChallengeNotFound));

        let now = env.ledger().timestamp();
        if now < challenge.start_time {
            soroban_sdk::panic_with_error!(&env, Error::ChallengeNotStarted);
        }
        if now > challenge.end_time {
            soroban_sdk::panic_with_error!(&env, Error::ChallengeAlreadyEnded);
        }

        let existing = get_challenge_entry(&env, &challenge_id, &user);
        if existing.is_some() {
            soroban_sdk::panic_with_error!(&env, Error::ChallengeAlreadyJoined);
        }

        let entry = ChallengeEntry {
            challenge_id: challenge_id.clone(),
            user: user.clone(),
            current_value: 0,
            completed: false,
            completed_at: 0,
        };
        put_challenge_entry(&env, &entry);

        env.events()
            .publish((symbol_short!("CHL_JN"), &user), &challenge_id);

        symbol_short!("ok")
    }

    /// Update challenge progress for a user.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `user` - User whose progress to update
    /// * `challenge_id` - Challenge being tracked
    /// * `increment` - Amount to add to current progress
    ///
    /// # Returns
    /// Whether the challenge was completed
    pub fn update_challenge_progress(
        env: Env,
        user: Address,
        challenge_id: Symbol,
        increment: i128,
    ) -> bool {
        user.require_auth();

        let challenge = get_challenge(&env, &challenge_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ChallengeNotFound));

        if !challenge.active {
            soroban_sdk::panic_with_error!(&env, Error::ChallengeAlreadyEnded);
        }

        let now = env.ledger().timestamp();
        if now > challenge.end_time || now < challenge.start_time {
            soroban_sdk::panic_with_error!(&env, Error::ChallengeAlreadyEnded);
        }

        let mut entry = get_challenge_entry(&env, &challenge_id, &user)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ChallengeNotFound));

        entry.current_value += increment;

        let completed = entry.current_value >= challenge.target_value;
        if completed && !entry.completed {
            entry.completed = true;
            entry.completed_at = now;

            // Distribute reward
            let pool = get_reward_pool(&env);
            if pool >= challenge.reward_amount {
                put_reward_pool(&env, pool - challenge.reward_amount);
                add_to_reward_distributed(&env, &user, challenge.reward_amount);
            }

            // Update user stats
            let mut stats = get_player_stats(&env, &user)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::UserNotFound));
            stats.challenges_won += 1;
            stats.total_score = compute_total_score(&stats);
            stats.tier = score_to_tier(stats.total_score);
            put_player_stats(&env, &stats);
            put_score(&env, &user, stats.total_score);

            env.events().publish(
                (symbol_short!("CHL_CO"), &user),
                (&challenge_id, challenge.reward_amount),
            );
        }

        put_challenge_entry(&env, &entry);
        completed
    }

    /// Get a user's challenge entry.
    pub fn get_challenge_entry(
        env: Env,
        user: Address,
        challenge_id: Symbol,
    ) -> Option<ChallengeEntry> {
        get_challenge_entry(&env, &challenge_id, &user)
    }

    /// Get challenge details.
    pub fn get_challenge(env: Env, challenge_id: Symbol) -> Option<Challenge> {
        get_challenge(&env, &challenge_id)
    }

    /// Get all challenge IDs.
    pub fn get_challenge_ids(env: Env) -> soroban_sdk::Vec<Symbol> {
        get_challenge_ids(&env)
    }

    /// End a challenge (admin only).
    pub fn end_challenge(env: Env, admin: Address, challenge_id: Symbol) -> Symbol {
        admin.require_auth();
        if admin != get_admin(&env) {
            soroban_sdk::panic_with_error!(&env, Error::AdminRequired);
        }

        let mut challenge = get_challenge(&env, &challenge_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ChallengeNotFound));

        challenge.active = false;
        put_challenge(&env, &challenge);

        symbol_short!("ok")
    }

    // -----------------------------------------------------------------------
    // Reward System
    // -----------------------------------------------------------------------

    /// Get the current reward pool balance.
    pub fn get_reward_pool(env: Env) -> i128 {
        get_reward_pool(&env)
    }

    /// Get total rewards distributed to a user.
    pub fn get_reward_distributed(env: Env, user: Address) -> i128 {
        get_reward_distributed(&env, &user)
    }

    /// Distribute tier-based rewards to a user (can be called periodically).
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `user` - User to reward
    ///
    /// # Returns
    /// Amount of rewards distributed
    pub fn distribute_tier_reward(env: Env, user: Address) -> i128 {
        let stats = get_player_stats(&env, &user)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::UserNotFound));

        let amount = tier_reward(&stats.tier);
        let pool = get_reward_pool(&env);

        if pool < amount {
            soroban_sdk::panic_with_error!(&env, Error::InsufficientRewardPool);
        }

        put_reward_pool(&env, pool - amount);
        add_to_reward_distributed(&env, &user, amount);

        env.events()
            .publish((symbol_short!("RWD"), &user), (amount, stats.tier as u32));

        amount
    }

    /// Add tokens to the reward pool (admin only).
    pub fn fund_reward_pool(env: Env, admin: Address, amount: i128) -> i128 {
        admin.require_auth();
        if admin != get_admin(&env) {
            soroban_sdk::panic_with_error!(&env, Error::AdminRequired);
        }

        let current = get_reward_pool(&env);
        let new_total = current + amount;
        put_reward_pool(&env, new_total);
        new_total
    }

    // -----------------------------------------------------------------------
    // Badge Definitions (Admin)
    // -----------------------------------------------------------------------

    /// Update a badge definition (admin only).
    pub fn update_badge_definition(
        env: Env,
        admin: Address,
        badge_id: Symbol,
        name: Symbol,
        description: Symbol,
        reward_amount: i128,
        active: bool,
    ) -> Symbol {
        admin.require_auth();
        if admin != get_admin(&env) {
            soroban_sdk::panic_with_error!(&env, Error::AdminRequired);
        }

        let mut defs = get_badge_definitions(&env);
        let mut found = false;
        let mut idx: u32 = 0;
        while idx < defs.len() {
            let mut def = defs.get(idx).unwrap();
            if def.badge_id == badge_id {
                def.name = name;
                def.description = description;
                def.reward_amount = reward_amount;
                def.active = active;
                defs.set(idx, def);
                found = true;
                break;
            }
            idx += 1;
        }

        if !found {
            soroban_sdk::panic_with_error!(&env, Error::BadgeDefinitionNotFound);
        }

        put_badge_definitions(&env, &defs);
        symbol_short!("ok")
    }

    // -----------------------------------------------------------------------
    // Admin
    // -----------------------------------------------------------------------

    /// Get the admin address.
    pub fn get_admin(env: Env) -> Address {
        get_admin(&env)
    }

    /// Transfer admin role to a new address.
    pub fn transfer_admin(env: Env, new_admin: Address) -> Symbol {
        let admin = get_admin(&env);
        admin.require_auth();
        put_admin(&env, &new_admin);
        symbol_short!("ok")
    }

    /// Get list of all registered user addresses.
    pub fn get_all_users(env: Env) -> soroban_sdk::Vec<Address> {
        get_user_ids(&env)
    }
}

// ============================================================================
// Helper for badge reward lookup
// ============================================================================

fn find_badge_reward(defs: &soroban_sdk::Vec<BadgeDefinition>, badge_id: &Symbol) -> i128 {
    for def in defs.iter() {
        if def.badge_id == *badge_id {
            return def.reward_amount;
        }
    }
    0
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    // --- Scoring Tests ---

    #[test]
    fn test_score_components_trade() {
        assert_eq!(compute_trade_score(0), 0);
        assert_eq!(compute_trade_score(5), 10);
        assert_eq!(compute_trade_score(15), TRADE_SCORE_MAX);
    }

    #[test]
    fn test_score_components_roi() {
        assert_eq!(compute_roi_score(0), 0);
        assert_eq!(compute_roi_score(-500), 0);
        assert_eq!(compute_roi_score(5_000), 15);
        assert_eq!(compute_roi_score(10_000), ROI_SCORE_MAX);
        assert_eq!(compute_roi_score(15_000), ROI_SCORE_MAX);
    }

    #[test]
    fn test_score_components_streak() {
        assert_eq!(compute_streak_score(0), 0);
        assert_eq!(compute_streak_score(3), 9);
        assert_eq!(compute_streak_score(5), STREAK_SCORE_MAX);
        assert_eq!(compute_streak_score(10), STREAK_SCORE_MAX);
    }

    #[test]
    fn test_score_components_learn() {
        assert_eq!(compute_learn_score(0, 5), 0);
        assert_eq!(compute_learn_score(5, 5), LEARN_SCORE_MAX);
        assert_eq!(compute_learn_score(3, 5), 9);
    }

    #[test]
    fn test_score_components_community() {
        assert_eq!(compute_community_score(0), 0);
        assert_eq!(compute_community_score(5), COMMUNITY_SCORE_MAX);
        assert_eq!(compute_community_score(10), COMMUNITY_SCORE_MAX);
    }

    #[test]
    fn test_tier_progression() {
        assert_eq!(score_to_tier(0), ProgressionTier::Bronze);
        assert_eq!(score_to_tier(24), ProgressionTier::Bronze);
        assert_eq!(score_to_tier(25), ProgressionTier::Silver);
        assert_eq!(score_to_tier(49), ProgressionTier::Silver);
        assert_eq!(score_to_tier(50), ProgressionTier::Gold);
        assert_eq!(score_to_tier(74), ProgressionTier::Gold);
        assert_eq!(score_to_tier(75), ProgressionTier::Platinum);
        assert_eq!(score_to_tier(100), ProgressionTier::Platinum);
    }

    #[test]
    fn test_tier_rewards() {
        assert_eq!(tier_reward(&ProgressionTier::Bronze), REWARD_BRONZE);
        assert_eq!(tier_reward(&ProgressionTier::Silver), REWARD_SILVER);
        assert_eq!(tier_reward(&ProgressionTier::Gold), REWARD_GOLD);
        assert_eq!(tier_reward(&ProgressionTier::Platinum), REWARD_PLATINUM);
    }

    #[test]
    fn test_constants() {
        assert_eq!(MAX_LEADERBOARD_RETURN, 100);
        assert_eq!(TRADE_SCORE_MAX, 30);
        assert_eq!(ROI_SCORE_MAX, 30);
        assert_eq!(STREAK_SCORE_MAX, 15);
        assert_eq!(LEARN_SCORE_MAX, 15);
        assert_eq!(COMMUNITY_SCORE_MAX, 10);
        assert_eq!(TIER_SILVER, 25);
        assert_eq!(TIER_GOLD, 50);
        assert_eq!(TIER_PLATINUM, 75);
    }

    #[test]
    fn test_error_codes() {
        assert_eq!(Error::AlreadyInitialized as u32, 1);
        assert_eq!(Error::NotInitialized as u32, 2);
        assert_eq!(Error::UserAlreadyRegistered as u32, 3);
        assert_eq!(Error::UserNotFound as u32, 4);
        assert_eq!(Error::BadgeAlreadyEarned as u32, 5);
        assert_eq!(Error::BadgeNotFound as u32, 6);
        assert_eq!(Error::ChallengeNotFound as u32, 7);
        assert_eq!(Error::ChallengeAlreadyEnded as u32, 8);
        assert_eq!(Error::ChallengeNotStarted as u32, 9);
        assert_eq!(Error::ChallengeAlreadyJoined as u32, 10);
        assert_eq!(Error::InvalidChallengeParams as u32, 11);
        assert_eq!(Error::InsufficientRewardPool as u32, 12);
        assert_eq!(Error::ArithmeticOverflow as u32, 13);
        assert_eq!(Error::AdminRequired as u32, 14);
        assert_eq!(Error::BadgeDefinitionNotFound as u32, 15);
    }

    #[test]
    fn test_find_badge_reward() {
        // Verify find_badge_reward helper works
        let env = Env::default();
        let defs = create_default_badge_defs(&env);
        assert!(find_badge_reward(&defs, &symbol_short!("1ST_TRD")) > 0);
        assert_eq!(find_badge_reward(&defs, &symbol_short!("NOPE")), 0);
    }

    #[test]
    fn test_progression_tier_ordering() {
        // Verify PartialOrd works correctly for tiers
        assert!(ProgressionTier::Bronze < ProgressionTier::Silver);
        assert!(ProgressionTier::Silver < ProgressionTier::Gold);
        assert!(ProgressionTier::Gold < ProgressionTier::Platinum);
        assert!(ProgressionTier::Platinum > ProgressionTier::Bronze);
    }

    #[test]
    fn test_tier_badge_conditions() {
        let env = Env::default();
        let contract_id = env.register_contract(None, GamificationEngine);
        let user = Address::generate(&env);

        env.as_contract(&contract_id, || {
            let mut stats = PlayerStats {
                user: user.clone(),
                trade_count: 0,
                successful_trades: 0,
                best_roi_bps: 0,
                current_streak: 0,
                best_streak: 0,
                modules_completed: 0,
                total_modules: 0,
                community_actions: 0,
                challenges_won: 0,
                tier: ProgressionTier::Bronze,
                total_score: 10,
                last_active: 0,
            };

            // Bronze tier with score < TIER_SILVER earns BRZ_TIR but not SLV_TIR
            let badges = evaluate_badges(&env, &user, &stats);
            assert!(badges.contains(symbol_short!("BRZ_TIR")));
            assert!(!badges.contains(symbol_short!("SLV_TIR")));

            // Silver tier earns SLV_TIR but not BRZ_TIR
            stats.tier = ProgressionTier::Silver;
            stats.total_score = 30;
            let badges = evaluate_badges(&env, &user, &stats);
            assert!(!badges.contains(symbol_short!("BRZ_TIR")));
            assert!(badges.contains(symbol_short!("SLV_TIR")));

            // Gold tier earns GLD_TIR, not BRZ_TIR or SLV_TIR
            stats.tier = ProgressionTier::Gold;
            stats.total_score = 60;
            let badges = evaluate_badges(&env, &user, &stats);
            assert!(!badges.contains(symbol_short!("BRZ_TIR")));
            assert!(!badges.contains(symbol_short!("SLV_TIR")));
            assert!(badges.contains(symbol_short!("GLD_TIR")));

            // Platinum tier earns PLT_TIR and GLD_TIR, not BRZ_TIR or SLV_TIR
            stats.tier = ProgressionTier::Platinum;
            stats.total_score = 85;
            let badges = evaluate_badges(&env, &user, &stats);
            assert!(!badges.contains(symbol_short!("BRZ_TIR")));
            assert!(!badges.contains(symbol_short!("SLV_TIR")));
            assert!(badges.contains(symbol_short!("GLD_TIR")));
            assert!(badges.contains(symbol_short!("PLT_TIR")));
        });
    }
}
