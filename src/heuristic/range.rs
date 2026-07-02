//! 手写贝叶斯对手 range 追踪器（"B2b" RangeVec）——Python `tools/range_baselines.py`
//! 与 `tools/range_featurize.py` 因果 m_prior 的 Rust 逐义移植（字节级对拍目标见
//! `tests/heuristic_range_parity.rs` 与 `docs/opponent_range_ml_design.md` M3）。
//!
//! 主结构：
//!   - [`RangeModel`]：持有 169 强度表 + 角色模板阈值 + per-class combo 枚举，提供
//!     模板先验 [`RangeModel::prior`]、逐动作似然算子 [`RangeModel::op`]、
//!     value/draw 轴 [`RangeModel::class_axes`]、以及**因果 m_prior 回放**
//!     [`RangeModel::causal_logpriors`]（按样本事件流重放，复刻 Python featurizer）。
//!
//! 索引口径与 [`super::equity`] 严格同轴：`card_id = rank*4 + suit`（rank 0..12 = 2..A，
//! suit cdhs = 0..3）；`canonical_index`：pair 0..12 / suited 13..90 / offsuit 91..168。

use crate::core::Card;
use crate::heuristic::equity::{canonical_index, EquityEngine};
use crate::heuristic::features::{draws, made_class};

// ===========================================================================
// 常量（B2_PARAMS 的 Rust 侧；见 range_baselines.py:41）
// ===========================================================================

/// 激进动作集合（bet/raise/all-in 别名）。
const AGGR: [&str; 4] = ["bet", "raise", "allin", "all_in"];

/// 每街需要的 board 张数（街 0..3 = 翻前/翻牌/转牌/河牌）。
const STREET_NEED: [usize; 4] = [0, 3, 4, 5];

/// token 流截断上限（range_featurize.py:45 `MAXTOK`）。
const MAXTOK: usize = 96;

/// 角色模板下标：0=3bet / 1=open / 2=coldcall / 3=limp / 4=bbcheck。
const N_ROLES: usize = 5;
/// frac → top-fraction 阈值。
const TPL_FRAC: [f64; N_ROLES] = [0.08, 0.17, 0.34, 0.55, 0.80];
/// cap_frac → 顶部截断阈值（None=不截断）。
const TPL_CAP_FRAC: [Option<f64>; N_ROLES] = [None, None, Some(0.05), Some(0.10), Some(0.12)];
/// logistic 软度（强度尺）。
const TPL_K: [f64; N_ROLES] = [0.03, 0.03, 0.04, 0.05, 0.06];
/// air_floor。
const TPL_AIR: [f64; N_ROLES] = [0.015, 0.02, 0.05, 0.10, 0.15];

/// 似然地板（range_baselines.py:60 `lik_floor`）。
const LIK_FLOOR: f64 = 1e-4;

/// 翻前角色（对目标座位）。
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum Role {
    ThreeBet = 0,
    Open = 1,
    ColdCall = 2,
    Limp = 3,
    BbCheck = 4,
}

// ===========================================================================
// 样本 / 事件（golden 的 "s" 字典逐字段；serde 直读）
// ===========================================================================

/// 单条事件（causal order）。字段与 golden `s["ev"][i]` 同名。
#[derive(serde::Deserialize, Clone)]
pub struct Event {
    /// 街 0..3。
    pub st: i64,
    /// 座位。
    pub s: i64,
    /// 动作字符串。
    pub a: String,
    /// 本街累计承诺（动作后）。
    pub c: i64,
    /// pot（动作后）。
    pub p: i64,
    /// to-call（动作前）。
    #[serde(default)]
    pub tc: i64,
    /// high-to-beat（动作前）。
    pub hb: i64,
}

/// 对手生命画像统计（`s["prof"]["life"]`）；缺字段 → 0。
#[derive(serde::Deserialize, Default, Clone)]
pub struct LifeStats {
    #[serde(default)]
    pub hands: f64,
    #[serde(default)]
    pub vpip_opp: f64,
    #[serde(default)]
    pub vpip_cnt: f64,
    #[serde(default)]
    pub pfr_opp: f64,
    #[serde(default)]
    pub pfr_cnt: f64,
    #[serde(default)]
    pub aggr_actions: f64,
    #[serde(default)]
    pub call_actions: f64,
}

/// 对手画像（只需 `life`）。
#[derive(serde::Deserialize, Default, Clone)]
pub struct Prof {
    #[serde(default)]
    pub life: LifeStats,
}

