//! `LbrEvaluator` Rust 自实现（API-450..API-457 / D-450..D-459）。
//!
//! Local Best Response（Lisý & Bowling 2017）作为 blueprint exploitability 上界
//! 评估器；stage 4 验收四锚点之一（D-450 字面 LBR < 200 mbb/g first usable）。
//!
//! **E2 \[实现\] 状态**（2026-05-15）：4 method 全部落地，走 myopic horizon=1
//! best response enumerate × n_hands sampled hands × 6 traverser independent
//! 路径（D-450 / D-452 / D-455 / D-456 / D-459）。
//!
//! **算法核心**（D-450 字面）：
//! 1. 选 1 个 LBR-player（traverser）；
//! 2. 在 1000 个 sampled hand 上跑 game tree：
//!    - LBR-player 第 1 个决策点：枚举 `action_set_size` 个 candidate action
//!      （14-action 主线 D-456 / 5-action ablation），对每个 candidate clone
//!      state + apply + 后续 path 走 blueprint sample 一次得到 EV 估计；
//!    - 其它决策点：opponents 走 blueprint，LBR-player 后续决策点也走 blueprint
//!      （myopic horizon=1，D-455 字面）；
//! 3. 取 max EV candidate 作为 LBR best response，记录 payoff。
//! 4. average over 1000 hand 得 LBR mbb/g（D-461 单位：big-blind=100 chip）。
//!
//! **作用域**：stage 4 only — Kuhn / Leduc / SimplifiedNlheGame 路径上 LBR 没
//! 阈值断言（验收锚点是 closed-form `-1/18` for Kuhn、内部 `expl < 0.1`
//! threshold for Leduc，stage 3 既有 [`crate::BestResponse`] trait 覆盖；
//! stage 4 LBR 仅用于 `NlheGame6` 14-action ablation baseline）。

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use crate::core::rng::RngSource;
use crate::error::TrainerError;
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::sampling::sample_discrete;
use crate::training::trainer::{EsMccfrTrainer, Trainer as _};

/// stage 4 D-450 / API-451 — 单 traverser LBR computation 结果（mbb/g 单位）。
#[derive(Clone, Debug)]
pub struct LbrResult {
    pub lbr_player: PlayerId,
    /// LBR upper bound（mbb/g 单位；越小越接近 Nash）。
    pub lbr_value_mbbg: f64,
    pub standard_error_mbbg: f64,
    pub n_hands: u64,
    pub computation_seconds: f64,
}

/// stage 4 D-459 / API-451 — 6-traverser LBR computation 结果（per-traverser
/// min/max/average 锚点）。
///
/// D-459 字面 6-traverser **每个独立通过门槛**（避免 1 traverser 优秀 + 5
/// traverser fail 的虚假通过）；F3 \[报告\] 输出 6 个 per_traverser 字面 +
/// `max_mbbg` carve-out 锚点。
#[derive(Clone, Debug)]
pub struct SixTraverserLbrResult {
    pub per_traverser: [LbrResult; 6],
    pub average_mbbg: f64,
    /// 6-traverser 最大 LBR（D-459 §carve-out 锚点 — 单 traverser fail 触发
    /// D-459-revM 翻面）。
    pub max_mbbg: f64,
    pub min_mbbg: f64,
}

/// stage 4 D-456 5-action ablation 字面集（与 stage 3 `DefaultActionAbstraction`
/// 同型 fold/check/call/raise_1pot/all-in 5 元集）。
const ACTION_SET_SIZE_FIVE: usize = 5;
const ACTION_SET_SIZE_FOURTEEN: usize = 14;

/// stage 4 D-455 myopic horizon main path lock。
const HORIZON_MYOPIC: u8 = 1;
/// stage 4 D-455 horizon=0 边界（pure blueprint self-play 退化）。
const HORIZON_ZERO: u8 = 0;

