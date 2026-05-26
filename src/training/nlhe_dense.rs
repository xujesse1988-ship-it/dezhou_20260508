//! NLHE 专用 dense infoset 表（`docs/temp/nlhe_dense_infoset_table_plan_2026_05_26.md`
//! Phase 1 原型）。
//!
//! 把 NLHE 的 `InfoSetId`（v2 layout：高 26 bit = betting tree `node_id`，低位
//! `bucket_id` / `street_tag`）映射成扁平数组下标，替代 `RegretTable<I>` /
//! `StrategyAccumulator<I>` 的 `HashMap<I, Vec<f64>>` 容器。布局是 §数据布局方案 A
//! 选定的 **full dense prealloc + 变长 action stride**：
//!
//! ```text
//! slot_base[node_id]  = prefix_sum_over_nodes(bucket_count(node) * action_count(node))
//! row_base[node_id]   = prefix_sum_over_nodes(bucket_count(node))
//! slot(info, a)       = slot_base[node_id] + bucket_id * action_count(node_id) + a
//! row(info)           = row_base[node_id]  + bucket_id
//! ```
//!
//! 每个 `(node_id, bucket_id)` 占连续 `action_count` 个 `f64` slot，2-action 节点不为
//! 6/8-action 浪费空间（Phase 0 实测目标 profile avg action_count 2.516）。
//!
//! **Phase 1 范围**（本模块）：indexer 索引数学 + 表的数值语义，**不接 trainer**。
//! 数值语义逐位对齐 [`crate::training::regret`]：
//! - regret matching（[`DenseNlheTable::current_strategy_smallvec_by_info`]）与
//!   [`crate::training::regret::RegretTable::current_strategy_smallvec`] 同一 R⁺
//!   累加 + 归一化序列。
//! - average strategy（[`DenseNlheTable::average_strategy_by_info`]）与
//!   [`crate::training::regret::StrategyAccumulator::average_strategy`] 同一 sum + 除法。
//! - full dense 下「未访问行」恒为 `0.0`，与 HashMap「key 缺失」走同一退化分支
//!   （均匀分布），故对外可观测策略 byte-equal。
//!
//! `touched_rows` bitset 不参与上面这条 byte-equal 语义（全 0 行本就退化成 uniform）；
//! 它服务于 Phase 2+ 的 Trainer public query「未见 infoset 返回空 `Vec`」语义、
//! Phase 4 sparse checkpoint、以及诊断。

use std::sync::Arc;

use crate::abstraction::info::{InfoSetId, StreetTag};
use crate::training::nlhe::NLHE_V2_NODE_ID_SHIFT;
use crate::training::nlhe_betting_tree::{NodeId, PublicBettingTree};
use crate::training::regret::SigmaVec;

/// 单个 betting tree 节点的 dense 布局元数据（按 `node_id` 索引）。
///
/// `slot_base` / `row_base` 是建表时一次性算好的 prefix sum；`bucket_count` /
/// `action_count` 决定该节点占多少行 / 每行多少 slot。`street` 仅作诊断 + checkpoint
/// fingerprint 用，不参与下标计算（bucket_count 已把街信息内化）。
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NlheDenseNodeMeta {
    /// 本节点第一个 slot 在扁平 values 数组中的下标 = Σ_{n<node} bucket×action。
    pub slot_base: u64,
    /// 本节点第一行在扁平 row 空间（touched bitset）中的下标 = Σ_{n<node} bucket。
    pub row_base: u64,
    /// 本节点的 bucket 数（preflop 169 / postflop 500）。
    pub bucket_count: u32,
    /// 本节点合法动作数 = `tree.node(id).legal_actions.len()`（变长 stride）。
    pub action_count: u8,
    /// 本节点所在街（诊断 / fingerprint）。
    pub street: StreetTag,
}

