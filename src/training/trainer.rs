//! `Trainer` trait + `VanillaCfrTrainer` + `EsMccfrTrainer`（API-310..API-313）。
//!
//! `Trainer<G: Game>` 统一 interface：[`Trainer::step`] 执行 1 iter（Vanilla CFR）
//! 或 1 update（ES-MCCFR）；[`Trainer::save_checkpoint`] / [`Trainer::load_checkpoint`]
//! 走 [`crate::training::Checkpoint`] 二进制 schema（D-350 / API-350）；
//! [`Trainer::current_strategy`] / [`Trainer::average_strategy`] stateless 查询
//! （D-328）。
//!
//! VanillaCfrTrainer for Kuhn / Leduc（D-300 Zinkevich 2007 详解伪代码）；
//! EsMccfrTrainer for 简化 NLHE（D-301 Lanctot 2009 详解伪代码 + D-321 多线程
//! thread-safety 模型 deferred 到 C2 \[实现\] 起步前 lock）。
//!
//! B2 \[实现\] 落地 [`VanillaCfrTrainer`] 全部 Trainer 方法（除 save/load checkpoint
//! 走 D2 \[实现\]）；[`EsMccfrTrainer`] 保持 `unimplemented!()`（C2 \[实现\] 落地）。

use std::path::Path;

use rayon::prelude::*;
use smallvec::SmallVec;

use crate::core::rng::RngSource;
use crate::error::{CheckpointError, TrainerError, TrainerVariant};
use crate::training::checkpoint::{preflight_trainer, read_file_bytes, Checkpoint, SCHEMA_VERSION};
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::regret::{
    LocalRegretDelta, LocalStrategyDelta, RegretTable, SigmaVec, StrategyAccumulator,
};
use crate::training::sampling::{derive_substream_seed, sample_discrete};

/// 与 `SigmaVec` 同型 inline-8 短向量，用于 traverser cfvs / regret delta /
/// strategy_sum weighted vec / nonzero opp 分布等热路径短数组（E2-rev1 \[实现\]
/// 优化）。命名与 `SigmaVec` 区分仅出于可读性（数值语义不限于"sigma"）。
type ShortVec<T> = SmallVec<[T; 8]>;

/// 训练器统一 trait（API-310 / D-371）。
pub trait Trainer<G: Game> {
    /// 执行 1 iter 训练（Vanilla CFR）或 1 update（ES-MCCFR D-307 alternating）。
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError>;

    /// 当前 InfoSet 上的 current strategy（regret matching；D-303 标准 RM）。
    fn current_strategy(&self, info_set: &G::InfoSet) -> Vec<f64>;

    /// 当前 InfoSet 上的 average strategy（strategy_sum 归一化；D-304 标准累积）。
    fn average_strategy(&self, info_set: &G::InfoSet) -> Vec<f64>;

    /// 已完成 iter / update 数（Vanilla CFR: iter；ES-MCCFR: per-player update）。
    fn update_count(&self) -> u64;

    /// 写出 checkpoint（D-353 write-to-temp + atomic rename + D-352 trailer BLAKE3）。
    fn save_checkpoint(&self, path: &Path) -> Result<(), CheckpointError>;

    /// 从 checkpoint 恢复（D-350 schema 校验 + D-352 eager BLAKE3 + D-356 多
    /// game 不兼容拒绝）。
    fn load_checkpoint(path: &Path, game: G) -> Result<Self, CheckpointError>
    where
        Self: Sized;
}

/// Vanilla CFR Trainer（API-311 / D-300）。
///
/// `rng_substream_seed` 是 master_seed 经 SplitMix64 finalizer × 4 派生的 32 byte
/// ChaCha20Rng seed（D-335），目前由 D2 \[实现\] checkpoint 序列化路径消费；B2
/// \[实现\] step 走 full-tree 全确定性枚举不消费 rng，因此本字段在 B2 阶段仅占位
/// 落表（`#[allow(dead_code)]` 在 D2 \[实现\] 落地后取消）。
pub struct VanillaCfrTrainer<G: Game> {
    pub(crate) game: G,
    pub(crate) regret: RegretTable<G::InfoSet>,
    pub(crate) strategy_sum: StrategyAccumulator<G::InfoSet>,
    pub(crate) iter: u64,
    pub(crate) rng_substream_seed: [u8; 32],
}

impl<G: Game> VanillaCfrTrainer<G> {
    /// 新建空 Trainer。`master_seed` 用 D-335 SplitMix64 finalizer × 4 派生 32 byte
    /// sub-stream seed 占位（Vanilla CFR full-tree 全确定性枚举，sub-stream seed
    /// 仅在 D2 \[实现\] checkpoint 序列化时存档；step 本身不消费）。
    pub fn new(game: G, master_seed: u64) -> Self {
        let rng_substream_seed = derive_substream_seed(master_seed, 0, 0);
        Self {
            game,
            regret: RegretTable::new(),
            strategy_sum: StrategyAccumulator::new(),
            iter: 0,
            rng_substream_seed,
        }
    }
}

