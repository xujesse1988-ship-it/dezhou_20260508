//! 测试共享工具（B1）。
//!
//! Rust 集成测试每个 `tests/*.rs` 文件单独编译为独立 crate。共享代码放在
//! `tests/common/mod.rs`，每个集成测试用 `mod common;` 引入。
//!
//! 本模块提供两类工具：
//!
//! 1. [`StackedDeckRng`]：按 D-028 协议反推 Fisher-Yates 索引序列，让测试
//!    通过指定 52 张牌的目标顺序来精确控制底牌 / flop / turn / river。
//!    适用于摊牌相关 fixed scenario（B1-A 类）与未来 fuzz 中"指定牌序"用例。
//! 2. [`Invariants`]：D-026 / I-001..I-007 / LA-001..LA-008 的运行期检查器。
//!    fuzz harness 与 cross-validation harness 在每步动作后调用 `check_all`。
//!
//! **角色边界**：本模块属测试基础设施，仅由 `tests/` 与 `benches/` 引用，
//! 不允许 `src/` 反向 import。

#![allow(dead_code)] // 不同集成测试 crate 引入子集，避免 dead_code 警告。

use std::collections::HashSet;

use poker::{
    Card, ChipAmount, GameState, LegalActionSet, Player, PlayerStatus, RngSource, SeatId, Street,
    TableConfig,
};

// ============================================================================
// StackedDeckRng（D-028 反推）
// ============================================================================

/// Stacked rng：按指定 52 张目标牌序产生 Fisher-Yates 所需的 `next_u64` 序列。
///
/// 用法：
/// ```ignore
/// let target: [u8; 52] = /* 想要的 deck 顺序（每元素 = Card::to_u8） */;
/// let mut rng = StackedDeckRng::from_target_u8(target);
/// let state = GameState::with_rng(&config, /*seed*/ 0, &mut rng);
/// ```
///
/// 反推逻辑（基于 D-028）：
///
/// - 初始 deck = `[Card(0), Card(1), ..., Card(51)]`。
/// - 第 `i` 轮（`i = 0..51`）：状态机按 D-028 计算 `j = i + (rng.next_u64() % (52 - i))`
///   并 `deck.swap(i, j)`。若希望第 `i` 位是 `target[i]`，找 `target[i]` 当前在
///   deck 中的位置 `p`（必有 `p >= i`，因为 `< i` 的位置已锁），令 `j = p`，
///   则 `next_u64() % (52 - i) == p - i`，因此第 `i` 次返回 `(p - i) as u64` 即可。
///
/// 仅消费 51 次；第 52 次以后的调用 panic。
pub struct StackedDeckRng {
    sequence: Vec<u64>,
    cursor: usize,
}

impl StackedDeckRng {
    /// 从 0..52 的 u8 目标序列构造。`target` 必须为 `[Card::to_u8]` 的一个排列
    /// （即 0..=51 各出现一次），否则 `panic`。
    pub fn from_target_u8(target: [u8; 52]) -> StackedDeckRng {
        let mut seen = [false; 52];
        for &v in &target {
            assert!(v < 52, "stacked deck: target value out of range: {v}");
            assert!(!seen[v as usize], "stacked deck: duplicate value: {v}");
            seen[v as usize] = true;
        }
        let mut deck: Vec<u8> = (0..52).collect();
        let mut sequence = Vec::with_capacity(51);
        for i in 0..51 {
            let want = target[i];
            let pos = deck[i..]
                .iter()
                .position(|&c| c == want)
                .unwrap_or_else(|| {
                    panic!("stacked deck: cannot place target[{i}] = {want} (already locked)")
                })
                + i;
            sequence.push((pos - i) as u64);
            deck.swap(i, pos);
        }
        debug_assert_eq!(
            deck,
            target.to_vec(),
            "stacked deck reverse derivation failed"
        );
        StackedDeckRng {
            sequence,
            cursor: 0,
        }
    }

    /// 便利构造：直接传 `[Card; 52]`。
    pub fn from_target_cards(target: [Card; 52]) -> StackedDeckRng {
        let mut u8s = [0u8; 52];
        for (i, c) in target.iter().enumerate() {
            u8s[i] = c.to_u8();
        }
        StackedDeckRng::from_target_u8(u8s)
    }
}

impl RngSource for StackedDeckRng {
    fn next_u64(&mut self) -> u64 {
        let v = *self.sequence.get(self.cursor).unwrap_or_else(|| {
            panic!(
                "StackedDeckRng exhausted: 52 calls to next_u64 but D-028 only allows 51 \
                 (implementation diverges from D-028 protocol)"
            )
        });
        self.cursor += 1;
        v
    }
}

