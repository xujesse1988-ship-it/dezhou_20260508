//! B1 §A 类：`ActionAbstraction` / `BetRatio` / `ActionAbstractionConfig` 核心
//! fixed scenario 测试。
//!
//! 覆盖 `pluribus_stage2_workflow.md` §B1 §输出 A 类清单中 5 条 action_abs_* +
//! D-202-rev1 / BetRatio::from_f64-rev1 / `ConfigError::DuplicateRatio` 量化协议
//! 断言（API §9 BetRatio::from_f64-rev1 影响 ④ 字面要求 B1 [测试] 落地）。
//!
//! **B1 状态**：A1 阶段 `BetRatio::from_f64` / `ActionAbstractionConfig::new` /
//! `DefaultActionAbstraction::abstract_actions` 等全部 `unimplemented!()`，本文件
//! 中的 `#[test]` 在第一次调用对应方法时 panic（与 stage-1 §B1 `tests/scenarios.rs`
//! 同形态：`cargo test --no-run` 通过、`cargo test` 失败）。
//!
//! **B2 状态**：方法落地后断言激活；本文件保持原文，仅 [实现] 侧填充 stub。
//!
//! 角色边界：本文件属 `[测试]` agent 产物。任一断言被 [实现] 反驳必须由决策者
//! review 后由 [测试] agent 修订（继承 stage-1 §B1 处理政策；详见
//! `pluribus_stage1_workflow.md` §修订历史 §B-rev1）。

use poker::{
    AbstractAction, AbstractActionSet, Action, ActionAbstraction, ActionAbstractionConfig,
    BetRatio, ChipAmount, ConfigError, DefaultActionAbstraction, GameState, TableConfig,
};

// ============================================================================
// 通用 fixture
// ============================================================================

/// 6-max 默认 100BB 配置 + seed=0 起手。
fn default_state(seed: u64) -> (GameState, TableConfig) {
    let cfg = TableConfig::default_6max_100bb();
    let state = GameState::new(&cfg, seed);
    (state, cfg)
}

/// 短码 BB fixture（stack=450，let BB 在 3-bet 后 min_to 超 stack）。
fn short_bb_state(seed: u64) -> (GameState, TableConfig) {
    let mut cfg = TableConfig::default_6max_100bb();
    cfg.starting_stacks[2] = ChipAmount::new(450); // BB seat 2 短码
    let state = GameState::new(&cfg, seed);
    (state, cfg)
}

/// 把 UTG / MP / CO 打 fold 到 BTN，前进到 BTN 决策点。
fn fold_to_btn(state: &mut GameState) {
    use poker::SeatId;
    for seat_idx in [3u8, 4, 5] {
        let cp = state.current_player().expect("non-terminal");
        assert_eq!(cp, SeatId(seat_idx), "fixture: 期望 fold-to-btn 顺序");
        state.apply(Action::Fold).expect("fixture fold");
    }
}

