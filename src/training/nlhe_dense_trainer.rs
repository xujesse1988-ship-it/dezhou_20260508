//! `DenseNlheEsMccfrTrainer`（`docs/temp/nlhe_dense_infoset_table_plan_2026_05_26.md`
//! Phase 2）：把 [`crate::training::trainer::EsMccfrTrainer`] 的简化 NLHE 单线程
//! ES-MCCFR recurse 复制一份，热路径存储从 `HashMap<InfoSetId, Vec<f64>>` 换成
//! [`DenseNlheTable`]（变长 dense 扁平数组）。
//!
//! **为什么复制而非泛型化**（plan §Trainer 集成路线）：第一版不把 `EsMccfrTrainer<G>`
//! 泛型化成 storage trait——那会动到 Kuhn / Leduc / generic trainer 的签名与既有
//! 测试。低风险路线是新增本 NLHE 专用 trainer，稳定后再评估抽 `RegretStorage` trait。
//!
//! **byte-equal 来源**：与 HashMap 路径在同 seed 下逐位相等，靠确定性 lockstep——
//! - `step` 的 traverser、`root(rng)` 发牌、recurse 结构、`sample_discrete` 采样全
//!   与 [`crate::training::trainer::EsMccfrTrainer::step`] 一致 → 消费 rng 完全一致 →
//!   同一 sampled trajectory。
//! - 每个 infoset 的 regret / strategy_sum 累积 f64 序列一致（dense 表的 accumulate /
//!   current_strategy / average_strategy 已在 Phase 1 验证与 `regret.rs` byte-equal）。
//! - 归纳：update 0 两表皆空 → σ 全 uniform → 同采样；第 t 步前两路径每个 infoset 值
//!   逐位相等 → σ 逐位相等 → 同采样 → 同 trajectory → 第 t 步后仍逐位相等。
//!
//! **未做**（后续 Phase）：并行 `step_parallel`（Phase 3）、checkpoint v3（Phase 4）。
//! 本 trainer 不实现 [`crate::training::Trainer`] trait（trait 的 checkpoint 方法属
//! Phase 4），只提供 inherent `step` / 查询 / 诊断入口。

use std::sync::Arc;

use smallvec::SmallVec;

use crate::abstraction::info::{InfoSetId, StreetTag};
use crate::core::rng::RngSource;
use crate::error::TrainerError;
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::nlhe::{SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheState};
use crate::training::nlhe_dense::{DenseNlheTable, NlheDenseIndexer};
use crate::training::regret::SigmaVec;
use crate::training::sampling::{derive_substream_seed, sample_discrete};

/// 简化 NLHE dense ES-MCCFR trainer（Phase 2 单线程原型）。
///
/// `regret` / `strategy_sum` 共享同一 [`NlheDenseIndexer`]（同一棵 betting tree 的
/// slot 布局），各持一张 [`DenseNlheTable`] full dense 值表。LCFR period rescale 语义
/// 与 [`crate::training::trainer::EsMccfrTrainer`] 一致。
pub struct DenseNlheEsMccfrTrainer {
    game: SimplifiedNlheGame,
    regret: DenseNlheTable,
    strategy_sum: DenseNlheTable,
    update_count: u64,
    /// 仅用于 Phase 4 checkpoint 序列化存档（step 本身不消费——randomness 来自
    /// `step` 传入的 `rng`）。与 `EsMccfrTrainer.rng_substream_seed` 同义。
    #[allow(dead_code)]
    rng_substream_seed: [u8; 32],
    lcfr_period_size: Option<u64>,
    lcfr_periods_completed: u64,
    lcfr_rescale_regret: bool,
}

impl DenseNlheEsMccfrTrainer {
    /// 新建空 trainer。从 `game` 的 betting tree + bucket table 每街 bucket 数构建
    /// dense indexer，一次性分配两张 full dense 表（目标 profile ~13.5 GiB / 当前
    /// 119.7M profile ~4.6 GiB）。
    ///
    /// `master_seed` 仅派生 `rng_substream_seed` 占位（同 `EsMccfrTrainer::new`）；
    /// step 的 randomness 全部来自 `step` 传入的 `rng`。
    pub fn new(game: SimplifiedNlheGame, master_seed: u64) -> Self {
        let indexer = Arc::new(NlheDenseIndexer::from_tree(
            game.tree(),
            bucket_count_by_street(&game),
        ));
        let regret = DenseNlheTable::new(Arc::clone(&indexer));
        let strategy_sum = DenseNlheTable::new(indexer);
        Self {
            game,
            regret,
            strategy_sum,
            update_count: 0,
            rng_substream_seed: derive_substream_seed(master_seed, 0, 0),
            lcfr_period_size: None,
            lcfr_periods_completed: 0,
            lcfr_rescale_regret: true,
        }
    }

