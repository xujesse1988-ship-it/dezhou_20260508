//! 多人蒙特卡洛 equity + 对手 range 模型。
//!
//! **设计核心**：整个引擎的主货币是「我方手牌 vs 建模对手 range 的胜率」。这里提供
//! 一个**人数无关**的蒙特卡洛 equity：每个对手按其行动推断的「最低强度阈值」range 采样
//! 隐藏牌，补全公共牌，评估摊牌。座位/人数任意。
//!
//! 强度标度：`strength169[hand]` = 该 169 类起手牌对 1 个随机对手的 equity（启动一次性
//! MC 算），∈ [0,1]。range「松紧」就用这个标度的最低阈值表达，可解释、可调。

use crate::core::Card;
use crate::eval::eval7 as eval7_pub;
use crate::eval::HandRank;

// ===========================================================================
// 快速确定性 PRNG（wyrand）——无全局状态，满足可复现约束。
// ===========================================================================

#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    #[inline]
    pub fn new(seed: u64) -> Rng {
        // 避免 seed=0 退化。
        Rng(seed ^ 0x2545_F491_4F6C_DD1D)
    }
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0xa076_1d64_78bd_642f);
        let s = self.0;
        let t = (s as u128).wrapping_mul((s ^ 0xe703_7ed1_a0b4_28db) as u128);
        (t as u64) ^ ((t >> 64) as u64)
    }
    /// 0..n 的均匀整数（n>0）。
    #[inline]
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % (n as u64)) as usize
    }
    #[inline]
    pub fn unit(&mut self) -> f64 {
        // 53-bit 精度 → [0,1)。
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

// ===========================================================================
// 169 类规范化索引
// ===========================================================================

#[inline]
fn tri(hi: u8, lo: u8) -> usize {
    // hi>lo，两者 0..12；返回 0..77。
    (hi as usize * (hi as usize - 1)) / 2 + lo as usize
}

/// 两张牌 → 169 类索引（0..168）。pair 0..12；suited 13..90；offsuit 91..168。
pub fn canonical_index(c1: Card, c2: Card) -> usize {
    let r1 = c1.to_u8() / 4;
    let r2 = c2.to_u8() / 4;
    let s1 = c1.to_u8() % 4;
    let s2 = c2.to_u8() % 4;
    if r1 == r2 {
        return r1 as usize; // pair
    }
    let (hi, lo) = if r1 > r2 { (r1, r2) } else { (r2, r1) };
    if s1 == s2 {
        13 + tri(hi, lo)
    } else {
        13 + 78 + tri(hi, lo)
    }
}

// ===========================================================================
// EquityEngine
// ===========================================================================

/// 持有 169 强度表的 equity 引擎（启动构建一次，决策点反复调用）。
pub struct EquityEngine {
    /// strength169[idx] = equity-vs-1-random ∈ [0,1]。
    pub strength: [f64; 169],
    /// 按 combo 权重排序后的强度分位（用于 top-frac → 阈值换算）。
    sorted_strength: Vec<(f64, u32)>, // (strength, combo_count)，降序
    total_combos: u32,
}

impl EquityEngine {
    /// 构建强度表。`table_samples` = 每类起手牌的 MC rollout 数。
    pub fn new(table_samples: u32, seed: u64) -> EquityEngine {
        let mut strength = [0.0f64; 169];
        let mut rng = Rng::new(seed ^ 0xA5A5_1234_DEAD_BEEF);
        // 为每个 169 类找一个代表 concrete 手，MC vs 1 random opponent。
        for (idx, slot) in strength.iter_mut().enumerate() {
            let (c1, c2) = representative_hand(idx);
            *slot = preflop_equity_vs_random(c1, c2, table_samples, &mut rng);
        }
        EquityEngine::from_strength(strength)
    }

    /// 直接由给定的 169 强度向量构建（例如从 Python 缓存的 `strength169.json` 载入），
    /// 重建 `sorted_strength`/`total_combos`，使 [`threshold_for_top`](Self::threshold_for_top)
    /// 与强度来源一致（跨语言字节级对拍前提）。
    pub fn from_strength(strength: [f64; 169]) -> EquityEngine {
        // combo 权重：pair=6, suited=4, offsuit=12。
        let mut sorted: Vec<(f64, u32)> = Vec::with_capacity(169);
        let mut total = 0u32;
        for (idx, &s) in strength.iter().enumerate() {
            let w = if idx < 13 {
                6
            } else if idx < 13 + 78 {
                4
            } else {
                12
            };
            sorted.push((s, w));
            total += w;
        }
        sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        EquityEngine {
            strength,
            sorted_strength: sorted,
            total_combos: total,
        }
    }

    /// 起手牌强度（[0,1]）。
    #[inline]
    pub fn hand_strength(&self, c1: Card, c2: Card) -> f64 {
        self.strength[canonical_index(c1, c2)]
    }

    /// top `frac`（0..1，按 combo 权重）对应的最低强度阈值。frac=1 → 0（含全部）。
    pub fn threshold_for_top(&self, frac: f64) -> f64 {
        let frac = frac.clamp(0.0, 1.0);
        if frac >= 1.0 {
            return 0.0;
        }
        let target = (frac * self.total_combos as f64).max(1.0);
        let mut acc = 0u32;
        let mut last = 1.0;
        for &(s, w) in &self.sorted_strength {
            acc += w;
            last = s;
            if acc as f64 >= target {
                return s;
            }
        }
        last
    }

    /// **多人蒙特卡洛 equity**：我方 `hole` 在 `board` 上对 `opp_thresholds`（每个仍在场
    /// 对手一个最低强度阈值）的 range 的胜率（含平分摊）。`samples` 越大越准。
    pub fn equity(
        &self,
        hole: [Card; 2],
        board: &[Card],
        opp_thresholds: &[f64],
        samples: u32,
        seed: u64,
    ) -> f64 {
        let n_opp = opp_thresholds.len();
        if n_opp == 0 {
            return 1.0;
        }
        // dead = hole + board。
        let mut dead: u64 = 0;
        let mut hero_cards: Vec<u8> = Vec::with_capacity(7);
        for c in hole.iter().chain(board.iter()) {
            let b = c.to_u8();
            dead |= 1u64 << b;
            hero_cards.push(b);
        }
        // 每个对手的允许 combo 列表（从 deck 减 dead 之后筛强度）。
        let avail: Vec<u8> = (0..52u8).filter(|c| dead & (1u64 << c) == 0).collect();
        let mut allowed: Vec<Vec<(u8, u8)>> = Vec::with_capacity(n_opp);
        for &thr in opp_thresholds {
            let mut combos: Vec<(u8, u8)> = Vec::new();
            for i in 0..avail.len() {
                for j in (i + 1)..avail.len() {
                    let a = avail[i];
                    let b = avail[j];
                    let ca = Card::from_u8(a).unwrap();
                    let cb = Card::from_u8(b).unwrap();
                    if self.strength[canonical_index(ca, cb)] >= thr {
                        combos.push((a, b));
                    }
                }
            }
            if combos.is_empty() {
                // 阈值过紧无组合 → 退化成任意两张（鲁棒）。
                for i in 0..avail.len() {
                    for j in (i + 1)..avail.len() {
                        combos.push((avail[i], avail[j]));
                    }
                }
            }
            allowed.push(combos);
        }

        let board_need = 5usize.saturating_sub(board.len());
        let mut rng = Rng::new(seed);
        let mut win_sum = 0.0f64;
        let evaluator_board_base: Vec<u8> = board.iter().map(|c| c.to_u8()).collect();

        for _ in 0..samples {
            let mut used = dead;
            // 抽对手手牌。
            let mut opp_hole: Vec<(u8, u8)> = Vec::with_capacity(n_opp);
            let mut ok = true;
            for combos in &allowed {
                let mut got = None;
                for _try in 0..40 {
                    let (a, b) = combos[rng.below(combos.len())];
                    if used & (1u64 << a) == 0 && used & (1u64 << b) == 0 {
                        got = Some((a, b));
                        break;
                    }
                }
                let (a, b) = match got {
                    Some(p) => p,
                    None => {
                        // 兜底：随机两张未用牌。
                        let a = draw_unused(&mut rng, used);
                        used |= 1u64 << a;
                        let b = draw_unused(&mut rng, used);
                        (a, b)
                    }
                };
                used |= 1u64 << a;
                used |= 1u64 << b;
                opp_hole.push((a, b));
                if a == b {
                    ok = false;
                }
            }
            if !ok {
                continue;
            }
            // 补全公共牌。
            let mut full_board = evaluator_board_base.clone();
            for _ in 0..board_need {
                let c = draw_unused(&mut rng, used);
                used |= 1u64 << c;
                full_board.push(c);
            }
            // 评估。
            let hero_rank = eval7_cards(hero_cards[0], hero_cards[1], &full_board);
            let mut best_opp = HandRank(0);
            let mut tie_top = 1u32; // 含我方
            let mut hero_best = true;
            for &(a, b) in &opp_hole {
                let r = eval7_cards(a, b, &full_board);
                if r > hero_rank {
                    hero_best = false;
                    break;
                } else if r == hero_rank {
                    tie_top += 1;
                }
                if r > best_opp {
                    best_opp = r;
                }
            }
            if hero_best {
                if tie_top == 1 {
                    win_sum += 1.0;
                } else {
                    win_sum += 1.0 / tie_top as f64;
                }
            }
        }
        win_sum / samples as f64
    }
}

#[inline]
fn draw_unused(rng: &mut Rng, used: u64) -> u8 {
    loop {
        let c = rng.below(52) as u8;
        if used & (1u64 << c) == 0 {
            return c;
        }
    }
}

#[inline]
fn eval7_cards(h0: u8, h1: u8, board: &[u8]) -> HandRank {
    let cards: [Card; 7] = [
        Card::from_u8(h0).unwrap(),
        Card::from_u8(h1).unwrap(),
        Card::from_u8(board[0]).unwrap(),
        Card::from_u8(board[1]).unwrap(),
        Card::from_u8(board[2]).unwrap(),
        Card::from_u8(board[3]).unwrap(),
        Card::from_u8(board[4]).unwrap(),
    ];
    eval7_pub(&cards)
}

/// 169 类的代表 concrete 手（用于建强度表）。
fn representative_hand(idx: usize) -> (Card, Card) {
    if idx < 13 {
        // pair：rank idx，两花色 c/d。
        let r = idx as u8;
        return (
            Card::from_u8(r * 4).unwrap(),
            Card::from_u8(r * 4 + 1).unwrap(),
        );
    }
    let (suited, t) = if idx < 13 + 78 {
        (true, idx - 13)
    } else {
        (false, idx - 13 - 78)
    };
    // 反解 tri → (hi, lo)。
    let mut hi = 1u8;
    while tri(hi + 1, 0) <= t {
        hi += 1;
    }
    let lo = (t - tri(hi, 0)) as u8;
    if suited {
        (
            Card::from_u8(hi * 4).unwrap(),
            Card::from_u8(lo * 4).unwrap(),
        )
    } else {
        (
            Card::from_u8(hi * 4).unwrap(),
            Card::from_u8(lo * 4 + 1).unwrap(),
        )
    }
}

/// 单手 preflop equity vs 1 random opponent（建表用）。
fn preflop_equity_vs_random(c1: Card, c2: Card, samples: u32, rng: &mut Rng) -> f64 {
    let dead0 = (1u64 << c1.to_u8()) | (1u64 << c2.to_u8());
    let h0 = c1.to_u8();
    let h1 = c2.to_u8();
    let mut win = 0.0f64;
    for _ in 0..samples {
        let mut used = dead0;
        let o0 = draw_unused(rng, used);
        used |= 1u64 << o0;
        let o1 = draw_unused(rng, used);
        used |= 1u64 << o1;
        let mut board = [0u8; 5];
        for b in &mut board {
            let c = draw_unused(rng, used);
            used |= 1u64 << c;
            *b = c;
        }
        let hr = eval7_cards(h0, h1, &board);
        let or = eval7_cards(o0, o1, &board);
        if hr > or {
            win += 1.0;
        } else if hr == or {
            win += 0.5;
        }
    }
    win / samples as f64
}