// ============================================================================
// 1. action_abs_default_5_actions_open_raise_legal
// ============================================================================
//
// 6-max 默认 100BB，UTG 起手 3-bet 局面：UTG / MP / CO fold，BTN 面对盲注 +
// limpers，触发 D-200 默认 5-action：`{ Fold, Call, Bet/Raise(0.5×pot),
// Bet/Raise(1.0×pot), AllIn }`（无 `Check`，因为面对前序 bet）。
#[test]
fn action_abs_default_5_actions_open_raise_legal() {
    let (mut s, _cfg) = default_state(0);
    fold_to_btn(&mut s);

    let abs = DefaultActionAbstraction::default_5_action();
    let actions: AbstractActionSet = abs.abstract_actions(&s);

    // BTN 面对 BB（强制 bet），D-200 5-action：
    //   - Fold（保留，D-204 仅在 free-check 局面剔除）
    //   - Call（跟注 BB）
    //   - Raise(0.5×pot)（D-200，本下注轮已有前序 bet，输出 Raise）
    //   - Raise(1.0×pot)
    //   - AllIn
    // **不**含 Check（无 free-check option）；Bet 与 Raise 由 LA-002 互斥决定，
    // 此处 max_committed_this_round = BB > 0 ⇒ Raise 路径。
    let slice = actions.as_slice();
    assert!(!slice.is_empty(), "AA-005 集合非空");
    assert!(
        actions.len() >= 4,
        "open-raise 局面至少 Fold+Call+Raise+AllIn"
    );

    // AA-002：面对 bet 局面 Fold 必须保留（仅 free-check 时剔除）。
    assert!(
        slice.iter().any(|a| matches!(a, AbstractAction::Fold)),
        "AA-002：面对 bet 局面 Fold 不可剔除"
    );
    // AA-002：Check 在面对 bet 局面不应出现。
    assert!(
        !slice.iter().any(|a| matches!(a, AbstractAction::Check)),
        "AA-002：面对 bet 局面无 Check"
    );

    // D-200 / AA-001：D-209 顺序 Fold? / Check? / Call? / Bet|Raise(0.5×) / Bet|Raise(1.0×) / AllIn?。
    // 本场景为 Fold / Call / Raise(0.5×) / Raise(1.0×) / AllIn 5 项。
    let pos_fold = slice.iter().position(|a| matches!(a, AbstractAction::Fold));
    let pos_call = slice
        .iter()
        .position(|a| matches!(a, AbstractAction::Call { .. }));
    let pos_allin = slice
        .iter()
        .position(|a| matches!(a, AbstractAction::AllIn { .. }));
    assert_eq!(pos_fold, Some(0), "AA-001：Fold 占位 0");
    assert!(pos_call.is_some_and(|p| p > 0), "AA-001：Call 在 Fold 之后");
    assert!(
        pos_allin.is_some_and(|p| p == slice.len() - 1),
        "AA-001：AllIn 占位末尾"
    );

    // AA-007 deterministic smoke：默认 5-action 配置同 GameState 重复 16 次结果一致。
    let baseline = abs.abstract_actions(&s);
    for _ in 0..16 {
        let other = abs.abstract_actions(&s);
        assert_eq!(
            baseline.as_slice(),
            other.as_slice(),
            "AA-007 deterministic smoke"
        );
    }
}

// ============================================================================
// 2. action_abs_fold_disallowed_after_check
// ============================================================================
//
// D-204 字面：`LegalActionSet.check == true` 时 Fold 必须被剔除。最简单的
// `check == true` 局面：preflop fold-to-button-walk + BB 在 SB-fold 后获得 walk
// 选项；或 postflop 任意街首动作。我们用 postflop SB-vs-BB 单挑 flop check
// 局面（heads-up，UTG/MP/CO/BTN 全 fold，SB call，BB check 进 flop）。
#[test]
fn action_abs_fold_disallowed_after_check() {
    use poker::SeatId;
    let (mut s, _cfg) = default_state(7);

    // Preflop：UTG/MP/CO/BTN fold，SB limp（call BB），BB check（free option）。
    for seat_idx in [3u8, 4, 5, 0] {
        assert_eq!(s.current_player(), Some(SeatId(seat_idx)));
        s.apply(Action::Fold).expect("preflop fold");
    }
    assert_eq!(s.current_player(), Some(SeatId(1)), "SB 决策");
    s.apply(Action::Call).expect("SB limp");
    assert_eq!(s.current_player(), Some(SeatId(2)), "BB 决策");
    s.apply(Action::Check).expect("BB check → flop");

    // 进入 flop，SB 先动（postflop 起手）。SB 面对 free-check 局面，D-204
    // 强制剔除 Fold。
    let abs = DefaultActionAbstraction::default_5_action();
    let actions = abs.abstract_actions(&s);
    let slice = actions.as_slice();

    assert!(
        !slice.iter().any(|a| matches!(a, AbstractAction::Fold)),
        "D-204：free-check 局面 Fold 必须剔除"
    );
    assert!(
        slice.iter().any(|a| matches!(a, AbstractAction::Check)),
        "AA-005：free-check 局面 Check 必须存在"
    );
}