/// 单手样本（golden `hands[i]["s"]`）。
#[derive(serde::Deserialize, Clone)]
pub struct Sample {
    /// SB 座位。
    pub sb: i64,
    /// BB 座位。
    pub bb: i64,
    /// SB 盲注筹码。
    pub sbamt: i64,
    /// BB 盲注筹码。
    pub bbamt: i64,
    /// 目标对手座位（range 说的就是这个座位）。
    pub tseat: i64,
    /// hero 座位（或 null）。
    #[serde(default)]
    pub hseat: Option<i64>,
    /// hero 底牌（或 null）。
    #[serde(default)]
    pub hhole: Option<Vec<String>>,
    /// 最终 board（0..5 张）。
    #[serde(default)]
    pub board: Vec<String>,
    /// 事件流。
    pub ev: Vec<Event>,
    /// 画像。
    #[serde(default)]
    pub prof: Prof,
}

// ===========================================================================
// 基元
// ===========================================================================

/// 卡串 → id（rank*4 + suit）。大小写宽容（同 Python `card_id`）。
pub fn card_id(s: &str) -> u8 {
    let b = s.as_bytes();
    let rank = match b[0].to_ascii_uppercase() {
        b'2' => 0,
        b'3' => 1,
        b'4' => 2,
        b'5' => 3,
        b'6' => 4,
        b'7' => 5,
        b'8' => 6,
        b'9' => 7,
        b'T' => 8,
        b'J' => 9,
        b'Q' => 10,
        b'K' => 11,
        b'A' => 12,
        _ => 0,
    };
    let suit = match b[1].to_ascii_lowercase() {
        b'c' => 0,
        b'd' => 1,
        b'h' => 2,
        b's' => 3,
        _ => 0,
    };
    rank * 4 + suit
}

/// logistic（±40 clamp，range_baselines.py:210）。
#[inline]
fn logistic(x: f64) -> f64 {
    if x < -40.0 {
        0.0
    } else if x > 40.0 {
        1.0
    } else {
        1.0 / (1.0 + (-x).exp())
    }
}

/// 画像分类（range_ml_common.py:314 / opponent.rs:113 移植）。
fn classify(life: &LifeStats) -> &'static str {
    // `not stats or hands<18 → unknown`：空 dict → hands 缺省 0 < 18，等价单条判断。
    if life.hands < 18.0 {
        return "unknown";
    }
    let vpip = if life.vpip_opp != 0.0 {
        life.vpip_cnt / life.vpip_opp
    } else {
        0.25
    };
    let pfr = if life.pfr_opp != 0.0 {
        life.pfr_cnt / life.pfr_opp
    } else {
        0.15
    };
    let af = if life.call_actions != 0.0 {
        life.aggr_actions / life.call_actions
    } else {
        1.5
    };
    let gap = (vpip - pfr).max(0.0);
    if vpip < 0.16 {
        return "nit";
    }
    if vpip > 0.45 && af < 1.5 {
        return "station";
    }
    if af > 3.5 || (vpip > 0.40 && pfr > 0.30) {
        return "maniac";
    }
    if gap > 0.18 && vpip > 0.30 {
        return "lag";
    }
    "tag"
}

/// 画像对 range 阈值的平移（range_ml_common.py:336）。
fn range_threshold_shift(life: &LifeStats) -> f64 {
    let cap = 0.12;
    match classify(life) {
        "nit" => cap,
        "station" => -cap,
        "maniac" => -cap * 0.8,
        "lag" => -cap * 0.4,
        _ => 0.0,
    }
}

/// draw_score（range_ml_common.py:249）。
#[inline]
fn draw_score(fd: bool, oesd: bool, gut: bool, combo: bool) -> f64 {
    let d = 0.9 * (combo as u8 as f64)
        + 0.6 * (fd as u8 as f64)
        + 0.5 * (oesd as u8 as f64)
        + 0.25 * (gut as u8 as f64);
    d.min(1.0)
}

/// CLASS_COMBOS[idx] = 该 169 类全部 concrete combo `(a,b)`（a<b）。
fn build_class_combos() -> Vec<Vec<(u8, u8)>> {
    let mut out: Vec<Vec<(u8, u8)>> = vec![Vec::new(); 169];
    for a in 0..52u8 {
        for b in (a + 1)..52u8 {
            let idx = canonical_index(
                Card::from_u8(a).expect("card a<52"),
                Card::from_u8(b).expect("card b<52"),
            );
            out[idx].push((a, b));
        }
    }
    out
}

// ===========================================================================
// RangeModel
// ===========================================================================

/// 因果快照结果：每街 0..3 的街末因果 log m_prior（f32 舍入回 f64，与 featurizer
/// 存储口径一致）；`None` = 该街无快照。
pub struct CausalResult {
    pub street_logprior: [Option<[f64; 169]>; 4],
}

