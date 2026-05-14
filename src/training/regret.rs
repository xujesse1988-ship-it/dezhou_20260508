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
//! E2-rev1 \[实现\]（2026-05-14）热路径优化：[`RegretTable::current_strategy`]
//! 返回 `SigmaVec` = `SmallVec<[f64; 8]>`，让 typical 5-action（NLHE D-209）/
//! 2-action（Kuhn）/ 3-action（Leduc）路径走 stack alloc，规避 `Vec::with_capacity`
//! 堆分配。`Trainer::current_strategy` / `Trainer::average_strategy` trait 入口
//! 仍返回 `Vec<f64>`（API-310 surface 不变），仅在 trait impl 边界经
//! `SigmaVec::into_vec` 转 owned `Vec<f64>`（cheap，spill 后零拷贝）。
//!
//! E2-rev1 同 commit 引入 `LocalRegretDelta` / `LocalStrategyDelta` 用作
//! `step_parallel` thread-local delta accumulator：`Vec<(I, SigmaVec)>` 按 DFS
//! 顺序 append，无内部 dedup / sort，merge 阶段按 insertion 顺序 playback 到主表。
//! 跨 run 决定性来源 = DFS 顺序 deterministic（rng 决定 sampled trajectory）+
//! tid 顺序 deterministic（rayon `par_iter_mut().enumerate().collect()` 保 index
//! 顺序），不依赖 HashMap iteration 顺序，省去原 D-321-rev1 batch merge 的
//! `format!("{:?}", I)` × O(N log N) 排序开销。

use std::collections::HashMap;
use std::hash::Hash;

use smallvec::SmallVec;

/// 短策略向量类型别名（E2-rev1 \[实现\]）。
///
/// `SmallVec<[f64; 8]>` inline 容量 8 覆盖 stage 3 D-209 5-action / Kuhn 2-action
/// / Leduc 3-action 全场景；溢出时自动 spill 到堆（保留对未来 large-action
/// abstraction 的兼容）。返回值实现 `IntoIterator<Item = f64>` 与
/// `Deref<Target = [f64]>`，调用站点与 `Vec<f64>` 替换无感（除显式 `Vec` 注解）。
pub(crate) type SigmaVec = SmallVec<[f64; 8]>;

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
    ///
    /// **API surface**：返回 `Vec<f64>`（API-320 surface 不变）。trainer 内部
    /// 热路径走 `Self::current_strategy_smallvec` 走 `SigmaVec` stack alloc 路径。
    pub fn current_strategy(&self, info_set: &I, n_actions: usize) -> Vec<f64> {
        self.current_strategy_smallvec(info_set, n_actions)
            .into_vec()
    }

    /// 热路径变体：返回 `SigmaVec` = `SmallVec<[f64; 8]>`，让 typical 5-action
    /// （NLHE）/ 2-action（Kuhn）/ 3-action（Leduc）路径走 stack alloc，规避
    /// `Vec::with_capacity` 堆分配（E2-rev1 \[实现\]）。
    ///
    /// f64 算术与原 `Vec<f64>` 路径完全等价（同样 R⁺ 累积 + 除法归一化），
    /// BLAKE3 byte-equal 不变（D-362 强 anchor）。
    ///
    /// `pub(crate)` 限定让 trainer 内部 hot path 直接消费 `SmallVec` 而不必经
    /// `Vec<f64>` 转换；外部 trait/test 入口仍走 [`Self::current_strategy`]
    /// 维持 API-320 surface（详见 `docs/pluribus_stage3_api.md`）。
    pub(crate) fn current_strategy_smallvec(&self, info_set: &I, n_actions: usize) -> SigmaVec {
        let uniform = || SigmaVec::from_elem(1.0 / n_actions as f64, n_actions);
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

        let mut positives: SigmaVec = SigmaVec::with_capacity(n_actions);
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

    /// 消费 [`RegretTable`] 取出 owned HashMap（E2 \[实现\] 落地 D-321-rev1 真并发
    /// `step_parallel` batch merge 时调用：每线程 spawn 持有独立 thread-local
    /// `RegretTable` 作为 delta accumulator，spawn 结束后 main thread 调用本方法
    /// 取出 owned entries → 按 InfoSet `Debug` 排序 → 顺序累加到主表）。
    ///
    /// E2-rev1 \[实现\]（2026-05-14）后 step_parallel 改走
    /// `LocalRegretDelta` append-only 路径，本方法不再被 trainer 内部调用，但
    /// 保留以维持外部测试 / harness 入口兼容（pub(crate) 语义不变）。
    #[doc(hidden)]
    #[allow(dead_code)]
    pub(crate) fn into_inner(self) -> HashMap<I, Vec<f64>> {
        self.inner
    }
}

