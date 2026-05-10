//! Equity calculator（API §3）。
//!
//! `EquityCalculator` trait + `EquityError` enum + `MonteCarloEquity`
//! （D-220 / D-220a-rev1 / D-221 / D-222 / D-223 / D-224-rev1 / D-227 / D-228）。
//!
//! **离线 clustering 训练路径专用**——运行时映射禁止触发（D-225）。`f64`
//! 出现在本模块是显式允许的——本路径在 `abstraction::equity` /
//! `abstraction::cluster` 子模块，与 `abstraction::map` 子模块（禁浮点，D-252）
//! 物理隔离。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use thiserror::Error;

use crate::abstraction::cluster::rng_substream::{
    derive_substream_seed, OCHS_FEATURE_INNER, OCHS_WARMUP,
};
use crate::abstraction::cluster::{kmeans_fit, reorder_by_ehs_median, KMeansConfig};
use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::{Card, Rank, Suit};
use crate::eval::HandEvaluator;

/// Equity 计算 trait。
///
/// **错误返回**（EquityCalculator-rev1 / D-224-rev1 / EQ-002-rev1）：4 个方法均
/// 返回 `Result<_, EquityError>`，把无效输入（重叠 / 板长非法 / iter=0 /
/// 内部错误）与合法 `Ok` 分流；EQ-002 finite invariant 仅适用于 `Ok` 路径。
pub trait EquityCalculator: Send + Sync {
    /// **hand-vs-uniform-random-hole** equity（EHS 路径，D-223）。对手 hole
    /// uniform over remaining cards。`Ok(x)` 时 `x ∈ [0.0, 1.0]` 且 finite
    /// （D-224 / EQ-002-rev1）。
    ///
    /// **不**满足反对称：`equity(A, board) + equity(B, board) ≠ 1`。EQ-001
    /// 反对称断言不要用本接口；用 `equity_vs_hand`。
    ///
    /// 错误：`InvalidBoardLen` / `OverlapBoard` / `IterTooLow` / `Internal`。
    fn equity(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError>;

    /// **pairwise** hand-vs-specific-hand equity（D-220a-rev1 / EQ-001 反对称
    /// 路径唯一接口；OCHS 内部计算的基本原语，D-223）。`Ok(x)` 时
    /// `x ∈ [0.0, 1.0]` 且 finite，含 ties counted as 0.5。
    ///
    /// 计算口径见 `docs/pluribus_stage2_api.md` §3 EQ-001-rev1：
    ///
    /// - **river**（`board.len() == 5`）：直接评估两手牌力，1.0/0.5/0.0 离散，
    ///   无 RNG 消费。
    /// - **turn**（`board.len() == 4`）：枚举 44 张未发 river，确定性。
    /// - **flop**（`board.len() == 3`）：枚举 `C(45, 2) = 990` 个 (turn, river)
    ///   无序对，确定性。
    /// - **preflop**（`board.len() == 0`）：outer Monte Carlo over 5-card 完整
    ///   公共牌组合，消费 RngSource，sub-stream 派生 D-228 `EQUITY_MONTE_CARLO`。
    ///   严格反对称需要**两个独立 RngSource，从同一 sub_seed 构造**（EQ-001-rev1）。
    ///
    /// 错误：`OverlapHole` / `OverlapBoard` / `InvalidBoardLen`。
    fn equity_vs_hand(
        &self,
        hole: [Card; 2],
        opp_hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError>;

    /// EHS²（potential-aware 二阶矩，D-223）。`Ok(x)` 时 `x ∈ [0.0, 1.0]`。
    /// river 状态退化为 `equity²`。
    fn ehs_squared(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError>;

    /// OCHS 向量（D-222 默认 N=8）。长度 = `n_opp_clusters`。
    /// `Ok(v)` 时 `v.len() == n_opp_clusters` 且每维 `∈ [0.0, 1.0]` 且 finite。
    ///
    /// 内部以 `equity_vs_hand` 为原语：每个 cluster k 的输出值 ≈
    /// `mean over opp ∈ cluster_k of equity_vs_hand(hole, opp, board, rng)`，
    /// 具体抽样 / 枚举策略由 \[实现\] 在 B2 / C2 选定（D-222 锁 N=8 + RngSource
    /// sub-stream 派生 D-228 `OCHS_FEATURE_INNER`）。
    fn ochs(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<Vec<f64>, EquityError>;
}

/// equity 错误（EquityCalculator-rev1 / D-224-rev1）。
#[derive(Debug, Error)]
pub enum EquityError {
    /// `opp_hole` 与 `hole` 重叠（同张牌）。
    #[error("opp_hole overlaps with hole: card {card:?}")]
    OverlapHole { card: Card },

    /// `hole` 或 `opp_hole` 与 `board` 重叠。
    #[error("hole or opp_hole overlaps with board: card {card:?}")]
    OverlapBoard { card: Card },

    /// `board.len() ∉ {0, 3, 4, 5}`。
    #[error("invalid board length: expected 0/3/4/5, got {got}")]
    InvalidBoardLen { got: usize },

    /// Monte Carlo `iter == 0`。默认 D-220 = 10_000 不触发，stage 4 消融可触发。
    #[error("Monte Carlo iter too low: expected >= 1, got {got}")]
    IterTooLow { got: u32 },

    /// 评估器内部错误透传（继承 stage 1 `HandEvaluator` 错误，可能性极低）。
    #[error("equity evaluator internal error: {0}")]
    Internal(String),
}

/// Monte Carlo equity 实现。基于 stage 1 `HandEvaluator`
/// （`pluribus_stage1_api.md` §6）。
pub struct MonteCarloEquity {
    iter: u32,
    n_opp_clusters: u8,
    evaluator: Arc<dyn HandEvaluator>,
}

impl MonteCarloEquity {
    /// 默认配置：`iter = 10_000`、`n_opp_clusters = 8`（D-220 / D-222）。
    pub fn new(evaluator: Arc<dyn HandEvaluator>) -> MonteCarloEquity {
        MonteCarloEquity {
            iter: 10_000,
            n_opp_clusters: 8,
            evaluator,
        }
    }

    /// 自定义 iter（CI 短测试可降到 1,000；clustering 训练必须用默认 10k）。
    pub fn with_iter(self, iter: u32) -> MonteCarloEquity {
        MonteCarloEquity { iter, ..self }
    }

    /// 自定义 OCHS opponent cluster 数（stage 2 默认 8；stage 4 消融可调）。
    pub fn with_opp_clusters(self, n: u8) -> MonteCarloEquity {
        MonteCarloEquity {
            n_opp_clusters: n,
            ..self
        }
    }

    pub fn iter(&self) -> u32 {
        self.iter
    }

    pub fn n_opp_clusters(&self) -> u8 {
        self.n_opp_clusters
    }
}

impl EquityCalculator for MonteCarloEquity {
    fn equity(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError> {
        validate_board_len(board)?;
        if self.iter == 0 {
            return Err(EquityError::IterTooLow { got: 0 });
        }
        let used = build_used_set(&hole, &[], board)?;
        let needed_board = 5 - board.len();
        let mut wins_x2: u64 = 0;
        for _ in 0..self.iter {
            let (opp_hole, full_board) = sample_opp_and_board(&used, board, needed_board, rng);
            let me = build_seven_cards(hole, &full_board);
            let opp = build_seven_cards(opp_hole, &full_board);
            wins_x2 += compare_x2(self.evaluator.eval7(&me), self.evaluator.eval7(&opp));
        }
        Ok(wins_x2 as f64 / (2.0 * self.iter as f64))
    }

    fn equity_vs_hand(
        &self,
        hole: [Card; 2],
        opp_hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError> {
        validate_board_len(board)?;
        check_hole_opp_disjoint(&hole, &opp_hole)?;
        let _used = build_used_set(&hole, &opp_hole, board)?;
        match board.len() {
            5 => {
                let me = build_seven_cards(hole, board);
                let opp = build_seven_cards(opp_hole, board);
                let rm = self.evaluator.eval7(&me);
                let ro = self.evaluator.eval7(&opp);
                let v = if rm > ro {
                    1.0
                } else if rm == ro {
                    0.5
                } else {
                    0.0
                };
                Ok(v)
            }
            4 => {
                let used = build_used_set(&hole, &opp_hole, board)?;
                let mut sum_x2: u64 = 0;
                let mut count: u64 = 0;
                for v in 0..52u8 {
                    if used[v as usize] {
                        continue;
                    }
                    let river = Card::from_u8(v).expect("0..52");
                    let mut full_board: [Card; 5] = [board[0]; 5];
                    full_board[0] = board[0];
                    full_board[1] = board[1];
                    full_board[2] = board[2];
                    full_board[3] = board[3];
                    full_board[4] = river;
                    let me = build_seven_cards(hole, &full_board);
                    let opp = build_seven_cards(opp_hole, &full_board);
                    sum_x2 += compare_x2(self.evaluator.eval7(&me), self.evaluator.eval7(&opp));
                    count += 1;
                }
                Ok(sum_x2 as f64 / (2.0 * count as f64))
            }
            3 => {
                let used = build_used_set(&hole, &opp_hole, board)?;
                let unused: Vec<u8> = (0..52u8).filter(|v| !used[*v as usize]).collect();
                let mut sum_x2: u64 = 0;
                let mut count: u64 = 0;
                for i in 0..unused.len() {
                    for j in (i + 1)..unused.len() {
                        let turn = Card::from_u8(unused[i]).expect("0..52");
                        let river = Card::from_u8(unused[j]).expect("0..52");
                        let full_board: [Card; 5] = [board[0], board[1], board[2], turn, river];
                        let me = build_seven_cards(hole, &full_board);
                        let opp = build_seven_cards(opp_hole, &full_board);
                        sum_x2 += compare_x2(self.evaluator.eval7(&me), self.evaluator.eval7(&opp));
                        count += 1;
                    }
                }
                Ok(sum_x2 as f64 / (2.0 * count as f64))
            }
            0 => {
                if self.iter == 0 {
                    return Err(EquityError::IterTooLow { got: 0 });
                }
                let used = build_used_set(&hole, &opp_hole, board)?;
                let mut sum_x2: u64 = 0;
                for _ in 0..self.iter {
                    let full_board = sample_n_board_cards(&used, 5, rng);
                    let me = build_seven_cards(hole, &full_board);
                    let opp = build_seven_cards(opp_hole, &full_board);
                    sum_x2 += compare_x2(self.evaluator.eval7(&me), self.evaluator.eval7(&opp));
                }
                Ok(sum_x2 as f64 / (2.0 * self.iter as f64))
            }
            _ => unreachable!("validate_board_len rejects other lengths"),
        }
    }

    fn ehs_squared(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError> {
        validate_board_len(board)?;
        if self.iter == 0 {
            return Err(EquityError::IterTooLow { got: 0 });
        }
        match board.len() {
            5 => {
                let inner = self.equity(hole, board, rng)?;
                Ok(inner * inner)
            }
            4 => {
                let used = build_used_set(&hole, &[], board)?;
                let mut sum_squared: f64 = 0.0;
                let mut count: u32 = 0;
                for v in 0..52u8 {
                    if used[v as usize] {
                        continue;
                    }
                    let river_card = Card::from_u8(v).expect("0..52");
                    let full_board: [Card; 5] =
                        [board[0], board[1], board[2], board[3], river_card];
                    let inner = self.equity(hole, &full_board, rng)?;
                    sum_squared += inner * inner;
                    count += 1;
                }
                Ok(sum_squared / count as f64)
            }
            3 => {
                let used = build_used_set(&hole, &[], board)?;
                let unused: Vec<u8> = (0..52u8).filter(|v| !used[*v as usize]).collect();
                let mut sum_squared: f64 = 0.0;
                let mut count: u32 = 0;
                for i in 0..unused.len() {
                    for j in (i + 1)..unused.len() {
                        let turn = Card::from_u8(unused[i]).expect("0..52");
                        let river = Card::from_u8(unused[j]).expect("0..52");
                        let full_board: [Card; 5] = [board[0], board[1], board[2], turn, river];
                        let inner = self.equity(hole, &full_board, rng)?;
                        sum_squared += inner * inner;
                        count += 1;
                    }
                }
                Ok(sum_squared / count as f64)
            }
            0 => {
                let used = build_used_set(&hole, &[], board)?;
                let mut sum_squared: f64 = 0.0;
                for _ in 0..self.iter {
                    let full_board = sample_n_board_cards(&used, 5, rng);
                    let inner = self.equity(hole, &full_board, rng)?;
                    sum_squared += inner * inner;
                }
                Ok(sum_squared / self.iter as f64)
            }
            _ => unreachable!("validate_board_len rejects other lengths"),
        }
    }

    fn ochs(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<Vec<f64>, EquityError> {
        validate_board_len(board)?;
        if self.iter == 0 {
            return Err(EquityError::IterTooLow { got: 0 });
        }
        let _used = build_used_set(&hole, &[], board)?;
        let n = u32::from(self.n_opp_clusters);
        let table = ochs_table(n, &self.evaluator);
        let mut out = Vec::with_capacity(n as usize);
        for cluster_id in 0..n as usize {
            let classes = &table.classes_per_cluster[cluster_id];
            let mut sum = 0.0_f64;
            let mut count = 0u32;
            for &class_id in classes {
                let opp = table.representative_hole[class_id as usize];
                // Skip class representatives that clash with (hole, board); the
                // remaining classes in the cluster carry the cluster's signal.
                // §C-rev2 §3：所有 reps 都冲突时 fallback 0.5（与 B2 stub 同型，但
                // 几乎不会触发——169 classes ÷ 8 clusters ≈ 21 reps/cluster vs ≤ 7
                // 不可用 cards on (hole + 5-card board)）。
                if pair_overlaps(&hole, &opp) || any_overlaps_board(&opp, board) {
                    continue;
                }
                let v = self.equity_vs_hand(hole, opp, board, rng)?;
                sum += v;
                count += 1;
            }
            let mean = if count > 0 {
                sum / f64::from(count)
            } else {
                0.5
            };
            out.push(mean);
        }
        Ok(out)
    }
}

// ============================================================================
// 内部 helper
// ============================================================================

fn validate_board_len(board: &[Card]) -> Result<(), EquityError> {
    match board.len() {
        0 | 3 | 4 | 5 => Ok(()),
        n => Err(EquityError::InvalidBoardLen { got: n }),
    }
}

fn check_hole_opp_disjoint(hole: &[Card; 2], opp_hole: &[Card; 2]) -> Result<(), EquityError> {
    for &h in hole {
        for &o in opp_hole {
            if h.to_u8() == o.to_u8() {
                return Err(EquityError::OverlapHole { card: h });
            }
        }
    }
    Ok(())
}

fn build_used_set(
    hole: &[Card; 2],
    opp_hole: &[Card],
    board: &[Card],
) -> Result<[bool; 52], EquityError> {
    let mut used = [false; 52];
    for c in hole.iter() {
        let idx = c.to_u8() as usize;
        if used[idx] {
            return Err(EquityError::OverlapBoard { card: *c });
        }
        used[idx] = true;
    }
    for c in opp_hole.iter() {
        let idx = c.to_u8() as usize;
        if used[idx] {
            return Err(EquityError::OverlapBoard { card: *c });
        }
        used[idx] = true;
    }
    for c in board.iter() {
        let idx = c.to_u8() as usize;
        if used[idx] {
            return Err(EquityError::OverlapBoard { card: *c });
        }
        used[idx] = true;
    }
    Ok(used)
}

fn build_seven_cards(hole: [Card; 2], board: &[Card]) -> [Card; 7] {
    debug_assert_eq!(board.len(), 5, "build_seven_cards expects 5-card board");
    [
        hole[0], hole[1], board[0], board[1], board[2], board[3], board[4],
    ]
}

fn compare_x2(me: crate::eval::HandRank, opp: crate::eval::HandRank) -> u64 {
    if me > opp {
        2
    } else if me == opp {
        1
    } else {
        0
    }
}

/// Stack-allocated unused-card buffer (`[u8; 52]` + length) to avoid Vec churn
/// on the Monte Carlo hot path. Hot-loop iterators rebuild this once per iter
/// rather than allocating on the heap.
fn build_unused_array(used: &[bool; 52]) -> ([u8; 52], usize) {
    let mut unused: [u8; 52] = [0; 52];
    let mut len = 0;
    for v in 0..52u8 {
        if !used[v as usize] {
            unused[len] = v;
            len += 1;
        }
    }
    (unused, len)
}

fn sample_n_board_cards(used: &[bool; 52], n: usize, rng: &mut dyn RngSource) -> [Card; 5] {
    debug_assert!(n <= 5);
    let (mut unused, len) = build_unused_array(used);
    for i in 0..n {
        let j = i + (rng.next_u64() % ((len - i) as u64)) as usize;
        unused.swap(i, j);
    }
    let mut result = [Card::from_u8(0).expect("0 valid card"); 5];
    for i in 0..n {
        result[i] = Card::from_u8(unused[i]).expect("0..52");
    }
    result
}

fn sample_opp_and_board(
    used: &[bool; 52],
    current_board: &[Card],
    needed_board: usize,
    rng: &mut dyn RngSource,
) -> ([Card; 2], [Card; 5]) {
    let total_to_sample = 2 + needed_board;
    let (mut unused, len) = build_unused_array(used);
    for i in 0..total_to_sample {
        let j = i + (rng.next_u64() % ((len - i) as u64)) as usize;
        unused.swap(i, j);
    }
    let opp_hole = [
        Card::from_u8(unused[0]).expect("0..52"),
        Card::from_u8(unused[1]).expect("0..52"),
    ];
    let mut full_board = [Card::from_u8(0).expect("0 valid"); 5];
    for (i, c) in current_board.iter().enumerate() {
        full_board[i] = *c;
    }
    for (offset, slot) in (current_board.len()..5).enumerate() {
        full_board[slot] = Card::from_u8(unused[2 + offset]).expect("0..52");
    }
    (opp_hole, full_board)
}

fn pair_overlaps(a: &[Card; 2], b: &[Card; 2]) -> bool {
    a.iter().any(|x| b.iter().any(|y| x.to_u8() == y.to_u8()))
}

fn any_overlaps_board(pair: &[Card; 2], board: &[Card]) -> bool {
    pair.iter()
        .any(|p| board.iter().any(|b| p.to_u8() == b.to_u8()))
}

// ============================================================================
// OCHS opponent cluster table（D-222 / §C-rev2 §3）
// ============================================================================

/// Number of preflop hole equivalence classes（D-217 / preflop.rs `hand_class`
/// 0..169，13 pocket pairs + 78 suited + 78 offsuit = 169）。
const N_PREFLOP_CLASSES: usize = 169;

/// Hardcoded master seed for OCHS table precomputation。Stays fixed across
/// processes to give byte-equal cluster assignments — feature_set_id = 1 仍以
/// "EHS² + OCHS(N=8) = 9 维"为语义，不 bump schema_version（与 §C-rev1 §1
/// carve-out 同型）。
const OCHS_TRAINING_SEED: u64 = 0x0CC8_5EED_C2D2_22A0;

/// Per-class EHS Monte Carlo iter（issue #5 出口建议 ≥10k 让单类标准误差 < 0.005；
/// 169 × 10k × 2 评估 ≈ 3.4M evaluator calls @ ~50ns/call ≈ 170ms first-call latency
/// + module-level cache 命中后零成本）。
const OCHS_PRECOMPUTE_ITER: u32 = 10_000;

struct OchsTable {
    /// 每个 class_id 的 canonical 代表 hole（具体两张牌）。
    representative_hole: [[Card; 2]; N_PREFLOP_CLASSES],
    /// `classes_per_cluster[cluster_id]` = 该 cluster 中所有 class_id 的列表
    /// （cluster id ∈ 0..n_clusters，D-236b 重编号后：0 = weakest median EHS /
    /// n-1 = strongest）。
    classes_per_cluster: Vec<Vec<u8>>,
}

static OCHS_TABLE_CACHE: OnceLock<Mutex<HashMap<u32, Arc<OchsTable>>>> = OnceLock::new();

fn ochs_cache() -> &'static Mutex<HashMap<u32, Arc<OchsTable>>> {
    OCHS_TABLE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取 OCHS 表（首次调用按 `n_clusters` 训练并缓存；同一 `n_clusters` 后续 O(1)
/// 命中 cache）。
///
/// **byte-equal 保证**：OCHS 表只依赖 `OCHS_TRAINING_SEED`（hardcoded）和
/// `n_clusters`，与 evaluator impl 无关（NaiveHandEvaluator 是 stage 1 唯一
/// `HandEvaluator` impl，输出确定性）。同 (`OCHS_TRAINING_SEED`, `n_clusters`)
/// 跨进程跨架构 byte-equal（与 stage 1 D-051 同型）。
fn ochs_table(n_clusters: u32, evaluator: &Arc<dyn HandEvaluator>) -> Arc<OchsTable> {
    let mut cache = ochs_cache().lock().expect("OCHS cache mutex poisoned");
    if let Some(t) = cache.get(&n_clusters) {
        return Arc::clone(t);
    }
    let table = Arc::new(build_ochs_table(n_clusters, Arc::clone(evaluator)));
    cache.insert(n_clusters, Arc::clone(&table));
    table
}

/// 训练 OCHS 表（n_clusters 8 默认；其他 n 走同路径）。
///
/// 步骤：
/// 1. 169 class 各取一个 canonical 代表 hole（pocket pair = 同 rank 双花色 / suited
///    = 双 Spades / offsuit = Spades + Hearts，pair_combination_index 升序索引）。
/// 2. 对每个 class，用 D-228 OCHS_FEATURE_INNER + class_id 派生 sub-stream 跑
///    `OCHS_PRECOMPUTE_ITER` 轮 Monte Carlo，估算 EHS = E\[equity vs random opp + 5-card random board\]。
/// 3. K-means K=n_clusters on 169 个 1D EHS scalar。op_id_init = OCHS_WARMUP，
///    op_id_split 复用 OCHS_WARMUP（`split_empty_cluster` 不消费 RNG，详见
///    `cluster.rs::split_empty_cluster` 的 `_master_seed` / `_op_id_split` 标注，
///    复用同一 op_id 不引入实际冲突）。
/// 4. D-236b：按 EHS 中位数升序重编号 cluster id（0 = weakest / n-1 = strongest）。
fn build_ochs_table(n_clusters: u32, evaluator: Arc<dyn HandEvaluator>) -> OchsTable {
    let representative_hole: [[Card; 2]; N_PREFLOP_CLASSES] =
        std::array::from_fn(|i| representative_hole_for_class(i as u8));

    // Step 1: per-class EHS Monte Carlo。
    let mut ehs_per_class: [f64; N_PREFLOP_CLASSES] = [0.0; N_PREFLOP_CLASSES];
    for class_id in 0..N_PREFLOP_CLASSES {
        let rep = representative_hole[class_id];
        let used =
            build_used_set(&rep, &[], &[]).expect("representative hole has 2 distinct cards");
        let sub_seed =
            derive_substream_seed(OCHS_TRAINING_SEED, OCHS_FEATURE_INNER, class_id as u32);
        let mut rng = ChaCha20Rng::from_seed(sub_seed);
        let mut wins_x2: u64 = 0;
        for _ in 0..OCHS_PRECOMPUTE_ITER {
            let (opp_hole, full_board) = sample_opp_and_board(&used, &[], 5, &mut rng);
            let me_seven = build_seven_cards(rep, &full_board);
            let opp_seven = build_seven_cards(opp_hole, &full_board);
            wins_x2 += compare_x2(evaluator.eval7(&me_seven), evaluator.eval7(&opp_seven));
        }
        ehs_per_class[class_id] = wins_x2 as f64 / (2.0 * f64::from(OCHS_PRECOMPUTE_ITER));
    }

    // Step 2: K-means on 1-d EHS features。
    let features: Vec<Vec<f64>> = ehs_per_class.iter().map(|&x| vec![x]).collect();
    let cfg = KMeansConfig::default_d232(n_clusters);
    let kmeans_res = kmeans_fit(&features, cfg, OCHS_TRAINING_SEED, OCHS_WARMUP, OCHS_WARMUP);

    // Step 3: D-236b 重编号（EHS 中位数升序：cluster 0 = weakest）。
    let (_centroids, reordered_assignments) =
        reorder_by_ehs_median(kmeans_res.centroids, kmeans_res.assignments, &ehs_per_class);

    // Step 4: build classes_per_cluster (inverted index for runtime ochs lookup)。
    let mut classes_per_cluster: Vec<Vec<u8>> = vec![Vec::new(); n_clusters as usize];
    for (class_id, &cid) in reordered_assignments.iter().enumerate() {
        classes_per_cluster[cid as usize].push(class_id as u8);
    }

    OchsTable {
        representative_hole,
        classes_per_cluster,
    }
}

/// 给定 169 class id，返回该类的 canonical 代表 hole（具体两张牌）。
///
/// - `0..=12`：pocket pair（rank = class_id；Spades + Hearts）。
/// - `13..=90`：suited（pair_combination_index 升序；双 Spades）。
/// - `91..=168`：offsuit（同索引；Spades + Hearts）。
fn representative_hole_for_class(class: u8) -> [Card; 2] {
    match class {
        0..=12 => {
            let r = Rank::from_u8(class).expect("0..13 valid rank");
            [Card::new(r, Suit::Spades), Card::new(r, Suit::Hearts)]
        }
        13..=90 => {
            let idx = class - 13;
            let (high, low) = decode_high_low(idx);
            [
                Card::new(Rank::from_u8(high).expect("0..13 valid"), Suit::Spades),
                Card::new(Rank::from_u8(low).expect("0..13 valid"), Suit::Spades),
            ]
        }
        91..=168 => {
            let idx = class - 91;
            let (high, low) = decode_high_low(idx);
            [
                Card::new(Rank::from_u8(high).expect("0..13 valid"), Suit::Spades),
                Card::new(Rank::from_u8(low).expect("0..13 valid"), Suit::Hearts),
            ]
        }
        _ => panic!("representative_hole_for_class: class {class} >= 169"),
    }
}

/// 反解 `idx = high * (high - 1) / 2 + low`（low < high，high ∈ 1..13）。
fn decode_high_low(idx: u8) -> (u8, u8) {
    let mut high: u8 = 1;
    while high * (high + 1) / 2 <= idx {
        high += 1;
    }
    (high, idx - high * (high - 1) / 2)
}
