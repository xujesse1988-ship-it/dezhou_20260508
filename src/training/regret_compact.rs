//! 阶段 5 紧凑 RegretTable / StrategyAccumulator（API-510..API-529 / D-510 +
//! D-511 字面）。
//!
//! **不替换** [`crate::training::regret::RegretTable`]（stage 3 D-321-rev2 既有
//! HashMap-backed naive 表维持作为 fallback + ablation baseline + stage 4
//! schema=2 checkpoint 加载路径必需）。stage 5 trainer dispatch 走 trait-based
//! selection（[`crate::error::TrainerVariant`] 内 RegretTable type 由 trainer
//! variant 决定）。
//!
//! ## 数据结构（D-510 字面）
//!
//! - **Open-addressed Robin Hood hashing**（不走 perfect hash；训练期 InfoSet
//!   动态发现，offline build perfect hash table 不可行）。
//! - **hash 函数** = **FxHash**（[`rustc_hash`] crate；InfoSetId 已是 stage 2
//!   D-218 64-bit pseudo-random 输出，FxHash 单 multiply + xor + shift 足够低
//!   碰撞 + 极快）。
//! - **load factor 上限** = **0.75**（超过触发 2× grow + rehash；D-518）。
//! - **初始 capacity** = **2^20 = 1,048,576 slot**（≈ 32 MiB per traverser per
//!   table；6 traverser × 2 table ≈ 384 MiB 起步）。
//! - **slot 布局** = **SoA 分离**：
//!   - `keys: Vec<u64>` — InfoSetId 数组，空槽用 `u64::MAX` 哨兵
//!   - `payloads: Vec<[i16; 16]>` — q15 quantized regret + 2 byte pad（64 byte
//!     单 cache line 对齐；padding `[14..16]` = `i16::MIN`）
//!   - `scales: Vec<f32>` — per-row scale factor
//! - **probe distance** = Robin Hood 字面（弱者让位强者，bounded probe length）。
//!
//! ## A1 \[实现\] 状态
//!
//! 所有方法体走 `unimplemented!()` 占位。B2 \[实现\] 落地 Robin Hood probe +
//! q15 quant/dequant + SIMD path（D-513 cfg-gate）。

#![allow(clippy::needless_pass_by_value)]

use std::hash::Hash;
use std::marker::PhantomData;

/// API-510 — 紧凑 RegretTable。
///
/// `<I>` 关联到 `Game::InfoSet`；bound 走 `Eq + Hash + Clone`（继承 stage 3
/// [`crate::training::regret::RegretTable`] 的 trait bound）。stage 5 D-510
/// 字面 hash key 走 `info_set.to_u64()`，所以实际 monomorphization 通常对应
/// `I = crate::abstraction::info::InfoSetId`（NlheGame6 路径）；type parameter
/// 保留让 Kuhn / Leduc / SimplifiedNlhe 测试通路可以复用。
///
/// # A1 \[实现\] 状态
///
/// 字段集字面锁；方法体走 `unimplemented!()` 占位。B2 \[实现\] 起步前消费
/// 全字段，`allow(dead_code)` 在 A1 stub 阶段抑制 dead-code 警告。
#[allow(dead_code)]
pub struct RegretTableCompact<I: Eq + Hash + Clone> {
    /// InfoSetId 数组，空槽用 `u64::MAX` 哨兵。
    pub(crate) keys: Vec<u64>,
    /// q15 quantized regret，padded 14 → 16 让 256-bit AVX2 register 单 row 覆盖
    /// （D-513 字面 padding `[14..16]` = `i16::MIN`）。
    pub(crate) payloads: Vec<[i16; 16]>,
    /// per-row scale factor（D-511 字面 row 量化范围 = `[-scale, scale)`）。
    pub(crate) scales: Vec<f32>,
    /// populated slot count。
    pub(crate) len: usize,
    /// power-of-two capacity（D-518 字面 grow 时 capacity `2^N → 2^(N+1)`）。
    pub(crate) capacity: usize,
    /// 保持 generic parameter 活跃；编译期 0-size。
    pub(crate) _info_set_marker: PhantomData<I>,
}

impl<I: Eq + Hash + Clone> RegretTableCompact<I> {
    /// API-510 — 以 InfoSet 数量估算构造空表。
    ///
    /// 实际 capacity 取 `max(estimated × 4/3, 2^20)` 向上对齐到 2^N（D-518 字面
    /// 初始 capacity）。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。
    pub fn with_initial_capacity_estimate(estimated_unique_info_sets: usize) -> Self {
        let _ = estimated_unique_info_sets;
        unimplemented!(
            "stage 5 A1 scaffold — RegretTableCompact::with_initial_capacity_estimate 落地于 B2 [实现]"
        )
    }

