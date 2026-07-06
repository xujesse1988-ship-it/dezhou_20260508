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

/// 成手坚果度（只读特征，`docs/scenario_heuristic_design.md` P1-3）：hero 的 **Monster**
/// （同花/顺子）在本板上是否**非坚果**——可被更高同花/顺子压制，或成对/成花板上被葫芦压制。
///
/// **消费方**：`heuristic9_sim --field flushtrap` 造靶（villain 仅持坚果同类升级加注战）
/// 与将来的**剥削层门控**（仅对画像为「河牌极化加注」的对手压制非坚果 stack-off）。
/// 引擎 policy **不消费**它：Phase C 第二轮实测，把非坚果同花/顺子 value_raise 降级为
/// 默认改动会退化标准矩阵（对弱/被动/跟注场，非坚果价值加注被更差的牌支付 = +EV，
/// 见 `docs/heuristic9_design.md` §6 驳回记录 + 实证 hand `1b3328bc`）。
///
/// 语义：仅对「同花」「顺子」两类 Monster 判非坚果；葫芦/四条/同花顺/皇家归**坚果**
/// （返回 false，不降级——避免重蹈 PairedTrap 对成对面非坚果两对限深被驳回的语境陷阱）；
/// 非 Monster 一律返回 false。board 不足 3 张返回 false。
pub fn monster_non_nut(hole: [Card; 2], board: &[Card]) -> bool {
    if board.len() < 3 {
        return false;
    }
    match best_category(hole, board) {
        HandCategory::Flush => flush_non_nut(hole, board),
        HandCategory::Straight => straight_non_nut(hole, board),
        _ => false,
    }
}

/// hero 是否成**非坚果同花**（best category = Flush 且可被更高同花压制）——`monster_non_nut`
/// 窄化到「仅同花」的判据（不含顺子）：sim 实测非坚果顺子在干板对 Tag 风格对手 value_raise
/// 是强 +EV（更差两对/暗三/更小顺子会跟），压制反漏价值。**消费方同 `monster_non_nut`**
/// （造靶度量 + 将来剥削层门控；引擎 policy 不消费，见其文档与 §6 驳回记录）。
pub fn nonnut_flush(hole: [Card; 2], board: &[Card]) -> bool {
    board.len() >= 3
        && best_category(hole, board) == HandCategory::Flush
        && flush_non_nut(hole, board)
}

/// hero 同花是否非坚果：存在比 hero 最高同花 hole 张更高、且不在 board / 不在 hero 手里的
/// 同花色牌 → 对手可持更高同花。hero 无该花色 hole 张（board 五张同花）→ 保守判非坚果。
fn flush_non_nut(hole: [Card; 2], board: &[Card]) -> bool {
    for suit in 0..4u8 {
        let board_cnt = board.iter().filter(|c| c.to_u8() % 4 == suit).count();
        let hole_cnt = hole.iter().filter(|c| c.to_u8() % 4 == suit).count();
        if board_cnt + hole_cnt < 5 {
            continue; // 非本花色的成花
        }
        let hole_hi = hole
            .iter()
            .filter(|c| c.to_u8() % 4 == suit)
            .map(|c| c.to_u8() / 4)
            .max();
        let hero_hi = match hole_hi {
            Some(h) => h,
            None => return true, // board 五张同花、hero 无参与 → 非坚果（保守）
        };
        let on_board = |r: u8| {
            board
                .iter()
                .any(|c| c.to_u8() % 4 == suit && c.to_u8() / 4 == r)
        };
        let in_hole = |r: u8| {
            hole.iter()
                .any(|c| c.to_u8() % 4 == suit && c.to_u8() / 4 == r)
        };
        for r in (hero_hi + 1)..13u8 {
            if !on_board(r) && !in_hole(r) {
                return true; // 更高同花可能存在 → 非坚果
            }
        }
        return false; // hero 持有效坚果同花（含 board 已含更高张的情形）
    }
    false
}

/// hero 顺子是否非坚果：monotone（可成花）/ 成对（可成葫芦）板一律非坚果；否则比较
/// board-only 可支撑的最高顺子 top（对手补 ≤2 张）与 hero 实际顺子 top，对手更高 → 非坚果。
fn straight_non_nut(hole: [Card; 2], board: &[Card]) -> bool {
    let mut suit_cnt = [0u8; 4];
    let mut rank_cnt = [0u8; 13];
    for c in board {
        suit_cnt[(c.to_u8() % 4) as usize] += 1;
        rank_cnt[(c.to_u8() / 4) as usize] += 1;
    }
    if suit_cnt.iter().any(|&c| c >= 3) || rank_cnt.iter().any(|&c| c >= 2) {
        return true; // 成花板 / 成对板：顺子被 flush / boat 压
    }
    let mut board_present = [false; 13];
    for c in board {
        board_present[(c.to_u8() / 4) as usize] = true;
    }
    let mut all_present = board_present;
    for c in hole.iter() {
        all_present[(c.to_u8() / 4) as usize] = true;
    }
    match (
        highest_straight_top(&all_present),
        highest_straight_top_with_help(&board_present, 2),
    ) {
        (Some(hero_top), Some(opp_top)) => opp_top > hero_top,
        _ => false,
    }
}

/// 给定 rank-present 集合里的最高顺子 top（rank index；wheel A2345 → top=3=Five）。
fn highest_straight_top(present: &[bool; 13]) -> Option<u8> {
    for t in (4..13u8).rev() {
        if (0..5).all(|k| present[(t - k) as usize]) {
            return Some(t);
        }
    }
    if present[12] && present[0] && present[1] && present[2] && present[3] {
        return Some(3); // wheel
    }
    None
}

/// 对手能补 `help` 张（≤2）时可成的最高顺子 top（board 须提供 `5-help` 张连号）。
fn highest_straight_top_with_help(board_present: &[bool; 13], help: usize) -> Option<u8> {
    let need = 5 - help;
    for t in (4..13u8).rev() {
        let have = (0..5).filter(|k| board_present[(t - k) as usize]).count();
        if have >= need {
            return Some(t);
        }
    }
    let wheel = [12usize, 0, 1, 2, 3]
        .iter()
        .filter(|&&r| board_present[r])
        .count();
    if wheel >= need {
        return Some(3);
    }
    None
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
