//! 决策编排（Phase B 场景森林）：显式节点分派 → 节点内响应 → 尺寸映射 → 合法化。
//!
//! 入口 [`decide`]。每个决策点先由 [`SpotNode::classify`]（对 [`GameView`] 的纯读取）恰好
//! 命中一个场景节点（设计 §1：场景=上下文，手牌不进 key），再经 `match` 分派到该节点的
//! 响应函数（enum + match，编译器保证完备性，设计 §8⑤）；trace 的节点路径由分派结构直接
//! 产出（「分派即路径」），不再事后标注。节点参数在 [`HeuristicConfig::nodes`]，跨节点
//! 共享地板（[`legalize`] / [`safe_fallback`] / [`situation_seed`] 确定性 RNG / 对手 range
//! 建模）对所有节点不变（设计 §3.4）。
//!
//! 设计文档：`docs/scenario_heuristic_design.md`（§3 森林与 id 规范、§6 Phase B）。

use crate::core::Street;
use crate::heuristic::config::HeuristicConfig;
use crate::heuristic::equity::{EquityEngine, Rng};
use crate::heuristic::features::{draws, made_class, texture, MadeClass};
use crate::heuristic::opponent::{
    facing_node_key, facing_raise_node_key, pot_type_name, ExploitAdjust, OpponentTable,
};
use crate::heuristic::range::RangeVec;
use crate::heuristic::range_online::MlRange;
use crate::heuristic::trace::{DecisionTrace, LegalizeChange};
use crate::heuristic::view::{GameView, PosBucket};

/// 决策结果。`Raise(to_chips)` 的 to 是 OpenPoker 单位的 raise-to 绝对额。
#[derive(Clone, Debug, PartialEq)]
pub enum DecisionAction {
    Fold,
    Check,
    Call,
    Raise(u64),
}

#[derive(Clone, Debug)]
pub struct Decision {
    pub action: DecisionAction,
    pub equity: f64,
    pub source: String,
    pub probs: Vec<(String, f64)>,
    /// 节点遥测（`docs/scenario_heuristic_design.md` §6）：节点路径 + 场景 key + 驱动信号。
    /// 只写不读——决策路径 / equity 采样 / RNG 消费不因 trace 改变。
    pub trace: Option<DecisionTrace>,
}

// ===========================================================================
// 场景森林：上下文节点（§1 场景=上下文，手牌不进 key；§8⑤ enum + match）
// ===========================================================================

/// 池型桶（§1.1 场景 key 维度）：由 `preflop_num_raises` 折出，翻后保留。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PotType {
    Limped,
    SingleRaised,
    ThreeBet,
    FourBetPlus,
}

impl PotType {
    /// 纯读取分类（无 RNG / 无 equity）。
    pub fn classify(view: &GameView) -> PotType {
        match view.preflop_num_raises {
            0 => PotType::Limped,
            1 => PotType::SingleRaised,
            2 => PotType::ThreeBet,
            _ => PotType::FourBetPlus,
        }
    }

    /// 已按池型拆分的叶节点 qualifier（§3.2 规则 2：Phase A 叶 id + qualifier 长出
    /// 子层级）。`None` = limped / 4bet+ 样本稀暂不拆，落 Phase A 父 id 兜底
    /// （父节点退化成「其余情况」，id 不变）。
    fn split_qualifier(self) -> Option<&'static str> {
        match self {
            PotType::SingleRaised => Some("srp"),
            PotType::ThreeBet => Some("3bp"),
            _ => None,
        }
    }
}

/// 翻前面对的加注级别（分派维度，同时选 [`NodesConfig`](crate::heuristic::config::NodesConfig)
/// 里对应级别的节点参数）。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RaiseLevel {
    Open,
    ThreeBet,
    FourBetPlus,
}

impl RaiseLevel {
    /// 叶节点 qualifier（§3.2 规则 2）。
    fn qualifier(self) -> &'static str {
        match self {
            RaiseLevel::Open => "vs_open",
            RaiseLevel::ThreeBet => "vs_3bet",
            RaiseLevel::FourBetPlus => "vs_4bet",
        }
    }
}

/// 场景森林的上下文节点。分类互斥完备：每个决策点**恰好命中一个**节点（遥测归因
/// 成立的前提，§3.1）；`match` 分派由编译器保证完备（§8⑤）。
///
/// `pre.short`（短栈推/弃）是覆盖层而非独立上下文节点：短栈达标时先评估 shove gate
/// （叶 `pre.short.shove`），不达标落回所在翻前节点的常规响应（与 Phase A 语义一致）。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SpotNode {
    /// 翻前，无人加注（含 limp 池）→ `pre.unopened.*`。
    PreUnopened,
    /// 翻前，面对加注（按级别分参/分叶）→ `pre.facing_raise.*[.vs_*]`。
    PreFacingRaise(RaiseLevel),
    /// 翻后，本街无人下注 → `post.unopened.*[.srp|.3bp]`。
    PostUnopened(PotType),
    /// 翻后，面对下注/加注（valid 权威：不能 check 即视为面对下注）→ `post.facing.*`。
    PostFacingBet(PotType),
}

impl SpotNode {
    /// 场景分类：对 [`GameView`] 的纯读取，总函数（任何输入恰好一个节点）。
    pub fn classify(view: &GameView) -> SpotNode {
        if view.street == Street::Preflop {
            match view.preflop_num_raises {
                0 => SpotNode::PreUnopened,
                1 => SpotNode::PreFacingRaise(RaiseLevel::Open),
                2 => SpotNode::PreFacingRaise(RaiseLevel::ThreeBet),
                _ => SpotNode::PreFacingRaise(RaiseLevel::FourBetPlus),
            }
        } else {
            let pot = PotType::classify(view);
            // valid 权威：不能 check = 面对下注（即便重建 to_call 因不一致输入算成 0）。
            if view.to_call_chips() > 0 || !view.valid.can_check {
                SpotNode::PostFacingBet(pot)
            } else {
                SpotNode::PostUnopened(pot)
            }
        }
    }
}

/// 叶 id 组装（§3.2 规则 2）：Phase A 稳定叶 id + 分派 qualifier 长出子层级；
/// `None` = 父 id 兜底（父节点 id 永不改义）。
fn leaf_id(parent: &'static str, qualifier: Option<&'static str>) -> String {
    match qualifier {
        Some(q) => format!("{parent}.{q}"),
        None => parent.to_string(),
    }
}

/// Stable action label (trace bookkeeping).
fn action_label(a: &DecisionAction) -> &'static str {
    match a {
        DecisionAction::Fold => "fold",
        DecisionAction::Check => "check",
        DecisionAction::Call => "call",
        DecisionAction::Raise(_) => "raise",
    }
}

/// raise-to amount if the action is a raise (trace bookkeeping).
fn raise_amount(a: &DecisionAction) -> Option<u64> {
    match a {
        DecisionAction::Raise(to) => Some(*to),
        _ => None,
    }
}

/// Attach the node trace to a finished policy decision: set the leaf node id (full
/// prefix path per design §3.2) and snapshot the policy-chosen action/size before the
/// legalize floor. Pure bookkeeping — never alters the decision.
fn traced(mut d: Decision, mut tr: DecisionTrace, leaf: &str) -> Decision {
    tr.set_leaf(leaf);
    tr.policy_action = action_label(&d.action).to_string();
    tr.policy_amount = raise_amount(&d.action);
    d.trace = Some(tr);
    d
}

/// 决策 RNG seed：手牌状态混合（确定性、可复现）。
fn situation_seed(view: &GameView, cfg: &HeuristicConfig) -> u64 {
    let mut h = cfg.mixing.seed ^ 0x1000_0193;
    let mix = |x: u64, h: &mut u64| {
        *h ^= x
            .wrapping_add(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(*h << 6)
            .wrapping_add(*h >> 2);
    };
    if let Some(hole) = view.hole {
        mix(hole[0].to_u8() as u64, &mut h);
        mix((hole[1].to_u8() as u64) << 8, &mut h);
    }
    for (i, c) in view.board.iter().enumerate() {
        mix((c.to_u8() as u64) << (i * 6 % 40), &mut h);
    }
    mix(view.high_bet, &mut h);
    mix(view.pot_chips(), &mut h);
    mix(view.street as u64, &mut h);
    h
}

/// 为每个仍在场对手估其 range 最低强度阈值（强度 = equity-vs-random 标度）。
pub fn build_opp_thresholds(
    view: &GameView,
    eq: &EquityEngine,
    cfg: &HeuristicConfig,
    opp: &OpponentTable,
) -> Vec<f64> {
    let mut out = Vec::new();
    let n = view.num_seats as usize;
    // 街越晚，range 越紧（人对大注/晚街更诚实）：flop 0.03 / turn 0.06 / river 0.09。
    let street_idx = match view.street {
        Street::Preflop => 0.0,
        Street::Flop => 1.0,
        Street::Turn => 2.0,
        _ => 3.0,
    };
    let postflop_tighten = 0.03 * street_idx;
    // 当前街下注尺寸 / 底池 → 对**本街下注者**额外收紧（面对大注假设更强 range）。
    // 无需对手数据、普遍正确：弱场多数玩家下大注 = 价值，少诈。
    let pot = view.pot_chips().max(1) as f64;
    let bet_frac = (view.high_bet as f64 / pot).clamp(0.0, 1.5);
    let aggressor_size_tighten = if view.street != Street::Preflop && view.high_bet > 0 {
        0.12 * bet_frac
    } else {
        0.0
    };
    for s in 0..n {
        if s == view.my_seat as usize || view.folded[s] {
            continue;
        }
        // base：是否激进者。
        let is_pre_aggr = view.preflop_aggressor == Some(s as u8);
        let is_street_aggr = view.street_aggressor == Some(s as u8);
        let committed_bb = view.committed_total[s] as f64 / view.big_blind as f64;
        let frac = if is_pre_aggr || is_street_aggr {
            if view.preflop_num_raises >= 2 {
                0.08 // 3bet+ 激进者
            } else {
                0.17 // 开池/单加注者
            }
        } else if committed_bb >= 2.5 {
            0.34 // 冷跟一个加注
        } else if committed_bb >= 1.5 {
            0.55 // limp/补盲
        } else {
            0.80 // 几乎随机（盲位 check option）
        };
        let mut thr = eq.threshold_for_top(frac) + postflop_tighten;
        // 本街下注者额外按注大小收紧。
        if is_street_aggr {
            thr += aggressor_size_tighten;
        }
        // 对手画像偏移。
        if let Some(name) = view.names.get(&(s as u8)) {
            thr += opp.range_threshold_shift(name, &cfg.exploit);
        }
        out.push(thr.clamp(0.0, 0.96));
    }
    out
}

/// 为每个仍在场对手用在线 ML 流水线建 169 类 [`RangeVec`]，喂
/// [`EquityEngine::equity_weighted`]。**座位序与 [`build_opp_thresholds`] 完全一致**
/// （同样跳过 `my_seat` 与已弃座、同样 `0..num_seats` 升序），保证加权 equity 的对手
/// 顺序与标量路径逐一对应。
pub fn build_opp_ranges_ml(view: &GameView, ml: &MlRange) -> Vec<RangeVec> {
    let mut out = Vec::new();
    let n = view.num_seats as usize;
    for s in 0..n {
        if s == view.my_seat as usize || view.folded[s] {
            continue;
        }
        out.push(ml.predict(view, s));
    }
    out
}

/// 主 equity 货币的分发：`Some(ml)`（`use_ml_range` 开且已载入）→ ML range 加权 equity；
/// `None` → 原标量阈值 equity。两侧 `samples`/`seed` 相同；对手顺序在
/// [`build_opp_ranges_ml`] 与 [`build_opp_thresholds`] 间保持一致。
fn equity_dispatch(
    view: &GameView,
    eq: &EquityEngine,
    ml: Option<&MlRange>,
    opp_thr: &[f64],
    hole: [crate::core::Card; 2],
    samples: u32,
    seed: u64,
) -> f64 {
    match ml {
        Some(m) => {
            let ranges = build_opp_ranges_ml(view, m);
            eq.equity_weighted(hole, &view.board, &ranges, samples, seed)
        }
        None => eq.equity(hole, &view.board, opp_thr, samples, seed),
    }
}

/// 聚合活跃对手的剥削调整。
fn exploit_adjust(view: &GameView, cfg: &HeuristicConfig, opp: &OpponentTable) -> ExploitAdjust {
    let mut names = Vec::new();
    for s in 0..view.num_seats as usize {
        if s == view.my_seat as usize || view.folded[s] {
            continue;
        }
        if let Some(nm) = view.names.get(&(s as u8)) {
            names.push(nm.clone());
        }
    }
    opp.aggregate_exploit(&names, &cfg.exploit)
}

/// 节点响应的共享输入：信息层信号源 + 全局地板句柄。一次组装，借给所有节点响应。
struct NodeCtx<'a> {
    view: &'a GameView,
    eq: &'a EquityEngine,
    cfg: &'a HeuristicConfig,
    adj: &'a ExploitAdjust,
    /// P0-3 阶段2：per-node villain fold 读数源（float 门节点级查询用；`adj` 只有聚合读数）。
    opp: &'a OpponentTable,
    opp_thr: &'a [f64],
    ml: Option<&'a MlRange>,
    hole: [crate::core::Card; 2],
}

