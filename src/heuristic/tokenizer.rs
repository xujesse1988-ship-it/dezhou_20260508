//! 每 token 72 维特征构造——`tools/range_featurize.py` `SampleFeaturizer.run` 的
//! **token/feat 发射**部分的 Rust 逐义移植（字节级对拍目标见
//! `tests/heuristic_tokenizer_parity.rs`）。
//!
//! 只负责 `feat` 张量：因果 prior / alive / snaps 的回放在
//! [`super::range::RangeModel::causal_logpriors`] 中已实现，这里**不重复**——两者共用
//! 完全相同的 token 流结构（CTX、PROF、逐街 board token、逐事件 action token、收尾
//! board、MAXTOK 截断），保证 `feat[t]` 与 range 侧 `pidx[t]` 逐 token 对齐。
//!
//! 特征布局（FDIM=72，无关组置零），组顺序与 `range_featurize.py:64-72` 一致：
//!   ttype@0(4) · street@4(4) · ctx@8(10) · prof@18(15) · act@33(20) · board@53(19)。

use std::collections::HashMap;

use crate::heuristic::range::{card_id, Prof, Sample};

/// 特征维度（`range_featurize.py` `FDIM`）。
pub const FDIM: usize = 72;

/// token 流截断上限（`range_featurize.py:45 MAXTOK`）。
const MAXTOK: usize = 96;

/// 每街需要的 board 张数（街 0..3）。
const STREET_NEED: [usize; 4] = [0, 3, 4, 5];

/// 激进动作集合（`range_featurize.py:42 AGGR`）。
const AGGR: [&str; 4] = ["bet", "raise", "allin", "all_in"];

// --- 特征组 flat 偏移（`range_featurize.py:64-72 _IDX`）---
const O_STREET: usize = 4;
const O_CTX: usize = 8;
const O_PROF: usize = 18;
const O_ACT: usize = 33;
const O_BOARD: usize = 53;

// --- ttype one-hot（组 @0）---
const TT_CTX: usize = 0;
const TT_PROF: usize = 1;
const TT_ACT: usize = 2;
const TT_BOARD: usize = 3;

// --- 位置桶（`range_featurize.py:74`）---
const POS_SB: usize = 0;
const POS_BB: usize = 1;
const POS_EARLY: usize = 2;
const POS_MID: usize = 3;
const POS_LATE: usize = 4;

/// 动作类型下标（`range_featurize.py:73 ACT_TYPES`）；未知 → 1（check）。
fn act_type(a: &str) -> usize {
    match a {
        "fold" => 0,
        "check" => 1,
        "call" => 2,
        "bet" => 3,
        "raise" => 4,
        "allin" | "all_in" => 5,
        _ => 1,
    }
}

/// 位置桶 0..4（`range_featurize.py:77 pos_bucket_idx`，环取在手座位）。
fn pos_bucket_idx(seat: i64, s: &Sample) -> usize {
    if seat == s.sb {
        return POS_SB;
    }
    if seat == s.bb {
        return POS_BB;
    }
    let mut ring: Vec<i64> = s.inh.clone();
    ring.sort_unstable();
    let n = ring.len();
    let btn = s.btn;
    let btn_in_ring = btn.map(|b| ring.contains(&b)).unwrap_or(false);
    let seat_in_ring = ring.contains(&seat);

    let idx: usize = if btn_in_ring {
        if seat_in_ring {
            let b = btn.expect("btn_in_ring 保证 Some");
            let seat_pos = ring.iter().position(|&x| x == seat).expect("seat in ring") as i64;
            let btn_pos = ring.iter().position(|&x| x == b).expect("btn in ring") as i64;
            (seat_pos - btn_pos).rem_euclid(n as i64) as usize
        } else {
            2
        }
    } else if !seat_in_ring {
        2
    } else {
        // sorted(ring, key=lambda x: ((x - (btn or 0)) % 32, x)).index(seat)
        let btn0 = btn.unwrap_or(0);
        let mut sr = ring.clone();
        sr.sort_by_key(|&a| ((a - btn0).rem_euclid(32), a));
        sr.iter().position(|&x| x == seat).expect("seat in ring")
    };

    if idx == 0 || (n > 0 && idx == n - 1) {
        POS_LATE
    } else if idx >= n.saturating_sub(3) {
        POS_MID
    } else {
        POS_EARLY
    }
}

