//! `DenseNlheEsMccfrTrainer`（`docs/temp/nlhe_dense_infoset_table_plan_2026_05_26.md`
//! Phase 2）：把 [`crate::training::trainer::EsMccfrTrainer`] 的简化 NLHE 单线程
//! ES-MCCFR recurse 复制一份，热路径存储从 `HashMap<InfoSetId, Vec<f64>>` 换成
//! [`DenseNlheTable`]（变长 dense 扁平数组）。
//!
//! **为什么复制而非泛型化**（plan §Trainer 集成路线）：第一版不把 `EsMccfrTrainer<G>`
//! 泛型化成 storage trait——那会动到 Kuhn / Leduc / generic trainer 的签名与既有
//! 测试。低风险路线是新增本 NLHE 专用 trainer，稳定后再评估抽 `RegretStorage` trait。
//!
//! **vanilla byte-equal 来源**：与 HashMap 路径在同 seed 下逐位相等，靠确定性
//! lockstep——
//! - `step` 的 traverser、`root(rng)` 发牌、recurse 结构、`sample_discrete` 采样全
//!   与 [`crate::training::trainer::EsMccfrTrainer::step`] 一致 → 消费 rng 完全一致 →
//!   同一 sampled trajectory。
//! - 每个 infoset 的 regret / strategy_sum 累积 f64 序列一致（dense 表的 accumulate /
//!   current_strategy / average_strategy 已在 Phase 1 验证与 `regret.rs` byte-equal）。
//! - 归纳：update 0 两表皆空 → σ 全 uniform → 同采样；第 t 步前两路径每个 infoset 值
//!   逐位相等 → σ 逐位相等 → 同采样 → 同 trajectory → 第 t 步后仍逐位相等。
//!
//! LCFR 开启后 dense `rescale_all` 走 global lazy scale，不再复刻 HashMap eager
//! rescale 的逐 slot f64 运算顺序；策略语义仍等价，但 `to_bits` 不再作为跨后端合同。
//!
//! **Phase 3**（已落地）：[`DenseNlheEsMccfrTrainer::step_parallel`] 走
//! deterministic local-delta + merge（镜像 [`crate::training::trainer::EsMccfrTrainer::step_parallel`]，
//! vanilla 下与其 byte-equal）。**Phase 4**（已落地）：`save_checkpoint` / `load_checkpoint`
//! 走 dense raw v3（[`crate::training::nlhe_dense_checkpoint`]），`from_hashmap_checkpoint`
//! 单向加载旧 v2 HashMap ckpt。本 trainer **不实现** [`crate::training::Trainer`] trait
//! （它是泛型 `Trainer<G>`，dense 是 NLHE 专属；保持 inherent 方法避免耦合 Kuhn/Leduc
//! 泛型路径），只提供 inherent `step` / `step_parallel` / checkpoint / 查询 / 诊断入口。

use std::path::Path;
use std::sync::Arc;

use rayon::prelude::*;
use smallvec::SmallVec;