    /// 启用 LCFR-MCCFR period rescale（Brown & Sandholm 2018）；语义同
    /// [`crate::training::trainer::EsMccfrTrainer::with_lcfr_period`]。必须在
    /// `update_count == 0` 时调用。
    pub fn with_lcfr_period(mut self, period_size: u64) -> Self {
        assert!(
            period_size > 0,
            "LCFR period_size must be > 0 (got 0); 不调用本方法 = vanilla ES-MCCFR"
        );
        assert_eq!(
            self.update_count, 0,
            "LCFR period_size must be configured before any step (update_count = {} != 0)",
            self.update_count
        );
        self.lcfr_period_size = Some(period_size);
        self
    }

    /// 仅对 strategy_sum 做 LCFR rescale（regret 不动）；对照实验入口，语义同
    /// [`crate::training::trainer::EsMccfrTrainer::with_lcfr_period_strategy_only`]。
    pub fn with_lcfr_period_strategy_only(mut self, period_size: u64) -> Self {
        assert!(
            period_size > 0,
            "LCFR period_size must be > 0 (got 0); 不调用本方法 = vanilla ES-MCCFR"
        );
        assert_eq!(
            self.update_count, 0,
            "LCFR period_size must be configured before any step (update_count = {} != 0)",
            self.update_count
        );
        self.lcfr_period_size = Some(period_size);
        self.lcfr_rescale_regret = false;
        self
    }

    /// 执行 1 个 update（D-307 alternating traverser = `update_count % n_players`）。
    ///
    /// 与 [`crate::training::trainer::EsMccfrTrainer::step`] 逐步对应，仅存储后端不同。
    pub fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        let n_players = self.game.n_players() as u64;
        let traverser = (self.update_count % n_players) as PlayerId;
        let root = self.game.root(rng);
        recurse_es_dense(
            root,
            traverser,
            1.0,
            &mut self.regret,
            &mut self.strategy_sum,
            rng,
        );
        self.update_count += 1;
        self.maybe_lcfr_rescale();
        Ok(())
    }

    /// LCFR period boundary rescale（`step` 末调用）；语义同
    /// [`crate::training::trainer::EsMccfrTrainer::maybe_lcfr_rescale`]。
    fn maybe_lcfr_rescale(&mut self) {
        let Some(period_size) = self.lcfr_period_size else {
            return;
        };
        let target = self.update_count / period_size;
        while self.lcfr_periods_completed < target {
            let n = self.lcfr_periods_completed + 1;
            let factor = (n as f64) / ((n + 1) as f64);
            if self.lcfr_rescale_regret {
                self.regret.rescale_all(factor);
            }
            self.strategy_sum.rescale_all(factor);
            self.lcfr_periods_completed = n;
        }
    }

    /// current strategy（regret matching）。与 HashMap
    /// [`crate::training::Trainer::current_strategy`] 同语义：两表都没碰过该 infoset
    /// → 空 `Vec`；否则走 regret matching（退化均匀分布）。
    pub fn current_strategy(&self, info: InfoSetId) -> Vec<f64> {
        let row = self.regret.indexer().locate(info).row_index;
        if self.regret.touched_row(row) || self.strategy_sum.touched_row(row) {
            self.regret.current_strategy_by_info(info)
        } else {
            Vec::new()
        }
    }

    /// average strategy（strategy_sum 归一化）。与 HashMap
    /// [`crate::training::Trainer::average_strategy`] 同语义：两表都没碰过 → 空 `Vec`；
    /// 否则归一化（sum 为 0 退化均匀分布）。
    pub fn average_strategy(&self, info: InfoSetId) -> Vec<f64> {
        let row = self.strategy_sum.indexer().locate(info).row_index;
        if self.strategy_sum.touched_row(row) || self.regret.touched_row(row) {
            self.strategy_sum.average_strategy_by_info(info)
        } else {
            Vec::new()
        }
    }

    /// 已完成 update 数。
    pub fn update_count(&self) -> u64 {
        self.update_count
    }

    /// regret 表只读访问（诊断 / 测试）。
    pub fn regret_table(&self) -> &DenseNlheTable {
        &self.regret
    }

    /// strategy_sum 表只读访问（诊断 / 测试）。
    pub fn strategy_sum(&self) -> &DenseNlheTable {
        &self.strategy_sum
    }

    /// 持有的 game 只读访问（诊断 / 测试：走 tree 定位 spot 的 node_id）。
    pub fn game(&self) -> &SimplifiedNlheGame {
        &self.game
    }
}

