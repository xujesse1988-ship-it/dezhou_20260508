//! 简化 NLHE 的 AIVAT 估计器（控制变量形式）。见 `docs/aivat_eval.md` §3/§4。
//!
//! 单边 `P_a = {chance, 我方}`：只修正发牌 + 公共牌 + 我方决策；**Slumbot 动作不碰**。
//! 对任意固定值函数无偏（§3），值表只决定降方差幅度。逐手算
//! ```text
//! AIVAT = U − ( c_deal_us + c_deal_opp + Σ c_b + [c_runout] + Σ c_act )
//! ```
//!
//! 重放走 [`crate::training::nlhe_replay`]（与 advisor 同一份）；σ 由调用方注入的
//! Hybrid 闭包给（在日志 `info_set` 上全精度重算）；a\* = 日志 `chosen`。值表 = 蓝图
//! 自对弈 [`AivatValueTables`]。
//!
//! **未访问 VF 格 → fallback 0.0**：仍是 fixed-V（确定性），无偏不受影响（§3：realized
//! 与全部 siblings 用同一函数，realized board 本身是 sibling 之一 → E[c]=0 精确），只略
//! 损降方差。
//!
//! **街切换 V_child（关键正确性，修 §4.5 字面"bucket 不变"）**：我方某动作若**关闭本街**
//! → 孩子是**下一街**决策节点；下一街的牌未知且**不可偷看 realized**（否则非 fixed-V →
//! 有偏）。正解：对下一街新牌**积分** `V_child = avg_{新牌} V_info[child, bucket(us, board+新牌)]`
//! ——这正是让各项 telescoping、把 U 方差逐层剥掉的值（同街动作则 `bucket 不变`，直接当前
//! board 取桶，与 doc 一致）。

use std::sync::Arc;

use crate::abstraction::action::StreetActionAbstraction;
use crate::abstraction::info::{InfoSetId, StreetTag};
use crate::abstraction::preflop::PreflopLossless169;
use crate::core::Card;
use crate::eval::eval7;
use crate::rules::action::Action;
use crate::rules::state::GameState;
use crate::training::aivat_value::AivatValueTables;
use crate::training::nlhe::SimplifiedNlheGame;
use crate::training::nlhe_betting_tree::{AbstractActionTag, Child, NodeId, TreeNode};
use crate::training::nlhe_dense::NlheDenseIndexer;
use crate::training::nlhe_replay::{
    committed_totals, replay_trajectory, tokenize, TerminalKind, BOARD_LEN,
};

/// 一手的输入（来自 strategy 日志 + hands 日志）。
pub struct HandInput {
    /// Slumbot `client_pos`（0 = BB / 1 = SB）。我方 solver 座位 = `1 − client_pos`。
    pub client_pos: u8,
    pub our_hole: [Card; 2],
    pub opp_hole: [Card; 2],
    /// 真实落地公共牌（len 0/3/4/5；fold 早终时短）。
    pub board: Vec<Card>,
    /// Slumbot action 串（完整一手）。
    pub action: String,
    /// `U` = 我方净收益 chips（= 日志 winnings）。
    pub winnings: i64,
    /// 每个**我方**决策的日志记录（按时间序）。
    pub log_decisions: Vec<LoggedDecision>,
}

/// strategy 日志里一个我方决策（用于一致性 cross-check；核心计算走 replay + 重算 σ）。
pub struct LoggedDecision {
    pub info_set: u64,
    /// `action_probs`：动作短名 → 概率（serde_json object，顺序不可靠 → 按名对齐）。
    pub probs_by_name: std::collections::HashMap<String, f64>,
    /// `chosen` 短名（`fold`/`call`/`0.5pot`/...）。
    pub chosen: String,
    pub fallback_uniform: bool,
}

