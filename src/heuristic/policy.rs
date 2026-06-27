//! 决策编排：翻前 + 翻后启发式 → 动作分布 → 混合 → 尺寸映射。
//!
//! 入口 [`decide`]。所有阈值/尺寸来自 [`HeuristicConfig`]，对手 range 来自
//! [`build_opp_thresholds`]，主货币是 [`EquityEngine::equity`]。

use crate::core::Street;
use crate::heuristic::config::HeuristicConfig;
use crate::heuristic::equity::{EquityEngine, Rng};
use crate::heuristic::features::{draws, made_class, texture, MadeClass};
use crate::heuristic::opponent::{ExploitAdjust, OpponentTable};
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

// ===========================================================================
// 入口
// ===========================================================================

pub fn decide(
    view: &GameView,
    eq: &EquityEngine,
    cfg: &HeuristicConfig,
    opp: &OpponentTable,
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

    let d = if view.street == Street::Preflop {
        preflop_decide(view, eq, cfg, &adj, &opp_thr, hole, &mut rng)
    } else {
        postflop_decide(view, eq, cfg, &adj, &opp_thr, hole, &mut rng)
    };
    legalize(d, view)
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

/// 异常兜底：能 check 就 check，否则 fold。
fn safe_fallback(view: &GameView, why: &str) -> Decision {
    let action = if view.valid.can_check {
        DecisionAction::Check
    } else {
        DecisionAction::Fold
    };
    Decision {
        action,
        equity: 0.0,
        source: format!("fallback:{why}"),
        probs: vec![],
    }
}

// ===========================================================================
// 翻前
// ===========================================================================

fn preflop_decide(
    view: &GameView,
    eq: &EquityEngine,
    cfg: &HeuristicConfig,
    adj: &ExploitAdjust,
    opp_thr: &[f64],
    hole: [crate::core::Card; 2],
    rng: &mut Rng,
) -> Decision {
    let pc = &cfg.preflop;
    let strength = eq.hand_strength(hole[0], hole[1]);
    let pos = view.my_pos_bucket();
    let eff_bb = view.effective_stack_chips() as f64 / view.big_blind as f64;
    let to_call_bb = view.to_call_bb();
    // 已入池对手数（多人收紧）。
    let entrants = view
        .committed_total
        .iter()
        .enumerate()
        .filter(|(s, &c)| *s != view.my_seat as usize && !view.folded[*s] && c > view.big_blind / 2)
        .count() as u32;

    let mut probs = vec![];

    // --- 短栈推/弃 ---
    if eff_bb < pc.shortstack_bb && view.valid.can_raise {
        let shove_thr = pc.shove_strength + 0.02 * entrants as f64 - adj.steal_bonus;
        if strength >= shove_thr {
            return raise_decision(view, view.valid.max_raise, strength, "preflop:shove", probs);
        }
        // 否则按下面常规逻辑（可能 call/fold）。
    }

    let facing_raise = view.preflop_num_raises >= 1;
    let facing_3bet = view.preflop_num_raises >= 2;

    if !facing_raise {
        // --- first-in / 面对 limper ---
        let mult_tighten = pc.multiway_tighten * entrants as f64;
        let open_thr_base = match pos {
            PosBucket::Early => pc.open_early,
            PosBucket::Mid => pc.open_mid,
            PosBucket::Late => pc.open_late,
            PosBucket::SmallBlind => pc.open_sb,
            PosBucket::BigBlind => pc.open_mid, // BB 可加注的 limp 底池
        };
        let open_thr = (open_thr_base + mult_tighten - adj.steal_bonus).clamp(0.0, 0.99);
        if strength >= open_thr && view.valid.can_raise {
            // 开池/iso 尺寸。
            let size_bb =
                pc.open_size_bb + pc.iso_extra_per_limper_bb * view.preflop_limpers as f64;
            let to_chips = (size_bb * view.big_blind as f64).round() as u64;
            return raise_to_clamped(view, to_chips, strength, "preflop:open", probs);
        }
        // 投机跟注（小对/同连，多人且便宜，set-mine / 隐含赔率）。
        let speculative = strength >= pc.flat_call - 0.06
            && to_call_bb <= 1.5
            && (entrants >= 1 || view.preflop_limpers >= 1);
        if speculative && view.valid.can_call {
            probs.push(("call".into(), 1.0));
            return Decision {
                action: DecisionAction::Call,
                equity: strength,
                source: "preflop:limp_behind".into(),
                probs,
            };
        }
        if view.valid.can_check {
            return Decision {
                action: DecisionAction::Check,
                equity: strength,
                source: "preflop:check_option".into(),
                probs,
            };
        }
        return Decision {
            action: DecisionAction::Fold,
            equity: strength,
            source: "preflop:fold".into(),
            probs,
        };
    }

    // --- 面对加注 ---
    let mult_tighten = pc.multiway_tighten * (entrants.saturating_sub(1)) as f64;
    let (rr_thr, call_thr) = if facing_3bet {
        (pc.fourbet, pc.vs_threebet_continue)
    } else {
        (pc.threebet, pc.flat_call)
    };
    let rr_thr = (rr_thr + mult_tighten + adj.value_shift.max(0.0)).clamp(0.0, 0.99);
    let call_thr = (call_thr + mult_tighten).clamp(0.0, 0.99);

    // value (re)raise
    if strength >= rr_thr && view.valid.can_raise {
        let face = view.facing_raise_chips();
        let ip = view.in_position();
        let mult = if ip {
            pc.threebet_mult_ip
        } else {
            pc.threebet_mult_oop
        };
        let to_chips = (face as f64 * mult).round() as u64;
        let tag = if facing_3bet {
            "preflop:4bet"
        } else {
            "preflop:3bet"
        };
        return raise_to_clamped(view, to_chips, strength, tag, probs);
    }

    // flat call：底池赔率驱动（修 BB 防守漏洞）。call_thr 只作为「想 flat 强牌」的补充入口，
    // 不作必要条件——否则 BB 拿好赔率也弃掉便宜防守。
    if view.valid.can_call {
        let real_eq = eq.equity(
            hole,
            &view.board,
            opp_thr,
            cfg.equity.samples / 2 + 1,
            situation_seed(view, cfg) ^ 0xCA11_0DD5,
        );
        // 隐含赔率折让：深栈 + 投机牌（暗三/坚果听的反向潜力），3bet 池折让收窄（更可能被支配）。
        let implied = if eff_bb > 40.0 {
            (0.05 / (1.0 + (view.preflop_num_raises.saturating_sub(1)) as f64)).min(0.05)
        } else {
            0.0
        };
        let needed = (view.pot_odds() + cfg.postflop.call_equity_margin - implied).max(0.0);
        if real_eq >= needed || strength >= call_thr {
            probs.push(("call".into(), 1.0));
            return Decision {
                action: DecisionAction::Call,
                equity: real_eq,
                source: "preflop:call".into(),
                probs,
            };
        }
        let _ = rng; // 预留混合
    }

    if view.valid.can_check {
        return Decision {
            action: DecisionAction::Check,
            equity: strength,
            source: "preflop:check".into(),
            probs,
        };
    }
    Decision {
        action: DecisionAction::Fold,
        equity: strength,
        source: "preflop:fold_vs_raise".into(),
        probs,
    }
}

// ===========================================================================
// 翻后
// ===========================================================================

fn postflop_decide(
    view: &GameView,
    eq: &EquityEngine,
    cfg: &HeuristicConfig,
    adj: &ExploitAdjust,
    opp_thr: &[f64],
    hole: [crate::core::Card; 2],
    rng: &mut Rng,
) -> Decision {
    let oc = &cfg.postflop;
    let equity = eq.equity(
        hole,
        &view.board,
        opp_thr,
        cfg.equity.samples,
        situation_seed(view, cfg),
    );
    let made = made_class(hole, &view.board);
    let dr = draws(hole, &view.board);
    let tex = texture(&view.board);
    let n_opp = view.num_active().saturating_sub(1).max(1);
    let ip = view.in_position();
    let spr = view.spr();
    // valid 权威：不能 check = 面对下注（即便重建 to_call 因不一致输入算成 0）。
    let facing_bet = view.to_call_chips() > 0 || !view.valid.can_check;

    // 多人收紧。
    let multiway = (n_opp.saturating_sub(1)) as f64;
    let mut value_thr = oc.value_equity + oc.multiway_value_tighten * multiway + adj.value_shift;
    if ip && n_opp == 1 {
        value_thr -= oc.thin_value_ip_bonus;
    }
    value_thr = value_thr.clamp(0.30, 0.95);
    let bluff_freq_base =
        (oc.air_bluff_freq * adj.bluff_mult * (1.0 - oc.multiway_bluff_cut * multiway).max(0.10))
            .clamp(0.0, 0.95);

    if !facing_bet {
        // === 无人下注：下注 vs 过牌 ===
        // 价值。
        if equity >= value_thr && made.rank() >= MadeClass::MidPair.rank() {
            let size = pick_bet_size(view, oc, &tex, made, spr, true);
            return bet_size_decision(view, size, equity, "postflop:value_bet");
        }
        // 低 SPR 承诺：顶对好牌直接下注承诺。
        if spr < oc.commit_spr && made.rank() >= MadeClass::TopPair.rank() {
            let size = pick_bet_size(view, oc, &tex, made, spr, true);
            return bet_size_decision(view, size, equity, "postflop:commit_bet");
        }
        // 半诈唬：有听牌。
        if dr.has_meaningful_draw() && equity >= oc.semibluff_min_equity {
            let semibluff_freq = (0.75 * adj.bluff_mult).clamp(0.0, 1.0);
            if rng.unit() < semibluff_freq {
                let size = pick_bet_size(view, oc, &tex, made, spr, true);
                return bet_size_decision(view, size, equity, "postflop:semibluff");
            }
        }
        // c-bet / 纯诈唬（主动权方在干面）。
        if view.i_have_initiative() && made == MadeClass::Air && !dr.has_meaningful_draw() {
            let cbet_freq = lerp(oc.cbet_freq_dry, oc.cbet_freq_wet, tex.wetness)
                * adj.bluff_mult
                * (1.0 - oc.multiway_bluff_cut * multiway).max(0.10);
            if rng.unit() < cbet_freq.clamp(0.0, 0.95) {
                let size = pick_bet_size(view, oc, &tex, made, spr, false);
                return bet_size_decision(view, size, equity, "postflop:cbet_bluff");
            }
        } else if made == MadeClass::Air && !dr.has_meaningful_draw() {
            // 无主动权的空气：偶尔无主动诈唬（低频）。
            if rng.unit() < bluff_freq_base * 0.5 {
                let size = pick_bet_size(view, oc, &tex, made, spr, false);
                return bet_size_decision(view, size, equity, "postflop:float_bluff");
            }
        }
        // 其余过牌（控池 / 放弃）。
        return Decision {
            action: DecisionAction::Check,
            equity,
            source: "postflop:check".into(),
            probs: vec![("check".into(), 1.0)],
        };
    }

    // === 面对下注：弃/跟/加 ===
    let pot_odds = {
        let po = view.pot_odds();
        if po <= 0.0 {
            0.33 // 不一致输入（valid 面对注但重建 to_call=0）兜底
        } else {
            po
        }
    };
    // 隐含赔率折让（有听牌、非河、栈深）。
    let implied = if dr.has_meaningful_draw() && view.street != Street::River {
        (0.04 * spr).min(0.12)
    } else {
        0.0
    };
    let needed = (pot_odds + oc.call_equity_margin - implied).max(0.0);

    // 价值加注。
    let raise_value_thr =
        (value_thr + 0.06 + oc.multiway_value_tighten * multiway).clamp(0.30, 0.97);
    if equity >= raise_value_thr && made.rank() >= MadeClass::TopPair.rank() && view.valid.can_raise
    {
        let size = pick_bet_size(view, oc, &tex, made, spr, true);
        return raise_pot_fraction(view, size, equity, "postflop:value_raise");
    }
    // 强听牌半诈唬加注（heads-up 偏多，多人少）。
    if dr.combo_draw || (dr.flush_draw && dr.oesd) {
        let sb_raise_freq = (0.5 * adj.bluff_mult * (1.0 - 0.5 * multiway)).clamp(0.0, 0.7);
        if view.valid.can_raise && rng.unit() < sb_raise_freq {
            let size = pick_bet_size(view, oc, &tex, made, spr, false);
            return raise_pot_fraction(view, size, equity, "postflop:semibluff_raise");
        }
    }
    // 跟注（含听牌靠隐含赔率）。
    if view.valid.can_call && equity >= needed {
        return Decision {
            action: DecisionAction::Call,
            equity,
            source: "postflop:call".into(),
            probs: vec![("call".into(), 1.0)],
        };
    }
    // 纯诈唬加注（低频，主要对紧手）。
    if made == MadeClass::Air
        && !dr.has_meaningful_draw()
        && view.valid.can_raise
        && view.street != Street::River
    {
        let bluff_raise_freq = (bluff_freq_base * 0.35).clamp(0.0, 0.4);
        if rng.unit() < bluff_raise_freq {
            let size = pick_bet_size(view, oc, &tex, made, spr, false);
            return raise_pot_fraction(view, size, equity, "postflop:bluff_raise");
        }
    }
    // 能 check 就 check（无需跟注的情形已在上面分支处理；这里 to_call>0 故 check 不可用）。
    if view.valid.can_check {
        return Decision {
            action: DecisionAction::Check,
            equity,
            source: "postflop:check_back".into(),
            probs: vec![("check".into(), 1.0)],
        };
    }
    Decision {
        action: DecisionAction::Fold,
        equity,
        source: "postflop:fold".into(),
        probs: vec![("fold".into(), 1.0)],
    }
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
    }
}
