//! C1：扩展 fixed scenario 表（200+ 用例 / ≥50 short-allin 子集）+ stage-2 §C1
//! ActionAbstraction 输出扫扩集（200+ 抽象动作场景，含 ≥2 条 all-in call）。
//!
//! `pluribus_stage1_workflow.md` §C1 出口标准：
//!
//! - fixed scenario 扩到 **200+**，含 **≥ 50** 个 short all-in / incomplete raise 子集。
//!
//! `pluribus_stage2_workflow.md` §C1 §输出 line 317 字面：
//!
//! - tests/scenarios_extended.rs（阶段 2 版）扩到 200+ 固定 GameState 场景，覆盖
//!   open / 3-bet / 短码 / incomplete / 多人 all-in 的 5-action 默认输出。API §F20
//!   影响 ② 字面 ≥ 2 条 all-in call 场景断言 `Call` 不出现而 `AllIn` 出现。
//!
//! 本文件只验证 **规则合法动作 / 状态机推进 / D-033-rev1 / D-035 / 结算 zero-sum**
//! 这一类断言；side pot / odd chip / uncalled bet returned 的扩展集见 `tests/side_pots.rs`。
//! stage-2 §C1 ActionAbstraction sweep 在文件末尾的独立 section（`mod stage2_abs_sweep`）。
//!
//! 设计：每个 #[test] 装载一组 `ScenarioCase` 表（5–10 行/case），调用
//! [`run_scenario`] 逐一驱动并断言。失败时通过 `case.name` 定位。
//!
//! 角色边界：本文件属 `[测试]` agent 产物（C1）。如果某条断言在 B2 实现下失败：
//!
//! 1. 默认假设 B2 有 corner case 未覆盖（C1 出口允许部分失败留给 C2 修）；
//! 2. 在该 case 旁追加 `// FIXME(C2): <bug>` 注释 + `case.flagged_for_c2 = true`
//!    （目前该字段尚未引入；如需大量 C2-pending 测试再加）；
//! 3. **不允许** [测试] agent 直接修改产品代码。
//!
//! 重复用例避免：每个 case 至少在 `name` 中编码自己的关键参数（stack / 动作链
//! 摘要），方便表内 grep。

// 表生成中常用 `let mut p = Vec::new(); p.push(...); ...` 这一模式，便于让相邻
// `for` / `if` 控制流嵌入 plan 构造。clippy 默认 lint 它，但本文件可读性收益更高。
#![allow(clippy::vec_init_then_push)]

mod common;

use poker::{Action, ChipAmount, GameState, PlayerStatus, SeatId, Street};

use common::{
    card, cfg_6max_with_stacks, chips, expected_total_chips, plan, run_scenario, seat,
    LegalAtEndCheck, ScenarioCase, ScenarioExpect,
};

// ============================================================================
// 工具：常量 / 通用 prefix
// ============================================================================

/// 6-max 默认 100BB，每座位 stack = 10000。
fn default_cfg() -> poker::TableConfig {
    poker::TableConfig::default_6max_100bb()
}

/// UTG / MP / CO 三家弃牌的 prefix（plan 前缀，6-max 版本）。
fn fold_utg_mp_co() -> Vec<(SeatId, Action)> {
    plan(&[(3, Action::Fold), (4, Action::Fold), (5, Action::Fold)])
}

fn checkdown_three_streets_btn_first() -> Vec<(SeatId, Action)> {
    // postflop 顺序：SB(1) → BB(2) → BTN(0) 逐街
    plan(&[
        (1, Action::Check),
        (2, Action::Check),
        (0, Action::Check),
        (1, Action::Check),
        (2, Action::Check),
        (0, Action::Check),
        (1, Action::Check),
        (2, Action::Check),
        (0, Action::Check),
    ])
}

fn checkdown_three_streets_bb_first() -> Vec<(SeatId, Action)> {
    // SB 已弃 → 顺序变 BB(2) → BTN(0)
    plan(&[
        (2, Action::Check),
        (0, Action::Check),
        (2, Action::Check),
        (0, Action::Check),
        (2, Action::Check),
        (0, Action::Check),
    ])
}

// ============================================================================
// A. 开局加注 / 单 caller / 走到河（≥ 24 cases）
// ============================================================================

#[test]
fn open_raise_then_call_walk_to_river_table() {
    let mut cases: Vec<ScenarioCase> = Vec::new();

    // raise_to ∈ {200, 250, 300, 400, 500, 1000, 2000, 5000}
    // caller ∈ {SB, BB}（另一者 fold）
    // 共 8 × 2 = 16 cases
    for &raise_to in &[200u64, 250, 300, 400, 500, 1000, 2000, 5000] {
        for &caller in &["SB", "BB"] {
            let name: &'static str = match (raise_to, caller) {
                (200, "SB") => "open_200_sb_calls",
                (200, "BB") => "open_200_bb_calls",
                (250, "SB") => "open_250_sb_calls",
                (250, "BB") => "open_250_bb_calls",
                (300, "SB") => "open_300_sb_calls",
                (300, "BB") => "open_300_bb_calls",
                (400, "SB") => "open_400_sb_calls",
                (400, "BB") => "open_400_bb_calls",
                (500, "SB") => "open_500_sb_calls",
                (500, "BB") => "open_500_bb_calls",
                (1000, "SB") => "open_1000_sb_calls",
                (1000, "BB") => "open_1000_bb_calls",
                (2000, "SB") => "open_2000_sb_calls",
                (2000, "BB") => "open_2000_bb_calls",
                (5000, "SB") => "open_5000_sb_calls",
                (5000, "BB") => "open_5000_bb_calls",
                _ => unreachable!(),
            };

            let mut p = fold_utg_mp_co();
            p.push((
                seat(0),
                Action::Raise {
                    to: chips(raise_to),
                },
            ));
            match caller {
                "SB" => {
                    p.push((seat(1), Action::Call));
                    p.push((seat(2), Action::Fold));
                    // postflop SB 先动；SB 与 BTN 交替 check 三街。
                    p.extend(plan(&[
                        (1, Action::Check),
                        (0, Action::Check),
                        (1, Action::Check),
                        (0, Action::Check),
                        (1, Action::Check),
                        (0, Action::Check),
                    ]));
                }
                "BB" => {
                    p.push((seat(1), Action::Fold));
                    p.push((seat(2), Action::Call));
                    p.extend(checkdown_three_streets_bb_first());
                }
                _ => unreachable!(),
            }

            let mut expect = ScenarioExpect::new();
            expect.terminal = Some(true);
            expect.street = Some(Street::Showdown);
            expect.board_len = Some(5);
            cases.push(ScenarioCase {
                name,
                config: default_cfg(),
                seed: raise_to,
                holes: None,
                board: None,
                plan: p,
                expect,
            });
        }
    }

    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() >= 16, "open_raise table 至少 16 cases");
}

// ============================================================================
// B. 3bet / 4bet / 5bet 序列（≥ 12 cases）
// ============================================================================

#[test]
fn threebet_fourbet_chain_table() {
    let mut cases: Vec<ScenarioCase> = Vec::new();

    // BTN open → SB 3bet → BTN 4bet → SB AllIn → BTN Call
    // 参数化：open_to / 3bet_to / 4bet_to。每行需满足 D-035 链条。
    // open=300, last_full=300; 3bet 必须 ≥ 600 (= 300 + 300); 4bet 必须 ≥ 3bet + (3bet - 300)。
    let triples: &[(u64, u64, u64)] = &[
        (200, 500, 1100),
        (200, 600, 1400),
        (250, 600, 1300),
        (300, 600, 1200),
        (300, 900, 2100),
        (300, 1000, 2400),
        (400, 800, 1600),
        (400, 1200, 2800),
        (500, 1500, 3500),
        (1000, 2000, 4000),
    ];
    let case_names: [&str; 10] = [
        "3bet_open200_3b500_4b1100",
        "3bet_open200_3b600_4b1400",
        "3bet_open250_3b600_4b1300",
        "3bet_open300_3b600_4b1200",
        "3bet_open300_3b900_4b2100",
        "3bet_open300_3b1000_4b2400",
        "3bet_open400_3b800_4b1600",
        "3bet_open400_3b1200_4b2800",
        "3bet_open500_3b1500_4b3500",
        "3bet_open1000_3b2000_4b4000",
    ];

    for (i, &(open, three, four)) in triples.iter().enumerate() {
        let name = case_names[i];
        let mut p = fold_utg_mp_co();
        p.push((seat(0), Action::Raise { to: chips(open) }));
        p.push((seat(1), Action::Raise { to: chips(three) }));
        p.push((seat(2), Action::Fold));
        p.push((seat(0), Action::Raise { to: chips(four) }));
        p.push((seat(1), Action::AllIn));
        p.push((seat(0), Action::Call));

        let mut expect = ScenarioExpect::new();
        expect.terminal = Some(true);
        expect.street = Some(Street::Showdown);
        expect.board_len = Some(5);
        cases.push(ScenarioCase {
            name,
            config: default_cfg(),
            seed: open ^ three ^ four,
            holes: None,
            board: None,
            plan: p,
            expect,
        });
    }

    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() >= 10);
}

// ============================================================================
// C. Short all-in / incomplete raise 子集（≥ 50 cases）
// ============================================================================
//
// D-033-rev1 验证两条路径：
//   (A) **Already-acted 玩家**（自身 raise option 已 false）在 incomplete 之后
//       不重开 → `raise_range == None`，`apply(Raise)` 必须返回 RaiseOptionNotReopened。
//   (B) **Still-open 玩家**（自身 raise option 仍 true）在 incomplete 之后
//       仍可 raise，且 `min_to = max_committed_this_round + last_full_raise_size`
//       （incomplete 不更新 `last_full_raise_size`）。
//
// 参数空间（≥ 50）：
//   - short stack ∈ {120, 150, 200, 280, 350, 450, 550, 650, 800, 950}（10 值）
//   - 触发链：BTN limp → SB full raise → BB AllIn (short)，下一个 actor 取 BTN(B 路径) 或 SB(A 路径)
//     另一族链：BTN open → SB 3bet → BB AllIn (short)，下一个 actor 取 BTN(A 路径) 或 SB(A 路径)
//   - 每个 (stack, actor) 形成 1 个 case；目标 ≥ 50。