    /// API-511 — 查询单 (info_set, action) 的 regret f32 值。
    ///
    /// 内部走 FxHash probe → 命中 slot 后 dequant q15 × scale → f32。未命中返
    /// 回 `0.0`（lazy init 语义，继承 stage 3 D-323）。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。
    pub fn regret_at(&self, info_set: I, action: usize) -> f32 {
        let _ = (info_set, action);
        unimplemented!("stage 5 A1 scaffold — RegretTableCompact::regret_at 落地于 B2 [实现]")
    }

    /// API-512 — 累加 (info_set, action) 上的 regret delta。
    ///
    /// 内部走 probe-or-insert → quant delta to q15 → saturating_add → 检查 row
    /// overflow → 必要时 row-renorm（D-511 字面 overflow 处理走单 row 重 quantize）。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。
    pub fn add_regret(&mut self, info_set: I, action: usize, delta: f32) {
        let _ = (info_set, action, delta);
        unimplemented!("stage 5 A1 scaffold — RegretTableCompact::add_regret 落地于 B2 [实现]")
    }

    /// API-513 — RM+ in-place clamp `max(regret, 0)` 全表 SIMD（D-513 字面
    /// AVX2 hot path #1）。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。
    pub fn clamp_rm_plus(&mut self) {
        unimplemented!("stage 5 A1 scaffold — RegretTableCompact::clamp_rm_plus 落地于 B2 [实现]")
    }

    /// API-514 — Linear discounting lazy 路径（D-511 字面 scale-only decay）。
    ///
    /// 仅 mutate `scales[i] *= decay` for all populated slots；int16 payload
    /// 不动。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。
    pub fn scale_linear_lazy(&mut self, decay: f32) {
        let _ = decay;
        unimplemented!(
            "stage 5 A1 scaffold — RegretTableCompact::scale_linear_lazy 落地于 B2 [实现]"
        )
    }

    /// API-515 — populated slot 数。
    pub fn len(&self) -> usize {
        self.len
    }

    /// API-515 — `len == 0` 等价。
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// API-516 — 当前 alloc byte 数（含 metadata overhead）。
    ///
    /// 公式（D-544 字面）：`section_bytes = (keys.capacity() × 8) + (payloads
    /// .capacity() × 32) + (scales.capacity() × 4) + sizeof(metadata)`。
    /// **capacity 而非 len**：紧凑 array dynamic grow 时 capacity 即真实 alloc，
    /// metrics 反映真实 RSS 占用。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。
    pub fn section_bytes(&self) -> u64 {
        unimplemented!("stage 5 A1 scaffold — RegretTableCompact::section_bytes 落地于 B2 [实现]")
    }

    /// API-517 — 迭代非空 slot 返回 `(info_set_u64, &[i16; 16], scale)`。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。
    pub fn iter(&self) -> RegretTableCompactIter<'_, I> {
        unimplemented!("stage 5 A1 scaffold — RegretTableCompact::iter 落地于 B2 [实现]")
    }

    /// API-518 — 全表 scale 重标定（D-511 字面每 `1e6 iter` 触发）。
    ///
    /// 若 `max(|q15|) < 16384` 则 `scale /= 2` + 全 q15 `<< 1`；若 `max(|q15|) ==
    /// 32767` 则 `scale *= 2` + 全 q15 `>> 1`。保持 dynamic range。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。
    pub fn renormalize_scales(&mut self) {
        unimplemented!(
            "stage 5 A1 scaffold — RegretTableCompact::renormalize_scales 落地于 B2 [实现]"
        )
    }

    /// API-519 — Robin Hood probing 健康检查（D-569 anchor 字面）。
    ///
    /// 返回 `(max_probe_distance, avg_probe_distance, load_factor)`。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。
    pub fn collision_metrics(&self) -> CollisionMetrics {
        unimplemented!(
            "stage 5 A1 scaffold — RegretTableCompact::collision_metrics 落地于 B2 [实现]"
        )
    }
}

/// API-517 — `RegretTableCompact::iter` 返回 item 引用包装。
///
/// 实际 item 类型 = `(u64, &[i16; 16], f32)`（D-517 字面 keys 数组不共享 = 单表
/// 独立迭代）。
///
/// # A1 \[实现\] 状态
///
/// 字段集占位 `_marker: PhantomData<&'a I>`，B2 \[实现\] 落地真实 `&'a Vec`
/// 引用 + index 游标。
pub struct RegretTableCompactIter<'a, I: Eq + Hash + Clone> {
    pub(crate) _marker: PhantomData<&'a I>,
}

