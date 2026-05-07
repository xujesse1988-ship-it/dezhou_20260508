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
        unimplemented!()
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