use crate::abstraction::info::{InfoSetId, StreetTag};
use crate::core::rng::RngSource;
use crate::error::{CheckpointError, GameVariant, TrainerError, TrainerVariant};
use crate::training::checkpoint::Checkpoint;
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::nlhe::{SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheState};
use crate::training::nlhe_dense::{DenseLocalDelta, DenseNlheTable, NlheDenseIndexer};
use crate::training::nlhe_dense_checkpoint::{
    load_dense_checkpoint, save_dense_checkpoint, DenseCheckpointMeta, DenseLayoutFingerprint,
};
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
    /// Phase 4 checkpoint 序列化存档（step 本身不消费——randomness 来自 `step` 传入的
    /// `rng`）。与 `EsMccfrTrainer.rng_substream_seed` 同义。
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

    /// 多线程并发 step（Phase 3 dense 并行路径）。
    ///
    /// 结构逐项镜像 [`crate::training::trainer::EsMccfrTrainer::step_parallel`]：一次
    /// 调用产 `n_active = min(n_threads, rng_pool.len())` × `batch_per_worker` 个
    /// update；每 worker 在 rayon 任务里跑 `batch_per_worker` 条 trajectory，σ 全程读
    /// **pre-dispatch shared dense regret**（[`DenseNlheTable::current_strategy_smallvec_at`]
    /// 只读借用，rayon 任务期内安全——主表本批不被任何 worker 写）；regret /
    /// strategy_sum 累积 push 到线程本地 [`DenseLocalDelta`]（slot-based）。dispatch
    /// 结束后 main thread 按 tid 升序 × 每 worker 内 push 顺序 playback merge 回主表。
    ///
    /// **确定性 / byte-equal**：plan §并行语义 默认路径 = deterministic local delta +
    /// merge，**不** atomic direct write / Hogwild。trajectory（rng 消费）、σ 读
    /// （pre-dispatch snapshot）、push 顺序（DFS）、merge 顺序（tid 升序 × push 顺序）
    /// 全 deterministic，且与 HashMap `step_parallel` 一一对应 →
    /// `(InfoSetId → slot)` 双射后每个 cell 的 f64 加法序列与 HashMap 路径完全一致，
    /// 两路径 strategy snapshot byte-equal（集成测试 anchor）。
    ///
    /// **边界**：`n_active == 0` / `batch_per_worker == 0` → no-op 返回 `Ok(())`。
    pub fn step_parallel(
        &mut self,
        rng_pool: &mut [Box<dyn RngSource>],
        n_threads: usize,
        batch_per_worker: usize,
    ) -> Result<(), TrainerError> {
        let n_active = n_threads.min(rng_pool.len());
        if n_active == 0 || batch_per_worker == 0 {
            return Ok(());
        }
        let active_pool = &mut rng_pool[..n_active];
        let n_players = self.game.n_players() as u64;
        let base_update_count = self.update_count;

        let game = &self.game;
        let shared_regret: &DenseNlheTable = &self.regret;

        // rayon dispatch：`par_iter_mut().enumerate()` 是 IndexedParallelIterator，
        // `.collect()` 保 input index 顺序，因此 `deltas[tid]` 与 tid 一一对应。
        // 每 worker σ 全程读 pre-dispatch `shared_regret`（slot-based 只读）。
        let deltas: Vec<(DenseLocalDelta, DenseLocalDelta)> = active_pool
            .par_iter_mut()
            .enumerate()
            .map(|(tid, rng_slot)| {
                let mut local_regret = DenseLocalDelta::new();
                let mut local_strategy = DenseLocalDelta::new();
                let rng = rng_slot.as_mut();
                for batch_idx in 0..batch_per_worker {
                    let trajectory_index = batch_idx as u64 * n_active as u64 + tid as u64;
                    let traverser =
                        ((base_update_count + trajectory_index) % n_players) as PlayerId;
                    let root = game.root(rng);
                    recurse_es_dense_parallel(
                        root,
                        traverser,
                        shared_regret,
                        &mut local_regret,
                        &mut local_strategy,
                        rng,
                    );
                }
                (local_regret, local_strategy)
            })
            .collect();

        // playback merge：tid 升序遍历 deltas，每 worker 内按 push 顺序 playback。
        for (local_regret, local_strategy) in deltas {
            for (slot_start, row_index, delta) in local_regret.into_entries() {
                self.regret
                    .accumulate_by_slot(slot_start, row_index, &delta);
            }
            for (slot_start, row_index, weighted) in local_strategy.into_entries() {
                self.strategy_sum
                    .accumulate_by_slot(slot_start, row_index, &weighted);
            }
        }
        self.update_count += (n_active as u64) * (batch_per_worker as u64);
        // LCFR period rescale 在批合并完成后触发（本批 delta 全在 pre-rescale scale
        // 下累积；语义同 EsMccfrTrainer::step_parallel）。
        self.maybe_lcfr_rescale();
        Ok(())
    }

    /// 多线程并发 step（dense **lock-free atomic** 并行路径，
    /// `docs/temp/nlhe_dense_parallel_merge_alternatives_2026_05_28.md` 方案 A）。
    ///
    /// 与 [`Self::step_parallel`] 的 deterministic local-delta + merge 区别：
    /// - **去掉 local delta + merge 阶段**：worker 直接对 `&self.regret` /
    ///   `&self.strategy_sum` 走 lock-free atomic f64 add（CAS loop，[`DenseNlheTable::accumulate_by_slot`]
    ///   `&self` 入口），整批结束后 rayon par_iter 自然 join。
    /// - **σ 读看到当下表**：worker 间没有 pre-dispatch snapshot 概念，每次决策
    ///   节点都 load 主表当前值（cell 之间可能跨 update，CFR/MCCFR 容忍；
    ///   Hogwild! Niu et al. 2011）。
    /// - **不保 byte-equal**：CAS race 顺序不确定，跨 run / 跨后端字节不再相等；
    ///   [`Self::step_parallel`] 仍保持 byte-equal anchor。
    /// - **LCFR rescale 时机一致**：rayon par_iter join 后调
    ///   [`Self::maybe_lcfr_rescale`]，与 [`Self::step_parallel`] 同——本批 delta
    ///   全用 pre-rescale `global_scale` 累积，rescale 不在 worker 内部触发。
    ///
    /// **边界**：`n_active == 0` / `batch_per_worker == 0` → no-op 返回 `Ok(())`。
    pub fn step_parallel_lockfree(
        &mut self,
        rng_pool: &mut [Box<dyn RngSource>],
        n_threads: usize,
        batch_per_worker: usize,
    ) -> Result<(), TrainerError> {
        let n_active = n_threads.min(rng_pool.len());
        if n_active == 0 || batch_per_worker == 0 {
            return Ok(());
        }
        let active_pool = &mut rng_pool[..n_active];
        let n_players = self.game.n_players() as u64;
        let base_update_count = self.update_count;

        let game = &self.game;
        let regret: &DenseNlheTable = &self.regret;
        let strategy_sum: &DenseNlheTable = &self.strategy_sum;

        active_pool
            .par_iter_mut()
            .enumerate()
            .for_each(|(tid, rng_slot)| {
                let rng = rng_slot.as_mut();
                for batch_idx in 0..batch_per_worker {
                    let trajectory_index = batch_idx as u64 * n_active as u64 + tid as u64;
                    let traverser =
                        ((base_update_count + trajectory_index) % n_players) as PlayerId;
                    let root = game.root(rng);
                    recurse_es_dense_lockfree(root, traverser, regret, strategy_sum, rng);
                }
            });

        self.update_count += (n_active as u64) * (batch_per_worker as u64);
        // LCFR period rescale 在 rayon par_iter join 后触发，worker 不可能并发；
        // `rescale_all` 用 `&mut self` 强制独占借用，CAS race 消失。
        self.maybe_lcfr_rescale();
        Ok(())
    }

    /// current strategy（regret matching）：两表都没 touch 过该 infoset → 空 `Vec`；
    /// 否则走 regret matching（退化均匀分布）。
    ///
    /// touched bit 只在 [`DenseNlheTable::accumulate_by_slot`] 时置位：traverser 节点写
    /// regret、非-traverser 节点写 strategy_sum（average strategy 在对手节点累积），所以
    /// 训练中**任何被访问过的 player 节点**都至少有一张表 touched——与 HashMap
    /// `recurse_es` 无条件 `regret.get_or_init` 出的 present 集合相同，未访问 infoset 两边
    /// 分别返回空 / 不 present。消费方仍应把空 `Vec` 按 uniform 兜底。
    ///
    /// （2026-10-26 之前 average strategy 在 traverser 节点累积，只作为对手路过的 infoset
    /// 两表都不 touched → dense 返回空、HashMap 返回 uniform；旧 checkpoint 里仍有这种行。）
    pub fn current_strategy(&self, info: InfoSetId) -> Vec<f64> {
        let row = self.regret.indexer().locate(info).row_index;
        if self.regret.touched_row(row) || self.strategy_sum.touched_row(row) {
            self.regret.current_strategy_by_info(info)
        } else {
            Vec::new()
        }
    }

    /// average strategy（strategy_sum 归一化）：两表都没 touch 过 → 空 `Vec`；否则
    /// 归一化（sum 为 0 退化均匀分布）。
    ///
    /// touched 语义见 [`Self::current_strategy`]。
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

    /// dense 表布局指纹（save / load 共用：从持有的 indexer + game bucket hash 算）。
    fn layout_fingerprint(&self) -> DenseLayoutFingerprint {
        DenseLayoutFingerprint::from_indexer(self.regret.indexer(), self.game.bucket_table_blake3())
    }

    /// 写出 dense checkpoint v3（Phase 4）：raw f64 两表 + touched bitset + lcfr
    /// 元数据 + layout fingerprint。格式见 [`crate::training::nlhe_dense_checkpoint`]。
    pub fn save_checkpoint(&self, path: &Path) -> Result<(), CheckpointError> {
        let fingerprint = self.layout_fingerprint();
        let meta = DenseCheckpointMeta {
            update_count: self.update_count,
            rng_state: self.rng_substream_seed,
            lcfr_period_size: self.lcfr_period_size,
            lcfr_periods_completed: self.lcfr_periods_completed,
            lcfr_rescale_regret: self.lcfr_rescale_regret,
        };
        save_dense_checkpoint(path, &fingerprint, &meta, &self.regret, &self.strategy_sum)
    }

    /// 从 dense checkpoint v3 恢复。`game` 重建 indexer + expected fingerprint；
    /// fingerprint 不符（不同树 / abstraction / bucket）即拒绝。**LCFR 元数据随
    /// dense checkpoint 一并恢复**——与 HashMap 路径不同（后者 resume 丢 LCFR 配置），
    /// dense resume 能无缝续跑 LCFR period rescale。
    pub fn load_checkpoint(path: &Path, game: SimplifiedNlheGame) -> Result<Self, CheckpointError> {
        let indexer = Arc::new(NlheDenseIndexer::from_tree(
            game.tree(),
            bucket_count_by_street(&game),
        ));
        let expected = DenseLayoutFingerprint::from_indexer(&indexer, game.bucket_table_blake3());
        let (meta, regret, strategy_sum) =
            load_dense_checkpoint(path, &expected, Arc::clone(&indexer))?;
        Ok(Self {
            game,
            regret,
            strategy_sum,
            update_count: meta.update_count,
            rng_substream_seed: meta.rng_state,
            lcfr_period_size: meta.lcfr_period_size,
            lcfr_periods_completed: meta.lcfr_periods_completed,
            lcfr_rescale_regret: meta.lcfr_rescale_regret,
        })
    }

    /// 从旧 HashMap path checkpoint（schema v2，`PLCKPT\0\0`）单向加载到 dense 表
    /// （plan §Checkpoint 兼容策略：HashMap → dense）。逐 entry `accumulate_by_info`
    /// 填空表（等价 set：空表 0 + delta = delta），值与 HashMap 路径 byte-equal。
    ///
    /// 校验 trainer_variant == EsMccfr / game_variant == SimplifiedNlhe /
    /// bucket_table_blake3 与 `game` 一致；不符返回相应 mismatch。LCFR 元数据不在 v2
    /// schema 内 → resume 后默认 vanilla（同 HashMap `load_checkpoint` 行为）。
    pub fn from_hashmap_checkpoint(
        path: &Path,
        game: SimplifiedNlheGame,
    ) -> Result<Self, CheckpointError> {
        let ckpt = Checkpoint::open(path)?;
        if ckpt.trainer_variant != TrainerVariant::EsMccfr
            || ckpt.game_variant != GameVariant::SimplifiedNlhe
        {
            return Err(CheckpointError::TrainerMismatch {
                expected: (TrainerVariant::EsMccfr, GameVariant::SimplifiedNlhe),
                got: (ckpt.trainer_variant, ckpt.game_variant),
            });
        }
        let expected_bucket = game.bucket_table_blake3();
        if ckpt.bucket_table_blake3 != expected_bucket {
            return Err(CheckpointError::BucketTableMismatch {
                expected: expected_bucket,
                got: ckpt.bucket_table_blake3,
            });
        }

        let regret_entries: Vec<(InfoSetId, Vec<f64>)> =
            bincode::deserialize(&ckpt.regret_table_bytes).map_err(|e| {
                CheckpointError::Corrupted {
                    offset: 0,
                    reason: format!("bincode deserialize regret table failed: {e}"),
                }
            })?;
        let strategy_entries: Vec<(InfoSetId, Vec<f64>)> =
            bincode::deserialize(&ckpt.strategy_sum_bytes).map_err(|e| {
                CheckpointError::Corrupted {
                    offset: 0,
                    reason: format!("bincode deserialize strategy table failed: {e}"),
                }
            })?;

        let mut trainer = Self::new(game, 0);
        for (info, v) in regret_entries {
            trainer.regret.accumulate_by_info(info, &v);
        }
        for (info, v) in strategy_entries {
            trainer.strategy_sum.accumulate_by_info(info, &v);
        }
        trainer.update_count = ckpt.update_count;
        trainer.rng_substream_seed = ckpt.rng_state;
        Ok(trainer)
    }
}

