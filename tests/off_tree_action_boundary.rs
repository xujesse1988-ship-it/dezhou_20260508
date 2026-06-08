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
    let aa = DefaultActionAbstraction::default_6_action();
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
    let aa = DefaultActionAbstraction::default_6_action();
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
    let aa = DefaultActionAbstraction::default_6_action();
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
    let aa = DefaultActionAbstraction::default_6_action();
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
    let aa = DefaultActionAbstraction::default_6_action();
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
    let aa = DefaultActionAbstraction::default_6_action();
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
    let aa = DefaultActionAbstraction::default_6_action();
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
    let aa = DefaultActionAbstraction::default_6_action();
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
    let aa = DefaultActionAbstraction::default_6_action();
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
    let aa = DefaultActionAbstraction::default_6_action();
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

// ============================================================================
// (G) 6c：pseudo-harmonic randomized rounding 行为验收
//
// 这一段验的是 6c **新算法**（取代旧 nearest-ratio stub）的两条关键性质，旧 stub
// 过不了第 (G2)(G3) 条：
//   G1 纯函数可复现（gate②「映射结果稳定可复现」+ AIVAT/replay 无状态重放一致）。
//   G2 概率方向：x 越靠近某档，越大概率 round 到该档（pseudo-harmonic f_A(x)）。
//   G3 边界打散（抗剥削，gate④）：同一笔「卡中点」off-tree 下注，在不同 board 下
//      被 round 到**两侧都出现**——旧 nearest-ratio 在算术中点恒定 round 一侧
//      （cliff，可被卡边界对手系统性套利），新算法把它打散。
// ============================================================================

/// 推进到 flop 起手（board 3 张、本街未起注 → max_committed=0、bet_range Some →
/// `map_off_tree` 落 case ④）。全 Check/Call 线，不会中途 terminal。
/// 返回 false 表示意外提前终局（调用方跳过该 seed）。
fn advance_to_flop_start(state: &mut GameState) -> bool {
    for _ in 0..24 {
        if state.board().len() >= 3 {
            return true;
        }
        if state.is_terminal() {
            return false;
        }
        let la = state.legal_actions();
        if la.check {
            state.apply(Action::Check).expect("Check legal then apply");
        } else if la.call.is_some() {
            state.apply(Action::Call).expect("Call legal then apply");
        } else {
            return false;
        }
    }
    state.board().len() >= 3
}

/// flop 起手处：max_committed=0、pot_after_call=pot。给定目标 pot-fraction
/// `x_milli`，算对应 off-tree raise 的 `real_to = x_milli × pot / 1000`。
fn real_to_for_x(state: &GameState, x_milli: u64) -> ChipAmount {
    let pot = state.pot().as_u64();
    ChipAmount::new(x_milli.saturating_mul(pot) / 1000)
}

/// 取 `map_off_tree` 输出的 Bet/Raise ratio_label milli；非 Bet/Raise → None。
fn off_tree_ratio_milli(m: AbstractAction) -> Option<u32> {
    match m {
        AbstractAction::Bet { ratio_label, .. } | AbstractAction::Raise { ratio_label, .. } => {
            Some(ratio_label.as_milli())
        }
        _ => None,
    }
}

#[test]
fn phm_off_tree_is_pure_reproducible_6c() {
    // G1：同 (state, real_to) → 64 次调用 byte-equal（纯函数契约）。
    let aa = DefaultActionAbstraction::default_6_action();
    let mut state = fresh_state(F1_MASTER_SEED ^ 0x6C01);
    assert!(advance_to_flop_start(&mut state), "应能推进到 flop 起手");
    // x=720 落在 (500,1000) 内、靠近 50% 交叉点 x*≈714 → 是随机 draw 的真实命中点。
    let real_to = real_to_for_x(&state, 720);
    let first = aa.map_off_tree(&state, real_to);
    for i in 0..64 {
        let again = aa.map_off_tree(&state, real_to);
        assert_eq!(first, again, "G1 纯函数破：第 {i} 次 map_off_tree 不一致");
    }
    // 落在 0.5/1.0 档之间 → 必是 Bet（flop 起手无前序 bet）的两档之一。
    let milli = off_tree_ratio_milli(first).expect("flop 起手 raise → Bet ratio_label");
    assert!(
        milli == 500 || milli == 1000,
        "x=720 应 round 到 0.5 或 1.0 档，得 {milli}"
    );
}