/// 建 indexer 的逐节点输入（`node_id` = 输入顺序下标）。把「从哪取 bucket/action 数」
/// 与 prefix-sum 计算解耦，让 indexer 既能从生产 betting tree 建，也能在单元测试里
/// 直接喂合成节点（不必构造完整树 / 巨型数组）。
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NlheNodeSpec {
    pub street: StreetTag,
    pub bucket_count: u32,
    pub action_count: u8,
}

/// `InfoSetId` → 扁平数组定位结果。`slot_start` 足以定位该 infoset 的 `action_count`
/// 个连续 slot；`row_index` 只用于 touched bitset 与诊断（§并行语义 local delta
/// 同型）。
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DenseSlot {
    pub slot_start: u64,
    pub row_index: u64,
    pub action_count: usize,
}

/// NLHE dense 表 indexer：持有每节点 prefix-sum 元数据 + 全表 row / slot 总数。
///
/// 与 [`crate::training::nlhe::pack_info_set_v2`] 共用 [`NLHE_V2_NODE_ID_SHIFT`]
/// 反解 `node_id`，pack / unpack 不会漂移。
#[derive(Debug)]
pub struct NlheDenseIndexer {
    nodes: Vec<NlheDenseNodeMeta>,
    total_rows: u64,
    total_slots: u64,
}

impl NlheDenseIndexer {
    /// 从逐节点 spec 建 indexer（`node_id` = 输入顺序下标）。prefix sum 一次扫完。
    ///
    /// panic：`action_count == 0`（决策节点必有 ≥1 合法动作）——这是建表期一次性
    /// 校验，命中说明上游 tree / spec 有 bug，宁可立刻炸而不是静默产出错位表。
    pub fn from_node_specs(specs: impl IntoIterator<Item = NlheNodeSpec>) -> Self {
        let mut nodes = Vec::new();
        let mut row_base: u64 = 0;
        let mut slot_base: u64 = 0;
        for spec in specs {
            assert!(
                spec.action_count >= 1,
                "node {} has action_count 0（决策节点必有合法动作）",
                nodes.len()
            );
            nodes.push(NlheDenseNodeMeta {
                slot_base,
                row_base,
                bucket_count: spec.bucket_count,
                action_count: spec.action_count,
                street: spec.street,
            });
            row_base += u64::from(spec.bucket_count);
            slot_base += u64::from(spec.bucket_count) * u64::from(spec.action_count);
        }
        Self {
            nodes,
            total_rows: row_base,
            total_slots: slot_base,
        }
    }

    /// 从生产 betting tree 建 indexer。`bucket_count_by_street` 按
    /// [`StreetTag`] 取值（下标 = `street as usize`），生产 = `[169, 500, 500, 500]`
    /// （preflop lossless / postflop v3 cafebabe）。
    ///
    /// `action_count` 逐节点取 `tree.node(id).legal_actions.len()`，因此天然兼容按街
    /// abstraction（flop 4-size 节点 7 动作、其余 3-size 节点 6 动作各取各的 stride）。
    ///
    /// panic：某节点 `legal_actions.len() > u8::MAX`（动作数远不该到 256，命中说明
    /// 树规模 / abstraction 超出预期）。
    pub fn from_tree(tree: &PublicBettingTree, bucket_count_by_street: [u32; 4]) -> Self {
        let specs = (0..tree.num_nodes() as NodeId).map(|id| {
            let node = tree.node(id);
            let action_count = node.legal_actions.len();
            assert!(
                action_count <= usize::from(u8::MAX),
                "node {id} action_count {action_count} 超 u8（树 / abstraction 异常）"
            );
            NlheNodeSpec {
                street: node.street,
                bucket_count: bucket_count_by_street[node.street as usize],
                action_count: action_count as u8,
            }
        });
        Self::from_node_specs(specs)
    }

