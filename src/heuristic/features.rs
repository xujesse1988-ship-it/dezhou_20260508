//! 翻后特征：made-hand 分类、听牌、board 质地。
//!
//! 这些特征不替代 equity（equity 是主货币），而是给 policy 区分**同样 equity 的不同性质**：
//! 「价值（made 强）」vs「半诈唬（听牌，落后但有反超）」vs「空气（无 made 无听牌）」——
//! 决定下注/过牌、尺寸、诈唬频率。

use crate::core::Card;
use crate::eval::{HandCategory, HandEvaluator, NaiveHandEvaluator};

/// made-hand 档位（翻后）。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MadeClass {
    /// 高牌、无自己的对（含纯 board 对）。
    Air,
    /// 底对 / 低于面的口袋对。
    WeakPair,
    /// 中对。
    MidPair,
    /// 顶对 / 超对。
    TopPair,
    /// 两对 / 三条。
    Strong,
    /// 顺子及以上（顺/同花/葫芦/四条/同花顺）。
    Monster,
}

impl MadeClass {
    /// 用于排序/阈值的序号（越大越强）。
    pub fn rank(self) -> u8 {
        match self {
            MadeClass::Air => 0,
            MadeClass::WeakPair => 1,
            MadeClass::MidPair => 2,
            MadeClass::TopPair => 3,
            MadeClass::Strong => 4,
            MadeClass::Monster => 5,
        }
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct DrawInfo {
    pub flush_draw: bool,
    pub oesd: bool,
    pub gutshot: bool,
    pub overcards: u8,
    pub combo_draw: bool,
}

impl DrawInfo {
    /// 是否有「值得半诈唬」的听牌势能。
    pub fn has_meaningful_draw(self) -> bool {
        self.flush_draw || self.oesd || self.combo_draw
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct Texture {
    pub paired: bool,
    pub two_tone: bool,
    pub monotone: bool,
    pub connectedness: f64,
    pub high_card: u8,
    /// 综合湿度 [0,1]：越高越湿（c-bet 频率/尺寸调制）。
    pub wetness: f64,
}

fn best_category(hole: [Card; 2], board: &[Card]) -> HandCategory {
    let ev = NaiveHandEvaluator;
    match board.len() {
        3 => {
            let c = [hole[0], hole[1], board[0], board[1], board[2]];
            ev.eval5(&c).category()
        }
        4 => {
            let c = [hole[0], hole[1], board[0], board[1], board[2], board[3]];
            ev.eval6(&c).category()
        }
        5 => {
            let c = [
                hole[0], hole[1], board[0], board[1], board[2], board[3], board[4],
            ];
            ev.eval7(&c).category()
        }
        _ => HandCategory::HighCard,
    }
}

/// board 各 rank 去重降序。
fn board_ranks_desc(board: &[Card]) -> Vec<u8> {
    let mut rs: Vec<u8> = board.iter().map(|c| c.to_u8() / 4).collect();
    rs.sort_unstable_by(|a, b| b.cmp(a));
    rs.dedup();
    rs
}

/// made-hand 分类。board.len() ∈ {3,4,5}。
pub fn made_class(hole: [Card; 2], board: &[Card]) -> MadeClass {
    if board.len() < 3 {
        return MadeClass::Air;
    }
    let cat = best_category(hole, board);
    match cat {
        HandCategory::Straight
        | HandCategory::Flush
        | HandCategory::FullHouse
        | HandCategory::Quads
        | HandCategory::StraightFlush
        | HandCategory::RoyalFlush => MadeClass::Monster,
        HandCategory::Trips | HandCategory::TwoPair => MadeClass::Strong,
        HandCategory::OnePair => classify_pair(hole, board),
        HandCategory::HighCard => MadeClass::Air,
    }
}

fn classify_pair(hole: [Card; 2], board: &[Card]) -> MadeClass {
    let r0 = hole[0].to_u8() / 4;
    let r1 = hole[1].to_u8() / 4;
    let ranks = board_ranks_desc(board);
    let top = ranks.first().copied().unwrap_or(0);
    let second = ranks.get(1).copied().unwrap_or(0);

    if r0 == r1 {
        // 口袋对。
        let p = r0;
        if p > top {
            return MadeClass::TopPair; // 超对
        }
        if p > second {
            return MadeClass::MidPair;
        }
        return MadeClass::WeakPair;
    }
    // 与面成对：找命中的 board rank。
    let hits_top = r0 == top || r1 == top;
    let hits_second = r0 == second || r1 == second;
    let hits_any = ranks.iter().any(|&br| br == r0 || br == r1);
    if hits_top {
        MadeClass::TopPair
    } else if hits_second {
        MadeClass::MidPair
    } else if hits_any {
        MadeClass::WeakPair
    } else {
        // OnePair 但来自 board 对、我无参与 → 当空气（仅高牌价值）。
        MadeClass::Air
    }
}

fn has_straight(present: &[bool; 13]) -> bool {
    // wheel：A2345 = ranks 12,0,1,2,3。
    if present[12] && present[0] && present[1] && present[2] && present[3] {
        return true;
    }
    let mut run = 0;
    for &p in present.iter() {
        if p {
            run += 1;
            if run >= 5 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// 完成顺子的不同 rank 数（outs，按 rank 计）。
fn straight_completing_ranks(ranks: &[u8]) -> u32 {
    let mut present = [false; 13];
    for &r in ranks {
        present[r as usize] = true;
    }
    if has_straight(&present) {
        return 0; // 已成顺，不算听牌
    }
    let mut outs = 0;
    for cand in 0..13u8 {
        if present[cand as usize] {
            continue;
        }
        let mut p = present;
        p[cand as usize] = true;
        if has_straight(&p) {
            outs += 1;
        }
    }
    outs
}

/// 听牌检测。board.len() ∈ {3,4}（river 无听牌可言）。
pub fn draws(hole: [Card; 2], board: &[Card]) -> DrawInfo {
    let mut info = DrawInfo::default();
    if board.len() >= 5 || board.len() < 3 {
        // river 或异常：仍报 overcards 供参考。
        if !board.is_empty() {
            let top = board.iter().map(|c| c.to_u8() / 4).max().unwrap_or(0);
            let o = (hole[0].to_u8() / 4 > top) as u8 + (hole[1].to_u8() / 4 > top) as u8;
            info.overcards = o;
        }
        return info;
    }

    // flush draw：某花色（hole+board）计数==4 且至少一张 hole 是该花色。
    let mut suit_count = [0u8; 4];
    let mut hole_suit = [false; 4];
    for c in hole.iter() {
        suit_count[(c.to_u8() % 4) as usize] += 1;
        hole_suit[(c.to_u8() % 4) as usize] = true;
    }
    for c in board.iter() {
        suit_count[(c.to_u8() % 4) as usize] += 1;
    }
    for s in 0..4 {
        if suit_count[s] == 4 && hole_suit[s] {
            info.flush_draw = true;
        }
    }

    // straight draw：用全部牌的 rank 集合。
    let mut all_ranks: Vec<u8> = board.iter().map(|c| c.to_u8() / 4).collect();
    all_ranks.push(hole[0].to_u8() / 4);
    all_ranks.push(hole[1].to_u8() / 4);
    let outs = straight_completing_ranks(&all_ranks);
    if outs >= 2 {
        info.oesd = true;
    } else if outs == 1 {
        info.gutshot = true;
    }
    info.combo_draw = info.flush_draw && (info.oesd || info.gutshot);

    let top = board.iter().map(|c| c.to_u8() / 4).max().unwrap_or(0);
    info.overcards = (hole[0].to_u8() / 4 > top) as u8 + (hole[1].to_u8() / 4 > top) as u8;
    info
}

/// board 质地。board 可为 3/4/5 张。
pub fn texture(board: &[Card]) -> Texture {
    let mut t = Texture::default();
    if board.is_empty() {
        return t;
    }
    let ranks: Vec<u8> = board.iter().map(|c| c.to_u8() / 4).collect();
    let mut suit_count = [0u8; 4];
    for c in board.iter() {
        suit_count[(c.to_u8() % 4) as usize] += 1;
    }
    // 对子面。
    let mut rank_count = [0u8; 13];
    for &r in &ranks {
        rank_count[r as usize] += 1;
    }
    t.paired = rank_count.iter().any(|&c| c >= 2);
    let max_suit = *suit_count.iter().max().unwrap_or(&0);
    t.monotone = max_suit >= 3;
    t.two_tone = max_suit == 2;
    t.high_card = *ranks.iter().max().unwrap_or(&0);

    // 连张程度：取去重排序后相邻最小三张的跨度。
    let mut uniq = ranks.clone();
    uniq.sort_unstable();
    uniq.dedup();
    let connectedness = if uniq.len() >= 2 {
        let span = (uniq[uniq.len() - 1] - uniq[0]) as f64;
        // 3 张牌跨度 2(连张) → 1.0；跨度 ≥ 8 → 0。
        (1.0 - (span - 2.0).max(0.0) / 8.0).clamp(0.0, 1.0)
    } else {
        0.0
    };
    t.connectedness = connectedness;

    // 综合湿度。
    let mut wet = 0.0;
    if t.monotone {
        wet += 0.55;
    } else if t.two_tone {
        wet += 0.30;
    }
    wet += 0.45 * connectedness;
    if t.paired {
        // 对子面整体偏干（少听牌组合），略降。
        wet -= 0.10;
    }
    t.wetness = wet.clamp(0.0, 1.0);
    t
}