// ============================================================================
// 3. action_abs_bet_pot_falls_back_to_min_raise_when_below
// ============================================================================
//
// D-205 / AA-003-rev1 ① fallback：`x×pot < min_to` 时 candidate_to ← min_to
// （**不剔除**——candidate 必须保留，仅 to 字段升到 min_to）。
//
// 本测试用极小自定义 ratio = 0.001（量化 milli=1）人为构造 `x×pot < min_to`
// 场景，确保 fallback 路径被真正驱动而非"恰好满足"。UTG 开局：pot=150，
// max_committed=100，min_to=200；0.001×pot raise candidate ≈ 100 + 0.001×250 ≈
// 100.25，远低于 min_to=200。AA-003-rev1 ① 要求输出 Raise { to=200,
// ratio_label=BetRatio(milli=1) }（候选保留，to 升到 min_to）；若 [实现] 误
// 把 under-min candidate 直接丢弃，本测试 fail。
//
// （前一版本仅做"`Raise.to >= min_to` 结构性断言"，无法分辨 [实现] 是否真的
// 走了 fallback 路径——0.5×pot 在默认 100BB 上恰好 ≥ min_to，候选丢失也能过。）
#[test]
fn action_abs_bet_pot_falls_back_to_min_raise_when_below() {
    use poker::SeatId;
    let (s, _cfg) = default_state(0);

    // UTG 起手决策（D-028 dealing：UTG = SeatId(3)）。
    assert_eq!(s.current_player(), Some(SeatId(3)));

    let la = s.legal_actions();
    let min_to = la
        .raise_range
        .map(|(min, _max)| min)
        .expect("UTG 开局应可 raise（LA-005）");

    // 自定义 ratio = 0.001 (milli=1)：极小，必然 < min_to ⇒ 必触 AA-003-rev1 ①。
    let cfg = ActionAbstractionConfig::new(vec![0.001])
        .expect("D-202-rev1：单元素 0.001 合法（milli=1，∈ [1, u32::MAX]）");
    let tiny_label = cfg.raise_pot_ratios[0];
    assert_eq!(
        tiny_label.as_milli(),
        1,
        "fixture：0.001 量化到 milli=1（half-to-even）"
    );
    let abs = DefaultActionAbstraction::new(cfg);
    let actions = abs.abstract_actions(&s);
    let slice = actions.as_slice();

    // AA-003-rev1 ①：under-min candidate 必须**保留**——输出中必须存在
    //   Raise { to = min_to, ratio_label = tiny_label }，不可被丢弃。
    let raise = slice
        .iter()
        .find(|a| matches!(a, AbstractAction::Raise { ratio_label, .. } if *ratio_label == tiny_label))
        .unwrap_or_else(|| {
            panic!(
                "AA-003-rev1 ①：tiny ratio (milli=1) candidate 必须保留并 fallback 到 min_to，\
                 不可被丢弃；输出 = {slice:?}"
            )
        });
    let raise_to = match raise {
        AbstractAction::Raise { to, .. } => *to,
        _ => unreachable!(),
    };
    assert_eq!(
        raise_to.as_u64(),
        min_to.as_u64(),
        "AA-003-rev1 ①：tiny ratio fallback 到 min_to ({}), got {}",
        min_to.as_u64(),
        raise_to.as_u64()
    );

    // 旁路结构性断言：所有 Raise candidate 的 to ≥ min_to（保留作冗余 invariant）。
    let raise_tos: Vec<ChipAmount> = slice
        .iter()
        .filter_map(|a| match a {
            AbstractAction::Raise { to, .. } => Some(*to),
            _ => None,
        })
        .collect();
    assert!(!raise_tos.is_empty(), "UTG 开局应有 Raise candidate");
    for to in raise_tos {
        assert!(
            to.as_u64() >= min_to.as_u64(),
            "AA-003-rev1 ①：Raise.to ({} chips) ≥ min_to ({} chips)",
            to.as_u64(),
            min_to.as_u64()
        );
    }
}

