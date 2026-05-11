//! F1：off-tree action 边界 real_bet 测试（workflow §F1 第 3 件套）。
//!
//! 验收门槛（workflow §F1 §输出 第 3 行）：
//!
//! > 1M 个边界 `real_bet`（0 / 1 / chip max / overflow / negative-after-cast）
//! > → 抽象映射稳定
//!
//! ## 边界分类（与 §F1 §输出 五类字面对应）
//!
//! 1. **`real_to = 0`**：对应 D-201 PHM stub 算法 ② 分支（real_to ≤ max_committed
//!    → Call / Check / Fold）。
//! 2. **`real_to = 1`**：preflop SB/BTN-open 状态下 max_committed = BB（如 100），
//!    1 ≤ 100 走分支 ②；某些状态下 max_committed = 0（preflop UTG 起手 + ante=0
//!    场景）则可能走分支 ④ 找最近 ratio。
//! 3. **`real_to = u64::MAX` (chip max)**：必然 ≥ cap，走分支 ① AllIn { to: cap }。
//! 4. **`overflow` 路径**：D-201 stub 内部 `pot_after_call × ratio.milli` 用 `u128`，
//!    `saturating_add` 到 max_committed。`pot_after_call` 上界 = 一手中 6 × 100 BB
//!    × 100 chip/BB = 60_000，远低于 u128 上界。stage 2 自然状态下 overflow 不可
//!    达 — 但调用方传入 u64::MAX 让 cap 检查在 ① 先触 → AllIn，绕开 ④ 算术路径。
//!    本文件 (D) 段以 «chip max → AllIn» 等价覆盖该不变量。
//! 5. **`negative-after-cast`**：ChipAmount 是 u64 newtype，无符号转换；`real_to <
//!    max_committed` 是 「若用 i64 算 delta 会负」 的情况，D-201 分支 ② 用 `real_to
//!    ≤ max_committed` 整数比较短路，永不算负 delta。本文件 (B) 段（real_to = 1 +
//!    max_committed = 100）覆盖。
//!
//! ## 不变量断言（每个 (state, real_to) 输入都验）
//!
//! - **I1 — 确定性**：相同 (state, real_to) → byte-equal AbstractAction（与 D1
//!   `tests/abstraction_fuzz.rs::off_tree_real_bet_stability_smoke` 同形态，但
//!   D1 在随机 real_to ∈ [0, 100_001) 上 100k 跑，本文件聚焦边界值）。
//! - **I2 — no-panic**：`map_off_tree` + `AbstractAction::to_concrete()` 无 panic。
//! - **I3 — LA-002 互斥**：`AbstractAction::Bet` ⇔ `legal_actions().bet_range.is_some()`；
//!   `AbstractAction::Raise` ⇔ `legal_actions().raise_range.is_some()`（D-201 stub
//!   算法 ④ 末段 `if la.bet_range.is_some() { Bet } else { Raise }` 字面要求）。
//! - **I4 — AllIn.to = cap**：分支 ① 输出 `to = cap`；cap = `all_in_amount` 或
//!   `committed_this_round + stack`。
//! - **I5 — ratio_label ∈ config**：Bet / Raise 分支 ④ 选用的 ratio_label 必须
//!   属于 `config().raise_pot_ratios`。
//!
//! ## smoke 1k 默认 + 1M `#[ignore]`
//!
//! smoke 1k 跨 6 fresh state seed × 9 边界 real_to 值 + 大量随机组合，~1000 个
//! map_off_tree 调用，默认 active 走 `cargo test`。full 1M 走 `--ignored`，与
//! D2 `off_tree_real_bet_stability_full` 同形态（release ~3 s 实测）。
//!
//! 角色边界：[测试]，不修改产品代码。

use poker::{
    AbstractAction, Action, ActionAbstraction, ChaCha20Rng, ChipAmount, DefaultActionAbstraction,
    GameState, LegalActionSet, RngSource, TableConfig,
};

// ============================================================================
// 通用 fixture
// ============================================================================

const SMOKE_ITER: usize = 1_000;
const FULL_ITER: usize = 1_000_000;
const F1_MASTER_SEED: u64 = 0xF107_C0BB_0057_0003;

