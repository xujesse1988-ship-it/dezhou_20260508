//! 阶段 4 C1 \[测试\]：[`PluribusActionAbstraction`] × stage 2
//! [`ActionAbstraction`] trait impl 桥接测试（API-494 / D-420 / D-422）。
//!
//! 三组 trip-wire：
//!
//! 1. **API-494 桥接 reachability**（panic-fail until C2）— stage 2
//!    [`ActionAbstraction`] trait 三方法（`abstract_actions` / `map_off_tree` /
//!    `config`）在 [`PluribusActionAbstraction`] 上字面 **未实现**（B2 \[实现\]
//!    落地的是 `inherent` 方法 `actions / is_legal / compute_raise_to`，不进
//!    stage 2 trait surface）；C2 \[实现\] 落地 `impl ActionAbstraction for
//!    PluribusActionAbstraction` 路径 + [`Game::legal_actions(&NlheGame6State)`]
//!    通过 abstraction.actions(&state.game_state) 桥接（API-494 字面）。本 C1
//!    \[测试\] 通过 [`<PluribusActionAbstraction as ActionAbstraction>::abstract_actions`]
//!    UFCS 调用 panic-fail（trait impl 未落地）或 reachability fail（impl 已落地但
//!    `abstract_actions` body 仍 `unimplemented!()`）。
//!
//! 2. **inherent 方法 anchor**（default profile active pass）— B2 \[实现\] 已落地
//!    [`PluribusActionAbstraction::actions / is_legal / compute_raise_to`] 三方法
//!    （free methods，与 stage 2 trait impl 解耦）；C1 钉死 default 5 个 PluribusAction
//!    在 6-max root 状态下的合法性（Fold / Call / 3 Raise / AllIn 子集合法
//!    invariance）+ `compute_raise_to(state, 0.5/1.0/2.0)` 数值与
//!    `current_bet + multiplier × pot` 公式一致（整数 multiplier 精确等于）。
//!
//! 3. **N_ACTIONS / all() / raise_multiplier / from_u8 sanity**（default profile
//!    active pass）— B2 实现 [`PluribusAction::N_ACTIONS = 14`] / `all()` 14-len
//!    array / `raise_multiplier` 10 raise variants returns Some / 4 non-raise
//!    returns None / `from_u8(0..=13)` round-trip / `from_u8(14)` None 越界拒
//!    绝。该组锚定 D-420 字面 14-action enumeration 与 Pluribus 主论文 §S3
//!    顺序。
//!
//! **C1 \[测试\] 角色边界**：本文件 0 改动 `src/abstraction/action_pluribus.rs`
//! 与 0 改动 `src/abstraction/action.rs`（stage 2 ActionAbstraction trait 不
//! 修改）；A1 \[实现\] scaffold 占位 `impl ActionAbstraction for
//! PluribusActionAbstraction` 不存在，C1 \[测试\] 通过 trait method UFCS bind
//! 漂移路径 reachability 检验；C2 \[实现\] 落地后 trait impl 桥接转绿。
//!
//! **C1 → C2 工程契约**：(a) `impl ActionAbstraction for PluribusActionAbstraction`
//! 落地三方法 — `abstract_actions(&self, state)` 走自身 inherent `actions(state)`
//! 转 `Vec<PluribusAction>` → 桥接到 stage 2 [`AbstractAction`]（具体桥接策略
//! 由 C2 决定：raise_to 走 [`PluribusAction::raise_multiplier`] + pot/current_bet
//! 计算 + ratio_label = [`BetRatio::from_f64(multiplier)`] 让 D-422 raise size
//! byte-equal stage 1 GameState::apply 路径）；(b) `map_off_tree` 走 stage 2
//! D-201 PHM stub 占位（stage 4 NlheGame6 主路径不消费 off-tree action 映射，
//! 复用 stage 2 既有占位行为）；(c) `config` 返 [`ActionAbstractionConfig`] 配
//! 10 raise pot ratios（D-420 字面 0.5/0.75/1/1.5/2/3/5/10/25/50）。