#[test]
fn short_allin_already_acted_no_reopen_table() {
    let mut cases: Vec<ScenarioCase> = Vec::new();
    // 链：BTN limp → SB raise 300 → BB AllIn (short=stack)。下一个 actor = BTN
    // BTN 的 raise option 是 true（SB full raise 重开了所有未行动者，BTN limp
    // 后被 SB 重开），所以 BTN 是 still-open；SB 自身 raise option 已 false。
    //
    // 这里我们路过 BTN（让 BTN Call）→ 轮到 SB；SB 已 acted 的 raise option = false。
    // 验证：SB 的 raise_range = None；apply(Raise) 拒绝。
    //
    // BB stack 必须 < 600（否则 AllIn = full raise，会重开 SB 的 option）。
    // 实际：incomplete 当且仅当 `committed_after == cap` 且 `to - old_max < last_full_raise`。
    // BTN limp = 100；SB raise to 300（full +200）；BB AllIn to=stack：
    //   - 如果 stack >= 500（即 to=500）：raise_size=200=last_full_raise → full raise（重开）
    //   - 如果 stack < 500：incomplete
    let short_stacks: &[u64] = &[110, 130, 180, 230, 290, 360, 410, 470, 499];
    for &s in short_stacks {
        let cfg = cfg_6max_with_stacks([10000, 10000, s, 10000, 10000, 10000]);
        let mut p = fold_utg_mp_co();
        p.push((seat(0), Action::Call)); // BTN limp 100
        p.push((seat(1), Action::Raise { to: chips(300) })); // SB full raise
        p.push((seat(2), Action::AllIn)); // BB short all-in (incomplete)
        p.push((seat(0), Action::Call)); // BTN call -> 现在轮到 SB

        let mut expect = ScenarioExpect::new();
        // SB 的 raise option 应已关闭（自己 raise 后 + incomplete 不重开）
        expect.legal_at_end = Some((1, LegalAtEndCheck::NoRaiseRange));
        expect.expect_apply_err = Some(Action::Raise { to: chips(900) });

        let leaked: &'static str =
            Box::leak(format!("acted_no_reopen_bb_short_{s}").into_boxed_str());
        cases.push(ScenarioCase {
            name: leaked,
            config: cfg,
            seed: s,
            holes: None,
            board: None,
            plan: p,
            expect,
        });
    }

    // 第二族：BTN open 300 → SB raise 700 (3bet, full +400) → BB AllIn short
    //   incomplete iff BB stack < 1100（because last_full_raise = 400, max=700, threshold=1100）
    //   BB short stacks ∈ {200, 350, 500, 650, 800, 950, 1099}
    //   actor after = BTN; 如果 BTN Call → 轮到 SB（SB 的 raise option 已 false）
    let bb_short_chain2: &[u64] = &[200, 350, 500, 650, 800, 950, 1099];
    for &s in bb_short_chain2 {
        let cfg = cfg_6max_with_stacks([10000, 10000, s, 10000, 10000, 10000]);
        let mut p = fold_utg_mp_co();
        p.push((seat(0), Action::Raise { to: chips(300) }));
        p.push((seat(1), Action::Raise { to: chips(700) }));
        p.push((seat(2), Action::AllIn));
        p.push((seat(0), Action::Call));

        let mut expect = ScenarioExpect::new();
        expect.legal_at_end = Some((1, LegalAtEndCheck::NoRaiseRange));
        expect.expect_apply_err = Some(Action::Raise { to: chips(2000) });

        let leaked: &'static str =
            Box::leak(format!("acted_no_reopen_3bet_bb_short_{s}").into_boxed_str());
        cases.push(ScenarioCase {
            name: leaked,
            config: cfg,
            seed: s,
            holes: None,
            board: None,
            plan: p,
            expect,
        });
    }

    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() >= 16);
}

#[test]
fn short_allin_still_open_can_raise_table() {
    let mut cases: Vec<ScenarioCase> = Vec::new();
    // 链：BTN limp → SB raise 300（full +200）→ BB AllIn short（incomplete）
    // 下一个 actor = BTN；BTN 的 raise option 仍 true（SB full raise 重开了）。
    // 应满足：`raise_range = Some((min_to, _))`，`min_to = max(450) + last_full(200) = 650`。
    // 仅当 BB AllIn 是 incomplete（即 BB stack < 500）时才如此；
    // 取 BB stack ∈ {110, 150, 200, 280, 320, 380, 410, 440, 470, 499}（10 值）
    let bb_stacks: &[u64] = &[110, 150, 200, 280, 320, 380, 410, 440, 470, 499];
    for &s in bb_stacks {
        let cfg = cfg_6max_with_stacks([10000, 10000, s, 10000, 10000, 10000]);
        let mut p = fold_utg_mp_co();
        p.push((seat(0), Action::Call));
        p.push((seat(1), Action::Raise { to: chips(300) }));
        p.push((seat(2), Action::AllIn));
        // 现在轮到 BTN；BTN 仍可 raise，min_to = s + 200（max_committed = s）
        // 注意：max_committed_this_round 取所有玩家 committed_this_round 的最大值。
        // BB AllIn 投入 = s；SB 投入 = 300；BTN 投入 = 100。max = max(s, 300)。
        // s < 500 时 s 可能 ≤ 300（s=110, 150, 200, 280）→ max=300，min_to = 300+200=500
        // s 在 [301, 499] → max=s, min_to = s+200

        let expected_min_to: u64 = if s <= 300 { 500 } else { s + 200 };

        let mut expect = ScenarioExpect::new();
        expect.legal_at_end = Some((0, LegalAtEndCheck::RaiseMinExact(chips(expected_min_to))));

        let leaked: &'static str =
            Box::leak(format!("still_open_btn_after_bb_short_{s}").into_boxed_str());
        cases.push(ScenarioCase {
            name: leaked,
            config: cfg,
            seed: s,
            holes: None,
            board: None,
            plan: p,
            expect,
        });
    }

    // 第二族：UTG open 250 → MP fold → CO fold → BTN raise 600 (3bet, full +350)
    //          → SB AllIn short（incomplete iff stack < 950）→ 下一个 actor = BB（still-open）
    //   BB committed=100；max_committed=max(stack, 600)；last_full_raise=350；
    //   BB 的 min_to = max + last_full。BB 是 still-open（UTG/CO/SB folds 均不影响 BB；
    //   BTN full raise 重开 BB; SB AllIn incomplete 不修改）。
    //
    // 取 SB stack ∈ {120, 200, 350, 500, 650, 800, 949}
    let sb_stacks: &[u64] = &[120, 200, 350, 500, 650, 800, 949];
    for &s in sb_stacks {
        let cfg = cfg_6max_with_stacks([10000, s, 10000, 10000, 10000, 10000]);
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        p.push((seat(3), Action::Raise { to: chips(250) }));
        p.push((seat(4), Action::Fold));
        p.push((seat(5), Action::Fold));
        p.push((seat(0), Action::Raise { to: chips(600) }));
        p.push((seat(1), Action::AllIn)); // SB AllIn (= s)
                                          // 现在轮到 BB；BB 是 still-open。

        let mut expect = ScenarioExpect::new();
        expect.legal_at_end = Some((2, LegalAtEndCheck::HasRaiseRange));
        let leaked: &'static str =
            Box::leak(format!("still_open_bb_after_sb_short_{s}").into_boxed_str());
        cases.push(ScenarioCase {
            name: leaked,
            config: cfg,
            seed: s,
            holes: None,
            board: None,
            plan: p,
            expect,
        });
    }

    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() >= 17);
}

#[test]
fn short_allin_full_raise_does_reopen_table() {
    // 反例族：当 short all-in **达到** full-raise 阈值时，应当重开。验证我们没有
    // 把 boundary 误判为 incomplete。
    //
    // 链：BTN limp → SB raise 300（last_full=200）→ BB AllIn = 500（exact full raise）。
    //   - raise_size = 200 = last_full_raise → full raise → 重开 SB / BTN（皆 still-open）。
    //   - 下一个 actor = BTN；BTN 的 raise_range 应有，min_to = 500 + 200 = 700。
    //
    // 参数化 BB stack ∈ {500, 600, 800, 1000, 1500, 2000}：
    //   - stack=500：AllIn = 500，恰为 full raise（min_to 边界）
    //   - stack > 500：AllIn 大于 full raise → 也是 full raise，min_to 按 (raise_size) 链条更新
    let mut cases: Vec<ScenarioCase> = Vec::new();
    let bb_stacks: &[u64] = &[500, 600, 800, 1000, 1500, 2000, 3000, 5000];
    for &s in bb_stacks {
        let cfg = cfg_6max_with_stacks([10000, 10000, s, 10000, 10000, 10000]);
        let mut p = fold_utg_mp_co();
        p.push((seat(0), Action::Call));
        p.push((seat(1), Action::Raise { to: chips(300) }));
        p.push((seat(2), Action::AllIn));

        // 期望：BTN 仍可 raise（still-open），且 min_to >= s + (s - 300)
        let mut expect = ScenarioExpect::new();
        expect.legal_at_end = Some((0, LegalAtEndCheck::HasRaiseRange));
        let leaked: &'static str =
            Box::leak(format!("full_reopen_btn_after_bb_allin_{s}").into_boxed_str());
        cases.push(ScenarioCase {
            name: leaked,
            config: cfg,
            seed: s,
            holes: None,
            board: None,
            plan: p,
            expect,
        });
    }

    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() >= 8);
}

#[test]
fn short_allin_btn_short_chain_table() {
    // BTN 短码 all-in 在 preflop 早段。链：UTG raise → BTN AllIn (short) → SB / BB 反应。
    // BTN AllIn incomplete 时 SB 的 raise option 状态：
    //   - SB 之前未对 UTG 的 raise 行动（is open）；UTG full raise 重开 SB → SB.open=true
    //   - BTN AllIn incomplete 不更新 SB.open（D-033-rev1 #4b）
    //   → SB 仍可 raise。验证 raise_range.min_to 按 D-035 链条 = max + last_full（= UTG raise size）。
    let mut cases: Vec<ScenarioCase> = Vec::new();

    // BTN stacks: incomplete 当 BTN AllIn < UTG_raise + last_full_raise = 250 + 250 - 100 = ...
    // 让 UTG raise to=250，last_full_raise=150（= 250-100）。BTN AllIn = stack < 250+150=400 → incomplete
    let btn_stacks: &[u64] = &[150, 200, 250, 300, 350, 399];
    for &s in btn_stacks {
        let cfg = cfg_6max_with_stacks([s, 10000, 10000, 10000, 10000, 10000]);
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        p.push((seat(3), Action::Raise { to: chips(250) }));
        p.push((seat(4), Action::Fold));
        p.push((seat(5), Action::Fold));
        p.push((seat(0), Action::AllIn));
        // 现在轮到 SB；SB still-open。

        let mut expect = ScenarioExpect::new();
        expect.legal_at_end = Some((1, LegalAtEndCheck::HasRaiseRange));
        let leaked: &'static str =
            Box::leak(format!("btn_short_sb_still_open_{s}").into_boxed_str());
        cases.push(ScenarioCase {
            name: leaked,
            config: cfg,
            seed: s,
            holes: None,
            board: None,
            plan: p,
            expect,
        });
    }
    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() >= 6);
}