/// stage 4 D-461 字面 big-blind = 100 chip（继承 stage 1 D-022 `TableConfig::
/// default_6max_100bb`）。LBR `value_in_chips → mbb/g` 转换走
/// `mbbg = chips / bb * 1000 = chips × 10`。
const BB_CHIPS: f64 = 100.0;
const MBBG_PER_BB: f64 = 1000.0;

/// stage 4 D-450 / D-453 LBR Evaluator（Rust 自实现）。
///
/// `trainer` 通过 `Arc` 持有，避免 LBR computation 期间 trainer 被独占（多个
/// `LbrEvaluator` 实例可对同一 blueprint 并行 evaluate 不同 traverser）。
/// `action_set_size` ∈ {5, 14} 对应 stage 3 SimplifiedNlheGame 5-action /
/// stage 4 NlheGame6 14-action（D-456 字面）。`myopic_horizon = 1` 是 D-455
/// lock（LBR 视角下不展开第 2 决策点；高 horizon 会让 LBR 上界变紧但增长
/// 训练评测的 wall time 指数级）。
pub struct LbrEvaluator<G: Game> {
    pub(crate) trainer: Arc<EsMccfrTrainer<G>>,
    pub(crate) action_set_size: usize,
    pub(crate) myopic_horizon: u8,
}

impl<G: Game> LbrEvaluator<G> {
    /// stage 4 D-450 / D-456 — 构造（拒绝 `action_set_size` 不在 {5, 14} 范围）。
    ///
    /// 失败路径：[`TrainerError::PreflopActionAbstractionMismatch`]（D-456 字面）。
    ///
    /// `myopic_horizon` 主路径 = 1（D-455 字面 lock）；horizon=0 退化为 pure
    /// blueprint self-play（[`Self::compute`] 不做 best-response 枚举直接 sample
    /// → LBR ≈ 0 mbb/g 零和游戏）；horizon ≥ 2 主路径不支持，本 ctor 接受
    /// 但 [`Self::compute`] 触达时 `unimplemented!()` panic（D-453-revM
    /// 主路径外 deferred）。
    pub fn new(
        trainer: Arc<EsMccfrTrainer<G>>,
        action_set_size: usize,
        myopic_horizon: u8,
    ) -> Result<Self, TrainerError> {
        if action_set_size != ACTION_SET_SIZE_FIVE && action_set_size != ACTION_SET_SIZE_FOURTEEN {
            return Err(TrainerError::PreflopActionAbstractionMismatch);
        }
        Ok(Self {
            trainer,
            action_set_size,
            myopic_horizon,
        })
    }

    /// stage 4 D-452 — 对一个 LBR-player 在 `n_hands`（通常 1000，D-452）上计算
    /// LBR 上界 mbb/g。
    ///
    /// `lbr_player` ∈ `[0, n_players)`；`rng` 显式注入（D-027 / D-050 字面继承）。
    ///
    /// 算法（D-450 字面）：每 hand 上初始化游戏 → DFS 走到 LBR-player 第一个
    /// 决策点 → 枚举 `action_set_size` 个 candidate action → 每 candidate
    /// 走 myopic playout（candidate 之后所有决策点走 blueprint sample）→ 取
    /// max EV candidate → 继续到 terminal 取 payoff。
    pub fn compute(
        &self,
        lbr_player: PlayerId,
        n_hands: u64,
        rng: &mut dyn RngSource,
    ) -> Result<LbrResult, TrainerError> {
        // D-455 horizon ≥ 2 主路径外 deferred（D-453-revM 候选）。
        if self.myopic_horizon > HORIZON_MYOPIC {
            unimplemented!(
                "LbrEvaluator::compute myopic_horizon > 1 deferred 到 D-453-revM（D-455 lock）"
            );
        }
        let n_players = self.trainer.game.n_players();
        if (lbr_player as usize) >= n_players {
            return Err(TrainerError::PreflopActionAbstractionMismatch);
        }

        let start = std::time::Instant::now();
        let mut sum_payoff = 0.0_f64;
        let mut sum_sq = 0.0_f64;
        let n = n_hands.max(1);
        for _ in 0..n {
            let payoff = self.simulate_one_hand(lbr_player, rng);
            sum_payoff += payoff;
            sum_sq += payoff * payoff;
        }
        let mean_chips = sum_payoff / n as f64;
        let variance = (sum_sq / n as f64) - mean_chips * mean_chips;
        let se_chips = (variance.max(0.0) / n as f64).sqrt();

        // chips → mbb/g（D-461 字面 bb=100 chip / mbb/g = chips * 10）。
        let lbr_value_mbbg = mean_chips / BB_CHIPS * MBBG_PER_BB;
        let standard_error_mbbg = se_chips / BB_CHIPS * MBBG_PER_BB;
        let computation_seconds = start.elapsed().as_secs_f64();

        Ok(LbrResult {
            lbr_player,
            lbr_value_mbbg,
            standard_error_mbbg,
            n_hands,
            computation_seconds,
        })
    }