impl<G: Game> Trainer<G> for VanillaCfrTrainer<G> {
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        // D-300：alternating traverser × 完整博弈树 DFS × cfv 累积 × regret update
        // × strategy_sum 累积。每 step 内部 traverser ∈ [0, n_players) 各遍历 1 次。
        let n_players = self.game.n_players();
        let root = self.game.root(rng);
        for traverser in 0..n_players as u8 {
            recurse_vanilla::<G>(
                root.clone(),
                traverser,
                1.0,
                1.0,
                &mut self.regret,
                &mut self.strategy_sum,
                rng,
            );
        }
        self.iter += 1;
        Ok(())
    }

    fn current_strategy(&self, info_set: &G::InfoSet) -> Vec<f64> {
        let n = self
            .regret
            .inner()
            .get(info_set)
            .map(|v| v.len())
            .or_else(|| self.strategy_sum.inner().get(info_set).map(|v| v.len()))
            .unwrap_or(0);
        if n == 0 {
            return Vec::new();
        }
        // API-310 入口走 RegretTable::current_strategy 直接返回 owned Vec<f64>
        // （API-320 surface 不变）。trainer hot path 走 current_strategy_smallvec。
        self.regret.current_strategy(info_set, n)
    }

    fn average_strategy(&self, info_set: &G::InfoSet) -> Vec<f64> {
        let n = self
            .strategy_sum
            .inner()
            .get(info_set)
            .map(|v| v.len())
            .or_else(|| self.regret.inner().get(info_set).map(|v| v.len()))
            .unwrap_or(0);
        if n == 0 {
            return Vec::new();
        }
        self.strategy_sum.average_strategy(info_set, n)
    }

    fn update_count(&self) -> u64 {
        self.iter
    }

    fn save_checkpoint(&self, path: &Path) -> Result<(), CheckpointError> {
        let regret_table_bytes = encode_table(self.regret.inner())?;
        let strategy_sum_bytes = encode_table(self.strategy_sum.inner())?;
        let ckpt = Checkpoint {
            schema_version: SCHEMA_VERSION,
            trainer_variant: TrainerVariant::VanillaCfr,
            game_variant: G::VARIANT,
            update_count: self.iter,
            rng_state: self.rng_substream_seed,
            bucket_table_blake3: self.game.bucket_table_blake3(),
            regret_table_bytes,
            strategy_sum_bytes,
        };
        ckpt.save(path)
    }

    fn load_checkpoint(path: &Path, game: G) -> Result<Self, CheckpointError>
    where
        Self: Sized,
    {
        let bytes = read_file_bytes(path)?;
        preflight_trainer(
            &bytes,
            TrainerVariant::VanillaCfr,
            G::VARIANT,
            game.bucket_table_blake3(),
        )?;
        let ckpt = Checkpoint::parse_bytes(&bytes)?;
        let regret = decode_table::<G::InfoSet>(&ckpt.regret_table_bytes)?;
        let strategy_sum = decode_strategy::<G::InfoSet>(&ckpt.strategy_sum_bytes)?;
        Ok(Self {
            game,
            regret,
            strategy_sum,
            iter: ckpt.update_count,
            rng_substream_seed: ckpt.rng_state,
        })
    }
}

/// Vanilla CFR DFS recurse（D-300 详解伪代码）。
///
/// 返回 traverser 视角的 cfv（counterfactual value）。
fn recurse_vanilla<G: Game>(
    state: G::State,
    traverser: PlayerId,
    pi_trav: f64,
    pi_opp: f64,
    regret: &mut RegretTable<G::InfoSet>,
    strategy_sum: &mut StrategyAccumulator<G::InfoSet>,
    rng: &mut dyn RngSource,
) -> f64 {
    match G::current(&state) {
        NodeKind::Terminal => G::payoff(&state, traverser),
        NodeKind::Chance => {
            let dist = G::chance_distribution(&state);
            let mut value = 0.0;
            for (action, prob) in dist {
                let next_state = G::next(state.clone(), action, rng);
                value += prob
                    * recurse_vanilla::<G>(
                        next_state,
                        traverser,
                        pi_trav,
                        pi_opp,
                        regret,
                        strategy_sum,
                        rng,
                    );
            }
            value
        }
        NodeKind::Player(actor) => {
            let info = G::info_set(&state, actor);
            let actions = G::legal_actions(&state);
            let n = actions.len();
            // ensure regret slot exists with correct length (D-324)
            regret.get_or_init(info.clone(), n);
            // 热路径走 current_strategy_smallvec 走 SmallVec stack alloc
            // （E2-rev1 \[实现\]，API-320 surface 不变）。
            let sigma = regret.current_strategy_smallvec(&info, n);

            if actor == traverser {
                // traverser node：枚举每个 action 的 cfv，累积 regret + strategy_sum
                let mut cfvs: ShortVec<f64> = ShortVec::with_capacity(n);
                for (i, action) in actions.iter().enumerate() {
                    let next_state = G::next(state.clone(), *action, rng);
                    let cfv = recurse_vanilla::<G>(
                        next_state,
                        traverser,
                        pi_trav * sigma[i],
                        pi_opp,
                        regret,
                        strategy_sum,
                        rng,
                    );
                    cfvs.push(cfv);
                }
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                // regret update R(I, a) += π_opp × (cfv_a - σ_node)
                let delta: ShortVec<f64> =
                    cfvs.iter().map(|c| pi_opp * (c - sigma_value)).collect();
                regret.accumulate(info.clone(), &delta);
                // strategy_sum update S(I, a) += π_traverser × σ(I, a)
                let weighted: ShortVec<f64> = sigma.iter().map(|s| pi_trav * s).collect();
                strategy_sum.accumulate(info, &weighted);
                sigma_value
            } else {
                // opponent node：σ 加权累计 cfv，opp reach probability 乘 σ(a)
                let mut value = 0.0;
                for (i, action) in actions.iter().enumerate() {
                    let next_state = G::next(state.clone(), *action, rng);
                    value += sigma[i]
                        * recurse_vanilla::<G>(
                            next_state,
                            traverser,
                            pi_trav,
                            pi_opp * sigma[i],
                            regret,
                            strategy_sum,
                            rng,
                        );
                }
                value
            }
        }
    }
}