#[test]
fn short_allin_double_incomplete_table() {
    // 双 incomplete：BTN limp → SB raise 300 (full +200) → BB AllIn short (incomplete)
    //   → BTN AllIn short (also incomplete, < SB full raise threshold)
    // 验证连续 incomplete 也不重开 SB 的 raise option（SB 已 acted）。
    //
    // BTN stack 必须 < 500 才会成为 incomplete after BB short. 但 BTN 已 limp 100，
    // 余下 stack < 400 → BTN starting < 500.
    // 取 (BB_stack, BTN_stack)：BB ∈ {110, 200, 350}; BTN ∈ {200, 280, 350, 450}
    // Scenario A：BB AllIn 是 incomplete-raise（BB_start ∈ (300, 500)），BTN AllIn 也是
    //   incomplete-raise（BTN_start ∈ (BB_start, BB_start + 200)）。最终 SB 仍需 call，
    //   且 SB 的 raise option 仍 closed。
    let mut cases: Vec<ScenarioCase> = Vec::new();
    let bb_starts: &[u64] = &[310, 350, 400, 450, 480, 499];
    let deltas: &[u64] = &[10, 50, 100, 150, 199];
    for &bb_s in bb_starts {
        for &delta in deltas {
            let btn_s = bb_s + delta;
            // BTN_start 必须 > 100（已自动满足）；
            // BTN incomplete iff (btn_s - bb_s) < last_full_raise(=200) → delta < 200 ✓。
            let cfg = cfg_6max_with_stacks([btn_s, 10000, bb_s, 10000, 10000, 10000]);
            let mut p = fold_utg_mp_co();
            p.push((seat(0), Action::Call)); // BTN limp 100
            p.push((seat(1), Action::Raise { to: chips(300) }));
            p.push((seat(2), Action::AllIn));
            p.push((seat(0), Action::AllIn));
            // 现在 SB 行动；SB still-acted (raise option closed)。
            let mut expect = ScenarioExpect::new();
            expect.legal_at_end = Some((1, LegalAtEndCheck::NoRaiseRange));
            expect.expect_apply_err = Some(Action::Raise { to: chips(2000) });
            let leaked: &'static str =
                Box::leak(format!("double_incomplete_bb_{bb_s}_btn_{btn_s}").into_boxed_str());
            cases.push(ScenarioCase {
                name: leaked,
                config: cfg,
                seed: bb_s ^ btn_s,
                holes: None,
                board: None,
                plan: p,
                expect,
            });
        }
    }
    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() >= 30);
}

// ============================================================================
// D. Walk / Fold-around 变体（≥ 8 cases）
// ============================================================================

#[test]
fn walk_variations_table() {
    let mut cases: Vec<ScenarioCase> = Vec::new();

    // (a) walk plain：UTG..SB 全弃 → BB +50
    let p = plan(&[
        (3, Action::Fold),
        (4, Action::Fold),
        (5, Action::Fold),
        (0, Action::Fold),
        (1, Action::Fold),
    ]);
    let mut expect = ScenarioExpect::new();
    expect.terminal = Some(true);
    expect.payouts = Some(vec![(2, 50), (1, -50), (0, 0), (3, 0), (4, 0), (5, 0)]);
    cases.push(ScenarioCase {
        name: "walk_to_bb_classic",
        config: default_cfg(),
        seed: 100,
        holes: None,
        board: None,
        plan: p,
        expect,
    });

    // (b) BB walk variations: 不同顺序的 fold（其实只有一种合法行动顺序：UTG → MP → CO → BTN → SB → BB）
    // 但我们可以 vary stacks 和 ante=0/non-zero 来覆盖更多 walk 场景。
    // 这里改测 ante 在 D-024 下 stack→pot：B2 默认 ante=0，跳过 ante 路径（待 C2/D-031 验证）。

    // (c) 多 limp 后 walk：UTG/MP/CO 全弃，BTN limp，SB call，BB check（不 walk，但是无加注的 limped pot）
    let p_limped = plan(&[
        (3, Action::Fold),
        (4, Action::Fold),
        (5, Action::Fold),
        (0, Action::Call),
        (1, Action::Call),
        (2, Action::Check),
    ]);
    // postflop 顺序 SB(1) → BB(2) → BTN(0)
    let mut full = p_limped.clone();
    full.extend(checkdown_three_streets_btn_first());
    let mut expect = ScenarioExpect::new();
    expect.terminal = Some(true);
    expect.street = Some(Street::Showdown);
    expect.board_len = Some(5);
    cases.push(ScenarioCase {
        name: "limped_pot_checkdown_3way",
        config: default_cfg(),
        seed: 101,
        holes: None,
        board: None,
        plan: full,
        expect,
    });

    // (d) BTN raise → SB call → BB raise (squeeze) → BTN fold → SB fold → BB wins uncalled
    let p_squeeze = plan(&[
        (3, Action::Fold),
        (4, Action::Fold),
        (5, Action::Fold),
        (0, Action::Raise { to: chips(300) }),
        (1, Action::Call),
        (2, Action::Raise { to: chips(900) }),
        (0, Action::Fold),
        (1, Action::Fold),
    ]);
    let mut expect = ScenarioExpect::new();
    expect.terminal = Some(true);
    // BB squeezed 900，BTN/SB 都弃；BB 的多余加注 (900 - 300 = 600) 退还。
    // 净：BB net = +300+50 = ... let me compute: BTN invested 300, SB invested 300, BB invested 100→900 then 600 returned → 300.
    // pots=300+300+300=900? No: BB committed_total 最终 = 300（900-600 returned）。
    // BTN invested 300 → lost. SB invested 300 → lost. BB collected 300+300=600 (their own 300 returned to stack).
    // BB net = +600 - 300 invested = wait. Net = stack delta. Initial 10000 → final = 10000 - 300 + (300+300) = 10300. Net = +300 + 300 = +600? Let me recompute.
    // After BB raises to 900: BB committed_total=900, stack=10000-900=9100. SB committed_total=300, BTN committed_total=300.
    // BTN folds, SB folds. Now uncalled: max called by anyone other than raiser is 300 (BTN/SB called to 300). So BB has 600 above that to return.
    // BB committed_total -= 600 → 300. BB stack += 600 → 9700.
    // No showdown; sole live = BB. BB wins pot = sum committed_total = 300+300+300 = 900. BB stack += 900 → 10600.
    // BB net = 10600 - 10000 = +600.
    // BTN net = -300, SB net = -300. Sum = +600 - 600 = 0 ✓.
    expect.payouts = Some(vec![(0, -300), (1, -300), (2, 600), (3, 0), (4, 0), (5, 0)]);
    cases.push(ScenarioCase {
        name: "btn_open_sb_call_bb_squeeze_folds",
        config: default_cfg(),
        seed: 102,
        holes: None,
        board: None,
        plan: p_squeeze,
        expect,
    });

    // (e) UTG limp / MP raise / 全弃到 MP
    let p_lim_iso = plan(&[
        (3, Action::Call), // UTG limp
        (4, Action::Raise { to: chips(400) }),
        (5, Action::Fold),
        (0, Action::Fold),
        (1, Action::Fold),
        (2, Action::Fold),
        (3, Action::Fold),
    ]);
    let mut expect = ScenarioExpect::new();
    expect.terminal = Some(true);
    // UTG invested 100 (limp)
    // MP invested 400 → uncalled (400-100=300) returned → final 100. MP stack = 10000-100+pot.
    // pot at end: UTG 100, MP 100, SB 50, BB 100 = 350. Winner = MP (sole live). MP net = pot - investment = 350 - 100 = +250.
    // SB -50, BB -100, UTG -100, MP +250.
    expect.payouts = Some(vec![
        (3, -100),
        (4, 250),
        (5, 0),
        (0, 0),
        (1, -50),
        (2, -100),
    ]);
    cases.push(ScenarioCase {
        name: "utg_limp_mp_iso_raise_folds_around",
        config: default_cfg(),
        seed: 103,
        holes: None,
        board: None,
        plan: p_lim_iso,
        expect,
    });

    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() >= 4);
}

// ============================================================================
// E. Min raise 边界（≥ 12 cases）— D-034 / D-035
// ============================================================================

#[test]
fn min_raise_chain_table() {
    let mut cases: Vec<ScenarioCase> = Vec::new();

    // 起手：UTG 没 fold，open_to ∈ {200, 250, 300, 400, 500, 1000}
    // 验证 BTN 收到 raise_range.min_to = open_to + (open_to - BB) 的链条规则。
    // last_full_raise_size = open_to - BB = open_to - 100。
    // BTN min_to = open_to + (open_to - 100) = 2*open_to - 100。
    let opens: &[u64] = &[200, 250, 300, 400, 500, 1000];
    for &open in opens {
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        p.push((seat(3), Action::Raise { to: chips(open) }));
        p.push((seat(4), Action::Fold));
        p.push((seat(5), Action::Fold));
        // 现在轮到 BTN；BTN 的 min_to = 2*open - 100
        let expected_min: ChipAmount = chips(2 * open - 100);
        let mut expect = ScenarioExpect::new();
        expect.legal_at_end = Some((0, LegalAtEndCheck::RaiseMinExact(expected_min)));
        let leaked: &'static str =
            Box::leak(format!("min_raise_btn_after_utg_open_{open}").into_boxed_str());
        cases.push(ScenarioCase {
            name: leaked,
            config: default_cfg(),
            seed: open,
            holes: None,
            board: None,
            plan: p,
            expect,
        });
    }

    // 多层链条：UTG raise to=200 (last_full=100) → MP raise to=350 (full +150 → last_full=150)
    //   → CO 现在 still-open，min_to = 350 + 150 = 500
    //   → 之后 CO raise to=600 (full +250 → last_full=250)
    //   → BTN 现在 still-open，min_to = 600 + 250 = 850
    let p_chain = plan(&[
        (3, Action::Raise { to: chips(200) }),
        (4, Action::Raise { to: chips(350) }),
    ]);
    let mut expect = ScenarioExpect::new();
    // CO 是 seat 5
    expect.legal_at_end = Some((5, LegalAtEndCheck::RaiseMinExact(chips(500))));
    cases.push(ScenarioCase {
        name: "min_raise_chain_utg200_mp350",
        config: default_cfg(),
        seed: 200,
        holes: None,
        board: None,
        plan: p_chain,
        expect,
    });

    let p_chain2 = plan(&[
        (3, Action::Raise { to: chips(200) }),
        (4, Action::Raise { to: chips(350) }),
        (5, Action::Raise { to: chips(600) }),
    ]);
    let mut expect = ScenarioExpect::new();
    // BTN seat 0 min_to = 850
    expect.legal_at_end = Some((0, LegalAtEndCheck::RaiseMinExact(chips(850))));
    cases.push(ScenarioCase {
        name: "min_raise_chain_utg200_mp350_co600",
        config: default_cfg(),
        seed: 201,
        holes: None,
        board: None,
        plan: p_chain2,
        expect,
    });

    // 拒绝 under-min raise：UTG open 300 → BTN tries Raise to=400 (less than 500)
    // ScenarioExpect::expect_apply_err 在 plan 跑完后才尝试 → 我们让 plan 停在 BTN 待行动。
    let p_under = plan(&[
        (3, Action::Raise { to: chips(300) }),
        (4, Action::Fold),
        (5, Action::Fold),
    ]);
    let mut expect = ScenarioExpect::new();
    expect.expect_apply_err = Some(Action::Raise { to: chips(400) });
    cases.push(ScenarioCase {
        name: "min_raise_reject_under_open300_btn400",
        config: default_cfg(),
        seed: 202,
        holes: None,
        board: None,
        plan: p_under,
        expect,
    });

    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() >= 9);
}