/// B2b range 模型。
pub struct RangeModel {
    strength: [f64; 169],
    /// 每角色的基础阈值（未含画像平移）。
    s0: [f64; N_ROLES],
    /// 每角色的顶部截断阈值。
    cap: [Option<f64>; N_ROLES],
    /// 每 169 类的 concrete combo 枚举。
    class_combos: Vec<Vec<(u8, u8)>>,
}

impl RangeModel {
    /// 从 equity 引擎构建（复用其 `strength` + `threshold_for_top`）。
    pub fn new(engine: &EquityEngine) -> RangeModel {
        let strength = engine.strength;
        let mut s0 = [0.0f64; N_ROLES];
        let mut cap: [Option<f64>; N_ROLES] = [None; N_ROLES];
        for i in 0..N_ROLES {
            s0[i] = engine.threshold_for_top(TPL_FRAC[i]);
            cap[i] = TPL_CAP_FRAC[i].map(|cf| engine.threshold_for_top(cf));
        }
        RangeModel {
            strength,
            s0,
            cap,
            class_combos: build_class_combos(),
        }
    }

    /// 模板先验（未归一 169 向量，range_baselines.py:229）。
    pub fn prior(&self, role: Role, life: &LifeStats) -> [f64; 169] {
        let ri = role as usize;
        let s0 = self.s0[ri] + range_threshold_shift(life);
        let cap_top = self.cap[ri];
        let k = TPL_K[ri];
        let air = TPL_AIR[ri];
        let mut out = [0.0f64; 169];
        for (o, &sv) in out.iter_mut().zip(self.strength.iter()) {
            let incl = logistic((sv - s0) / k);
            let cap = match cap_top {
                Some(ct) => 1.0 - logistic((sv - ct) / k),
                None => 1.0,
            };
            *o = incl * cap + air;
        }
        out
    }

    /// 单动作似然算子（range_baselines.py:241）；`None` = fold/未知（不施算子）。
    pub fn op(&self, e: &Event, v: &[f64; 169], d: &[f64; 169]) -> Option<[f64; 169]> {
        let a = e.a.as_str();
        let is_aggr = AGGR.contains(&a);
        let aggressive = is_aggr && e.c > e.hb;
        let mut l = [0.0f64; 169];
        if aggressive {
            let beta = (e.c as f64 / (e.p.max(1) as f64)).min(1.5);
            if e.hb == 0 {
                // first-in bet：a_v=1.0 / a_d=0.30 / eps=0.05 / κ=k0+k1·β。
                let kappa = 1.1 + 1.2 * beta;
                for k in 0..169 {
                    l[k] = v[k].powf(kappa) + 0.30 * d[k].powf(kappa) + 0.05;
                }
            } else {
                // raise：a_v=1.0 / a_d=0.25 / eps=0.03 / κ=k0+k1·β+extra（k0/k1 取 bet）。
                let kappa = 1.1 + 1.2 * beta + 0.8;
                for k in 0..169 {
                    l[k] = v[k].powf(kappa) + 0.25 * d[k].powf(kappa) + 0.03;
                }
            }
        } else if a == "call" || (is_aggr && e.c <= e.hb) {
            // call（含短筹码 all-in call）。
            let inv2s2 = 1.0 / (2.0 * 0.22 * 0.22);
            for k in 0..169 {
                let mut x = (-((v[k] - 0.45).powi(2)) * inv2s2).exp() + 0.60 * d[k];
                if v[k] > 0.9 {
                    x -= 0.35;
                }
                if v[k] < 0.1 && d[k] < 0.15 {
                    x -= 0.55;
                }
                l[k] = x.max(0.02);
            }
        } else if a == "check" {
            for k in 0..169 {
                let tent = (1.0 - 2.0 * (v[k] - 0.5).abs()).max(0.0);
                let mut x = 0.35 + 0.50 * tent;
                if v[k] < 0.15 && d[k] < 0.2 {
                    x += 0.40;
                }
                l[k] = x;
            }
        } else {
            // fold 或未知：不施算子。
            return None;
        }
        for lk in &mut l {
            *lk = lk.max(LIK_FLOOR);
        }
        Some(l)
    }