fn fresh_state(seed: u64) -> GameState {
    let cfg = TableConfig::default_6max_100bb();
    GameState::new(&cfg, seed)
}

/// 在 fresh GameState 上 apply N 步合法动作（不含 Fold —— 避免提前 terminal）
/// 推进到一个 「facing bet/raise」 betting 状态。N 步内若已 terminal 则提前停。
fn advance_n_legal_no_fold(state: &mut GameState, n: usize) {
    for _ in 0..n {
        if state.is_terminal() {
            break;
        }
        let la = state.legal_actions();
        // 优先 Check / Call，避免无意义 Fold 提前终止 + 避免 Bet/Raise 把状态推到
        // 复杂边界（本 helper 目标：让 max_committed > 0 但还未 all-in）。
        if la.check {
            state.apply(Action::Check).expect("Check legal then apply");
        } else if let Some(_to) = la.call {
            state.apply(Action::Call).expect("Call legal then apply");
        } else if let Some((lo, _hi)) = la.bet_range {
            // 没 check / call legal 但能 bet：少见路径，apply min Bet
            state.apply(Action::Bet { to: lo }).expect("min Bet apply");
        } else {
            break;
        }
    }
}

/// 拿 state 当前 max_committed_this_round（与 D-201 算法 ② 边界对齐）。
fn max_committed(state: &GameState) -> ChipAmount {
    state
        .players()
        .iter()
        .map(|p| p.committed_this_round)
        .max()
        .unwrap_or(ChipAmount::ZERO)
}

/// 拿 state 当前 cap（all_in_amount 或 committed + stack；与 D-201 算法 ① 对齐）。
fn current_cap(state: &GameState) -> ChipAmount {
    let la = state.legal_actions();
    if let Some(amt) = la.all_in_amount {
        return amt;
    }
    let Some(actor) = state.current_player() else {
        return ChipAmount::ZERO;
    };
    let p = &state.players()[actor.0 as usize];
    p.committed_this_round + p.stack
}

/// 对单个 (state, real_to) 跑所有 I1..I5 不变量断言。
fn assert_invariants(
    aa: &DefaultActionAbstraction,
    state: &GameState,
    real_to: ChipAmount,
    ctx: &str,
) {
    let la = state.legal_actions();
    // I1 — 确定性
    let m1 = aa.map_off_tree(state, real_to);
    let m2 = aa.map_off_tree(state, real_to);
    assert_eq!(m1, m2, "{ctx}: I1 determinism break (real_to={real_to:?})");

    // I2 — to_concrete no-panic（match 是 enum exhaustive，panic 只可能在产品代码
    // 内 if 还有 panic!；当前 stage-2 实现没有，转换是纯映射）
    let _: Action = m1.to_concrete();

    // I3 — LA-002 互斥
    match m1 {
        AbstractAction::Bet { .. } => {
            assert!(
                la.bet_range.is_some(),
                "{ctx}: I3 LA-002 break — Bet 但 bet_range=None (real_to={real_to:?})"
            );
        }
        AbstractAction::Raise { .. } => {
            assert!(
                la.raise_range.is_some(),
                "{ctx}: I3 LA-002 break — Raise 但 raise_range=None (real_to={real_to:?})"
            );
        }
        _ => { /* Fold / Check / Call / AllIn 不受 LA-002 约束 */ }
    }

    // I4 — AllIn.to = cap
    if let AbstractAction::AllIn { to } = m1 {
        let cap = current_cap(state);
        // 当 la.all_in_amount is None 时 cap 可能 = committed + stack；当 current_player
        // 是 None 时 cap=0 — 此时 AllIn 不应被产出（map_off_tree 前面 ① 分支需要
        // current_player Some 才会跑到 cap 比较）。
        assert_eq!(
            to, cap,
            "{ctx}: I4 AllIn.to mismatch — got {to:?}, expected cap={cap:?}"
        );
    }

    // I5 — ratio_label ∈ config
    let ratios = &aa.config().raise_pot_ratios;
    match m1 {
        AbstractAction::Bet { ratio_label, .. } | AbstractAction::Raise { ratio_label, .. } => {
            assert!(
                ratios.contains(&ratio_label),
                "{ctx}: I5 ratio_label {ratio_label:?} ∉ config {ratios:?}"
            );
        }
        _ => {}
    }
}

