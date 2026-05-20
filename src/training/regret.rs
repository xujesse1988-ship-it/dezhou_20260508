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
//! 返回 `SigmaVec` = `SmallVec<[f64; 8]>`，让 typical 6-action（NLHE D-209）/
//! 2-action（Kuhn）/ 3-action（Leduc）路径走 stack alloc，规避 `Vec::with_capacity`
//! 堆分配。`Trainer::current_strategy` / `Trainer::average_strategy` trait 入口
//! 仍返回 `Vec<f64>`（API-310 surface 不变），仅在 trait impl 边界经
//! `SigmaVec::into_vec` 转 owned `Vec<f64>`（cheap，spill 后零拷贝）。
//!
//! `LocalRegretDelta` / `LocalStrategyDelta` 作为 `step_parallel` thread-local
//! delta accumulator：`IndexMap<I, SigmaVec>` 在 DFS 过程中 in-place 累加同一
//! InfoSet 的多次访问，merge 阶段按首次 push 顺序 playback 到主表，每个唯一
//! InfoSet 只触发一次主表 HashMap entry 调用。跨 run 决定性来源 = DFS 顺序
//! deterministic（rng 决定 sampled trajectory）+ tid 顺序 deterministic
//! （rayon `par_iter_mut().enumerate().collect()` 保 index 顺序）+ `IndexMap`
//! 保 insertion order，不依赖 std `HashMap` iteration 顺序。

use std::collections::HashMap;
use std::hash::Hash;

use indexmap::IndexMap;
use smallvec::SmallVec;

/// 短策略向量类型别名（E2-rev1 \[实现\]）。
///
/// `SmallVec<[f64; 8]>` inline 容量 8 覆盖 stage 3 D-209 6-action / Kuhn 2-action
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

    /// 热路径变体：返回 `SigmaVec` = `SmallVec<[f64; 8]>`，让 typical 6-action
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

/// thread-local regret delta accumulator（`step_parallel` 真并发 batch merge 入口）。
///
/// 内部 `IndexMap<I, SigmaVec>`：DFS 内同一 InfoSet 多次访问时 **in-place 累加**
/// 在同一槽位，merge 阶段每个唯一 InfoSet 只往主 [`RegretTable`] 写一次。
/// `IndexMap` 保 insertion 顺序，merge 按 tid 升序 × 每 thread 内首次 push 顺序
/// playback。
///
/// 跨 run BLAKE3 byte-equal 来源：
/// - 同 trajectory 内多次 push 同 InfoSet 时 in-place 累加按 DFS 顺序结合，
///   `local[I] = (((0 + d1) + d2) + d3) + ...`，单线程内 f64 加法序列恒定。
/// - merge 时 `main.accumulate(I, &local[I])` 把 thread-local 已聚合的总量加到主表，
///   跨 thread 顺序仍是 tid 升序（rayon `par_iter_mut().enumerate().collect()`
///   保 index 顺序）。
/// - `IndexMap` 的 insertion-order iteration 与 std `HashMap` 不同，是确定性保 N
///   个唯一 InfoSet 按首次 push 顺序 playback 的关键。
///
/// 性能优势 vs 旧 `Vec<(I, SigmaVec)>` append-only 路径（status step 2）：
/// - merge 阶段对主表的 HashMap entry 调用数从 **N push 数** 降到 **N 唯一 InfoSet 数**
///   （DFS 内同 InfoSet 多次访问时 dedup）。
/// - 主表 entry call 是 step_parallel 序列化点，T_merge ~ 88 μs/delta（status
///   段 "瓶颈定位"），dedup 后等比下降。
#[derive(Debug, Default)]
pub(crate) struct LocalRegretDelta<I: Eq + Hash> {
    entries: IndexMap<I, SigmaVec>,
}

impl<I: Eq + Hash> LocalRegretDelta<I> {
    /// 空容器。
    pub(crate) fn new() -> Self {
        Self {
            entries: IndexMap::new(),
        }
    }

    /// Push 1 个 (info_set, delta) 条目：
    /// - 首次见 `info_set` → 直接 insert（保 insertion order）。
    /// - 已存在 → in-place `slot[i] += delta[i]` 累加（DFS 访问序左结合）。
    ///
    /// `delta` 调用方持有 `SmallVec<[f64; 8]>`（`SigmaVec`），首次插入 owned
    /// 转移避免再分配；已存在时只读 slice 累加。
    pub(crate) fn push(&mut self, info_set: I, delta: SigmaVec) {
        match self.entries.get_mut(&info_set) {
            Some(slot) => {
                debug_assert_eq!(
                    slot.len(),
                    delta.len(),
                    "LocalRegretDelta::push action_count mismatch: stored {}, requested {} (D-324)",
                    slot.len(),
                    delta.len()
                );
                for (s, d) in slot.iter_mut().zip(delta.iter()) {
                    *s += *d;
                }
            }
            None => {
                self.entries.insert(info_set, delta);
            }
        }
    }

    /// 已存唯一 InfoSet 数（监控用）。
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// 空容器 sanity（让 clippy::len_without_is_empty 通过）。
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 消费并返回 owned entries（merge 入口），按 insertion order 输出。
    pub(crate) fn into_entries(self) -> Vec<(I, SigmaVec)> {
        self.entries.into_iter().collect()
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

/// thread-local strategy_sum delta accumulator
/// （语义同 [`LocalRegretDelta`]，作用于 [`StrategyAccumulator`]）。
#[derive(Debug, Default)]
pub(crate) struct LocalStrategyDelta<I: Eq + Hash> {
    entries: IndexMap<I, SigmaVec>,
}

impl<I: Eq + Hash> LocalStrategyDelta<I> {
    /// 空容器。
    pub(crate) fn new() -> Self {
        Self {
            entries: IndexMap::new(),
        }
    }

    /// Push 1 个 (info_set, weighted_strategy) 条目（语义同
    /// [`LocalRegretDelta::push`]）。
    pub(crate) fn push(&mut self, info_set: I, weighted: SigmaVec) {
        match self.entries.get_mut(&info_set) {
            Some(slot) => {
                debug_assert_eq!(
                    slot.len(),
                    weighted.len(),
                    "LocalStrategyDelta::push action_count mismatch: stored {}, requested {} \
                     (D-324)",
                    slot.len(),
                    weighted.len()
                );
                for (s, d) in slot.iter_mut().zip(weighted.iter()) {
                    *s += *d;
                }
            }
            None => {
                self.entries.insert(info_set, weighted);
            }
        }
    }

    /// 已存唯一 InfoSet 数（监控用）。
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// 空容器 sanity（让 clippy::len_without_is_empty 通过）。
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 消费并返回 owned entries（merge 入口），按 insertion order 输出。
    pub(crate) fn into_entries(self) -> Vec<(I, SigmaVec)> {
        self.entries.into_iter().collect()
    }
}