    /// 把 `InfoSetId` 定位到扁平数组的 `(slot_start, row_index, action_count)`。
    ///
    /// `node_id = raw >> NLHE_V2_NODE_ID_SHIFT`（与 v2 packer 同 shift）；
    /// `bucket_id = info.bucket_id()`（低 24 bit）。
    #[inline]
    pub fn locate(&self, info: InfoSetId) -> DenseSlot {
        let node_id = (info.raw() >> NLHE_V2_NODE_ID_SHIFT) as usize;
        debug_assert!(
            node_id < self.nodes.len(),
            "node_id {node_id} 越界（indexer 有 {} 节点）；info 来自不同树？",
            self.nodes.len()
        );
        let meta = &self.nodes[node_id];
        let bucket_id = u64::from(info.bucket_id());
        debug_assert!(
            bucket_id < u64::from(meta.bucket_count),
            "bucket_id {bucket_id} >= bucket_count {}（node {node_id}）",
            meta.bucket_count
        );
        let action_count = u64::from(meta.action_count);
        DenseSlot {
            slot_start: meta.slot_base + bucket_id * action_count,
            row_index: meta.row_base + bucket_id,
            action_count: meta.action_count as usize,
        }
    }

    /// dense 表行数 = Σ bucket_count（应 == infoset 数；建 sizing 工具已自洽校验）。
    pub fn total_rows(&self) -> u64 {
        self.total_rows
    }

    /// dense 表 slot 数 = Σ bucket_count × action_count（variable stride 下的 f64 数）。
    pub fn total_slots(&self) -> u64 {
        self.total_slots
    }

    /// 节点数（= betting tree 决策节点数）。
    pub fn num_nodes(&self) -> usize {
        self.nodes.len()
    }

    /// 只读访问某节点元数据（诊断 / fingerprint）。
    pub fn node_meta(&self, node_id: NodeId) -> &NlheDenseNodeMeta {
        &self.nodes[node_id as usize]
    }
}

/// full dense prealloc 的一张值表（regret 或 strategy_sum 各一个实例，共享同一
/// `Arc<NlheDenseIndexer>`）。
///
/// `values` 在 [`Self::new`] 时一次性分配满 `total_slots`（§方案 A：接受未访问行
/// 的 0 占用，换热路径无 page lookup）。`touched_rows` 标记被写过的行，供 Phase 2+
/// public query / Phase 4 checkpoint。
///
/// 数值语义逐位对齐 [`crate::training::regret`]（模块文档）。
#[derive(Debug)]
pub struct DenseNlheTable {
    indexer: Arc<NlheDenseIndexer>,
    values: Vec<f64>,
    touched_rows: TouchedRows,
}

impl DenseNlheTable {
    /// 按 indexer 的 `total_slots` / `total_rows` 一次性分配 0 值表 + 空 bitset。
    pub fn new(indexer: Arc<NlheDenseIndexer>) -> Self {
        let total_slots = indexer.total_slots();
        let total_rows = indexer.total_rows();
        Self {
            indexer,
            values: vec![0.0; total_slots as usize],
            touched_rows: TouchedRows::new(total_rows),
        }
    }

    /// 持有的 indexer（共享只读）。
    pub fn indexer(&self) -> &Arc<NlheDenseIndexer> {
        &self.indexer
    }

    /// 已分配 slot 数（= `indexer.total_slots()`）。
    pub fn num_slots(&self) -> usize {
        self.values.len()
    }

    /// 累积到指定 slot（热路径入口；§并行语义 local delta 已带 `slot_start` /
    /// `row_index`，merge 时直接调用，省一次 `locate`）。
    ///
    /// `values[slot_start + a] += delta[a]`，并标记 `row_index` 已访问。f64 加法序列
    /// 与 [`crate::training::regret::RegretTable::accumulate`] 完全等价。
    #[inline]
    pub fn accumulate_by_slot(&mut self, slot_start: u64, row_index: u64, delta: &[f64]) {
        let start = slot_start as usize;
        let end = start + delta.len();
        debug_assert!(
            end <= self.values.len(),
            "slot_start {slot_start} + len {} 超 total_slots {}",
            delta.len(),
            self.values.len()
        );
        for (slot, &d) in self.values[start..end].iter_mut().zip(delta) {
            *slot += d;
        }
        self.touched_rows.set(row_index);
    }