/// 一手的 AIVAT 分解结果（mbb 之前的 chips 单位，= 净收益）。
#[derive(Clone, Copy, Debug, Default)]
pub struct HandResult {
    pub raw: f64,
    pub aivat: f64,
    pub c_deal_us: f64,
    pub c_deal_opp: f64,
    pub c_board: f64,
    pub c_runout: f64,
    pub c_act: f64,
    pub n_our_decisions: usize,
    /// 我方位置（0 = SB/button，1 = BB），供调用方按位置拆分统计。
    pub our_pos: usize,
}

/// 值函数抽象：估计器只按 `(pos, node, bucket)` / `(pos, 双方 169 类)` 查，未访问返回
/// `None`（估计器走 fallback 0.0）。生产用 [`TableValueFn`]（包 [`AivatValueTables`] +
/// indexer）；测试可注入合成闭包，省掉多 GB dense 表 + blueprint。
pub trait AivatValueFn {
    fn v_info(&self, pos: usize, node: NodeId, bucket: u32) -> Option<f64>;
    fn v_root_both(&self, pos: usize, our_class: usize, opp_class: usize) -> Option<f64>;
}

/// 生产值函数：[`AivatValueTables`] + dense indexer（`row_for(node,bucket)` 定位行）。
pub struct TableValueFn {
    pub tables: AivatValueTables,
    pub indexer: Arc<NlheDenseIndexer>,
}

impl AivatValueFn for TableValueFn {
    fn v_info(&self, pos: usize, node: NodeId, bucket: u32) -> Option<f64> {
        self.tables.v_info(pos, self.indexer.row_for(node, bucket))
    }
    fn v_root_both(&self, pos: usize, our_class: usize, opp_class: usize) -> Option<f64> {
        self.tables.v_root_both(pos, our_class, opp_class)
    }
}

/// AIVAT 估计器（持只读资源；值函数 + σ 由调用方注入）。
pub struct AivatNlheEstimator<'a> {
    game: &'a SimplifiedNlheGame,
    vf: &'a dyn AivatValueFn,
    abstraction: StreetActionAbstraction,
    button_seat: u8,
    pf: PreflopLossless169,
    /// `info → Hybrid 解析后的归一分布`（advisor 当时实际抽样所用）。
    sigma_fn: Box<dyn Fn(InfoSetId) -> Vec<f64> + 'a>,
    /// σ cross-check 容差（重算 σ 四舍五入到 4 位后与日志值的最大允许差）。
    pub sigma_tol: f64,
}

impl<'a> AivatNlheEstimator<'a> {
    pub fn new(
        game: &'a SimplifiedNlheGame,
        vf: &'a dyn AivatValueFn,
        sigma_fn: Box<dyn Fn(InfoSetId) -> Vec<f64> + 'a>,
    ) -> Self {
        AivatNlheEstimator {
            game,
            vf,
            abstraction: StreetActionAbstraction::default_6_action(),
            button_seat: game.config.button_seat.0,
            pf: PreflopLossless169::new(),
            sigma_fn,
            sigma_tol: 1.5e-4, // 4 位小数舍入 ±0.5e-4，留余量
        }
    }

