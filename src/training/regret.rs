//! `RegretTable` + `StrategyAccumulator`（API-320 / API-321）。
//!
//! D-320 `HashMap<I, Vec<f64>>` 容器选型；D-323 lazy 初始化；D-324 action_count
//! 训练全程恒定（不一致 panic / [`crate::error::TrainerError::ActionCountMismatch`]）；
//! D-328 query API stateless 返回 `Vec<f64>`；D-329 浮点容差 warn 不 panic（实际
//! tolerance enforce 在 [`crate::training::trainer::Trainer::step`] 内部触发
//! [`crate::error::TrainerError::ProbabilitySumOutOfTolerance`]）。
//!
//! 浮点路径限定（D-379）：本模块允许 `f64` regret / strategy_sum，但不允许泄露到
//! stage 1 / stage 2 锁定路径。
//!
//! A1 \[实现\] 阶段所有方法体 `unimplemented!()`；B2 \[实现\] 落地
//! [`RegretTable::accumulate`] / [`RegretTable::current_strategy`] /
//! [`StrategyAccumulator::accumulate`] / [`StrategyAccumulator::average_strategy`]。

use std::collections::HashMap;
use std::hash::Hash;

/// regret 累积容器（API-320 / D-320）。
///
/// 内部存储 `HashMap<I, Vec<f64>>`：key = InfoSet（由 [`crate::training::Game::InfoSet`]
/// 关联类型决定）、value = 长度 `n_actions` 的 regret 累积向量。`Vec<f64>` 顺序与
/// [`crate::training::Game::legal_actions`] 输出索引一一对应（D-324）。
///
/// 不变量（API-320 invariants）：
/// - `get_or_init(I, n)` / `accumulate(I, delta)` 在同 `I` 上 `n` / `delta.len()`
///   必须一致，否则 panic / 返回 [`crate::error::TrainerError::ActionCountMismatch`]
///   （D-324）。
/// - [`Self::current_strategy`] 返回 `Vec<f64>` 长度 = `n_actions`，sum = `1.0 ±
///   1e-9`（D-330）。
/// - 已访问 InfoSet 后 [`Self::len`] 单调非降。
#[derive(Debug)]
#[allow(dead_code)] // B2 \[实现\] 落地后字段会被读取（参见模块 doc）
pub struct RegretTable<I: Eq + Hash + Clone> {
    inner: HashMap<I, Vec<f64>>,
    n_actions_index: HashMap<I, usize>,
}

impl<I: Eq + Hash + Clone> Default for RegretTable<I> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: Eq + Hash + Clone> RegretTable<I> {
    /// 空容器（D-323 lazy 初始化：InfoSet 首次访问时才分配 `Vec<f64>`）。
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
            n_actions_index: HashMap::new(),
        }
    }

    /// 获取 InfoSet 上的 regret vec 可变引用；首次访问时 lazy 分配（D-323）。
    ///
    /// 校验 `n_actions` 与已分配 vec 长度一致；不一致 panic（B2 \[实现\] 路径）
    /// 或在 [`crate::training::Trainer::step`] 内部触发
    /// [`crate::error::TrainerError::ActionCountMismatch`]（D-324）。
    pub fn get_or_init(&mut self, _info_set: I, _n_actions: usize) -> &mut Vec<f64> {
        unimplemented!("stage 3 A1 scaffold: RegretTable::get_or_init (B2 实现)")
    }

    /// 计算 current strategy：regret matching + 退化均匀分布（D-303 + D-331）。
    ///
    /// `R⁺(I, a) = max(R(I, a), 0)`；若 `Σ R⁺ > 0` 则 `σ(I, a) = R⁺(I, a) / Σ_b R⁺(I, b)`；
    /// 否则返回均匀分布 `1 / n_actions`。
    pub fn current_strategy(&self, _info_set: &I, _n_actions: usize) -> Vec<f64> {
        unimplemented!("stage 3 A1 scaffold: RegretTable::current_strategy (B2 实现)")
    }

    /// 累积 regret（D-305 标准 CFR update）。
    ///
    /// `R(I, a) += delta[a]`，遍历 `delta.len()` 个 action。D-324 校验
    /// `delta.len() == n_actions_index[I]`，不一致触发
    /// [`crate::error::TrainerError::ActionCountMismatch`]。
    pub fn accumulate(&mut self, _info_set: I, _delta: &[f64]) {
        unimplemented!("stage 3 A1 scaffold: RegretTable::accumulate (B2 实现)")
    }

    /// 已访问 InfoSet 数（监控用 + 跨 host 一致性 sanity）。
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// 空容器 sanity（让 clippy::len_without_is_empty 通过）。
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// strategy 累积容器（API-321 / D-322）。
///
/// 与 [`RegretTable`] 同型 HashMap-backed lazy 分配，但语义不同：累积
/// `S(I, a) += π_traverser × σ(I, a)`（Vanilla CFR D-304 标准累积）或
/// `S(I, a) += σ(I, a) × 1` per sampled reach（ES-MCCFR D-304）。
///
/// 不变量同 [`RegretTable`]（D-324 / D-330 / 单调非降 len）。
#[derive(Debug)]
#[allow(dead_code)] // B2 \[实现\] 落地后字段会被读取（参见模块 doc）
pub struct StrategyAccumulator<I: Eq + Hash + Clone> {
    inner: HashMap<I, Vec<f64>>,
    n_actions_index: HashMap<I, usize>,
}

impl<I: Eq + Hash + Clone> Default for StrategyAccumulator<I> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: Eq + Hash + Clone> StrategyAccumulator<I> {
    /// 空容器（D-323 lazy 初始化）。
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
            n_actions_index: HashMap::new(),
        }
    }

    /// 累积 weighted strategy（D-304 标准累积）。
    ///
    /// `weighted_strategy[a] = π_traverser × σ(I, a)`（Vanilla CFR）或
    /// `weighted_strategy[a] = σ(I, a)`（ES-MCCFR sampled reach 1）。
    pub fn accumulate(&mut self, _info_set: I, _weighted_strategy: &[f64]) {
        unimplemented!("stage 3 A1 scaffold: StrategyAccumulator::accumulate (B2 实现)")
    }

    /// 计算 average strategy（D-304）：`avg_σ(I, a) = S(I, a) / Σ_b S(I, b)`。
    ///
    /// 未访问 InfoSet（`inner.get(I) == None`）返回均匀分布 `1 / n_actions`
    /// （D-331 退化局面）。
    pub fn average_strategy(&self, _info_set: &I, _n_actions: usize) -> Vec<f64> {
        unimplemented!("stage 3 A1 scaffold: StrategyAccumulator::average_strategy (B2 实现)")
    }

    /// 已访问 InfoSet 数（监控用 + 跨 host 一致性 sanity）。
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// 空容器 sanity（让 clippy::len_without_is_empty 通过）。
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}