// ============================================================================
// 牌序工具：构造"按发牌顺序"的目标 deck
// ============================================================================

/// 按 D-028 发牌顺序拼接：`(holes_seat0_card0, ..., holes_seatN_card0, holes_seat0_card1,
/// ..., holes_seatN_card1, flop[0], flop[1], flop[2], turn, river, 余下不关心)`。
///
/// `n_seats` 必须 >= 玩家数量。`holes` 长度 = `n_seats`，按发牌起点（SB = 按钮左 1，
/// 见 D-028）顺序提供每座位的两张底牌。`flop / turn / river` 共 5 张。
/// `padding`：用于填满剩余 `52 - 2*n_seats - 5` 张；调用方需保证全 deck 无重复。
///
/// 例：6-max 时返回 52 张顺序：
/// `[h0a,h1a,h2a,h3a,h4a,h5a, h0b,h1b,h2b,h3b,h4b,h5b, fA,fB,fC, T, R, padding...]`
pub fn build_dealing_order(
    n_seats: usize,
    holes: &[(Card, Card)],
    flop: [Card; 3],
    turn: Card,
    river: Card,
    padding: &[Card],
) -> [Card; 52] {
    assert_eq!(holes.len(), n_seats, "holes 长度必须等于 n_seats");
    let needed_padding = 52 - 2 * n_seats - 5;
    assert_eq!(
        padding.len(),
        needed_padding,
        "padding 长度必须为 {needed_padding}（52 - 2*n_seats - 5）"
    );

    // 合法性：合并后 52 张必须互不相同。
    let mut all: Vec<Card> = Vec::with_capacity(52);
    for (a, b) in holes {
        all.push(*a);
        all.push(*b);
    }
    for c in &flop {
        all.push(*c);
    }
    all.push(turn);
    all.push(river);
    all.extend_from_slice(padding);
    let unique: HashSet<u8> = all.iter().map(|c| c.to_u8()).collect();
    assert_eq!(unique.len(), 52, "build_dealing_order: 合并后存在重复牌");

    // 按 D-028 发牌索引重排：deck[k] = 第 k 个座位的第 1 张底牌（k = 0..n），
    // deck[n+k] = 第 k 个座位的第 2 张底牌，deck[2n..2n+3] = flop，deck[2n+3] = turn，
    // deck[2n+4] = river，deck[2n+5..] = padding。
    let mut deck = [Card::from_u8(0).expect("0 < 52"); 52];
    for (k, (a, _b)) in holes.iter().enumerate() {
        deck[k] = *a;
    }
    for (k, (_a, b)) in holes.iter().enumerate() {
        deck[n_seats + k] = *b;
    }
    deck[2 * n_seats] = flop[0];
    deck[2 * n_seats + 1] = flop[1];
    deck[2 * n_seats + 2] = flop[2];
    deck[2 * n_seats + 3] = turn;
    deck[2 * n_seats + 4] = river;
    for (i, p) in padding.iter().enumerate() {
        deck[2 * n_seats + 5 + i] = *p;
    }
    deck
}

/// 构造一个用于"reach 终点不关心"场景的 padding：从 `Card(0)` 开始挑出未被
/// `used` 占用的牌，按 `to_u8` 升序，直到凑够 `count` 张。`used` 应为已确定
/// 牌的 u8 集合。
pub fn pick_unused_padding(used: &HashSet<u8>, count: usize) -> Vec<Card> {
    let mut out = Vec::with_capacity(count);
    for v in 0..52u8 {
        if used.contains(&v) {
            continue;
        }
        out.push(Card::from_u8(v).expect("0..52 valid"));
        if out.len() == count {
            break;
        }
    }
    assert_eq!(
        out.len(),
        count,
        "pick_unused_padding: 未占用牌不足 (used={}, count={})",
        used.len(),
        count
    );
    out
}

// ============================================================================
// Invariants 检查器（D-026 / I-001..I-007 / LA-001..LA-008）
// ============================================================================

/// 一次完整不变量检查。`Ok(())` 表示通过；`Err(reason)` 表示违反。
///
/// 调用约定：`check_all` 接受一个 `&GameState` 和 `expected_starting_total`
/// （= `sum(TableConfig.starting_stacks)`，见 D-024 / I-001）。ante 在 D-024
/// 下从 stack 转入 pot，**总量守恒**，所以这里**不**再额外加 `ante * n_seats`。
/// fuzz 与 cross-validation harness 在每步 `apply` 后调用。
///
/// **B1 范围**：本检查器在 A1 unimplemented 状态下被 `catch_unwind` 包裹。
/// B2 起 GameState 落地，所有断言激活。
pub struct Invariants;