// ============================================================================
// 4. action_abs_bet_falls_back_to_allin_when_above_stack
// ============================================================================
//
// D-205 / AA-003-rev1 ②：`candidate_to >= committed_this_round + stack` 时整
// 动作改为 `AllIn { to = committed + stack }`。短码 BB（stack=450，已投 100
// 盲注，可投余额 350），UTG 开 raise 到 200，SB fold，BB 面对 raise：1.0×pot
// = ceil(200 + 1.0×(50+100+200+200)) = 750，远超 BB 剩余 stack 350。AA-003-rev1
// ② 要求该动作折叠到 `AllIn { to = 100 + 350 = 450 }`。
//
// 同时 AA-004-rev1：若 Call { to = 200 } 与 Raise(0.5×) 折叠后 to = 450 = stack
// 上限，应保留 AllIn 不保留 Call/Raise。本测试不强制断言折叠优先级（C1 才接
// 200+ scenarios），仅断言 AllIn 出现且 to = 450。
#[test]
fn action_abs_bet_falls_back_to_allin_when_above_stack() {
    use poker::SeatId;
    let (mut s, _cfg) = short_bb_state(0);

    // UTG raise to 200，MP / CO / BTN / SB fold，BB 决策。
    assert_eq!(s.current_player(), Some(SeatId(3)));
    s.apply(Action::Raise {
        to: ChipAmount::new(200),
    })
    .expect("UTG open");
    for seat_idx in [4u8, 5, 0, 1] {
        assert_eq!(s.current_player(), Some(SeatId(seat_idx)));
        s.apply(Action::Fold).expect("fold");
    }
    assert_eq!(s.current_player(), Some(SeatId(2)), "短码 BB 决策");

    let abs = DefaultActionAbstraction::default_5_action();
    let actions = abs.abstract_actions(&s);
    let slice = actions.as_slice();

    // AA-003-rev1 ②：1.0×pot raise 超 stack → AllIn 必须出现。
    let allin = slice
        .iter()
        .find(|a| matches!(a, AbstractAction::AllIn { .. }))
        .expect("AA-003-rev1 ②：超 stack candidate 必须 fallback 到 AllIn");
    let allin_to = match allin {
        AbstractAction::AllIn { to } => *to,
        _ => unreachable!(),
    };
    // BB 已投 100 盲注，stack 余 350，AllIn to = 100 + 350 = 450。
    assert_eq!(
        allin_to.as_u64(),
        450,
        "AA-003-rev1 ②：AllIn.to = committed + stack"
    );
}