    /// 逐手 AIVAT 分解。任何一致性断言失败 → Err（loud，不静默给偏结果）。
    pub fn estimate_hand(&self, input: &HandInput) -> Result<HandResult, String> {
        if input.client_pos > 1 {
            return Err(format!("client_pos 必须 0/1，收到 {}", input.client_pos));
        }
        let our_seat = 1 - input.client_pos;
        let our_pos = if our_seat == self.button_seat { 0 } else { 1 };

        let ctx = HandCtx {
            est: self,
            our_hole: input.our_hole,
            opp_hole: input.opp_hole,
            board: &input.board,
            our_pos,
        };

        let tokens = tokenize(&input.action)?;
        let traj = replay_trajectory(self.game, &self.abstraction, &tokens)?;

        // ---- 我方决策对齐日志 ----
        let our_decisions: Vec<&_> = traj
            .decisions
            .iter()
            .filter(|d| d.actor == our_seat)
            .collect();
        if our_decisions.len() != input.log_decisions.len() {
            return Err(format!(
                "我方决策数 {} != 日志 {}（action={:?}）",
                our_decisions.len(),
                input.log_decisions.len(),
                input.action
            ));
        }

        let u = input.winnings as f64;
        let mut res = HandResult {
            raw: u,
            n_our_decisions: our_decisions.len(),
            our_pos,
            ..Default::default()
        };

        // ---- §4.1 我方发牌 ----
        res.c_deal_us = ctx.c_deal_us();
        // ---- §4.2 对方发牌 ----
        res.c_deal_opp = ctx.c_deal_opp();

        // ---- §4.3 公共牌事件（有后续决策的街）----
        let mut c_board = 0.0;
        for s in [StreetTag::Flop, StreetTag::Turn, StreetTag::River] {
            if let Some(node_b) = traj.street_first_decision[s as usize] {
                c_board += ctx.c_board_event(node_b, s)?;
            }
        }
        res.c_board = c_board;

        // ---- §4.4 runout（锁定后纯发牌段）----
        if let TerminalKind::Showdown { lock_street } = traj.terminal {
            if (lock_street as usize) < (StreetTag::River as usize) {
                let m = committed_totals(&traj.final_real)
                    .iter()
                    .copied()
                    .min()
                    .unwrap() as f64;
                let board_so_far = &input.board[..BOARD_LEN[lock_street as usize]];
                let e_runout = ctx.runout_ev(board_so_far, m);
                res.c_runout = u - e_runout;
            }
        }

        // ---- §4.5 我方动作 ----
        let mut c_act = 0.0;
        for (k, d) in our_decisions.iter().enumerate() {
            c_act += ctx.c_act(
                d.node_id,
                d.street,
                d.chosen_idx,
                &input.log_decisions[k],
                &d.real_before,
            )?;
        }
        res.c_act = c_act;

        res.aivat = u - (res.c_deal_us + res.c_deal_opp + res.c_board + res.c_runout + res.c_act);
        Ok(res)
    }
}

/// 一手内的上下文 + 值函数原语。
struct HandCtx<'a> {
    est: &'a AivatNlheEstimator<'a>,
    our_hole: [Card; 2],
    opp_hole: [Card; 2],
    board: &'a [Card],
    our_pos: usize,
}

impl<'a> HandCtx<'a> {
    /// 用我方牌在 `board_for_bucket` 上对 `node` 取桶查 `V_info`，未访问 → 0.0（fallback）。
    fn vf_at(&self, node: NodeId, board_for_bucket: &[Card]) -> f64 {
        let bucket = self
            .est
            .game
            .info_set_for_cards(node, self.our_hole, board_for_bucket)
            .bucket_id();
        self.est
            .vf
            .v_info(self.our_pos, node, bucket)
            .unwrap_or(0.0)
    }

    /// `avg_{新牌} V_info[node, bucket(us, fixed_prefix + 新牌)]`，新牌从扣掉双方底牌 +
    /// `fixed_prefix` 后的牌堆取 `n_new` 张全组合。`n_new == 0` 退化为 [`Self::vf_at`]。
    fn avg_vf_over_new_cards(&self, node: NodeId, fixed_prefix: &[Card], n_new: usize) -> f64 {
        if n_new == 0 {
            return self.vf_at(node, fixed_prefix);
        }
        let deck = remaining_deck(&self.our_hole, &self.opp_hole, fixed_prefix);
        let mut board: Vec<Card> = fixed_prefix.to_vec();
        board.extend(std::iter::repeat(fixed_prefix[0]).take(n_new)); // 占位，循环里覆盖
        let prefix_len = fixed_prefix.len();
        let mut sum = 0.0;
        let mut count: u64 = 0;
        enumerate_combinations(&deck, n_new, &mut |combo| {
            board[prefix_len..].copy_from_slice(combo);
            sum += self.vf_at(node, &board);
            count += 1;
        });
        sum / count as f64
    }