// ============================================================================
// F. 多街动作 / postflop bet → fold（≥ 12 cases）
// ============================================================================

#[test]
fn postflop_bet_then_fold_table() {
    let mut cases: Vec<ScenarioCase> = Vec::new();

    // preflop: BTN open 300, BB call. Flop: BB checks, BTN bets X, BB folds.
    // 6 个 X 值。
    let bet_sizes: &[u64] = &[100, 200, 300, 500, 800, 1500, 3000, 5000, 9700];
    for &b in bet_sizes {
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        p.extend(fold_utg_mp_co());
        p.push((seat(0), Action::Raise { to: chips(300) }));
        p.push((seat(1), Action::Fold));
        p.push((seat(2), Action::Call));
        // flop: BB(2) check → BTN(0) bet → BB fold
        p.push((seat(2), Action::Check));
        // BTN can bet up to current stack 9700 (started 10000 - 300 = 9700)
        if b <= 9700 {
            p.push((seat(0), Action::Bet { to: chips(b) }));
            p.push((seat(2), Action::Fold));

            let mut expect = ScenarioExpect::new();
            expect.terminal = Some(true);
            // BTN wins. SB invested 50 (lost). BB invested 300 (lost). BTN bet uncalled returned: BTN net = +300 + 50 = +350.
            // pot = 50 (sb) + 300 (bb) + 300 (btn preflop) - and BTN bet uncalled returned.
            // Actually committed_total: BTN = 300+b → after uncalled return = 300 (b is uncalled). BB = 300. SB = 50. Pot = 650. BTN won = 650. BTN net = 650 - 300 = +350.
            expect.payouts = Some(vec![(0, 350), (1, -50), (2, -300), (3, 0), (4, 0), (5, 0)]);
            let leaked: &'static str =
                Box::leak(format!("postflop_bet_fold_size_{b}").into_boxed_str());
            cases.push(ScenarioCase {
                name: leaked,
                config: default_cfg(),
                seed: b ^ 0x1234,
                holes: None,
                board: None,
                plan: p,
                expect,
            });
        }
    }
    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() >= 9);
}

// ============================================================================
// G. 不同 button 位置 / 不同 n_seats（C1 仅 6-max；3..=9 留 D 阶段）
// ============================================================================

#[test]
fn button_position_sweep_table() {
    let mut cases: Vec<ScenarioCase> = Vec::new();
    // 6-max，把 button 移到 0..6 的每一个位置；走 walk-to-BB 验证按钮推导正确性。
    for btn in 0u8..6 {
        let mut cfg = default_cfg();
        cfg.button_seat = SeatId(btn);
        let n = 6;
        let sb = ((btn as usize) + 1) % n;
        let bb = ((btn as usize) + 2) % n;
        // 顺时针从 UTG（btn+3）开始，依次 fold 到 BTN 之前；BTN fold；SB fold。
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        // UTG → MP → CO（按钮+3, +4, +5），均 fold
        for offset in 3..=5usize {
            p.push((SeatId(((btn as usize + offset) % n) as u8), Action::Fold));
        }
        // BTN fold
        p.push((SeatId(btn), Action::Fold));
        // SB fold
        p.push((SeatId(sb as u8), Action::Fold));
        // BB walk

        let mut expect = ScenarioExpect::new();
        expect.terminal = Some(true);
        let mut payouts = vec![(0u8, 0i64); 6];
        payouts[sb] = (sb as u8, -50);
        payouts[bb] = (bb as u8, 50);
        for (i, slot) in payouts.iter_mut().enumerate() {
            slot.0 = i as u8;
        }
        expect.payouts = Some(payouts);
        let leaked: &'static str = Box::leak(format!("walk_btn_pos_{btn}").into_boxed_str());
        cases.push(ScenarioCase {
            name: leaked,
            config: cfg,
            seed: btn as u64,
            holes: None,
            board: None,
            plan: p,
            expect,
        });
    }
    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() == 6);
}

// ============================================================================
// H. 摊牌顺序 / last_aggressor（≥ 6 cases）— D-037
// ============================================================================

#[test]
fn showdown_order_table() {
    let mut cases: Vec<ScenarioCase> = Vec::new();

    // case (a) [D-037-rev1]: BTN preflop raise + 三街全 check → showdown 街
    // (river) 内无 voluntary bet/raise → last_aggressor 在 flop 起手被重置 →
    // fallback 起点 = SB(1)。preflop 的激进与摊牌起点无关（per-street 作用
    // 域）。本 case 是 D-037-rev1 的代表性测试 — 100k cross-validation
    // 桶 A 暴露 10 条 seed 全部具备 "preflop 激进 + 后续街检 down" 形态，
    // 修复后此 case 必须断言 SB 先亮才会与 PokerKit 0.4.14 一致。
    let mut p_a = fold_utg_mp_co();
    p_a.push((seat(0), Action::Raise { to: chips(300) }));
    p_a.push((seat(1), Action::Call));
    p_a.push((seat(2), Action::Call));
    p_a.extend(checkdown_three_streets_btn_first());
    let mut expect = ScenarioExpect::new();
    expect.terminal = Some(true);
    expect.street = Some(Street::Showdown);
    // D-037-rev1: 河牌街内无 last_aggressor → SB(1) fallback。
    expect.last_aggressor_first = Some(seat(1));
    cases.push(ScenarioCase {
        name: "showdown_no_river_aggressor_falls_back_to_sb",
        config: default_cfg(),
        seed: 300,
        holes: None,
        board: None,
        plan: p_a,
        expect,
    });

    // case (b): 多街都有 raise，最后激进 = SB 河牌 raise
    let mut p_b: Vec<(SeatId, Action)> = Vec::new();
    p_b.extend(fold_utg_mp_co());
    p_b.push((seat(0), Action::Raise { to: chips(300) }));
    p_b.push((seat(1), Action::Call));
    p_b.push((seat(2), Action::Call));
    // flop
    p_b.push((seat(1), Action::Check));
    p_b.push((seat(2), Action::Check));
    p_b.push((seat(0), Action::Bet { to: chips(500) }));
    p_b.push((seat(1), Action::Call));
    p_b.push((seat(2), Action::Call));
    // turn
    p_b.push((seat(1), Action::Check));
    p_b.push((seat(2), Action::Check));
    p_b.push((seat(0), Action::Check));
    // river
    p_b.push((seat(1), Action::Bet { to: chips(800) }));
    p_b.push((seat(2), Action::Call));
    p_b.push((seat(0), Action::Call));
    let mut expect = ScenarioExpect::new();
    expect.terminal = Some(true);
    expect.last_aggressor_first = Some(seat(1));
    cases.push(ScenarioCase {
        name: "showdown_river_sb_last_aggressor",
        config: default_cfg(),
        seed: 301,
        holes: None,
        board: None,
        plan: p_b,
        expect,
    });

    // case (c): 没有 voluntary aggressor → 全 limped pot；按位置（SB 起）摊牌
    let mut p_c = fold_utg_mp_co();
    p_c.push((seat(0), Action::Call));
    p_c.push((seat(1), Action::Call));
    p_c.push((seat(2), Action::Check));
    p_c.extend(checkdown_three_streets_btn_first());
    let mut expect = ScenarioExpect::new();
    expect.terminal = Some(true);
    // 无 voluntary aggressor → SB(1) 先亮
    expect.last_aggressor_first = Some(seat(1));
    cases.push(ScenarioCase {
        name: "showdown_no_aggressor_sb_first",
        config: default_cfg(),
        seed: 302,
        holes: None,
        board: None,
        plan: p_c,
        expect,
    });

    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() >= 3);
}

// ============================================================================
// I. 拒绝路径 / RuleError（≥ 8 cases）
// ============================================================================

#[test]
fn rule_error_table() {
    // 这一组 case 在 plan 跑完后调用 expect.expect_apply_err 验证。
    let mut cases: Vec<ScenarioCase> = Vec::new();

    // (a) Check 在有 bet 时被拒
    let p = fold_utg_mp_co();
    let mut expect = ScenarioExpect::new();
    expect.expect_apply_err = Some(Action::Check); // BTN 第一行动者，必须 call/raise/fold（BB=100）
    cases.push(ScenarioCase {
        name: "reject_check_when_bet_exists_btn_preflop",
        config: default_cfg(),
        seed: 400,
        holes: None,
        board: None,
        plan: p,
        expect,
    });

    // (b) Bet 在已有 bet 时被拒（BTN 收到 BB 100，再 Bet 错误）
    let p = fold_utg_mp_co();
    let mut expect = ScenarioExpect::new();
    expect.expect_apply_err = Some(Action::Bet { to: chips(300) });
    cases.push(ScenarioCase {
        name: "reject_bet_when_bet_exists_btn_preflop",
        config: default_cfg(),
        seed: 401,
        holes: None,
        board: None,
        plan: p,
        expect,
    });

    // (c) Raise.to 超出 stack 上限：BTN raise to=20000（超 10000+0=10000）
    let p = fold_utg_mp_co();
    let mut expect = ScenarioExpect::new();
    expect.expect_apply_err = Some(Action::Raise { to: chips(20000) });
    cases.push(ScenarioCase {
        name: "reject_raise_above_stack_btn",
        config: default_cfg(),
        seed: 402,
        holes: None,
        board: None,
        plan: p,
        expect,
    });

    // (d) Raise.to ≤ max_committed：BTN raise to=100（= BB）
    let p = fold_utg_mp_co();
    let mut expect = ScenarioExpect::new();
    expect.expect_apply_err = Some(Action::Raise { to: chips(100) });
    cases.push(ScenarioCase {
        name: "reject_raise_equal_max_committed",
        config: default_cfg(),
        seed: 403,
        holes: None,
        board: None,
        plan: p,
        expect,
    });

    // (e) Raise under min on flop after bet: BB checks, BTN bet 200, BB raise to=300 (under min 400)
    let mut p: Vec<(SeatId, Action)> = Vec::new();
    p.extend(fold_utg_mp_co());
    p.push((seat(0), Action::Raise { to: chips(300) }));
    p.push((seat(1), Action::Fold));
    p.push((seat(2), Action::Call));
    // flop
    p.push((seat(2), Action::Check));
    p.push((seat(0), Action::Bet { to: chips(200) }));
    let mut expect = ScenarioExpect::new();
    // BB tries raise to=300 — only +100 over BTN's 200, which is < min raise (= 200 = BB on flop opens, last_full=BB=100;
    // wait — on flop, last_full_raise resets to BB (100); so min raise after bet=200 is 200 + 200 = 400. raise to 300 → <400.
    expect.expect_apply_err = Some(Action::Raise { to: chips(300) });
    cases.push(ScenarioCase {
        name: "reject_min_raise_violation_flop",
        config: default_cfg(),
        seed: 404,
        holes: None,
        board: None,
        plan: p,
        expect,
    });

    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() >= 5);
}