    /// 每 board 前缀 + 死牌掩码的 per-class value/draw 轴（range_ml_common.py:347）。
    pub fn class_axes(&self, board_prefix: &[u8], dead_mask: u64) -> ([f64; 169], [f64; 169]) {
        let mut v = [0.0f64; 169];
        let mut d = [0.0f64; 169];
        let board: Vec<Card> = board_prefix
            .iter()
            .map(|&c| Card::from_u8(c).expect("board card<52"))
            .collect();
        for idx in 0..169 {
            let mut sv = 0.0f64;
            let mut sd = 0.0f64;
            let mut n = 0u32;
            for &(a, b) in &self.class_combos[idx] {
                if dead_mask & (1u64 << a) != 0 || dead_mask & (1u64 << b) != 0 {
                    continue;
                }
                n += 1;
                let hole = [
                    Card::from_u8(a).expect("combo a<52"),
                    Card::from_u8(b).expect("combo b<52"),
                ];
                sv += made_class(hole, &board).rank() as f64 / 5.0;
                let dr = draws(hole, &board);
                sd += draw_score(dr.flush_draw, dr.oesd, dr.gutshot, dr.combo_draw);
            }
            if n > 0 {
                v[idx] = sv / n as f64;
                d[idx] = sd / n as f64;
            }
        }
        (v, d)
    }

    /// 因果 m_prior 回放：忠实复刻 `range_featurize.py` `SampleFeaturizer.run` 的
    /// token 流 + snaps + f32 log prior 存储，返回每街街末快照的 log m_prior。
    pub fn causal_logpriors(&self, s: &Sample) -> CausalResult {
        let bb = s.bbamt.max(1);
        let tseat = s.tseat;

        // 观察者死牌：hero 底牌（仅当 hero 非目标且有 2 张）。
        let obs_hole: Vec<u8> = match (&s.hhole, s.hseat) {
            (Some(h), Some(hs)) if h.len() == 2 && hs != tseat => {
                h.iter().map(|c| card_id(c)).collect()
            }
            _ => Vec::new(),
        };
        let board_ids: Vec<u8> = s.board.iter().map(|c| card_id(c)).collect();
        // 最深已知 board 街。
        let mut max_bstreet: i64 = 0;
        for st in [1usize, 2, 3] {
            if board_ids.len() >= STREET_NEED[st] {
                max_bstreet = st as i64;
            }
        }
        let life = s.prof.life.clone();

        // --- 因果 prior 状态 ---
        // commits：(street, seat) → 本街累计（用小 Vec 线性查，规模极小）。
        let mut commits: Vec<((i64, i64), i64)> = Vec::new();
        set_commit(&mut commits, 0, s.sb, s.sbamt);
        set_commit(&mut commits, 0, s.bb, s.bbamt);
        let mut npr_sofar: i64 = 0;
        let mut pagg_sofar: Option<i64> = None;
        let mut tgt_folded = false;
        let mut op_prod = [1.0f64; 169];
        let mut cur_role: Option<Role> = None;
        let mut base_prior = [0.0f64; 169];
        let mut cur_pidx: usize = 0;

        // 去重 prior 表（f32 舍入 log 向量）。
        let mut priors: Vec<[f64; 169]> = Vec::new();
        // token 流：(clamped_street, pidx)。
        let mut tokens: Vec<(i64, usize)> = Vec::new();

        // 初始角色。
        let r0 = role_now(npr_sofar, pagg_sofar, tseat, bb, &commits);
        set_role(
            self,
            r0,
            &life,
            &mut cur_role,
            &mut base_prior,
            &op_prod,
            &mut priors,
            &mut cur_pidx,
        );

        // CTX + PROF token（街 0）。
        tokens.push((0, cur_pidx));
        tokens.push((0, cur_pidx));

        let mut last_st: i64 = 0;
        for e in &s.ev {
            let st = last_st.max(e.st.min(3));
            // 新翻开的街的 board token。
            while last_st < st {
                last_st += 1;
                if last_st <= max_bstreet {
                    let need_lo = STREET_NEED[(last_st - 1) as usize];
                    let need_hi = STREET_NEED[last_st as usize];
                    for _ in need_lo..need_hi {
                        tokens.push((last_st, cur_pidx));
                    }
                }
            }
            let seat = e.s;
            let act = e.a.as_str();
            set_commit(&mut commits, st, seat, e.c);
            let is_aggr = AGGR.contains(&act);
            if st == 0 && is_aggr {
                npr_sofar += 1;
                pagg_sofar = Some(seat);
            }
            // 因果 prior 更新（目标 fold 后冻结）。
            if !tgt_folded {
                if seat == tseat && act == "fold" {
                    tgt_folded = true;
                } else if st == 0 {
                    let r = role_now(npr_sofar, pagg_sofar, tseat, bb, &commits);
                    set_role(
                        self,
                        r,
                        &life,
                        &mut cur_role,
                        &mut base_prior,
                        &op_prod,
                        &mut priors,
                        &mut cur_pidx,
                    );
                } else if seat == tseat && st <= max_bstreet {
                    let need = STREET_NEED[st as usize];
                    let board_prefix = &board_ids[..need];
                    let mut dead_mask = 0u64;
                    for &c in &obs_hole {
                        dead_mask |= 1u64 << c;
                    }
                    for &c in board_prefix {
                        dead_mask |= 1u64 << c;
                    }
                    let (v, d) = self.class_axes(board_prefix, dead_mask);
                    if let Some(l) = self.op(e, &v, &d) {
                        for i in 0..169 {
                            op_prod[i] *= l[i];
                        }
                        cur_pidx = push_prior(&mut priors, &base_prior, &op_prod);
                    }
                }
            }
            // 动作 token。
            let clamped = if st <= max_bstreet { st } else { max_bstreet };
            tokens.push((clamped, cur_pidx));
        }
        // 收尾：有 board 但无事件的街的 board token。
        while last_st < max_bstreet {
            last_st += 1;
            let need_lo = STREET_NEED[(last_st - 1) as usize];
            let need_hi = STREET_NEED[last_st as usize];
            for _ in need_lo..need_hi {
                tokens.push((last_st, cur_pidx));
            }
        }

        // 截断（range_featurize.py:323）。
        if tokens.len() > MAXTOK {
            tokens.truncate(MAXTOK);
        }

        // 街末快照：snaps[S] = 最后一个 clamped-street <= S 的 token。
        let mut out: [Option<[f64; 169]>; 4] = [None, None, None, None];
        for (s_idx, slot) in out.iter_mut().enumerate() {
            if board_ids.len() < STREET_NEED[s_idx] {
                continue;
            }
            let mut snap: Option<usize> = None;
            for (i, &(stk, _)) in tokens.iter().enumerate() {
                if stk <= s_idx as i64 {
                    snap = Some(i);
                }
            }
            if let Some(spi) = snap {
                let pidx = tokens[spi].1;
                *slot = Some(priors[pidx]);
            }
        }
        CausalResult {
            street_logprior: out,
        }
    }
}