/// 每街 bucket 数 `[preflop, flop, turn, river]`（indexer `from_tree` 入参）。
/// preflop 固定 169 lossless；postflop 从 bucket table config 读（v3 = 500/500/500）。
pub(crate) fn bucket_count_by_street(game: &SimplifiedNlheGame) -> [u32; 4] {
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
                let mut cfvs: SigmaVec = SigmaVec::with_capacity(n);
                for action in &actions {
                    let next_state = SimplifiedNlheGame::next(state.clone(), *action, rng);
                    let cfv = recurse_es_dense(next_state, traverser, regret, strategy_sum, rng);
                    cfvs.push(cfv);
                }
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                // External sampling：opponent/chance reach 由 visitation 概率隐式提供，
                // per-visit delta 不再乘 pi_opp（与 recurse_es 完全一致）。
                let delta: SigmaVec = cfvs.iter().map(|c| c - sigma_value).collect();
                regret.accumulate_by_info(info, &delta);
                sigma_value
            } else {
                // opponent node：average strategy 在此按权重 1 累积（全 σ，含零概率 action；
                // 同 HashMap recurse_es 的 `avg_at_opp` 路径）。
                strategy_sum.accumulate_by_info(info, &sigma);
                // 按 σ 采样 1 action（剔除零概率 outcome；D-309 / D-337）。
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
                recurse_es_dense(next_state, traverser, regret, strategy_sum, rng)
            }
        }
    }
}

