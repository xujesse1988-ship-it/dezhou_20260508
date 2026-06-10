//! 多人（N 座）AIVAT 估计器（缺口⑥，`realtime_search_openpoker_exec` §3.2 #6 / §4.1
//! live 功效预算）。HU 版见 [`crate::training::aivat_nlhe`]（Slumbot 单边，对手底牌恒知）；
//! 本模块面向 **live 多人对局（OpenPoker HH 日志）**——对手底牌大多**不可见**（只有摊牌
//! `shown_cards`），修正项集合因此与 HU 版不同。
//!
//! # 修正项（v1）与无偏性
//!
//! ```text
//! AIVAT = U − ( c_deal_us + c_runout )
//! ```
//!
//! - **`c_deal_us`（我方发牌，精确无偏）**：手开始时我方底牌均匀于 C(52,2)=1326，修正不
//!   条件在任何对局信息上 → 对任意固定 `V₁(rel_pos, 169类)` 均值精确为零（HU §4.1 同式，
//!   位置推广为相对 button 的 N 座 rel_pos）。值函数经 [`MultiwayDealValueFn`] 注入（生产 =
//!   6-max blueprint 自对弈 VF-1，169×N 小表；缺省 = 该项 0）。
//! - **`c_runout`（all-in 锁定后纯发牌段，主力降方差项）**：最后一个决策动作之后还有公共牌
//!   要发（摊牌且锁定街 < river）→ `c_runout = U − E_runout[U]`，`E_runout` 对剩余牌堆**精确
//!   枚举全部补全**，逐补全经 [`GameState::with_external_cards_and_runout`] + `apply`（最后
//!   动作）+ 权威 [`GameState::payouts`] 结算——N-way side pot / all-in-for-less 退注 /
//!   odd-chip 与真实结算同一套口径，零漂移（HU §4.4 的 clone+finalize 处方，多人下必须走
//!   引擎、不能用 `m·(2eq−1)` 闭式）。
//!
//! # 不纳入的修正项（与 HU 版的诚实差异）
//!
//! - **`c_deal_opp`**：需要对手底牌。live 只有摊牌亮牌——若只在摊牌手上纳入，纳入与否
//!   条件在对局结果上（selection bias，`E[c | 摊牌] ≠ 0`）→ **任何情况下都不纳入**。
//! - **`c_board` / `c_act`**：HU 生产实测（`docs/aivat_eval.md` §10）自对弈 VF 的 board/act
//!   修正**净加噪声**（推荐估计量 = deals+runout）；且 live 多人下 `c_board` 的 sibling 分布
//!   严格依赖全部在场底牌（未知）——v1 不做。`c_act` 数学上精确无偏（我方自采样），等 HH
//!   日志带 σ 后可作 v2 增量。
//!
//! # 已知近似（文档明示，唯一一处）
//!
//! `E_runout` 的牌堆 = 52 − 我方底牌 − 摊牌亮牌 − 已发 board；**弃牌座的未知底牌仍留在
//! 牌堆里**。真实 runout 分布条件在全部已发牌上（含弃牌座），而弃牌动作与其底牌相关 →
//! 本近似带 card-bunching 残差（量级 = 弃牌 range 的牌移除效应 × all-in 频率，远小于
//! live 关心的 effect；由 `tests/aivat_multiway_selfplay.rs` 的配对 d 闸门经验封顶——
//! 模拟里弃牌策略显式依赖底牌，残差若实质会被 |mean d| ≤ 1.96·SE(d) 抓到）。
//! `n_unknown_folded` 逐手记录暴露面。
//!
//! # 单位（统一 mbb/g）
//!
//! 估计器输入 / 输出一律 **chips（solver 单位）**；报表层用 [`chips_to_mbb_per_hand`]
//! 统一换算 mbb/g（与 §11.5d / live 功效预算同一量纲：1 mbb = BB/1000）。
//!
//! # 成本注记
//!
//! `E_runout` 精确枚举的补全数随锁定街指数涨：turn 锁 ~44、flop 锁 ~C(43,2)≈900、
//! **preflop 锁 ~C(48,5)≈1.7M**（×每补全 clone+apply+payouts ≈ 数 µs release）——preflop
//! all-in 手单手 ~秒级。生产批量评测若被 preflop 锁拖慢，可换固定 seed 的 MC sibling 采样
//! （采样与 realized 无关 → 仍无偏、只多噪声），目前不做（正确性优先、先量再优化）。