// --- causal 回放辅助 ---

/// commits[(st,seat)] = v（线性查小表）。
fn set_commit(commits: &mut Vec<((i64, i64), i64)>, st: i64, seat: i64, v: i64) {
    for entry in commits.iter_mut() {
        if entry.0 == (st, seat) {
            entry.1 = v;
            return;
        }
    }
    commits.push(((st, seat), v));
}

/// commits.get((st,seat), 0)。
fn get_commit(commits: &[((i64, i64), i64)], st: i64, seat: i64) -> i64 {
    for entry in commits {
        if entry.0 == (st, seat) {
            return entry.1;
        }
    }
    0
}

/// role_now（range_featurize.py:188）。
fn role_now(
    npr: i64,
    pagg: Option<i64>,
    tseat: i64,
    bb: i64,
    commits: &[((i64, i64), i64)],
) -> Role {
    if pagg == Some(tseat) {
        return if npr >= 2 { Role::ThreeBet } else { Role::Open };
    }
    let cbb = get_commit(commits, 0, tseat) as f64 / bb as f64;
    if cbb >= 2.5 {
        Role::ColdCall
    } else if cbb >= 1.5 {
        Role::Limp
    } else {
        Role::BbCheck
    }
}

/// push_prior（range_featurize.py:194）：vec = base_prior · op_prod → log → f32 舍入。
fn push_prior(
    priors: &mut Vec<[f64; 169]>,
    base_prior: &[f64; 169],
    op_prod: &[f64; 169],
) -> usize {
    let mut lp = [0.0f64; 169];
    for i in 0..169 {
        let vec = base_prior[i] * op_prod[i];
        let logv = vec.max(1e-30).ln();
        // featurizer 存 float32 → 回读时 widen 到 f64，此处照做以字节对齐。
        lp[i] = logv as f32 as f64;
    }
    priors.push(lp);
    priors.len() - 1
}

/// set_role（range_featurize.py:200）：角色变则重算 base_prior 并 push。
#[allow(clippy::too_many_arguments)]
fn set_role(
    model: &RangeModel,
    r: Role,
    life: &LifeStats,
    cur_role: &mut Option<Role>,
    base_prior: &mut [f64; 169],
    op_prod: &[f64; 169],
    priors: &mut Vec<[f64; 169]>,
    cur_pidx: &mut usize,
) {
    if *cur_role != Some(r) {
        *cur_role = Some(r);
        *base_prior = model.prior(r, life);
        *cur_pidx = push_prior(priors, base_prior, op_prod);
    }
}