/// Phase 3 并行 DFS recurse（dense 存储版）。
///
/// 与 [`recurse_es_dense`] 同型语义，差别仅在累积容器分流（镜像 HashMap 路径的
/// [`crate::training::trainer`] 私有 `recurse_es_parallel`）：
/// - **σ 计算**：走 **共享只读** `shared_regret`（[`DenseNlheTable::current_strategy_smallvec_at`]
///   对未访问行返回 uniform，等价 HashMap 未见 InfoSet 回退；parallel 路径不写
///   主表，避免跨线程数据竞争）。
/// - **regret / strategy_sum push**：写入 **线程本地** [`DenseLocalDelta`]
///   （slot-based append-only），merge 阶段按 push 顺序 playback 到主表。
///
/// `locate` 每决策节点只调一次（拿 `slot_start` / `row_index` / `action_count`），
/// σ 读与 delta push 都复用同一定位结果。push 顺序与 [`recurse_es_dense`] /
/// HashMap `recurse_es_parallel` 一致：非-traverser 分支先 push strategy_sum 再递归，
/// traverser 分支递归子节点后再 push regret。
///
/// traverser 分支走 last-iteration-move（前 n-1 次 `state.clone()`，最后一次 move），
/// 同 HashMap 并行版 `recurse_es_parallel`——`next` 调用顺序不变，仅省 1 次 clone+drop，
/// byte-equal 不受影响。单线程 [`recurse_es_dense`] 不做此优化（对齐单线程 HashMap
/// `recurse_es` 的全 clone），两条 byte-equal 关系各自独立成立。
fn recurse_es_dense_parallel(
    state: SimplifiedNlheState,
    traverser: PlayerId,
    shared_regret: &DenseNlheTable,
    local_regret: &mut DenseLocalDelta,
    local_strategy: &mut DenseLocalDelta,
    rng: &mut dyn RngSource,
) -> f64 {
    match SimplifiedNlheGame::current(&state) {
        NodeKind::Terminal => SimplifiedNlheGame::payoff(&state, traverser),
        NodeKind::Chance => unreachable!(
            "简化 NLHE 无 in-game chance node（randomness 全在 Game::root 消费）；\
             current(state) 不应返回 Chance"
        ),
        NodeKind::Player(actor) => {
            let info = SimplifiedNlheGame::info_set(&state, actor);
            let actions = SimplifiedNlheGame::legal_actions(&state);
            let n = actions.len();
            // 每决策节点 locate 一次：σ 读 + delta push 复用同一 slot 定位。
            let slot = shared_regret.indexer().locate(info);
            let sigma =
                shared_regret.current_strategy_smallvec_at(slot.slot_start, slot.action_count);

            if actor == traverser {
                debug_assert!(
                    n > 0,
                    "traverser Player 节点必有 ≥ 1 legal action（fold 兜底）"
                );
                let mut cfvs: SigmaVec = SigmaVec::with_capacity(n);
                // last iteration 消耗原 state（move 进 `next`）；前 n-1 次 clone。每个
                // traverser 节点省 1 次 State::clone + drop（镜像 HashMap 并行版
                // `recurse_es_parallel`）。`next` 调用顺序不变 → rng 消费序列不变 →
                // 与单线程 `recurse_es_dense` / HashMap 并行版 byte-equal 不受影响。
                let last_idx = n - 1;
                for &action in &actions[..last_idx] {
                    let next_state = SimplifiedNlheGame::next(state.clone(), action, rng);
                    let cfv = recurse_es_dense_parallel(
                        next_state,
                        traverser,
                        shared_regret,
                        local_regret,
                        local_strategy,
                        rng,
                    );
                    cfvs.push(cfv);
                }
                let last_state = SimplifiedNlheGame::next(state, actions[last_idx], rng);
                let cfv_last = recurse_es_dense_parallel(
                    last_state,
                    traverser,
                    shared_regret,
                    local_regret,
                    local_strategy,
                    rng,
                );
                cfvs.push(cfv_last);
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                // External sampling：per-visit delta 不乘 pi_opp（与 recurse_es_dense 一致）。
                let delta: SigmaVec = cfvs.iter().map(|c| c - sigma_value).collect();
                local_regret.push(slot.slot_start, slot.row_index, delta);
                sigma_value
            } else {
                // opponent node：按 σ 采样 1 action（剔除零概率 outcome）。
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
                // average strategy 按权重 1 push（位置同 HashMap recurse_es_parallel）。
                local_strategy.push(slot.slot_start, slot.row_index, sigma);
                let sampled = sample_discrete(&nonzero_dist, rng);
                let next_state = SimplifiedNlheGame::next(state, sampled, rng);
                recurse_es_dense_parallel(
                    next_state,
                    traverser,
                    shared_regret,
                    local_regret,
                    local_strategy,
                    rng,
                )
            }
        }
    }
}