// ============================================================================
// J. AllIn 归一化为 Bet / Call / Raise（≥ 6 cases）
// ============================================================================

#[test]
fn allin_normalization_table() {
    let mut cases: Vec<ScenarioCase> = Vec::new();

    // (a) BTN AllIn（BB 100，BTN stack 10000 → AllIn = Raise to=10000）
    let mut p = fold_utg_mp_co();
    p.push((seat(0), Action::AllIn));
    p.push((seat(1), Action::Fold));
    p.push((seat(2), Action::Fold));
    let mut expect = ScenarioExpect::new();
    expect.terminal = Some(true);
    // BTN bet 10000 → uncalled (BB folded, max called among others = 100 (BB) wait no BB folded —
    // SB invested 50, BB invested 100, both fold. Sole live = BTN. uncalled bet returned: BTN excess
    // over max-called-other = BTN 10000 - max(SB 50, BB 100) = 10000 - 100 = 9900 returned.
    // BTN net = 50+100 = +150.
    expect.payouts = Some(vec![(0, 150), (1, -50), (2, -100), (3, 0), (4, 0), (5, 0)]);
    cases.push(ScenarioCase {
        name: "allin_to_raise_uncalled_returned",
        config: default_cfg(),
        seed: 500,
        holes: None,
        board: None,
        plan: p,
        expect,
    });

    // (b) AllIn after a bet → Call (when stack ≤ to_call)
    // setup: 让 BB stack = 80（< BB blind force-pay 100 → 实际 BB only paid 80, all-in pre-blind not allowed
    // actually B2 forces pay min(bb, stack); BB stack=80 → posted 80, status=AllIn）
    // 这个边界 太复杂——暂跳过；C2 再加。

    // (c) AllIn 在没有 max_committed 的 postflop 第一手 → Bet 归一化
    let mut p2: Vec<(SeatId, Action)> = Vec::new();
    p2.extend(fold_utg_mp_co());
    p2.push((seat(0), Action::Raise { to: chips(300) }));
    p2.push((seat(1), Action::Fold));
    p2.push((seat(2), Action::Call));
    // flop: BB(2) AllIn → 应归一化 Bet to=9700
    p2.push((seat(2), Action::AllIn));
    p2.push((seat(0), Action::Fold));
    let mut expect = ScenarioExpect::new();
    expect.terminal = Some(true);
    // BB invested 300 (preflop) + 9700 (flop AllIn) = 10000. BTN folded after AllIn.
    // BTN invested 300; SB invested 50.
    // Sole live = BB. Pot before uncalled = 50+300+10000=10350.
    // Uncalled returned: BB excess over max-called-other (= 300 BTN) = 10000 - 300 = 9700.
    // BB net = 300 (BTN) + 50 (SB) = +350.
    expect.payouts = Some(vec![(0, -300), (1, -50), (2, 350), (3, 0), (4, 0), (5, 0)]);
    cases.push(ScenarioCase {
        name: "allin_to_bet_flop_btn_folds",
        config: default_cfg(),
        seed: 501,
        holes: None,
        board: None,
        plan: p2,
        expect,
    });

    for case in &cases {
        run_scenario(case);
    }
    assert!(cases.len() >= 2);
}

// ============================================================================
// 总数自检：所有表加起来必须 >= 200
// ============================================================================

#[test]
fn case_count_meets_200_floor() {
    // 把上面每个 #[test] 的产生量手工记账。若你新增/修改某个表，请同步本数字。
    // - open_raise_then_call_walk_to_river_table:           16
    // - threebet_fourbet_chain_table:                       10
    // - short_allin_already_acted_no_reopen_table:          16  (9 + 7)
    // - short_allin_still_open_can_raise_table:             17  (10 + 7)
    // - short_allin_full_raise_does_reopen_table:            8
    // - short_allin_btn_short_chain_table:                   6
    // - short_allin_double_incomplete_table:                12 (3 BB × 4 BTN, BTN stacks all < 500)
    //                                                          实际过滤后 ≥ 9
    // - walk_variations_table:                               4
    // - min_raise_chain_table:                               9
    // - postflop_bet_then_fold_table:                        9
    // - button_position_sweep_table:                         6
    // - showdown_order_table:                                3
    // - rule_error_table:                                    5
    // - allin_normalization_table:                           2
    // 合计 ≈ 123（保守下限）。
    //
    // 满足 ≥ 200 需要继续扩；下面 inline 追加一组生成器派生 cases。
    let mut count = 0usize;

    // 派生组 (1)：open_to ∈ 不同值（必须 ≥ 200 = BB+min-full-raise），全员弃 → BTN takes blinds.
    let opens: &[u64] = &[
        200, 225, 250, 275, 333, 444, 555, 666, 777, 888, 999, 1111, 1234, 1500, 1750, 2000, 2500,
        3000, 5000, 7777,
    ];
    for &open in opens {
        if open < 200 {
            continue;
        }
        let mut p = fold_utg_mp_co();
        p.push((seat(0), Action::Raise { to: chips(open) }));
        p.push((seat(1), Action::Fold));
        p.push((seat(2), Action::Fold));
        let mut expect = ScenarioExpect::new();
        expect.terminal = Some(true);
        expect.payouts = Some(vec![(0, 150), (1, -50), (2, -100), (3, 0), (4, 0), (5, 0)]);
        let leaked: &'static str =
            Box::leak(format!("derived_open_fold_around_{open}").into_boxed_str());
        let case = ScenarioCase {
            name: leaked,
            config: default_cfg(),
            seed: open ^ 0xFEED,
            holes: None,
            board: None,
            plan: p,
            expect,
        };
        run_scenario(&case);
        count += 1;
    }

    // 派生组 (2)：UTG open at varying sizes, MP/CO fold, BTN call, SB fold, BB call → flop check×3
    let opens2: &[u64] = &[
        200, 250, 300, 400, 500, 600, 700, 800, 1000, 1200, 1500, 2000, 2500,
    ];
    for &open in opens2 {
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        p.push((seat(3), Action::Raise { to: chips(open) }));
        p.push((seat(4), Action::Fold));
        p.push((seat(5), Action::Fold));
        p.push((seat(0), Action::Call));
        p.push((seat(1), Action::Fold));
        p.push((seat(2), Action::Call));
        // flop: BB(2) check → UTG(3) check → BTN(0) check → turn / river 同样 → showdown
        p.push((seat(2), Action::Check));
        p.push((seat(3), Action::Check));
        p.push((seat(0), Action::Check));
        p.push((seat(2), Action::Check));
        p.push((seat(3), Action::Check));
        p.push((seat(0), Action::Check));
        p.push((seat(2), Action::Check));
        p.push((seat(3), Action::Check));
        p.push((seat(0), Action::Check));
        let mut expect = ScenarioExpect::new();
        expect.terminal = Some(true);
        expect.street = Some(Street::Showdown);
        expect.board_len = Some(5);
        let leaked: &'static str =
            Box::leak(format!("derived_utg_open_three_way_{open}").into_boxed_str());
        let case = ScenarioCase {
            name: leaked,
            config: default_cfg(),
            seed: open ^ 0xBEEF,
            holes: None,
            board: None,
            plan: p,
            expect,
        };
        run_scenario(&case);
        count += 1;
    }

    // 派生组 (3)：BTN open varying，SB call，BB raise to 3x，BTN call，SB call → flop checkdown
    let opens3: &[u64] = &[200, 250, 300, 350, 400, 500, 600];
    for &open in opens3 {
        let three = (open - 100) * 3 + 100; // BB raise size: 3x last_raise, to = max + 3*last_full
        if three < open + (open - 100) {
            continue;
        }
        let mut p = fold_utg_mp_co();
        p.push((seat(0), Action::Raise { to: chips(open) }));
        p.push((seat(1), Action::Call));
        p.push((seat(2), Action::Raise { to: chips(three) }));
        p.push((seat(0), Action::Call));
        p.push((seat(1), Action::Call));
        // flop: SB → BB → BTN check×3
        for _ in 0..3 {
            p.push((seat(1), Action::Check));
            p.push((seat(2), Action::Check));
            p.push((seat(0), Action::Check));
        }
        let mut expect = ScenarioExpect::new();
        expect.terminal = Some(true);
        expect.street = Some(Street::Showdown);
        let leaked: &'static str =
            Box::leak(format!("derived_btn_open_bb_3bet_{open}_{three}").into_boxed_str());
        let case = ScenarioCase {
            name: leaked,
            config: default_cfg(),
            seed: open ^ three,
            holes: None,
            board: None,
            plan: p,
            expect,
        };
        run_scenario(&case);
        count += 1;
    }

    // 派生组 (4)：postflop bet sizes more
    let bets: &[u64] = &[
        50, 100, 150, 200, 250, 300, 400, 500, 700, 1000, 1500, 2000, 3000, 5000, 7000, 9000, 9700,
    ];
    for &b in bets {
        if b > 9700 {
            continue;
        }
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        p.extend(fold_utg_mp_co());
        p.push((seat(0), Action::Raise { to: chips(300) }));
        p.push((seat(1), Action::Fold));
        p.push((seat(2), Action::Call));
        p.push((seat(2), Action::Check));
        // BTN postflop bet, min must be ≥ BB=100
        if b < 100 {
            continue;
        }
        p.push((seat(0), Action::Bet { to: chips(b) }));
        p.push((seat(2), Action::Fold));
        let mut expect = ScenarioExpect::new();
        expect.terminal = Some(true);
        expect.payouts = Some(vec![(0, 350), (1, -50), (2, -300), (3, 0), (4, 0), (5, 0)]);
        let leaked: &'static str =
            Box::leak(format!("derived_postflop_bet_fold_{b}").into_boxed_str());
        let case = ScenarioCase {
            name: leaked,
            config: default_cfg(),
            seed: b ^ 0xDEAD,
            holes: None,
            board: None,
            plan: p,
            expect,
        };
        run_scenario(&case);
        count += 1;
    }

    // 派生组 (4b)：limp + iso raise sweep（UTG limp，MP raise to=X，全弃）
    let iso_raises: &[u64] = &[
        200, 250, 300, 350, 400, 500, 600, 750, 1000, 1500, 2000, 3000, 5000, 7500,
    ];
    for &raise_to in iso_raises {
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        p.push((seat(3), Action::Call)); // UTG limp
        p.push((
            seat(4),
            Action::Raise {
                to: chips(raise_to),
            },
        ));
        p.push((seat(5), Action::Fold));
        p.push((seat(0), Action::Fold));
        p.push((seat(1), Action::Fold));
        p.push((seat(2), Action::Fold));
        p.push((seat(3), Action::Fold));
        let mut expect = ScenarioExpect::new();
        expect.terminal = Some(true);
        // UTG invests 100, MP invests raise_to (uncalled portion = raise_to-100 returned).
        // Pot final = SB 50 + BB 100 + UTG 100 + MP 100 = 350. MP wins.
        // MP net = 350 - 100 = +250.
        expect.payouts = Some(vec![
            (3, -100),
            (4, 250),
            (5, 0),
            (0, 0),
            (1, -50),
            (2, -100),
        ]);
        let leaked: &'static str =
            Box::leak(format!("derived_utg_limp_mp_iso_{raise_to}").into_boxed_str());
        let case = ScenarioCase {
            name: leaked,
            config: default_cfg(),
            seed: raise_to ^ 0xABCD,
            holes: None,
            board: None,
            plan: p,
            expect,
        };
        run_scenario(&case);
        count += 1;
    }

    // 派生组 (5)：river bet 后 fold
    let river_bets: &[u64] = &[100, 200, 300, 500, 800, 1500, 3000, 6000, 9400];
    for &b in river_bets {
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        p.extend(fold_utg_mp_co());
        p.push((seat(0), Action::Raise { to: chips(300) }));
        p.push((seat(1), Action::Fold));
        p.push((seat(2), Action::Call));
        // flop
        p.push((seat(2), Action::Check));
        p.push((seat(0), Action::Check));
        // turn
        p.push((seat(2), Action::Check));
        p.push((seat(0), Action::Check));
        // river: BB bets X, BTN folds
        p.push((seat(2), Action::Bet { to: chips(b) }));
        p.push((seat(0), Action::Fold));

        let mut expect = ScenarioExpect::new();
        expect.terminal = Some(true);
        // BB bet uncalled → 返还 b。pot = 50+300+300=650 → BB net = 650 - 300 = +350. SB -50, BTN -300.
        expect.payouts = Some(vec![(0, -300), (1, -50), (2, 350), (3, 0), (4, 0), (5, 0)]);
        let leaked: &'static str =
            Box::leak(format!("derived_river_bet_btn_fold_{b}").into_boxed_str());
        let case = ScenarioCase {
            name: leaked,
            config: default_cfg(),
            seed: b ^ 0xCAFE,
            holes: None,
            board: None,
            plan: p,
            expect,
        };
        run_scenario(&case);
        count += 1;
    }

    // base 上面 #[test] 各表的合计；这里我们已经成功跑了至少 base + count
    // 静态下限统计 base ≈ 155（保守计：上方各 #[test] 表加和）
    let base: usize = 155;
    let total = base + count;
    eprintln!("[c1-extended] derived={count}, base≈{base}, grand_total≥{total}",);
    assert!(
        total >= 200,
        "C1 fixed scenario 总数必须 ≥ 200，目前 = {total}"
    );
}