impl Invariants {
    /// 全套检查。失败时返回第一个违反的不变量编号 + 描述。
    pub fn check_all(state: &GameState, expected_starting_total: u64) -> Result<(), String> {
        Self::i001_chip_conservation(state, expected_starting_total)?;
        Self::i003_no_duplicate_cards(state)?;
        Self::i004_round_end_committed_equality(state)?;
        Self::pot_equals_sum_committed_total(state)?;
        Self::la_invariants(state)?;
        Ok(())
    }

    /// I-001：`sum(player.stack) + pot() = sum(starting_stacks)`（含 ante 转移）。
    pub fn i001_chip_conservation(state: &GameState, expected: u64) -> Result<(), String> {
        let stack_sum: u64 = state.players().iter().map(|p| p.stack.as_u64()).sum();
        let pot = state.pot().as_u64();
        if stack_sum + pot != expected {
            return Err(format!(
                "I-001 violated: sum(stack)={stack_sum} + pot={pot} = {} != expected={expected}",
                stack_sum + pot
            ));
        }
        Ok(())
    }

    /// I-003：一手内不出现重复 Card（hole + board 合集去重 = 总数）。
    pub fn i003_no_duplicate_cards(state: &GameState) -> Result<(), String> {
        let mut seen: HashSet<u8> = HashSet::new();
        for p in state.players() {
            if let Some([a, b]) = p.hole_cards {
                for c in [a, b] {
                    if !seen.insert(c.to_u8()) {
                        return Err(format!(
                            "I-003 violated: duplicate card {} (seat {:?})",
                            c.to_u8(),
                            p.seat
                        ));
                    }
                }
            }
        }
        for c in state.board() {
            if !seen.insert(c.to_u8()) {
                return Err(format!(
                    "I-003 violated: duplicate board card {}",
                    c.to_u8()
                ));
            }
        }
        Ok(())
    }

    /// I-004：每个 betting round **结束** 时，所有 `Active` 玩家的
    /// `committed_this_round` 相等。注意：街内的中间状态不需满足。
    /// 检查触发条件：当前 `street == Showdown` 或 `is_terminal == true`，
    /// 或 `current_player == None` 但仍有未行动玩家（all-in 跳轮场景）。
    ///
    /// `current_player.is_none()` 是 round end 的代理，会同时覆盖两条路径：
    ///
    /// 1. **正常 round 结束**：本街所有 active 玩家投入相等 → 实际可比；
    ///    actives.len() ≥ 2，进入字段相等性检查。
    /// 2. **D-036 跳轮**：除 ≤ 1 名 active 外全员 all-in → actives.len() < 2，
    ///    本不变量在该状态下不可观察（all-in 玩家投入未必等于 active 玩家），
    ///    通过 `actives.len() < 2 → Ok` 短路放行，不误报。
    pub fn i004_round_end_committed_equality(state: &GameState) -> Result<(), String> {
        // 仅在 round end 时刻可观察：使用 current_player == None 作为代理
        // （round end 才会出现 None，街内每步都有当前行动者）。
        if state.current_player().is_some() {
            return Ok(());
        }
        let actives: Vec<&Player> = state
            .players()
            .iter()
            .filter(|p| p.status == PlayerStatus::Active)
            .collect();
        if actives.len() < 2 {
            // D-036 跳轮 / 终局，不可观察；上方 doc 解释短路理由。
            return Ok(());
        }
        let first = actives[0].committed_this_round;
        for p in &actives[1..] {
            if p.committed_this_round != first {
                return Err(format!(
                    "I-004 violated at round end: seat {:?} committed_this_round={:?}, \
                     seat {:?} committed_this_round={:?}",
                    actives[0].seat, first, p.seat, p.committed_this_round
                ));
            }
        }
        Ok(())
    }

    /// pot = sum(committed_total) 的强约束。D-040 uncalled bet returned 触发
    /// 后，raiser 的 `committed_total` 同步减少，因此该等式仍成立。
    pub fn pot_equals_sum_committed_total(state: &GameState) -> Result<(), String> {
        let pot = state.pot().as_u64();
        let sum: u64 = state
            .players()
            .iter()
            .map(|p| p.committed_total.as_u64())
            .sum();
        if pot != sum {
            return Err(format!(
                "pot/committed mismatch: pot={pot} != sum(committed_total)={sum}"
            ));
        }
        Ok(())
    }