/// 每街 bucket 数 `[preflop, flop, turn, river]`（indexer `from_tree` 入参）。
/// preflop 固定 169 lossless；postflop 从 bucket table config 读（v3 = 500/500/500）。
fn bucket_count_by_street(game: &SimplifiedNlheGame) -> [u32; 4] {
    let bt = &game.bucket_table;
    [
        bt.bucket_count(StreetTag::Preflop),
        bt.bucket_count(StreetTag::Flop),
        bt.bucket_count(StreetTag::Turn),
        bt.bucket_count(StreetTag::River),
    ]
}

/// 简化 NLHE 单线程 ES-MCCFR DFS recurse（dense 存储版）。
///
/// 与 [`crate::training::trainer`] 私有的 `recurse_es`（HashMap 版）逐行对应，差别
/// 只在 σ 读 / regret 累积 / strategy_sum 累积换成 [`DenseNlheTable`] 入口。NLHE 无
/// in-game chance node（D-308 / D-315 chance 在 root 一次性消费），`Chance` 分支
/// unreachable。
///
/// 返回值语义（D-301）：terminal = `payoff`；traverser decision = `Σ σ(a)·v_a`；
/// non-traverser decision = sampled action 路径的 recursed value。
fn recurse_es_dense(
    state: SimplifiedNlheState,
    traverser: PlayerId,
    pi_trav: f64,
    regret: &mut DenseNlheTable,
    strategy_sum: &mut DenseNlheTable,
    rng: &mut dyn RngSource,
) -> f64 {
    match SimplifiedNlheGame::current(&state) {
        NodeKind::Terminal => SimplifiedNlheGame::payoff(&state, traverser),
        NodeKind::Chance => unreachable!(
            "简化 NLHE 无 in-game chance node（randomness 全在 Game::root 消费）；\
             current(state) 不应返回 Chance"
        ),
        NodeKind::Player(actor) => {
            // InfoSetId 是 Copy，沿用 HashMap 版的访问顺序：先 info_set / legal_actions，
            // 再读 σ（dense full prealloc 无需 get_or_init）。
            let info = SimplifiedNlheGame::info_set(&state, actor);
            let actions = SimplifiedNlheGame::legal_actions(&state);
            let n = actions.len();
            let sigma = regret.current_strategy_smallvec_by_info(info);

            if actor == traverser {
                // traverser node：先按 traverser 自身 reach 累积 average strategy
                // （顺序与 recurse_es 一致：strategy_sum 先于 regret）。
                let weighted: SigmaVec = sigma.iter().map(|s| pi_trav * s).collect();
                strategy_sum.accumulate_by_info(info, &weighted);

                let mut cfvs: SigmaVec = SigmaVec::with_capacity(n);
                for (i, action) in actions.iter().enumerate() {
                    let next_state = SimplifiedNlheGame::next(state.clone(), *action, rng);
                    let cfv = recurse_es_dense(
                        next_state,
                        traverser,
                        pi_trav * sigma[i],
                        regret,
                        strategy_sum,
                        rng,
                    );
                    cfvs.push(cfv);
                }
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                // External sampling：opponent/chance reach 由 visitation 概率隐式提供，
                // per-visit delta 不再乘 pi_opp（与 recurse_es 完全一致）。
                let delta: SigmaVec = cfvs.iter().map(|c| c - sigma_value).collect();
                regret.accumulate_by_info(info, &delta);
                sigma_value
            } else {
                // opponent node：按 σ 采样 1 action（剔除零概率 outcome；D-309 / D-337）。
                let nonzero_dist: SmallVec<[(SimplifiedNlheAction, f64); 8]> = actions
                    .iter()
                    .copied()
                    .zip(sigma.iter().copied())
                    .filter(|(_, p)| *p > 0.0)
                    .collect();
                debug_assert!(
                    !nonzero_dist.is_empty(),
                    "non-traverser σ all-zero impossible: current_strategy 退化局面回退均匀分布"
                );
                let sampled = sample_discrete(&nonzero_dist, rng);
                let next_state = SimplifiedNlheGame::next(state, sampled, rng);
                recurse_es_dense(next_state, traverser, pi_trav, regret, strategy_sum, rng)
            }
        }
    }
}