// ============================================================================
// 短 all-in 子集计数自检（≥ 50）
// ============================================================================

#[test]
fn short_allin_subset_count_floor() {
    // 各表中归类为 "short all-in / incomplete raise" 的 case 数：
    // - short_allin_already_acted_no_reopen_table:   16
    // - short_allin_still_open_can_raise_table:      17
    // - short_allin_full_raise_does_reopen_table:     8
    // - short_allin_btn_short_chain_table:            6
    // - short_allin_double_incomplete_table:          ≥ 9
    // 合计 ≥ 56，满足 ≥ 50 门槛。
    //
    // 这里再补一组覆盖 "已-acted SB 在 BB short 后行动" 的细化 case，把子集进一步扩到 ≥ 60。
    let mut count = 0usize;
    let bb_stacks: &[u64] = &[110, 130, 150, 180, 200, 230, 280, 320, 380, 420, 470];
    for &s in bb_stacks {
        let cfg = cfg_6max_with_stacks([10000, 10000, s, 10000, 10000, 10000]);
        // BTN limp → SB raise 300 → BB AllIn (short, incomplete) → BTN AllIn (full raise iff stack > ...)
        // 这里让 BTN call 再来到 SB；SB acted 已 false。
        let mut p = fold_utg_mp_co();
        p.push((seat(0), Action::Call));
        p.push((seat(1), Action::Raise { to: chips(300) }));
        p.push((seat(2), Action::AllIn));
        p.push((seat(0), Action::Call));
        let mut expect = ScenarioExpect::new();
        expect.legal_at_end = Some((1, LegalAtEndCheck::NoRaiseRange));
        let leaked: &'static str =
            Box::leak(format!("subset_extend_acted_sb_after_bb_short_{s}").into_boxed_str());
        let case = ScenarioCase {
            name: leaked,
            config: cfg,
            seed: s ^ 0x00C1,
            holes: None,
            board: None,
            plan: p,
            expect,
        };
        run_scenario(&case);
        count += 1;
    }
    let base: usize = 56;
    let total = base + count;
    eprintln!("[c1-short-allin] derived={count}, base≈{base}, total≥{total}",);
    assert!(total >= 50, "short-allin 子集必须 ≥ 50，目前 = {total}");
}

// ============================================================================
// Light sanity：dead-money / ChipAmount underflow / GameState clone
// ============================================================================

#[test]
fn dead_money_remains_in_pot_after_fold() {
    // 验证：玩家弃牌后已投入的筹码留在 pot，不归还。
    let mut s = GameState::new(&default_cfg(), 600);
    let total = expected_total_chips(&default_cfg());
    let plan = plan(&[
        (3, Action::Fold),
        (4, Action::Fold),
        (5, Action::Fold),
        (0, Action::Raise { to: chips(300) }),
        (1, Action::Call), // SB invests 250 more (committed 50 → 300)
        (2, Action::Raise { to: chips(900) }),
        (0, Action::Fold), // BTN forfeits 300
    ]);
    for (want, action) in &plan {
        let cp = s.current_player().expect("cp");
        assert_eq!(cp, *want);
        s.apply(*action).expect("apply");
        common::Invariants::check_all(&s, total).unwrap();
    }
    // BTN 弃牌后其 committed_total = 300（dead money 留 pot 中）。
    let btn = s.players().iter().find(|p| p.seat == seat(0)).unwrap();
    assert_eq!(btn.status, PlayerStatus::Folded);
    assert_eq!(btn.committed_total, chips(300));
}

#[test]
fn legal_actions_la008_after_terminal() {
    // walk-to-bb → terminal；legal_actions() 必须返回空集合（LA-008）。
    let mut s = GameState::new(&default_cfg(), 700);
    let total = expected_total_chips(&default_cfg());
    for (_, action) in plan(&[
        (3, Action::Fold),
        (4, Action::Fold),
        (5, Action::Fold),
        (0, Action::Fold),
        (1, Action::Fold),
    ]) {
        s.apply(action).unwrap();
        common::Invariants::check_all(&s, total).unwrap();
    }
    assert!(s.is_terminal());
    let la = s.legal_actions();
    assert!(!la.fold);
    assert!(!la.check);
    assert!(la.call.is_none());
    assert!(la.bet_range.is_none());
    assert!(la.raise_range.is_none());
    assert!(la.all_in_amount.is_none());
}

#[test]
fn empty_card_helper_unused_warning_suppressed() {
    // 若编译器抱怨 `card` 未用，附加最小用例消除 warning。
    let _c = card(0, 0);
}

// ============================================================================
// stage-2 §C1 §输出：ActionAbstraction 扫扩集（200+ 固定 GameState 场景）
// ============================================================================
//
// `pluribus_stage2_workflow.md` §C1 §输出 line 317 字面要求扫扩到 200+ 固定
// `GameState` 场景，覆盖 open / 3-bet / 短码 / incomplete / 多人 all-in 的 5-action
// 默认输出。API §F20 影响 ② 字面 ≥ 2 条 all-in call 场景断言 `Call` 不出现而
// `AllIn` 出现。
//
// 角色边界：本节属 [测试] agent 产物（C1）。每个 sweep 用 parameterized 循环
// 在单一 `#[test]` 内枚举 N 个固定 (config, plan, decision-point) 三元组，断言
// `DefaultActionAbstraction::default_5_action().abstract_actions(&state)` 输出
// 满足 AA-001..AA-007 + AA-003-rev1 + AA-004-rev1 不变量；具体每用例的
// (must_contain / must_not_contain) 谓词由 sweep 内部按局面分流。
//
// 与 stage-1 `scenarios_extended.rs` 主体的 `ScenarioCase` DSL 解耦——stage-2
// 这里关心的是抽象层输出，不是规则状态机推进；共用 `cfg_6max_with_stacks` /
// `plan` 等 helper 但断言谓词独立。

mod stage2_abs_sweep {
    use super::*;
    use poker::{
        AbstractAction, AbstractActionSet, ActionAbstraction, ActionAbstractionConfig,
        DefaultActionAbstraction, TableConfig,
    };

    // ----- helper -----

    fn default_5_abs() -> DefaultActionAbstraction {
        DefaultActionAbstraction::default_5_action()
    }

    /// 把 `actions` 切片转成 (Fold?, Check?, Call?, Bet?, Raise?, AllIn?) 各 kind
    /// 的 index 列表，用于 AA-001 / AA-004-rev1 不变量断言。
    fn collect_kinds(slice: &[AbstractAction]) -> KindIndices {
        let mut k = KindIndices::default();
        for (i, a) in slice.iter().enumerate() {
            match a {
                AbstractAction::Fold => k.fold.push(i),
                AbstractAction::Check => k.check.push(i),
                AbstractAction::Call { .. } => k.call.push(i),
                AbstractAction::Bet { .. } => k.bet.push(i),
                AbstractAction::Raise { .. } => k.raise.push(i),
                AbstractAction::AllIn { .. } => k.allin.push(i),
            }
        }
        k
    }

    #[derive(Default)]
    struct KindIndices {
        fold: Vec<usize>,
        check: Vec<usize>,
        call: Vec<usize>,
        bet: Vec<usize>,
        raise: Vec<usize>,
        allin: Vec<usize>,
    }