    /// §4.1：`V₁(c_us,i) − (1/1326) Σ_c V₁(c,i)`，`V₁ = V_info[root, preflop169(c)]`。
    fn c_deal_us(&self) -> f64 {
        let root = self.est.game.tree().root_id();
        let v1 =
            |class: u32| -> f64 { self.est.vf.v_info(self.our_pos, root, class).unwrap_or(0.0) };
        let realized = v1(u32::from(self.est.pf.hand_class(self.our_hole)));
        // 全 1326 牌对的边际（每个 169 类按多重度自然计入）。
        let mut sum = 0.0;
        let mut n = 0u64;
        for a in 0u8..52 {
            for b in (a + 1)..52 {
                let hole = [Card::from_u8(a).unwrap(), Card::from_u8(b).unwrap()];
                sum += v1(u32::from(self.est.pf.hand_class(hole)));
                n += 1;
            }
        }
        realized - sum / n as f64
    }

    /// §4.2：`V₂(c_us,c_opp,i) − (1/1225) Σ_{c'} V₂(c_us,c',i)`，`V₂ = V_root_both`。
    fn c_deal_opp(&self) -> f64 {
        let us_class = self.est.pf.hand_class(self.our_hole) as usize;
        let v2 = |opp_class: usize| -> f64 {
            self.est
                .vf
                .v_root_both(self.our_pos, us_class, opp_class)
                .unwrap_or(0.0)
        };
        let realized = v2(self.est.pf.hand_class(self.opp_hole) as usize);
        // 剩 50 张的全部对子（扣我方 2 张）。
        let used = [self.our_hole[0].to_u8(), self.our_hole[1].to_u8()];
        let mut sum = 0.0;
        let mut n = 0u64;
        for a in 0u8..52 {
            if used.contains(&a) {
                continue;
            }
            for b in (a + 1)..52 {
                if used.contains(&b) {
                    continue;
                }
                let opp = [Card::from_u8(a).unwrap(), Card::from_u8(b).unwrap()];
                sum += v2(self.est.pf.hand_class(opp) as usize);
                n += 1;
            }
        }
        realized - sum / n as f64
    }

    /// §4.3：街 `s` 的牌事件修正。`node_b` = 该街首决策节点；realized board = 日志 board
    /// 到该街；siblings = 扣双方底牌 + 已发前街后该街全部牌组合（flop C(48,3) / turn 45 /
    /// river 44）。
    fn c_board_event(&self, node_b: NodeId, s: StreetTag) -> Result<f64, String> {
        let s_idx = s as usize;
        let board_len_s = BOARD_LEN[s_idx];
        if self.board.len() < board_len_s {
            return Err(format!(
                "board 长度 {} < 街 {s:?} 期望 {board_len_s}",
                self.board.len()
            ));
        }
        // 前街已发 board（flop 的前街 = preflop，0 张）。
        let prev_len = BOARD_LEN[s_idx - 1];
        let fixed_prefix = &self.board[..prev_len];
        let n_new = board_len_s - prev_len;
        let realized = self.vf_at(node_b, &self.board[..board_len_s]);
        let avg = self.avg_vf_over_new_cards(node_b, fixed_prefix, n_new);
        Ok(realized - avg)
    }

