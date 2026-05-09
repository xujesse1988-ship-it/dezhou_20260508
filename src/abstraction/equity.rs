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
use crate::core::Card;
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
    #[allow(dead_code)] // A1 stub; B2 fills.
    iter: u32,
    #[allow(dead_code)]
    n_opp_clusters: u8,
    /// HandEvaluator 引用、OCHS opponent cluster 中心、缓存等（B2 填充）。
    #[allow(dead_code)]
    evaluator: Arc<dyn HandEvaluator>,
}

impl MonteCarloEquity {
    /// 默认配置：`iter = 10_000`、`n_opp_clusters = 8`（D-220 / D-222）。
    pub fn new(_evaluator: Arc<dyn HandEvaluator>) -> MonteCarloEquity {
        unimplemented!("A1 stub; B2 implements per D-220 / D-222")
    }

    /// 自定义 iter（CI 短测试可降到 1,000；clustering 训练必须用默认 10k）。
    pub fn with_iter(self, _iter: u32) -> MonteCarloEquity {
        unimplemented!("A1 stub; B2 implements")
    }

    /// 自定义 OCHS opponent cluster 数（stage 2 默认 8；stage 4 消融可调）。
    pub fn with_opp_clusters(self, _n: u8) -> MonteCarloEquity {
        unimplemented!("A1 stub; B2 implements")
    }

    pub fn iter(&self) -> u32 {
        unimplemented!("A1 stub; B2 implements")
    }

    pub fn n_opp_clusters(&self) -> u8 {
        unimplemented!("A1 stub; B2 implements")
    }
}

impl EquityCalculator for MonteCarloEquity {
    fn equity(
        &self,
        _hole: [Card; 2],
        _board: &[Card],
        _rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError> {
        unimplemented!("A1 stub; B2 implements per D-223 EHS")
    }

    fn equity_vs_hand(
        &self,
        _hole: [Card; 2],
        _opp_hole: [Card; 2],
        _board: &[Card],
        _rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError> {
        unimplemented!("A1 stub; B2 implements per D-220a-rev1 / EQ-001-rev1")
    }

    fn ehs_squared(
        &self,
        _hole: [Card; 2],
        _board: &[Card],
        _rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError> {
        unimplemented!(
            "A1 stub; B2 implements per D-227 EHS² rollout (river=0 / turn=46 / flop=1081)"
        )
    }

    fn ochs(
        &self,
        _hole: [Card; 2],
        _board: &[Card],
        _rng: &mut dyn RngSource,
    ) -> Result<Vec<f64>, EquityError> {
        unimplemented!("A1 stub; B2 implements per D-222 OCHS (N=8)")
    }
}