// ===========================================================================
// 入口
// ===========================================================================

pub fn decide(
    view: &GameView,
    eq: &EquityEngine,
    cfg: &HeuristicConfig,
    opp: &OpponentTable,
    ml: Option<&MlRange>,
) -> Decision {
    let hole = match view.hole {
        Some(h) => h,
        None => {
            // 无手牌信息：安全动作。
            return safe_fallback(view, "no_hole");
        }
    };
    let mut rng = Rng::new(situation_seed(view, cfg));
    let adj = exploit_adjust(view, cfg, opp);
    let opp_thr = build_opp_thresholds(view, eq, cfg, opp);
    let tr = DecisionTrace::new(view);
    let ctx = NodeCtx {
        view,
        eq,
        cfg,
        adj: &adj,
        opp,
        opp_thr: &opp_thr,
        ml,
        hole,
    };

    // 场景森林分派（分派即路径）：恰好一个节点，match 完备性由编译器保证。
    let d = match SpotNode::classify(view) {
        SpotNode::PreUnopened => pre_unopened(&ctx, &mut rng, tr),
        SpotNode::PreFacingRaise(lvl) => pre_facing_raise(&ctx, lvl, &mut rng, tr),
        SpotNode::PostUnopened(pot) => post_unopened(&ctx, pot, &mut rng, tr),
        SpotNode::PostFacingBet(pot) => post_facing_bet(&ctx, pot, &mut rng, tr),
    };
    // legalize floor; if it changed the policy action/size, record before→after in the
    // trace (fallback-rate input, design §8④).
    let before = d.action.clone();
    let mut d = legalize(d, view);
    if d.action != before {
        if let Some(t) = d.trace.as_mut() {
            t.legalized = Some(LegalizeChange {
                before_action: action_label(&before).into(),
                before_amount: raise_amount(&before),
                after_action: action_label(&d.action).into(),
                after_amount: raise_amount(&d.action),
            });
        }
    }
    d
}

/// 最终合法性网：保证输出动作严格落在 `valid` 内（兜底，正常路径已合法）。
/// 仅在「重建态与 valid 不一致」的边角输入触发。
fn legalize(mut d: Decision, view: &GameView) -> Decision {
    let v = &view.valid;
    match d.action {
        DecisionAction::Fold => {}
        DecisionAction::Check if !v.can_check => {
            // 不能 check：保守退化（不能 call 就 fold，避免 air 乱跟）。
            d.action = if v.can_call {
                DecisionAction::Call
            } else {
                DecisionAction::Fold
            };
            d.source.push_str("|legalize_nocheck");
        }
        DecisionAction::Call if !v.can_call => {
            d.action = if v.can_check {
                DecisionAction::Check
            } else {
                DecisionAction::Fold
            };
            d.source.push_str("|legalize_nocall");
        }
        DecisionAction::Raise(to) if !v.can_raise => {
            d.action = if v.can_call {
                DecisionAction::Call
            } else if v.can_check {
                DecisionAction::Check
            } else {
                DecisionAction::Fold
            };
            d.source.push_str("|legalize_noraise");
            let _ = to;
        }
        DecisionAction::Raise(to) => {
            let mn = v.min_raise.unwrap_or(to);
            let mx = v.max_raise.unwrap_or(to);
            d.action = DecisionAction::Raise(to.clamp(mn.min(mx), mx));
        }
        _ => {}
    }
    d
}

/// 异常兜底：能 check 就 check，否则 fold。trace 落 `fallback.<why>` 节点（设计 §8④）。
fn safe_fallback(view: &GameView, why: &str) -> Decision {
    let action = if view.valid.can_check {
        DecisionAction::Check
    } else {
        DecisionAction::Fold
    };
    let mut tr = DecisionTrace::new(view);
    tr.set_leaf(&format!("fallback.{why}"));
    tr.policy_action = action_label(&action).to_string();
    Decision {
        action,
        equity: 0.0,
        source: format!("fallback:{why}"),
        probs: vec![],
        trace: Some(tr),
    }
}

// ===========================================================================
// 翻前节点响应
// ===========================================================================

/// 已入池对手数（多人收紧的分母；投入超过半个 BB 才算入池）。
fn preflop_entrants(view: &GameView) -> u32 {
    view.committed_total
        .iter()
        .enumerate()
        .filter(|(s, &c)| *s != view.my_seat as usize && !view.folded[*s] && c > view.big_blind / 2)
        .count() as u32
}

/// `pre.short` 推/弃覆盖层 gate：短栈（有效栈 < `nodes.short.max_eff_bb`）且可加注时
/// 评估 shove 阈值（评估过的比较记入 trace）。不达标返回 `false`，落回所在翻前节点的
/// 常规响应（短栈弱牌仍可 open / odds-call / fold）。
fn pre_short_shove_gate(
    ctx: &NodeCtx,
    strength: f64,
    entrants: u32,
    tr: &mut DecisionTrace,
) -> bool {
    let ns = &ctx.cfg.nodes.short;
    let eff_bb = ctx.view.effective_stack_chips() as f64 / ctx.view.big_blind as f64;
    if eff_bb >= ns.max_eff_bb || !ctx.view.valid.can_raise {
        return false;
    }
    let shove_thr = ns.shove_strength + 0.02 * entrants as f64 - ctx.adj.steal_bonus;
    tr.cmp(
        "preflop.shove_thr",
        strength,
        shove_thr,
        strength >= shove_thr,
    );
    strength >= shove_thr
}

/// `pre.unopened`：翻前无人加注（first-in / 面对 limper）。
/// 叶：`pre.short.shove` / `pre.unopened.{open|limp_behind|check_option|fold}`。
fn pre_unopened(ctx: &NodeCtx, _rng: &mut Rng, mut tr: DecisionTrace) -> Decision {
    let view = ctx.view;
    let nc = &ctx.cfg.nodes;
    let strength = ctx.eq.hand_strength(ctx.hole[0], ctx.hole[1]);
    tr.note_strength(strength);
    let pos = view.my_pos_bucket();
    let to_call_bb = view.to_call_bb();
    let entrants = preflop_entrants(view);
    let mut probs = vec![];

    // --- pre.short 覆盖层：短栈推/弃 ---
    if pre_short_shove_gate(ctx, strength, entrants, &mut tr) {
        return traced(
            raise_decision(view, view.valid.max_raise, strength, "preflop:shove", probs),
            tr,
            "pre.short.shove",
        );
    }

    // --- 开池 / iso ---
    let open_thr_base = match pos {
        PosBucket::Early => nc.open.early,
        PosBucket::Mid => nc.open.mid,
        PosBucket::Late => nc.open.late,
        PosBucket::SmallBlind => nc.open.sb,
        PosBucket::BigBlind => nc.open.mid, // BB 可加注的 limp 底池
    };
    let mult_tighten = ctx.cfg.preflop.multiway_tighten * entrants as f64;
    let open_thr = (open_thr_base + mult_tighten - ctx.adj.steal_bonus).clamp(0.0, 0.99);
    tr.cmp("preflop.open_thr", strength, open_thr, strength >= open_thr);
    if strength >= open_thr && view.valid.can_raise {
        let size_bb = ctx.cfg.preflop.open_size_bb
            + ctx.cfg.preflop.iso_extra_per_limper_bb * view.preflop_limpers as f64;
        let to_chips = (size_bb * view.big_blind as f64).round() as u64;
        return traced(
            raise_to_clamped(view, to_chips, strength, "preflop:open", probs),
            tr,
            "pre.unopened.open",
        );
    }
    // --- 投机跟注（小对/同连，多人且便宜，set-mine / 隐含赔率）---
    let limp_thr = nc.limp_behind.min_strength;
    let speculative = strength >= limp_thr
        && to_call_bb <= nc.limp_behind.max_call_bb
        && (entrants >= 1 || view.preflop_limpers >= 1);
    tr.cmp("preflop.limp_thr", strength, limp_thr, strength >= limp_thr);
    if speculative && view.valid.can_call {
        probs.push(("call".into(), 1.0));
        return traced(
            Decision {
                action: DecisionAction::Call,
                equity: strength,
                source: "preflop:limp_behind".into(),
                probs,
                trace: None,
            },
            tr,
            "pre.unopened.limp_behind",
        );
    }
    if view.valid.can_check {
        return traced(
            Decision {
                action: DecisionAction::Check,
                equity: strength,
                source: "preflop:check_option".into(),
                probs,
                trace: None,
            },
            tr,
            "pre.unopened.check_option",
        );
    }
    traced(
        Decision {
            action: DecisionAction::Fold,
            equity: strength,
            source: "preflop:fold".into(),
            probs,
            trace: None,
        },
        tr,
        "pre.unopened.fold",
    )
}

