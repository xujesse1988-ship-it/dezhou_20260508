//! 一次性手牌审核：A3hh 在 4s 7c 5h Jh（turn）的**精确**权益枚举。
//!
//! turn 只剩一张河牌，44 张未知牌全枚举；对手范围按显式 2-card combo 枚举。
//! 零 MC 噪声、零采样——结果是数学常数，与机器无关。
//!
//!   cargo run --release --example turn_equity_review

use poker::eval::NaiveHandEvaluator;
use poker::{Card, HandEvaluator, Rank, Suit};

const RANKS: [Rank; 13] = [
    Rank::Two,
    Rank::Three,
    Rank::Four,
    Rank::Five,
    Rank::Six,
    Rank::Seven,
    Rank::Eight,
    Rank::Nine,
    Rank::Ten,
    Rank::Jack,
    Rank::Queen,
    Rank::King,
    Rank::Ace,
];
const SUITS: [Suit; 4] = [Suit::Clubs, Suit::Diamonds, Suit::Hearts, Suit::Spades];

fn c(r: Rank, s: Suit) -> Card {
    Card::new(r, s)
}

fn parse(s: &str) -> Card {
    let b = s.as_bytes();
    let r = match b[0] {
        b'2' => Rank::Two,
        b'3' => Rank::Three,
        b'4' => Rank::Four,
        b'5' => Rank::Five,
        b'6' => Rank::Six,
        b'7' => Rank::Seven,
        b'8' => Rank::Eight,
        b'9' => Rank::Nine,
        b'T' => Rank::Ten,
        b'J' => Rank::Jack,
        b'Q' => Rank::Queen,
        b'K' => Rank::King,
        b'A' => Rank::Ace,
        _ => panic!("rank {s}"),
    };
    let su = match b[1] {
        b'c' => Suit::Clubs,
        b'd' => Suit::Diamonds,
        b'h' => Suit::Hearts,
        b's' => Suit::Spades,
        _ => panic!("suit {s}"),
    };
    c(r, su)
}

fn avail(used: &[bool; 52], r: Rank) -> Vec<Card> {
    SUITS
        .iter()
        .map(|&s| c(r, s))
        .filter(|cd| !used[cd.to_u8() as usize])
        .collect()
}

/// 所有未被占用、rank ∈ ranks 的牌。
fn avail_ranks(used: &[bool; 52], ranks: &[Rank]) -> Vec<Card> {
    ranks.iter().flat_map(|&r| avail(used, r)).collect()
}

fn unordered_pairs(cards: &[Card]) -> Vec<[Card; 2]> {
    let mut out = Vec::new();
    for i in 0..cards.len() {
        for j in (i + 1)..cards.len() {
            out.push([cards[i], cards[j]]);
        }
    }
    out
}

/// 一张来自 a，一张来自 b（用于两对/顶对），去重去自重叠。
fn cross(a: &[Card], b: &[Card]) -> Vec<[Card; 2]> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for &x in a {
        for &y in b {
            if x.to_u8() == y.to_u8() {
                continue;
            }
            let key = (x.to_u8().min(y.to_u8()), x.to_u8().max(y.to_u8()));
            if seen.insert(key) {
                out.push([x, y]);
            }
        }
    }
    out
}

/// HU 精确权益：hero vs 对手 combo 列表（每个 (combo, river) 等权）。
fn equity_hu(
    hero: [Card; 2],
    board: &[Card],
    opp: &[[Card; 2]],
    eval: &dyn HandEvaluator,
) -> (f64, u64) {
    let mut base = [false; 52];
    for &cd in hero.iter().chain(board) {
        base[cd.to_u8() as usize] = true;
    }
    let mut win = 0.0_f64;
    let mut total = 0u64;
    for o in opp {
        if base[o[0].to_u8() as usize]
            || base[o[1].to_u8() as usize]
            || o[0].to_u8() == o[1].to_u8()
        {
            continue;
        }
        let mut used = base;
        used[o[0].to_u8() as usize] = true;
        used[o[1].to_u8() as usize] = true;
        for ru in 0..52u8 {
            if used[ru as usize] {
                continue;
            }
            let river = Card::from_u8(ru).unwrap();
            let h7 = [
                hero[0], hero[1], board[0], board[1], board[2], board[3], river,
            ];
            let o7 = [o[0], o[1], board[0], board[1], board[2], board[3], river];
            let hr = eval.eval7(&h7);
            let or = eval.eval7(&o7);
            win += match hr.cmp(&or) {
                std::cmp::Ordering::Greater => 1.0,
                std::cmp::Ordering::Equal => 0.5,
                std::cmp::Ordering::Less => 0.0,
            };
            total += 1;
        }
    }
    (win / total as f64, total)
}