/// External-Sampling MCCFR Trainer（API-312 / D-301）。
///
/// **D-321-rev1 lock**（2026-05-13，C2 \[实现\] 起步前；详见
/// `pluribus_stage3_decisions.md` §10.2）：thread-safety 模型 = thread-local
/// accumulator + batch merge（候选 ③）。E2 \[实现\]（2026-05-14）落地真并发：
/// `step_parallel` 走 [`std::thread::scope`] × n_active 并发 spawn，每线程持有
/// 独立 thread-local `(RegretTable, StrategyAccumulator)` 作为 delta accumulator
/// （只在本次 step 内被访问的 InfoSet 上累积，不是 full main-table clone）；
/// spawn 内 σ 走只读共享主 `RegretTable::current_strategy`（无 lock，HashMap 只读
/// 在 thread::scope 借用期内安全）；spawn 结束后 main thread 按 tid 升序 ×
/// 每 thread 内 InfoSet `Debug` 排序顺序 batch merge 回主表（继承 `encode_table`
/// 同型 sort 规则，保跨 run BLAKE3 byte-equal）。`rng_substream_seed` 字段由
/// D2 \[实现\] checkpoint 序列化路径消费。
pub struct EsMccfrTrainer<G: Game> {
    pub(crate) game: G,
    pub(crate) regret: RegretTable<G::InfoSet>,
    pub(crate) strategy_sum: StrategyAccumulator<G::InfoSet>,
    pub(crate) update_count: u64,
    pub(crate) rng_substream_seed: [u8; 32],
    /// LCFR-MCCFR period 大小（updates per period）。`None` = vanilla ES-MCCFR
    /// （strategy_sum / regret 不衰减）。`Some(P)` = Brown & Sandholm 2018
    /// §Discounted Monte Carlo CFR：每完成 P 个 update 触发一次 period boundary
    /// rescale，period n 末乘 `n/(n+1)`，等价 Linear CFR 权重但 amortize 到 period
    /// 粒度。Burch 2017 + Brown 2018 验证 CFR+ 加 MCCFR 不起作用，LCFR 加 MCCFR
    /// 显著优于 vanilla（HUNL subgame Figure 10/11，arxiv 1809.04040）。
    pub(crate) lcfr_period_size: Option<u64>,
    /// 已完成的 period 数（= 已触发 rescale 次数）。`step` / `step_parallel` 末
    /// `target = update_count / period_size` 与本字段比较，差值为本批应补的 rescale
    /// 次数。`lcfr_period_size = None` 时本字段恒 0。
    pub(crate) lcfr_periods_completed: u64,
    /// LCFR rescale 是否同时作用于 regret 表（Brown 2018 默认 true）；false 时
    /// 仅 rescale strategy_sum（对照实验入口，见 [`Self::with_lcfr_period_strategy_only`]）。
    pub(crate) lcfr_rescale_regret: bool,
}

impl<G: Game> EsMccfrTrainer<G> {
    /// 新建空 Trainer（API-312）。`master_seed` 用 D-335 SplitMix64 finalizer ×
    /// 4 派生 32 byte sub-stream seed 占位（D2 checkpoint 序列化时存档；step
    /// 本身不消费——`step` 接受的 `rng: &mut dyn RngSource` 是唯一 randomness
    /// 来源）。
    pub fn new(game: G, master_seed: u64) -> Self {
        let rng_substream_seed = derive_substream_seed(master_seed, 0, 0);
        Self {
            game,
            regret: RegretTable::new(),
            strategy_sum: StrategyAccumulator::new(),
            update_count: 0,
            rng_substream_seed,
            lcfr_period_size: None,
            lcfr_periods_completed: 0,
            lcfr_rescale_regret: true,
        }
    }

    /// 启用 LCFR-MCCFR period rescale（Brown & Sandholm 2018 §Discounted Monte
    /// Carlo CFR）。`period_size` = 每 period 多少个 update；典型取值让
    /// `total_updates / period_size` 落在 20–100 之间（period 太少 → linear 权重
    /// 不充分，period 太多 → rescale 开销可见）。0 或 `update_count > 0` 时调用
    /// panic（必须在训练开始前 / `update_count == 0` 状态配置）。
    pub fn with_lcfr_period(mut self, period_size: u64) -> Self {
        assert!(
            period_size > 0,
            "LCFR period_size must be > 0 (got 0); pass None / 不调用本方法 = vanilla ES-MCCFR"
        );
        assert_eq!(
            self.update_count, 0,
            "LCFR period_size must be configured before any step (update_count = {} != 0)",
            self.update_count
        );
        self.lcfr_period_size = Some(period_size);
        self
    }