/// `pre.facing_raise`：翻前面对加注（按 [`RaiseLevel`] 分参）。
/// 叶：`pre.short.shove` / `pre.facing_raise.{value_reraise|odds_call|fold}.<vs_*>` /
/// `pre.facing_raise.check`（不一致输入边角，未拆）。
fn pre_facing_raise(
    ctx: &NodeCtx,
    lvl: RaiseLevel,
    _rng: &mut Rng,
    mut tr: DecisionTrace,
) -> Decision {
    let view = ctx.view;
    let nc = &ctx.cfg.nodes;
    let strength = ctx.eq.hand_strength(ctx.hole[0], ctx.hole[1]);
    tr.note_strength(strength);
    let eff_bb = view.effective_stack_chips() as f64 / view.big_blind as f64;
    let entrants = preflop_entrants(view);
    let mut probs = vec![];

    // --- pre.short 覆盖层：短栈推/弃 ---
    if pre_short_shove_gate(ctx, strength, entrants, &mut tr) {
        return traced(
            raise_decision(view, view.valid.max_raise, strength, "preflop:shove", probs),
            tr,
            "pre.short.shove",
        );
    }

    // 节点参数：面对的加注级别决定阈值与隐含赔率折让。
    let (reraise, call_strength, implied_discount) = match lvl {
        RaiseLevel::Open => (
            nc.vs_open.reraise,
            nc.vs_open.call_strength,
            nc.vs_open.implied_discount,
        ),
        RaiseLevel::ThreeBet => (
            nc.vs_3bet.reraise,
            nc.vs_3bet.call_strength,
            nc.vs_3bet.implied_discount,
        ),
        RaiseLevel::FourBetPlus => (
            nc.vs_4bet.reraise,
            nc.vs_4bet.call_strength,
            nc.vs_4bet.implied_discount,
        ),
    };
    let mult_tighten = ctx.cfg.preflop.multiway_tighten * (entrants.saturating_sub(1)) as f64;
    let rr_thr = (reraise + mult_tighten + ctx.adj.value_shift.max(0.0)).clamp(0.0, 0.99);
    let call_thr = (call_strength + mult_tighten).clamp(0.0, 0.99);

    // --- 价值再加注 ---
    tr.cmp("preflop.reraise_thr", strength, rr_thr, strength >= rr_thr);
    if strength >= rr_thr && view.valid.can_raise {
        let face = view.facing_raise_chips();
        let mult = if view.in_position() {
            ctx.cfg.preflop.threebet_mult_ip
        } else {
            ctx.cfg.preflop.threebet_mult_oop
        };
        let to_chips = (face as f64 * mult).round() as u64;
        let tag = match lvl {
            RaiseLevel::Open => "preflop:3bet",
            _ => "preflop:4bet",
        };
        return traced(
            raise_to_clamped(view, to_chips, strength, tag, probs),
            tr,
            &leaf_id("pre.facing_raise.value_reraise", Some(lvl.qualifier())),
        );
    }

    // --- 赔率平跟：底池赔率驱动（BB 防守修复沿革）。call_thr 只作为「想 flat 强牌」的
    // 补充入口，不作必要条件——否则 BB 拿好赔率也弃掉便宜防守。---
    if view.valid.can_call {
        let real_eq = equity_dispatch(
            view,
            ctx.eq,
            ctx.ml,
            ctx.opp_thr,
            ctx.hole,
            ctx.cfg.equity.samples / 2 + 1,
            situation_seed(view, ctx.cfg) ^ 0xCA11_0DD5,
        );
        // 隐含赔率折让：深栈 + 投机牌（暗三/坚果听的反向潜力），加注级别越高折让越窄。
        let implied = if eff_bb > 40.0 { implied_discount } else { 0.0 };
        let needed = (view.pot_odds() + ctx.cfg.postflop.call_equity_margin - implied).max(0.0);
        tr.note_equity(real_eq);
        tr.cmp("preflop.call_needed_eq", real_eq, needed, real_eq >= needed);
        tr.cmp("preflop.call_thr", strength, call_thr, strength >= call_thr);
        if real_eq >= needed || strength >= call_thr {
            probs.push(("call".into(), 1.0));
            return traced(
                Decision {
                    action: DecisionAction::Call,
                    equity: real_eq,
                    source: "preflop:call".into(),
                    probs,
                    trace: None,
                },
                tr,
                &leaf_id("pre.facing_raise.odds_call", Some(lvl.qualifier())),
            );
        }
    }

    if view.valid.can_check {
        return traced(
            Decision {
                action: DecisionAction::Check,
                equity: strength,
                source: "preflop:check".into(),
                probs,
                trace: None,
            },
            tr,
            "pre.facing_raise.check",
        );
    }
    traced(
        Decision {
            action: DecisionAction::Fold,
            equity: strength,
            source: "preflop:fold_vs_raise".into(),
            probs,
            trace: None,
        },
        tr,
        &leaf_id("pre.facing_raise.fold", Some(lvl.qualifier())),
    )
}

// ===========================================================================
// 翻后节点响应
// ===========================================================================

/// 翻后共享信号包（equity + 三正交特征 + 场面量），无人下注/面对下注两节点共用。
struct PostSignals {
    equity: f64,
    made: MadeClass,
    dr: crate::heuristic::features::DrawInfo,
    tex: crate::heuristic::features::Texture,
    n_opp: u32,
    ip: bool,
    spr: f64,
    multiway: f64,
}

/// 计算翻后信号并记入 trace（equity 采样次数/seed 与节点无关，属共享信息层）。
fn post_signals(ctx: &NodeCtx, tr: &mut DecisionTrace) -> PostSignals {
    let view = ctx.view;
    let equity = equity_dispatch(
        view,
        ctx.eq,
        ctx.ml,
        ctx.opp_thr,
        ctx.hole,
        ctx.cfg.equity.samples,
        situation_seed(view, ctx.cfg),
    );
    let made = made_class(ctx.hole, &view.board);
    let dr = draws(ctx.hole, &view.board);
    let tex = texture(&view.board);
    tr.note_equity(equity);
    tr.made = Some(format!("{made:?}"));
    tr.draw = Some(dr.has_meaningful_draw());
    let n_opp = view.num_active().saturating_sub(1).max(1);
    PostSignals {
        equity,
        made,
        dr,
        tex,
        n_opp,
        ip: view.in_position(),
        spr: view.spr(),
        multiway: (n_opp.saturating_sub(1)) as f64,
    }
}

/// 空气诈唬基础频率（多人递减 + 画像调制），float / bluff_raise 两节点共用的基线。
fn air_bluff_freq_base(ctx: &NodeCtx, multiway: f64) -> f64 {
    let oc = &ctx.cfg.postflop;
    (oc.air_bluff_freq * ctx.adj.bluff_mult * (1.0 - oc.multiway_bluff_cut * multiway).max(0.10))
        .clamp(0.0, 0.95)
}

/// fold 率 → float 频率乘子的单调映射（两锚线性插值，斜率/阈值造靶校准，避开朴素 breakeven）。
fn map_fold_to_mult(fold: f64, fg: &crate::heuristic::config::FloatGateConfig) -> f64 {
    if fg.fold_hi <= fg.fold_lo {
        return fg.mult_lo;
    }
    let t = ((fold - fg.fold_lo) / (fg.fold_hi - fg.fold_lo)).clamp(0.0, 1.0);
    fg.mult_lo + t * (fg.mult_hi - fg.mult_lo)
}

/// binding villain 的节点级 fold 读数（P0-3 阶段2）：对偶 `(街.池型)` 节点上、面对 hero
/// 本街 stab 的**最粘者**（min shrunk_fold —— bluff 需全弃，最粘者 binding）的 `(shrunk_fold, n)`。
///
/// `None` 语义 = **无法门控 → 调用方回退固定乘子**：翻前（不应到达）、或**任一对手无名**
/// （exploit 关：sim `names` 空 / live 缺名 → 逐字节回退）。节点 key 用与采集侧同口径的
/// [`facing_node_key`]/[`pot_type_name`] 构造（§5.2 单一坐标系）。
fn float_binding_readout(
    view: &GameView,
    opp: &OpponentTable,
    fg: &crate::heuristic::config::FloatGateConfig,
) -> Option<(f64, u32)> {
    let street = match view.street {
        Street::Flop => 1u8,
        Street::Turn => 2,
        Street::River | Street::Showdown => 3,
        Street::Preflop => return None, // float 语境本就翻后；翻前 → 回退。
    };
    let node = facing_node_key(street, pot_type_name(view.preflop_num_raises));
    let mut binding: Option<(f64, u32)> = None; // (shrunk_fold, n)
    for s in 0..view.num_seats as usize {
        if s == view.my_seat as usize || view.folded[s] {
            continue;
        }
        // 有对手无名 → None（exploit 关回退，保 byte-equal）。
        let name = view.names.get(&(s as u8))?;
        let r = opp.node_readout(name, &node, fg.shrink_k);
        binding = Some(match binding {
            Some(prev) if prev.0 <= r.shrunk_fold => prev, // 保留更小 fold（更粘 = binding）
            _ => (r.shrunk_fold, r.n),
        });
    }
    binding
}

/// binding 读数 → float 频率乘子（纯核，可单测）：node_gated 关 / 冷启动（`None`）/ binding
/// 样本 `n < min_samples` → 固定 `float_freq_mult`（= 现默认，保标准矩阵不退、exploit 关一致）；
/// 样本足 → 按 fold 单调映射（fold 越高 → 乘子越高，避开朴素 breakeven，锚造靶校准）。
fn float_mult_from_readout(
    binding: Option<(f64, u32)>,
    nc: &crate::heuristic::config::NodesConfig,
) -> f64 {
    let fg = &nc.float;
    if !fg.node_gated {
        return nc.float_freq_mult;
    }
    match binding {
        Some((fold, n)) if n >= fg.min_samples => map_fold_to_mult(fold, fg),
        _ => nc.float_freq_mult,
    }
}

/// `post.unopened.float` 频率乘子（P0-3 阶段2 节点级门控，`scenario §5.2`）：查 binding villain
/// 读数 → 纯核映射。**多人池保守**取最粘者（min shrunk_fold）读数并以其样本量做门。
fn float_freq_mult(ctx: &NodeCtx) -> f64 {
    let nc = &ctx.cfg.nodes;
    let binding = float_binding_readout(ctx.view, ctx.opp, &nc.float);
    float_mult_from_readout(binding, nc)
}