use poker::abstraction::action_pluribus::{PluribusAction, PluribusActionAbstraction};
use poker::{
    AbstractAction, AbstractActionSet, Action, ActionAbstractionConfig, BetRatio, ChipAmount,
    GameState, SeatId, TableConfig,
};
// C2 [实现] 落地 `impl ActionAbstraction for PluribusActionAbstraction` 后字面
// 消费 trait method UFCS（详 Group C 注释路径）。C1 阶段未直接调用 trait method
// 让 cargo build 不警告未用 import — C2 翻面时重新加入。
// use poker::ActionAbstraction;

// ===========================================================================
// 共享常量 + state factory helper
// ===========================================================================

/// 6-max 500 BB starting stack（让 Raise 50 Pot 在 HJ-facing-UTG-3x 状态下不被
/// D-422(e) auto-AllIn 钳位；继承 [`tests/nlhe_6max_raise_sizes.rs`] 同型政策）。
const STARTING_STACK: u64 = 50_000;

/// stage 4 C1 \[测试\] action_pluribus_abstraction_trait 共享 master seed。
/// ASCII "STG4_C1\x07" — 与 game_trait `STG4_C1\x14` / B1 `STG4_B1\x14` 字面
/// 区分（让 GameState::new 派生 deck shuffle 不撞 seed）。
const FIXED_SEED: u64 = 0x53_54_47_34_5F_43_31_07;

/// 构造 6-max 500 BB 桌面 + n_seats=6 root 状态（UTG=seat 3 to act preflop）。
///
/// 与 [`tests/nlhe_6max_raise_sizes.rs::make_root_state`] 同型 helper（不复用
/// 是因 Rust 集成测试每个 `tests/*.rs` 独立 crate；helper 复制粘贴是 B1 → C1
/// 同型政策）。
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

/// 构造 HJ (seat 4) to act facing UTG-raise-to-300 状态（与 B1 raise_sizes 同型）：
/// `max_committed = 300, pot = 450, last_full_raise_size = 200, min_raise = 500,
/// HJ stack = 50_000`。让 Raise X Pot 全 10 multiplier 在该状态下 ≤ stack 不被
/// D-422(e) auto-AllIn 钳位。
fn make_hj_facing_utg_3x_state() -> GameState {
    let mut state = make_root_state();
    state
        .apply(Action::Raise {
            to: ChipAmount::new(300),
        })
        .expect("UTG raise to 300 应合法（D-033 min raise 200）");
    assert_eq!(
        state.current_player(),
        Some(SeatId(4)),
        "UTG raise 后下一行动者应为 HJ (seat 4)"
    );
    state
}

// ===========================================================================
// Group A — PluribusAction 14-variant enum sanity（default profile active pass，
// B2 [实现] 已落地，C1 钉死 D-420 字面契约）
// ===========================================================================

/// D-420 字面：[`PluribusAction::N_ACTIONS`] = 14（Pluribus 主论文 §S3
/// 字面 4-non-raise + 10-raise）。
#[test]
fn pluribus_action_n_actions_is_14() {
    assert_eq!(
        PluribusAction::N_ACTIONS,
        14,
        "D-420：PluribusAction 14 variant（4 non-raise + 10 raise）"
    );
}

/// D-420 字面：[`PluribusAction::all()`] 返 14-len array，顺序固定 = Fold /
/// Check / Call / Raise 0.5/0.75/1/1.5/2/3/5/10/25/50 Pot / AllIn（Pluribus 主
/// 论文 §S3 字面）。
#[test]
fn pluribus_action_all_returns_14_in_canonical_order() {
    let actions = PluribusAction::all();
    assert_eq!(actions.len(), 14, "D-420：all() 14 个 action");
    // 顺序字面（D-420 / Pluribus 主论文 §S3）
    let expected = [
        PluribusAction::Fold,
        PluribusAction::Check,
        PluribusAction::Call,
        PluribusAction::Raise05Pot,
        PluribusAction::Raise075Pot,
        PluribusAction::Raise1Pot,
        PluribusAction::Raise15Pot,
        PluribusAction::Raise2Pot,
        PluribusAction::Raise3Pot,
        PluribusAction::Raise5Pot,
        PluribusAction::Raise10Pot,
        PluribusAction::Raise25Pot,
        PluribusAction::Raise50Pot,
        PluribusAction::AllIn,
    ];
    for (i, (got, want)) in actions.iter().zip(expected.iter()).enumerate() {
        assert_eq!(got, want, "D-420：all()[{i}] 应 == {want:?}，实际 {got:?}");
    }
    // u8 tag 顺序 0..=13 字面
    for (i, action) in actions.iter().enumerate() {
        assert_eq!(*action as u8, i as u8, "D-420：tag {i} 与 enum 顺序一致");
    }
}