    /// LA-001..LA-008：合法动作集合不变量。仅在 `current_player.is_some()` 时
    /// 检查；终局 / 跳轮时 LA-008 单独保证（见 [`la_terminal_empty`]）。
    pub fn la_invariants(state: &GameState) -> Result<(), String> {
        let la = state.legal_actions();
        match state.current_player() {
            None => Self::la_terminal_empty(&la),
            Some(_) => Self::la_active_invariants(state, &la),
        }
    }

    /// LA-008：`current_player == None` 时所有字段为 `false / None`。
    pub fn la_terminal_empty(la: &LegalActionSet) -> Result<(), String> {
        if la.fold
            || la.check
            || la.call.is_some()
            || la.bet_range.is_some()
            || la.raise_range.is_some()
            || la.all_in_amount.is_some()
        {
            return Err(format!("LA-008 violated: terminal/skipped but la = {la:?}"));
        }
        Ok(())
    }

    /// LA-001..LA-007：current_player 存在时各项约束。
    pub fn la_active_invariants(state: &GameState, la: &LegalActionSet) -> Result<(), String> {
        // LA-003: fold 永远合法
        if !la.fold {
            return Err(
                "LA-003 violated: fold should be legal when current_player.is_some()".into(),
            );
        }
        // LA-001: check / call 互斥
        if la.check && la.call.is_some() {
            return Err("LA-001 violated: check && call.is_some()".into());
        }
        // LA-004: check 与 call 至少一个真
        if !la.check && la.call.is_none() {
            return Err("LA-004 violated: neither check nor call legal".into());
        }
        // LA-002: bet_range / raise_range 互斥
        if la.bet_range.is_some() && la.raise_range.is_some() {
            return Err("LA-002 violated: bet_range && raise_range both Some".into());
        }
        // LA-006: 上界不超过 committed_this_round + stack
        if let Some(cp) = state.current_player() {
            let p = state
                .players()
                .iter()
                .find(|p| p.seat == cp)
                .ok_or_else(|| format!("current_player {cp:?} not found in players()"))?;
            let cap = p.committed_this_round + p.stack;
            if let Some((_min, max)) = la.bet_range {
                if max > cap {
                    return Err(format!(
                        "LA-006 violated (bet_range): max={max:?} > committed+stack={cap:?}"
                    ));
                }
            }
            if let Some((_min, max)) = la.raise_range {
                if max > cap {
                    return Err(format!(
                        "LA-006 violated (raise_range): max={max:?} > committed+stack={cap:?}"
                    ));
                }
            }
            // LA-007: all_in_amount 当且仅当 stack > 0
            let stack_pos = p.stack > ChipAmount::ZERO;
            if stack_pos && la.all_in_amount.is_none() {
                return Err("LA-007 violated: stack > 0 but all_in_amount = None".into());
            }
            if !stack_pos && la.all_in_amount.is_some() {
                return Err("LA-007 violated: stack == 0 but all_in_amount = Some".into());
            }
            if let Some(amt) = la.all_in_amount {
                if amt != p.committed_this_round + p.stack {
                    return Err(format!(
                        "LA-007 violated: all_in_amount={amt:?} != committed+stack={:?}",
                        p.committed_this_round + p.stack
                    ));
                }
            }
        }
        Ok(())
    }
}

// ============================================================================
// 辅助：从 starting_stacks 推导预期 chip 总量
// ============================================================================

/// 给定 [`TableConfig`]，按 D-024 / I-001 计算"任意时刻 `sum(stacks) + pot`
/// 的目标值"。当前等价于 `sum(starting_stacks)`，因为 ante 是从 stack 转入 pot，
/// 总量不变。
pub fn expected_total_chips(config: &TableConfig) -> u64 {
    config
        .starting_stacks
        .iter()
        .map(|c| c.as_u64())
        .sum::<u64>()
}

// ============================================================================
// 便利 SeatId / Card 字面量
// ============================================================================

pub fn seat(id: u8) -> SeatId {
    SeatId(id)
}

pub fn chips(amount: u64) -> ChipAmount {
    ChipAmount::new(amount)
}

#[allow(clippy::missing_panics_doc)]
pub fn card(rank: u8, suit: u8) -> Card {
    // rank * 4 + suit 编码（D-020 / API §1）
    Card::from_u8(rank * 4 + suit).expect("rank<13 && suit<4")
}

#[allow(dead_code)]
pub const fn _streets_u8(s: Street) -> u8 {
    match s {
        Street::Preflop => 0,
        Street::Flop => 1,
        Street::Turn => 2,
        Street::River => 3,
        Street::Showdown => 4,
    }
}