    /// §4.5：我方一个决策的 `c_act = V_child(a*) − Σ_a σ(a)·V_child(a)`。
    /// `replay_chosen_idx` = 重放解析出的选中动作下标（tree legal 序）；`log` = 日志记录
    /// （info_set / action_probs / chosen / fallback，按名对齐做一致性断言）。
    fn c_act(
        &self,
        node_id: NodeId,
        street: StreetTag,
        replay_chosen_idx: usize,
        log: &LoggedDecision,
        real_before: &GameState,
    ) -> Result<f64, String> {
        // info_set 一致性（重建 == 日志）。
        let board_at = &self.board[..BOARD_LEN[street as usize]];
        let info = self
            .est
            .game
            .info_set_for_cards(node_id, self.our_hole, board_at);
        if info.raw() != log.info_set {
            return Err(format!(
                "info_set 不一致：重建 {} != 日志 {}（node {node_id} street {street:?}）",
                info.raw(),
                log.info_set
            ));
        }

        let node = self.est.game.tree().node(node_id);
        let n = node.legal_actions.len();

        // σ：Hybrid 全精度重算（advisor 实际抽样分布）。dense 后端未访问行返回 uniform，
        // 永不空 / 全零 → fallback_uniform 恒 false（与本文件日志一致）。
        let raw = (self.est.sigma_fn)(info);
        let computed_fallback = raw.is_empty() || raw.iter().all(|p| *p <= 0.0);
        let sigma: Vec<f64> = if computed_fallback {
            vec![1.0 / n as f64; n]
        } else {
            raw
        };
        if sigma.len() != n {
            return Err(format!(
                "σ 长度 {} != legal {n}（node {node_id}）",
                sigma.len()
            ));
        }
        if computed_fallback != log.fallback_uniform {
            return Err(format!(
                "fallback_uniform 不一致：重算 {computed_fallback} != 日志 {}（node {node_id}）",
                log.fallback_uniform
            ));
        }
        // 按动作短名把日志对齐到 tree legal 顺序 → σ cross-check + 定位 a*。
        let mut log_chosen_idx: Option<usize> = None;
        for (i, tag) in node.legal_actions.iter().enumerate() {
            let name = crate::training::nlhe_replay::tag_short_name(*tag);
            if name == log.chosen {
                log_chosen_idx = Some(i);
            }
            if let Some(&lg) = log.probs_by_name.get(&name) {
                let r = (sigma[i] * 1e4).round() / 1e4;
                if (r - lg).abs() > self.est.sigma_tol {
                    return Err(format!(
                        "σ cross-check 失败 @ node {node_id} 动作 {name}: 重算 {:.6}(→{r:.4}) vs 日志 {lg:.4}",
                        sigma[i]
                    ));
                }
            } else {
                return Err(format!("日志 action_probs 缺动作 {name}（node {node_id}）"));
            }
        }
        let log_chosen_idx = log_chosen_idx.ok_or_else(|| {
            format!(
                "日志 chosen {:?} 不在 tree legal（node {node_id}）",
                log.chosen
            )
        })?;
        // a* 下标一致（replay 解析 == 日志 chosen 位置）。
        if replay_chosen_idx != log_chosen_idx {
            return Err(format!(
                "chosen 下标不一致：replay {replay_chosen_idx} != 日志 {log_chosen_idx}（node {node_id}）"
            ));
        }

        // 各动作的 V_child。
        let mut vchild = Vec::with_capacity(n);
        for i in 0..n {
            vchild.push(self.v_child(node, i, street, real_before)?);
        }
        let baseline: f64 = sigma.iter().zip(&vchild).map(|(s, v)| s * v).sum();
        Ok(vchild[replay_chosen_idx] - baseline)
    }

    /// 单个动作 `i`（在 `node`）的孩子值 `V_child`。
    fn v_child(
        &self,
        node: &TreeNode,
        i: usize,
        street: StreetTag,
        real_before: &GameState,
    ) -> Result<f64, String> {
        match node.children[i] {
            Child::Decision(child) => {
                let child_street = self.est.game.tree().node(child).street;
                if child_street == street {
                    // 同街：bucket 不变（当前 board）。
                    Ok(self.vf_at(child, &self.board[..BOARD_LEN[street as usize]]))
                } else {
                    // 街切换（关闭本街 → 下一街决策）：积分下一街新牌（不偷看 realized）。
                    let cur = BOARD_LEN[street as usize];
                    let nxt = BOARD_LEN[child_street as usize];
                    let fixed_prefix = &self.board[..cur];
                    Ok(self.avg_vf_over_new_cards(child, fixed_prefix, nxt - cur))
                }
            }
            Child::Terminal => self.v_terminal(node.legal_actions[i], street, real_before),
        }
    }