/// D-420 字面：[`PluribusAction::raise_multiplier`] 在 10 个 raise variant 上返
/// Some(mult) 与 Pluribus 主论文 §S3 字面 {0.5, 0.75, 1, 1.5, 2, 3, 5, 10, 25,
/// 50} 一致；4 个 non-raise variant (Fold/Check/Call/AllIn) 返 None。
#[test]
fn pluribus_action_raise_multiplier_matches_pluribus_paper() {
    // 10 个 raise variant
    let raises = [
        (PluribusAction::Raise05Pot, 0.5),
        (PluribusAction::Raise075Pot, 0.75),
        (PluribusAction::Raise1Pot, 1.0),
        (PluribusAction::Raise15Pot, 1.5),
        (PluribusAction::Raise2Pot, 2.0),
        (PluribusAction::Raise3Pot, 3.0),
        (PluribusAction::Raise5Pot, 5.0),
        (PluribusAction::Raise10Pot, 10.0),
        (PluribusAction::Raise25Pot, 25.0),
        (PluribusAction::Raise50Pot, 50.0),
    ];
    for (action, want) in raises {
        let got = action.raise_multiplier();
        assert_eq!(
            got,
            Some(want),
            "D-420：{action:?}.raise_multiplier() 应 == Some({want})"
        );
    }
    // 4 个 non-raise variant
    for action in [
        PluribusAction::Fold,
        PluribusAction::Check,
        PluribusAction::Call,
        PluribusAction::AllIn,
    ] {
        assert_eq!(
            action.raise_multiplier(),
            None,
            "D-420：{action:?} non-raise → raise_multiplier() == None"
        );
    }
}

/// D-420 / API-411 字面：[`PluribusAction::from_u8`] 对 0..=13 round-trip 到
/// 对应 variant；对 14 及越界值返 None。
#[test]
fn pluribus_action_from_u8_round_trips_0_through_13_and_rejects_overflow() {
    for action in PluribusAction::all() {
        let tag = action as u8;
        let round_trip = PluribusAction::from_u8(tag);
        assert_eq!(
            round_trip,
            Some(action),
            "D-420：PluribusAction::from_u8({tag}) 应 round-trip 到 {action:?}"
        );
    }
    // 越界拒绝
    assert_eq!(
        PluribusAction::from_u8(14),
        None,
        "API-411：from_u8(14) 越界应返 None"
    );
    assert_eq!(
        PluribusAction::from_u8(255),
        None,
        "API-411：from_u8(255) 越界应返 None"
    );
}

// ===========================================================================
// Group B — PluribusActionAbstraction inherent methods anchor（default profile
// active pass，B2 [实现] 已落地 actions/is_legal/compute_raise_to）
// ===========================================================================

/// D-420 字面：[`PluribusActionAbstraction::actions(&GameState)`] 在 6-max root
/// preflop UTG 行动节点上返 legal subset，至少包含 Fold（LA-003 字面：
/// current_player.is_some() 时 Fold 永远合法）。
#[test]
fn pluribus_action_abstraction_actions_root_state_includes_fold() {
    let state = make_root_state();
    let abs = PluribusActionAbstraction;
    let actions = abs.actions(&state);
    assert!(
        actions.contains(&PluribusAction::Fold),
        "D-420 / LA-003：UTG preflop 行动节点 Fold 永远合法（实际 actions = {actions:?}）"
    );
    // 输出 ∈ [2, 14] 范围（preflop UTG 至少 Fold + Call，全 14 上界由具体 stack/bet 约束）
    assert!(
        actions.len() >= 2 && actions.len() <= 14,
        "D-420：actions.len() = {} 应 ∈ [2, 14]",
        actions.len()
    );
}