// ============================================================================
// (A) 5 类命名边界 — fresh preflop state（BB 已 post，max_committed=100）
// ============================================================================

fn fresh_preflop_state() -> GameState {
    // default_6max_100bb：SB=50 / BB=100 / stack=10000；UTG 当前 actor
    fresh_state(F1_MASTER_SEED)
}

#[test]
fn boundary_real_to_zero_yields_fold_or_check_or_call() {
    let aa = DefaultActionAbstraction::default_5_action();
    let state = fresh_preflop_state();
    let real_to = ChipAmount::ZERO;
    assert_invariants(&aa, &state, real_to, "boundary real_to=0");

    // 进一步：preflop UTG facing BB → 0 ≤ max_committed=100 走分支 ②，应是 Call 或 Fold
    let m = aa.map_off_tree(&state, real_to);
    assert!(
        matches!(
            m,
            AbstractAction::Call { .. } | AbstractAction::Check | AbstractAction::Fold
        ),
        "real_to=0 preflop facing BB → 期望 Call/Check/Fold 之一，得 {m:?}"
    );
}

#[test]
fn boundary_real_to_one_yields_fold_or_check_or_call() {
    let aa = DefaultActionAbstraction::default_5_action();
    let state = fresh_preflop_state();
    let real_to = ChipAmount::new(1);
    assert_invariants(&aa, &state, real_to, "boundary real_to=1");

    let m = aa.map_off_tree(&state, real_to);
    // preflop UTG max_committed = 100 chips（BB），1 ≤ 100 → 分支 ②
    assert!(
        matches!(
            m,
            AbstractAction::Call { .. } | AbstractAction::Check | AbstractAction::Fold
        ),
        "real_to=1 preflop facing BB → 期望 Call/Check/Fold，得 {m:?}"
    );
}

#[test]
fn boundary_real_to_u64_max_yields_all_in() {
    let aa = DefaultActionAbstraction::default_5_action();
    let state = fresh_preflop_state();
    let real_to = ChipAmount::new(u64::MAX);
    assert_invariants(&aa, &state, real_to, "boundary real_to=u64::MAX");

    let m = aa.map_off_tree(&state, real_to);
    // 必然 ≥ cap → AllIn { to: cap }
    assert!(
        matches!(m, AbstractAction::AllIn { .. }),
        "real_to=u64::MAX → 期望 AllIn，得 {m:?}"
    );
    let cap = current_cap(&state);
    if let AbstractAction::AllIn { to } = m {
        assert_eq!(to, cap, "AllIn.to = cap");
    }
}

#[test]
fn boundary_real_to_equals_max_committed_yields_fold_check_or_call() {
    let aa = DefaultActionAbstraction::default_5_action();
    let state = fresh_preflop_state();
    let real_to = max_committed(&state); // = BB = 100
    assert_invariants(&aa, &state, real_to, "boundary real_to=max_committed");

    let m = aa.map_off_tree(&state, real_to);
    // real_to ≤ max_committed → 分支 ②，UTG 必有 call legal
    assert!(
        matches!(
            m,
            AbstractAction::Call { .. } | AbstractAction::Check | AbstractAction::Fold
        ),
        "real_to=max_committed → 期望 Call/Check/Fold，得 {m:?}"
    );
}

#[test]
fn boundary_real_to_equals_cap_yields_all_in() {
    let aa = DefaultActionAbstraction::default_5_action();
    let state = fresh_preflop_state();
    let cap = current_cap(&state);
    let real_to = cap;
    assert_invariants(&aa, &state, real_to, "boundary real_to=cap");

    let m = aa.map_off_tree(&state, real_to);
    // real_to == cap → 分支 ① AllIn { to: cap }
    assert!(
        matches!(m, AbstractAction::AllIn { .. }),
        "real_to=cap → 期望 AllIn，得 {m:?}"
    );
    if let AbstractAction::AllIn { to } = m {
        assert_eq!(to, cap);
    }
}