/// 15 维画像特征（`range_featurize.py:97 prof_features`）：life/w50 双通道各 7 + n_shows。
fn prof_features(prof: &Prof) -> [f64; 15] {
    let mut out = [0.0f64; 15];
    let mut i = 0usize;
    for st in [&prof.life, &prof.w50] {
        let (vo, vc) = (st.vpip_opp, st.vpip_cnt);
        let (po, pc) = (st.pfr_opp, st.pfr_cnt);
        let (ag, ca) = (st.aggr_actions, st.call_actions);
        out[i] = (st.hands.ln_1p() / 7.0).min(1.0);
        out[i + 1] = if vo != 0.0 { vc / vo } else { 0.25 };
        out[i + 2] = (vo.ln_1p() / 6.0).min(1.0);
        out[i + 3] = if po != 0.0 { pc / po } else { 0.15 };
        out[i + 4] = (po.ln_1p() / 6.0).min(1.0);
        out[i + 5] = if (ag + ca) != 0.0 {
            ag / (ag + ca)
        } else {
            0.5
        };
        out[i + 6] = ((ag + ca).ln_1p() / 6.0).min(1.0);
        i += 7;
    }
    out[14] = (prof.n_shows.ln_1p() / 4.0).min(1.0);
    out
}

/// board 质地（`range_featurize.py:119 board_texture`），只返回 token 用的
/// (paired, suitmax/5)。
fn board_texture_pt(board_ids: &[u8]) -> (f64, f64) {
    if board_ids.is_empty() {
        return (0.0, 0.0);
    }
    let mut suit_count = [0i32; 4];
    for &c in board_ids {
        suit_count[(c % 4) as usize] += 1;
    }
    let sc = suit_count.iter().copied().max().unwrap_or(0);
    let mut ranks: Vec<u8> = board_ids.iter().map(|&c| c / 4).collect();
    let n = ranks.len();
    ranks.sort_unstable();
    ranks.dedup();
    let paired = if ranks.len() < n { 1.0 } else { 0.0 };
    (paired, sc as f64 / 5.0)
}

/// 花色首现顺序重标号（`range_featurize.py:169 canon_suit`，每手重置）。
fn canon_suit(su: usize, map: &mut [Option<usize>; 4], next: &mut usize) -> usize {
    if let Some(v) = map[su] {
        v
    } else {
        let v = *next;
        map[su] = Some(v);
        *next += 1;
        v
    }
}

/// 空 token（ttype one-hot @0 + street one-hot @4，`range_featurize.py:212 emit`）。
fn base_token(ttype: usize, st: i64) -> [f64; FDIM] {
    let mut f = [0.0f64; FDIM];
    f[ttype] = 1.0;
    f[O_STREET + st.clamp(0, 3) as usize] = 1.0;
    f
}

/// 发一街新翻开 board 的每张牌一个 board token（`range_featurize.py:238 emit_board_street`）。
fn emit_board_street(
    st: i64,
    board_ids: &[u8],
    feats: &mut Vec<[f64; FDIM]>,
    suit_map: &mut [Option<usize>; 4],
    next_suit: &mut usize,
) {
    let need_hi = STREET_NEED[st as usize];
    let need_lo = if st > 0 {
        STREET_NEED[(st - 1) as usize]
    } else {
        0
    };
    let (paired, suitmax) = board_texture_pt(&board_ids[..need_hi]);
    for &c in &board_ids[need_lo..need_hi] {
        let mut f = base_token(TT_BOARD, st);
        f[O_BOARD + (c / 4) as usize] = 1.0;
        let cs = canon_suit((c % 4) as usize, suit_map, next_suit);
        f[O_BOARD + 13 + cs] = 1.0;
        f[O_BOARD + 17] = paired;
        f[O_BOARD + 18] = suitmax;
        feats.push(f);
    }
}