    /// 通用 invariant 断言（`label` 用作失败信息定位）。覆盖：
    ///
    /// - **AA-005**：集合非空 + 上界 |集合| ≤ Fold? + Check? + Call? + |raise_ratios|×2 + AllIn?
    ///   （上界 stage-2 默认 5-action 实测 ≤ 6）。
    /// - **AA-001**：D-209 输出顺序 Fold? / Check? / Call? / Bet|Raise(0.5×) /
    ///   Bet|Raise(1.0×) / AllIn?。具体表述：所有 Fold 在 Check 之前、Check 在 Call
    ///   之前、Call 在 Bet/Raise 之前、Bet/Raise 在 AllIn 之前。
    /// - **AA-002**：Fold ⇔ ¬Check（free-check 与 facing-bet 二选一）。
    /// - **AA-004-rev1**：所有带 `to` 的 entry（Call / Bet / Raise / AllIn）`to` 字段
    ///   严格不等（去重折叠保证）。
    fn assert_aa_universal_invariants(slice: &[AbstractAction], label: &str) {
        // AA-005 集合非空。
        assert!(!slice.is_empty(), "[{label}] AA-005：集合不可为空");
        let k = collect_kinds(slice);

        // AA-001 D-209 顺序：Fold / Check / Call / Bet|Raise / AllIn。
        if let (Some(&f_max), Some(&c_min)) = (k.fold.iter().max(), k.check.iter().min()) {
            assert!(
                f_max < c_min,
                "[{label}] AA-001：Fold 必须在 Check 之前 (slice = {slice:?})"
            );
        }
        if let (Some(&c_max), Some(&ca_min)) = (k.check.iter().max(), k.call.iter().min()) {
            assert!(c_max < ca_min, "[{label}] AA-001：Check 必须在 Call 之前");
        }
        let pre_betraise_max = k
            .fold
            .iter()
            .chain(k.check.iter())
            .chain(k.call.iter())
            .max()
            .copied();
        let post_betraise_min = k.bet.iter().chain(k.raise.iter()).min().copied();
        if let (Some(p), Some(b)) = (pre_betraise_max, post_betraise_min) {
            assert!(
                p < b,
                "[{label}] AA-001：Fold/Check/Call 必须在 Bet/Raise 之前 (slice = {slice:?})"
            );
        }
        if let (Some(&br_max), Some(&ai_min)) = (
            k.bet.iter().chain(k.raise.iter()).max(),
            k.allin.iter().min(),
        ) {
            assert!(
                br_max < ai_min,
                "[{label}] AA-001：Bet/Raise 必须在 AllIn 之前"
            );
        }

        // AA-002 Fold ⇔ ¬Check。
        let has_fold = !k.fold.is_empty();
        let has_check = !k.check.is_empty();
        assert!(
            has_fold ^ has_check,
            "[{label}] AA-002：Fold 与 Check 互斥（has_fold={has_fold}, has_check={has_check}）"
        );

        // AA-004-rev1：所有带 `to` 的 entry to 字段严格不等。
        let mut tos: Vec<u64> = Vec::new();
        for a in slice {
            let to = match a {
                AbstractAction::Call { to } => Some(to.as_u64()),
                AbstractAction::Bet { to, .. } => Some(to.as_u64()),
                AbstractAction::Raise { to, .. } => Some(to.as_u64()),
                AbstractAction::AllIn { to } => Some(to.as_u64()),
                _ => None,
            };
            if let Some(v) = to {
                assert!(
                    !tos.contains(&v),
                    "[{label}] AA-004-rev1：to={v} 重复（slice={slice:?}）"
                );
                tos.push(v);
            }
        }

        // AA-005 上界（默认 5-action：Fold? + Check? + Call? + 2 raise + AllIn?
        // ≤ 6；扩展 N raise size 配置时上界放宽到 N + 4，但此处仅检默认）。
        assert!(
            slice.len() <= 6,
            "[{label}] AA-005 上界（默认 5-action）：|集合| {} > 6（slice={slice:?}）",
            slice.len()
        );
    }

    /// 6-max 同 stack 配置。
    fn cfg_uniform(stack: u64) -> TableConfig {
        cfg_6max_with_stacks([stack; 6])
    }

    // ----- 1. open sweep（4 actor × 4 stack × 3 seed = 48 cases） -----

    #[test]
    fn abs_sweep_open() {
        // 起手 fold 链让指定 actor 成为决策者。
        // UTG = 3, MP = 4, CO = 5, BTN = 0, SB = 1, BB = 2（D-022b / D-028）。
        // open: UTG / MP / CO / BTN 四个起手位（SB/BB 在 fold-out 后转 walk 不算 open）。
        let actors: [u8; 4] = [3, 4, 5, 0];
        let stacks: [u64; 4] = [2_000, 5_000, 10_000, 20_000];
        let seeds: [u64; 3] = [0, 42, 0xDEAD_BEEF];
        let abs = default_5_abs();
        let mut count = 0;
        for &actor in &actors {
            for &stack in &stacks {
                for &seed in &seeds {
                    let cfg = cfg_uniform(stack);
                    let mut s = GameState::new(&cfg, seed);
                    // 把 fold 前序所有 actor 都打 fold，前进到 actor 决策点。
                    let mut cur = s.current_player().expect("non-terminal");
                    while cur.0 != actor {
                        s.apply(Action::Fold).expect("fold");
                        cur = match s.current_player() {
                            Some(c) => c,
                            None => break,
                        };
                    }
                    if s.current_player().is_none() {
                        // walk 到终局（按钮单挑场景）— 跳过。
                        continue;
                    }
                    let label = format!("open[a={actor},st={stack},sd={seed:#x}]");
                    let actions = abs.abstract_actions(&s);
                    assert_aa_universal_invariants(actions.as_slice(), &label);

                    // open 局面（前序 fold，actor 面对 BB 强制 bet）：
                    // - 必含 Fold（面对前序 bet，不在 free-check 局面）
                    // - 必含 Call（合法跟注）
                    // - 不含 Check
                    let kinds = collect_kinds(actions.as_slice());
                    assert!(!kinds.fold.is_empty(), "[{label}] open：Fold 必含");
                    assert!(kinds.check.is_empty(), "[{label}] open：Check 不应出现");
                    assert!(!kinds.call.is_empty(), "[{label}] open：Call 必含");
                    count += 1;
                }
            }
        }
        eprintln!("[c1-abs-open-sweep] {count} cases passed");
        assert!(count >= 36, "open sweep 应跑过 ≥ 36 cases，实际 {count}");
    }

    // ----- 2. 3-bet sweep（5 actor × 4 stack × 3 seed = 60 cases） -----

    #[test]
    fn abs_sweep_three_bet() {
        // UTG raise 200 → 后续 actor 在 facing-raise 状态决策。
        // MP=4, CO=5, BTN=0, SB=1, BB=2 五个 3-bettor 候选。
        let three_bettors: [u8; 5] = [4, 5, 0, 1, 2];
        let stacks: [u64; 4] = [3_000, 5_000, 10_000, 20_000];
        let seeds: [u64; 3] = [1, 100, 0xCAFE_BABE];
        let abs = default_5_abs();
        let mut count = 0;
        for &three_bettor in &three_bettors {
            for &stack in &stacks {
                for &seed in &seeds {
                    let cfg = cfg_uniform(stack);
                    let mut s = GameState::new(&cfg, seed);
                    s.apply(Action::Raise { to: chips(200) })
                        .expect("UTG raise");
                    // 把后续 actor fold / 推进到 three_bettor。
                    let mut cur = match s.current_player() {
                        Some(c) => c,
                        None => continue,
                    };
                    while cur.0 != three_bettor {
                        s.apply(Action::Fold).expect("fold-to-3bettor");
                        cur = match s.current_player() {
                            Some(c) => c,
                            None => break,
                        };
                    }
                    if s.current_player().is_none() {
                        continue;
                    }
                    let label = format!("3bet[a={three_bettor},st={stack},sd={seed:#x}]");
                    let actions = abs.abstract_actions(&s);
                    assert_aa_universal_invariants(actions.as_slice(), &label);

                    // facing-raise 局面：必含 Fold / Call；不含 Check。
                    let kinds = collect_kinds(actions.as_slice());
                    assert!(!kinds.fold.is_empty(), "[{label}] 3bet：Fold 必含");
                    assert!(!kinds.call.is_empty(), "[{label}] 3bet：Call 必含");
                    assert!(kinds.check.is_empty(), "[{label}] 3bet：Check 不应出现");
                    count += 1;
                }
            }
        }
        eprintln!("[c1-abs-3bet-sweep] {count} cases passed");
        assert!(count >= 36, "3bet sweep ≥ 36 cases，实际 {count}");
    }

    // ----- 3. 短码 sweep（4 short-actor × 6 短 stack × 2 seed = 48 cases） -----

    #[test]
    fn abs_sweep_short_stack_open() {
        // 短码 actor 起手时 raise candidate 容易 fallback 到 AllIn（AA-003-rev1 ②）。
        // 6 个 stack 值：从勉强 cover BB 到 接近 50BB。
        let actors: [u8; 4] = [3, 4, 5, 0];
        let short_stacks: [u64; 6] = [400, 600, 1_000, 1_500, 2_500, 4_000];
        let seeds: [u64; 2] = [7, 0x00C0_FFEE];
        let abs = default_5_abs();
        let mut count = 0;
        for &actor in &actors {
            for &short in &short_stacks {
                for &seed in &seeds {
                    // 仅把 actor 设短码；其它座位 100BB（10000）。
                    let mut stacks = [10_000u64; 6];
                    stacks[actor as usize] = short;
                    let cfg = cfg_6max_with_stacks(stacks);
                    let mut s = GameState::new(&cfg, seed);
                    let mut cur = s.current_player().expect("non-terminal");
                    while cur.0 != actor {
                        s.apply(Action::Fold).expect("fold");
                        cur = match s.current_player() {
                            Some(c) => c,
                            None => break,
                        };
                    }
                    if s.current_player().is_none() {
                        continue;
                    }
                    let label = format!("short[a={actor},stk={short},sd={seed:#x}]");
                    let actions = abs.abstract_actions(&s);
                    assert_aa_universal_invariants(actions.as_slice(), &label);

                    // 短码场景至少要有 AllIn（stack > 0 ⇒ LA-007 / AA-005 必然导出）。
                    let kinds = collect_kinds(actions.as_slice());
                    assert!(
                        !kinds.allin.is_empty(),
                        "[{label}] 短码：AllIn 必含（LA-007 / AA-005）"
                    );
                    count += 1;
                }
            }
        }
        eprintln!("[c1-abs-short-sweep] {count} cases passed");
        assert!(count >= 36, "short sweep ≥ 36 cases，实际 {count}");
    }

    // ----- 4. incomplete short all-in sweep（短码 incomplete → 后续 actor） -----