    /// stage 4 D-459 — 6-traverser average LBR（D-414 6 traverser 独立 RegretTable
    /// 数组的 cross-traverser 评测入口）。
    ///
    /// 内部对 6 个 traverser 调用 [`Self::compute`]，输出 per-traverser × 6 +
    /// average + max + min（D-459 字面 §carve-out 锚点）。
    ///
    /// **HU 退化 / `n_players < 6` 兼容**：当 trainer 配 `NlheGame6::new_hu`
    /// （`n_players=2`）时，per_traverser slot 索引 `i % n_players` 让结果数组
    /// 仍输出 6 个 entry（重复采样 traverser）。D-459 字面主路径只在
    /// `n_players=6` 上消费；HU 退化路径仅作为 stage 3 anchor 桥接，不进入
    /// stage 4 F3 \[报告\] 验收口径。
    pub fn compute_six_traverser_average(
        &self,
        n_hands_per_traverser: u64,
        rng: &mut dyn RngSource,
    ) -> Result<SixTraverserLbrResult, TrainerError> {
        let n_players = self.trainer.game.n_players().max(1);
        let mut results: Vec<LbrResult> = Vec::with_capacity(6);
        for i in 0..6 {
            let traverser = (i % n_players) as PlayerId;
            let mut r = self.compute(traverser, n_hands_per_traverser, rng)?;
            // D-459 字面 per_traverser[i].lbr_player = i（顺序排列），便于
            // F3 \[报告\] 按 seat index 对齐 LBR 输出。
            r.lbr_player = i as PlayerId;
            results.push(r);
        }
        let per_traverser: [LbrResult; 6] = results.try_into().map_err(|_| {
            // 不可达：上述循环明确 push 6 次。保留 fallback 让 `try_into`
            // 不直接 `.expect()` panic。
            TrainerError::PreflopActionAbstractionMismatch
        })?;
        let mut max_v = f64::NEG_INFINITY;
        let mut min_v = f64::INFINITY;
        let mut sum_v = 0.0_f64;
        for r in &per_traverser {
            max_v = max_v.max(r.lbr_value_mbbg);
            min_v = min_v.min(r.lbr_value_mbbg);
            sum_v += r.lbr_value_mbbg;
        }
        let average_mbbg = sum_v / 6.0;
        Ok(SixTraverserLbrResult {
            per_traverser,
            average_mbbg,
            max_mbbg: max_v,
            min_mbbg: min_v,
        })
    }

