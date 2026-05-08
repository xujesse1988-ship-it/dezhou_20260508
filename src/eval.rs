//! 手牌评估器（API §6）。

use crate::core::Card;

/// 不透明手牌强度。数值越大越强；同值代表同强度（split pot）。
///
/// 注意：不要求 `HandRank` 数值跨不同 evaluator 实现一致；只要求**同一 evaluator
/// 内部全序稳定**。
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash, Debug)]
pub struct HandRank(pub u32);

impl HandRank {
    pub fn category(self) -> HandCategory {
        match self.0 / RANK_BASE {
            0 => HandCategory::HighCard,
            1 => HandCategory::OnePair,
            2 => HandCategory::TwoPair,
            3 => HandCategory::Trips,
            4 => HandCategory::Straight,
            5 => HandCategory::Flush,
            6 => HandCategory::FullHouse,
            7 => HandCategory::Quads,
            8 => HandCategory::StraightFlush,
            _ => HandCategory::RoyalFlush,
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum HandCategory {
    HighCard,
    OnePair,
    TwoPair,
    Trips,
    Straight,
    Flush,
    FullHouse,
    Quads,
    StraightFlush,
    RoyalFlush,
}

/// 评估器接口。同一 trait 同时支持 5/6/7-card。
///
/// `eval6` / `eval7` 必须返回所有 5-card 子集中最强的 `HandRank`；
/// 三个接口对相同 5-card 输入必须返回相同 `HandRank`。
pub trait HandEvaluator: Send + Sync {
    fn eval5(&self, cards: &[Card; 5]) -> HandRank;
    fn eval6(&self, cards: &[Card; 6]) -> HandRank;
    fn eval7(&self, cards: &[Card; 7]) -> HandRank;
}

const RANK_BASE: u32 = 13_u32.pow(5);

/// 朴素枚举评估器。B2/C2 只追求正确性，E2 再替换热路径实现。
#[derive(Copy, Clone, Debug, Default)]
pub struct NaiveHandEvaluator;

impl HandEvaluator for NaiveHandEvaluator {
    fn eval5(&self, cards: &[Card; 5]) -> HandRank {
        eval5_inner(cards)
    }

    fn eval6(&self, cards: &[Card; 6]) -> HandRank {
        let mut best = HandRank(0);
        for skip in 0..6 {
            let mut hand = [cards[0]; 5];
            let mut out = 0;
            for (i, card) in cards.iter().enumerate() {
                if i == skip {
                    continue;
                }
                hand[out] = *card;
                out += 1;
            }
            best = best.max(eval5_inner(&hand));
        }
        best
    }

    fn eval7(&self, cards: &[Card; 7]) -> HandRank {
        let mut best = HandRank(0);
        for skip_a in 0..6 {
            for skip_b in skip_a + 1..7 {
                let mut hand = [cards[0]; 5];
                let mut out = 0;
                for (i, card) in cards.iter().enumerate() {
                    if i == skip_a || i == skip_b {
                        continue;
                    }
                    hand[out] = *card;
                    out += 1;
                }
                best = best.max(eval5_inner(&hand));
            }
        }
        best
    }
}

pub(crate) fn eval7(cards: &[Card; 7]) -> HandRank {
    NaiveHandEvaluator.eval7(cards)
}

fn eval5_inner(cards: &[Card; 5]) -> HandRank {
    let mut rank_counts = [0u8; 13];
    let mut suit_counts = [0u8; 4];
    for card in cards {
        rank_counts[(card.to_u8() / 4) as usize] += 1;
        suit_counts[(card.to_u8() % 4) as usize] += 1;
    }

    let flush = suit_counts.contains(&5);
    let straight_high = straight_high(&rank_counts);

    if flush {
        if let Some(high) = straight_high {
            if high == 12 {
                return encode(HandCategory::RoyalFlush, &[12]);
            }
            return encode(HandCategory::StraightFlush, &[high]);
        }
    }

    let groups = rank_groups(&rank_counts);
    if groups[0].0 == 4 {
        let kicker = groups
            .iter()
            .find(|(count, _)| *count == 1)
            .map(|(_, rank)| *rank)
            .expect("quads must have kicker");
        return encode(HandCategory::Quads, &[groups[0].1, kicker]);
    }

    if groups[0].0 == 3 && groups[1].0 == 2 {
        return encode(HandCategory::FullHouse, &[groups[0].1, groups[1].1]);
    }

    if flush {
        return encode(HandCategory::Flush, &ranks_desc(&rank_counts));
    }

    if let Some(high) = straight_high {
        return encode(HandCategory::Straight, &[high]);
    }

    if groups[0].0 == 3 {
        let mut kickers = vec![groups[0].1];
        kickers.extend(
            groups
                .iter()
                .filter(|(count, _)| *count == 1)
                .map(|(_, r)| *r),
        );
        return encode(HandCategory::Trips, &kickers);
    }

    if groups[0].0 == 2 && groups[1].0 == 2 {
        let kicker = groups
            .iter()
            .find(|(count, _)| *count == 1)
            .map(|(_, rank)| *rank)
            .expect("two pair must have kicker");
        return encode(HandCategory::TwoPair, &[groups[0].1, groups[1].1, kicker]);
    }

    if groups[0].0 == 2 {
        let mut kickers = vec![groups[0].1];
        kickers.extend(
            groups
                .iter()
                .filter(|(count, _)| *count == 1)
                .map(|(_, r)| *r),
        );
        return encode(HandCategory::OnePair, &kickers);
    }

    encode(HandCategory::HighCard, &ranks_desc(&rank_counts))
}

fn straight_high(rank_counts: &[u8; 13]) -> Option<u8> {
    for high in (4..=12).rev() {
        if (0..5).all(|offset| rank_counts[(high - offset) as usize] > 0) {
            return Some(high);
        }
    }
    if rank_counts[12] > 0
        && rank_counts[0] > 0
        && rank_counts[1] > 0
        && rank_counts[2] > 0
        && rank_counts[3] > 0
    {
        return Some(3);
    }
    None
}

fn rank_groups(rank_counts: &[u8; 13]) -> Vec<(u8, u8)> {
    let mut groups = Vec::with_capacity(5);
    for rank in (0..13u8).rev() {
        let count = rank_counts[rank as usize];
        if count > 0 {
            groups.push((count, rank));
        }
    }
    groups.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    groups
}

fn ranks_desc(rank_counts: &[u8; 13]) -> Vec<u8> {
    let mut ranks = Vec::with_capacity(5);
    for rank in (0..13u8).rev() {
        for _ in 0..rank_counts[rank as usize] {
            ranks.push(rank);
        }
    }
    ranks
}

fn encode(category: HandCategory, ranks: &[u8]) -> HandRank {
    let category_value = match category {
        HandCategory::HighCard => 0,
        HandCategory::OnePair => 1,
        HandCategory::TwoPair => 2,
        HandCategory::Trips => 3,
        HandCategory::Straight => 4,
        HandCategory::Flush => 5,
        HandCategory::FullHouse => 6,
        HandCategory::Quads => 7,
        HandCategory::StraightFlush => 8,
        HandCategory::RoyalFlush => 9,
    };
    let mut value = category_value * RANK_BASE;
    let mut kicker_value = 0u32;
    for i in 0..5 {
        kicker_value *= 13;
        kicker_value += ranks.get(i).copied().unwrap_or(0) as u32;
    }
    value += kicker_value;
    HandRank(value)
}
