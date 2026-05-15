//! 阶段 4 B1 \[测试\]：14-action raise sizes 走 stage 1 GameState::apply byte-equal
//! regression（D-422）。
//!
//! 每 [`PluribusAction`] 一条测试，共 14 条：
//!
//! - Fold / Check / Call / AllIn（4 条 non-raise）— 验证 [`PluribusActionAbstraction::
//!   actions`] 包含 + [`PluribusActionAbstraction::is_legal`] 返回 `true` 在
//!   适当 GameState 上 + 走对应 stage 1 [`Action`] apply 成功（pot / stack 更新
//!   一致）。
//! - Raise 0.5/0.75/1/1.5/2/3/5/10/25/50 Pot（10 条 raise）— 验证
//!   [`PluribusActionAbstraction::compute_raise_to`] 输出 `raise_to` 等于
//!   `current_bet + multiplier × pot_size`（整数 multiplier × pot 精确等于，
//!   非整数 0.75 Pot ±1 chip 容差）+ 走 stage 1 [`Action::Raise`] apply 成功 +
//!   `state.legal_actions().raise_range` 包含 `raise_to` + apply 后 `last_full_raise_size`
//!   按 stage 1 D-033 协议更新。
//!
//! **统一 scenario**：6-max 100BB（500 BB starting stack 让所有 raise size 在
//! HJ-facing-UTG-3x 状态下 ≤ stack 不被 D-422(e) auto-AllIn 钳位）。Test 1 / 3 /
//! 14 用 root 状态（UTG to act）；Test 2 用 HU postflop check 状态；其余 Raise
//! 系列均用 HJ-facing-UTG-300（max_committed=300, last_full_raise=200, pot=450,
//! min_raise=500）。
//!
//! **B1 \[测试\] 角色边界**：本文件 0 改动 `src/abstraction/action_pluribus.rs`
//! 与 0 改动 `src/training/nlhe_6max.rs` 与 0 改动
//! `docs/pluribus_stage4_{validation,decisions,api}.md`；A1 \[实现\] scaffold
//! 阶段 [`PluribusActionAbstraction::actions/is_legal/compute_raise_to`] 三方法均
//! `unimplemented!()`，本文件 14 测试在 default profile 必 panic-fail，B2
//! \[实现\] 落地走 stage 1 [`GameState`] legal action + pot / current_bet 计算后
//! 转绿。
//!
//! **D-422 trip-wire 锚点**：(a) min raise（stage 1 D-033） / (b) incomplete raise
//! 不 reopen（stage 1 D-033-rev1） / (c) `Action::Raise { to }` 绝对量（stage 1
//! D-026） / (d) all-in short raise 不 reopen（stage 1 D-033-rev1） / (e) raise size
//! 超 stack 自动 AllIn（stage 1 现有行为 byte-equal）— 任一退化在 14 测试套件
//! 至少触发 1 条 fail。

use poker::abstraction::action_pluribus::{PluribusAction, PluribusActionAbstraction};
use poker::{Action, ChipAmount, GameState, PlayerStatus, SeatId, TableConfig};

// ===========================================================================
// 共享常量 + state factory helper
// ===========================================================================

/// 6-max 500 BB starting stack（让 Raise 50 Pot 在 HJ-facing-UTG-3x 状态下 raise_to
/// = 22800 ≤ 50_000 stack 不被 D-422(e) auto-AllIn 钳位）。
const STARTING_STACK: u64 = 50_000;

/// stage 4 B1 \[测试\] 14-action raise sizes 共享 master seed（ASCII "STG4_B1\x14"
/// 让 GameState::new 派生 deck shuffle 决定性 — 跨多 test 同型 seed 让 hand-history
/// fixture replay 一致）。
const FIXED_SEED: u64 = 0x53_54_47_34_5F_42_31_14; // ASCII "STG4_B1" + 0x14 (14-action)