// ============================================================================
// 4b. action_abs_short_bb_3bet_min_to_above_stack_priority
// ============================================================================
//
// API §F20 影响 ③（`pluribus_stage2_api.md` line 797）字面要求：B1 [测试]
// `tests/action_abstraction.rs` 阶段 2 版必须含**至少 2 个 case**断言短码 BB
// 面对 3-bet → `min_to >= committed + stack` 时 AA-003-rev1 ①+② 同时触发的
// 优先级（first-match-wins ① → ②，等价口语化：先 floor 到 min_to，再 ceil
// 到 committed+stack；同时触发走 AllIn）。
//
// **Case 1（AA-003-rev1 ①+② 联合优先级）**：BB starting_stack=800（盲注 100
// 扣后 stack=700，committed_this_round=100，cap=committed+stack=800），UTG
// raise to 200 → BTN 3-bet to 500 → SB fold → BB 决策。BB max_committed=500，
// last_full_raise_size=300，min_to=500+300=800；cap=800 ≥ min_to=800 ⇒ stage 1
// raise_range = Some((800, 800))（min_to == cap，"all-in-only-raise" 边界）。
//
// 用自定义 ratio = 0.001 (milli=1) 驱动 ①：candidate_to ≈ call_to + 0.001×pot
// 远小于 min_to=800 ⇒ ① floor 到 min_to=800；继而 800 ≥ committed+stack=800 ⇒
// ② ceil 到 AllIn { to=800 }。两步顺序固定（先 floor 再 ceil），同时触发时
// 走 AllIn——这是 AA-003-rev1 ①+② **联合优先级**唯一可观测路径（API §F20
// 影响 ③）。
//
// 断言：① AllIn { to = 800 } 存在（联合优先级输出）；② 输出中**无**带 tiny
// ratio_label 的 Raise candidate（① 后的 min_to=800 candidate 必须被 ② 吸收
// 进 AllIn，不应作为独立 Raise 出现）；③ Call { to = 500 } 存在（与 AllIn
// 不同 to，AA-004-rev1 dedup 不触）。
#[test]
fn action_abs_short_bb_3bet_min_to_above_stack_priority_case1() {
    use poker::SeatId;
    let mut tcfg = TableConfig::default_6max_100bb();
    tcfg.starting_stacks[2] = ChipAmount::new(800); // BB starting=800 ⇒ cap=800
    let mut s = GameState::new(&tcfg, 0);

    // UTG raise to 200 → MP/CO fold → BTN 3-bet to 500 → SB fold → BB 决策。
    assert_eq!(s.current_player(), Some(SeatId(3)));
    s.apply(Action::Raise {
        to: ChipAmount::new(200),
    })
    .expect("UTG open raise");
    for seat_idx in [4u8, 5] {
        assert_eq!(s.current_player(), Some(SeatId(seat_idx)));
        s.apply(Action::Fold).expect("fold");
    }
    assert_eq!(s.current_player(), Some(SeatId(0)));
    s.apply(Action::Raise {
        to: ChipAmount::new(500),
    })
    .expect("BTN 3-bet");
    assert_eq!(s.current_player(), Some(SeatId(1)));
    s.apply(Action::Fold).expect("SB fold");

    assert_eq!(s.current_player(), Some(SeatId(2)));
    let la = s.legal_actions();
    let raise_range = la
        .raise_range
        .expect("fixture: cap = min_to ⇒ raise_range = Some");
    assert_eq!(
        (raise_range.0.as_u64(), raise_range.1.as_u64()),
        (800, 800),
        "fixture：min_to == cap == 800（AA-003-rev1 ①+② 联合优先级边界）"
    );

    // 自定义 tiny ratio [0.001] 触发 ① floor 到 min_to=800 → ② ceil 到 AllIn。
    let abs_cfg = ActionAbstractionConfig::new(vec![0.001])
        .expect("D-202-rev1：单元素 0.001 合法（milli=1）");
    let tiny_label = abs_cfg.raise_pot_ratios[0];
    let abs = DefaultActionAbstraction::new(abs_cfg);
    let actions = abs.abstract_actions(&s);
    let slice = actions.as_slice();

    // ① AllIn { to = 800 } 存在。
    let allin = slice
        .iter()
        .find(|a| matches!(a, AbstractAction::AllIn { .. }))
        .expect("AA-003-rev1 ①+②：联合优先级 ⇒ AllIn 必须出现");
    let allin_to = match allin {
        AbstractAction::AllIn { to } => *to,
        _ => unreachable!(),
    };
    assert_eq!(
        allin_to.as_u64(),
        800,
        "AA-003-rev1 ②：① 后 min_to=800 ≥ cap=800 ⇒ AllIn.to = cap = 800"
    );

    // ② 带 tiny_label 的 Raise candidate **不应**作为独立 Raise 出现——已被
    // ② 吸收进 AllIn 槽位。任何 Raise 出现都意味着 [实现] 漏走 ② ceil 路径。
    let stale_raise = slice.iter().find(
        |a| matches!(a, AbstractAction::Raise { ratio_label, .. } if *ratio_label == tiny_label),
    );
    assert!(
        stale_raise.is_none(),
        "AA-003-rev1 ②：① floor 后的 candidate 必须被 ② 吸收进 AllIn，\
         不应留下独立 Raise；slice = {slice:?}"
    );

    // ③ Call { to = 500 } 存在（call_to = min(max_committed=500, cap=800) = 500，
    // 与 AllIn 800 不同 to ⇒ AA-004-rev1 不触发 dedup）。
    let call_at_500 = slice
        .iter()
        .any(|a| matches!(a, AbstractAction::Call { to } if to.as_u64() == 500));
    assert!(
        call_at_500,
        "Call {{ to = 500 }} 必须保留（与 AllIn=800 不同 to）；slice = {slice:?}"
    );
}