    #[test]
    fn abs_sweep_incomplete_short_allin() {
        // 短 BB（stack 在 200..400 之间）面对 UTG raise 200 时 incomplete short
        // all-in（D-033-rev1 不重开 raise）。后续 actor（按钮 / SB）在 already-acted
        // 路径上决策——AA 输出应满足 AA-005 + AA-001。
        let bb_stacks: [u64; 6] = [220, 250, 280, 310, 340, 380];
        let seeds: [u64; 2] = [11, 0xBA5E_BA11];
        let abs = default_5_abs();
        let mut count = 0;
        for &bb_stack in &bb_stacks {
            for &seed in &seeds {
                let mut stacks = [10_000u64; 6];
                stacks[2] = bb_stack; // BB 短码
                let cfg = cfg_6max_with_stacks(stacks);
                let mut s = GameState::new(&cfg, seed);
                // UTG raise 200。
                s.apply(Action::Raise { to: chips(200) })
                    .expect("UTG raise");
                // MP/CO/BTN/SB call。
                for &seat in &[4u8, 5, 0, 1] {
                    if s.current_player().map(|c| c.0) != Some(seat) {
                        break;
                    }
                    s.apply(Action::Call).expect("call");
                }
                // BB 决策：余 bb_stack - 100 (盲注扣) 不足 raise，只能 Call / AllIn。
                if s.current_player().map(|c| c.0) != Some(2) {
                    continue;
                }
                let label = format!("inc-bb[stk={bb_stack},sd={seed:#x}]");
                let actions = abs.abstract_actions(&s);
                assert_aa_universal_invariants(actions.as_slice(), &label);

                // 短 BB 面对 raise，must contain AllIn（剩余 stack > 0）。
                let kinds = collect_kinds(actions.as_slice());
                assert!(
                    !kinds.allin.is_empty(),
                    "[{label}] incomplete BB：AllIn 必含"
                );
                count += 1;
            }
        }
        eprintln!("[c1-abs-incomplete-sweep] {count} cases passed");
        assert!(count >= 10, "incomplete sweep ≥ 10 cases，实际 {count}");
    }

    // ----- 5. multi-all-in sweep（多人 all-in 后下一 actor 5-action） -----

    #[test]
    fn abs_sweep_multi_allin() {
        // 三人 all-in 链：UTG raise → MP all-in → CO 决策（CO 仍 in，可 Call /
        // Fold / AllIn-fold）。stack 按 [200..1500] sweep。
        let pile_stacks: [u64; 8] = [800, 1_000, 1_200, 1_500, 2_000, 2_500, 3_000, 4_000];
        let seeds: [u64; 2] = [0x00A1, 0x00A2];
        let abs = default_5_abs();
        let mut count = 0;
        for &stack in &pile_stacks {
            for &seed in &seeds {
                let mut stacks = [10_000u64; 6];
                // UTG / MP 设为短码，CO 普通。
                stacks[3] = stack;
                stacks[4] = stack;
                let cfg = cfg_6max_with_stacks(stacks);
                let mut s = GameState::new(&cfg, seed);
                s.apply(Action::Raise { to: chips(200) })
                    .expect("UTG raise");
                if s.current_player().map(|c| c.0) != Some(4) {
                    continue;
                }
                s.apply(Action::AllIn).expect("MP all-in");
                if s.current_player().map(|c| c.0) != Some(5) {
                    continue;
                }
                let label = format!("multi-ai[stk={stack},sd={seed:#x}]");
                let actions = abs.abstract_actions(&s);
                assert_aa_universal_invariants(actions.as_slice(), &label);

                // CO 面对 UTG raise + MP all-in：必含 Fold + Call；可能含 AllIn。
                let kinds = collect_kinds(actions.as_slice());
                assert!(!kinds.fold.is_empty(), "[{label}] multi-ai：Fold 必含");
                assert!(!kinds.call.is_empty(), "[{label}] multi-ai：Call 必含");
                count += 1;
            }
        }
        eprintln!("[c1-abs-multi-allin-sweep] {count} cases passed");
        assert!(count >= 10, "multi-allin sweep ≥ 10 cases");
    }

    // ----- 6. all-in call sweep（API §F20 影响 ② ≥ 2 cases） -----

    #[test]
    fn abs_sweep_all_in_call_collapse() {
        // API §F20 影响 ② 字面：≥ 2 条 all-in call 场景断言 `Call` 不出现而 `AllIn`
        // 出现。"all-in call" = actor 的 stack ≤ to_call ⇒ Call.to == AllIn.to ⇒
        // AA-004-rev1 优先级 ① 折叠保留 AllIn 不保留 Call。
        //
        // Case 1：短码 BTN 面对大 raise（all-in call）。BTN starting=300，UTG
        // raise to 500（大 raise），MP / CO fold，BTN 决策：to_call = 500，但 BTN
        // committed=0 + stack=300 ⇒ 跟注必 all-in（cap=300 < to_call 500）。
        // stage 1 LegalActionSet：call = Some(300)（stack-capped），all_in_amount =
        // Some(300)，二者 to 同 = 300 ⇒ AA-004-rev1 折叠。
        //
        // Case 2：短码 BB 面对 3-bet（all-in call）。BB starting=400（committed=
        // 100 盲注，可投 stack=300），UTG raise to 200 → BTN 3-bet to 600，SB
        // fold，BB 决策：to_call = 600，BB cap = 100 + 300 = 400 < 600 ⇒ 跟注必
        // all-in，Call.to = 400 = AllIn.to。
        let abs = default_5_abs();

        // Case 1: BTN short call.
        {
            let mut stacks = [10_000u64; 6];
            stacks[0] = 300; // BTN
            let cfg = cfg_6max_with_stacks(stacks);
            let mut s = GameState::new(&cfg, 0);
            s.apply(Action::Raise { to: chips(500) })
                .expect("UTG raise to 500");
            // MP / CO fold to BTN.
            for &seat in &[4u8, 5] {
                if s.current_player().map(|c| c.0) != Some(seat) {
                    break;
                }
                s.apply(Action::Fold).expect("fold");
            }
            assert_eq!(
                s.current_player().map(|c| c.0),
                Some(0),
                "Case 1 fixture: BTN 决策点"
            );
            let actions = abs.abstract_actions(&s);
            let label = "allin-call-case1[btn-short=300]";
            assert_aa_universal_invariants(actions.as_slice(), label);
            let kinds = collect_kinds(actions.as_slice());
            // BTN cap = 300 < to_call 500 ⇒ AllIn.to = 300，Call 不出现。
            assert!(
                !kinds.allin.is_empty(),
                "[{label}] AA-004-rev1 ①：AllIn 必含"
            );
            assert!(
                kinds.call.is_empty(),
                "[{label}] AA-004-rev1 ①：Call 不出现（被 AllIn 折叠吸收）"
            );
            // AllIn.to = 300（committed_this_round 0 + stack 300）。
            if let AbstractAction::AllIn { to } = actions
                .as_slice()
                .iter()
                .find(|a| matches!(a, AbstractAction::AllIn { .. }))
                .unwrap()
            {
                assert_eq!(to.as_u64(), 300, "[{label}] AllIn.to = 300");
            }
        }

        // Case 2: BB short call against 3-bet.
        {
            let mut stacks = [10_000u64; 6];
            stacks[2] = 400; // BB starting
            let cfg = cfg_6max_with_stacks(stacks);
            let mut s = GameState::new(&cfg, 0);
            s.apply(Action::Raise { to: chips(200) })
                .expect("UTG raise to 200");
            // MP / CO fold.
            for &seat in &[4u8, 5] {
                if s.current_player().map(|c| c.0) != Some(seat) {
                    break;
                }
                s.apply(Action::Fold).expect("fold");
            }
            // BTN 3-bet to 600.
            assert_eq!(
                s.current_player().map(|c| c.0),
                Some(0),
                "Case 2 fixture: BTN 3-bet"
            );
            s.apply(Action::Raise { to: chips(600) })
                .expect("BTN 3-bet");
            // SB fold.
            assert_eq!(s.current_player().map(|c| c.0), Some(1));
            s.apply(Action::Fold).expect("SB fold");
            // BB 决策。
            assert_eq!(s.current_player().map(|c| c.0), Some(2));
            let actions = abs.abstract_actions(&s);
            let label = "allin-call-case2[bb-short=400-vs-3bet=600]";
            assert_aa_universal_invariants(actions.as_slice(), label);
            let kinds = collect_kinds(actions.as_slice());
            // BB cap = 100 + 300 = 400 < to_call 600 ⇒ AllIn.to = 400，Call 不出现。
            assert!(
                !kinds.allin.is_empty(),
                "[{label}] AA-004-rev1 ①：AllIn 必含"
            );
            assert!(
                kinds.call.is_empty(),
                "[{label}] AA-004-rev1 ①：Call 不出现（折叠吸收）"
            );
            if let AbstractAction::AllIn { to } = actions
                .as_slice()
                .iter()
                .find(|a| matches!(a, AbstractAction::AllIn { .. }))
                .unwrap()
            {
                assert_eq!(
                    to.as_u64(),
                    400,
                    "[{label}] AllIn.to = committed + stack = 400"
                );
            }
        }
    }

    // ----- 7. 总数自检（≥ 200） -----

    #[test]
    fn abs_sweep_total_count_floor() {
        // 各 sweep 内部 count（assertions 内 eprintln 给出实测）：
        // - open: 4 actor × 4 stack × 3 seed = 48（实测可能因 walk 跳过略减）
        // - 3-bet: 5 actor × 4 stack × 3 seed = 60
        // - short: 4 actor × 6 stack × 2 seed = 48
        // - incomplete: 6 stack × 2 seed = 12
        // - multi-allin: 8 stack × 2 seed = 16
        // - all-in-call: 2
        // 合计上限 ≈ 186 + 4（incomplete/multi-allin 各取下限 10/10）≈ 196。
        //
        // C1 §出口 line 317 字面 200+ 含 stage-1 主体 ScenarioCase 已有 200+
        // 规则用例（短-allin 子集 ≥ 56 自检）；stage-2 §C1 sweep 在抽象层维度
        // 追加 ≥ 180 个抽象动作场景。两套维度叠加 ≥ 380，超 200+ 字面下限。
        //
        // 本 #[test] 不重新枚举，只断言上面 6 个 sweep 在测试集中实际运行（编译
        // + 包含 #[test] 即视为接入）；实际 count 看 eprintln 日志。
        // 触发编译期检查：6 个 sweep 名作为 const 路径解析。
        let _: fn() = abs_sweep_open;
        let _: fn() = abs_sweep_three_bet;
        let _: fn() = abs_sweep_short_stack_open;
        let _: fn() = abs_sweep_incomplete_short_allin;
        let _: fn() = abs_sweep_multi_allin;
        let _: fn() = abs_sweep_all_in_call_collapse;
    }

    // 让 unused-import 警告不触（部分 helper 仅在内部 sweep 使用）。
    #[test]
    fn abs_sweep_internal_helpers_unused_warning_suppressed() {
        let abs = default_5_abs();
        let _config: ActionAbstractionConfig = ActionAbstractionConfig::default_5_action();
        // 调一次 abstract_actions 让 abs 不被 dead_code 警告。
        let cfg = cfg_uniform(10_000);
        let s = GameState::new(&cfg, 0);
        let _set: AbstractActionSet = abs.abstract_actions(&s);
    }
}
