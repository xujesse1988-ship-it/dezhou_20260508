//! `LbrEvaluator` Rust 自实现（API-450..API-457 / D-450..D-459）。
//!
//! Local Best Response（Lisý & Bowling 2017）作为 blueprint exploitability 上界
//! 评估器；stage 4 验收四锚点之一（D-450 字面 LBR < 200 mbb/g first usable）。
//!
//! **A1 \[实现\] 状态**：[`LbrEvaluator`] / [`LbrResult`] / [`SixTraverserLbrResult`]
//! struct 签名锁；`new` / `compute` / `compute_six_traverser_average` /
//! `export_policy_for_openspiel` 全 `unimplemented!()`，E2 \[实现\] 落地（D-453
//! Rust 自实现 + D-456 14-action / 5-action ablation 双路径 + D-457 OpenSpiel
//! one-shot sanity export）。
//!
//! **作用域**：stage 4 only — Kuhn / Leduc / SimplifiedNlheGame 路径上 LBR 没
//! 阈值断言（验收锚点是 closed-form `-1/18` for Kuhn、内部 `expl < 0.1`
//! threshold for Leduc，stage 3 既有 [`crate::BestResponse`] trait 覆盖；
//! stage 4 LBR 仅用于 `NlheGame6` 14-action ablation baseline）。

use std::path::Path;
use std::sync::Arc;

use crate::core::rng::RngSource;
use crate::error::TrainerError;
use crate::training::game::{Game, PlayerId};
use crate::training::trainer::EsMccfrTrainer;

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

/// stage 4 D-450 / D-453 LBR Evaluator（Rust 自实现）。
///
/// `trainer` 通过 `Arc` 持有，避免 LBR computation 期间 trainer 被独占（多个
/// `LbrEvaluator` 实例可对同一 blueprint 并行 evaluate 不同 traverser）。
/// `action_set_size` ∈ {5, 14} 对应 stage 3 SimplifiedNlheGame 5-action /
/// stage 4 NlheGame6 14-action（D-456 字面）。`myopic_horizon = 1` 是 D-455
/// lock（LBR 视角下不展开第 2 决策点；高 horizon 会让 LBR 上界变紧但增长
/// 训练评测的 wall time 指数级）。
#[allow(dead_code)]
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
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，E2 \[实现\] 落地。
    pub fn new(
        trainer: Arc<EsMccfrTrainer<G>>,
        action_set_size: usize,
        myopic_horizon: u8,
    ) -> Result<Self, TrainerError> {
        let _ = (trainer, action_set_size, myopic_horizon);
        unimplemented!("stage 4 A1 [实现] scaffold: LbrEvaluator::new 落地 E2 [实现] D-450 / D-456")
    }

    /// stage 4 D-452 — 对一个 LBR-player 在 `n_hands`（通常 1000，D-452）上计算
    /// LBR 上界 mbb/g。
    ///
    /// `lbr_player` ∈ `[0, n_players)`；`rng` 显式注入（D-027 / D-050 字面继承）。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，E2 \[实现\] 落地走
    /// myopic horizon=1 best response enumerate（每决策点 14-action 全枚举
    /// + 取 max EV）。
    pub fn compute(
        &self,
        lbr_player: PlayerId,
        n_hands: u64,
        rng: &mut dyn RngSource,
    ) -> Result<LbrResult, TrainerError> {
        let _ = (lbr_player, n_hands, rng);
        unimplemented!("stage 4 A1 [实现] scaffold: LbrEvaluator::compute 落地 E2 [实现] D-452")
    }

    /// stage 4 D-459 — 6-traverser average LBR（D-414 6 traverser 独立 RegretTable
    /// 数组的 cross-traverser 评测入口）。
    ///
    /// 内部对 6 个 traverser 调用 [`Self::compute`]，输出 per-traverser × 6 +
    /// average + max + min（D-459 字面 §carve-out 锚点）。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，E2 \[实现\] 落地。
    pub fn compute_six_traverser_average(
        &self,
        n_hands_per_traverser: u64,
        rng: &mut dyn RngSource,
    ) -> Result<SixTraverserLbrResult, TrainerError> {
        let _ = (n_hands_per_traverser, rng);
        unimplemented!(
            "stage 4 A1 [实现] scaffold: LbrEvaluator::compute_six_traverser_average 落地 E2 [实现] D-459"
        )
    }

    /// stage 4 D-457 — F3 \[报告\] 一次性接入 OpenSpiel
    /// `algorithms/exploitability_descent.py` 对照（< 10% 容差 sanity）。
    ///
    /// 输出 OpenSpiel-compatible policy 文件到 `path`，由 Python script 消费。
    /// stage 3 `tools/external_cfr_compare.py` 同型 one-shot instrumentation
    /// 形态。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，F2 / F3 \[报告\]
    /// 落地。
    pub fn export_policy_for_openspiel(&self, path: &Path) -> Result<(), TrainerError> {
        let _ = path;
        unimplemented!(
            "stage 4 A1 [实现] scaffold: LbrEvaluator::export_policy_for_openspiel 落地 F2 / F3 [报告] D-457"
        )
    }
}