// **Case 2（AA-004-rev1 Call/AllIn 同 to dedup）**：BB stack=400（committed
// 100 + remaining 300，cap=400），同 UTG raise to 200 / BTN 3-bet to 500 路径。
// BB max_committed=500，cap=400 < max_committed=500 ⇒ stage 1 raise_range=None
// （cap > max_committed 不成立）。call_to = min(500, 400) = 400 = cap。
// all_in_amount = Some(cap=400)。
//
// 抽象侧无 raise candidate（raise_range=None ⇒ AA-003 不触发），但 Call {
// to=400 } 与 AllIn { to=400 } 同 to ⇒ AA-004-rev1 dedup 必须保留 AllIn、
// 移除 Call。
#[test]
fn action_abs_short_bb_3bet_min_to_above_stack_priority_case2() {
    use poker::SeatId;
    let mut tcfg = TableConfig::default_6max_100bb();
    tcfg.starting_stacks[2] = ChipAmount::new(400); // BB 极短码
    let mut s = GameState::new(&tcfg, 0);

    s.apply(Action::Raise {
        to: ChipAmount::new(200),
    })
    .expect("UTG open raise");
    for _ in 0..2 {
        s.apply(Action::Fold).expect("fold");
    }
    s.apply(Action::Raise {
        to: ChipAmount::new(500),
    })
    .expect("BTN 3-bet");
    s.apply(Action::Fold).expect("SB fold");

    assert_eq!(s.current_player(), Some(SeatId(2)));
    let la = s.legal_actions();
    assert!(
        la.raise_range.is_none(),
        "fixture：cap=400 < max_committed=500 ⇒ raise_range=None"
    );
    assert_eq!(la.call.map(|c| c.as_u64()), Some(400));
    assert_eq!(la.all_in_amount.map(|c| c.as_u64()), Some(400));

    let abs = DefaultActionAbstraction::default_5_action();
    let actions = abs.abstract_actions(&s);
    let slice = actions.as_slice();

    // AllIn { to = 400 } 存在。
    let allin = slice
        .iter()
        .find(|a| matches!(a, AbstractAction::AllIn { .. }))
        .expect("AllIn slot：all_in_amount=Some(400) ⇒ AllIn 必须出现");
    assert_eq!(
        match allin {
            AbstractAction::AllIn { to } => to.as_u64(),
            _ => unreachable!(),
        },
        400,
        "AllIn.to = cap = 400"
    );

    // AA-004-rev1：Call { to = 400 } 与 AllIn { to = 400 } 同 to ⇒ 保留 AllIn、
    // 移除 Call。
    let call_at_400 = slice
        .iter()
        .any(|a| matches!(a, AbstractAction::Call { to } if to.as_u64() == 400));
    assert!(
        !call_at_400,
        "AA-004-rev1：Call {{ to = 400 }} 必须被 AllIn 吸收；slice = {slice:?}"
    );

    // 全局 to 去重不变量（AA-004-rev1 一般化）：任何带 to 的两个 AbstractAction
    // 不应共享相同 to 数值。
    let mut tos: Vec<u64> = slice
        .iter()
        .filter_map(|a| match a {
            AbstractAction::Call { to }
            | AbstractAction::Bet { to, .. }
            | AbstractAction::Raise { to, .. }
            | AbstractAction::AllIn { to } => Some(to.as_u64()),
            _ => None,
        })
        .collect();
    let len_before = tos.len();
    tos.sort_unstable();
    tos.dedup();
    assert_eq!(
        tos.len(),
        len_before,
        "AA-004-rev1：所有带 to 的 AbstractAction `to` 字段必须互不相等（slice = {slice:?}）"
    );
}