// ---------------------------------------------------------------------------
// P0-3 阶段2 第三消费轮：srp cbet_bluff 节点级门控（same-role，复用 float 门机制）。
// 造靶实测 srp c-bet 诈唬最优频率随 villain fold-to-cbet **反号**（粘站低频 / 爱弃高频，
// 与 float 同型；3bp 池候选② 已证 robust → 仅门控 srp）。四函数照 float 四函数写。
// ---------------------------------------------------------------------------

/// fold 率 → srp cbet_bluff 频率乘子的单调映射（两锚线性插值，锚**独立重标定**——cbet 默认
/// 频率高、乘子上限 1.0 只降不升，见 [`CbetBluffGateConfig`] 默认注释）。
fn map_fold_to_cbet_mult(fold: f64, cg: &crate::heuristic::config::CbetBluffGateConfig) -> f64 {
    if cg.fold_hi <= cg.fold_lo {
        return cg.mult_lo;
    }
    let t = ((fold - cg.fold_lo) / (cg.fold_hi - cg.fold_lo)).clamp(0.0, 1.0);
    cg.mult_lo + t * (cg.mult_hi - cg.mult_lo)
}

/// binding villain 的节点级 fold-to-cbet 读数（对偶 `(街.池型)` 节点上、面对 hero c-bet 的
/// **最粘者** min shrunk_fold —— bluff 需全弃，最粘者 binding）。`None` = 无法门控 → 回退乘子
/// 1.0：翻前（不应到达）、或任一对手无名（exploit 关：sim `names` 空 / live 缺名 → 逐字节回退）。
/// 节点 key 用与采集侧同口径的 [`facing_node_key`]/[`pot_type_name`] 构造（§5.2 单一坐标系）。
fn cbet_bluff_binding_readout(
    view: &GameView,
    opp: &OpponentTable,
    cg: &crate::heuristic::config::CbetBluffGateConfig,
) -> Option<(f64, u32)> {
    let street = match view.street {
        Street::Flop => 1u8,
        Street::Turn => 2,
        Street::River | Street::Showdown => 3,
        Street::Preflop => return None, // cbet_bluff 语境本就翻后；翻前 → 回退。
    };
    let node = facing_node_key(street, pot_type_name(view.preflop_num_raises));
    let mut binding: Option<(f64, u32)> = None; // (shrunk_fold, n)
    for s in 0..view.num_seats as usize {
        if s == view.my_seat as usize || view.folded[s] {
            continue;
        }
        // 有对手无名 → None（exploit 关回退，保 byte-equal）。
        let name = view.names.get(&(s as u8))?;
        let r = opp.node_readout(name, &node, cg.shrink_k);
        binding = Some(match binding {
            Some(prev) if prev.0 <= r.shrunk_fold => prev, // 保留更小 fold（更粘 = binding）
            _ => (r.shrunk_fold, r.n),
        });
    }
    binding
}

/// binding 读数 → srp cbet_bluff 频率乘子（纯核，可单测）：node_gated 关 / 冷启动（`None`）/
/// binding 样本 `n < min_samples` → **乘子 1.0**（= 保留 [`CbetSplit`] 现固定频率，标准矩阵不退、
/// exploit 关逐字节一致）；样本足 → 按 fold 单调映射（fold 越高 → 乘子越高，剥削反号）。
fn cbet_bluff_mult_from_readout(
    binding: Option<(f64, u32)>,
    cg: &crate::heuristic::config::CbetBluffGateConfig,
) -> f64 {
    if !cg.node_gated {
        return 1.0;
    }
    match binding {
        Some((fold, n)) if n >= cg.min_samples => map_fold_to_cbet_mult(fold, cg),
        _ => 1.0,
    }
}

/// `post.unopened.cbet_bluff.srp` 频率乘子（P0-3 阶段2 第三消费轮）：查 binding villain 读数 →
/// 纯核映射。**仅 srp 池门控**（3bp/other/limped → 乘子 1.0，保候选② robust 结论 + 现固定频率）。
fn cbet_bluff_freq_mult(ctx: &NodeCtx, pot: PotType) -> f64 {
    if pot != PotType::SingleRaised {
        return 1.0; // 3bp robust（候选②）/ other 稀疏 → 不门控，保现固定频率。
    }
    let cg = &ctx.cfg.nodes.cbet_gate;
    let binding = cbet_bluff_binding_readout(ctx.view, ctx.opp, cg);
    cbet_bluff_mult_from_readout(binding, cg)
}

// ---------------------------------------------------------------------------
// P0-3 阶段2 第四消费轮：bluff_raise 节点级门控（**same-role**，真杠杆 = fold-to-raise）。
// 上轮（4893739）读 fold-to-cbet = 跨角色代理被 REJECT；commit fac17a8 扩采 fold-to-raise 后，
// 本轮门读 `facing_raise_node_key`（**唯一实质区别** vs float/cbet 的 `facing_node_key`）= 同轴真信号。
// 造靶实测最优频率随 fold-to-raise **反号**（DonkFold 高弃→高频 / DonkStick 低弃→零频）→ **双向门**
// （乘子 [0.0, 2.0]，不套 cbet 上限 1.0）。四函数照 float 四函数写（回退 = 固定 `bluff_raise_freq_mult`）。
// ---------------------------------------------------------------------------

/// fold-to-raise 率 → bluff_raise 频率乘子的单调映射（两锚线性插值，锚**双向独立标定**——爱弃升频
/// 剥削 / 粘站降频止损，见 [`BluffRaiseGateConfig`] 默认注释）。
fn map_fold_to_bluff_raise_mult(
    fold: f64,
    bg: &crate::heuristic::config::BluffRaiseGateConfig,
) -> f64 {
    if bg.fold_hi <= bg.fold_lo {
        return bg.mult_lo;
    }
    let t = ((fold - bg.fold_lo) / (bg.fold_hi - bg.fold_lo)).clamp(0.0, 1.0);
    bg.mult_lo + t * (bg.mult_hi - bg.mult_lo)
}

/// binding villain 的节点级 **fold-to-raise** 读数（对偶 `<街>.<池型>.facing_raise` 节点上、面对
/// hero 加注的**最粘者** min shrunk_fold —— bluff-raise 需下注方全弃，最粘者 binding）。`None` =
/// 无法门控 → 回退固定 `bluff_raise_freq_mult`：翻前（不应到达）、或任一对手无名（exploit 关：sim
/// `names` 空 / live 缺名 → 逐字节回退）。
///
/// **唯一区别 vs float/cbet 门**：节点 key 用 [`facing_raise_node_key`]（fold-to-raise，villain 作
/// **进攻方被加注**）而非 [`facing_node_key`]（fold-to-cbet，villain 作**防守方**）——这正是上轮跨角色
/// REJECT → 本轮 same-role 的信号源分野。与采集侧共用同一构造器（§5.2 单一坐标系）。
fn bluff_raise_binding_readout(
    view: &GameView,
    opp: &OpponentTable,
    bg: &crate::heuristic::config::BluffRaiseGateConfig,
) -> Option<(f64, u32)> {
    let street = match view.street {
        Street::Flop => 1u8,
        Street::Turn => 2,
        Street::River | Street::Showdown => 3, // bluff_raise 决策非河短路，此分支实为 dead（保对称）。
        Street::Preflop => return None,        // bluff_raise 语境本就翻后；翻前 → 回退。
    };
    // ⚠ facing_raise_node_key（fold-to-raise）≠ facing_node_key（fold-to-cbet）：本轮 same-role 关键。
    let node = facing_raise_node_key(street, pot_type_name(view.preflop_num_raises));
    let mut binding: Option<(f64, u32)> = None; // (shrunk_fold, n)
    for s in 0..view.num_seats as usize {
        if s == view.my_seat as usize || view.folded[s] {
            continue;
        }
        // 有对手无名 → None（exploit 关回退，保 byte-equal）。
        let name = view.names.get(&(s as u8))?;
        let r = opp.node_readout(name, &node, bg.shrink_k);
        binding = Some(match binding {
            Some(prev) if prev.0 <= r.shrunk_fold => prev, // 保留更小 fold（更粘 = binding）
            _ => (r.shrunk_fold, r.n),
        });
    }
    binding
}

/// binding 读数 → bluff_raise 频率乘子（纯核，可单测）：node_gated 关 / 冷启动（`None`）/ binding
/// 样本 `n < min_samples` → 固定 `bluff_raise_freq_mult`（= 现默认 0.10，保标准矩阵不退、exploit 关
/// 逐字节一致、节点存活续攒样本）；样本足 → 按 fold-to-raise 单调映射（**双向**：爱弃升频 / 粘站降频）。
fn bluff_raise_mult_from_readout(
    binding: Option<(f64, u32)>,
    nc: &crate::heuristic::config::NodesConfig,
) -> f64 {
    let bg = &nc.bluff_raise_gate;
    if !bg.node_gated {
        return nc.bluff_raise_freq_mult;
    }
    match binding {
        Some((fold, n)) if n >= bg.min_samples => map_fold_to_bluff_raise_mult(fold, bg),
        _ => nc.bluff_raise_freq_mult,
    }
}

/// `post.facing.bluff_raise` 频率乘子（P0-3 阶段2 第四消费轮，same-role fold-to-raise 门控）：查
/// binding villain 读数 → 纯核映射。**门控全池型**（各按 `<街>.<池型>.facing_raise` 读，无 3bp robust
/// 例外——那是 cbet 的事）。冷启动/未知/小样本/`node_gated=false` → 固定 `bluff_raise_freq_mult`。
fn bluff_raise_freq_mult_gated(ctx: &NodeCtx) -> f64 {
    let nc = &ctx.cfg.nodes;
    let binding = bluff_raise_binding_readout(ctx.view, ctx.opp, &nc.bluff_raise_gate);
    bluff_raise_mult_from_readout(binding, nc)
}

