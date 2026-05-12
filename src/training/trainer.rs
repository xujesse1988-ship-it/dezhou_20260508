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
//! A1 \[实现\] 阶段所有方法体 `unimplemented!()`；B2 \[实现\] 落地
//! [`VanillaCfrTrainer::step`] 与全套 Trainer 方法（让 Kuhn 10K iter 单线程 release
//! `< 1 s` 收敛到 player 1 EV `-1/18`）；C2 \[实现\] 落地 [`EsMccfrTrainer::step`]
//! 与 [`EsMccfrTrainer::step_parallel`] 多线程入口（D-361 单线程 ≥ 10K update/s
//! 与 4-core ≥ 50K update/s）。

use std::path::Path;

use crate::core::rng::RngSource;
use crate::error::{CheckpointError, TrainerError};
use crate::training::game::Game;
use crate::training::regret::{RegretTable, StrategyAccumulator};

/// 训练器统一 trait（API-310 / D-371）。
///
/// `Trainer<G: Game>` 让 `VanillaCfrTrainer` / `EsMccfrTrainer` 在 Kuhn / Leduc /
/// 简化 NLHE 上同型可替换（具体 `step` 内部按算法变体派发：Vanilla CFR 遍历完整
/// 博弈树 `n_players` 次，每次 1 traverser；ES-MCCFR D-307 alternating
/// traverser）。
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
/// `iter` 字段单调非降；`rng_substream_seed` D-335 sub-stream root seed（
/// per-iter sub-stream 由 [`crate::training::sampling::derive_substream_seed`] 派生）。
///
/// 字段 `pub(crate)` 让同 crate 测试 / bench 直接 inspect 内部状态而不暴露给外部
/// 消费者（D-376 公开 vs 私有 API）。
#[allow(dead_code)] // B2 \[实现\] 落地 VanillaCfrTrainer::step 后字段会被读取
pub struct VanillaCfrTrainer<G: Game> {
    pub(crate) game: G,
    pub(crate) regret: RegretTable<G::InfoSet>,
    pub(crate) strategy_sum: StrategyAccumulator<G::InfoSet>,
    pub(crate) iter: u64,
    pub(crate) rng_substream_seed: [u8; 32],
}

impl<G: Game> VanillaCfrTrainer<G> {
    /// 新建空 Trainer（B2 \[实现\] 落地全部 field 初始化）。
    pub fn new(_game: G, _master_seed: u64) -> Self {
        unimplemented!("stage 3 A1 scaffold: VanillaCfrTrainer::new (B2 实现)")
    }
}

impl<G: Game> Trainer<G> for VanillaCfrTrainer<G> {
    fn step(&mut self, _rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        // D-300 伪代码：alternating traverser × 完整博弈树 DFS × cfv 累积 ×
        // regret update × strategy_sum 累积。
        unimplemented!("stage 3 A1 scaffold: VanillaCfrTrainer::step (B2 实现)")
    }

    fn current_strategy(&self, _info_set: &G::InfoSet) -> Vec<f64> {
        unimplemented!("stage 3 A1 scaffold: VanillaCfrTrainer::current_strategy (B2 实现)")
    }

    fn average_strategy(&self, _info_set: &G::InfoSet) -> Vec<f64> {
        unimplemented!("stage 3 A1 scaffold: VanillaCfrTrainer::average_strategy (B2 实现)")
    }

    fn update_count(&self) -> u64 {
        unimplemented!("stage 3 A1 scaffold: VanillaCfrTrainer::update_count (B2 实现)")
    }

    fn save_checkpoint(&self, _path: &Path) -> Result<(), CheckpointError> {
        unimplemented!("stage 3 A1 scaffold: VanillaCfrTrainer::save_checkpoint (D2 实现)")
    }

    fn load_checkpoint(_path: &Path, _game: G) -> Result<Self, CheckpointError>
    where
        Self: Sized,
    {
        unimplemented!("stage 3 A1 scaffold: VanillaCfrTrainer::load_checkpoint (D2 实现)")
    }
}

/// External-Sampling MCCFR Trainer（API-312 / D-301）。
///
/// 与 [`VanillaCfrTrainer`] 同型字段（`update_count` 替换 `iter` 反映 ES-MCCFR
/// 按 D-307 alternating traverser × per-iter 1-sample 路径累积；`regret` 在
/// 多线程模式下可能包装为 D-321 锁定的 thread-safety wrapper，A1 阶段保持
/// `RegretTable` 直接持有，C2 \[实现\] 起步前由 D-321 lock 时改写）。
#[allow(dead_code)] // C2 \[实现\] 落地 EsMccfrTrainer::step 后字段会被读取
pub struct EsMccfrTrainer<G: Game> {
    pub(crate) game: G,
    pub(crate) regret: RegretTable<G::InfoSet>,
    pub(crate) strategy_sum: StrategyAccumulator<G::InfoSet>,
    pub(crate) update_count: u64,
    pub(crate) rng_substream_seed: [u8; 32],
}

impl<G: Game> EsMccfrTrainer<G> {
    /// 新建空 Trainer（C2 \[实现\] 落地全部 field 初始化）。
    pub fn new(_game: G, _master_seed: u64) -> Self {
        unimplemented!("stage 3 A1 scaffold: EsMccfrTrainer::new (C2 实现)")
    }

    /// 多线程并发 step（D-321 thread-safety 模型决定具体实现；C2 \[实现\] 起步前
    /// lock 候选 `parking_lot::RwLock<HashMap>` / `dashmap::DashMap` / thread-local
    /// accumulator + 周期 batch merge / `crossbeam::SegQueue` snapshot reduce）。
    ///
    /// 目标 SLO：4-core release `≥ 50K update/s`（D-361 效率 ≥ 0.5）。
    pub fn step_parallel(
        &mut self,
        _rng_pool: &mut [Box<dyn RngSource>],
        _n_threads: usize,
    ) -> Result<(), TrainerError> {
        unimplemented!("stage 3 A1 scaffold: EsMccfrTrainer::step_parallel (C2 实现)")
    }
}

impl<G: Game> Trainer<G> for EsMccfrTrainer<G> {
    fn step(&mut self, _rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        // D-301 伪代码：alternating traverser × DFS × chance node 1-sample
        // （D-308）× opponent action sampled by current_strategy（D-309 / D-337）
        // × traverser action 完整枚举累积 cfv（D-338）× regret update。
        unimplemented!("stage 3 A1 scaffold: EsMccfrTrainer::step (C2 实现)")
    }

    fn current_strategy(&self, _info_set: &G::InfoSet) -> Vec<f64> {
        unimplemented!("stage 3 A1 scaffold: EsMccfrTrainer::current_strategy (C2 实现)")
    }

    fn average_strategy(&self, _info_set: &G::InfoSet) -> Vec<f64> {
        unimplemented!("stage 3 A1 scaffold: EsMccfrTrainer::average_strategy (C2 实现)")
    }

    fn update_count(&self) -> u64 {
        unimplemented!("stage 3 A1 scaffold: EsMccfrTrainer::update_count (C2 实现)")
    }

    fn save_checkpoint(&self, _path: &Path) -> Result<(), CheckpointError> {
        unimplemented!("stage 3 A1 scaffold: EsMccfrTrainer::save_checkpoint (D2 实现)")
    }

    fn load_checkpoint(_path: &Path, _game: G) -> Result<Self, CheckpointError>
    where
        Self: Sized,
    {
        unimplemented!("stage 3 A1 scaffold: EsMccfrTrainer::load_checkpoint (D2 实现)")
    }
}