/// **Lock-free atomic** 并行 DFS recurse（dense 存储版，方案 A）。
///
/// 与 [`recurse_es_dense_parallel`] 同构，差别在累积写入直接走 **共享主表** 的
/// lock-free atomic add（[`DenseNlheTable::accumulate_by_slot`] `&self` 入口，
/// CAS loop）：不再 push 进线程本地 `DenseLocalDelta`，无 merge 阶段。σ 读看到的是
/// 当下表（不再是 pre-dispatch snapshot），跨 cell 可能跨 update，CFR/MCCFR 容忍
/// （Hogwild! Niu et al. 2011 + 后续 MCCFR 经验）。
///
/// `locate` 每决策节点只调一次；σ 读 + 写入都复用同一 slot 定位。push 顺序与
/// [`recurse_es_dense_parallel`] 一致：非-traverser 分支先写 strategy_sum 再递归，
/// traverser 分支递归子节点后再写 regret。`last-iteration move`（前 n-1 次 `state.clone()` / 最后一次
/// move）保持，对齐并行同型；本路径 byte-equal 本就不成立（CAS race 顺序不定），
/// 但 rng 消费序列与 deterministic 路径一致，便于做 LBR / avg_strategy 统计对照。
fn recurse_es_dense_lockfree(
    state: SimplifiedNlheState,
    traverser: PlayerId,
    regret: &DenseNlheTable,
    strategy_sum: &DenseNlheTable,
    rng: &mut dyn RngSource,
) -> f64 {
    match SimplifiedNlheGame::current(&state) {
        NodeKind::Terminal => SimplifiedNlheGame::payoff(&state, traverser),
        NodeKind::Chance => unreachable!(
            "简化 NLHE 无 in-game chance node（randomness 全在 Game::root 消费）；\
             current(state) 不应返回 Chance"
        ),
        NodeKind::Player(actor) => {
            let info = SimplifiedNlheGame::info_set(&state, actor);
            let actions = SimplifiedNlheGame::legal_actions(&state);
            let n = actions.len();
            let slot = regret.indexer().locate(info);
            let sigma = regret.current_strategy_smallvec_at(slot.slot_start, slot.action_count);

            if actor == traverser {
                debug_assert!(
                    n > 0,
                    "traverser Player 节点必有 ≥ 1 legal action（fold 兜底）"
                );
                let mut cfvs: SigmaVec = SigmaVec::with_capacity(n);
                let last_idx = n - 1;
                for &action in &actions[..last_idx] {
                    let next_state = SimplifiedNlheGame::next(state.clone(), action, rng);
                    let cfv =
                        recurse_es_dense_lockfree(next_state, traverser, regret, strategy_sum, rng);
                    cfvs.push(cfv);
                }
                let last_state = SimplifiedNlheGame::next(state, actions[last_idx], rng);
                let cfv_last =
                    recurse_es_dense_lockfree(last_state, traverser, regret, strategy_sum, rng);
                cfvs.push(cfv_last);
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                let delta: SigmaVec = cfvs.iter().map(|c| c - sigma_value).collect();
                regret.accumulate_by_slot(slot.slot_start, slot.row_index, &delta);
                sigma_value
            } else {
                strategy_sum.accumulate_by_slot(slot.slot_start, slot.row_index, &sigma);
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
                recurse_es_dense_lockfree(next_state, traverser, regret, strategy_sum, rng)
            }
        }
    }
}