use crate::abstraction::preflop::PreflopLossless169;
use crate::core::{Card, PlayerStatus, SeatId};
use crate::rules::action::Action;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::aivat_nlhe::enumerate_combinations;

/// 重放 seed：牌全部被外部注入覆盖 / 或不被读（fold 终局），seed 只是 `GameState::new` 的
/// 强制输入。
const REPLAY_SEED: u64 = 0x4D57_4149_5641_5430; // "MWAIVAT0"

/// 一手的输入（HH 日志口径，全部 **solver 单位** chips）。
pub struct MultiwayHandInput {
    /// 真实 per-seat 起始栈（不等栈合法）+ 盲注 + button 的桌面配置。
    pub config: TableConfig,
    /// 我方座位。
    pub our_seat: SeatId,
    pub our_hole: [Card; 2],
    /// 摊牌亮牌 per seat（`None` = 未亮）。我方座可 `Some`（须与 `our_hole` 一致）可 `None`。
    pub revealed: Vec<Option<[Card; 2]>>,
    /// 真实落地公共牌（fold 早终时短；摊牌手须全 5 张）。
    pub board: Vec<Card>,
    /// 整手时序的 (座位, 具体动作)。
    pub actions: Vec<(SeatId, Action)>,
    /// `U` = 我方净收益 chips（HH 结算；估计器重放交叉校验）。
    pub winnings: i64,
}

/// 一手的 AIVAT 分解结果（chips）。
#[derive(Clone, Copy, Debug, Default)]
pub struct MultiwayHandResult {
    pub raw: f64,
    pub aivat: f64,
    pub c_deal_us: f64,
    pub c_runout: f64,
    /// 本手是否进入纯发牌段（all-in 锁定 → `c_runout` 生效）。
    pub has_runout: bool,
    /// `E_runout` 枚举的补全数（`has_runout` 时 > 0）。
    pub n_runout_completions: u64,
    /// 锁定时刻底牌未知的弃牌座数（card-bunching 近似的暴露面，见模块 doc）。
    pub n_unknown_folded: usize,
    /// 我方相对 button 的位置（0 = button，按座位序递增）——`c_deal_us` 的 VF 键。
    pub our_rel_pos: usize,
}

/// `c_deal_us` 的值函数（VF-1）：`(相对 button 位置, preflop 169 类) → E[U]`。任意**固定**
/// 函数都保持无偏（只影响降方差幅度）；未覆盖格返回 `None`（估计器按 0 计）。
pub trait MultiwayDealValueFn {
    fn v_deal(&self, rel_pos: usize, class169: usize) -> Option<f64>;
}

/// chips → mbb/g（统一评测量纲：1 mbb = BB/1000）。
pub fn chips_to_mbb_per_hand(chips: f64, big_blind_chips: u64) -> f64 {
    chips * 1000.0 / big_blind_chips as f64
}

/// 多人 AIVAT 估计器。`vf = None` → `c_deal_us ≡ 0`（纯 runout 修正，无需任何 artifact）。
pub struct MultiwayAivatEstimator<'a> {
    vf: Option<&'a dyn MultiwayDealValueFn>,
    pf: PreflopLossless169,
}

impl<'a> MultiwayAivatEstimator<'a> {
    pub fn new(vf: Option<&'a dyn MultiwayDealValueFn>) -> Self {
        MultiwayAivatEstimator {
            vf,
            pf: PreflopLossless169::new(),
        }
    }