/// 3-way pot-share：hero vs (opp1 range) × (opp2 range)，并列按 1/t 分。
fn share_3way(
    hero: [Card; 2],
    board: &[Card],
    r1: &[[Card; 2]],
    r2: &[[Card; 2]],
    eval: &dyn HandEvaluator,
) -> (f64, u64) {
    let mut base = [false; 52];
    for &cd in hero.iter().chain(board) {
        base[cd.to_u8() as usize] = true;
    }
    let mut share = 0.0_f64;
    let mut total = 0u64;
    for a in r1 {
        if base[a[0].to_u8() as usize]
            || base[a[1].to_u8() as usize]
            || a[0].to_u8() == a[1].to_u8()
        {
            continue;
        }
        for b in r2 {
            let ids = [a[0].to_u8(), a[1].to_u8(), b[0].to_u8(), b[1].to_u8()];
            if base[b[0].to_u8() as usize]
                || base[b[1].to_u8() as usize]
                || b[0].to_u8() == b[1].to_u8()
                || ids[2] == ids[0]
                || ids[2] == ids[1]
                || ids[3] == ids[0]
                || ids[3] == ids[1]
            {
                continue;
            }
            let mut used = base;
            for &id in &ids {
                used[id as usize] = true;
            }
            for ru in 0..52u8 {
                if used[ru as usize] {
                    continue;
                }
                let river = Card::from_u8(ru).unwrap();
                let bb = [board[0], board[1], board[2], board[3], river];
                let h7 = [hero[0], hero[1], bb[0], bb[1], bb[2], bb[3], bb[4]];
                let a7 = [a[0], a[1], bb[0], bb[1], bb[2], bb[3], bb[4]];
                let b7 = [b[0], b[1], bb[0], bb[1], bb[2], bb[3], bb[4]];
                let hr = eval.eval7(&h7);
                let ar = eval.eval7(&a7);
                let br = eval.eval7(&b7);
                if ar > hr || br > hr {
                    // beaten
                } else {
                    let t = 1 + (ar == hr) as u32 + (br == hr) as u32;
                    share += 1.0 / t as f64;
                }
                total += 1;
            }
        }
    }
    (share / total as f64, total)
}

fn main() {
    let eval = NaiveHandEvaluator;
    let hero = [parse("Ah"), parse("3h")];
    let board: Vec<Card> = ["4s", "7c", "5h", "Jh"].iter().map(|s| parse(s)).collect();

    let mut used = [false; 52];
    for &cd in hero.iter().chain(board.iter()) {
        used[cd.to_u8() as usize] = true;
    }

    // ---- 对手范围（显式 combo） ----
    let four = avail(&used, Rank::Four);
    let five = avail(&used, Rank::Five);
    let seven = avail(&used, Rank::Seven);
    let jack = avail(&used, Rank::Jack);

    let set_44 = unordered_pairs(&four);
    let set_55 = unordered_pairs(&five);
    let set_77 = unordered_pairs(&seven);
    let set_jj = unordered_pairs(&jack); // 顶 set
    let mut sets = Vec::new();
    for s in [&set_44, &set_55, &set_77, &set_jj] {
        sets.extend_from_slice(s);
    }

    // 两对：两张分别命中两个不同 board rank
    let board_ranks = [Rank::Four, Rank::Five, Rank::Seven, Rank::Jack];
    let mut two_pair = Vec::new();
    for i in 0..board_ranks.len() {
        for j in (i + 1)..board_ranks.len() {
            two_pair.extend(cross(
                &avail(&used, board_ranks[i]),
                &avail(&used, board_ranks[j]),
            ));
        }
    }

    // 顶对 Jx（强踢脚 A/K/Q/T）
    let kickers = avail_ranks(&used, &[Rank::Ace, Rank::King, Rank::Queen, Rank::Ten]);
    let top_pair_j = cross(&jack, &kickers);

    // 超对 AA/KK/QQ
    let mut overpair = Vec::new();
    for r in [Rank::Ace, Rank::King, Rank::Queen] {
        overpair.extend(unordered_pairs(&avail(&used, r)));
    }

    // 更差同花听（两张红心，被 hero 的坚果同花听压制）
    let hearts = avail_ranks(&used, &RANKS)
        .into_iter()
        .filter(|cd| cd.suit() == Suit::Hearts)
        .collect::<Vec<_>>();
    let worse_fd = unordered_pairs(&hearts);

    // 随机手（全部 C(46,2)）
    let live: Vec<Card> = (0..52u8)
        .filter(|&u| !used[u as usize])
        .map(|u| Card::from_u8(u).unwrap())
        .collect();
    let random = unordered_pairs(&live);

    // 价值混合（BTN turn 加注的纯价值假设：sets + 两对 + 顶对J + 超对）
    let mut value_blend = Vec::new();
    for s in [&sets, &two_pair, &top_pair_j, &overpair] {
        value_blend.extend_from_slice(s);
    }

    println!("# A3hh on 4s 7c 5h Jh — 精确 turn 权益（44 河牌全枚举）\n");
    let report = |label: &str, combos: &[[Card; 2]]| {
        let (eq, _n) = equity_hu(hero, &board, combos, &eval);
        println!(
            "  {:<24} HU equity = {:>6.2}%   ({} 个 combo)",
            label,
            eq * 100.0,
            combos.len()
        );
    };
    report("vs random hand", &random);
    report("vs set (44/55/77/JJ)", &sets);
    report("  vs set JJ (top set)", &set_jj);
    report("  vs set 77/55/44", &{
        let mut v = Vec::new();
        for s in [&set_77, &set_55, &set_44] {
            v.extend_from_slice(s);
        }
        v
    });
    report("vs two pair", &two_pair);
    report("vs top pair Jx", &top_pair_j);
    report("vs overpair AA/KK/QQ", &overpair);
    report("vs worse flush draw", &worse_fd);
    report("vs VALUE blend", &value_blend);

    println!("\n  --- 3-way pot share（两边都跟到底，保守下界）---");
    // seat1 续注范围（顶对/超对/两对/同花听），seat3 价值混合
    let mut seat1_range = Vec::new();
    for s in [&top_pair_j, &overpair, &two_pair, &worse_fd] {
        seat1_range.extend_from_slice(s);
    }
    let (sh, n) = share_3way(hero, &board, &seat1_range, &value_blend, &eval);
    println!(
        "  hero vs seat1(续注范围) vs seat3(价值混合): pot share = {:.2}%   ({} 局面)",
        sh * 100.0,
        n
    );
}
