//! 启发式 9-max 无限德州扑克决策引擎（与 CFR/blueprint 主线完全独立）。
//!
//! 设计与迭代闭环见 `docs/heuristic9_design.md`。公开入口 [`HeuristicBot`]：构造时建一次
//! equity 强度表，之后每个决策点 [`HeuristicBot::respond`] 吃一个 [`Request`]、出一个
//! [`Response`]（合法、永不 panic）。座位无关，2–9 人通用。

pub mod config;
pub mod equity;
pub mod features;
pub mod opponent;
pub mod policy;
pub mod view;

pub use config::HeuristicConfig;
pub use equity::EquityEngine;
pub use opponent::{ObservedAction, OppType, OpponentTable};
pub use policy::{decide, Decision, DecisionAction};
pub use view::{GameView, HistAction, Request, Response, ValidActions};

/// 常驻决策机：一份 config + 一张 equity 强度表 + 累积对手画像。
pub struct HeuristicBot {
    pub config: HeuristicConfig,
    pub equity: EquityEngine,
    pub opponents: OpponentTable,
}

impl HeuristicBot {
    /// 构造（建 equity 强度表，几毫秒一次性）。
    pub fn new(config: HeuristicConfig) -> HeuristicBot {
        let equity = EquityEngine::new(config.equity.preflop_table_samples, config.equity.seed);
        HeuristicBot {
            config,
            equity,
            opponents: OpponentTable::new(),
        }
    }

    /// 默认 config 构造。
    pub fn with_defaults() -> HeuristicBot {
        HeuristicBot::new(HeuristicConfig::default())
    }

    /// 决策（返回引擎内部 [`Decision`]）。
    pub fn decide_view(&self, view: &GameView) -> Decision {
        decide(view, &self.equity, &self.config, &self.opponents)
    }

    /// 从原始请求决策。
    pub fn decide_request(&self, req: &Request) -> Decision {
        let view = GameView::from_request(req);
        self.decide_view(&view)
    }

    /// 决策并映射成 IPC [`Response`]（合法、可直接回 driver）。
    pub fn respond(&self, req: &Request) -> Response {
        let view = GameView::from_request(req);
        let d = self.decide_view(&view);
        let street = Some(
            match view.street {
                crate::core::Street::Preflop => "preflop",
                crate::core::Street::Flop => "flop",
                crate::core::Street::Turn => "turn",
                crate::core::Street::River => "river",
                crate::core::Street::Showdown => "showdown",
            }
            .to_string(),
        );
        let probs = if d.probs.is_empty() {
            None
        } else {
            Some(d.probs.clone())
        };
        match d.action {
            DecisionAction::Fold => Response {
                action: "fold".into(),
                amount: None,
                source: d.source,
                street,
                equity: Some(round4(d.equity)),
                probs,
            },
            DecisionAction::Check => Response {
                action: "check".into(),
                amount: None,
                source: d.source,
                street,
                equity: Some(round4(d.equity)),
                probs,
            },
            DecisionAction::Call => Response {
                action: "call".into(),
                amount: None,
                source: d.source,
                street,
                equity: Some(round4(d.equity)),
                probs,
            },
            DecisionAction::Raise(to) => Response {
                action: "raise".into(),
                amount: Some(to),
                source: d.source,
                street,
                equity: Some(round4(d.equity)),
                probs,
            },
        }
    }

    /// 喂一手观测进对手画像（累积经验）。
    pub fn observe_hand(&mut self, actions: &[ObservedAction], names_in_hand: &[String]) {
        self.opponents.observe_hand(actions, names_in_hand);
    }
}

fn round4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}