    /// 终局孩子的 `V_child`。fold → 确定性 payoff；摊牌/runout → 用日志真实牌算
    /// （河牌摊牌确定；河前 all-in-call → E_runout，不偷看 realized runout）。
    fn v_terminal(
        &self,
        tag: AbstractActionTag,
        street: StreetTag,
        real_before: &GameState,
    ) -> Result<f64, String> {
        let action = match tag {
            AbstractActionTag::Fold => Action::Fold,
            AbstractActionTag::Check => Action::Check,
            AbstractActionTag::Call => Action::Call,
            AbstractActionTag::AllIn => Action::AllIn,
            other => {
                return Err(format!(
                    "终局孩子来自非 fold/check/call/allin tag {other:?}"
                ));
            }
        };
        let mut real = real_before.clone();
        let our_seat = if self.our_pos == 0 {
            self.est.button_seat
        } else {
            1 - self.est.button_seat
        };
        real.apply(action)
            .map_err(|e| format!("v_terminal real.apply({action:?}) 非法: {e:?}"))?;
        if !real.is_terminal() {
            return Err(format!(
                "v_terminal: tag {tag:?} 后 real 非终局（帧漂移？）"
            ));
        }
        if let AbstractActionTag::Fold = tag {
            // fold：确定性 payoff（与牌无关），直接读规则引擎结算。
            let payouts = real.payouts().ok_or("fold 终局无 payouts")?;
            let p = payouts
                .iter()
                .find(|(s, _)| s.0 == our_seat)
                .ok_or("payouts 缺我方座位")?
                .1;
            return Ok(p as f64);
        }
        // 摊牌 / runout：matched amount = min(committed_total)。
        let m = committed_totals(&real).iter().copied().min().unwrap() as f64;
        if (street as usize) >= (StreetTag::River as usize) {
            // 河牌（全 5 张已发）：确定性摊牌。
            Ok(self.showdown_net(m))
        } else {
            // 河前 all-in-call：runout（积分剩余牌，不偷看 realized）。
            let board_so_far = &self.board[..BOARD_LEN[street as usize]];
            Ok(self.runout_ev(board_so_far, m))
        }
    }

    /// 河牌摊牌净收益（全 5 张已知）：`+m / 0 / −m`。
    fn showdown_net(&self, m: f64) -> f64 {
        debug_assert_eq!(self.board.len(), 5, "showdown_net 需全 5 张 board");
        let board5 = [
            self.board[0],
            self.board[1],
            self.board[2],
            self.board[3],
            self.board[4],
        ];
        showdown_net(self.our_hole, self.opp_hole, &board5, m)
    }

    fn runout_ev(&self, board_so_far: &[Card], m: f64) -> f64 {
        runout_ev(self.our_hole, self.opp_hole, board_so_far, m)
    }
}

/// 河牌摊牌净收益（全 5 张已知）：我方胜 `+m` / 负 `−m` / 平 `0`。
pub fn showdown_net(our: [Card; 2], opp: [Card; 2], board: &[Card; 5], m: f64) -> f64 {
    let oh = eval7(&[
        our[0], our[1], board[0], board[1], board[2], board[3], board[4],
    ]);
    let ph = eval7(&[
        opp[0], opp[1], board[0], board[1], board[2], board[3], board[4],
    ]);
    match oh.cmp(&ph) {
        std::cmp::Ordering::Greater => m,
        std::cmp::Ordering::Less => -m,
        std::cmp::Ordering::Equal => 0.0,
    }
}