/// 构造 6-max 500 BB 桌面 + n_seats=6 root 状态（UTG=seat 3 to act preflop）。
///
/// 状态参数：
/// - button = seat 0，SB = seat 1, BB = seat 2, UTG = seat 3, HJ = seat 4, CO = seat 5
/// - starting_stacks = `[50_000; 6]`（500 BB 让所有 raise size 不被 stack 钳位）
/// - small_blind = 50，big_blind = 100，ante = 0
/// - root pot = 150（SB+BB），max_committed = 100，last_full_raise_size = 100（=BB）
/// - UTG (seat 3) to act，current_player = SeatId(3)
fn make_root_state() -> GameState {
    let cfg = TableConfig {
        n_seats: 6,
        starting_stacks: vec![ChipAmount::new(STARTING_STACK); 6],
        small_blind: ChipAmount::new(50),
        big_blind: ChipAmount::new(100),
        ante: ChipAmount::ZERO,
        button_seat: SeatId(0),
    };
    GameState::new(&cfg, FIXED_SEED)
}

/// 构造 HJ (seat 4) to act facing UTG-raise-to-300 状态：
///
/// - 起 `make_root_state()`
/// - UTG (seat 3) raise to 300（=3 BB 标准 preflop open）
/// - 切换后 HJ (seat 4) to act：max_committed = 300, pot = 50+100+300 = 450,
///   last_full_raise_size = 200（=300-100），min_raise = 500，HJ stack = 50_000
///
/// 让 Raise X Pot 全 10 个 multiplier 在该状态下：
/// - 0.5 Pot：raise_to = 300 + 225 = 525 ≥ 500 ✓
/// - 0.75 Pot：raise_to = 300 + 337.5 = 637.5（整数容差 ±1 chip）
/// - 1 Pot：750 / 1.5 Pot：975 / 2 Pot：1200 / 3 Pot：1650 / 5 Pot：2550 /
///   10 Pot：4800 / 25 Pot：11550 / 50 Pot：22800（全 ≤ 50_000 stack）
fn make_hj_facing_utg_3x_state() -> GameState {
    let mut state = make_root_state();
    state
        .apply(Action::Raise {
            to: ChipAmount::new(300),
        })
        .expect("UTG raise to 300 应当合法（D-033 min raise 200）");
    assert_eq!(
        state.current_player(),
        Some(SeatId(4)),
        "UTG raise 后下一行动者应为 HJ (seat 4)"
    );
    state
}

/// 构造 HU **flop 街首行动** check-option 状态（B2 \[实现\] §B2-revM carve-out
/// 修改：原 preflop 状态在 BB Check 后会触发 preflop 关街 + flop deal +
/// committed_this_round 全员 reset，让 `apply(Check)` 前后 `committed=100 → 0`
/// 不等违反测试不变量「Check 不改变 committed_this_round」；本 factory 改走
/// HU flop 街首行动状态：SB Call + BB Check 后已进入 flop，OOP=BB 先行
/// （D-022b-rev1 postflop OOP 先行字面），committed=0 → Check 不关街
/// （SB 未行动），committed 保持 0 不变）：
///
/// - HU（n_seats=2，button=0=SB，non-button=1=BB）
/// - SB Call → BB Check → 进入 flop
/// - flop：BB（seat 1）OOP 先行，committed_this_round = 0，max_committed = 0
///   ⇒ [`PluribusAction::Check`] legal；apply 后 BB.committed 仍 0（街内 Check
///   no-op，SB 未行动街不关）。
fn make_hu_bb_check_state() -> GameState {
    let cfg = TableConfig {
        n_seats: 2,
        starting_stacks: vec![ChipAmount::new(STARTING_STACK); 2],
        small_blind: ChipAmount::new(50),
        big_blind: ChipAmount::new(100),
        ante: ChipAmount::ZERO,
        button_seat: SeatId(0),
    };
    let mut state = GameState::new(&cfg, FIXED_SEED);
    // D-022b-rev1：HU 走 button=SB 语义；SB（seat 0）acts first preflop。
    state.apply(Action::Call).expect("SB call 应当合法");
    state
        .apply(Action::Check)
        .expect("BB check 应当合法（关闭 preflop）");
    assert_eq!(
        state.street(),
        poker::Street::Flop,
        "SB Call + BB Check 后应进入 Flop 街"
    );
    assert_eq!(
        state.current_player(),
        Some(SeatId(1)),
        "flop 街 BB (seat 1, OOP) 先行（D-022b-rev1 postflop OOP 先行）"
    );
    state
}

