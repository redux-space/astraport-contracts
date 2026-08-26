//! Fee reporting and analytics.

use soroban_sdk::Symbol;

use super::records::{FeeCategory, FeeRecord, FeeSummary, PortfolioFeeReport};

pub fn build_fee_summary(records: &soroban_sdk::Vec<FeeRecord>) -> FeeSummary {
    let env = records.env();
    let mut total_collected: i128 = 0;
    let mut total_discounts: i128 = 0;
    let mut waived_count: u32 = 0;
    let mut cat_r: i128 = 0;
    let mut cat_y: i128 = 0;
    let mut cat_m: i128 = 0;
    let mut cat_t: i128 = 0;
    let mut cat_p: i128 = 0;
    let mut cat_c: i128 = 0;

    for record in records.iter() {
        total_collected += record.fee_amount;
        waived_count += if record.waived { 1 } else { 0 };
        let discount = if record.waived {
            record.base_amount
        } else {
            record.base_amount - record.fee_amount
        };
        if discount > 0 {
            total_discounts += discount;
        }
        match record.category {
            FeeCategory::Rebalancing => cat_r += record.fee_amount,
            FeeCategory::Yield => cat_y += record.fee_amount,
            FeeCategory::Management => cat_m += record.fee_amount,
            FeeCategory::Trading => cat_t += record.fee_amount,
            FeeCategory::Protocol => cat_p += record.fee_amount,
            FeeCategory::Custom => cat_c += record.fee_amount,
        }
    }

    let mut by_category = soroban_sdk::Vec::<(FeeCategory, i128)>::new(env);
    if cat_r > 0 {
        by_category.push_back((FeeCategory::Rebalancing, cat_r));
    }
    if cat_y > 0 {
        by_category.push_back((FeeCategory::Yield, cat_y));
    }
    if cat_m > 0 {
        by_category.push_back((FeeCategory::Management, cat_m));
    }
    if cat_t > 0 {
        by_category.push_back((FeeCategory::Trading, cat_t));
    }
    if cat_p > 0 {
        by_category.push_back((FeeCategory::Protocol, cat_p));
    }
    if cat_c > 0 {
        by_category.push_back((FeeCategory::Custom, cat_c));
    }

    FeeSummary {
        total_collected,
        total_events: records.len(),
        by_category,
        by_portfolio: soroban_sdk::Vec::new(env),
        total_discounts,
        waived_count,
    }
}

pub fn build_portfolio_breakdown(
    records: &soroban_sdk::Vec<FeeRecord>,
) -> soroban_sdk::Vec<(Symbol, i128)> {
    let env = records.env();
    let mut result = soroban_sdk::Vec::<(Symbol, i128)>::new(env);
    for record in records.iter() {
        let mut found = false;
        for i in 0..result.len() {
            let (pid, amount) = result.get(i).unwrap();
            if pid == record.portfolio_id {
                result.set(i, (pid, amount + record.fee_amount));
                found = true;
                break;
            }
        }
        if !found {
            result.push_back((record.portfolio_id.clone(), record.fee_amount));
        }
    }
    result
}

pub fn build_portfolio_report(
    portfolio_id: &Symbol,
    records: &soroban_sdk::Vec<FeeRecord>,
    assigned_fee_id: Symbol,
) -> PortfolioFeeReport {
    let env = records.env();
    let mut total_fees: i128 = 0;
    let mut event_count: u32 = 0;
    let mut by_category = soroban_sdk::Vec::<(FeeCategory, i128)>::new(env);
    let mut cat_r: i128 = 0;
    let mut cat_y: i128 = 0;
    let mut cat_m: i128 = 0;
    let mut cat_t: i128 = 0;
    let mut cat_p: i128 = 0;
    let mut cat_c: i128 = 0;

    for record in records.iter() {
        if &record.portfolio_id == portfolio_id {
            total_fees += record.fee_amount;
            event_count += 1;
            match record.category {
                FeeCategory::Rebalancing => cat_r += record.fee_amount,
                FeeCategory::Yield => cat_y += record.fee_amount,
                FeeCategory::Management => cat_m += record.fee_amount,
                FeeCategory::Trading => cat_t += record.fee_amount,
                FeeCategory::Protocol => cat_p += record.fee_amount,
                FeeCategory::Custom => cat_c += record.fee_amount,
            }
        }
    }

    if cat_r > 0 {
        by_category.push_back((FeeCategory::Rebalancing, cat_r));
    }
    if cat_y > 0 {
        by_category.push_back((FeeCategory::Yield, cat_y));
    }
    if cat_m > 0 {
        by_category.push_back((FeeCategory::Management, cat_m));
    }
    if cat_t > 0 {
        by_category.push_back((FeeCategory::Trading, cat_t));
    }
    if cat_p > 0 {
        by_category.push_back((FeeCategory::Protocol, cat_p));
    }
    if cat_c > 0 {
        by_category.push_back((FeeCategory::Custom, cat_c));
    }

    PortfolioFeeReport {
        portfolio_id: portfolio_id.clone(),
        total_fees,
        event_count,
        by_category,
        assigned_fee_id,
    }
}

pub fn count_matching(records: &soroban_sdk::Vec<FeeRecord>, category: FeeCategory) -> u32 {
    let mut count: u32 = 0;
    for r in records.iter() {
        if r.category == category {
            count += 1;
        }
    }
    count
}

pub fn has_portfolio_records(records: &soroban_sdk::Vec<FeeRecord>, portfolio_id: &Symbol) -> bool {
    for r in records.iter() {
        if &r.portfolio_id == portfolio_id {
            return true;
        }
    }
    false
}