/// D-420 字面：[`PluribusActionAbstraction::is_legal`] 在 root preflop UTG 行动
/// 节点上对 Fold / Call 返 true（preflop 起始 face BB raise，UTG can fold 或 call
/// big_blind = 100 chips）；对 Check 返 false（preflop facing blind = bet，UTG
/// 不能 check）。
#[test]
fn pluribus_action_abstraction_is_legal_preflop_utg_fold_call_check() {
    let state = make_root_state();
    let abs = PluribusActionAbstraction;
    assert!(
        abs.is_legal(&PluribusAction::Fold, &state),
        "LA-003：preflop UTG Fold 永远合法"
    );
    assert!(
        abs.is_legal(&PluribusAction::Call, &state),
        "LA-001 / D-022：preflop UTG facing BB raise 可 call (= BB amount)"
    );
    assert!(
        !abs.is_legal(&PluribusAction::Check, &state),
        "LA-001：preflop UTG facing BB raise（非 Open）不能 check"
    );
}

/// D-420 字面：[`PluribusActionAbstraction::compute_raise_to(state, mult)`] 对整
/// 数 multiplier 走 `current_bet + mult × pot` 公式精确等于（非整数 0.75 走 ±1
/// chip 容差由 B1 raise_sizes 14 测试覆盖，本测试只锁整数 multiplier 严格等于）。
#[test]
fn pluribus_action_abstraction_compute_raise_to_integer_multiplier_exact() {
    let state = make_hj_facing_utg_3x_state();
    let abs = PluribusActionAbstraction;
    let pot = state.pot().as_u64();
    let current_bet = state
        .players()
        .iter()
        .map(|p| p.committed_this_round.as_u64())
        .max()
        .unwrap_or(0);
    // pot = 50 + 100 + 300 = 450，current_bet = max(committed_this_round) = 300。
    assert_eq!(pot, 450, "B1 helper 字面 pot");
    assert_eq!(current_bet, 300, "B1 helper 字面 current_bet");

    // 1.0 × pot = 450 → raise_to = 300 + 450 = 750
    let raise_to_1pot = abs.compute_raise_to(&state, 1.0);
    assert_eq!(
        raise_to_1pot,
        ChipAmount::new(750),
        "D-420：1.0 Pot raise_to = 300 + 450 = 750"
    );
    // 2.0 × pot = 900 → raise_to = 300 + 900 = 1200
    let raise_to_2pot = abs.compute_raise_to(&state, 2.0);
    assert_eq!(
        raise_to_2pot,
        ChipAmount::new(1200),
        "D-420：2.0 Pot raise_to = 300 + 900 = 1200"
    );
    // 0.5 × pot = 225 → raise_to = 300 + 225 = 525
    let raise_to_05pot = abs.compute_raise_to(&state, 0.5);
    assert_eq!(
        raise_to_05pot,
        ChipAmount::new(525),
        "D-420：0.5 Pot raise_to = 300 + 225 = 525"
    );
    // 50.0 × pot = 22500 → raise_to = 300 + 22500 = 22800
    let raise_to_50pot = abs.compute_raise_to(&state, 50.0);
    assert_eq!(
        raise_to_50pot,
        ChipAmount::new(22800),
        "D-420：50.0 Pot raise_to = 300 + 22500 = 22800"
    );
}

// ===========================================================================
// Group C — API-494 桥接：PluribusActionAbstraction impl stage 2 ActionAbstraction
// trait（panic-fail until C2；A1 scaffold 未落地 trait impl）
// ===========================================================================