/// `post.unopened`：翻后本街无人下注（下注 vs 过牌）。价值与 c-bet 按池型分参。
/// 叶：`post.unopened.value_bet[.srp|.3bp]` / `post.unopened.{commit|semibluff}` /
/// `post.unopened.cbet_bluff[.srp|.3bp]` / `post.unopened.{float|check}`。
fn post_unopened(ctx: &NodeCtx, pot: PotType, rng: &mut Rng, mut tr: DecisionTrace) -> Decision {
    let view = ctx.view;
    let oc = &ctx.cfg.postflop;
    let nc = &ctx.cfg.nodes;
    let s = post_signals(ctx, &mut tr);

    // --- 价值下注（池型分参节点）---
    let vb = match pot {
        PotType::SingleRaised => &nc.value_bet.srp,
        PotType::ThreeBet => &nc.value_bet.threebet,
        _ => &nc.value_bet.other,
    };
    let mut value_thr =
        vb.value_equity + oc.multiway_value_tighten * s.multiway + ctx.adj.value_shift;
    if s.ip && s.n_opp == 1 {
        value_thr -= oc.thin_value_ip_bonus;
    }
    value_thr = value_thr.clamp(0.30, 0.95);
    tr.cmp(
        "postflop.value_thr",
        s.equity,
        value_thr,
        s.equity >= value_thr,
    );
    if s.equity >= value_thr && s.made.rank() >= MadeClass::MidPair.rank() {
        let size = value_size(view, oc, &s.tex, s.made, s.spr, ctx.adj);
        return traced(
            bet_size_decision(view, size, s.equity, "postflop:value_bet"),
            tr,
            &leaf_id("post.unopened.value_bet", pot.split_qualifier()),
        );
    }
    // --- 低 SPR 承诺：顶对好牌直接下注承诺 ---
    tr.cmp(
        "postflop.commit_spr",
        s.spr,
        oc.commit_spr,
        s.spr < oc.commit_spr,
    );
    if s.spr < oc.commit_spr && s.made.rank() >= MadeClass::TopPair.rank() {
        let size = value_size(view, oc, &s.tex, s.made, s.spr, ctx.adj);
        return traced(
            bet_size_decision(view, size, s.equity, "postflop:commit_bet"),
            tr,
            "post.unopened.commit",
        );
    }
    // --- 半诈唬：有听牌 ---
    if s.dr.has_meaningful_draw() && s.equity >= oc.semibluff_min_equity {
        let semibluff_freq = (0.75 * ctx.adj.bluff_mult).clamp(0.0, 1.0);
        let roll = rng.unit();
        tr.cmp(
            "postflop.semibluff_freq",
            roll,
            semibluff_freq,
            roll < semibluff_freq,
        );
        if roll < semibluff_freq {
            let size = pick_bet_size(view, oc, &s.tex, s.made, s.spr, true);
            return traced(
                bet_size_decision(view, size, s.equity, "postflop:semibluff"),
                tr,
                "post.unopened.semibluff",
            );
        }
    }
    // --- 空气：主动权方 c-bet（池型分参）；无主动权低频 float ---
    if view.i_have_initiative() && s.made == MadeClass::Air && !s.dr.has_meaningful_draw() {
        let cb = match pot {
            PotType::SingleRaised => &nc.cbet.srp,
            PotType::ThreeBet => &nc.cbet.threebet,
            _ => &nc.cbet.other,
        };
        let cbet_freq = lerp(cb.freq_dry, cb.freq_wet, s.tex.wetness)
            * ctx.adj.bluff_mult
            * (1.0 - oc.multiway_bluff_cut * s.multiway).max(0.10)
            * cbet_bluff_freq_mult(ctx, pot); // P0-3 阶段2: srp 节点级门控（仅 srp；3bp robust 不动）
        let gate = cbet_freq.clamp(0.0, 0.95);
        let roll = rng.unit(); // gate 后掷点、位置绝对不动 → RNG 消费序列不变（trace 测试会抓）。
        tr.cmp("postflop.cbet_freq", roll, gate, roll < gate);
        if roll < gate {
            let size = pick_bet_size(view, oc, &s.tex, s.made, s.spr, false);
            return traced(
                bet_size_decision(view, size, s.equity, "postflop:cbet_bluff"),
                tr,
                &leaf_id("post.unopened.cbet_bluff", pot.split_qualifier()),
            );
        }
    } else if s.made == MadeClass::Air && !s.dr.has_meaningful_draw() {
        // P0-3 阶段2：float 频率乘子按 villain 节点级 fold-to-stab 门控（冷启动/未知/小样本
        // 回退固定 `float_freq_mult`）。掷点仍在 gate 之后、位置不变 → RNG 消费序列不变。
        let gate = air_bluff_freq_base(ctx, s.multiway) * float_freq_mult(ctx);
        let roll = rng.unit();
        tr.cmp("postflop.float_freq", roll, gate, roll < gate);
        if roll < gate {
            let size = pick_bet_size(view, oc, &s.tex, s.made, s.spr, false);
            return traced(
                bet_size_decision(view, size, s.equity, "postflop:float_bluff"),
                tr,
                "post.unopened.float",
            );
        }
    }
    // --- 其余过牌（控池 / 放弃）---
    traced(
        Decision {
            action: DecisionAction::Check,
            equity: s.equity,
            source: "postflop:check".into(),
            probs: vec![("check".into(), 1.0)],
            trace: None,
        },
        tr,
        "post.unopened.check",
    )
}

/// `post.facing`：翻后面对下注/加注（弃 / 跟 / 加）。池型暂未分参（场景 key 已记录，
/// Phase C 拆分候选：树 C 听牌防守细化）。
/// 叶：`post.facing.{value_raise|semibluff_raise|odds_call|bluff_raise|check_back|fold}`。
fn post_facing_bet(ctx: &NodeCtx, _pot: PotType, rng: &mut Rng, mut tr: DecisionTrace) -> Decision {
    let view = ctx.view;
    let oc = &ctx.cfg.postflop;
    // bluff_raise 频率乘子经 `bluff_raise_freq_mult_gated(ctx)`（内部读 `ctx.cfg.nodes`）→ 本函数
    // 无需 `nc` 直接绑定。
    let s = post_signals(ctx, &mut tr);

    let pot_odds = {
        let po = view.pot_odds();
        if po <= 0.0 {
            0.33 // 不一致输入（valid 面对注但重建 to_call=0）兜底
        } else {
            po
        }
    };
    // 隐含赔率折让（有听牌、非河、栈深）。
    let implied = if s.dr.has_meaningful_draw() && view.street != Street::River {
        (0.04 * s.spr).min(0.12)
    } else {
        0.0
    };
    let needed = (pot_odds + oc.call_equity_margin - implied).max(0.0);

    // --- 价值加注 ---
    let mut value_thr =
        oc.value_equity + oc.multiway_value_tighten * s.multiway + ctx.adj.value_shift;
    if s.ip && s.n_opp == 1 {
        value_thr -= oc.thin_value_ip_bonus;
    }
    value_thr = value_thr.clamp(0.30, 0.95);
    let raise_value_thr =
        (value_thr + 0.06 + oc.multiway_value_tighten * s.multiway).clamp(0.30, 0.97);
    tr.cmp(
        "postflop.raise_value_thr",
        s.equity,
        raise_value_thr,
        s.equity >= raise_value_thr,
    );
    if s.equity >= raise_value_thr
        && s.made.rank() >= MadeClass::TopPair.rank()
        && view.valid.can_raise
    {
        let size = value_size(view, oc, &s.tex, s.made, s.spr, ctx.adj);
        return traced(
            raise_pot_fraction(view, size, s.equity, "postflop:value_raise"),
            tr,
            "post.facing.value_raise",
        );
    }
    // --- 强听牌半诈唬加注（heads-up 偏多，多人少）---
    if s.dr.combo_draw || (s.dr.flush_draw && s.dr.oesd) {
        let sb_raise_freq = (0.5 * ctx.adj.bluff_mult * (1.0 - 0.5 * s.multiway)).clamp(0.0, 0.7);
        // Keep the short-circuit: the roll is only drawn when can_raise.
        if view.valid.can_raise {
            let roll = rng.unit();
            tr.cmp(
                "postflop.semibluff_raise_freq",
                roll,
                sb_raise_freq,
                roll < sb_raise_freq,
            );
            if roll < sb_raise_freq {
                let size = pick_bet_size(view, oc, &s.tex, s.made, s.spr, false);
                return traced(
                    raise_pot_fraction(view, size, s.equity, "postflop:semibluff_raise"),
                    tr,
                    "post.facing.semibluff_raise",
                );
            }
        }
    }
    // --- 跟注（含听牌靠隐含赔率）---
    tr.cmp(
        "postflop.call_needed_eq",
        s.equity,
        needed,
        s.equity >= needed,
    );
    if view.valid.can_call && s.equity >= needed {
        return traced(
            Decision {
                action: DecisionAction::Call,
                equity: s.equity,
                source: "postflop:call".into(),
                probs: vec![("call".into(), 1.0)],
                trace: None,
            },
            tr,
            "post.facing.odds_call",
        );
    }
    // --- 纯诈唬加注（低频，主要对紧手）---
    if s.made == MadeClass::Air
        && !s.dr.has_meaningful_draw()
        && view.valid.can_raise
        && view.street != Street::River
    {
        // P0-3 阶段2 第四消费轮：bluff_raise 频率乘子按 binding villain 节点级 **fold-to-raise**
        // （same-role 真杠杆）双向门控（冷启动/未知/小样本回退固定 `bluff_raise_freq_mult`）。掷点仍
        // 在 gate 之后、位置不变 → RNG 消费序列不变（trace 测试会抓）。
        let bluff_raise_freq = (air_bluff_freq_base(ctx, s.multiway)
            * bluff_raise_freq_mult_gated(ctx))
        .clamp(0.0, 0.4);
        let roll = rng.unit();
        tr.cmp(
            "postflop.bluff_raise_freq",
            roll,
            bluff_raise_freq,
            roll < bluff_raise_freq,
        );
        if roll < bluff_raise_freq {
            let size = pick_bet_size(view, oc, &s.tex, s.made, s.spr, false);
            return traced(
                raise_pot_fraction(view, size, s.equity, "postflop:bluff_raise"),
                tr,
                "post.facing.bluff_raise",
            );
        }
    }
    // 能 check 就 check（不一致输入边角：valid 说面对注但可 check）。
    if view.valid.can_check {
        return traced(
            Decision {
                action: DecisionAction::Check,
                equity: s.equity,
                source: "postflop:check_back".into(),
                probs: vec![("check".into(), 1.0)],
                trace: None,
            },
            tr,
            "post.facing.check_back",
        );
    }
    traced(
        Decision {
            action: DecisionAction::Fold,
            equity: s.equity,
            source: "postflop:fold".into(),
            probs: vec![("fold".into(), 1.0)],
            trace: None,
        },
        tr,
        "post.facing.fold",
    )
}

// ===========================================================================
// 尺寸
// ===========================================================================

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// 选下注底池分数。`value`=true 时按 made 强度/质地/SPR 选；诈唬偏向与价值同尺寸（平衡）。
fn pick_bet_size(
    _view: &GameView,
    oc: &crate::heuristic::config::PostflopConfig,
    tex: &crate::heuristic::features::Texture,
    made: MadeClass,
    spr: f64,
    value: bool,
) -> f64 {
    // 坚果 + 深 SPR 偶尔 overbet。
    if value && made == MadeClass::Monster && spr > 3.0 && tex.wetness > 0.4 {
        return oc.size_overbet;
    }
    // 湿面下大、干面下小。
    let base = lerp(oc.size_small, oc.size_big, tex.wetness);
    // 低 SPR 倾向更大（承诺）。
    let spr_adj = if spr < 2.0 { 0.15 } else { 0.0 };
    (base + spr_adj).clamp(oc.size_small, oc.size_overbet)
}

