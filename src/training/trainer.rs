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
use crate::error::{CheckpointError, GameVariant, TrainerError, TrainerVariant};
use crate::training::checkpoint::{
    preflight_trainer, read_file_bytes, Checkpoint, SCHEMA_VERSION, SCHEMA_VERSION_V1,
};
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

    /// stage 4 API-403 — 6-traverser routing 入口（D-412 / D-414）。
    ///
    /// `traverser` ∈ `[0, n_players)`，返回该 traverser 视角下的 current strategy。
    /// stage 3 Kuhn / Leduc / SimplifiedNlheGame 路径（`n_players` ≤ 2）走默认
    /// 实现退化到 single-traverser [`Self::current_strategy`]；stage 4
    /// `NlheGame6` 路径 C2 \[实现\] 起步前 override 走 6 套独立 RegretTable
    /// 数组 + traverser routing。
    fn current_strategy_for_traverser(
        &self,
        traverser: PlayerId,
        info_set: &G::InfoSet,
    ) -> Vec<f64> {
        let _ = traverser;
        self.current_strategy(info_set)
    }

    /// stage 4 API-403 — 6-traverser average strategy routing（D-412 / D-414）。
    ///
    /// 同 [`Self::current_strategy_for_traverser`] 默认退化到
    /// [`Self::average_strategy`]；C2 \[实现\] 落地 `NlheGame6` override。
    fn average_strategy_for_traverser(
        &self,
        traverser: PlayerId,
        info_set: &G::InfoSet,
    ) -> Vec<f64> {
        let _ = traverser;
        self.average_strategy(info_set)
    }
}

/// stage 4 D-401-revM — `EsMccfrTrainer` Linear discounting eager vs lazy 选型
/// （API-401）。
///
/// `EagerDecay` 是 A0 \[决策\] 默认，每 iter 起始扫全表应用 decay factor；
/// `LazyDecay` 是 D-401-revM 候选，每 entry 存 `(value, last_update_count_t)`
/// tuple 让 query 时延迟应用。B2 \[实现\] 起步前根据 stage 3 §8.1 carry-forward
/// (I) perf flamegraph 实测 lock 选项。
#[derive(Clone, Copy, Eq, PartialEq, Debug, Default)]
pub enum DecayStrategy {
    /// stage 4 A0 默认 — eager decay，每 iter 起始扫全表应用 decay factor。
    #[default]
    EagerDecay,
    /// stage 4 D-401-revM 候选 — lazy decay；query 时延迟应用 decay factor。
    /// B2 \[实现\] 起步前 evaluate 是否翻面。
    LazyDecay,
}

/// stage 4 API-401 — `EsMccfrTrainer` 配置字段聚合。
///
/// stage 3 既有字段 (`n_threads` / `checkpoint_interval` / `metrics_interval`)
/// 在 stage 3 trainer 内部分散；stage 4 A1 \[实现\] 起步阶段聚合到本 struct，
/// 配 `EsMccfrTrainer::config: TrainerConfig` 字段。stage 3 `EsMccfrTrainer::new(...)`
/// 路径维持 `TrainerConfig::default()`（linear_weighting / rm_plus / warmup_complete
/// 全部 disable，等价 stage 3 standard CFR + RM 路径 byte-equal 保持）；stage 4
/// 路径走 `EsMccfrTrainer::with_linear_rm_plus(warmup_complete_at)` builder 切到
/// Linear MCCFR + RM+ 模式。
///
/// **A1 \[实现\] 状态**：struct 签名锁；字段语义在 B2 \[实现\] 起步前根据
/// flamegraph 实测可能调整 `decay_strategy` 默认值。
#[derive(Clone, Copy, Debug)]
pub struct TrainerConfig {
    /// stage 3 既有 — 并发线程数（`step_parallel` 入参 `n_threads`，与本字段
    /// 解耦让 stage 3 模式不必构造 `TrainerConfig`）。
    pub n_threads: u8,
    /// stage 3 既有 — checkpoint cadence。
    pub checkpoint_interval: u64,
    /// stage 3 既有 — metrics observe cadence（stage 4 D-476 字面继承
    /// 10⁵ update 默认）。
    pub metrics_interval: u64,

    /// stage 4 D-401 — Linear discounting on/off。
    pub linear_weighting_enabled: bool,
    /// stage 4 D-402 — RM+ clamp on/off。
    pub rm_plus_enabled: bool,
    /// stage 4 D-409 — warm-up phase 长度（默认 1_000_000 update）。warm-up
    /// 期间走 stage 3 standard CFR + RM 路径 byte-equal 保持（D-409
    /// 字面继承 stage 3 1M update × 3 BLAKE3 anchor）；切换后下一次
    /// `step()` 起触发路径切换。
    pub warmup_complete_at: u64,
    /// stage 4 D-401-revM — eager vs lazy decay 选型（详见 [`DecayStrategy`]）。
    pub decay_strategy: DecayStrategy,
}

