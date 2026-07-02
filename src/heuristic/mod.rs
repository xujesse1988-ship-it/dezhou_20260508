//! 启发式 9-max 无限德州扑克决策引擎（与 CFR/blueprint 主线完全独立）。
//!
//! 设计与迭代闭环见 `docs/heuristic9_design.md`。公开入口 [`HeuristicBot`]：构造时建一次
//! equity 强度表，之后每个决策点 [`HeuristicBot::respond`] 吃一个 [`Request`]、出一个
//! [`Response`]（合法、永不 panic）。座位无关，2–9 人通用。

pub mod config;
pub mod equity;
pub mod features;
pub mod mlrange;
pub mod opponent;
pub mod policy;
pub mod range;
pub mod range_online;
pub mod tokenizer;
pub mod view;

pub use config::HeuristicConfig;
pub use equity::EquityEngine;
pub use opponent::{ObservedAction, OppType, OpponentTable};
pub use policy::{decide, Decision, DecisionAction};
pub use range_online::MlRange;
pub use view::{GameView, HistAction, Request, Response, ValidActions};

/// 常驻决策机：一份 config + 一张 equity 强度表 + 累积对手画像
/// （+ 可选在线 ML range 预测器，`use_ml_range` 开且产物存在时载入）。
pub struct HeuristicBot {
    pub config: HeuristicConfig,
    pub equity: EquityEngine,
    pub opponents: OpponentTable,
    /// 在线 ML range 预测器；`None` = 关闭或产物缺失（回落标量 equity 路径）。
    pub ml_range: Option<MlRange>,
}

impl HeuristicBot {
    /// 构造（建 equity 强度表，几毫秒一次性）。`use_ml_range` 开时另载 ML range 模型
    /// （产物缺失 → 日志 + 回落标量路径）。
    pub fn new(config: HeuristicConfig) -> HeuristicBot {
        let equity = EquityEngine::new(config.equity.preflop_table_samples, config.equity.seed);
        let ml_range = if config.range.use_ml_range {
            match MlRange::load(
                &config.range.ml_strength_path,
                &config.range.ml_weights_path,
                config.range.ml_rscale,
            ) {
                Some(m) => Some(m),
                None => {
                    eprintln!(
                        "[heuristic] use_ml_range=true 但 ML 产物载入失败（strength={} weights={}）\
                         → 回落标量 equity",
                        config.range.ml_strength_path, config.range.ml_weights_path
                    );
                    None
                }
            }
        } else {
            None
        };
        HeuristicBot {
            config,
            equity,
            opponents: OpponentTable::new(),
            ml_range,
        }
    }

    /// 默认 config 构造。
    pub fn with_defaults() -> HeuristicBot {
        HeuristicBot::new(HeuristicConfig::default())
    }

    /// 决策（返回引擎内部 [`Decision`]）。
    pub fn decide_view(&self, view: &GameView) -> Decision {
        decide(
            view,
            &self.equity,
            &self.config,
            &self.opponents,
            self.ml_range.as_ref(),
        )
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
        // 纯日志度量：读已重建好的 view 状态，绝不回馈决策（analyzer 用真实价位判过松/过紧）。
        let pot = view.pot_chips();
        let to_call = view.to_call_chips();
        let required_eq = view.pot_odds();
        let opps_in_pot = view.num_opponents();
        let base = Response {
            source: d.source,
            street,
            equity: Some(round4(d.equity)),
            probs,
            pot: Some(pot),
            to_call: Some(to_call),
            required_eq: Some(round4(required_eq)),
            opps_in_pot: Some(opps_in_pot),
            ..Default::default()
        };
        match d.action {
            DecisionAction::Fold => Response {
                action: "fold".into(),
                ..base
            },
            DecisionAction::Check => Response {
                action: "check".into(),
                ..base
            },
            DecisionAction::Call => Response {
                action: "call".into(),
                ..base
            },
            DecisionAction::Raise(to) => Response {
                action: "raise".into(),
                amount: Some(to),
                ..base
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
