//! Phase A 决策遥测（`docs/scenario_heuristic_design.md` §6 Phase A，拍板点①）。
//!
//! 每个 [`Decision`](crate::heuristic::policy::Decision) 挂一份**只写** trace：本次实际走过的
//! 命名分支路径（稳定节点 id，§3.2）、从 [`GameView`] 现有字段读出的场景 key 维度（§1.1）、
//! 以及决策路径上**本来就算出**的驱动信号（equity / made / 阈值比较 / RNG 频率门掷点）。
//!
//! 硬约束（byte-equal 一票否决）：trace 只写不读——决策路径、equity 采样次数、RNG 消费序列
//! 必须逐字节不变。禁止为 trace 多算一次 equity、多抽一个随机数；这里记录的全部是既有中间值
//! 或对 `GameView` 的纯读取。

use crate::core::Street;
use crate::heuristic::view::{GameView, PosBucket};
use serde::{Deserialize, Serialize};

fn round4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

fn round2(x: f64) -> f64 {
    (x * 1e2).round() / 1e2
}

/// One threshold comparison actually evaluated on the taken decision path.
/// `pass` mirrors the exact condition the policy tested (direction varies per gate).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraceCmp {
    /// Stable gate name (e.g. `postflop.value_thr`).
    pub what: String,
    /// Signal side of the comparison (equity / strength / rng roll / spr).
    pub lhs: f64,
    /// Threshold side of the comparison.
    pub rhs: f64,
    /// Whether the policy condition held.
    pub pass: bool,
}

/// Scenario-key dimensions (design §1.1). Every field is a pure read of existing
/// [`GameView`] state — building this struct never touches RNG or equity sampling.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScenarioKey {
    /// `pre` / `flop` / `turn` / `river`.
    pub street: String,
    /// Pot type bucket from `preflop_num_raises`: `limped` (0) / `srp` (1) / `3bp` (2) / `4bp+` (>=3).
    pub pot_type: String,
    /// Hero role: `agg` (hero is preflop aggressor) / `blind_def` (raised pot, hero in
    /// blinds, not aggressor) / `caller` (raised pot, non-blind, not aggressor) /
    /// `none` (unraised pot, hero not aggressor).
    pub role: String,
    /// Facing bucket. Preflop mirrors the branch condition (`unopened` / `open` / `3bet`
    /// / `4bet+` from `preflop_num_raises`); postflop mirrors `facing_bet`
    /// (`unopened` / `bet`, valid-authoritative like `postflop_decide`).
    pub facing: String,
    /// Players still in the hand (hero included) — `num_active()`.
    pub num_active: u32,
    /// SPR bucket: `low` (<3) / `mid` / `deep` (>8).
    pub spr_bucket: String,
    /// Raw SPR (rounded to 2 decimals, log compactness only).
    pub spr: f64,
    /// Preflop position bucket: `early` / `mid` / `late` / `sb` / `bb`.
    pub pos: String,
    /// Postflop in-position flag (relative to remaining opponents).
    pub ip: bool,
    /// P0-1 derived line feature: consecutive-street barrel count hero is facing
    /// ([`GameView::facing_barrel_n`]).
    pub barrel_n: u8,
    /// P0-1 derived line feature: hero faces a delayed c-bet ([`GameView::is_delayed_cbet`]).
    pub delayed_cbet: bool,
    /// P0-1 derived line feature: every remaining opponent shows a capped (passive-only
    /// postflop) line ([`GameView::villains_capped`]).
    pub villains_capped: bool,
}