/// 价值动作尺寸 = `pick_bet_size(value)` × 剥削 value 放大（对 station 做大价值注），夹到
/// `[size_small, 1.6]`。仅价值注/价值加注调用；诈唬/半诈唬走 `pick_bet_size(false)` 不放大
/// （放大诈唬只会对不弃牌的站增亏）。`value_size_mult==1.0` 时与 `pick_bet_size(true)` 等价。
fn value_size(
    view: &GameView,
    oc: &crate::heuristic::config::PostflopConfig,
    tex: &crate::heuristic::features::Texture,
    made: MadeClass,
    spr: f64,
    adj: &ExploitAdjust,
) -> f64 {
    (pick_bet_size(view, oc, tex, made, spr, true) * adj.value_size_mult).clamp(oc.size_small, 1.6)
}

/// 把「底池分数」下注转成 raise-to chips 并夹到 valid（无前序 bet 的下注）。
fn bet_size_decision(view: &GameView, pot_frac: f64, equity: f64, src: &str) -> Decision {
    let pot = view.pot_chips();
    let me = view.my_seat as usize;
    let amount = (pot_frac * pot as f64).round() as u64;
    let to = view.committed_round[me] + amount;
    raise_to_clamped(view, to, equity, src, vec![])
}

/// 把「底池分数」加注转成 raise-to chips（面对前序 bet）。
fn raise_pot_fraction(view: &GameView, pot_frac: f64, equity: f64, src: &str) -> Decision {
    let pot = view.pot_chips();
    let to_call = view.to_call_chips();
    let raise_over = (pot_frac * (pot + to_call) as f64).round() as u64;
    let to = view.high_bet + raise_over;
    raise_to_clamped(view, to, equity, src, vec![])
}

/// 直接给 raise-to chips（短栈推用 max_raise）。
fn raise_decision(
    view: &GameView,
    max_raise: Option<u64>,
    equity: f64,
    src: &str,
    probs: Vec<(String, f64)>,
) -> Decision {
    let to = max_raise
        .unwrap_or_else(|| view.my_stack_chips() + view.committed_round[view.my_seat as usize]);
    raise_to_clamped(view, to, equity, src, probs)
}

/// raise-to 夹到 [min_raise, max_raise]；不能加注则退化 call/check/fold。
fn raise_to_clamped(
    view: &GameView,
    to_chips: u64,
    equity: f64,
    src: &str,
    mut probs: Vec<(String, f64)>,
) -> Decision {
    if !view.valid.can_raise {
        // 不能加注：能 call 就 call（有价值），否则 check/fold。
        if view.valid.can_call {
            return Decision {
                action: DecisionAction::Call,
                equity,
                source: format!("{src}:downgrade_call"),
                probs,
                trace: None,
            };
        }
        return Decision {
            action: if view.valid.can_check {
                DecisionAction::Check
            } else {
                DecisionAction::Fold
            },
            equity,
            source: format!("{src}:downgrade"),
            probs,
            trace: None,
        };
    }
    let min_r = view.valid.min_raise.unwrap_or(to_chips);
    let max_r = view.valid.max_raise.unwrap_or(to_chips);
    let to = to_chips.clamp(min_r.min(max_r), max_r);
    if probs.is_empty() {
        probs.push(("raise".into(), 1.0));
    }
    Decision {
        action: DecisionAction::Raise(to),
        equity,
        source: src.into(),
        probs,
        trace: None,
    }
}

// ===========================================================================
// P0-3 阶段2：float 节点级门控单测（映射单调 / 冷启动=默认 / n 门槛回退 / 节点查询正确）
// ===========================================================================
#[cfg(test)]
mod float_gate_tests {
    use super::{float_binding_readout, float_mult_from_readout, map_fold_to_mult};
    use crate::heuristic::config::NodesConfig;
    use crate::heuristic::opponent::{FacingCounts, OpponentTable};
    use crate::heuristic::view::{GameView, HistAction, Request, ValidActions};

    /// 造一个 flop-SRP float 局面：6-max，btn=4，hero(座0,BB) 平跟座4 的开池 → 翻牌
    /// 无人下注、hero 无主动权（座4 是翻前加注者）。座4 = 面对 hero stab 的 villain。
    fn float_view() -> GameView {
        let req = Request {
            hole: vec!["2c".into(), "Kc".into()],
            board: vec!["8s".into(), "6d".into(), "Td".into()],
            button_seat: 4,
            my_seat: 0,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                HistAction {
                    seat: 1,
                    action: "fold".into(),
                    to: None,
                    street: None,
                },
                HistAction {
                    seat: 2,
                    action: "fold".into(),
                    to: None,
                    street: None,
                },
                HistAction {
                    seat: 3,
                    action: "fold".into(),
                    to: None,
                    street: None,
                },
                HistAction {
                    seat: 4,
                    action: "raise".into(),
                    to: Some(40),
                    street: None,
                },
                HistAction {
                    seat: 5,
                    action: "fold".into(),
                    to: None,
                    street: None,
                },
                HistAction {
                    seat: 0,
                    action: "call".into(),
                    to: None,
                    street: None,
                },
            ],
            valid: ValidActions {
                can_check: true,
                can_call: false,
                can_raise: true,
                min_raise: Some(20),
                max_raise: Some(1960),
            },
            stacks: vec![2000, 1416, 1322, 6020, 11891, 1460],
            names: Default::default(),
            sb_seat: None,
            bb_seat: None,
        };
        GameView::from_request(&req)
    }

    /// 便捷：给对手名在某节点塞 facing 计数（fold_rate ≈ folds/n）。
    fn with_facing(opp: &mut OpponentTable, name: &str, node: &str, n: u32, folds: u32) {
        opp.by_name
            .entry(name.to_string())
            .or_default()
            .facing
            .insert(
                node.to_string(),
                FacingCounts {
                    n,
                    folds,
                    calls: n - folds,
                    raises: 0,
                },
            );
    }

    // --- 映射单调 + 端点/夹取 ---
    #[test]
    fn mapping_is_monotone_and_clamped() {
        let fg = NodesConfig::default().float; // 锚 [0.50→0.0, 0.65→3.0]
        assert_eq!(map_fold_to_mult(0.30, &fg), 0.0, "低锚以下 → mult_lo");
        assert_eq!(map_fold_to_mult(0.50, &fg), 0.0, "低锚点 = mult_lo");
        assert!(
            (map_fold_to_mult(0.65, &fg) - 3.0).abs() < 1e-9,
            "高锚点 = mult_hi"
        );
        assert!(
            (map_fold_to_mult(0.90, &fg) - 3.0).abs() < 1e-9,
            "高锚以上 → 夹取 mult_hi"
        );
        // 中点 0.575 → t=0.5 → 1.5。
        assert!((map_fold_to_mult(0.575, &fg) - 1.5).abs() < 1e-6);
        // 严格单调递增（fold 越高 → 乘子越高）。
        assert!(map_fold_to_mult(0.52, &fg) < map_fold_to_mult(0.58, &fg));
        assert!(map_fold_to_mult(0.58, &fg) < map_fold_to_mult(0.63, &fg));
    }

    // --- 冷启动 / node_gated 关 / n 门槛回退 = 固定 float_freq_mult ---
    #[test]
    fn gating_falls_back_to_fixed_mult() {
        let nc = NodesConfig::default(); // float_freq_mult=0.5, min_samples=30, node_gated=true
                                         // 冷启动（无 binding 读数）→ 固定乘子。
        assert_eq!(float_mult_from_readout(None, &nc), 0.5);
        // 小样本（n < 30）→ 固定乘子（即便 fold 很高）。
        assert_eq!(float_mult_from_readout(Some((0.95, 29)), &nc), 0.5);
        // node_gated 关 → 恒固定乘子（即便样本足、fold 高）。
        let mut nc_off = nc.clone();
        nc_off.float.node_gated = false;
        assert_eq!(float_mult_from_readout(Some((0.95, 500)), &nc_off), 0.5);
    }

    // --- 样本足 → 走映射（爱弃升 float / 粘站降 float） ---
    #[test]
    fn gating_uses_mapping_when_sample_ready() {
        let nc = NodesConfig::default();
        // 爱弃（fold 0.90，n 足）→ 高乘子（夹取 3.0 → float 高）。
        assert!((float_mult_from_readout(Some((0.90, 100)), &nc) - 3.0).abs() < 1e-9);
        // 粘站（fold 0.30，n 足）→ 0 乘子（float≈0，避开朴素 breakeven）。
        assert_eq!(float_mult_from_readout(Some((0.30, 100)), &nc), 0.0);
    }

    // --- 节点查询正确：查对偶 (街.池型)="flop.srp"，冷启动 = 默认（回退固定乘子） ---
    #[test]
    fn binding_queries_correct_node_and_cold_start_defaults() {
        let nc = NodesConfig::default();
        let mut v = float_view();
        assert_eq!(v.preflop_num_raises, 1, "srp 池");
        // 无名对手（exploit 关：names 空）→ None → 回退固定乘子（byte-equal 语义）。
        assert!(float_binding_readout(&v, &OpponentTable::new(), &nc.float).is_none());
        assert_eq!(
            float_mult_from_readout(
                float_binding_readout(&v, &OpponentTable::new(), &nc.float),
                &nc
            ),
            0.5,
            "冷启动 = 默认"
        );
        // 给座4 命名 + 在 flop.srp 塞高 fold 计数（爱弃 villain）。
        v.names.insert(4, "foldy".into());
        let mut opp = OpponentTable::new();
        with_facing(&mut opp, "foldy", "flop.srp", 200, 180); // raw fold 90%
        let b = float_binding_readout(&v, &opp, &nc.float).expect("有名 villain → Some");
        assert_eq!(b.1, 200, "读到 flop.srp 的 n（查错节点会得 n=0）");
        assert!(b.0 > 0.80, "shrunk_fold 应贴近 90%（n 大）");
        // 若把计数塞到 turn.srp（错节点）→ 查 flop.srp 得 n=0（回退）。
        let mut opp_wrong = OpponentTable::new();
        with_facing(&mut opp_wrong, "foldy", "turn.srp", 200, 180);
        let bw = float_binding_readout(&v, &opp_wrong, &nc.float).unwrap();
        assert_eq!(bw.1, 0, "flop.srp 无样本 → n=0");
        assert_eq!(
            float_mult_from_readout(Some(bw), &nc),
            0.5,
            "n=0 < 门槛 → 固定乘子"
        );
    }

    // --- 多人池保守：取最粘者（min shrunk_fold）binding ---
    #[test]
    fn binding_picks_stickiest_villain_multiway() {
        let nc = NodesConfig::default();
        let mut v = float_view();
        v.folded[3] = false; // 座3 也在场（多人池）
        v.names.insert(3, "sticky".into());
        v.names.insert(4, "foldy".into());
        let mut opp = OpponentTable::new();
        with_facing(&mut opp, "sticky", "flop.srp", 200, 60); // raw fold 30%（粘）
        with_facing(&mut opp, "foldy", "flop.srp", 200, 180); // raw fold 90%（弃）
        let b = float_binding_readout(&v, &opp, &nc.float).unwrap();
        assert!(b.0 < 0.40, "binding = 最粘者（sticky ~0.31），非爱弃者");
        // binding 粘 → 乘子 0（bluff 需全弃，最粘者 binding → 不 float）。
        assert_eq!(float_mult_from_readout(Some(b), &nc), 0.0);
        // 反证：仅 foldy 在场 → binding = foldy → 高乘子。
        v.folded[3] = true;
        let b2 = float_binding_readout(&v, &opp, &nc.float).unwrap();
        assert!((float_mult_from_readout(Some(b2), &nc) - 3.0).abs() < 1e-9);
    }
}