pub fn count_waived(records: &soroban_sdk::Vec<FeeRecord>) -> u32 {
    let mut count: u32 = 0;
    for r in records.iter() {
        if r.waived {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{symbol_short, testutils::Address as _, Address, Env};

    fn make_record(
        env: &Env,
        fid: Symbol,
        category: FeeCategory,
        portfolio: Symbol,
        base: i128,
        fee: i128,
        discount_bps: i128,
        waived: bool,
    ) -> FeeRecord {
        FeeRecord {
            fee_id: fid,
            category,
            portfolio_id: portfolio,
            base_amount: base,
            fee_amount: fee,
            discount_bps,
            waived,
            timestamp: 1000,
            collector: Address::generate(env),
        }
    }

    #[test]
    fn test_summary_single_category() {
        let env = Env::default();
        let mut records = soroban_sdk::Vec::<FeeRecord>::new(&env);
        records.push_back(make_record(
            &env,
            symbol_short!("R"),
            FeeCategory::Rebalancing,
            symbol_short!("P1"),
            1_000_000,
            25_000,
            0,
            false,
        ));
        records.push_back(make_record(
            &env,
            symbol_short!("R"),
            FeeCategory::Rebalancing,
            symbol_short!("P1"),
            2_000_000,
            50_000,
            0,
            false,
        ));
        let summary = build_fee_summary(&records);
        assert_eq!(summary.total_collected, 75_000);
        assert_eq!(summary.total_events, 2);
        assert_eq!(summary.waived_count, 0);
    }

    #[test]
    fn test_summary_mixed_categories() {
        let env = Env::default();
        let mut records = soroban_sdk::Vec::<FeeRecord>::new(&env);
        records.push_back(make_record(
            &env,
            symbol_short!("R"),
            FeeCategory::Rebalancing,
            symbol_short!("P1"),
            1_000_000,
            25_000,
            0,
            false,
        ));
        records.push_back(make_record(
            &env,
            symbol_short!("Y"),
            FeeCategory::Yield,
            symbol_short!("P2"),
            500_000,
            5_000,
            0,
            false,
        ));
        records.push_back(make_record(
            &env,
            symbol_short!("M"),
            FeeCategory::Management,
            symbol_short!("P1"),
            10_000_000,
            10_000,
            0,
            false,
        ));
        let summary = build_fee_summary(&records);
        assert_eq!(summary.total_collected, 40_000);
        assert_eq!(summary.total_events, 3);
    }

    #[test]
    fn test_summary_with_waived() {
        let env = Env::default();
        let mut records = soroban_sdk::Vec::<FeeRecord>::new(&env);
        records.push_back(make_record(
            &env,
            symbol_short!("R"),
            FeeCategory::Rebalancing,
            symbol_short!("P1"),
            1_000_000,
            25_000,
            0,
            false,
        ));
        records.push_back(make_record(
            &env,
            symbol_short!("R"),
            FeeCategory::Rebalancing,
            symbol_short!("P2"),
            1_000_000,
            0,
            0,
            true,
        ));
        let summary = build_fee_summary(&records);
        assert_eq!(summary.total_collected, 25_000);
        assert_eq!(summary.waived_count, 1);
    }

    #[test]
    fn test_portfolio_breakdown() {
        let env = Env::default();
        let mut records = soroban_sdk::Vec::<FeeRecord>::new(&env);
        records.push_back(make_record(
            &env,
            symbol_short!("R"),
            FeeCategory::Rebalancing,
            symbol_short!("P1"),
            1_000_000,
            25_000,
            0,
            false,
        ));
        records.push_back(make_record(
            &env,
            symbol_short!("R"),
            FeeCategory::Rebalancing,
            symbol_short!("P2"),
            2_000_000,
            50_000,
            0,
            false,
        ));
        records.push_back(make_record(
            &env,
            symbol_short!("R"),
            FeeCategory::Rebalancing,
            symbol_short!("P1"),
            500_000,
            12_500,
            0,
            false,
        ));
        let breakdown = build_portfolio_breakdown(&records);
        assert_eq!(breakdown.len(), 2);
    }

    #[test]
    fn test_filter_by_category() {
        let env = Env::default();
        let mut records = soroban_sdk::Vec::<FeeRecord>::new(&env);
        records.push_back(make_record(
            &env,
            symbol_short!("R"),
            FeeCategory::Rebalancing,
            symbol_short!("P1"),
            1_000_000,
            25_000,
            0,
            false,
        ));
        records.push_back(make_record(
            &env,
            symbol_short!("Y"),
            FeeCategory::Yield,
            symbol_short!("P2"),
            500_000,
            5_000,
            0,
            false,
        ));
        assert_eq!(count_matching(&records, FeeCategory::Rebalancing), 1);
        assert_eq!(count_matching(&records, FeeCategory::Yield), 1);
    }

    #[test]
    fn test_filter_waived() {
        let env = Env::default();
        let mut records = soroban_sdk::Vec::<FeeRecord>::new(&env);
        records.push_back(make_record(
            &env,
            symbol_short!("R"),
            FeeCategory::Rebalancing,
            symbol_short!("P1"),
            1_000_000,
            25_000,
            0,
            false,
        ));
        records.push_back(make_record(
            &env,
            symbol_short!("R"),
            FeeCategory::Rebalancing,
            symbol_short!("P2"),
            1_000_000,
            0,
            0,
            true,
        ));
        assert_eq!(count_waived(&records), 1);
    }
}