/// `E_runout[U] = m·(2·eq − 1)`，`eq = (wins + 0.5·ties)/total`，对剩余 board 枚举全补全
/// （扣双方底牌 + `board_so_far`），逐补全 eval7 比大小。HU 无 side pot，已证逐 completion
/// == GameState compute_payouts（含 all-in-for-less，见 `tests/aivat_nlhe_runout.rs`）。
pub fn runout_ev(our: [Card; 2], opp: [Card; 2], board_so_far: &[Card], m: f64) -> f64 {
    let n_new = 5 - board_so_far.len();
    let deck = remaining_deck(&our, &opp, board_so_far);
    let mut full: [Card; 5] = [board_so_far.first().copied().unwrap_or(our[0]); 5];
    full[..board_so_far.len()].copy_from_slice(board_so_far);
    let prefix = board_so_far.len();
    let mut wins: u64 = 0;
    let mut ties: u64 = 0;
    let mut total: u64 = 0;
    enumerate_combinations(&deck, n_new, &mut |combo| {
        full[prefix..].copy_from_slice(combo);
        let oh = eval7(&[our[0], our[1], full[0], full[1], full[2], full[3], full[4]]);
        let ph = eval7(&[opp[0], opp[1], full[0], full[1], full[2], full[3], full[4]]);
        match oh.cmp(&ph) {
            std::cmp::Ordering::Greater => wins += 1,
            std::cmp::Ordering::Equal => ties += 1,
            std::cmp::Ordering::Less => {}
        }
        total += 1;
    });
    let eq = (wins as f64 + 0.5 * ties as f64) / total as f64;
    m * (2.0 * eq - 1.0)
}

/// 52 张里扣掉 `holes`（两副 2 张）+ `board` 后剩余的牌。
fn remaining_deck(our: &[Card; 2], opp: &[Card; 2], board: &[Card]) -> Vec<Card> {
    let mut used = [false; 52];
    for c in our.iter().chain(opp.iter()).chain(board.iter()) {
        used[c.to_u8() as usize] = true;
    }
    (0u8..52)
        .filter(|&i| !used[i as usize])
        .map(|i| Card::from_u8(i).unwrap())
        .collect()
}

/// 枚举 `deck` 的全部 `C(deck.len(), k)` 组合，逐个回调（复用 `combo` 缓冲，零分配/组合）。
fn enumerate_combinations(deck: &[Card], k: usize, f: &mut dyn FnMut(&[Card])) {
    let n = deck.len();
    if k == 0 {
        f(&[]);
        return;
    }
    if k > n {
        return;
    }
    let mut idx: Vec<usize> = (0..k).collect();
    let mut combo: Vec<Card> = idx.iter().map(|&i| deck[i]).collect();
    loop {
        f(&combo);
        // 下一组合（字典序）：找最右可增位。
        let mut i = k;
        loop {
            if i == 0 {
                return;
            }
            i -= 1;
            if idx[i] != i + n - k {
                break;
            }
        }
        idx[i] += 1;
        combo[i] = deck[idx[i]];
        for j in (i + 1)..k {
            idx[j] = idx[j - 1] + 1;
            combo[j] = deck[idx[j]];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> Card {
        crate::training::nlhe_replay::parse_card(s).unwrap()
    }

    #[test]
    fn combinations_count_matches_binomial() {
        let deck: Vec<Card> = (0u8..10).map(|i| Card::from_u8(i).unwrap()).collect();
        for k in 0..=5 {
            let mut cnt = 0u64;
            let mut seen = std::collections::HashSet::new();
            enumerate_combinations(&deck, k, &mut |combo| {
                cnt += 1;
                // 严格递增（无重复、有序）。
                for w in combo.windows(2) {
                    assert!(w[0].to_u8() < w[1].to_u8());
                }
                seen.insert(combo.iter().map(|c| c.to_u8()).collect::<Vec<_>>());
            });
            let expect = binom(10, k as u64);
            assert_eq!(cnt, expect, "C(10,{k})");
            assert_eq!(seen.len() as u64, expect, "distinct C(10,{k})");
        }
    }

    fn binom(n: u64, k: u64) -> u64 {
        if k > n {
            return 0;
        }
        let mut r = 1u64;
        for i in 0..k {
            r = r * (n - i) / (i + 1);
        }
        r
    }

    #[test]
    fn remaining_deck_excludes_used() {
        let our = [c("As"), c("Ks")];
        let opp = [c("Qs"), c("Js")];
        let board = [c("Ts"), c("9s"), c("8s")];
        let deck = remaining_deck(&our, &opp, &board);
        assert_eq!(deck.len(), 52 - 7);
        for u in our.iter().chain(opp.iter()).chain(board.iter()) {
            assert!(!deck.contains(u));
        }
    }
}