// ===========================================================================
// P0-3 阶段2 第三消费轮：srp cbet_bluff 节点级门控单测
// （映射单调 / 冷启动=乘子 1.0 / node_gated 关 / n 门槛回退 / 节点查询正确即查对偶 flop.srp）
// ===========================================================================
#[cfg(test)]
mod cbet_gate_tests {
    use super::{cbet_bluff_binding_readout, cbet_bluff_mult_from_readout, map_fold_to_cbet_mult};
    use crate::heuristic::config::NodesConfig;
    use crate::heuristic::opponent::{FacingCounts, OpponentTable};
    use crate::heuristic::view::{GameView, HistAction, Request, ValidActions};

    /// 造一个 flop-SRP 局面（hero 座0 开池被跟 → srp 池、flop）：座4 = 面对 hero c-bet 的 villain。
    /// 对 binding 读数只需 `preflop_num_raises==1`（srp）+ `street==Flop`，故沿用 float_view 同构。
    fn srp_flop_view() -> GameView {
        let req = Request {
            hole: vec!["2c".into(), "Kc".into()],
            board: vec!["8s".into(), "6d".into(), "Td".into()],
            button_seat: 4,
            my_seat: 0,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                HistAction {
                    seat: 1,
                    action: "fold".into(),
                    to: None,
                    street: None,
                },
                HistAction {
                    seat: 2,
                    action: "fold".into(),
                    to: None,
                    street: None,
                },
                HistAction {
                    seat: 3,
                    action: "fold".into(),
                    to: None,
                    street: None,
                },
                HistAction {
                    seat: 4,
                    action: "raise".into(),
                    to: Some(40),
                    street: None,
                },
                HistAction {
                    seat: 5,
                    action: "fold".into(),
                    to: None,
                    street: None,
                },
                HistAction {
                    seat: 0,
                    action: "call".into(),
                    to: None,
                    street: None,
                },
            ],
            valid: ValidActions {
                can_check: true,
                can_call: false,
                can_raise: true,
                min_raise: Some(20),
                max_raise: Some(1960),
            },
            stacks: vec![2000, 1416, 1322, 6020, 11891, 1460],
            names: Default::default(),
            sb_seat: None,
            bb_seat: None,
        };
        GameView::from_request(&req)
    }

    fn with_facing(opp: &mut OpponentTable, name: &str, node: &str, n: u32, folds: u32) {
        opp.by_name
            .entry(name.to_string())
            .or_default()
            .facing
            .insert(
                node.to_string(),
                FacingCounts {
                    n,
                    folds,
                    calls: n - folds,
                    raises: 0,
                },
            );
    }

    // --- 映射单调 + 端点/夹取（锚 [0.50→0.15, 0.75→1.0]，独立重标定） ---
    #[test]
    fn cbet_mapping_is_monotone_and_clamped() {
        let cg = NodesConfig::default().cbet_gate;
        assert_eq!(
            map_fold_to_cbet_mult(0.30, &cg),
            0.15,
            "低锚以下 → mult_lo（粘站降频）"
        );
        assert_eq!(map_fold_to_cbet_mult(0.50, &cg), 0.15, "低锚点 = mult_lo");
        assert!(
            (map_fold_to_cbet_mult(0.75, &cg) - 1.0).abs() < 1e-9,
            "高锚点 = mult_hi（爱弃保默认）"
        );
        assert!(
            (map_fold_to_cbet_mult(0.95, &cg) - 1.0).abs() < 1e-9,
            "高锚以上 → 夹取 mult_hi（乘子只降不升）"
        );
        // 中点 0.625 → t=0.5 → 0.575。
        assert!((map_fold_to_cbet_mult(0.625, &cg) - 0.575).abs() < 1e-6);
        // 严格单调递增（fold 越高 → 乘子越高 → 频率越高，剥削反号）。
        assert!(map_fold_to_cbet_mult(0.55, &cg) < map_fold_to_cbet_mult(0.65, &cg));
        assert!(map_fold_to_cbet_mult(0.65, &cg) < map_fold_to_cbet_mult(0.72, &cg));
        // 乘子恒 ≤ 1.0（默认频率高，门只降不升）。
        assert!(map_fold_to_cbet_mult(0.99, &cg) <= 1.0);
    }

    // --- 冷启动 / node_gated 关 / n 门槛回退 = 乘子 1.0（保现固定频率、byte-equal） ---
    #[test]
    fn cbet_gating_falls_back_to_unit_mult() {
        let cg = NodesConfig::default().cbet_gate; // node_gated=true, min_samples=30
                                                   // 冷启动（无 binding 读数）→ 乘子 1.0。
        assert_eq!(cbet_bluff_mult_from_readout(None, &cg), 1.0);
        // 小样本（n < 30）→ 乘子 1.0（即便 fold 很粘）。
        assert_eq!(cbet_bluff_mult_from_readout(Some((0.30, 29)), &cg), 1.0);
        // node_gated 关 → 恒乘子 1.0（即便样本足、fold 很粘）。
        let mut cg_off = cg.clone();
        cg_off.node_gated = false;
        assert_eq!(
            cbet_bluff_mult_from_readout(Some((0.30, 500)), &cg_off),
            1.0
        );
    }

    // --- 样本足 → 走映射（粘站降频 / 爱弃保默认） ---
    #[test]
    fn cbet_gating_uses_mapping_when_sample_ready() {
        let cg = NodesConfig::default().cbet_gate;
        // 粘站（fold 0.30，n 足）→ 0.15 乘子（降频，vs 默认 +40 mbb/g）。
        assert_eq!(cbet_bluff_mult_from_readout(Some((0.30, 100)), &cg), 0.15);
        // 爱弃（fold 0.90，n 足）→ 夹取 1.0（保默认高频 = 爱弃最优）。
        assert!((cbet_bluff_mult_from_readout(Some((0.90, 100)), &cg) - 1.0).abs() < 1e-9);
    }

    // --- 节点查询正确：查对偶 (街.池型)="flop.srp"，冷启动 = 乘子 1.0 ---
    #[test]
    fn cbet_binding_queries_correct_node_and_cold_start_defaults() {
        let cg = NodesConfig::default().cbet_gate;
        let mut v = srp_flop_view();
        assert_eq!(v.preflop_num_raises, 1, "srp 池");
        // 无名对手（exploit 关：names 空）→ None → 乘子 1.0（byte-equal 语义）。
        assert!(cbet_bluff_binding_readout(&v, &OpponentTable::new(), &cg).is_none());
        assert_eq!(
            cbet_bluff_mult_from_readout(
                cbet_bluff_binding_readout(&v, &OpponentTable::new(), &cg),
                &cg
            ),
            1.0,
            "冷启动 = 乘子 1.0"
        );
        // 给座4 命名 + 在 flop.srp 塞粘站计数（低 fold）。
        v.names.insert(4, "sticky".into());
        let mut opp = OpponentTable::new();
        with_facing(&mut opp, "sticky", "flop.srp", 200, 60); // raw fold 30%
        let b = cbet_bluff_binding_readout(&v, &opp, &cg).expect("有名 villain → Some");
        assert_eq!(b.1, 200, "读到 flop.srp 的 n（查错节点会得 n=0）");
        assert!(b.0 < 0.40, "shrunk_fold 应贴近 30%（n 大）");
        assert_eq!(
            cbet_bluff_mult_from_readout(Some(b), &cg),
            0.15,
            "粘站 → 降频 0.15"
        );
        // 若把计数塞到 turn.srp（错节点）→ 查 flop.srp 得 n=0（回退乘子 1.0）。
        let mut opp_wrong = OpponentTable::new();
        with_facing(&mut opp_wrong, "sticky", "turn.srp", 200, 60);
        let bw = cbet_bluff_binding_readout(&v, &opp_wrong, &cg).unwrap();
        assert_eq!(bw.1, 0, "flop.srp 无样本 → n=0");
        assert_eq!(
            cbet_bluff_mult_from_readout(Some(bw), &cg),
            1.0,
            "n=0 < 门槛 → 乘子 1.0"
        );
    }

    // --- 多人池保守：取最粘者（min shrunk_fold）binding ---
    #[test]
    fn cbet_binding_picks_stickiest_villain_multiway() {
        let cg = NodesConfig::default().cbet_gate;
        let mut v = srp_flop_view();
        v.folded[3] = false; // 座3 也在场（多人池）
        v.names.insert(3, "sticky".into());
        v.names.insert(4, "foldy".into());
        let mut opp = OpponentTable::new();
        with_facing(&mut opp, "sticky", "flop.srp", 200, 60); // raw fold 30%（粘）
        with_facing(&mut opp, "foldy", "flop.srp", 200, 180); // raw fold 90%（弃）
        let b = cbet_bluff_binding_readout(&v, &opp, &cg).unwrap();
        assert!(b.0 < 0.40, "binding = 最粘者（sticky ~0.31），非爱弃者");
        // binding 粘 → 降频 0.15（多人池保守：任一粘站在场即降 c-bet 空气频率）。
        assert_eq!(cbet_bluff_mult_from_readout(Some(b), &cg), 0.15);
        // 反证：仅 foldy 在场 → binding = foldy → 保默认乘子 1.0。
        v.folded[3] = true;
        let b2 = cbet_bluff_binding_readout(&v, &opp, &cg).unwrap();
        assert!((cbet_bluff_mult_from_readout(Some(b2), &cg) - 1.0).abs() < 1e-9);
    }
}

// ===========================================================================
// P0-3 阶段2 第四消费轮：bluff_raise 门单测（same-role fold-to-raise）
// （映射单调双向 / 冷启动=固定 0.10 / node_gated 关 / n 门槛回退 /
//  **节点查询用 facing_raise key 而非 facing key** = 本轮 vs 上轮 REJECT 的决定性区别）
// ===========================================================================
#[cfg(test)]
mod bluff_raise_gate_tests {
    use super::{
        bluff_raise_binding_readout, bluff_raise_mult_from_readout, map_fold_to_bluff_raise_mult,
    };
    use crate::heuristic::config::NodesConfig;
    use crate::heuristic::opponent::{FacingCounts, OpponentTable};
    use crate::heuristic::view::{GameView, HistAction, Request, ValidActions};