/// 单样本 → 每 token 72 维特征序列（截断到 MAXTOK），`hand.feat` 的逐义 Rust 复刻。
pub fn tokenize(s: &Sample) -> Vec<[f64; FDIM]> {
    let bb = s.bbamt.max(1);
    let tseat = s.tseat;
    let board_ids: Vec<u8> = s.board.iter().map(|c| card_id(c)).collect();
    let mut max_bstreet: i64 = 0;
    for st in [1usize, 2, 3] {
        if board_ids.len() >= STREET_NEED[st] {
            max_bstreet = st as i64;
        }
    }
    let prof_vec = prof_features(&s.prof);

    // seed_commit（=盲注），供 ctx 目标筹码与 act rem 计算。
    let seed_commit_of = |seat: i64| -> i64 {
        if seat == s.sb {
            s.sbamt
        } else if seat == s.bb {
            s.bbamt
        } else {
            0
        }
    };
    let stack_of =
        |seat: i64| -> i64 { s.stacks.get(&seat.to_string()).copied().unwrap_or(100 * bb) };

    // 逐街承诺 / 逐座位累计承诺（与 range_featurize 主循环同口径，用于 act delta/rem）。
    let mut commits: HashMap<(i64, i64), i64> = HashMap::new();
    commits.insert((0, s.sb), s.sbamt);
    commits.insert((0, s.bb), s.bbamt);
    let mut total_commit: HashMap<i64, i64> = HashMap::new();
    total_commit.insert(s.sb, s.sbamt);
    total_commit.insert(s.bb, s.bbamt);

    // 花色首现重标号（每手重置）。
    let mut suit_map: [Option<usize>; 4] = [None; 4];
    let mut next_suit = 0usize;

    let mut feats: Vec<[f64; FDIM]> = Vec::new();

    // CTX token。
    let tgt_stack_bb = (stack_of(tseat) + seed_commit_of(tseat)) as f64 / bb as f64;
    let ns = s.ns.unwrap_or(2);
    {
        let mut f = base_token(TT_CTX, 0);
        f[O_CTX] = ns as f64 / 6.0;
        f[O_CTX + 1] = if ns == 2 { 1.0 } else { 0.0 };
        f[O_CTX + 2] = ((tgt_stack_bb.max(1.0) / 100.0).log2() / 3.0).clamp(-1.0, 1.0);
        f[O_CTX + 3 + pos_bucket_idx(tseat, s)] = 1.0;
        feats.push(f);
    }

    // PROF token。
    {
        let mut f = base_token(TT_PROF, 0);
        for (k, &v) in prof_vec.iter().enumerate() {
            f[O_PROF + k] = v;
        }
        feats.push(f);
    }

    // 事件回放：先补新翻开街的 board token，再发 action token。
    let mut last_st: i64 = 0;
    for e in &s.ev {
        let st = last_st.max(e.st.min(3));
        while last_st < st {
            last_st += 1;
            if last_st <= max_bstreet {
                emit_board_street(
                    last_st,
                    &board_ids,
                    &mut feats,
                    &mut suit_map,
                    &mut next_suit,
                );
            }
        }
        let seat = e.s;
        let act = e.a.as_str();
        let cb = commits.get(&(st, seat)).copied().unwrap_or(0);
        let delta = e.c - cb;
        commits.insert((st, seat), e.c);
        let tc_seat = total_commit.get(&seat).copied().unwrap_or(0) + delta;
        total_commit.insert(seat, tc_seat);
        let pot_before = (e.p - delta).max(1) as f64;
        let rem = (stack_of(seat) - (tc_seat - seed_commit_of(seat))).max(0);

        let mut f = base_token(TT_ACT, st);
        f[O_ACT + pos_bucket_idx(seat, s)] = 1.0;
        f[O_ACT + 5] = if seat == tseat { 1.0 } else { 0.0 };
        f[O_ACT + 6 + act_type(act)] = 1.0;
        f[O_ACT + 12] = if AGGR.contains(&act) && e.c > e.hb {
            1.0
        } else {
            0.0
        };
        let c = e.c as f64;
        f[O_ACT + 13] = (c / pot_before).min(4.0) / 4.0;
        f[O_ACT + 14] = (c / pot_before).min(8.0).ln_1p() / 2.2;
        f[O_ACT + 15] = (delta as f64 / pot_before).min(4.0) / 4.0;
        f[O_ACT + 16] = (e.tc as f64 / pot_before).min(4.0) / 4.0;
        f[O_ACT + 17] = (rem as f64 / pot_before).min(25.0) / 25.0;
        f[O_ACT + 18] = ((e.p as f64 / bb as f64).ln_1p() / 6.0).min(1.0);
        f[O_ACT + 19] = (e.hb as f64 / pot_before).min(4.0) / 4.0;
        feats.push(f);
    }
    // 收尾：有 board 但无更多事件的街。
    while last_st < max_bstreet {
        last_st += 1;
        emit_board_street(
            last_st,
            &board_ids,
            &mut feats,
            &mut suit_map,
            &mut next_suit,
        );
    }

    if feats.len() > MAXTOK {
        feats.truncate(MAXTOK);
    }
    feats
}