/// E2-rev1 \[实现\] thread-local regret delta accumulator（D-321-rev1 lock 真并发
/// `step_parallel` batch merge 入口）。
///
/// `Vec<(I, SigmaVec)>` 按 DFS 顺序 append（不 dedup / 不 sort，保留单线程内
/// f64 加法顺序的天然 deterministic）。merge 时 main thread 按 tid 升序 ×
/// 每 thread 内 insertion 顺序 playback 到主 [`RegretTable`]。
///
/// 与原 D-321-rev1 lock（thread-local `RegretTable` + `format!("{:?}", I)` ×
/// O(N log N) 排序合并）等价的跨 run BLAKE3 byte-equal 来源：
/// - DFS 顺序 deterministic（rng 决定 sampled trajectory）
/// - tid 顺序 deterministic（rayon `par_iter_mut().enumerate().collect()` 保
///   index 顺序，等价 `std::thread::scope` spawn-by-tid 顺序）
/// - 同 InfoSet 多次访问（traverser 5-action enumerate 偶发触发）按 push 顺序
///   playback，f64 加法序列与原 thread-local table accumulate 后再合并完全等价
///   （`main += local_table[i] = (((0+d1)+d2)+...)` 与 `main += d1; main += d2;
///   ...` 数值结果恒等，f64 结合律失败仅在不同顺序下）。
///
/// 性能优势：省去 `format!("{:?}", I)` × 每比较一次 String alloc 的开销
/// （F1-rev1 vultr 4-core 加速比 1.14× 主因 = batch merge sort 而非 spawn
/// overhead），merge cost 由 O(N log N × |Debug(I)|) 降到 O(N)。
#[derive(Debug, Default)]
pub(crate) struct LocalRegretDelta<I> {
    entries: Vec<(I, SigmaVec)>,
}

impl<I> LocalRegretDelta<I> {
    /// 空容器。
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Append 1 个 (info_set, delta) 条目；不 dedup / 不 sort（merge 时按 push
    /// 顺序 playback）。`delta` 调用方持有 `SmallVec<[f64; 8]>`（`SigmaVec`），
    /// 本方法直接 owned 接收避免再分配。
    pub(crate) fn push(&mut self, info_set: I, delta: SigmaVec) {
        self.entries.push((info_set, delta));
    }

    /// 已 push 条目数（监控用）。
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// 空容器 sanity（让 clippy::len_without_is_empty 通过）。
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 消费并返回 owned entries（merge 入口）。
    pub(crate) fn into_entries(self) -> Vec<(I, SigmaVec)> {
        self.entries
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

    /// 消费 [`StrategyAccumulator`] 取出 owned HashMap（E2 \[实现\] 落地 D-321-rev1
    /// 真并发 `step_parallel` batch merge 入口；语义同 [`RegretTable::into_inner`]）。
    ///
    /// E2-rev1 \[实现\] 后 step_parallel 改走 `LocalStrategyDelta` append-only
    /// 路径，本方法不再被 trainer 内部调用，但保留以维持兼容。
    #[doc(hidden)]
    #[allow(dead_code)]
    pub(crate) fn into_inner(self) -> HashMap<I, Vec<f64>> {
        self.inner
    }
}

/// E2-rev1 \[实现\] thread-local strategy_sum delta accumulator
/// （语义同 `LocalRegretDelta`，作用于 [`StrategyAccumulator`]）。
#[derive(Debug, Default)]
pub(crate) struct LocalStrategyDelta<I> {
    entries: Vec<(I, SigmaVec)>,
}

impl<I> LocalStrategyDelta<I> {
    /// 空容器。
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Append 1 个 (info_set, weighted_strategy) 条目（语义同
    /// [`LocalRegretDelta::push`]）。
    pub(crate) fn push(&mut self, info_set: I, weighted: SigmaVec) {
        self.entries.push((info_set, weighted));
    }

    /// 已 push 条目数（监控用）。
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// 空容器 sanity（让 clippy::len_without_is_empty 通过）。
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 消费并返回 owned entries（merge 入口）。
    pub(crate) fn into_entries(self) -> Vec<(I, SigmaVec)> {
        self.entries
    }
}