    /// 便捷入口：先 `locate(info)` 再 [`Self::accumulate_by_slot`]。`delta.len()`
    /// 必须 == 该节点 action_count（debug_assert）。
    #[inline]
    pub fn accumulate_by_info(&mut self, info: InfoSetId, delta: &[f64]) {
        let slot = self.indexer.locate(info);
        debug_assert_eq!(
            slot.action_count,
            delta.len(),
            "accumulate_by_info: delta.len() {} != action_count {}",
            delta.len(),
            slot.action_count
        );
        self.accumulate_by_slot(slot.slot_start, slot.row_index, delta);
    }

    /// regret matching 热路径变体（返回 `SigmaVec` stack alloc）。
    ///
    /// 与 [`crate::training::regret::RegretTable::current_strategy_smallvec`] 同一
    /// R⁺ 累加 + 除法归一化：`Σ R⁺ > 0` 取 `R⁺ / Σ R⁺`，否则均匀分布。full dense 下
    /// 未访问行恒为 0 → `Σ R⁺ == 0` → uniform，与 HashMap key 缺失同分支，byte-equal。
    pub(crate) fn current_strategy_smallvec_by_info(&self, info: InfoSetId) -> SigmaVec {
        let slot = self.indexer.locate(info);
        self.current_strategy_smallvec_at(slot.slot_start, slot.action_count)
    }

    /// regret matching 热路径变体，直接按已定位的 `slot_start` / `action_count` 读
    /// （Phase 3 并行 recurse 已 `locate` 一次，省第二次 unpack）。数值序列与
    /// [`Self::current_strategy_smallvec_by_info`] 完全一致——后者就是先 `locate`
    /// 再调本方法。`&self` 只读，可在 rayon worker 间共享借用。
    pub(crate) fn current_strategy_smallvec_at(
        &self,
        slot_start: u64,
        action_count: usize,
    ) -> SigmaVec {
        let n = action_count;
        let start = slot_start as usize;
        let regrets = &self.values[start..start + n];

        let uniform = || SigmaVec::from_elem(1.0 / n as f64, n);
        let mut positives: SigmaVec = SigmaVec::with_capacity(n);
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

    /// regret matching public 入口（返回 owned `Vec<f64>`，API 形态对齐
    /// [`crate::training::regret::RegretTable::current_strategy`]）。
    pub fn current_strategy_by_info(&self, info: InfoSetId) -> Vec<f64> {
        self.current_strategy_smallvec_by_info(info).into_vec()
    }

    /// average strategy：`avg_σ(a) = S(a) / Σ_b S(b)`，`Σ S == 0` 退化均匀分布。
    ///
    /// 与 [`crate::training::regret::StrategyAccumulator::average_strategy`] 同一
    /// sum + 除法序列。full dense 下未访问行 sum == 0 → uniform，byte-equal。
    pub fn average_strategy_by_info(&self, info: InfoSetId) -> Vec<f64> {
        let slot = self.indexer.locate(info);
        let n = slot.action_count;
        let start = slot.slot_start as usize;
        let sums = &self.values[start..start + n];

        let total: f64 = sums.iter().sum();
        if total > 0.0 {
            sums.iter().map(|s| s / total).collect()
        } else {
            vec![1.0 / n as f64; n]
        }
    }

    /// 整表逐元素 × factor（LCFR period boundary rescale，语义同
    /// [`crate::training::regret::RegretTable::rescale_all`]）。
    ///
    /// full dense 下扫满整张表，未访问行 `0.0 * factor == 0.0` 无副作用——对可观测
    /// 策略与 HashMap「只 scale 已访问 entry」byte-equal（缺失 entry 隐含 0）。
    pub fn rescale_all(&mut self, factor: f64) {
        for slot in self.values.iter_mut() {
            *slot *= factor;
        }
    }

    /// 该 infoset 行各 slot 值之和（只读诊断）。strategy_sum 表上 `> 0` 等价 HashMap
    /// 路径「entry present 且非全零」——LBR Hybrid 退化判定 / `HasAverage` probe filter
    /// 用它做后端无关的「该 infoset 有 average 信号吗」判断。未访问行恒 0 → 0.0。
    pub fn row_sum_by_info(&self, info: InfoSetId) -> f64 {
        let slot = self.indexer.locate(info);
        let start = slot.slot_start as usize;
        self.values[start..start + slot.action_count].iter().sum()
    }

    /// 某行是否被写过（Phase 2+ public query「未见 infoset 返回空 `Vec`」语义入口）。
    pub fn touched_row(&self, row_index: u64) -> bool {
        self.touched_rows.get(row_index)
    }

    /// 已访问行数（诊断 / sparse checkpoint 预估）。
    pub fn touched_count(&self) -> u64 {
        self.touched_rows.count()
    }

    /// raw 扁平 values 只读 slice（Phase 4 checkpoint save：直接写 raw f64 LE）。
    pub(crate) fn raw_values(&self) -> &[f64] {
        &self.values
    }

    /// raw 扁平 values 可写 slice（Phase 4 checkpoint load：streaming 填回 raw f64）。
    /// 调用方负责保证写入长度 == `num_slots()`。
    pub(crate) fn raw_values_mut(&mut self) -> &mut [f64] {
        &mut self.values
    }

    /// touched bitset words 只读 slice（Phase 4 checkpoint save）。
    pub(crate) fn touched_words(&self) -> &[u64] {
        self.touched_rows.words()
    }

    /// touched bitset words 可写 slice（Phase 4 checkpoint load）。
    pub(crate) fn touched_words_mut(&mut self) -> &mut [u64] {
        self.touched_rows.words_mut()
    }
}

/// 行级 touched bitset（`Vec<u64>` word 数组；无第三方依赖，符合 D-275
/// `unsafe_code = "forbid"`）。
#[derive(Debug)]
struct TouchedRows {
    words: Vec<u64>,
    len: u64,
}

impl TouchedRows {
    fn new(len: u64) -> Self {
        let n_words = (len as usize).div_ceil(64);
        Self {
            words: vec![0u64; n_words],
            len,
        }
    }