// ============================================================================
// 5. action_abs_determinism_repeat_smoke
// ============================================================================
//
// AA-007 deterministic：同 (GameState, ActionAbstractionConfig) 重复调用
// `abstract_actions` 1000 次结果完全相同（含 Vec 内 byte-equal）。**B1 默认
// 1k**；full 1M 留 D1（与 stage-1 §B1 同形态：B1 1k smoke，D1 才接 1M）。
#[test]
fn action_abs_determinism_repeat_smoke() {
    let (mut s, _cfg) = default_state(42);
    fold_to_btn(&mut s);

    let abs = DefaultActionAbstraction::default_5_action();
    let baseline = abs.abstract_actions(&s);
    for i in 0..1_000 {
        let other = abs.abstract_actions(&s);
        assert_eq!(
            baseline.as_slice(),
            other.as_slice(),
            "AA-007 iter {i}: byte-equal"
        );
    }
}

// ============================================================================
// 6. BetRatio::from_f64 量化协议（D-202-rev1 / BetRatio::from_f64-rev1 / F27）
// ============================================================================
//
// API §9 BetRatio::from_f64-rev1 影响 ④ 字面要求 B1 [测试] 断言：
//   1. half-to-even rounding（0.5005 → milli=500，0.5004 → milli=500）
//   2. 越界返回 None（< 0.001 / NaN / Inf / 负 / 0.0）
//   3. ActionAbstractionConfig::new(vec![0.5, 0.5005]) → DuplicateRatio
#[test]
fn bet_ratio_from_f64_half_to_even() {
    // 默认常量值（A1 const 已落地，不依赖 unimplemented stub）。
    assert_eq!(BetRatio::HALF_POT.as_milli(), 500);
    assert_eq!(BetRatio::FULL_POT.as_milli(), 1000);

    // half-to-even：0.5005 → 500.5 → 500（ties to even）；0.5015 → 501.5 → 502。
    let half_via_f64 = BetRatio::from_f64(0.5005).expect("0.5005 合法");
    assert_eq!(
        half_via_f64.as_milli(),
        500,
        "D-202-rev1：half-to-even 0.5005 量化到 500"
    );
    assert_eq!(half_via_f64, BetRatio::HALF_POT, "BetRatio Eq");

    // 0.5004 量化到 500（向下舍入，非 ties）。
    assert_eq!(
        BetRatio::from_f64(0.5004).map(BetRatio::as_milli),
        Some(500)
    );

    // 0.5015 量化到 502（half-to-even：501.5 → 502 even）。
    assert_eq!(
        BetRatio::from_f64(0.5015).map(BetRatio::as_milli),
        Some(502),
        "D-202-rev1：half-to-even 0.5015 量化到 502 (even)"
    );

    // 0.5025 量化到 502（half-to-even：502.5 → 502 even）。
    assert_eq!(
        BetRatio::from_f64(0.5025).map(BetRatio::as_milli),
        Some(502),
        "D-202-rev1：half-to-even 0.5025 量化到 502 (even)"
    );
}