/// 整数 multiplier × pot_size 严格等于 expected raise_to（D-422 字面公式）。
fn assert_raise_to_eq_exact(
    abstraction: &PluribusActionAbstraction,
    state: &GameState,
    multiplier: f64,
    expected: u64,
) {
    let actual = abstraction.compute_raise_to(state, multiplier);
    assert_eq!(
        actual.as_u64(),
        expected,
        "D-422 字面 raise_to = current_bet + multiplier × pot：mult={multiplier} \
         actual={} expected={expected}",
        actual.as_u64()
    );
}

/// 非整数 multiplier × pot_size 接受 ±1 chip 容差（如 0.75 Pot rounding policy）。
fn assert_raise_to_eq_within_one_chip(
    abstraction: &PluribusActionAbstraction,
    state: &GameState,
    multiplier: f64,
    expected_f: f64,
) {
    let actual = abstraction.compute_raise_to(state, multiplier);
    let diff = (actual.as_u64() as f64) - expected_f;
    assert!(
        diff.abs() <= 1.0,
        "D-422 raise_to 容差超 ±1 chip：mult={multiplier} actual={} expected≈{expected_f} \
         diff={diff}",
        actual.as_u64()
    );
}

/// 验证 [`Action::Raise`] { to: raise_to } 在 `state` 上 apply 成功 + 走 stage 1
/// `legal_actions().raise_range` 容纳 `raise_to`（D-422(c) 绝对量约定 + D-033
/// min raise 校验 byte-equal）。
fn assert_apply_raise_byte_equal(state: &GameState, raise_to: ChipAmount) {
    let legal = state.legal_actions();
    let raise_range = legal
        .raise_range
        .expect("D-422 raise scenario 必须暴露非空 raise_range");
    assert!(
        raise_to >= raise_range.0 && raise_to <= raise_range.1,
        "D-422 raise_to {raise_to:?} 落在 stage 1 raise_range [{:?}, {:?}] 外（D-033 min \
         raise 或 stack cap 违反）",
        raise_range.0,
        raise_range.1
    );

    let mut after = state.clone();
    let before_pot = after.pot();
    after
        .apply(Action::Raise { to: raise_to })
        .expect("D-422(c) raise 绝对量 apply 应当合法");
    let after_pot = after.pot();
    let delta = after_pot.as_u64() - before_pot.as_u64();
    let actor_idx = state.current_player().unwrap().0 as usize;
    let actor_committed_before = state.players()[actor_idx].committed_this_round;
    let expected_delta = raise_to.as_u64() - actor_committed_before.as_u64();
    assert_eq!(
        delta,
        expected_delta,
        "D-422 pot delta 不一致：actor committed_before = {} raise_to = {} → \
         expected_delta = {expected_delta} actual_delta = {delta}",
        actor_committed_before.as_u64(),
        raise_to.as_u64()
    );
}

// ===========================================================================
// Test 1 — PluribusAction::Fold legal at root（UTG to act）
// ===========================================================================

/// D-422 Fold 验证：UTG (seat 3) at root 上 [`PluribusAction::Fold`] legal +
/// `actions()` 包含 + apply [`Action::Fold`] 后 UTG status = Folded。
#[test]
fn pluribus_action_fold_legal_at_root_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_root_state();
    assert!(
        abstraction.is_legal(&PluribusAction::Fold, &state),
        "D-422 Fold 在 root 任意 player_turn 状态下必 legal"
    );
    let actions = abstraction.actions(&state);
    assert!(
        actions.contains(&PluribusAction::Fold),
        "actions() 不包含 Fold：{actions:?}"
    );

    let actor_idx = state.current_player().unwrap().0 as usize;
    let mut after = state.clone();
    after
        .apply(Action::Fold)
        .expect("Action::Fold apply 应当合法");
    assert_eq!(
        after.players()[actor_idx].status,
        PlayerStatus::Folded,
        "Fold 后 UTG status 应为 Folded"
    );
}

// ===========================================================================
// Test 2 — PluribusAction::Check legal at HU postflop check option
// ===========================================================================