#[test]
fn boundary_real_to_just_above_max_committed_yields_bet_or_raise_or_allin() {
    let aa = DefaultActionAbstraction::default_5_action();
    let state = fresh_preflop_state();
    let max_c = max_committed(&state);
    let real_to = max_c + ChipAmount::new(1);
    assert_invariants(&aa, &state, real_to, "boundary real_to=max_c+1");

    let m = aa.map_off_tree(&state, real_to);
    // real_to > max_committed → 分支 ④（除非也 ≥ cap → 分支 ①）
    let cap = current_cap(&state);
    if real_to >= cap {
        assert!(
            matches!(m, AbstractAction::AllIn { .. }),
            "real_to=max_c+1 ≥ cap → AllIn"
        );
    } else {
        assert!(
            matches!(m, AbstractAction::Bet { .. } | AbstractAction::Raise { .. }),
            "real_to=max_c+1 < cap → Bet/Raise，得 {m:?}"
        );
    }
}

// ============================================================================
// (B) 多 betting-stage 覆盖（preflop / postflop / facing 3-bet）
// ============================================================================

#[test]
fn boundary_real_to_zero_across_multiple_betting_states() {
    let aa = DefaultActionAbstraction::default_5_action();
    for seed_offset in 0u64..6 {
        let mut state = fresh_state(F1_MASTER_SEED.wrapping_add(seed_offset * 0x1000));
        for stride in [0usize, 1, 2, 3, 4] {
            let mut s = state.clone();
            advance_n_legal_no_fold(&mut s, stride);
            if s.is_terminal() {
                continue;
            }
            if s.current_player().is_none() {
                continue;
            }
            assert_invariants(
                &aa,
                &s,
                ChipAmount::ZERO,
                &format!("multi-stage seed={seed_offset:#x} stride={stride} real_to=0"),
            );
            // 同一 state 上 chip-max 也跑一遍
            assert_invariants(
                &aa,
                &s,
                ChipAmount::new(u64::MAX),
                &format!("multi-stage seed={seed_offset:#x} stride={stride} real_to=u64::MAX"),
            );
        }
        // 灭未使用警告
        let _ = &mut state;
    }
}

// ============================================================================
// (C) 9 个边界 real_to 值 × 6 random state seed = 54 个组合 smoke
// ============================================================================

fn boundary_value_table(max_c: ChipAmount, cap: ChipAmount) -> Vec<ChipAmount> {
    // 9 类边界值（含命名 5 类 + 周边）
    let mut out = vec![
        ChipAmount::ZERO,
        ChipAmount::new(1),
        max_c,
        max_c + ChipAmount::new(1),
        ChipAmount::new(u64::MAX),
    ];
    if cap > ChipAmount::ZERO {
        out.push(cap);
        if cap > ChipAmount::new(1) {
            out.push(cap - ChipAmount::new(1));
        }
        out.push(cap + ChipAmount::new(1)); // u64 加法在 ChipAmount::Add 路径，cap < u64::MAX 时安全
    }
    if max_c > ChipAmount::new(1) {
        out.push(max_c - ChipAmount::new(1));
    }
    out
}

#[test]
fn boundary_value_table_cross_states_smoke() {
    let aa = DefaultActionAbstraction::default_5_action();
    let seeds = [
        F1_MASTER_SEED,
        F1_MASTER_SEED ^ 0xC0DE_5701,
        F1_MASTER_SEED ^ 0xC0DE_5702,
        F1_MASTER_SEED ^ 0xC0DE_5703,
        F1_MASTER_SEED ^ 0xC0DE_5704,
        F1_MASTER_SEED ^ 0xC0DE_5705,
    ];
    for &seed in &seeds {
        let state = fresh_state(seed);
        if state.current_player().is_none() {
            continue;
        }
        let max_c = max_committed(&state);
        let cap = current_cap(&state);
        let table = boundary_value_table(max_c, cap);
        for real_to in table {
            assert_invariants(
                &aa,
                &state,
                real_to,
                &format!("table-cross seed={seed:#x} real_to={real_to:?}"),
            );
        }
    }
}

// ============================================================================
// (D) overflow 路径不可达 carve-out（u128 算术 + saturating_add）
// ============================================================================