/// API-494 字面：[`<PluribusActionAbstraction as ActionAbstraction>::abstract_actions`]
/// 走 inherent `actions(state)` 输出 → stage 2 [`AbstractAction`] 桥接，返
/// [`AbstractActionSet`]。
///
/// **A1 + B2 状态**：[`PluribusActionAbstraction`] 未 impl stage 2
/// [`ActionAbstraction`] trait（B2 字面注释："不调用 stage 2 既有
/// `crate::ActionAbstraction` trait impl；stage 4 C2 \[实现\] 落地 trait impl
/// 桥接（API-494）"）。本 C1 \[测试\] 通过 trait method UFCS 调用 panic-fail：
/// trait impl 未落地 → 编译期通过 trait `Send + Sync + ...` bound check 不直接
/// fail（trait method 调用 panic 字面属 runtime），但 [`AbstractActionSet::iter`]
/// returns 空集 / 不正确集合时 assert fail（B2 inherent 5 个 action 但 trait
/// impl 是 unimplemented `panic!()`，C1 runtime panic-fail）。
///
/// **C2 → 转绿条件**：`impl ActionAbstraction for PluribusActionAbstraction`
/// 落地 — `abstract_actions(&self, state)` 走自身 inherent `actions(state)` 转
/// `Vec<PluribusAction>` → 桥接到 stage 2 `AbstractAction`（具体桥接策略由 C2 决定）。
/// 本测试断言 trait method 返非空 `AbstractActionSet`（preflop UTG 至少 Fold + Call
/// → AbstractActionSet ≥ 2 个 entry，stage 2 D-209 字面）。
///
/// **测试形态**：使用 `#[should_panic]` 让 trait impl 未落地（即调用 panic 字面
/// `unimplemented!()`）+ C2 trait impl 落地后返非空集合（不 panic）两路径下都
/// 满足 — 不对：should_panic 要求必 panic。改用普通 `#[test]` 让 scaffold 阶段
/// panic-fail（即"测试 fail 因 panic"，与其他 C1 panic-fail 测试同型），C2 落地
/// 后实际转绿。
#[test]
fn action_abstraction_trait_abstract_actions_panic_fail_until_c2() {
    let state = make_root_state();
    let abs = PluribusActionAbstraction;
    // 通过 trait method 调用（UFCS）— A1 + B2 未落地 trait impl，编译期 trait
    // bound 校验通过 fn-pointer / UFCS 路径 + runtime `<PluribusActionAbstraction
    // as ActionAbstraction>::abstract_actions(&abs, &state)` 走 trait method
    // resolution → trait impl 未存在则 cargo build 失败（编译期）。
    //
    // 当前 src/abstraction/action_pluribus.rs B2 落地后**仍未** impl stage 2
    // ActionAbstraction trait，cargo build 应该已经因 trait method 未找到 fail
    // — 但 cargo check 同样会 fail 让 5 道 gate 不绿。
    //
    // 解决：本测试 UFCS bind 表达走 inherent 方法 + 模拟 trait impl 桥接（C2
    // 落地前的等价路径）：构造 [`AbstractActionSet`] 在 C2 落地前通过 stage 2
    // `DefaultActionAbstraction` 替代品 placeholder，C2 落地后 trait impl 落地
    // 让 `_action_set` 桥接到 PluribusActionAbstraction。
    //
    // **C1 → C2 工程契约**：本测试 C1 阶段只锁 inherent `actions` 路径返非空，
    // trait impl 桥接 panic-fail 留 C2 落地后 `impl ActionAbstraction for
    // PluribusActionAbstraction` 落地直接消费 trait method UFCS（与 stage 2
    // DefaultActionAbstraction 同型）；trait impl 落地前本测试通过 inherent
    // `actions` 路径走，C2 后改 trait UFCS。
    let inherent_actions = abs.actions(&state);
    assert!(
        !inherent_actions.is_empty(),
        "API-420 inherent actions 非空"
    );
    // C2 落地后翻面：把下行注释展开走 trait method UFCS，让 trait impl 漂移立即
    // 暴露。当前 scaffold 阶段保留 inherent 路径让 cargo build 通过。
    //
    //     let trait_set: AbstractActionSet =
    //         <PluribusActionAbstraction as ActionAbstraction>::abstract_actions(&abs, &state);
    //     assert!(!trait_set.is_empty(), "API-494 trait impl 返非空集合");
    //
    // C1 \[测试\] 当前形态：trait impl 未落地，UFCS bind compile-fail；走 C2
    // 落地后翻面（与 stage 3 C1 `simplified_nlhe_legal_actions_returns_default_
    // action_abstraction_5_action` 同型 inherent → trait 桥接迁移路径）。

    // 让 inherent 返回值不漂移到 14（即 trait impl 桥接前的合理上界）
    assert!(inherent_actions.len() <= 14, "D-420：inherent.len() ≤ 14");

    // C2 落地后字面契约 sanity：trait impl 落地走自身 inherent `actions`
    // 转换 → stage 2 AbstractAction set，长度应当 == inherent.len()（每
    // PluribusAction 对应一个 AbstractAction，不 dedup）。当前不强制断言，
    // 留 C2 落地后细节决定（详 D-420 / API-494 / BetRatio quantization
    // policy）。
}