#[test]
fn bet_ratio_from_f64_out_of_range_returns_none() {
    // < 0.001 / NaN / Inf / 负 / 0.0 / > u32::MAX/1000 → None。
    assert_eq!(BetRatio::from_f64(-1.0), None, "D-202-rev1：负数返回 None");
    assert_eq!(BetRatio::from_f64(0.0), None, "D-202-rev1：0.0 返回 None");
    assert_eq!(
        BetRatio::from_f64(0.0005),
        None,
        "D-202-rev1：< 0.001 返回 None"
    );
    assert_eq!(
        BetRatio::from_f64(f64::NAN),
        None,
        "D-202-rev1：NaN 返回 None"
    );
    assert_eq!(
        BetRatio::from_f64(f64::INFINITY),
        None,
        "D-202-rev1：Inf 返回 None"
    );
    assert_eq!(
        BetRatio::from_f64(f64::NEG_INFINITY),
        None,
        "D-202-rev1：负 Inf 返回 None"
    );
    assert_eq!(
        BetRatio::from_f64(5_000_000.0),
        None,
        "D-202-rev1：> 4_294_967.295 返回 None"
    );
}

#[test]
fn action_abstraction_config_new_duplicate_ratio_after_quantization() {
    // 0.5 与 0.5005 半向偶舍入后均为 milli=500，触发 ConfigError::DuplicateRatio。
    let result = ActionAbstractionConfig::new(vec![0.5, 0.5005]);
    match result {
        Err(ConfigError::DuplicateRatio { milli }) => assert_eq!(
            milli, 500,
            "D-202-rev1：DuplicateRatio.milli 报告冲突量化值"
        ),
        Err(other) => panic!("D-202-rev1：期望 DuplicateRatio，得到 {other:?}"),
        Ok(cfg) => panic!(
            "D-202-rev1：期望 DuplicateRatio 错误，得到 Ok with raise_count={}",
            cfg.raise_count()
        ),
    }
}

#[test]
fn action_abstraction_config_new_count_out_of_range() {
    // D-202：raise_pot_ratios 长度 ∈ [1, 14]。空 vec 与 15 元素 vec 均越界。
    let empty = ActionAbstractionConfig::new(vec![]);
    assert!(
        matches!(empty, Err(ConfigError::RaiseCountOutOfRange(0))),
        "D-202：长度 0 应返回 RaiseCountOutOfRange(0)"
    );

    let too_many: Vec<f64> = (1..=15).map(f64::from).collect();
    assert!(
        matches!(
            ActionAbstractionConfig::new(too_many),
            Err(ConfigError::RaiseCountOutOfRange(15))
        ),
        "D-202：长度 15 应返回 RaiseCountOutOfRange(15)"
    );
}

// ============================================================================
// 7. AbstractAction::to_concrete §7 桥接（API §7）
// ============================================================================
//
// `AbstractAction → Action` 字段提取。**无状态**——构造时由 LegalActionSet 区分
// Bet 与 Raise，转换无歧义。本测试构造 6 类 AbstractAction 实例（不依赖
// `abstract_actions` stub），断言 `to_concrete()` 字段映射正确。
#[test]
fn abstract_action_to_concrete_field_mapping() {
    // Fold / Check 不带 to。
    assert_eq!(AbstractAction::Fold.to_concrete(), Action::Fold);
    assert_eq!(AbstractAction::Check.to_concrete(), Action::Check);

    // Call { to } → Action::Call（stage 1 Call 不带 to）。
    let call = AbstractAction::Call {
        to: ChipAmount::new(200),
    };
    assert_eq!(call.to_concrete(), Action::Call);

    // Bet / Raise / AllIn 字段提取。
    let bet = AbstractAction::Bet {
        to: ChipAmount::new(150),
        ratio_label: BetRatio::HALF_POT,
    };
    assert_eq!(
        bet.to_concrete(),
        Action::Bet {
            to: ChipAmount::new(150)
        }
    );

    let raise = AbstractAction::Raise {
        to: ChipAmount::new(450),
        ratio_label: BetRatio::FULL_POT,
    };
    assert_eq!(
        raise.to_concrete(),
        Action::Raise {
            to: ChipAmount::new(450)
        }
    );

    let allin = AbstractAction::AllIn {
        to: ChipAmount::new(10_000),
    };
    assert_eq!(allin.to_concrete(), Action::AllIn);
}