    /// 逐手 AIVAT 分解。任何一致性断言失败（重放对不上 / U 校验失败 / 摊牌参与者底牌缺失）
    /// → `Err`（loud：数据管道问题须暴露，不静默给偏结果；**绝不 panic**）。
    pub fn estimate_hand(&self, input: &MultiwayHandInput) -> Result<MultiwayHandResult, String> {
        let n = input.config.n_seats as usize;
        let our = input.our_seat.0 as usize;
        if our >= n {
            return Err(format!("our_seat {our} 越界（{n} 座）"));
        }
        if input.revealed.len() != n {
            return Err(format!(
                "revealed 长度 {} ≠ 座位数 {n}",
                input.revealed.len()
            ));
        }
        if let Some(r) = input.revealed[our] {
            let mut a = [r[0].to_u8(), r[1].to_u8()];
            let mut b = [input.our_hole[0].to_u8(), input.our_hole[1].to_u8()];
            a.sort_unstable();
            b.sort_unstable();
            if a != b {
                return Err("revealed[our_seat] 与 our_hole 不一致".to_string());
            }
        }
        if input.actions.is_empty() {
            return Err("动作序为空".to_string());
        }

        // ---- 重放（actor 逐步校验；牌不读——下注几何与牌无关）----
        let mut state = GameState::new(&input.config, REPLAY_SEED);
        let mut pre_final: Option<GameState> = None;
        for (i, (seat, action)) in input.actions.iter().enumerate() {
            if state.current_player() != Some(*seat) {
                return Err(format!(
                    "重放第 {i} 步：期望行动者 {:?} ≠ 日志 {seat:?}",
                    state.current_player()
                ));
            }
            if i + 1 == input.actions.len() {
                pre_final = Some(state.clone());
            }
            state
                .apply(*action)
                .map_err(|e| format!("重放第 {i} 步 apply({action:?}) 非法: {e:?}"))?;
        }
        if !state.is_terminal() {
            return Err("动作序结束但未到终局（HH 不完整？）".to_string());
        }
        let pre_final = pre_final.expect("actions 非空已查");
        let final_action = input.actions[input.actions.len() - 1].1;

        // ---- 终局类型 ----
        let contenders: Vec<usize> = state
            .players()
            .iter()
            .enumerate()
            .filter(|(_, p)| p.status != PlayerStatus::Folded)
            .map(|(i, _)| i)
            .collect();
        let showdown = contenders.len() >= 2;

        let u = input.winnings as f64;
        let mut res = MultiwayHandResult {
            raw: u,
            our_rel_pos: (our + n - input.config.button_seat.0 as usize) % n,
            ..Default::default()
        };

        // holes（按 **pre_final** 的未弃牌口径——with_external_cards_and_runout 校验已弃座不给
        // 底牌）：pre_final 未弃 + 已知（我方 / 亮牌）→ Some；pre_final 未弃但未知（只可能是
        // 最后一动作弃牌收口的座，摊牌前已弃、payouts 不读）→ None（占位）；pre_final 已弃 →
        // None。我方在锁定前已弃 → holes 不装（但已知牌仍进牌堆扣除，见 c_runout 的 known）。
        // 摊牌参与者底牌缺失 = 数据管道问题 → Err。
        let holes: Vec<Option<[Card; 2]>> = (0..n)
            .map(|i| {
                if pre_final.players()[i].hole_cards.is_none() {
                    None
                } else if i == our {
                    Some(input.our_hole)
                } else {
                    input.revealed[i]
                }
            })
            .collect();
        if showdown {
            for &i in &contenders {
                if holes[i].is_none() {
                    return Err(format!(
                        "摊牌参与者 seat {i} 底牌缺失（HH 日志缺 shown_cards？）"
                    ));
                }
            }
            if input.board.len() != 5 {
                return Err(format!("摊牌手 board 须 5 张，得 {}", input.board.len()));
            }
        }

        // ---- U 交叉校验（loud：抓单位 / 重放漂移）----
        let u_replay = if showdown {
            let bd = pre_final.board().len();
            let realized = pre_final.with_external_cards_and_runout(
                &holes,
                &input.board[..bd],
                &input.board[bd..],
            )?;
            let mut realized = realized;
            realized
                .apply(final_action)
                .map_err(|e| format!("realized 终局 apply 非法: {e:?}"))?;
            if !realized.is_terminal() {
                return Err("realized 重放最后动作后未终局".to_string());
            }
            payout_for(&realized, our)?
        } else {
            payout_for(&state, our)?
        };
        if u_replay != input.winnings {
            return Err(format!(
                "U 校验失败：重放结算 {u_replay} ≠ 日志 winnings {}（单位/重放漂移）",
                input.winnings
            ));
        }

        // ---- c_runout（摊牌、我方在 contender 中、且最后动作之后还有发牌段）----
        // 我方在锁定前已弃 → U = −committed_total 与 runout 无关 → c_runout 恒 0，跳过枚举。
        if showdown && contenders.contains(&our) && pre_final.board().len() < 5 {
            res.has_runout = true;
            let prefix: Vec<Card> = pre_final.board().to_vec();
            res.n_unknown_folded = (0..n)
                .filter(|&i| {
                    i != our
                        && state.players()[i].status == PlayerStatus::Folded
                        && input.revealed[i].is_none()
                })
                .count();
            // 牌堆 = 52 − 已知（我方底牌 + 全部亮牌 + 已发 prefix）；弃牌未知座的 2 张留在堆中
            // （模块 doc 的 card-bunching 近似）。注意从 *prefix* 起枚举——realized 后缀是补全之一。
            let mut known: Vec<Card> = prefix.clone();
            known.extend_from_slice(&input.our_hole);
            for (i, h) in input.revealed.iter().enumerate() {
                if i != our {
                    if let Some(h) = h {
                        known.extend_from_slice(h);
                    }
                }
            }
            let deck = remaining_deck_excluding(&known);
            let k = 5 - prefix.len();
            let mut sum: f64 = 0.0;
            let mut count: u64 = 0;
            let mut first_err: Option<String> = None;
            enumerate_combinations(&deck, k, &mut |combo| {
                if first_err.is_some() {
                    return;
                }
                let r = (|| -> Result<i64, String> {
                    let mut st =
                        pre_final.with_external_cards_and_runout(&holes, &prefix, combo)?;
                    st.apply(final_action)
                        .map_err(|e| format!("补全终局 apply 非法: {e:?}"))?;
                    if !st.is_terminal() {
                        return Err("补全重放最后动作后未终局".to_string());
                    }
                    payout_for(&st, our)
                })();
                match r {
                    Ok(p) => {
                        sum += p as f64;
                        count += 1;
                    }
                    Err(e) => first_err = Some(e),
                }
            });
            if let Some(e) = first_err {
                return Err(format!("E_runout 补全失败: {e}"));
            }
            if count == 0 {
                return Err("E_runout 无补全（牌堆耗尽？）".to_string());
            }
            res.n_runout_completions = count;
            let e_runout = sum / count as f64;
            res.c_runout = u - e_runout;
        }

        // ---- c_deal_us（VF-1 注入时）----
        if let Some(vf) = self.vf {
            let pos = res.our_rel_pos;
            let v1 = |class: usize| -> f64 { vf.v_deal(pos, class).unwrap_or(0.0) };
            let realized = v1(usize::from(self.pf.hand_class(input.our_hole)));
            let mut sum = 0.0;
            let mut cnt = 0u64;
            for a in 0u8..52 {
                for b in (a + 1)..52 {
                    let hole = [Card::from_u8(a).unwrap(), Card::from_u8(b).unwrap()];
                    sum += v1(usize::from(self.pf.hand_class(hole)));
                    cnt += 1;
                }
            }
            res.c_deal_us = realized - sum / cnt as f64;
        }

        res.aivat = u - (res.c_deal_us + res.c_runout);
        Ok(res)
    }
}