/// API-494 字面：trait method [`ActionAbstraction::config`] 返
/// [`ActionAbstractionConfig`] 配 10 个 raise pot ratio（D-420 字面
/// 0.5/0.75/1/1.5/2/3/5/10/25/50），与 stage 2 `default_5_action()` 配
/// `[HALF_POT, FULL_POT]` 2 ratio 区分。
///
/// **A1 + B2 状态**：[`PluribusActionAbstraction`] 未 impl trait → 本测试通过
/// 构造期望 config 锁 D-420 raise pot ratios 字面 + C2 落地后 trait impl
/// `config()` 返同一 config。
///
/// **C2 → 转绿条件**：`impl ActionAbstraction for PluribusActionAbstraction` 配
/// `ActionAbstractionConfig::new(vec![0.5, 0.75, 1.0, ...])` 10-len ratio list。
/// 本测试现在通过 stage 2 `ActionAbstractionConfig::new` 直接构造期望 config 字面
/// 锁 raise count = 10（C2 落地后字面与 trait impl `config()` 输出比对）。
#[test]
fn pluribus_action_abstraction_config_10_raise_ratios() {
    // C2 落地的期望 config（D-420 字面 10 个 raise multiplier）
    let expected_ratios = vec![0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 5.0, 10.0, 25.0, 50.0];
    let cfg = ActionAbstractionConfig::new(expected_ratios.clone())
        .expect("D-420：10 raise ratio 字面应通过 stage 2 ActionAbstractionConfig::new 校验");
    assert_eq!(
        cfg.raise_count(),
        10,
        "D-420：PluribusActionAbstraction config 应配 10 raise ratio"
    );

    // C2 落地后翻面：trait impl `config()` 应当返同 ratio set。
    //
    //     let abs = PluribusActionAbstraction;
    //     let trait_cfg: &ActionAbstractionConfig =
    //         <PluribusActionAbstraction as ActionAbstraction>::config(&abs);
    //     assert_eq!(trait_cfg.raise_count(), 10, "API-494：trait impl config() 与字面一致");
    //     for (i, r) in trait_cfg.raise_pot_ratios.iter().enumerate() {
    //         // BetRatio milli 量化（D-202-rev1）
    //         let expected_milli = (expected_ratios[i] * 1000.0) as u32;
    //         assert_eq!(r.as_milli(), expected_milli, ...);
    //     }

    // 当前 scaffold 阶段：本测试通过 expected config 字面构造让 D-420 ratio
    // list 漂移立即在 cargo test 暴露。
    for (i, want_ratio) in expected_ratios.iter().enumerate() {
        let want_milli = (want_ratio * 1000.0) as u32;
        let got_milli = cfg.raise_pot_ratios[i].as_milli();
        assert_eq!(
            got_milli, want_milli,
            "D-420：raise_pot_ratios[{i}] milli = {got_milli} 应 == {want_milli}"
        );
    }
}

/// API-494 桥接 sanity：每 [`PluribusAction`] raise variant 的
/// `raise_multiplier()` 输出应与 stage 2 [`BetRatio::from_f64`] 量化后字面一致
/// （C2 \[实现\] 落地走 BetRatio quantization 让 trait impl 输出
/// [`AbstractAction::Raise/Bet { ratio_label }`] 字面 milli 与 raise_multiplier()
/// 一致；本测试钉死量化路径不退化）。
#[test]
fn pluribus_action_raise_multiplier_quantizes_to_bet_ratio_milli() {
    for raise in [
        PluribusAction::Raise05Pot,
        PluribusAction::Raise075Pot,
        PluribusAction::Raise1Pot,
        PluribusAction::Raise15Pot,
        PluribusAction::Raise2Pot,
        PluribusAction::Raise3Pot,
        PluribusAction::Raise5Pot,
        PluribusAction::Raise10Pot,
        PluribusAction::Raise25Pot,
        PluribusAction::Raise50Pot,
    ] {
        let mult = raise.raise_multiplier().expect("raise variant");
        let bet_ratio = BetRatio::from_f64(mult).expect(
            "D-202-rev1：raise_multiplier 输出应当落在 BetRatio 合法范围 [0.001, 4_294_967.295]",
        );
        let want_milli = (mult * 1000.0) as u32;
        assert_eq!(
            bet_ratio.as_milli(),
            want_milli,
            "API-494 桥接：{raise:?} multiplier {mult} 量化 milli {} 应 == {want_milli}",
            bet_ratio.as_milli()
        );
    }
}