/// D-422 Check 验证：HU BB-after-SB-Call 状态下 [`PluribusAction::Check`] legal
/// + `actions()` 包含 + apply [`Action::Check`] 成功（committed_this_round 不变）。
#[test]
fn pluribus_action_check_legal_at_hu_bb_option_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_hu_bb_check_state();
    assert!(
        abstraction.is_legal(&PluribusAction::Check, &state),
        "D-422 Check 在 HU BB after SB-Call 状态下必 legal"
    );
    let actions = abstraction.actions(&state);
    assert!(
        actions.contains(&PluribusAction::Check),
        "actions() 不包含 Check：{actions:?}"
    );

    let actor_idx = state.current_player().unwrap().0 as usize;
    let before_committed = state.players()[actor_idx].committed_this_round;
    let mut after = state.clone();
    after
        .apply(Action::Check)
        .expect("Action::Check apply 应当合法");
    let after_committed = after.players()[actor_idx].committed_this_round;
    assert_eq!(
        before_committed, after_committed,
        "Check 不应改变 committed_this_round：before = {:?} after = {:?}",
        before_committed, after_committed
    );
}

// ===========================================================================
// Test 3 — PluribusAction::Call legal at root（UTG to call BB）
// ===========================================================================

/// D-422 Call 验证：UTG at root 上 [`PluribusAction::Call`] legal + apply
/// [`Action::Call`] 后 UTG committed_this_round = max_committed (100)。
#[test]
fn pluribus_action_call_legal_at_root_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_root_state();
    assert!(
        abstraction.is_legal(&PluribusAction::Call, &state),
        "D-422 Call 在 root UTG 状态下必 legal（to_call = BB）"
    );
    let actions = abstraction.actions(&state);
    assert!(
        actions.contains(&PluribusAction::Call),
        "actions() 不包含 Call：{actions:?}"
    );

    let actor_idx = state.current_player().unwrap().0 as usize;
    let mut after = state.clone();
    after
        .apply(Action::Call)
        .expect("Action::Call apply 应当合法");
    assert_eq!(
        after.players()[actor_idx].committed_this_round.as_u64(),
        100,
        "Call 后 UTG committed_this_round 应 == 100 (BB)"
    );
}

// ===========================================================================
// Test 4 — PluribusAction::Raise05Pot at HJ-facing-UTG-3x（raise_to = 525）
// ===========================================================================

/// D-422 Raise 0.5 Pot 验证：HJ facing UTG-raise-to-300 状态（pot=450, current_bet=300,
/// min_raise=500），`compute_raise_to(state, 0.5) = 300 + 225 = 525` 整数精确等于。
#[test]
fn pluribus_action_raise_05pot_at_hj_facing_3x_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_hj_facing_utg_3x_state();
    assert!(
        abstraction.is_legal(&PluribusAction::Raise05Pot, &state),
        "Raise 0.5 Pot 在 HJ-facing-3x 状态下 raise_to=525 ≥ min_raise=500 必 legal"
    );
    let actions = abstraction.actions(&state);
    assert!(
        actions.contains(&PluribusAction::Raise05Pot),
        "actions() 不包含 Raise05Pot：{actions:?}"
    );
    assert_raise_to_eq_exact(&abstraction, &state, 0.5, 525);
    assert_apply_raise_byte_equal(&state, ChipAmount::new(525));
}

// ===========================================================================
// Test 5 — PluribusAction::Raise075Pot at HJ-facing-UTG-3x（raise_to ≈ 637.5）
// ===========================================================================

/// D-422 Raise 0.75 Pot 验证：HJ facing UTG-3x 状态，`compute_raise_to(state, 0.75)
/// = 300 + 337.5 = 637.5`（非整数，B2 \[实现\] rounding policy 未 lock；接受
/// ±1 chip 容差让 floor / round-half-up / ceil 任一 policy 均通过）。
#[test]
fn pluribus_action_raise_075pot_at_hj_facing_3x_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_hj_facing_utg_3x_state();
    assert!(
        abstraction.is_legal(&PluribusAction::Raise075Pot, &state),
        "Raise 0.75 Pot 在 HJ-facing-3x 状态下 raise_to≈637.5 ≥ min_raise=500 必 legal"
    );
    let actions = abstraction.actions(&state);
    assert!(
        actions.contains(&PluribusAction::Raise075Pot),
        "actions() 不包含 Raise075Pot：{actions:?}"
    );
    assert_raise_to_eq_within_one_chip(&abstraction, &state, 0.75, 637.5);
    let raise_to = abstraction.compute_raise_to(&state, 0.75);
    assert_apply_raise_byte_equal(&state, raise_to);
}

