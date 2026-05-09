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
// （不剔除）。NLHE pot 随 call 增长，default 100BB 配置下 0.5×pot raise 几乎
// 总满足 ≥ min_to。本测试断言**结构性不变量**——任何 Raise candidate 的 `to`
// ≥ stage 1 `LegalActionSet.raise_range.min_to`：fallback 触发时落到 min_to，
// 未触发时落到 ratio×pot 但仍 ≥ min_to。具体 fallback 数值场景留 C1 200+
// scenarios。
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

    let abs = DefaultActionAbstraction::default_5_action();
    let actions = abs.abstract_actions(&s);
    let slice = actions.as_slice();

    // AA-003-rev1 ①：所有 Raise candidate 的 to ≥ min_to。
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