#[test]
fn phm_off_tree_probability_biases_toward_nearer_ratio_6c() {
    // G2：在 (0.5,1.0) 档之间扫 x；x 靠近 0.5 档时多 round→0.5，靠近 1.0 档时多
    // round→1.0（pseudo-harmonic f_lower(x) 单调递减）。跨 8 个 board 聚合降方差。
    let aa = DefaultActionAbstraction::default_6_action();

    // (lower_count, total) for 两个 sub-range。
    let mut lower_third_to_500 = 0u32;
    let mut lower_third_total = 0u32;
    let mut upper_third_to_500 = 0u32;
    let mut upper_third_total = 0u32;

    for s in 0..8u64 {
        let mut state = fresh_state(F1_MASTER_SEED.wrapping_add(0x6C20 + s));
        if !advance_to_flop_start(&mut state) {
            continue;
        }
        // lower third x ∈ [550,650]，upper third x ∈ [850,950]（避开端点）。
        for x in (550..=650).step_by(2) {
            let m = aa.map_off_tree(&state, real_to_for_x(&state, x));
            if let Some(milli) = off_tree_ratio_milli(m) {
                lower_third_total += 1;
                if milli == 500 {
                    lower_third_to_500 += 1;
                }
            }
        }
        for x in (850..=950).step_by(2) {
            let m = aa.map_off_tree(&state, real_to_for_x(&state, x));
            if let Some(milli) = off_tree_ratio_milli(m) {
                upper_third_total += 1;
                if milli == 500 {
                    upper_third_to_500 += 1;
                }
            }
        }
    }

    assert!(
        lower_third_total > 100 && upper_third_total > 100,
        "样本不足：lower={lower_third_total} upper={upper_third_total}"
    );
    // f_lower 在 lower third 均值 ~0.74、upper third 均值 ~0.16 → 宽容门槛。
    let lo_frac = lower_third_to_500 as f64 / lower_third_total as f64;
    let up_frac = upper_third_to_500 as f64 / upper_third_total as f64;
    assert!(
        lo_frac > 0.55,
        "G2 破：x 近 0.5 档却只有 {lo_frac:.3} round→0.5（应偏向近档）"
    );
    assert!(
        up_frac < 0.45,
        "G2 破：x 近 1.0 档却有 {up_frac:.3} round→0.5（应偏向远离 0.5）"
    );
    assert!(
        lo_frac > up_frac,
        "G2 破：round→0.5 比例应随 x 增大单调下降（lo={lo_frac:.3} up={up_frac:.3}）"
    );
}

#[test]
fn phm_off_tree_smears_midpoint_boundary_anti_exploit_6c() {
    // G3（核心抗剥削证据）：x=750 = 0.5/1.0 档的**算术中点**——旧 nearest-ratio 在此
    // 恒 round 到同一档（tie→smaller milli=0.5，cliff）。新算法按 board 派生种子打散
    // → 跨 board 两侧都出现，卡中点对手拿不到确定方向。
    let aa = DefaultActionAbstraction::default_6_action();
    let mut seen_500 = false;
    let mut seen_1000 = false;
    let mut samples = 0u32;

    for s in 0..96u64 {
        let mut state = fresh_state(F1_MASTER_SEED.wrapping_add(0x6C40 + s));
        if !advance_to_flop_start(&mut state) {
            continue;
        }
        let m = aa.map_off_tree(&state, real_to_for_x(&state, 750));
        match off_tree_ratio_milli(m) {
            Some(500) => seen_500 = true,
            Some(1000) => seen_1000 = true,
            _ => {}
        }
        samples += 1;
    }

    assert!(samples > 50, "样本不足：{samples}");
    assert!(
        seen_500 && seen_1000,
        "G3 破（退化成确定性 cliff）：x=750 中点跨 {samples} 个 board 只 round 到一侧 \
         (seen_500={seen_500}, seen_1000={seen_1000}) —— 抗剥削打散失效"
    );
}

#[test]
fn off_tree_map_algorithm_version_recorded_6c() {
    // gate①：算法版本标识写入（供策略服务版本元数据引用）。
    assert_eq!(
        poker::OFF_TREE_MAP_ALGORITHM,
        "pseudo-harmonic-randomized-rounding-v1"
    );
}