    #[inline]
    fn set(&mut self, idx: u64) {
        debug_assert!(idx < self.len, "row {idx} 越界（共 {} 行）", self.len);
        let word = (idx / 64) as usize;
        let bit = (idx % 64) as u32;
        self.words[word] |= 1u64 << bit;
    }

    #[inline]
    fn get(&self, idx: u64) -> bool {
        debug_assert!(idx < self.len, "row {idx} 越界（共 {} 行）", self.len);
        let word = (idx / 64) as usize;
        let bit = (idx % 64) as u32;
        (self.words[word] >> bit) & 1 == 1
    }

    fn count(&self) -> u64 {
        self.words.iter().map(|w| u64::from(w.count_ones())).sum()
    }

    /// 底层 word 数组只读（Phase 4 checkpoint save）。长度 = `total_rows.div_ceil(64)`。
    fn words(&self) -> &[u64] {
        &self.words
    }

    /// 底层 word 数组可写（Phase 4 checkpoint load：直接填回持久化的 bit）。
    fn words_mut(&mut self) -> &mut [u64] {
        &mut self.words
    }
}

/// Phase 3 并行 worker 的线程本地 delta accumulator（plan §并行语义 slot-based
/// local delta）。HashMap 路径的 `(InfoSetId, SigmaVec)` 在这里换成
/// `(slot_start, row_index, SigmaVec)`：merge 时 main thread 直接调
/// [`DenseNlheTable::accumulate_by_slot`]，省掉一次 `locate`。`row_index` 仅供
/// touched bitset 标记。
///
/// 与 [`crate::training::regret::LocalRegretDelta`] 同型：按 DFS 顺序 append，
/// 不 dedup / 不 sort；merge 阶段 main thread 按 tid 升序 × 每 worker 内 push
/// 顺序 playback，f64 加法序列 deterministic（跨 run BLAKE3 byte-equal）。
#[derive(Debug, Default)]
pub(crate) struct DenseLocalDelta {
    entries: Vec<(u64, u64, SigmaVec)>,
}

impl DenseLocalDelta {
    /// 空容器。
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Append 1 条 `(slot_start, row_index, delta)`；不 dedup / 不 sort。
    pub(crate) fn push(&mut self, slot_start: u64, row_index: u64, delta: SigmaVec) {
        self.entries.push((slot_start, row_index, delta));
    }

