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
pub struct RegretTable<I: Eq + Hash + Clone> {
    inner: HashMap<I, Vec<f64>>,
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
        }
    }

    /// 获取 InfoSet 上的 regret vec 可变引用；首次访问时 lazy 分配（D-323）。
    ///
    /// 校验 `n_actions` 与已分配 vec 长度一致；不一致 panic（D-324
    /// `ActionCountMismatch` 在 `Trainer::step` 层提升为
    /// [`crate::error::TrainerError::ActionCountMismatch`]，本层 panic 是底层
    /// trip-wire，正常调用路径不会触达）。
    pub fn get_or_init(&mut self, info_set: I, n_actions: usize) -> &mut Vec<f64> {
        let entry = self
            .inner
            .entry(info_set)
            .or_insert_with(|| vec![0.0; n_actions]);
        assert_eq!(
            entry.len(),
            n_actions,
            "RegretTable::get_or_init action_count mismatch: stored {}, requested {} (D-324)",
            entry.len(),
            n_actions
        );
        entry
    }

    /// 计算 current strategy：regret matching + 退化均匀分布（D-303 + D-331）。
    ///
    /// `R⁺(I, a) = max(R(I, a), 0)`；若 `Σ R⁺ > 0` 则 `σ(I, a) = R⁺(I, a) / Σ_b R⁺(I, b)`；
    /// 否则返回均匀分布 `1 / n_actions`。
    pub fn current_strategy(&self, info_set: &I, n_actions: usize) -> Vec<f64> {
        let uniform = || vec![1.0 / n_actions as f64; n_actions];
        let Some(regrets) = self.inner.get(info_set) else {
            return uniform();
        };
        assert_eq!(
            regrets.len(),
            n_actions,
            "RegretTable::current_strategy action_count mismatch: stored {}, requested {} (D-324)",
            regrets.len(),
            n_actions
        );

        let mut positives = Vec::with_capacity(n_actions);
        let mut sum = 0.0_f64;
        for &r in regrets {
            let r_plus = if r > 0.0 { r } else { 0.0 };
            positives.push(r_plus);
            sum += r_plus;
        }
        if sum > 0.0 {
            for p in &mut positives {
                *p /= sum;
            }
            positives
        } else {
            uniform()
        }
    }

    /// 累积 regret（D-305 标准 CFR update）。
    ///
    /// `R(I, a) += delta[a]`，遍历 `delta.len()` 个 action。D-324 校验
    /// `delta.len() == stored.len()`，不一致 panic（同 [`Self::get_or_init`]）。
    pub fn accumulate(&mut self, info_set: I, delta: &[f64]) {
        let entry = self.get_or_init(info_set, delta.len());
        for (slot, &d) in entry.iter_mut().zip(delta) {
            *slot += d;
        }
    }

    /// 已访问 InfoSet 数（监控用 + 跨 host 一致性 sanity）。
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// 空容器 sanity（让 clippy::len_without_is_empty 通过）。
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// 已访问 InfoSet 的内部 HashMap 只读访问（监控 / 测试用；D-376 公开 vs 私有
    /// 边界：返回引用而非克隆，避免外部消费者复制大表）。
    #[doc(hidden)]
    pub fn inner(&self) -> &HashMap<I, Vec<f64>> {
        &self.inner
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
pub struct StrategyAccumulator<I: Eq + Hash + Clone> {
    inner: HashMap<I, Vec<f64>>,
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
        }
    }

    /// 累积 weighted strategy（D-304 标准累积）。
    ///
    /// `weighted_strategy[a] = π_traverser × σ(I, a)`（Vanilla CFR）或
    /// `weighted_strategy[a] = σ(I, a)`（ES-MCCFR sampled reach 1）。
    pub fn accumulate(&mut self, info_set: I, weighted_strategy: &[f64]) {
        let entry = self
            .inner
            .entry(info_set)
            .or_insert_with(|| vec![0.0; weighted_strategy.len()]);
        assert_eq!(
            entry.len(),
            weighted_strategy.len(),
            "StrategyAccumulator::accumulate action_count mismatch: stored {}, requested {} (D-324)",
            entry.len(),
            weighted_strategy.len()
        );
        for (slot, &w) in entry.iter_mut().zip(weighted_strategy) {
            *slot += w;
        }
    }

    /// 计算 average strategy（D-304）：`avg_σ(I, a) = S(I, a) / Σ_b S(I, b)`。
    ///
    /// 未访问 InfoSet（`inner.get(I) == None`）返回均匀分布 `1 / n_actions`
    /// （D-331 退化局面）。
    pub fn average_strategy(&self, info_set: &I, n_actions: usize) -> Vec<f64> {
        let uniform = || vec![1.0 / n_actions as f64; n_actions];
        let Some(sums) = self.inner.get(info_set) else {
            return uniform();
        };
        assert_eq!(
            sums.len(),
            n_actions,
            "StrategyAccumulator::average_strategy action_count mismatch: stored {}, requested {} (D-324)",
            sums.len(),
            n_actions
        );
        let total: f64 = sums.iter().sum();
        if total > 0.0 {
            sums.iter().map(|s| s / total).collect()
        } else {
            uniform()
        }
    }

    /// 已访问 InfoSet 数（监控用 + 跨 host 一致性 sanity）。
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// 空容器 sanity（让 clippy::len_without_is_empty 通过）。
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// 已访问 InfoSet 的内部 HashMap 只读访问（监控 / 测试用）。
    #[doc(hidden)]
    pub fn inner(&self) -> &HashMap<I, Vec<f64>> {
        &self.inner
    }
}