    /// 处理 LCFR period boundary rescale（`step` / `step_parallel` 末调用）。
    /// `update_count` 已更新到本批末尾值；本方法补做 `lcfr_periods_completed` 与
    /// `update_count / period_size` 之间所有缺失的 rescale。
    ///
    /// `step_parallel` 一批可能跨多个 period boundary（n_threads × P 的极端情形），
    /// 通过 while-loop 顺序补齐，每个 boundary 用对应的 `n/(n+1)` 因子。
    fn maybe_lcfr_rescale(&mut self) {
        let Some(period_size) = self.lcfr_period_size else {
            return;
        };
        let target = self.update_count / period_size;
        while self.lcfr_periods_completed < target {
            let n = self.lcfr_periods_completed + 1;
            // factor = n / (n+1)；n+1 不溢出（u64 范围远超 NLHE 实际 period count）。
            let factor = (n as f64) / ((n + 1) as f64);
            if self.lcfr_rescale_regret {
                self.regret.rescale_all(factor);
            }
            self.strategy_sum.rescale_all(factor);
            self.lcfr_periods_completed = n;
        }
    }

    /// 仅对 strategy_sum 做 LCFR rescale，regret 不动（"Linear average only"
    /// 对照实验入口）。Brown 2018 §Discounted MCCFR 标准形态是 regret + strategy_sum
    /// 同时 rescale；本方法用于诊断"regret discount 是否在 Leduc 收敛后期 over-discount"。
    /// 必须在 `update_count == 0` 时调用。
    pub fn with_lcfr_period_strategy_only(mut self, period_size: u64) -> Self {
        assert!(
            period_size > 0,
            "LCFR period_size must be > 0 (got 0); pass None / 不调用本方法 = vanilla ES-MCCFR"
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

    /// 已访问 infoset 的 strategy_sum 表只读访问（诊断用：导出
    /// `Σ_a strategy_sum[I][a]` 作为该 infoset 的"学习信号权重"，用于分布分析）。
    #[doc(hidden)]
    pub fn strategy_sum(&self) -> &StrategyAccumulator<G::InfoSet> {
        &self.strategy_sum
    }

    /// 已访问 infoset 的 regret 表只读访问（诊断用，同 [`Self::strategy_sum`]）。
    #[doc(hidden)]
    pub fn regret_table(&self) -> &RegretTable<G::InfoSet> {
        &self.regret
    }

    /// 只读访问内部 `game`。S6 实时搜索用：subgame solve 完后用 game 在 subtree root
    /// 为 hero 真实手构造查询 infoset / 合法动作（见
    /// [`crate::training::subgame::subgame_search`]）。
    pub fn game(&self) -> &G {
        &self.game
    }

    /// 多线程并发 step（D-321-rev1 lock + E2-rev1 \[实现\] 优化）。
    ///
    /// **E2-rev1 \[实现\] 形态（rayon long-lived pool + append-only delta，
    /// 2026-05-14）**：一次调用产出 `n_active = min(n_threads, rng_pool.len())`
    /// 个 update（每线程 1 个），`update_count += n_active`。alternating traverser
    /// 在线程间共享 `(update_count + tid) % n_players`：tid=0 对应进入本次
    /// `step_parallel` 时的 traverser，后续线程按 `tid` 递增 alternate
    /// （D-307 跨线程扩展，与原 D-321-rev1 形态等价）。
    ///
    /// **每 worker 跑 `batch_per_worker` 条 trajectory**（关键性能 knob）：
    /// rayon `par_iter_mut().enumerate()` 内 `for batch_idx in 0..batch_per_worker`
    /// 循环；一次 `step_parallel` 调用产 `n_active × batch_per_worker` 个 update。
    /// `batch_per_worker = 1` 退化为"每 worker 1 trajectory 然后 sync"的原形。
    ///
    /// **为什么需要 batching**：32-vCPU c6a.8xlarge 实测 perf 显示 17% CPU 花在
    /// `sched_yield` + `futex`、13% 花在 crossbeam / rayon 工作偷取，原因是每次
    /// `step_parallel` 的 dispatch 只产 `n_threads` 条 ~1 ms 的小任务，worker
    /// 醒来跑完立刻又等下一批 → 调度成本与计算量同量级。batch_per_worker = 16
    /// 让每次 dispatch 覆盖 ~16 ms 计算，调度开销摊薄 16×，scaling 从 1→32
    /// vCPU 由 2.85× 提到接近 8-10×（c6a.8xlarge 32t 实测）。
    ///
    /// **alternating traverser**：trajectory_index = `batch_idx × n_active + tid`，
    /// 让 tid=0 在 `batch_per_worker = 1` 时映射到 `(base + 0) % n_players`，与
    /// 原版完全一致；B ≥ 2 时 tid 维度 stride-N 走遍完整 update 区间，traverser
    /// 周期性 alternate 等价原 D-307 alternating 语义。
    ///
    /// **rayon pool 替 `std::thread::scope`**：rayon 全局 pool 复用长寿命 worker，
    /// scope-fifo 任务分发 ≈ ns 级 atomic dequeue。`par_iter_mut().enumerate().
    /// collect()` 对 [`IndexedParallelIterator`] 保 input 顺序，因此 `deltas[tid]`
    /// 与 tid 一一对应。
    ///
    /// **append-only delta**：每线程持有 `LocalRegretDelta` / `LocalStrategyDelta`
    /// = `Vec<(I, SigmaVec)>` 按 DFS 顺序 append；merge 阶段按 tid 升序 × 每
    /// thread 内 push 顺序 playback 到主表，省去原 `format!("{:?}", I)` ×
    /// O(N log N) 排序。
    ///
    /// **线程内语义**：σ 计算走只读共享 `&self.regret`
    /// （[`RegretTable::current_strategy`] 对 HashMap 无锁只读在 rayon 任务
    /// 借用期内 by-design 安全；HashMap 未触发结构 rehash 因主表不被任何
    /// worker 写）；regret + strategy_sum 累积全 push 到线程内本地 delta vec。
    /// 同 worker 的 `batch_per_worker` 条 trajectory 都读 pre-dispatch σ
    /// （stale-σ window 由 `n_active` 变为 `n_active × batch_per_worker`，
    /// NLHE 119M infoset 下 256² / 119M ≈ 0.06% 重访概率，bias 可忽略；
    /// Leduc 288 infoset 不能走 batched 路径，应继续单线程 `step`）。
    ///
    /// **跨 run 决定性**：append-only 路径下 thread 内 push 顺序 = DFS 顺序
    /// deterministic（rng 决定 sampled trajectory）；tid 顺序 deterministic
    /// （`par_iter_mut().enumerate().collect()` 保 index 顺序）；同 InfoSet 多次
    /// 访问按 push 顺序 playback，f64 加法序列 deterministic。BLAKE3 byte-equal
    /// 不破（D-362 anchor `tests/cfr_simplified_nlhe.rs` Test 5 走纯单线程
    /// `step`，本路径修改不触达；step_parallel-only 测试在 perf_slo.rs 仅断言
    /// throughput 不断言数值）。
    ///
    /// **边界**：`rng_pool.is_empty()` / `n_threads == 0` / `batch_per_worker == 0`
    /// → no-op，返回 `Ok(())`；`n_active > rng_pool.len()` 时截断到 `rng_pool.len()`。
    pub fn step_parallel(
        &mut self,
        rng_pool: &mut [Box<dyn RngSource>],
        n_threads: usize,
        batch_per_worker: usize,
    ) -> Result<(), TrainerError>
    where
        G: Sync,
        G::InfoSet: Send,
    {
        let n_active = n_threads.min(rng_pool.len());
        if n_active == 0 || batch_per_worker == 0 {
            return Ok(());
        }
        let active_pool = &mut rng_pool[..n_active];
        let n_players = self.game.n_players() as u64;
        let base_update_count = self.update_count;

        let game = &self.game;
        let shared_regret: &RegretTable<G::InfoSet> = &self.regret;

        // rayon 全局 pool dispatch：`par_iter_mut().enumerate()` 是
        // `IndexedParallelIterator`，`.collect()` 保 input index 顺序，因此
        // `deltas[tid]` 与 tid 一一对应。每 worker 在自己的 rayon 任务里跑
        // `batch_per_worker` 条 trajectory，σ 全程读 pre-dispatch `shared_regret`。
        #[allow(clippy::type_complexity)]
        let deltas: Vec<(LocalRegretDelta<G::InfoSet>, LocalStrategyDelta<G::InfoSet>)> =
            active_pool
                .par_iter_mut()
                .enumerate()
                .map(|(tid, rng_slot)| {
                    let mut local_regret = LocalRegretDelta::<G::InfoSet>::new();
                    let mut local_strategy = LocalStrategyDelta::<G::InfoSet>::new();
                    let rng = rng_slot.as_mut();
                    for batch_idx in 0..batch_per_worker {
                        let trajectory_index = batch_idx as u64 * n_active as u64 + tid as u64;
                        let traverser =
                            ((base_update_count + trajectory_index) % n_players) as PlayerId;
                        let root = game.root(rng);
                        recurse_es_parallel::<G>(
                            root,
                            traverser,
                            1.0,
                            shared_regret,
                            &mut local_regret,
                            &mut local_strategy,
                            rng,
                        );
                    }
                    (local_regret, local_strategy)
                })
                .collect();

        // playback merge：tid 升序遍历 deltas，每 thread 内按 push 顺序 playback。
        for (local_regret, local_strategy) in deltas {
            for (info, delta) in local_regret.into_entries() {
                self.regret.accumulate(info, &delta);
            }
            for (info, weighted) in local_strategy.into_entries() {
                self.strategy_sum.accumulate(info, &weighted);
            }
        }
        self.update_count += (n_active as u64) * (batch_per_worker as u64);
        // LCFR-MCCFR period rescale 在批合并完成后触发；本批的所有 delta 都在
        // pre-rescale scale 下累积。跨 period boundary 时 boundary 后
        // ≤ n_active × batch_per_worker 个 update 享受 boundary 前 weight，
        // period_size ≫ n_active × batch_per_worker 时误差可忽略
        // （NLHE 默认 period 1M / n_active × B = 32×16 = 512 → 0.05% 偏置）。
        self.maybe_lcfr_rescale();
        Ok(())
    }
}

impl<G: Game> Trainer<G> for EsMccfrTrainer<G> {
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        // D-307 alternating traverser：iter t 上 traverser = (t mod n_players)。
        let n_players = self.game.n_players() as u64;
        let traverser = (self.update_count % n_players) as PlayerId;
        let root = self.game.root(rng);
        recurse_es::<G>(
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

    fn current_strategy(&self, info_set: &G::InfoSet) -> Vec<f64> {
        let n = self
            .regret
            .inner()
            .get(info_set)
            .map(|v| v.len())
            .or_else(|| self.strategy_sum.inner().get(info_set).map(|v| v.len()))
            .unwrap_or(0);
        if n == 0 {
            return Vec::new();
        }
        // API-310 入口走 RegretTable::current_strategy 直接返回 owned Vec<f64>
        // （API-320 surface 不变）。trainer hot path 走 current_strategy_smallvec。
        self.regret.current_strategy(info_set, n)
    }

    fn average_strategy(&self, info_set: &G::InfoSet) -> Vec<f64> {
        let n = self
            .strategy_sum
            .inner()
            .get(info_set)
            .map(|v| v.len())
            .or_else(|| self.regret.inner().get(info_set).map(|v| v.len()))
            .unwrap_or(0);
        if n == 0 {
            return Vec::new();
        }
        self.strategy_sum.average_strategy(info_set, n)
    }

    fn update_count(&self) -> u64 {
        self.update_count
    }

    fn save_checkpoint(&self, path: &Path) -> Result<(), CheckpointError> {
        let regret_table_bytes = encode_table(self.regret.inner())?;
        let strategy_sum_bytes = encode_table(self.strategy_sum.inner())?;
        let ckpt = Checkpoint {
            schema_version: SCHEMA_VERSION,
            trainer_variant: TrainerVariant::EsMccfr,
            game_variant: G::VARIANT,
            update_count: self.update_count,
            rng_state: self.rng_substream_seed,
            bucket_table_blake3: self.game.bucket_table_blake3(),
            regret_table_bytes,
            strategy_sum_bytes,
        };
        ckpt.save(path)
    }

    fn load_checkpoint(path: &Path, game: G) -> Result<Self, CheckpointError>
    where
        Self: Sized,
    {
        let bytes = read_file_bytes(path)?;
        preflight_trainer(
            &bytes,
            TrainerVariant::EsMccfr,
            G::VARIANT,
            game.bucket_table_blake3(),
        )?;
        let ckpt = Checkpoint::parse_bytes(&bytes)?;
        let regret = decode_table::<G::InfoSet>(&ckpt.regret_table_bytes)?;
        let strategy_sum = decode_strategy::<G::InfoSet>(&ckpt.strategy_sum_bytes)?;
        Ok(Self {
            game,
            regret,
            strategy_sum,
            update_count: ckpt.update_count,
            rng_substream_seed: ckpt.rng_state,
            // LCFR period 配置不存 checkpoint（schema v2 不变）。resume 后默认
            // vanilla ES-MCCFR；调用方若要继续 LCFR 需立即调 `with_lcfr_period`，
            // 但本 trainer 校验 update_count == 0 → resume 路径无法 enable LCFR。
            // production 路径走"先决定 period_size，cold start，跑到底"。
            lcfr_period_size: None,
            lcfr_periods_completed: 0,
            lcfr_rescale_regret: true,
        })
    }
}

/// External-Sampling MCCFR DFS recurse（D-301 详解伪代码）。
///
/// 返回值语义（D-301 详解）：
/// - terminal：`utility(state, traverser)`
/// - traverser decision：`Σ_a σ(I, a) × v_a`（σ-加权 cfv 之和）
/// - non-traverser decision：sampled action 路径上的 recursed value
///
/// 参数：
/// - `state`：当前 owned 状态（D-319 owned clone state representation）
/// - `traverser`：本 step 的 traverser（D-307 alternating）
/// - `pi_trav`：traverser 自身 reach probability，用于 average strategy 权重。
///   external sampling 的 opponent/chance reach 由“是否采样到该节点”的概率隐式
///   提供，不能在 sampled path 上再次显式相乘。
/// - `regret` / `strategy_sum`：可变借用累积容器
/// - `rng`：chance + opp action sampling 共享 rng（D-315 显式注入）
fn recurse_es<G: Game>(
    state: G::State,
    traverser: PlayerId,
    pi_trav: f64,
    regret: &mut RegretTable<G::InfoSet>,
    strategy_sum: &mut StrategyAccumulator<G::InfoSet>,
    rng: &mut dyn RngSource,
) -> f64 {
    match G::current(&state) {
        NodeKind::Terminal => G::payoff(&state, traverser),
        NodeKind::Chance => {
            // D-308 chance sample-1：在 chance_distribution 上采样 1 outcome。
            // chance reach 由命中该节点的采样概率隐式提供，不更新 pi_trav。
            let dist = G::chance_distribution(&state);
            let action = sample_discrete(&dist, rng);
            let next_state = G::next(state, action, rng);
            recurse_es::<G>(next_state, traverser, pi_trav, regret, strategy_sum, rng)
        }
        NodeKind::Player(actor) => {
            let info = G::info_set(&state, actor);
            let actions = G::legal_actions(&state);
            let n = actions.len();
            // ensure regret slot exists with correct length (D-324)
            regret.get_or_init(info.clone(), n);
            // 热路径走 current_strategy_smallvec 走 SmallVec stack alloc
            // （E2-rev1 \[实现\]，API-320 surface 不变）。
            let sigma = regret.current_strategy_smallvec(&info, n);

            if actor == traverser {
                // traverser node：枚举每个 action 的 sampled counterfactual value，
                // 累积 regret，并按 traverser 自身 reach 累积 average strategy。
                // chance / opponent reach 由采样到该节点的概率隐式提供；在单条
                // sampled path 上再次乘 pi_opp 会把 reach 权重平方化。
                let weighted: ShortVec<f64> = sigma.iter().map(|s| pi_trav * s).collect();
                strategy_sum.accumulate(info.clone(), &weighted);

                let mut cfvs: ShortVec<f64> = ShortVec::with_capacity(n);
                for (i, action) in actions.iter().enumerate() {
                    let next_state = G::next(state.clone(), *action, rng);
                    let cfv = recurse_es::<G>(
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
                // External sampling estimator：opponent/chance reach is represented
                // by visitation probability, so the per-visit delta is unweighted.
                let delta: ShortVec<f64> = cfvs.iter().map(|c| c - sigma_value).collect();
                regret.accumulate(info, &delta);
                sigma_value
            } else {
                // opponent node（D-309 / D-337）：按 σ 采样 1 action。
                //
                // 过滤零概率 outcome（API-331 [`sample_discrete`] 不变量：所有
                // p > 0；零概率 action 由 caller 剔除）。当 regret matching 后
                // 某些 action 的 σ 严格为 0 时（normalized R⁺ 分布常见情形），
                // 这些 action 在采样阶段不可达，从分布中剔除即可——剩余 σ 仍
                // sum 到 1（D-330 容差）。
                //
                let nonzero_dist: ShortVec<(G::Action, f64)> = actions
                    .iter()
                    .copied()
                    .zip(sigma.iter().copied())
                    .filter(|(_, p)| *p > 0.0)
                    .collect();
                debug_assert!(
                    !nonzero_dist.is_empty(),
                    "non-traverser σ all-zero impossible: RegretTable::current_strategy 退化局面 \
                     回退均匀分布 (D-331)，sum = n_actions × (1/n_actions) = 1.0 strictly > 0"
                );
                let sampled = sample_discrete(&nonzero_dist, rng);

                let next_state = G::next(state, sampled, rng);
                recurse_es::<G>(next_state, traverser, pi_trav, regret, strategy_sum, rng)
            }
        }
    }
}

/// E2 \[实现\] 真并发 DFS recurse（D-301 详解伪代码 + D-321-rev1 真并发路径）。
///
/// 与 [`recurse_es`] 同型语义，差别仅在累积容器分流：
/// - **σ 计算（current_strategy）**：走 **共享只读** `shared_regret`
///   （[`RegretTable::current_strategy`] 对未见 InfoSet 自动回退均匀分布
///   `1 / n_actions`，等价 [`RegretTable::get_or_init`] 后查；
///   parallel 路径下不调 `get_or_init` 避免线程间 HashMap 写竞争）。
/// - **regret push**：写入 **线程本地** `LocalRegretDelta` append-only
///   `Vec<(I, SigmaVec)>`（E2-rev1 改型，原 D-321-rev1 thread-local
///   `RegretTable` 路径退役；append-only 容器在 thread 独占下零竞争，
///   merge 阶段按 push 顺序 playback 到主表，省去
///   `format!("{:?}", I)` × O(N log N) 排序）。
/// - **strategy_sum push**：写入 **线程本地** `LocalStrategyDelta`，同型。
///
/// 单线程语义偏离记录：deferred merge 让同 step 内多次访问同 InfoSet 时
/// σ 走 pre-step 状态；ES-MCCFR sample-1 trajectory 下同 step 内 InfoSet
/// 重访稀有，差异可忽略（详 [`EsMccfrTrainer::step_parallel`] doc）。
#[allow(clippy::too_many_arguments)]
fn recurse_es_parallel<G: Game>(
    state: G::State,
    traverser: PlayerId,
    pi_trav: f64,
    shared_regret: &RegretTable<G::InfoSet>,
    local_regret: &mut LocalRegretDelta<G::InfoSet>,
    local_strategy: &mut LocalStrategyDelta<G::InfoSet>,
    rng: &mut dyn RngSource,
) -> f64 {
    match G::current(&state) {
        NodeKind::Terminal => G::payoff(&state, traverser),
        NodeKind::Chance => {
            let dist = G::chance_distribution(&state);
            let action = sample_discrete(&dist, rng);
            let next_state = G::next(state, action, rng);
            recurse_es_parallel::<G>(
                next_state,
                traverser,
                pi_trav,
                shared_regret,
                local_regret,
                local_strategy,
                rng,
            )
        }
        NodeKind::Player(actor) => {
            let info = G::info_set(&state, actor);
            let actions = G::legal_actions(&state);
            let n = actions.len();
            // 共享只读：current_strategy_smallvec 对未见 InfoSet 返回均匀分布
            // (`1 / n_actions`)，等价 get_or_init 后查；parallel 路径下不写
            // 共享主表避免 HashMap 跨线程写竞争。E2-rev1：走 SmallVec hot path。
            let sigma = shared_regret.current_strategy_smallvec(&info, n);

            if actor == traverser {
                debug_assert!(
                    n > 0,
                    "traverser Player 节点必有 ≥ 1 legal action（API-300 invariant：fold 兜底）"
                );
                let weighted: SigmaVec = sigma.iter().map(|s| pi_trav * s).collect();
                local_strategy.push(info.clone(), weighted);

                let mut cfvs: ShortVec<f64> = ShortVec::with_capacity(n);
                // last iteration 消耗原 state（move 进 `G::next`）；前 n-1 次 clone。
                // 每个 traverser 节点 fan-out 省 1 次 State::clone + drop。NLHE 默认
                // 6-action 节点 → -16% clone 流量（在 traverser 分支内）。
                let last_idx = n - 1;
                for i in 0..last_idx {
                    let next_state = G::next(state.clone(), actions[i], rng);
                    let cfv = recurse_es_parallel::<G>(
                        next_state,
                        traverser,
                        pi_trav * sigma[i],
                        shared_regret,
                        local_regret,
                        local_strategy,
                        rng,
                    );
                    cfvs.push(cfv);
                }
                let last_state = G::next(state, actions[last_idx], rng);
                let cfv_last = recurse_es_parallel::<G>(
                    last_state,
                    traverser,
                    pi_trav * sigma[last_idx],
                    shared_regret,
                    local_regret,
                    local_strategy,
                    rng,
                );
                cfvs.push(cfv_last);
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                let delta: SigmaVec = cfvs.iter().map(|c| c - sigma_value).collect();
                local_regret.push(info, delta);
                sigma_value
            } else {
                let nonzero_dist: ShortVec<(G::Action, f64)> = actions
                    .iter()
                    .copied()
                    .zip(sigma.iter().copied())
                    .filter(|(_, p)| *p > 0.0)
                    .collect();
                debug_assert!(
                    !nonzero_dist.is_empty(),
                    "non-traverser σ all-zero impossible: RegretTable::current_strategy 退化局面 \
                     回退均匀分布 (D-331)，sum = n_actions × (1/n_actions) = 1.0 strictly > 0"
                );
                let sampled = sample_discrete(&nonzero_dist, rng);

                let next_state = G::next(state, sampled, rng);
                recurse_es_parallel::<G>(
                    next_state,
                    traverser,
                    pi_trav,
                    shared_regret,
                    local_regret,
                    local_strategy,
                    rng,
                )
            }
        }
    }
}

// ===========================================================================
// Checkpoint serialization helpers（D-327 / D-354）
// ===========================================================================

/// HashMap<I, Vec<f64>> → bincode-serialized bytes，按 `Ord::cmp` 排序保证跨 host
/// byte-equal（D-327）。
///
/// 输出格式 = `bincode::serialize(&Vec<(I, Vec<f64>)>::sorted)`。bincode 1.x
/// 默认走 little-endian + varint integer encoding（D-354），不依赖 host endian。
///
/// 排序键：直接走 `I: Ord` 派生比较，不再用 `format!("{:?}", I)`。
/// 旧路径每次 cmp 分配 2 个 String，总 alloc 数 O(N log N)，500K entries 实测
/// final checkpoint 写盘耗时主要被排序里的字符串分配吃掉（compute 148s vs
/// checkpoint 261s）。`Ord::cmp` 路径零分配 + O(N log N) 比较，跨 host byte-equal
/// 仍由 derived Ord 的确定性保证。
fn encode_table<I>(
    table: &std::collections::HashMap<I, Vec<f64>>,
) -> Result<Vec<u8>, CheckpointError>
where
    I: Clone + Ord + serde::Serialize,
{
    let mut entries: Vec<(I, Vec<f64>)> =
        table.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    bincode::serialize(&entries).map_err(|e| CheckpointError::Corrupted {
        offset: 0,
        reason: format!("bincode serialize regret/strategy table failed: {e}"),
    })
}

/// bincode-serialized bytes → [`RegretTable<I>`]（`encode_table` 的逆）。
fn decode_table<I>(bytes: &[u8]) -> Result<RegretTable<I>, CheckpointError>
where
    I: Clone + Eq + std::hash::Hash + std::fmt::Debug + serde::de::DeserializeOwned,
{
    let entries: Vec<(I, Vec<f64>)> =
        bincode::deserialize(bytes).map_err(|e| CheckpointError::Corrupted {
            offset: 0,
            reason: format!("bincode deserialize regret table failed: {e}"),
        })?;
    let mut table = RegretTable::new();
    for (k, v) in entries {
        // 空表上 accumulate 等价 set：get_or_init 创建 vec![0; n]，再加 delta
        // = vec![0+d; n]。
        table.accumulate(k, &v);
    }
    Ok(table)
}

/// bincode-serialized bytes → [`StrategyAccumulator<I>`]（`encode_table` 的逆，
/// 与 [`decode_table`] 输出类型不同所以独立成函数）。
fn decode_strategy<I>(bytes: &[u8]) -> Result<StrategyAccumulator<I>, CheckpointError>
where
    I: Clone + Eq + std::hash::Hash + std::fmt::Debug + serde::de::DeserializeOwned,
{
    let entries: Vec<(I, Vec<f64>)> =
        bincode::deserialize(bytes).map_err(|e| CheckpointError::Corrupted {
            offset: 0,
            reason: format!("bincode deserialize strategy table failed: {e}"),
        })?;
    let mut acc = StrategyAccumulator::new();
    for (k, v) in entries {
        acc.accumulate(k, &v);
    }
    Ok(acc)
}