    /// 消费返回 owned entries（merge 入口）。
    pub(crate) fn into_entries(self) -> Vec<(u64, u64, SigmaVec)> {
        self.entries
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abstraction::info::StreetTag;
    use crate::rules::config::TableConfig;
    use crate::training::nlhe::pack_info_set_v2;
    use crate::training::regret::{RegretTable, StrategyAccumulator};
    use std::collections::HashSet;

    fn spec(street: StreetTag, bucket_count: u32, action_count: u8) -> NlheNodeSpec {
        NlheNodeSpec {
            street,
            bucket_count,
            action_count,
        }
    }

    /// 生产 profile bucket 数（preflop lossless 169 / postflop v3 500）。
    const PROD_BUCKETS: [u32; 4] = [169, 500, 500, 500];

    /// 默认全街 `{0.5,1,2}` 树上 indexer 的 total_rows / total_slots 必须与
    /// `tools/nlhe_betting_tree_sizing.rs` 独立 walk 的实测一致（plan §规模估算当前
    /// 119.7M profile 表）。两条代码路径（indexer prefix-sum vs sizing 工具 walk）
    /// 对同一棵树算出同样的量 = 索引数学的强外部对照。
    #[test]
    fn indexer_default_tree_matches_sizing_tool() {
        let tree = PublicBettingTree::build(&TableConfig::default_hu_200bb());
        let idx = NlheDenseIndexer::from_tree(&tree, PROD_BUCKETS);
        assert_eq!(idx.num_nodes(), 240_096, "默认树节点数");
        assert_eq!(
            idx.total_rows(),
            119_746_128,
            "total_rows 必须 == infoset 数（sizing 工具自洽校验值）"
        );
        assert_eq!(
            idx.total_slots(),
            310_151_877,
            "total_slots 必须 == sizing 工具实测 variable-action slot 数"
        );
        // total_rows 还应等于 Σ per-node bucket_count（再算一遍兜底）。
        let summed_rows: u64 = (0..idx.num_nodes() as NodeId)
            .map(|id| u64::from(idx.node_meta(id).bucket_count))
            .sum();
        assert_eq!(summed_rows, idx.total_rows());
    }

    /// indexer action_count 必须逐节点等于 betting tree 的 `legal_actions.len()`
    /// （spot-check 前若干节点 + root）。
    #[test]
    fn indexer_action_count_matches_tree() {
        let tree = PublicBettingTree::build(&TableConfig::default_hu_200bb());
        let idx = NlheDenseIndexer::from_tree(&tree, PROD_BUCKETS);
        for id in 0..2000u32.min(tree.num_nodes() as u32) {
            assert_eq!(
                usize::from(idx.node_meta(id).action_count),
                tree.node(id).legal_actions.len(),
                "node {id} action_count 与 tree 不一致"
            );
            assert_eq!(idx.node_meta(id).street, tree.node(id).street);
        }
    }

    /// 索引数学不变量（小合成树，穷举所有 (node, bucket)）：
    /// - `slot_start == slot_base + bucket * action_count`
    /// - `row_index == row_base + bucket`
    /// - `slot_start + action_count <= total_slots`，`row_index < total_rows`
    /// - 不同 (node, bucket) → 不同 row（row 全单射，count == total_rows）
    /// - slot 区间互不重叠（按 start 排序后 next.start >= prev.end）
    #[test]
    fn index_math_invariants_exhaustive_small() {
        // 用小 bucket 数让穷举便宜；action 数覆盖 2/3/6/7（目标 profile 实际出现值）。
        let specs = [
            spec(StreetTag::Preflop, 5, 3),
            spec(StreetTag::Flop, 4, 7),
            spec(StreetTag::Flop, 6, 6),
            spec(StreetTag::Turn, 3, 2),
            spec(StreetTag::River, 7, 4),
        ];
        let idx = NlheDenseIndexer::from_node_specs(specs.iter().copied());

        let mut seen_rows: HashSet<u64> = HashSet::new();
        let mut slot_ranges: Vec<(u64, u64)> = Vec::new();
        for (node_id, s) in specs.iter().enumerate() {
            let meta = idx.node_meta(node_id as NodeId);
            assert_eq!(usize::from(meta.action_count), usize::from(s.action_count));
            for bucket in 0..s.bucket_count {
                let info = pack_info_set_v2(bucket, node_id as NodeId, s.street);
                let loc = idx.locate(info);
                assert_eq!(loc.action_count, usize::from(s.action_count));
                assert_eq!(
                    loc.slot_start,
                    meta.slot_base + u64::from(bucket) * u64::from(s.action_count)
                );
                assert_eq!(loc.row_index, meta.row_base + u64::from(bucket));
                assert!(loc.slot_start + loc.action_count as u64 <= idx.total_slots());
                assert!(loc.row_index < idx.total_rows());
                assert!(seen_rows.insert(loc.row_index), "row 碰撞 @ {loc:?}");
                slot_ranges.push((loc.slot_start, loc.slot_start + loc.action_count as u64));
            }
        }
        assert_eq!(seen_rows.len() as u64, idx.total_rows(), "row 必须全单射");

        slot_ranges.sort_unstable();
        for w in slot_ranges.windows(2) {
            assert!(w[0].1 <= w[1].0, "slot 区间重叠：{:?} vs {:?}", w[0], w[1]);
        }
        assert_eq!(slot_ranges.last().unwrap().1, idx.total_slots());
    }

    // xorshift64：测试内确定性伪随机 f64 ∈ [-1, 1)，制造含负 regret 的 delta。
    fn next_f64(state: &mut u64) -> f64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        ((x >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }

    fn bits(v: &[f64]) -> Vec<u64> {
        v.iter().map(|x| x.to_bits()).collect()
    }

    /// dense 表的 current_strategy / average_strategy 必须与 HashMap
    /// `RegretTable` / `StrategyAccumulator` 在同一 delta 序列下 **byte-equal**
    /// （f64 to_bits 逐位）。对相同 info 多次累积 + 含负 regret + rescale，覆盖：
    /// - R⁺ clamp（负 regret → 0）
    /// - Σ R⁺ == 0 退化 uniform（全负）
    /// - 未访问 info 退化 uniform
    /// - 多节点不串扰（串扰会让某 info 策略偏离 HashMap，被逐位比较抓到）
    #[test]
    fn byte_equal_vs_hashmap_under_synthetic_deltas() {
        let specs = [
            spec(StreetTag::Preflop, 3, 3),
            spec(StreetTag::Flop, 4, 6),
            spec(StreetTag::Flop, 2, 7),
            spec(StreetTag::Turn, 3, 2),
            spec(StreetTag::River, 5, 4),
        ];
        let idx = Arc::new(NlheDenseIndexer::from_node_specs(specs.iter().copied()));

        let mut regret_hm: RegretTable<InfoSetId> = RegretTable::new();
        let mut strat_hm: StrategyAccumulator<InfoSetId> = StrategyAccumulator::new();
        let mut regret_dense = DenseNlheTable::new(Arc::clone(&idx));
        let mut strat_dense = DenseNlheTable::new(Arc::clone(&idx));

        // 枚举一部分 (node, bucket) 作为被访问 infoset；其余留作未访问对照。
        let mut visited: Vec<(InfoSetId, usize)> = Vec::new();
        for (node_id, s) in specs.iter().enumerate() {
            // 每节点取偶数 bucket，制造「部分 bucket 未访问」。
            for bucket in (0..s.bucket_count).step_by(2) {
                visited.push((
                    pack_info_set_v2(bucket, node_id as NodeId, s.street),
                    usize::from(s.action_count),
                ));
            }
        }

        // 3 轮：同一 info 多次累积，f64 加法顺序在两条路径上一致。
        let mut rng = 0x9E37_79B9_7F4A_7C15_u64;
        for round in 0..3 {
            for &(info, n) in &visited {
                let regret_delta: Vec<f64> = (0..n).map(|_| next_f64(&mut rng) * 4.0).collect();
                // strategy 累积量非负（reach × σ）；取正幅度。
                let strat_delta: Vec<f64> =
                    (0..n).map(|_| (next_f64(&mut rng) + 1.0) * 0.5).collect();
                regret_hm.accumulate(info, &regret_delta);
                regret_dense.accumulate_by_info(info, &regret_delta);
                strat_hm.accumulate(info, &strat_delta);
                strat_dense.accumulate_by_info(info, &strat_delta);
            }
            // LCFR period boundary rescale（HashMap 只 scale 已访问，dense scale 全表）。
            if round == 1 {
                let factor = 2.0 / 3.0;
                regret_hm.rescale_all(factor);
                regret_dense.rescale_all(factor);
                strat_hm.rescale_all(factor);
                strat_dense.rescale_all(factor);
            }
        }

        // 已访问 info：current_strategy + average_strategy 必须逐位相等。
        for &(info, n) in &visited {
            assert_eq!(
                bits(&regret_hm.current_strategy(&info, n)),
                bits(&regret_dense.current_strategy_by_info(info)),
                "current_strategy byte mismatch @ info {:#x}",
                info.raw()
            );
            assert_eq!(
                bits(&strat_hm.average_strategy(&info, n)),
                bits(&strat_dense.average_strategy_by_info(info)),
                "average_strategy byte mismatch @ info {:#x}",
                info.raw()
            );
        }

        // 未访问 info（奇数 bucket）：两路径都退化均匀分布，逐位相等。
        for (node_id, s) in specs.iter().enumerate() {
            for bucket in (1..s.bucket_count).step_by(2) {
                let info = pack_info_set_v2(bucket, node_id as NodeId, s.street);
                let n = usize::from(s.action_count);
                assert!(!regret_dense.touched_row(idx.locate(info).row_index));
                assert_eq!(
                    bits(&regret_hm.current_strategy(&info, n)),
                    bits(&regret_dense.current_strategy_by_info(info)),
                    "未访问 current_strategy 应均匀且 byte-equal"
                );
                assert_eq!(
                    bits(&strat_hm.average_strategy(&info, n)),
                    bits(&strat_dense.average_strategy_by_info(info)),
                    "未访问 average_strategy 应均匀且 byte-equal"
                );
            }
        }
    }

    /// touched bitset：只标记被 accumulate 过的行，count 等于不同访问行数。
    #[test]
    fn touched_rows_track_accumulated_rows() {
        let specs = [spec(StreetTag::Preflop, 4, 3), spec(StreetTag::Flop, 5, 6)];
        let idx = Arc::new(NlheDenseIndexer::from_node_specs(specs.iter().copied()));
        let mut table = DenseNlheTable::new(Arc::clone(&idx));

        let touched_infos = [
            pack_info_set_v2(0, 0, StreetTag::Preflop),
            pack_info_set_v2(2, 0, StreetTag::Preflop),
            pack_info_set_v2(3, 1, StreetTag::Flop),
        ];
        for &info in &touched_infos {
            let n = idx.locate(info).action_count;
            table.accumulate_by_info(info, &vec![1.0; n]);
        }
        assert_eq!(table.touched_count(), touched_infos.len() as u64);
        for &info in &touched_infos {
            assert!(table.touched_row(idx.locate(info).row_index));
        }
        // 未写过的行
        assert!(!table.touched_row(
            idx.locate(pack_info_set_v2(1, 0, StreetTag::Preflop))
                .row_index
        ));
    }
}