    /// 造一个 flop-SRP 局面（hero 座0 开池被跟 → srp 池、flop）：座4 = 面对 hero 的 villain。
    /// bluff_raise 读数只需 `preflop_num_raises==1`（srp）+ `street==Flop`（readout 不校验「面对注」）。
    fn srp_flop_view() -> GameView {
        let req = Request {
            hole: vec!["2c".into(), "Kc".into()],
            board: vec!["8s".into(), "6d".into(), "Td".into()],
            button_seat: 4,
            my_seat: 0,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                HistAction {
                    seat: 1,
                    action: "fold".into(),
                    to: None,
                    street: None,
                },
                HistAction {
                    seat: 2,
                    action: "fold".into(),
                    to: None,
                    street: None,
                },
                HistAction {
                    seat: 3,
                    action: "fold".into(),
                    to: None,
                    street: None,
                },
                HistAction {
                    seat: 4,
                    action: "raise".into(),
                    to: Some(40),
                    street: None,
                },
                HistAction {
                    seat: 5,
                    action: "fold".into(),
                    to: None,
                    street: None,
                },
                HistAction {
                    seat: 0,
                    action: "call".into(),
                    to: None,
                    street: None,
                },
            ],
            valid: ValidActions {
                can_check: true,
                can_call: false,
                can_raise: true,
                min_raise: Some(20),
                max_raise: Some(1960),
            },
            stacks: vec![2000, 1416, 1322, 6020, 11891, 1460],
            names: Default::default(),
            sb_seat: None,
            bb_seat: None,
        };
        GameView::from_request(&req)
    }

    fn with_facing(opp: &mut OpponentTable, name: &str, node: &str, n: u32, folds: u32) {
        opp.by_name
            .entry(name.to_string())
            .or_default()
            .facing
            .insert(
                node.to_string(),
                FacingCounts {
                    n,
                    folds,
                    calls: n - folds,
                    raises: 0,
                },
            );
    }

    // --- 映射单调**双向** + 端点/夹取（锚 [0.30→0.0, 0.80→2.0]，不套 cbet 上限 1.0） ---
    #[test]
    fn bluff_raise_mapping_is_monotone_bidirectional_and_clamped() {
        let bg = NodesConfig::default().bluff_raise_gate;
        // 低锚以下（粘站 DonkStick 3.3%）→ mult_lo=0.0（降频止损，vs 回退 0.10 更低）。
        assert_eq!(map_fold_to_bluff_raise_mult(0.033, &bg), 0.0);
        assert_eq!(
            map_fold_to_bluff_raise_mult(0.30, &bg),
            0.0,
            "低锚点 = mult_lo"
        );
        // 高锚以上（爱弃 DonkFold 86.7%）→ 夹取 mult_hi=2.0（升频剥削，远超回退 0.10）。
        assert!((map_fold_to_bluff_raise_mult(0.867, &bg) - 2.0).abs() < 1e-9);
        assert!(
            (map_fold_to_bluff_raise_mult(0.80, &bg) - 2.0).abs() < 1e-9,
            "高锚点 = mult_hi"
        );
        // DonkFold turn 74.5% → t=(0.745-0.30)/0.50=0.89 → 1.78（升频，未夹顶）。
        assert!((map_fold_to_bluff_raise_mult(0.745, &bg) - 1.78).abs() < 1e-6);
        // 中点 0.55 → t=0.5 → 1.0（跨越回退 0.10，证双向可上调）。
        assert!((map_fold_to_bluff_raise_mult(0.55, &bg) - 1.0).abs() < 1e-6);
        // 严格单调递增（fold-to-raise 越高 → 乘子越高 → 频率越高，剥削反号）。
        assert!(map_fold_to_bluff_raise_mult(0.40, &bg) < map_fold_to_bluff_raise_mult(0.60, &bg));
        assert!(map_fold_to_bluff_raise_mult(0.60, &bg) < map_fold_to_bluff_raise_mult(0.78, &bg));
        // 双向证据：低端 < 回退 0.10 < 高端（门既能下调又能上调）。
        assert!(map_fold_to_bluff_raise_mult(0.20, &bg) < 0.10);
        assert!(map_fold_to_bluff_raise_mult(0.85, &bg) > 0.10);
    }

    // --- 冷启动 / node_gated 关 / n 门槛回退 = 固定 bluff_raise_freq_mult（0.10），非 1.0 ---
    #[test]
    fn bluff_raise_gating_falls_back_to_fixed_mult() {
        let nc = NodesConfig::default(); // bluff_raise_freq_mult=0.10, node_gated=true, min=30
        let fb = nc.bluff_raise_freq_mult;
        assert!((fb - 0.10).abs() < 1e-12, "回退基准 = 0.10");
        // 冷启动（无 binding 读数）→ 固定 0.10（非 cbet 的 1.0）。
        assert_eq!(bluff_raise_mult_from_readout(None, &nc), fb);
        // 小样本（n < 30）→ 固定 0.10（即便 fold-to-raise 很高/很低）。
        assert_eq!(bluff_raise_mult_from_readout(Some((0.90, 29)), &nc), fb);
        assert_eq!(bluff_raise_mult_from_readout(Some((0.02, 29)), &nc), fb);
        // node_gated 关 → 恒固定 0.10（即便样本足）。
        let mut nc_off = nc.clone();
        nc_off.bluff_raise_gate.node_gated = false;
        assert_eq!(
            bluff_raise_mult_from_readout(Some((0.90, 500)), &nc_off),
            fb
        );
        assert_eq!(
            bluff_raise_mult_from_readout(Some((0.02, 500)), &nc_off),
            fb
        );
    }

    // --- 样本足 → 走映射（爱弃升频 / 粘站降频，双向剥削反号） ---
    #[test]
    fn bluff_raise_gating_uses_mapping_when_sample_ready() {
        let nc = NodesConfig::default();
        // DonkFold（fold-to-raise 0.867，n 足）→ 夹取 2.0（升频剥削爱弃，>> 回退 0.10）。
        assert!((bluff_raise_mult_from_readout(Some((0.867, 154)), &nc) - 2.0).abs() < 1e-9);
        // DonkStick（fold-to-raise 0.033，n 足）→ 0.0（降频止损粘站，< 回退 0.10）。
        assert_eq!(bluff_raise_mult_from_readout(Some((0.033, 154)), &nc), 0.0);
    }

    // --- 决定性测试：节点查询用 **facing_raise** key（fold-to-raise）而非 facing key（fold-to-cbet） ---
    // 上轮 REJECT 正因门读 facing_node_key（fold-to-cbet）跨角色；本轮读 facing_raise_node_key 同轴。
    #[test]
    fn bluff_raise_binding_queries_facing_raise_node_not_facing_node() {
        let nc = NodesConfig::default();
        let bg = &nc.bluff_raise_gate;
        let mut v = srp_flop_view();
        assert_eq!(v.preflop_num_raises, 1, "srp 池");
        // 无名对手（exploit 关：names 空）→ None → 回退固定 0.10（byte-equal 语义）。
        assert!(bluff_raise_binding_readout(&v, &OpponentTable::new(), bg).is_none());
        assert_eq!(
            bluff_raise_mult_from_readout(
                bluff_raise_binding_readout(&v, &OpponentTable::new(), bg),
                &nc
            ),
            nc.bluff_raise_freq_mult,
            "冷启动 = 回退 0.10"
        );
        // 给座4 命名，**同时**在 fold-to-cbet 节点（flop.srp）塞粘站计数、在 fold-to-raise 节点
        // （flop.srp.facing_raise）塞爱弃计数 —— 两轴相反，证门读的是 facing_raise（而非 facing）。
        v.names.insert(4, "villain".into());
        let mut opp = OpponentTable::new();
        with_facing(&mut opp, "villain", "flop.srp", 200, 20); // fold-to-cbet 10%（粘，上轮读这个）
        with_facing(&mut opp, "villain", "flop.srp.facing_raise", 154, 145); // fold-to-raise ~94%（爱弃）
        let b = bluff_raise_binding_readout(&v, &opp, bg).expect("有名 villain → Some");
        assert_eq!(
            b.1, 154,
            "读到 flop.srp.facing_raise 的 n=154（若误读 facing key 会得 n=200）"
        );
        assert!(
            b.0 > 0.80,
            "shrunk_fold 贴近 fold-to-raise 94%（若误读 fold-to-cbet 会 ~0.10）"
        );
        assert!(
            (bluff_raise_mult_from_readout(Some(b), &nc) - 2.0).abs() < 1e-9,
            "爱弃(被加注) → 升频 2.0（若误读 fold-to-cbet 粘站会得 0.0，方向反）"
        );
        // 反证：只在 fold-to-cbet 节点有样本、facing_raise 无 → 查 facing_raise 得 n=0 → 回退 0.10。
        let mut opp_cbet_only = OpponentTable::new();
        with_facing(&mut opp_cbet_only, "villain", "flop.srp", 200, 180); // 仅 fold-to-cbet
        let bc = bluff_raise_binding_readout(&v, &opp_cbet_only, bg).unwrap();
        assert_eq!(
            bc.1, 0,
            "facing_raise 节点无样本 → n=0（证没去读 fold-to-cbet 节点）"
        );
        assert_eq!(
            bluff_raise_mult_from_readout(Some(bc), &nc),
            nc.bluff_raise_freq_mult,
            "n=0 < 门槛 → 回退 0.10"
        );
    }

    // --- 多人池保守：取最粘者（min shrunk_fold）binding（bluff-raise 需全弃 → min 正确） ---
    #[test]
    fn bluff_raise_binding_picks_stickiest_villain_multiway() {
        let nc = NodesConfig::default();
        let bg = &nc.bluff_raise_gate;
        let node = "flop.srp.facing_raise";
        let mut v = srp_flop_view();
        v.folded[3] = false; // 座3 也在场（多人池）
        v.names.insert(3, "sticky".into());
        v.names.insert(4, "foldy".into());
        let mut opp = OpponentTable::new();
        with_facing(&mut opp, "sticky", node, 154, 8); // fold-to-raise ~5%（粘，DonkStick 型）
        with_facing(&mut opp, "foldy", node, 154, 145); // fold-to-raise ~94%（弃，DonkFold 型）
        let b = bluff_raise_binding_readout(&v, &opp, bg).unwrap();
        assert!(b.0 < 0.20, "binding = 最粘者（sticky ~0.05），非爱弃者");
        // binding 粘 → 降频 0.0（多人池保守：任一粘站在场即压 bluff_raise，因需全弃才成）。
        assert_eq!(bluff_raise_mult_from_readout(Some(b), &nc), 0.0);
        // 反证：仅 foldy 在场 → binding = foldy → 升频 2.0（可剥削）。
        v.folded[3] = true;
        let b2 = bluff_raise_binding_readout(&v, &opp, bg).unwrap();
        assert!((bluff_raise_mult_from_readout(Some(b2), &nc) - 2.0).abs() < 1e-9);
    }
}