impl Default for TrainerConfig {
    fn default() -> Self {
        Self {
            n_threads: 1,
            checkpoint_interval: 0,
            metrics_interval: 100_000,
            linear_weighting_enabled: false,
            rm_plus_enabled: false,
            warmup_complete_at: 1_000_000,
            decay_strategy: DecayStrategy::EagerDecay,
        }
    }
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
        // stage 4 D2 \[实现\]：VanillaCfrTrainer 仍写 schema=1（stage 3
        // byte-equal 维持），4 个 stage 4 字段以默认值占位让 Checkpoint struct
        // 字段集 12-field 在 v1 / v2 路径下统一构造。
        let ckpt = Checkpoint {
            schema_version: SCHEMA_VERSION_V1,
            trainer_variant: TrainerVariant::VanillaCfr,
            game_variant: G::VARIANT,
            update_count: self.iter,
            rng_state: self.rng_substream_seed,
            bucket_table_blake3: self.game.bucket_table_blake3(),
            regret_table_bytes,
            strategy_sum_bytes,
            traverser_count: 1,
            linear_weighting_enabled: false,
            rm_plus_enabled: false,
            warmup_complete: false,
        };
        ckpt.save(path)
    }

    fn load_checkpoint(path: &Path, game: G) -> Result<Self, CheckpointError>
    where
        Self: Sized,
    {
        let bytes = read_file_bytes(path)?;
        // VanillaCfrTrainer 只支持 schema=1（stage 3 path）。stage 4 schema=2
        // 文件（EsMccfrLinearRmPlus 写出）经此入口加载 → SchemaMismatch。
        ensure_trainer_schema(&bytes, SCHEMA_VERSION_V1)?;
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

/// stage 4 D2 \[实现\] — trainer 侧 schema_version 预检（D-449 字面）。
///
/// 在 [`preflight_trainer`] 与 [`Checkpoint::parse_bytes`] 之前 eager 校验
/// file `schema_version` 字段与 trainer expected 是否一致；命中 mismatch 立即
/// 返回 [`CheckpointError::SchemaMismatch { expected, got }`]。
///
/// 行为细节：
/// - 文件过短 / magic 错误 → 不预拦截（让后续 [`Checkpoint::parse_bytes`] 走
///   标准 dispatch 返 Corrupted）。
/// - 文件 schema == expected → `Ok(())`。
/// - 文件 schema != expected → 立即 `Err(SchemaMismatch)`。
fn ensure_trainer_schema(bytes: &[u8], expected_schema: u32) -> Result<(), CheckpointError> {
    use crate::training::checkpoint::{HEADER_LEN_V1, MAGIC};
    if bytes.len() < HEADER_LEN_V1 {
        return Ok(());
    }
    if bytes[0..8] != MAGIC {
        return Ok(());
    }
    let schema = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    if schema != expected_schema {
        return Err(CheckpointError::SchemaMismatch {
            expected: expected_schema,
            got: schema,
        });
    }
    Ok(())
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
    /// stage 4 API-401 — trainer 配置（默认 stage 3 standard CFR + RM 路径，
    /// `with_linear_rm_plus()` builder 切到 stage 4 Linear MCCFR + RM+ 模式）。
    pub(crate) config: TrainerConfig,

    /// stage 4 E2 \[实现\] — 6-traverser independent table arrays（D-412 字面
    /// "6 套独立 RegretTable + StrategyAccumulator 每 traverser 1 套"）。
    ///
    /// `None` 时 trainer 走 single shared `regret` / `strategy_sum` 路径
    /// （stage 3 + warm-up phase + 非 NlheGame6 G 全部走此路径，stage 1/2/3
    /// BLAKE3 byte-equal anchor 不变）。`Some(...)` 由首次 post-warmup
    /// Linear+RM+ step 触发 lazy 初始化（[`Self::ensure_per_traverser_initialized`]）
    /// 走 deep-clone single shared 表 × `n_players` 复制实现 — 每 traverser
    /// 从 warmup 出口的同一份共享 state 起步独立累积（D-414 字面 "cross-traverser
    /// regret 不共享"）。
    ///
    /// 一旦激活，trainer step / step_parallel 在每次 `update_count` 增 1 之前
    /// 路由到 `per_traverser[traverser]` 的 regret + strategy_sum 上累积，
    /// 单 shared `regret` / `strategy_sum` 在 post-warmup 阶段不再被写入
    /// （读路径仍可作为 fallback，但 query API 通常走 per-traverser override）。
    pub(crate) per_traverser: Option<PerTraverserTables<G::InfoSet>>,
}

/// stage 4 E2 \[实现\] — 6-traverser independent table 容器（[`EsMccfrTrainer::
/// per_traverser`] 字段类型；D-412 字面）。
///
/// `Vec<RegretTable<I>>` 长度 = `n_players`（NlheGame6 = 6；HU 退化 = 2）；
/// `Vec<StrategyAccumulator<I>>` 同长。`new_initialized_from_shared` 通过
/// `Clone::clone(shared_table)` × `n_players` 复制实现初始化（`RegretTable` /
/// `StrategyAccumulator` 在 stage 4 E2 \[实现\] 起步加 `Clone` derive）。
pub struct PerTraverserTables<I: std::hash::Hash + Eq + Clone> {
    pub regret: Vec<RegretTable<I>>,
    pub strategy_sum: Vec<StrategyAccumulator<I>>,
}

impl<I: std::hash::Hash + Eq + Clone> PerTraverserTables<I> {
    /// 从 single shared `(regret, strategy_sum)` 复制初始化 `n_players` 套
    /// 独立 table（每 traverser 一套）。
    pub fn new_initialized_from_shared(
        shared_regret: &RegretTable<I>,
        shared_strategy: &StrategyAccumulator<I>,
        n_players: usize,
    ) -> Self {
        let regret = (0..n_players).map(|_| shared_regret.clone()).collect();
        let strategy_sum = (0..n_players).map(|_| shared_strategy.clone()).collect();
        Self {
            regret,
            strategy_sum,
        }
    }

    /// 构造 `n_players` 套空 table（reload v2 checkpoint sub-region body 时入口）。
    pub fn new_empty(n_players: usize) -> Self {
        let regret = (0..n_players).map(|_| RegretTable::new()).collect();
        let strategy_sum = (0..n_players).map(|_| StrategyAccumulator::new()).collect();
        Self {
            regret,
            strategy_sum,
        }
    }
}

impl<G: Game> EsMccfrTrainer<G> {
    /// 新建空 Trainer（API-312）。`master_seed` 用 D-335 SplitMix64 finalizer ×
    /// 4 派生 32 byte sub-stream seed 占位（D2 checkpoint 序列化时存档；step
    /// 本身不消费——`step` 接受的 `rng: &mut dyn RngSource` 是唯一 randomness
    /// 来源）。
    ///
    /// stage 4 `config` 字段默认 [`TrainerConfig::default()`]（linear_weighting /
    /// rm_plus / warmup_complete 全部 disable，等价 stage 3 standard CFR + RM
    /// 路径 byte-equal 保持）；走 stage 4 路径调用 [`Self::with_linear_rm_plus`]
    /// builder 切入。
    pub fn new(game: G, master_seed: u64) -> Self {
        let rng_substream_seed = derive_substream_seed(master_seed, 0, 0);
        Self {
            game,
            regret: RegretTable::new(),
            strategy_sum: StrategyAccumulator::new(),
            update_count: 0,
            rng_substream_seed,
            config: TrainerConfig::default(),
            per_traverser: None,
        }
    }

    /// stage 4 E2 \[实现\] — 6-traverser per-traverser table 激活条件
    /// （NlheGame6 + Linear+RM+ + post-warmup）。
    ///
    /// 返回 `true` 即 trainer 已切入 D-412 字面 "6 套独立表" 路径，step /
    /// step_parallel + query API 都走 `per_traverser` 数组；返回 `false` 即
    /// 维持 single shared regret + strategy_sum（stage 3 byte-equal anchor /
    /// warm-up phase / 非 NlheGame6 路径）。
    pub fn per_traverser_active(&self) -> bool {
        self.per_traverser.is_some()
    }

    /// stage 4 E2 \[实现\] — lazy 初始化 [`Self::per_traverser`]（首次
    /// post-warmup Linear+RM+ NlheGame6 step 触发；其它入口不调用）。
    ///
    /// 不变量：调用前 `per_traverser` 必须为 `None`；调用后变为 `Some(...)`
    /// 长度 = `n_players`（NlheGame6 = 6 / HU 退化 = 2）。
    fn ensure_per_traverser_initialized(&mut self) {
        if self.per_traverser.is_none() {
            self.per_traverser = Some(PerTraverserTables::new_initialized_from_shared(
                &self.regret,
                &self.strategy_sum,
                self.game.n_players(),
            ));
        }
    }

    /// 当前 step / step_parallel 是否应路由到 per-traverser 表数组。
    fn should_use_per_traverser(&self) -> bool {
        let warm_up_done = self.update_count >= self.config.warmup_complete_at;
        self.config.linear_weighting_enabled
            && self.config.rm_plus_enabled
            && G::VARIANT == GameVariant::Nlhe6Max
            && warm_up_done
    }

    /// stage 4 API-400 — 切到 Linear MCCFR + RM+ 模式（D-400 / D-401 / D-402 /
    /// D-403 + D-409 warm-up）。
    ///
    /// **不变量**：
    /// - 切换之前累积的 regret / strategy_sum 保留不动（warmup phase 1M update
    ///   走 stage 3 standard CFR + RM 路径 byte-equal 保持，stage 3 BLAKE3 anchor
    ///   1M update × 3 不变量在 stage 4 warmup phase 必须重现一致）。
    /// - 切换后下一次 [`Trainer::step`] 起触发 D-409 warm-up phase 检查：
    ///   `update_count < warmup_complete_at` 走 stage 3 路径，
    ///   `update_count >= warmup_complete_at` 走 stage 4 路径。
    /// - Deterministic 切换边界：切换点 `update_count = warmup_complete_at`
    ///   的那一个 step 必须 byte-equal across multiple runs（warmup_complete
    ///   状态进 checkpoint header，API-440 D-446 字面）。
    ///
    /// **A1 \[实现\] 状态**：方法体只更新 config 字段（B2 \[实现\] 落地实际
    /// step 路径切换；warm-up boundary deterministic byte-equal 由 B1 \[测试\]
    /// 钉死）。
    pub fn with_linear_rm_plus(mut self, warmup_complete_at: u64) -> Self {
        self.config.linear_weighting_enabled = true;
        self.config.rm_plus_enabled = true;
        self.config.warmup_complete_at = warmup_complete_at;
        self
    }

    /// stage 4 API-401 — 公开 read-only config（B2 \[实现\] 起步前评估是否
    /// 转 `pub config: TrainerConfig` 字段；A1 \[实现\] 走 getter 占位）。
    pub fn config(&self) -> &TrainerConfig {
        &self.config
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
    /// **rayon pool 替 `std::thread::scope`**（F1-rev1 vultr 实测加速比仅
    /// 1.14× 的根因之一 = 12,500 次 step_parallel × 4 OS thread spawn 开销
    /// ≈ 1-2 s overhead；rayon 全局 pool 复用长寿命 worker，scope-fifo 任务
    /// 分发 ≈ ns 级 atomic dequeue）。`par_iter_mut().enumerate().collect()`
    /// 对 [`IndexedParallelIterator`] 保 input 顺序，等价原 `Vec::map(spawn).
    /// map(join)` 的 tid-顺序输出。
    ///
    /// **append-only delta 替 thread-local `RegretTable`**（F1-rev1 实测
    /// batch merge sort `format!("{:?}", InfoSetId)` × O(N log N) 占主导
    /// merge cost）：每线程持有 `LocalRegretDelta` / `LocalStrategyDelta`
    /// = `Vec<(I, SigmaVec)>` 按 DFS 顺序 append；merge 阶段按 tid 升序 ×
    /// 每 thread 内 push 顺序 playback 到主表。
    ///
    /// **线程内语义**：σ 计算走只读共享 `&self.regret`
    /// （[`RegretTable::current_strategy`] 对 HashMap 无锁只读在 rayon 任务
    /// 借用期内 by-design 安全；HashMap 未触发结构 rehash 因主表不被任何
    /// worker 写）；regret + strategy_sum 累积全 push 到线程内本地 delta vec。
    ///
    /// **跨 run 决定性**（D-362 carry-forward）：append-only 路径下 thread
    /// 内 push 顺序 = DFS 顺序 deterministic（rng 决定 sampled trajectory）；
    /// tid 顺序 deterministic（`par_iter_mut().enumerate().collect()` 保
    /// index 顺序）；同 InfoSet 多次访问按 push 顺序 playback，f64 加法序列
    /// 与原 thread-local table accumulate 后再合并完全等价（数值结果恒等）。
    /// BLAKE3 byte-equal 不破（test_5 1M update × 3 走单线程 `step`，本路径
    /// 修改不触达；step_parallel-only 测试在 perf_slo.rs 仅断言 throughput
    /// 不断言数值）。
    ///
    /// **与单线程 `step` 的语义差异**：deferred merge → 同 step 内多次访问同
    /// InfoSet 时 σ 走 pre-step 状态而非 in-step 累积；ES-MCCFR sample-1
    /// trajectory 下同 step 内 InfoSet 重访稀有，差异可忽略；D-362 单线程 1M
    /// update × 3 BLAKE3 路径不消费 `step_parallel`（`tests/cfr_simplified_nlhe.rs`
    /// Test 5 走纯 single-threaded `step`），byte-equal 不受影响。
    ///
    /// **边界**：`rng_pool.is_empty()` 或 `n_threads == 0` → no-op，返回 `Ok(())`；
    /// `n_active > rng_pool.len()` 时截断到 `rng_pool.len()`。
    pub fn step_parallel(
        &mut self,
        rng_pool: &mut [Box<dyn RngSource>],
        n_threads: usize,
    ) -> Result<(), TrainerError>
    where
        G: Sync,
        G::InfoSet: Send,
    {
        let n_active = n_threads.min(rng_pool.len());
        if n_active == 0 {
            return Ok(());
        }

        // stage 4 E2 \[实现\] — 6-traverser table-array dispatch（§D2-revM 翻面）。
        // post-warmup Linear+RM+ NlheGame6 路径下 step_parallel 走
        // `[RegretTable; n_players]` + `[StrategyAccumulator; n_players]`
        // 数组：每线程读 traverser 自己的 `regret[traverser]` 作为 σ 共享只读
        // 源（D-414 字面 cross-traverser 不共享），写入到线程本地
        // LocalRegretDelta / LocalStrategyDelta；merge 阶段按 (tid 顺序 →
        // 对应 traverser) 把 delta playback 到该 traverser 的 table（多个 tid
        // 指向同一 traverser 时按 tid 升序串行 playback，保跨 run 决定性）。
        // ensure_per_traverser_initialized 必须在任何 `&self.*` 不可变借用
        // （`game` / `tables` / `regret_ref`）之前完成，避免与可变借用冲突。
        let use_per_traverser = self.should_use_per_traverser();
        if use_per_traverser {
            self.ensure_per_traverser_initialized();
        }

        let active_pool = &mut rng_pool[..n_active];
        let n_players = self.game.n_players() as u64;
        let base_update_count = self.update_count;
        let game = &self.game;

        if use_per_traverser {
            let tables = self
                .per_traverser
                .as_ref()
                .expect("ensure_per_traverser_initialized 已激活 per_traverser");
            let regret_ref = &tables.regret;

            #[allow(clippy::type_complexity)]
            let deltas: Vec<(
                PlayerId,
                LocalRegretDelta<G::InfoSet>,
                LocalStrategyDelta<G::InfoSet>,
            )> = active_pool
                .par_iter_mut()
                .enumerate()
                .map(|(tid, rng_slot)| {
                    let traverser = ((base_update_count + tid as u64) % n_players) as PlayerId;
                    let shared_regret_for_traverser = &regret_ref[traverser as usize];
                    let mut local_regret = LocalRegretDelta::<G::InfoSet>::new();
                    let mut local_strategy = LocalStrategyDelta::<G::InfoSet>::new();
                    let rng = rng_slot.as_mut();
                    let root = game.root(rng);
                    recurse_es_parallel::<G>(
                        root,
                        traverser,
                        1.0,
                        1.0,
                        shared_regret_for_traverser,
                        &mut local_regret,
                        &mut local_strategy,
                        rng,
                    );
                    (traverser, local_regret, local_strategy)
                })
                .collect();

            // merge — 按 tid 升序 playback 到 per-traverser 表（保跨 run BLAKE3
            // 决定性同 single-shared 路径同型政策）。
            let tables = self.per_traverser.as_mut().expect("per_traverser 已激活");
            for (traverser, local_regret, local_strategy) in deltas {
                let r = &mut tables.regret[traverser as usize];
                let s = &mut tables.strategy_sum[traverser as usize];
                for (info, delta) in local_regret.into_entries() {
                    r.accumulate(info, &delta);
                }
                for (info, weighted) in local_strategy.into_entries() {
                    s.accumulate(info, &weighted);
                }
            }
        } else {
            // single-shared 路径（stage 3 + warm-up phase + 非 NlheGame6 G 全部
            // 走此分支，E2-rev1 stage 3 D-321-rev2 路径不退化）。
            let shared_regret: &RegretTable<G::InfoSet> = &self.regret;

            #[allow(clippy::type_complexity)]
            let deltas: Vec<(
                LocalRegretDelta<G::InfoSet>,
                LocalStrategyDelta<G::InfoSet>,
            )> = active_pool
                .par_iter_mut()
                .enumerate()
                .map(|(tid, rng_slot)| {
                    let traverser = ((base_update_count + tid as u64) % n_players) as PlayerId;
                    let mut local_regret = LocalRegretDelta::<G::InfoSet>::new();
                    let mut local_strategy = LocalStrategyDelta::<G::InfoSet>::new();
                    let rng = rng_slot.as_mut();
                    let root = game.root(rng);
                    recurse_es_parallel::<G>(
                        root,
                        traverser,
                        1.0,
                        1.0,
                        shared_regret,
                        &mut local_regret,
                        &mut local_strategy,
                        rng,
                    );
                    (local_regret, local_strategy)
                })
                .collect();

            for (local_regret, local_strategy) in deltas {
                for (info, delta) in local_regret.into_entries() {
                    self.regret.accumulate(info, &delta);
                }
                for (info, weighted) in local_strategy.into_entries() {
                    self.strategy_sum.accumulate(info, &weighted);
                }
            }
        }
        self.update_count += n_active as u64;
        Ok(())
    }
}

impl<G: Game> Trainer<G> for EsMccfrTrainer<G> {
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        // D-409 warm-up phase routing：前 warmup_complete_at update 走 stage 3
        // standard CFR + RM 路径 byte-equal 保持（stage 3 D-362 1M update × 3
        // BLAKE3 anchor 不退化）；warmup_complete_at 之后切 stage 4 Linear
        // MCCFR + RM+ 路径。当 `with_linear_rm_plus()` 未调用时
        // `config.linear_weighting_enabled = config.rm_plus_enabled = false`，
        // 与 stage 3 路径完全等价（B1 Test 1 anchor）。
        let warm_up_done = self.update_count >= self.config.warmup_complete_at;
        let use_linear = self.config.linear_weighting_enabled && warm_up_done;
        let use_rm_plus = self.config.rm_plus_enabled && warm_up_done;

        // D-401 字面 `R̃_t(I, a) = (t / (t + 1)) × R̃_{t-1}(I, a) + r_t(I, a)`
        // 中的 t：stage 4 phase 内 1-indexed iter counter。warm-up phase 后
        // 第一 step 起 t=1（让 t=1 时 decay factor = 1/2 应用于 R̃_0 = 0 退化
        // 不影响数值，与 stage 3 路径在 t=1 处 σ byte-equal — B1 Test 7 字面
        // sanity anchor）。
        let t_stage4: u64 = if use_linear {
            self.update_count - self.config.warmup_complete_at + 1
        } else {
            0
        };

        let n_players = self.game.n_players() as u64;
        let traverser = (self.update_count % n_players) as PlayerId;
        let root = self.game.root(rng);
        let strategy_sum_weight: f64 = if use_linear { t_stage4 as f64 } else { 1.0 };

        // stage 4 E2 \[实现\] — 6-traverser table-array dispatch（§D2-revM
        // table-array deferral 翻面）：post-warmup Linear+RM+ NlheGame6 路径
        // 走 [`PerTraverserTables`] per-traverser table 数组；其它路径维持
        // single shared regret + strategy_sum（stage 1/2/3 byte-equal）。
        if self.should_use_per_traverser() {
            self.ensure_per_traverser_initialized();
            let tables = self
                .per_traverser
                .as_mut()
                .expect("ensure_per_traverser_initialized 已激活 per_traverser");
            let regret = &mut tables.regret[traverser as usize];
            let strategy_sum = &mut tables.strategy_sum[traverser as usize];

            // 步骤 1：D-401 Linear discounting eager decay（per-traverser
            // table 上 in-place 乘 t/(t+1)）。
            if use_linear {
                let decay = (t_stage4 as f64) / ((t_stage4 + 1) as f64);
                regret.apply_decay(decay);
            }
            // 步骤 2：标准 ES-MCCFR DFS recurse + D-403 Linear weighted
            // strategy sum 累积（与 single-shared 路径同型）。
            recurse_es::<G>(
                root,
                traverser,
                1.0,
                1.0,
                regret,
                strategy_sum,
                rng,
                strategy_sum_weight,
            );
            // 步骤 3：D-402 RM+ in-place clamp（per-traverser table 上）。
            if use_rm_plus {
                regret.clamp_nonneg();
            }
        } else {
            // single-shared 路径（stage 3 byte-equal + warm-up phase + 非
            // NlheGame6 G 全部走此分支）。
            // 步骤 1：D-401 Linear discounting eager decay。
            if use_linear {
                let decay = (t_stage4 as f64) / ((t_stage4 + 1) as f64);
                self.regret.apply_decay(decay);
            }
            // 步骤 2：标准 ES-MCCFR DFS recurse + D-403。
            recurse_es::<G>(
                root,
                traverser,
                1.0,
                1.0,
                &mut self.regret,
                &mut self.strategy_sum,
                rng,
                strategy_sum_weight,
            );
            // 步骤 3：D-402 RM+ in-place clamp。
            if use_rm_plus {
                self.regret.clamp_nonneg();
            }
        }

        self.update_count += 1;
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

    /// stage 4 E2 \[实现\] — 6-traverser per-traverser query 路由（D-414 字面）。
    ///
    /// `per_traverser` 激活时（NlheGame6 + Linear+RM+ + post-warmup）读
    /// `per_traverser[traverser].regret` 上的 current strategy；其它路径退化
    /// 到 single-shared `Self::current_strategy`（stage 3 + Kuhn / Leduc /
    /// SimplifiedNlhe / warm-up phase 全部走此 fallback）。
    fn current_strategy_for_traverser(
        &self,
        traverser: PlayerId,
        info_set: &G::InfoSet,
    ) -> Vec<f64> {
        if let Some(tables) = self.per_traverser.as_ref() {
            let idx = traverser as usize;
            if idx < tables.regret.len() {
                let regret = &tables.regret[idx];
                let strategy_sum = &tables.strategy_sum[idx];
                let n = regret
                    .inner()
                    .get(info_set)
                    .map(|v| v.len())
                    .or_else(|| strategy_sum.inner().get(info_set).map(|v| v.len()))
                    .unwrap_or(0);
                if n > 0 {
                    return regret.current_strategy(info_set, n);
                }
            }
        }
        self.current_strategy(info_set)
    }

    /// stage 4 E2 \[实现\] — 6-traverser average strategy 路由（D-414 字面）。
    ///
    /// 语义同 [`Self::current_strategy_for_traverser`]，作用于
    /// `strategy_sum` 表。LBR `compute_six_traverser_average` /
    /// `export_policy_for_openspiel` 走此入口取 per-traverser blueprint。
    fn average_strategy_for_traverser(
        &self,
        traverser: PlayerId,
        info_set: &G::InfoSet,
    ) -> Vec<f64> {
        if let Some(tables) = self.per_traverser.as_ref() {
            let idx = traverser as usize;
            if idx < tables.strategy_sum.len() {
                let strategy_sum = &tables.strategy_sum[idx];
                let regret = &tables.regret[idx];
                let n = strategy_sum
                    .inner()
                    .get(info_set)
                    .map(|v| v.len())
                    .or_else(|| regret.inner().get(info_set).map(|v| v.len()))
                    .unwrap_or(0);
                if n > 0 {
                    return strategy_sum.average_strategy(info_set, n);
                }
            }
        }
        self.average_strategy(info_set)
    }

    fn update_count(&self) -> u64 {
        self.update_count
    }

    fn save_checkpoint(&self, path: &Path) -> Result<(), CheckpointError> {
        // stage 4 D2 \[实现\] — schema_version dispatch（D-449 字面）：
        // - G == NlheGame6 AND linear_weighting && rm_plus → 走 v2 path
        //   (schema_version=2, trainer=EsMccfrLinearRmPlus, 4 个 stage 4
        //    字段从 config 持久化)；当 [`Self::per_traverser`] 激活时 body
        //    走 6-region encoding (E2 \[实现\] 翻面)，否则走 single-region
        //    encoding 与 traverser_count=1 字面（warm-up 阶段 save）。
        // - 其它（含 NlheGame6 default-config 与 SimplifiedNlheGame）→ 走 v1 path
        //   (schema_version=1, trainer=EsMccfr, 4 个 stage 4 字段以默认值占位)。
        let stage4_path = G::VARIANT == GameVariant::Nlhe6Max
            && self.config.linear_weighting_enabled
            && self.config.rm_plus_enabled;
        let ckpt = if stage4_path {
            let warmup_complete = self.update_count >= self.config.warmup_complete_at;
            // stage 4 E2 \[实现\] — per-traverser table 激活时走 6-region body
            // encoding（D-412 字面 6 套独立表）；非激活路径（pre-warmup save）
            // 走 single-region encoding 与 traverser_count=1 字面（与 stage 3
            // path body 同型，warm-up 阶段 BLAKE3 byte-equal 不破）。
            let (regret_table_bytes, strategy_sum_bytes, traverser_count) =
                if let Some(tables) = self.per_traverser.as_ref() {
                    let regret_inner: Vec<&std::collections::HashMap<G::InfoSet, Vec<f64>>> =
                        tables.regret.iter().map(|t| t.inner()).collect();
                    let strategy_inner: Vec<&std::collections::HashMap<G::InfoSet, Vec<f64>>> =
                        tables.strategy_sum.iter().map(|t| t.inner()).collect();
                    (
                        encode_multi_table(&regret_inner)?,
                        encode_multi_table(&strategy_inner)?,
                        tables.regret.len() as u8,
                    )
                } else {
                    (
                        encode_table(self.regret.inner())?,
                        encode_table(self.strategy_sum.inner())?,
                        1u8,
                    )
                };
            Checkpoint {
                schema_version: SCHEMA_VERSION,
                trainer_variant: TrainerVariant::EsMccfrLinearRmPlus,
                game_variant: G::VARIANT,
                update_count: self.update_count,
                rng_state: self.rng_substream_seed,
                bucket_table_blake3: self.game.bucket_table_blake3(),
                regret_table_bytes,
                strategy_sum_bytes,
                traverser_count,
                linear_weighting_enabled: true,
                rm_plus_enabled: true,
                warmup_complete,
            }
        } else {
            let regret_table_bytes = encode_table(self.regret.inner())?;
            let strategy_sum_bytes = encode_table(self.strategy_sum.inner())?;
            Checkpoint {
                schema_version: SCHEMA_VERSION_V1,
                trainer_variant: TrainerVariant::EsMccfr,
                game_variant: G::VARIANT,
                update_count: self.update_count,
                rng_state: self.rng_substream_seed,
                bucket_table_blake3: self.game.bucket_table_blake3(),
                regret_table_bytes,
                strategy_sum_bytes,
                traverser_count: 1,
                linear_weighting_enabled: false,
                rm_plus_enabled: false,
                warmup_complete: false,
            }
        };
        ckpt.save(path)
    }

    fn load_checkpoint(path: &Path, game: G) -> Result<Self, CheckpointError>
    where
        Self: Sized,
    {
        let bytes = read_file_bytes(path)?;

        // stage 4 D2 \[实现\] — schema dispatch（D-449 字面）：
        // - G == NlheGame6 → 接受 v1 与 v2（v1 走 HU 退化兼容 / v2 走 Linear+RM+ 主路径）。
        // - 其它 G → 严格 v1（stage 3 path）；v2 文件经此入口 → SchemaMismatch(expected=1, got=2)。
        let (expected_trainer, expected_config) = if G::VARIANT == GameVariant::Nlhe6Max {
            // NlheGame6 接受两个 schema；trainer_variant 预检按文件 schema 字段分流。
            let schema = peek_schema(&bytes);
            match schema {
                Some(SCHEMA_VERSION) => (
                    TrainerVariant::EsMccfrLinearRmPlus,
                    build_linear_rm_plus_config(&self_default_config_nlhe6max(), &bytes),
                ),
                _ => (TrainerVariant::EsMccfr, TrainerConfig::default()),
            }
        } else {
            ensure_trainer_schema(&bytes, SCHEMA_VERSION_V1)?;
            (TrainerVariant::EsMccfr, TrainerConfig::default())
        };

        preflight_trainer(
            &bytes,
            expected_trainer,
            G::VARIANT,
            game.bucket_table_blake3(),
        )?;
        let ckpt = Checkpoint::parse_bytes(&bytes)?;

        // stage 4 E2 \[实现\] — body sub-region encoding dispatch（D-412 字面）。
        // - traverser_count <= 1 → single-region body（stage 3 / pre-warmup v2 save）
        // - traverser_count > 1 → 6-region body 解码到 per_traverser 数组
        let (regret, strategy_sum, per_traverser) = if ckpt.traverser_count as usize > 1 {
            let regret_tables =
                decode_multi_regret::<G::InfoSet>(&ckpt.regret_table_bytes, ckpt.traverser_count)?;
            let strategy_tables = decode_multi_strategy::<G::InfoSet>(
                &ckpt.strategy_sum_bytes,
                ckpt.traverser_count,
            )?;
            // 单 shared 表保持空（per_traverser 激活后 trainer 不再读写
            // self.regret/strategy_sum；保留构造让 struct 字段非 Option）。
            (
                RegretTable::new(),
                StrategyAccumulator::new(),
                Some(PerTraverserTables {
                    regret: regret_tables,
                    strategy_sum: strategy_tables,
                }),
            )
        } else {
            let regret = decode_table::<G::InfoSet>(&ckpt.regret_table_bytes)?;
            let strategy_sum = decode_strategy::<G::InfoSet>(&ckpt.strategy_sum_bytes)?;
            (regret, strategy_sum, None)
        };

        Ok(Self {
            game,
            regret,
            strategy_sum,
            update_count: ckpt.update_count,
            rng_substream_seed: ckpt.rng_state,
            // stage 4 D2 \[实现\]：schema=2 路径 reconstruct TrainerConfig
            // （linear_weighting / rm_plus / warmup_complete_at 从 header
            // 4 字段还原；warmup_complete 字段反推：true → warmup_complete_at = 0
            //  / false → 沿用 default 1_000_000，因 update_count < default
            //  时 step path 自然走 warmup 路径，与 byte-equal 不变量一致）。
            config: expected_config,
            per_traverser,
        })
    }
}

/// peek 文件 `schema_version` 字段；文件不合法或 magic 错误 → `None`。
fn peek_schema(bytes: &[u8]) -> Option<u32> {
    use crate::training::checkpoint::{HEADER_LEN_V1, MAGIC};
    if bytes.len() < HEADER_LEN_V1 {
        return None;
    }
    if bytes[0..8] != MAGIC {
        return None;
    }
    Some(u32::from_le_bytes(bytes[8..12].try_into().unwrap()))
}

/// stage 4 D2 \[实现\] — NlheGame6 + schema=2 path 默认 TrainerConfig 重建。
///
/// 从 v2 header 4 字段还原 `linear_weighting_enabled` /
/// `rm_plus_enabled` / `warmup_complete_at`（warmup_complete bool → effective
/// warmup_complete_at 字段）。读取 4 字段失败 / 文件不是 v2 → 落 default。
fn build_linear_rm_plus_config(default: &TrainerConfig, bytes: &[u8]) -> TrainerConfig {
    use crate::training::checkpoint::{
        HEADER_LEN, OFFSET_LINEAR_WEIGHTING, OFFSET_RM_PLUS, OFFSET_WARMUP_COMPLETE,
    };
    if bytes.len() < HEADER_LEN {
        return *default;
    }
    let linear = bytes[OFFSET_LINEAR_WEIGHTING] == 1;
    let rm_plus = bytes[OFFSET_RM_PLUS] == 1;
    let warmup_complete = bytes[OFFSET_WARMUP_COMPLETE] == 1;
    TrainerConfig {
        linear_weighting_enabled: linear,
        rm_plus_enabled: rm_plus,
        // warmup_complete=true → warmup_at=0 让 reload 后第一个 step 立即走
        // Linear+RM+ 路径（与 save 之前的 trainer state 一致）；
        // warmup_complete=false → 维持 default 让 step 路径继续 stage 3 path 直到
        // update_count 跨边界（与 stage 3 anchor 兼容）。
        warmup_complete_at: if warmup_complete {
            0
        } else {
            default.warmup_complete_at
        },
        ..*default
    }
}

/// stage 4 D2 \[实现\] — NlheGame6 path 的 trainer default config helper。
///
/// 让 [`<EsMccfrTrainer<G> as Trainer<G>>::load_checkpoint`] 在 generic G 路径下
/// 拿到 `TrainerConfig::default()` 让 [`build_linear_rm_plus_config`] override
/// 4 个 stage 4 字段。函数避免在 trait impl 内部使用关键字。
fn self_default_config_nlhe6max() -> TrainerConfig {
    TrainerConfig::default()
}

/// External-Sampling MCCFR DFS recurse（D-301 详解伪代码）。
///
/// 返回值语义（D-301 详解）：
/// - terminal：`utility(state, traverser) / π_traverser`（importance weighting）
/// - traverser decision：`Σ_a σ(I, a) × v_a`（σ-加权 cfv 之和）
/// - non-traverser decision：sampled action 路径上的 recursed value
///
/// 参数：
/// - `state`：当前 owned 状态（D-319 owned clone state representation）
/// - `traverser`：本 step 的 traverser（D-307 alternating）
/// - `pi_trav` / `pi_opp`：当前节点 reach probability 分解（不含 chance）
/// - `regret` / `strategy_sum`：可变借用累积容器
/// - `rng`：chance + opp action sampling 共享 rng（D-315 显式注入）
/// - `strategy_sum_weight`：stage 4 D-403 Linear weighted strategy sum 累积因子
///   `S_t(I, a) ← S_{t-1}(I, a) + w × σ_t(I, a)`，stage 3 path 走 `w = 1.0`
///   字面等价 stage 3 D-304 unweighted by t 累积（B1 Test 1 anchor 字面继承）；
///   stage 4 path 走 `w = t_stage4`（caller 在 [`EsMccfrTrainer::step`] 内传入）。
#[allow(clippy::too_many_arguments)]
fn recurse_es<G: Game>(
    state: G::State,
    traverser: PlayerId,
    pi_trav: f64,
    pi_opp: f64,
    regret: &mut RegretTable<G::InfoSet>,
    strategy_sum: &mut StrategyAccumulator<G::InfoSet>,
    rng: &mut dyn RngSource,
    strategy_sum_weight: f64,
) -> f64 {
    match G::current(&state) {
        NodeKind::Terminal => {
            // D-301 详解：terminal 返回 `utility / π_traverser`（importance
            // weighting：traverser sampled reach 倒数）。`pi_trav > 0` 在
            // 任意 traverser-reachable terminal 上恒成立——traverser branch
            // 内每个 action 沿 σ(a) 走，σ(a) 在 D-331 退化局面回退均匀分布
            // (1/n_actions) > 0，避免 zero division。
            let u = G::payoff(&state, traverser);
            if pi_trav > 0.0 {
                u / pi_trav
            } else {
                // 防御：π_traverser == 0 实际不可达（recurse_es 入口 π_trav =
                // 1.0，每次乘 σ(a) > 0）；触发即视作算法 bug，但 stage 3
                // 早期 carve-out 允许 fail-safe 返回 raw utility 让训练继续。
                u
            }
        }
        NodeKind::Chance => {
            // D-308 chance sample-1：在 chance_distribution 上采样 1 outcome，
            // 递归继续。chance node 不影响 π_trav / π_opp（chance 概率仅在
            // sampling 阶段隐含通过 1 / dist[i] importance correction 处理，但
            // ES-MCCFR D-308 中 chance 是单 1-sample 不做 importance correction，
            // 因此 π 不更新）。
            let dist = G::chance_distribution(&state);
            let action = sample_discrete(&dist, rng);
            let next_state = G::next(state, action, rng);
            recurse_es::<G>(
                next_state,
                traverser,
                pi_trav,
                pi_opp,
                regret,
                strategy_sum,
                rng,
                strategy_sum_weight,
            )
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
                // traverser node：枚举每个 action 的 cfv，累积 regret。
                // strategy_sum 在 D-301 详解 ES-MCCFR mode 仅在 non-traverser
                // 决策点累积（Lanctot 2009 §4.1）；traverser 决策点不累积。
                let mut cfvs: ShortVec<f64> = ShortVec::with_capacity(n);
                for (i, action) in actions.iter().enumerate() {
                    let next_state = G::next(state.clone(), *action, rng);
                    let cfv = recurse_es::<G>(
                        next_state,
                        traverser,
                        pi_trav * sigma[i],
                        pi_opp,
                        regret,
                        strategy_sum,
                        rng,
                        strategy_sum_weight,
                    );
                    cfvs.push(cfv);
                }
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                // regret update R(I, a) += π_opp × (cfv_a - σ_node)
                let delta: ShortVec<f64> =
                    cfvs.iter().map(|c| pi_opp * (c - sigma_value)).collect();
                regret.accumulate(info, &delta);
                sigma_value
            } else {
                // opponent node（D-309 / D-337）：按 σ 采样 1 action；非
                // traverser 决策点 strategy_sum 累积 `S(I, b) += w × σ(b)` for
                // all b（D-301 详解 / D-322 + stage 4 D-403 Linear weighted
                // 累积 `w = t_stage4`；stage 3 path `w = 1.0` byte-equal 维持）。
                //
                // 过滤零概率 outcome（API-331 [`sample_discrete`] 不变量：所有
                // p > 0；零概率 action 由 caller 剔除）。当 regret matching 后
                // 某些 action 的 σ 严格为 0 时（normalized R⁺ 分布常见情形），
                // 这些 action 在采样阶段不可达，从分布中剔除即可——剩余 σ 仍
                // sum 到 1（D-330 容差）。
                //
                // strategy_sum 仍按全 σ 累积（zero σ 加权累加零等价于不更新；
                // 保留 statement 让 D-304 标准累积形式不变形）。
                if strategy_sum_weight == 1.0 {
                    strategy_sum.accumulate(info, &sigma);
                } else {
                    let weighted: SigmaVec =
                        sigma.iter().map(|s| s * strategy_sum_weight).collect();
                    strategy_sum.accumulate(info, &weighted);
                }

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
                let sampled_idx = actions
                    .iter()
                    .position(|a| *a == sampled)
                    .expect("sampled action must be in legal_actions");
                let sampled_sigma = sigma[sampled_idx];

                let next_state = G::next(state, sampled, rng);
                recurse_es::<G>(
                    next_state,
                    traverser,
                    pi_trav,
                    pi_opp * sampled_sigma,
                    regret,
                    strategy_sum,
                    rng,
                    strategy_sum_weight,
                )
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
    pi_opp: f64,
    shared_regret: &RegretTable<G::InfoSet>,
    local_regret: &mut LocalRegretDelta<G::InfoSet>,
    local_strategy: &mut LocalStrategyDelta<G::InfoSet>,
    rng: &mut dyn RngSource,
) -> f64 {
    match G::current(&state) {
        NodeKind::Terminal => {
            let u = G::payoff(&state, traverser);
            if pi_trav > 0.0 {
                u / pi_trav
            } else {
                u
            }
        }
        NodeKind::Chance => {
            let dist = G::chance_distribution(&state);
            let action = sample_discrete(&dist, rng);
            let next_state = G::next(state, action, rng);
            recurse_es_parallel::<G>(
                next_state,
                traverser,
                pi_trav,
                pi_opp,
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
                let mut cfvs: ShortVec<f64> = ShortVec::with_capacity(n);
                for (i, action) in actions.iter().enumerate() {
                    let next_state = G::next(state.clone(), *action, rng);
                    let cfv = recurse_es_parallel::<G>(
                        next_state,
                        traverser,
                        pi_trav * sigma[i],
                        pi_opp,
                        shared_regret,
                        local_regret,
                        local_strategy,
                        rng,
                    );
                    cfvs.push(cfv);
                }
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                let delta: SigmaVec = cfvs.iter().map(|c| pi_opp * (c - sigma_value)).collect();
                local_regret.push(info, delta);
                sigma_value
            } else {
                // strategy_sum 全 σ 累积（同 single-thread 路径，zero σ 加零等价
                // 不更新但保 D-304 标准累积形式不变形）。SigmaVec → SigmaVec
                // 直接 clone 走 SmallVec inline 路径（不触发堆分配）。
                local_strategy.push(info, sigma.clone());

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
                let sampled_idx = actions
                    .iter()
                    .position(|a| *a == sampled)
                    .expect("sampled action must be in legal_actions");
                let sampled_sigma = sigma[sampled_idx];

                let next_state = G::next(state, sampled, rng);
                recurse_es_parallel::<G>(
                    next_state,
                    traverser,
                    pi_trav,
                    pi_opp * sampled_sigma,
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

/// HashMap<I, Vec<f64>> → bincode-serialized bytes，按 Debug 排序保证跨 host
/// byte-equal（D-327）。
///
/// 输出格式 = `bincode::serialize(&Vec<(I, Vec<f64>)>::sorted)`。bincode 1.x
/// 默认走 little-endian + varint integer encoding（D-354），不依赖 host endian。
fn encode_table<I>(
    table: &std::collections::HashMap<I, Vec<f64>>,
) -> Result<Vec<u8>, CheckpointError>
where
    I: Clone + std::fmt::Debug + serde::Serialize,
{
    let mut entries: Vec<(I, Vec<f64>)> =
        table.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    // D-327 sorted-by-InfoSet 顺序：以 Debug 输出为排序键（避免给 InfoSet
    // 引入 Ord bound — KuhnInfoSet / LeducInfoSet 未派生 Ord，且 Debug
    // 输出对每个 InfoSet 类型确定性，足以保证跨 host byte-equal）。
    entries.sort_by(|a, b| format!("{:?}", a.0).cmp(&format!("{:?}", b.0)));
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

// ===========================================================================
// stage 4 E2 \[实现\] — Checkpoint v2 6-region body 序列化 helpers（D-412
// 字面 6 套独立表持久化 + §D2-revM table-array deferral 翻面）。
// ===========================================================================

/// 多 traverser 表 → bincode bytes（regret 与 strategy_sum 共用）。
///
/// 序列化形态 `Vec<Vec<(I, Vec<f64>)>>` 长度 = 输入 slice 长度（NlheGame6 = 6
/// / HU 退化 = 2）。每个内层 entries 按 `Debug` 排序保跨 host BLAKE3 byte-equal
/// （与 [`encode_table`] 同 D-327 政策）。outer 顺序 = traverser index 0..N
/// （deterministic，无需排序）。
fn encode_multi_table<I>(
    tables: &[&std::collections::HashMap<I, Vec<f64>>],
) -> Result<Vec<u8>, CheckpointError>
where
    I: Clone + std::fmt::Debug + serde::Serialize,
{
    let mut outer: Vec<Vec<(I, Vec<f64>)>> = Vec::with_capacity(tables.len());
    for table in tables {
        let mut entries: Vec<(I, Vec<f64>)> =
            table.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        entries.sort_by(|a, b| format!("{:?}", a.0).cmp(&format!("{:?}", b.0)));
        outer.push(entries);
    }
    bincode::serialize(&outer).map_err(|e| CheckpointError::Corrupted {
        offset: 0,
        reason: format!("bincode serialize multi-traverser table failed: {e}"),
    })
}

/// bincode bytes → `Vec<RegretTable<I>>`（[`encode_multi_table`] 的 regret 侧逆向）。
///
/// 校验 outer 长度与 expected `traverser_count` 一致，不一致 → Corrupted。
fn decode_multi_regret<I>(
    bytes: &[u8],
    expected_count: u8,
) -> Result<Vec<RegretTable<I>>, CheckpointError>
where
    I: Clone + Eq + std::hash::Hash + std::fmt::Debug + serde::de::DeserializeOwned,
{
    let outer: Vec<Vec<(I, Vec<f64>)>> =
        bincode::deserialize(bytes).map_err(|e| CheckpointError::Corrupted {
            offset: 0,
            reason: format!("bincode deserialize multi-traverser regret table failed: {e}"),
        })?;
    if outer.len() != expected_count as usize {
        return Err(CheckpointError::Corrupted {
            offset: 0,
            reason: format!(
                "multi-traverser regret table count mismatch: header={} body={}",
                expected_count,
                outer.len()
            ),
        });
    }
    let mut tables = Vec::with_capacity(outer.len());
    for entries in outer {
        let mut t = RegretTable::new();
        for (k, v) in entries {
            t.accumulate(k, &v);
        }
        tables.push(t);
    }
    Ok(tables)
}

/// bincode bytes → `Vec<StrategyAccumulator<I>>`（[`encode_multi_table`] 的
/// strategy_sum 侧逆向，与 [`decode_multi_regret`] 同型）。
fn decode_multi_strategy<I>(
    bytes: &[u8],
    expected_count: u8,
) -> Result<Vec<StrategyAccumulator<I>>, CheckpointError>
where
    I: Clone + Eq + std::hash::Hash + std::fmt::Debug + serde::de::DeserializeOwned,
{
    let outer: Vec<Vec<(I, Vec<f64>)>> =
        bincode::deserialize(bytes).map_err(|e| CheckpointError::Corrupted {
            offset: 0,
            reason: format!("bincode deserialize multi-traverser strategy table failed: {e}"),
        })?;
    if outer.len() != expected_count as usize {
        return Err(CheckpointError::Corrupted {
            offset: 0,
            reason: format!(
                "multi-traverser strategy table count mismatch: header={} body={}",
                expected_count,
                outer.len()
            ),
        });
    }
    let mut tables = Vec::with_capacity(outer.len());
    for entries in outer {
        let mut t = StrategyAccumulator::new();
        for (k, v) in entries {
            t.accumulate(k, &v);
        }
        tables.push(t);
    }
    Ok(tables)
}