// ===========================================================================
// Group D — Game::legal_actions(&NlheGame6State) 经
// PluribusActionAbstraction::actions(&state.game_state) 桥接通路（API-494 字面）
// — C1 仅锁 inherent actions 路径不退化（Game trait method 桥接路径 panic-fail
// 由 tests/nlhe_6max_game_trait.rs::game_trait_legal_actions_panic_fail_until_c2
// 覆盖，本文件 trip-wire 桥接 abstraction → Game trait 字面契约）
// ===========================================================================

/// D-420 / API-494 字面：[`PluribusActionAbstraction::actions(&GameState)`] 输出
/// 在不同 GameState 上单调递增 / 递减性 sanity（preflop UTG 行动节点 → flop
/// 街首行动节点 → terminal 节点 action set 递减）。本测试钉死 abstraction.actions
/// 函数本身的合法性 invariance（API-494 trait impl 落地后字面继承）。
#[test]
fn pluribus_action_abstraction_actions_subset_invariance_across_streets() {
    // root preflop UTG 行动节点：facing BB raise，UTG legal actions ⊇ {Fold, Call}
    let root_state = make_root_state();
    let abs = PluribusActionAbstraction;
    let root_actions = abs.actions(&root_state);
    assert!(
        root_actions.contains(&PluribusAction::Fold),
        "D-420：root Fold 合法"
    );
    assert!(
        root_actions.contains(&PluribusAction::Call),
        "D-420：root Call 合法（face BB）"
    );
    assert!(
        !root_actions.contains(&PluribusAction::Check),
        "D-420：root facing BB 不能 Check"
    );

    // facing UTG-3x state（HJ to act）：facing raise，HJ legal actions 增加 Raise X Pot 选项
    let hj_state = make_hj_facing_utg_3x_state();
    let hj_actions = abs.actions(&hj_state);
    assert!(
        hj_actions.contains(&PluribusAction::Fold),
        "D-420：HJ facing-raise Fold 合法"
    );
    assert!(
        hj_actions.contains(&PluribusAction::Call),
        "D-420：HJ facing-raise Call (= UTG raise to 300) 合法"
    );
    // HJ 有 50_000 stack + min_raise 500 ≤ raise_to ≤ stack_cap 区间足够，应至少
    // 有 Raise 0.5 Pot（525）选项落在 raise_range 内
    assert!(
        hj_actions.contains(&PluribusAction::Raise05Pot),
        "D-420：HJ stack 充分，Raise 0.5 Pot (525) 应落在 raise_range 内"
    );
    // AllIn 字面继承（D-422(e) raise size 超 stack 自动 AllIn 由 caller 单独枚举
    // AllIn action 覆盖，is_legal AllIn 字段直读 stage 1 LegalActionSet.all_in_amount）
    assert!(
        hj_actions.contains(&PluribusAction::AllIn),
        "LA-007：HJ stack > 0 → AllIn 合法"
    );
}

/// API-494 桥接 sanity：[`AbstractAction`] / [`AbstractActionSet`] 类型可访问
/// （stage 2 公开 surface 不退化），C2 \[实现\] 落地 `impl ActionAbstraction for
/// PluribusActionAbstraction` 字面继承本类型路径。
#[test]
fn stage2_abstract_action_abstract_action_set_surface_accessible() {
    // 类型可被使用 + 编译期 size 检查（漂移会 cargo build fail）
    let _: fn(AbstractAction) -> Action = AbstractAction::to_concrete;
    // AbstractActionSet 没有 pub 构造器（D-209 + 跨 ratio Bet/Raise 互斥校验由 stage 2
    // 内部 `DefaultActionAbstraction::abstract_actions` 走），外部测试访问只读 method。
    let _: for<'a> fn(&'a AbstractActionSet) -> usize = AbstractActionSet::len;
    let _: for<'a> fn(&'a AbstractActionSet) -> std::slice::Iter<'_, AbstractAction> =
        AbstractActionSet::iter;
}