/// 终局 `payouts` 里我方座位的净收益。
fn payout_for(state: &GameState, seat: usize) -> Result<i64, String> {
    let payouts = state.payouts().ok_or("终局无 payouts")?;
    payouts
        .iter()
        .find(|(s, _)| s.0 as usize == seat)
        .map(|(_, p)| *p)
        .ok_or_else(|| format!("payouts 缺座位 {seat}"))
}

/// 52 张里扣掉 `known` 后的剩余牌堆。
fn remaining_deck_excluding(known: &[Card]) -> Vec<Card> {
    let mut used = [false; 52];
    for c in known {
        used[c.to_u8() as usize] = true;
    }
    (0u8..52)
        .filter(|&i| !used[i as usize])
        .map(|i| Card::from_u8(i).unwrap())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ChipAmount;

    fn c(s: &str) -> Card {
        crate::training::nlhe_replay::parse_card(s).unwrap()
    }

    fn cfg_3max(stacks: [u64; 3]) -> TableConfig {
        TableConfig {
            n_seats: 3,
            starting_stacks: stacks.iter().map(|&s| ChipAmount::new(s)).collect(),
            small_blind: ChipAmount::new(50),
            big_blind: ChipAmount::new(100),
            ante: ChipAmount::ZERO,
            button_seat: SeatId(0),
        }
    }

    /// fold 终局（无摊牌）：c_runout=0、U 校验走重放 payouts、rel_pos 正确。
    /// 3-max button=0：SB=1 BB=2，button 先行动。button fold → SB fold → BB 收锅。
    #[test]
    fn fold_end_no_runout_and_u_checked() {
        let cfg = cfg_3max([10_000, 10_000, 10_000]);
        let input = MultiwayHandInput {
            config: cfg,
            our_seat: SeatId(2), // BB（fold-win，+SB 50）
            our_hole: [c("Ah"), c("Kd")],
            revealed: vec![None, None, None],
            board: vec![],
            actions: vec![(SeatId(0), Action::Fold), (SeatId(1), Action::Fold)],
            winnings: 50,
        };
        let est = MultiwayAivatEstimator::new(None);
        let r = est.estimate_hand(&input).expect("fold 终局应 Ok");
        assert!(!r.has_runout);
        assert_eq!(r.c_runout, 0.0);
        assert_eq!(r.c_deal_us, 0.0, "vf=None → c_deal_us 0");
        assert_eq!(r.aivat, r.raw);
        assert_eq!(r.our_rel_pos, 2);
        // U 错值 → loud Err。
        let bad = MultiwayHandInput {
            winnings: 60,
            ..input
        };
        assert!(est.estimate_hand(&bad).is_err(), "U 校验失败应 Err");
    }

    /// 摊牌参与者底牌缺失 → Err（数据管道问题必须暴露，不静默跳过——选择性跳过 = selection bias）。
    #[test]
    fn missing_showdown_cards_is_loud_err() {
        let cfg = cfg_3max([10_000, 10_000, 10_000]);
        // button fold；SB all-in；BB(我) call → HU all-in 摊牌（runout）。
        let input = MultiwayHandInput {
            config: cfg,
            our_seat: SeatId(2),
            our_hole: [c("Ah"), c("Kd")],
            revealed: vec![None, None, None], // SB 摊牌但未亮 → 必 Err
            board: vec![c("2c"), c("7d"), c("9h"), c("Ts"), c("3s")],
            actions: vec![
                (SeatId(0), Action::Fold),
                (SeatId(1), Action::AllIn),
                (SeatId(2), Action::Call),
            ],
            winnings: 0,
        };
        let est = MultiwayAivatEstimator::new(None);
        let e = est.estimate_hand(&input).unwrap_err();
        assert!(e.contains("底牌缺失"), "应报摊牌底牌缺失，得 {e}");
    }
}