impl<'a, I: Eq + Hash + Clone> Iterator for RegretTableCompactIter<'a, I> {
    type Item = (u64, &'a [i16; 16], f32);
    fn next(&mut self) -> Option<Self::Item> {
        unimplemented!("stage 5 A1 scaffold — RegretTableCompactIter::next 落地于 B2 [实现]")
    }
}

/// API-519 — Robin Hood 健康度三元组。
#[derive(Clone, Copy, Debug)]
pub struct CollisionMetrics {
    /// 单次插入 / 查找经历的最大 probe 步数（D-569 阈值 ≤ 16）。
    pub max_probe_distance: usize,
    /// 全表 probe 步数均值（D-569 阈值 ≤ 2.0）。
    pub avg_probe_distance: f32,
    /// `len / capacity`（D-569 阈值 ≤ 0.75）。
    pub load_factor: f32,
}

// ---------------------------------------------------------------------------
// StrategyAccumulatorCompact — 同型布局，签名独立锁让 B2 \[实现\] 起步前两个
// 表可以独立演化（D-517 字面 RegretTable + StrategyAccumulator hash table **不
// 共享**；keys 数组各 alloc，避免 Linear discounting 不同更新频率破坏并发原子
// 性）。
// ---------------------------------------------------------------------------

/// API-526 — 紧凑 StrategyAccumulator。
///
/// 同 [`RegretTableCompact`] SoA 布局（D-517 字面 RegretTable + StrategyAccumulator
/// hash table **不共享**）。
///
/// # A1 \[实现\] 状态
///
/// 同 [`RegretTableCompact`] — 字段集字面锁；B2 \[实现\] 起步前消费全字段。
#[allow(dead_code)]
pub struct StrategyAccumulatorCompact<I: Eq + Hash + Clone> {
    pub(crate) keys: Vec<u64>,
    pub(crate) payloads: Vec<[i16; 16]>,
    pub(crate) scales: Vec<f32>,
    pub(crate) len: usize,
    pub(crate) capacity: usize,
    pub(crate) _info_set_marker: PhantomData<I>,
}

impl<I: Eq + Hash + Clone> StrategyAccumulatorCompact<I> {
    /// API-526 — 同 [`RegretTableCompact::with_initial_capacity_estimate`]。
    pub fn with_initial_capacity_estimate(estimated_unique_info_sets: usize) -> Self {
        let _ = estimated_unique_info_sets;
        unimplemented!(
            "stage 5 A1 scaffold — StrategyAccumulatorCompact::with_initial_capacity_estimate \
             落地于 B2 [实现]"
        )
    }

    /// API-526 — 累加 strategy_sum delta。
    pub fn add_strategy_sum(&mut self, info_set: I, action: usize, delta: f32) {
        let _ = (info_set, action, delta);
        unimplemented!(
            "stage 5 A1 scaffold — StrategyAccumulatorCompact::add_strategy_sum 落地于 B2 [实现]"
        )
    }

    /// API-526 — 单 InfoSet 上的 14-action 平均策略归一化输出。
    pub fn average_strategy(&self, info_set: I, out: &mut [f32; 14]) {
        let _ = (info_set, out);
        unimplemented!(
            "stage 5 A1 scaffold — StrategyAccumulatorCompact::average_strategy 落地于 B2 [实现]"
        )
    }

    /// API-526 — Linear discounting lazy 路径（D-511 同型 [`RegretTableCompact`]）。
    pub fn scale_linear_lazy(&mut self, decay: f32) {
        let _ = decay;
        unimplemented!(
            "stage 5 A1 scaffold — StrategyAccumulatorCompact::scale_linear_lazy 落地于 B2 [实现]"
        )
    }

    /// API-526 — 当前 alloc byte 数（D-544 同公式）。
    pub fn section_bytes(&self) -> u64 {
        unimplemented!(
            "stage 5 A1 scaffold — StrategyAccumulatorCompact::section_bytes 落地于 B2 [实现]"
        )
    }

    /// API-526 — populated slot 数。
    pub fn len(&self) -> usize {
        self.len
    }

    /// API-526 — `len == 0`。
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// API-526 — 同 [`RegretTableCompact::iter`]。
    pub fn iter(&self) -> StrategyAccumulatorCompactIter<'_, I> {
        unimplemented!("stage 5 A1 scaffold — StrategyAccumulatorCompact::iter 落地于 B2 [实现]")
    }

    /// API-526 — 全表 scale 重标定。
    pub fn renormalize_scales(&mut self) {
        unimplemented!(
            "stage 5 A1 scaffold — StrategyAccumulatorCompact::renormalize_scales 落地于 B2 [实现]"
        )
    }
}

/// API-526 — `StrategyAccumulatorCompact::iter` 返回 item 引用包装。
pub struct StrategyAccumulatorCompactIter<'a, I: Eq + Hash + Clone> {
    pub(crate) _marker: PhantomData<&'a I>,
}

impl<'a, I: Eq + Hash + Clone> Iterator for StrategyAccumulatorCompactIter<'a, I> {
    type Item = (u64, &'a [i16; 16], f32);
    fn next(&mut self) -> Option<Self::Item> {
        unimplemented!(
            "stage 5 A1 scaffold — StrategyAccumulatorCompactIter::next 落地于 B2 [实现]"
        )
    }
}