impl ScenarioKey {
    /// Build the scenario key by pure reads of the [`GameView`] (no RNG / no equity).
    pub fn from_view(view: &GameView) -> ScenarioKey {
        let street = match view.street {
            Street::Preflop => "pre",
            Street::Flop => "flop",
            Street::Turn => "turn",
            _ => "river",
        };
        let pot_type = match view.preflop_num_raises {
            0 => "limped",
            1 => "srp",
            2 => "3bp",
            _ => "4bp+",
        };
        let pos_bucket = view.my_pos_bucket();
        let role = if view.preflop_aggressor == Some(view.my_seat) {
            "agg"
        } else if view.preflop_raised {
            match pos_bucket {
                PosBucket::SmallBlind | PosBucket::BigBlind => "blind_def",
                _ => "caller",
            }
        } else {
            "none"
        };
        let facing = if view.street == Street::Preflop {
            match view.preflop_num_raises {
                0 => "unopened",
                1 => "open",
                2 => "3bet",
                _ => "4bet+",
            }
        } else if view.to_call_chips() > 0 || !view.valid.can_check {
            // valid-authoritative, mirrors `facing_bet` in `postflop_decide`.
            "bet"
        } else {
            "unopened"
        };
        let spr = view.spr();
        let spr_bucket = if spr < 3.0 {
            "low"
        } else if spr > 8.0 {
            "deep"
        } else {
            "mid"
        };
        let pos = match pos_bucket {
            PosBucket::Early => "early",
            PosBucket::Mid => "mid",
            PosBucket::Late => "late",
            PosBucket::SmallBlind => "sb",
            PosBucket::BigBlind => "bb",
        };
        ScenarioKey {
            street: street.into(),
            pot_type: pot_type.into(),
            role: role.into(),
            facing: facing.into(),
            num_active: view.num_active(),
            spr_bucket: spr_bucket.into(),
            spr: round2(spr),
            pos: pos.into(),
            ip: view.in_position(),
            barrel_n: view.facing_barrel_n(),
            delayed_cbet: view.is_delayed_cbet(),
            villains_capped: view.villains_capped(),
        }
    }
}

/// legalize / downgrade record: set iff the legalize floor changed the policy-chosen
/// action or raise size（设计 §8④ 兜底率指标的输入之一）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LegalizeChange {
    pub before_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_amount: Option<u64>,
    pub after_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_amount: Option<u64>,
}

/// Phase A per-decision telemetry. Attached to every `Decision`, forwarded verbatim by
/// the advisor `Response` into the action log. Write-only by construction.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DecisionTrace {
    /// Full node path root→leaf（§3.2 规则 3：记完整路径，不只叶 id），e.g.
    /// `["pre", "pre.facing_raise", "pre.facing_raise.odds_call"]`.
    pub path: Vec<String>,
    /// Scenario-key dimensions (design §1.1).
    pub scenario: ScenarioKey,
    /// Threshold comparisons evaluated along the taken path, in evaluation order.
    pub cmps: Vec<TraceCmp>,
    /// vs-range equity when it was computed on this path (always postflop; preflop only
    /// on the odds-call branch). `None` = not computed — never compute extra for trace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equity: Option<f64>,
    /// Preflop 169-strength (equity-vs-random scale) when computed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strength: Option<f64>,
    /// Postflop made-hand class (Debug name) when computed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub made: Option<String>,
    /// Postflop meaningful-draw flag when computed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draw: Option<bool>,
    /// Policy output entering the legalize floor: action name + raise-to (if raise).
    pub policy_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_amount: Option<u64>,
    /// Set iff the legalize floor changed the policy action/size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legalized: Option<LegalizeChange>,
}

impl DecisionTrace {
    /// Fresh trace with the scenario key filled from the view (pure reads only).
    pub fn new(view: &GameView) -> DecisionTrace {
        DecisionTrace {
            scenario: ScenarioKey::from_view(view),
            ..Default::default()
        }
    }

    /// Set the leaf node id and expand the full prefix path (`a.b.c` → `[a, a.b, a.b.c]`).
    pub fn set_leaf(&mut self, leaf: &str) {
        self.path.clear();
        let mut acc = String::new();
        for part in leaf.split('.') {
            if !acc.is_empty() {
                acc.push('.');
            }
            acc.push_str(part);
            self.path.push(acc.clone());
        }
    }

    /// Leaf node id (last path element), empty string if unset.
    pub fn leaf(&self) -> &str {
        self.path.last().map(|s| s.as_str()).unwrap_or("")
    }

    /// Record one threshold comparison actually evaluated by the policy.
    pub fn cmp(&mut self, what: &str, lhs: f64, rhs: f64, pass: bool) {
        self.cmps.push(TraceCmp {
            what: what.into(),
            lhs: round4(lhs),
            rhs: round4(rhs),
            pass,
        });
    }

    /// Note the vs-range equity that the policy already computed.
    pub fn note_equity(&mut self, eq: f64) {
        self.equity = Some(round4(eq));
    }

    /// Note the preflop 169-strength that the policy already computed.
    pub fn note_strength(&mut self, s: f64) {
        self.strength = Some(round4(s));
    }
}