// ===========================================================================
// Test 6 — PluribusAction::Raise1Pot at HJ-facing-UTG-3x（raise_to = 750）
// ===========================================================================

#[test]
fn pluribus_action_raise_1pot_at_hj_facing_3x_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_hj_facing_utg_3x_state();
    assert!(abstraction.is_legal(&PluribusAction::Raise1Pot, &state));
    let actions = abstraction.actions(&state);
    assert!(actions.contains(&PluribusAction::Raise1Pot));
    assert_raise_to_eq_exact(&abstraction, &state, 1.0, 750);
    assert_apply_raise_byte_equal(&state, ChipAmount::new(750));
}

// ===========================================================================
// Test 7 — PluribusAction::Raise15Pot at HJ-facing-UTG-3x（raise_to = 975）
// ===========================================================================

#[test]
fn pluribus_action_raise_15pot_at_hj_facing_3x_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_hj_facing_utg_3x_state();
    assert!(abstraction.is_legal(&PluribusAction::Raise15Pot, &state));
    let actions = abstraction.actions(&state);
    assert!(actions.contains(&PluribusAction::Raise15Pot));
    assert_raise_to_eq_exact(&abstraction, &state, 1.5, 975);
    assert_apply_raise_byte_equal(&state, ChipAmount::new(975));
}

// ===========================================================================
// Test 8 — PluribusAction::Raise2Pot at HJ-facing-UTG-3x（raise_to = 1200）
// ===========================================================================

#[test]
fn pluribus_action_raise_2pot_at_hj_facing_3x_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_hj_facing_utg_3x_state();
    assert!(abstraction.is_legal(&PluribusAction::Raise2Pot, &state));
    let actions = abstraction.actions(&state);
    assert!(actions.contains(&PluribusAction::Raise2Pot));
    assert_raise_to_eq_exact(&abstraction, &state, 2.0, 1200);
    assert_apply_raise_byte_equal(&state, ChipAmount::new(1200));
}

// ===========================================================================
// Test 9 — PluribusAction::Raise3Pot at HJ-facing-UTG-3x（raise_to = 1650）
// ===========================================================================

#[test]
fn pluribus_action_raise_3pot_at_hj_facing_3x_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_hj_facing_utg_3x_state();
    assert!(abstraction.is_legal(&PluribusAction::Raise3Pot, &state));
    let actions = abstraction.actions(&state);
    assert!(actions.contains(&PluribusAction::Raise3Pot));
    assert_raise_to_eq_exact(&abstraction, &state, 3.0, 1650);
    assert_apply_raise_byte_equal(&state, ChipAmount::new(1650));
}

// ===========================================================================
// Test 10 — PluribusAction::Raise5Pot at HJ-facing-UTG-3x（raise_to = 2550）
// ===========================================================================

#[test]
fn pluribus_action_raise_5pot_at_hj_facing_3x_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_hj_facing_utg_3x_state();
    assert!(abstraction.is_legal(&PluribusAction::Raise5Pot, &state));
    let actions = abstraction.actions(&state);
    assert!(actions.contains(&PluribusAction::Raise5Pot));
    assert_raise_to_eq_exact(&abstraction, &state, 5.0, 2550);
    assert_apply_raise_byte_equal(&state, ChipAmount::new(2550));
}

// ===========================================================================
// Test 11 — PluribusAction::Raise10Pot at HJ-facing-UTG-3x（raise_to = 4800）
// ===========================================================================

#[test]
fn pluribus_action_raise_10pot_at_hj_facing_3x_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_hj_facing_utg_3x_state();
    assert!(abstraction.is_legal(&PluribusAction::Raise10Pot, &state));
    let actions = abstraction.actions(&state);
    assert!(actions.contains(&PluribusAction::Raise10Pot));
    assert_raise_to_eq_exact(&abstraction, &state, 10.0, 4800);
    assert_apply_raise_byte_equal(&state, ChipAmount::new(4800));
}

