use super::{BuildBudget, DEFAULT_BUILD_BUDGET_MS, LANDING_ALLOWANCE_MS, MAX_STAGE_WALL_MS};

#[test]
fn build_and_stage_share_one_wall_clock() {
    let budget = BuildBudget::new(1_000, None);
    assert_eq!(budget.report(1_000).budget_ms, DEFAULT_BUILD_BUDGET_MS);
    assert_eq!(budget.stage_wall_ms(1_000), MAX_STAGE_WALL_MS);
    assert_eq!(budget.stage_wall_ms(2_100_000), 1_501_000);
    assert_eq!(budget.remaining_ms(3_601_000), 0);
    assert!(budget.is_exhausted(3_601_000));
}

#[test]
fn provider_wait_pauses_the_build_clock() {
    let mut budget = BuildBudget::new(0, Some(10_000));
    assert!(budget.pause(2_000));
    assert!(!budget.pause(3_000));
    assert_eq!(budget.used_ms(8_000), 2_000);
    assert!(budget.resume(8_000));
    assert_eq!(budget.report(9_000).used_ms, 3_000);
    assert_eq!(budget.report(9_000).paused_ms, 6_000);
}

#[test]
fn landing_has_its_own_ten_minute_allowance_and_excludes_waits() {
    let mut budget = BuildBudget::new(0, Some(50_000));
    assert!(budget.begin_landing(40_000));
    assert!(!budget.begin_landing(41_000));
    assert_eq!(
        budget.landing_remaining_ms(40_000),
        Some(LANDING_ALLOWANCE_MS)
    );
    budget.pause(60_000);
    assert_eq!(
        budget.landing_remaining_ms(160_000),
        Some(LANDING_ALLOWANCE_MS - 20_000)
    );
    budget.resume(160_000);
    assert_eq!(
        budget.landing_remaining_ms(180_000),
        Some(LANDING_ALLOWANCE_MS - 40_000)
    );
}