    /// stage 4 D-457 — F3 \[报告\] 一次性接入 OpenSpiel
    /// `algorithms/exploitability_descent.py` 对照（< 10% 容差 sanity）。
    ///
    /// 输出 OpenSpiel-compatible policy 文件到 `path`，由 Python script 消费。
    /// stage 3 `tools/external_cfr_compare.py` 同型 one-shot instrumentation
    /// 形态。
    ///
    /// **格式**：text JSON（line-delimited）— 每行 `{"traverser": t, "info_set":
    /// "<Debug>", "average_strategy": [p_0, p_1, ...]}`，traverser 升序 ×
    /// 每 traverser 内 InfoSet `Debug` 排序保跨 host byte-equal（D-457 字面 +
    /// 继承 stage 3 D-327 encode_table 同型 sort 规则）。空 InfoSet（trainer
    /// 未访问）不输出。
    pub fn export_policy_for_openspiel(&self, path: &Path) -> Result<(), TrainerError> {
        let mut file = std::fs::File::create(path).map_err(|e| {
            TrainerError::Checkpoint(crate::error::CheckpointError::Corrupted {
                offset: 0,
                reason: format!("LBR export: create file {path:?} failed: {e}"),
            })
        })?;

        let n_players = self.trainer.game.n_players();
        for traverser_idx in 0..n_players.min(6) {
            let traverser = traverser_idx as PlayerId;
            // 走 per_traverser → strategy_sum 数组（如激活），否则 fallback 到 single
            // shared strategy_sum；两种路径均通过 trainer 内部公共 inner() 读取。
            let entries = self.collect_policy_entries_for_traverser(traverser);
            for (info_dbg, avg_strategy) in entries {
                let strategy_json = avg_strategy
                    .iter()
                    .map(|p| format!("{p:.17e}"))
                    .collect::<Vec<_>>()
                    .join(",");
                // info_dbg 已经是 Debug-formatted string；JSON 字符串 escape
                // 走简化路径（替换 `"` 和 `\`，stage 4 InfoSet Debug 输出
                // 不含其它 control char）。
                let info_escaped = info_dbg.replace('\\', "\\\\").replace('"', "\\\"");
                writeln!(
                    file,
                    "{{\"traverser\":{traverser_idx},\"info_set\":\"{info_escaped}\",\
                     \"average_strategy\":[{strategy_json}]}}"
                )
                .map_err(|e| {
                    TrainerError::Checkpoint(crate::error::CheckpointError::Corrupted {
                        offset: 0,
                        reason: format!("LBR export: write file {path:?} failed: {e}"),
                    })
                })?;
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 内部实现（仅 LbrEvaluator inherent；非 pub trait surface）
    // -----------------------------------------------------------------------

    /// 简化：返回 strategy_sum 上每个 InfoSet 的 `(Debug, avg_σ)` 已 sort 后的
    /// 序列；当 trainer 走 per_traverser 路径时读对应 `tables.strategy_sum[t]`
    /// 否则 fallback 到 single shared `trainer.strategy_sum`。
    fn collect_policy_entries_for_traverser(&self, traverser: PlayerId) -> Vec<(String, Vec<f64>)> {
        // 通过 average_strategy_for_traverser 入口查询每个 InfoSet 的 average σ。
        // 为了让输出 deterministic 跨 host byte-equal，按 Debug 排序 InfoSet。
        let mut keys: Vec<&G::InfoSet> = Vec::new();
        let trainer = &*self.trainer;
        // 优先收集 trainer 当前 traverser 的 strategy_sum keys；当 per_traverser
        // 未激活时回退到 single-shared strategy_sum keys。
        let mut seen_pairs: Vec<(String, &G::InfoSet)> = Vec::new();

        // single-shared strategy_sum keys（pre-warmup 或非 NlheGame6 路径）。
        for k in trainer_strategy_sum_inner_keys(trainer) {
            let dbg = format!("{k:?}");
            seen_pairs.push((dbg, k));
        }
        // per_traverser 路径下额外 union keys（可能与 shared keys 重叠）。
        if let Some(t_keys) = trainer_per_traverser_strategy_keys(trainer, traverser) {
            for k in t_keys {
                let dbg = format!("{k:?}");
                if !seen_pairs.iter().any(|(d, _)| d == &dbg) {
                    seen_pairs.push((dbg, k));
                }
            }
        }
        seen_pairs.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, k) in &seen_pairs {
            keys.push(*k);
        }

        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            let avg = trainer.average_strategy_for_traverser(traverser, k);
            if avg.is_empty() {
                continue;
            }
            let dbg = format!("{k:?}");
            out.push((dbg, avg));
        }
        out
    }

    /// 单 hand simulation：从 root 开始 DFS，LBR-player 第一个决策点走 best
    /// response enumerate，其它决策点走 blueprint sample。返回 lbr_player 视角
    /// 的 chip 净收益（D-316 字面）。
    fn simulate_one_hand(&self, lbr_player: PlayerId, rng: &mut dyn RngSource) -> f64 {
        // horizon=0 字面：纯 blueprint self-play，LBR-player 不做 best-response
        // → 直接返回 blueprint sample payoff（零和游戏均值 ≈ 0）。
        let mut state = self.trainer.game.root(rng);
        let mut lbr_decision_done = self.myopic_horizon == HORIZON_ZERO;

        loop {
            match G::current(&state) {
                NodeKind::Terminal => {
                    return G::payoff(&state, lbr_player);
                }
                NodeKind::Chance => {
                    let dist = G::chance_distribution(&state);
                    let action = sample_discrete(&dist, rng);
                    state = G::next(state, action, rng);
                }
                NodeKind::Player(actor) => {
                    if actor == lbr_player && !lbr_decision_done {
                        // D-450 / D-456 — best-response enumerate over
                        // `action_set_size` candidates。每 candidate clone state
                        // + apply + 后续走 blueprint sample 取 EV 估计；选 max。
                        let candidates =
                            self.restrict_action_set(&G::legal_actions(&state), lbr_player);
                        if candidates.is_empty() {
                            // 防御：无 candidate（不可达，因 legal_actions 在
                            // decision node 上至少 1 元）。直接 blueprint sample。
                            let info = G::info_set(&state, actor);
                            let avg = self.trainer.average_strategy_for_traverser(actor, &info);
                            let actions = G::legal_actions(&state);
                            let action = sample_blueprint_action(&actions, &avg, rng);
                            state = G::next(state, action, rng);
                            lbr_decision_done = true;
                            continue;
                        }
                        let mut best_action = candidates[0];
                        let mut best_ev = f64::NEG_INFINITY;
                        for cand in candidates.iter().copied() {
                            let sub_state = G::next(state.clone(), cand, rng);
                            // playout：myopic horizon=1，LBR-player 后续走
                            // blueprint sample；opponents 也走 blueprint。
                            let ev = self.playout_blueprint(sub_state, lbr_player, rng);
                            if ev > best_ev {
                                best_ev = ev;
                                best_action = cand;
                            }
                        }
                        state = G::next(state, best_action, rng);
                        lbr_decision_done = true;
                    } else {
                        // blueprint sample（actor 走 average strategy）。
                        let info = G::info_set(&state, actor);
                        let avg = self.trainer.average_strategy_for_traverser(actor, &info);
                        let actions = G::legal_actions(&state);
                        let action = sample_blueprint_action(&actions, &avg, rng);
                        state = G::next(state, action, rng);
                    }
                }
            }
        }
    }

    /// 在已经 commit LBR-player's best response 之后，blueprint sample 走到
    /// terminal 取 LBR-player payoff（myopic horizon=1：LBR-player 后续也走
    /// blueprint）。
    fn playout_blueprint(
        &self,
        mut state: G::State,
        lbr_player: PlayerId,
        rng: &mut dyn RngSource,
    ) -> f64 {
        loop {
            match G::current(&state) {
                NodeKind::Terminal => return G::payoff(&state, lbr_player),
                NodeKind::Chance => {
                    let dist = G::chance_distribution(&state);
                    let action = sample_discrete(&dist, rng);
                    state = G::next(state, action, rng);
                }
                NodeKind::Player(actor) => {
                    let info = G::info_set(&state, actor);
                    let avg = self.trainer.average_strategy_for_traverser(actor, &info);
                    let actions = G::legal_actions(&state);
                    let action = sample_blueprint_action(&actions, &avg, rng);
                    state = G::next(state, action, rng);
                }
            }
        }
    }

    /// D-456 — 把 legal_actions 限制到 `action_set_size`（14 主线 / 5
    /// ablation）。5-action 路径在 14-action 集合中选首 5 个 (与 stage 3
    /// `DefaultActionAbstraction` 字面 fold/check/call/raise_1pot/all-in
    /// 5 元集语义对齐，避免引入跨 stage `Vec<G::Action>` 重映射)。
    ///
    /// 当 legal_actions 数量 < `action_set_size` 时直接返回全部 legal_actions
    /// （preflop / postflop 14-action 集合可能 < 14，比如已经全员 all-in，
    /// 仅剩 fold/check/call 等）。
    fn restrict_action_set(&self, legal: &[G::Action], _lbr_player: PlayerId) -> Vec<G::Action> {
        if legal.len() <= self.action_set_size {
            return legal.to_vec();
        }
        legal[..self.action_set_size].to_vec()
    }
}

/// 共享 helper — 取 trainer single-shared strategy_sum 的 InfoSet keys
/// 引用（policy export 走此入口）。
fn trainer_strategy_sum_inner_keys<G: Game>(trainer: &EsMccfrTrainer<G>) -> Vec<&G::InfoSet> {
    trainer.strategy_sum.inner().keys().collect()
}

/// 共享 helper — 取 trainer per_traverser `strategy_sum[traverser]` 的 InfoSet
/// keys 引用（policy export 走此入口；未激活时 `None`）。
fn trainer_per_traverser_strategy_keys<G: Game>(
    trainer: &EsMccfrTrainer<G>,
    traverser: PlayerId,
) -> Option<Vec<&G::InfoSet>> {
    let tables = trainer.per_traverser.as_ref()?;
    let idx = traverser as usize;
    if idx >= tables.strategy_sum.len() {
        return None;
    }
    Some(tables.strategy_sum[idx].inner().keys().collect())
}

/// 共享 helper — 按 blueprint average σ 采样 1 个 action（softmax-free / 直接
/// 走 cumulative distribution）。`avg` 空 / 长度不一致 / sum ≤ 0 → 均匀分布
/// fallback（D-331 退化局面字面继承）。
fn sample_blueprint_action<A: Copy>(actions: &[A], avg: &[f64], rng: &mut dyn RngSource) -> A {
    debug_assert!(
        !actions.is_empty(),
        "sample_blueprint_action: actions slice empty"
    );
    if avg.is_empty() || avg.len() != actions.len() {
        // fallback uniform
        let n = actions.len() as u64;
        let idx = (rng.next_u64() % n) as usize;
        return actions[idx];
    }
    // 过滤 zero-probability actions（API-331 sample_discrete 不变量）
    let nonzero: Vec<(A, f64)> = actions
        .iter()
        .copied()
        .zip(avg.iter().copied())
        .filter(|(_, p)| *p > 0.0)
        .collect();
    if nonzero.is_empty() {
        let n = actions.len() as u64;
        let idx = (rng.next_u64() % n) as usize;
        return actions[idx];
    }
    // 归一化以满足 sample_discrete 的 sum=1±1e-12 不变量（average_strategy
    // 在 short-history 期间可能轻微偏离 1）。
    let sum: f64 = nonzero.iter().map(|(_, p)| *p).sum();
    if sum <= 0.0 || !sum.is_finite() {
        let n = actions.len() as u64;
        let idx = (rng.next_u64() % n) as usize;
        return actions[idx];
    }
    let normalized: Vec<(A, f64)> = nonzero.iter().map(|(a, p)| (*a, p / sum)).collect();
    sample_discrete(&normalized, rng)
}