// ===========================================================================
// Test 12 — PluribusAction::Raise25Pot at HJ-facing-UTG-3x（raise_to = 11550）
// ===========================================================================

/// HJ stack = 50_000 - 0 = 50_000；raise_to = 11550 ≤ 50_000 ✓ legal（不被
/// D-422(e) auto-AllIn 钳位）。
#[test]
fn pluribus_action_raise_25pot_at_hj_facing_3x_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_hj_facing_utg_3x_state();
    assert!(abstraction.is_legal(&PluribusAction::Raise25Pot, &state));
    let actions = abstraction.actions(&state);
    assert!(actions.contains(&PluribusAction::Raise25Pot));
    assert_raise_to_eq_exact(&abstraction, &state, 25.0, 11550);
    assert_apply_raise_byte_equal(&state, ChipAmount::new(11550));
}

// ===========================================================================
// Test 13 — PluribusAction::Raise50Pot at HJ-facing-UTG-3x（raise_to = 22800）
// ===========================================================================

/// raise_to = 22800 ≤ 50_000 stack ✓（500 BB starting stack 让最大 raise mult
/// 50 也保持 < stack；D-422(e) auto-AllIn 钳位边界由 PluribusAction::AllIn
/// 独立测试覆盖）。
#[test]
fn pluribus_action_raise_50pot_at_hj_facing_3x_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_hj_facing_utg_3x_state();
    assert!(abstraction.is_legal(&PluribusAction::Raise50Pot, &state));
    let actions = abstraction.actions(&state);
    assert!(actions.contains(&PluribusAction::Raise50Pot));
    assert_raise_to_eq_exact(&abstraction, &state, 50.0, 22800);
    assert_apply_raise_byte_equal(&state, ChipAmount::new(22800));
}

// ===========================================================================
// Test 14 — PluribusAction::AllIn at root（UTG all-in to 50_000）
// ===========================================================================

/// D-422 AllIn 验证：UTG at root 上 [`PluribusAction::AllIn`] legal +
/// `actions()` 包含 + apply [`Action::AllIn`] 后 UTG stack = 0 + pot 增加
/// stack 量。**走 stage 1 `Action::AllIn`**（继承 D-026 绝对量 + D-022 all-in
/// 协议）让 stage 4 抽象不重新发明 AllIn 路径。
#[test]
fn pluribus_action_all_in_legal_at_root_apply_byte_equal() {
    let abstraction = PluribusActionAbstraction;
    let state = make_root_state();
    assert!(
        abstraction.is_legal(&PluribusAction::AllIn, &state),
        "D-422 AllIn 在 root UTG (stack > 0) 状态下必 legal"
    );
    let actions = abstraction.actions(&state);
    assert!(
        actions.contains(&PluribusAction::AllIn),
        "actions() 不包含 AllIn：{actions:?}"
    );

    let actor_idx = state.current_player().unwrap().0 as usize;
    let before_stack = state.players()[actor_idx].stack;
    let before_committed = state.players()[actor_idx].committed_this_round;
    let before_pot = state.pot();
    let mut after = state.clone();
    after
        .apply(Action::AllIn)
        .expect("Action::AllIn apply 应当合法");
    let after_stack = after.players()[actor_idx].stack;
    let after_committed = after.players()[actor_idx].committed_this_round;
    let after_pot = after.pot();

    assert_eq!(
        after_stack.as_u64(),
        0,
        "AllIn 后 UTG stack 应 == 0（before = {}）",
        before_stack.as_u64()
    );
    let expected_committed = before_committed.as_u64() + before_stack.as_u64();
    assert_eq!(
        after_committed.as_u64(),
        expected_committed,
        "AllIn 后 UTG committed_this_round = {} 不等于 before({}) + stack({}) = {expected_committed}",
        after_committed.as_u64(),
        before_committed.as_u64(),
        before_stack.as_u64()
    );
    let expected_pot_delta = before_stack.as_u64();
    let actual_pot_delta = after_pot.as_u64() - before_pot.as_u64();
    assert_eq!(
        actual_pot_delta, expected_pot_delta,
        "AllIn 后 pot delta = {actual_pot_delta} 不等于 before_stack = {expected_pot_delta}"
    );
}