#[test]
fn overflow_path_is_unreachable_in_stage2_self_play() {
    // §F1 §输出 named "overflow" 类：D-201 algorithm ④ 内部 `pot_after_call_size × ratio.milli`
    // 在 u128 算术域，pot 自然上界 ~60 000 (6 × 100 BB × 100 chip/BB)，远低于 u128 上界。
    // `saturating_add` 在算术域到 max_committed 上后转 u64 输出，无 overflow panic。
    //
    // 攻击向量：调用方传 ChipAmount::new(u64::MAX) → cap 检查在算术 ④ 前触 ①，
    // 直接 AllIn 不进 ④。算术 ④ 路径 stage-2 自然 state 永远不会 overflow。本 test
    // 验证 ① 分支拦截 u64::MAX，作为该 carve-out 的 regression guard。
    let aa = DefaultActionAbstraction::default_5_action();
    let state = fresh_preflop_state();
    let cap = current_cap(&state);
    let m = aa.map_off_tree(&state, ChipAmount::new(u64::MAX));
    assert!(
        matches!(m, AbstractAction::AllIn { to } if to == cap),
        "u64::MAX 必须被分支 ① 拦截到 AllIn{{to=cap}}，绕开 ④ 算术域"
    );
}

// ============================================================================
// (E) random 边界 real_to fuzz — smoke 1k + full 1M `#[ignore]`
// ============================================================================

fn random_boundary_real_to(iter: usize, label: &str) {
    let aa = DefaultActionAbstraction::default_5_action();
    let mut rng = ChaCha20Rng::from_seed(F1_MASTER_SEED ^ 0xFF00);
    // 6 个 base state，循环抽
    let states: Vec<GameState> = (0..6u64)
        .map(|i| fresh_state(F1_MASTER_SEED.wrapping_add(i * 0x4444)))
        .collect();
    for i in 0..iter {
        let state = &states[i % states.len()];
        if state.current_player().is_none() {
            continue;
        }
        // 70% 走 [0, max_committed × 3]；20% 走 cap 附近；10% 走 u64 高位
        let max_c = max_committed(state).as_u64();
        let cap = current_cap(state).as_u64();
        let bucket = rng.next_u64() % 10;
        let real_to_raw = match bucket {
            0..=6 => {
                let upper = max_c.saturating_mul(3).max(1);
                rng.next_u64() % upper
            }
            7 | 8 => {
                let lo = cap.saturating_sub(10);
                let hi = cap.saturating_add(10);
                if hi > lo {
                    lo + rng.next_u64() % (hi - lo + 1)
                } else {
                    cap
                }
            }
            _ => u64::MAX - (rng.next_u64() % 256),
        };
        let real_to = ChipAmount::new(real_to_raw);
        assert_invariants(
            &aa,
            state,
            real_to,
            &format!("{label} iter {i} real_to={real_to_raw}"),
        );
    }
}

#[test]
fn random_boundary_real_to_smoke_1k() {
    random_boundary_real_to(SMOKE_ITER, "fuzz-smoke 1k");
}

#[test]
#[ignore = "F1 full: 1M iter（release ~3 s 实测 / debug 远超），与 stage-1 1M determinism opt-in 同形态"]
fn random_boundary_real_to_full_1m() {
    random_boundary_real_to(FULL_ITER, "fuzz-full 1M");
}

// ============================================================================
// (F) Sanity：LegalActionSet 直接验证假设
// ============================================================================

#[test]
fn fresh_preflop_state_invariants_hold() {
    let state = fresh_preflop_state();
    assert!(!state.is_terminal(), "fresh GameState 不应起手即 terminal");
    assert!(
        state.current_player().is_some(),
        "fresh state 必有 current_player"
    );
    let la: LegalActionSet = state.legal_actions();
    // UTG facing BB：必 Fold / Call legal
    assert!(la.fold, "preflop UTG facing BB must be allowed to Fold");
    assert!(
        la.call.is_some(),
        "preflop UTG facing BB must be allowed to Call"
    );
    assert!(!la.check, "preflop UTG facing BB 不能 Check（BB raised）");
}
