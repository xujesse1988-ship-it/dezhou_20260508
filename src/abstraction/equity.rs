//! Equity calculator（API §3）。
//!
//! `EquityCalculator` trait + `EquityError` enum + `MonteCarloEquity`
//! （D-220 / D-220a-rev1 / D-221 / D-222 / D-223 / D-224-rev1 / D-227 / D-228）。
//!
//! **离线 clustering 训练路径专用**——运行时映射禁止触发（D-225）。`f64`
//! 出现在本模块是显式允许的——本路径在 `abstraction::equity` /
//! `abstraction::cluster` 子模块，与 `abstraction::map` 子模块（禁浮点，D-252）
//! 物理隔离。

use std::sync::Arc;

use thiserror::Error;

use crate::core::rng::RngSource;
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
        let opp_reps = ochs_opp_representatives();
        let n = self.n_opp_clusters as usize;
        let mut out = Vec::with_capacity(n);
        for k in 0..n {
            let opp = opp_reps[k % opp_reps.len()];
            // Skip representatives that clash with hole or board: fall back to 0.5
            // (B2 stub; C2 trains true 169-class clustering with disjoint reps).
            if pair_overlaps(&hole, &opp) || any_overlaps_board(&opp, board) {
                out.push(0.5);
                continue;
            }
            let v = self.equity_vs_hand(hole, opp, board, rng)?;
            out.push(v);
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

/// 8 个 OCHS opponent class 代表 hole（B2 stub；C2 用 1D EHS k-means 训练真实
/// 169-class → 8-cluster 后取 strongest representative）。
///
/// 选用 8 个具有代表性的 hole，按 EHS 强度递减大致排列：
/// AsAh / KsKh / QsQh / TsTh / 8h8d / 5h5d / 7s2d / 7s2h
/// （前 6 类是 pocket pair 强度递减，后 2 类是经典最弱 offsuit）。同 hole 出现
/// 在多个 cluster 时 `MonteCarloEquity::ochs` 自动 fallback 到 0.5（避免
/// `OverlapHole` 错误）。完整实现留 C2 \[实现\]。
fn ochs_opp_representatives() -> [[Card; 2]; 8] {
    [
        [
            Card::new(Rank::Ace, Suit::Spades),
            Card::new(Rank::Ace, Suit::Hearts),
        ],
        [
            Card::new(Rank::King, Suit::Spades),
            Card::new(Rank::King, Suit::Hearts),
        ],
        [
            Card::new(Rank::Queen, Suit::Spades),
            Card::new(Rank::Queen, Suit::Hearts),
        ],
        [
            Card::new(Rank::Ten, Suit::Spades),
            Card::new(Rank::Ten, Suit::Hearts),
        ],
        [
            Card::new(Rank::Eight, Suit::Hearts),
            Card::new(Rank::Eight, Suit::Diamonds),
        ],
        [
            Card::new(Rank::Five, Suit::Hearts),
            Card::new(Rank::Five, Suit::Diamonds),
        ],
        [
            Card::new(Rank::Seven, Suit::Spades),
            Card::new(Rank::Two, Suit::Diamonds),
        ],
        [
            Card::new(Rank::Seven, Suit::Spades),
            Card::new(Rank::Two, Suit::Hearts),
        ],
    ]
}

fn pair_overlaps(a: &[Card; 2], b: &[Card; 2]) -> bool {
    a.iter().any(|x| b.iter().any(|y| x.to_u8() == y.to_u8()))
}

fn any_overlaps_board(pair: &[Card; 2], board: &[Card]) -> bool {
    pair.iter()
        .any(|p| board.iter().any(|b| p.to_u8() == b.to_u8()))
}
