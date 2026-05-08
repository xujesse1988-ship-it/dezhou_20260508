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
    Action, Card, ChipAmount, GameState, LegalActionSet, Player, PlayerStatus, RngSource, SeatId,
    Street, TableConfig,
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

// ============================================================================
// Scenario DSL（C1）
// ============================================================================
//
// `ScenarioCase` 把"一手 fixed scenario"拍平为可表的数据结构。`run_scenario`
// 是单一驱动入口：构造状态、（可选）注入 stacked deck、按 plan 应用动作并断言
// 期望。这样让 200+ scenarios 能用 5–10 行紧凑表达，而不是每场景一个 `#[test]`
// 函数（编译开销与可读性都更好）。
//
// 角色边界：DSL 属测试基础设施。**不允许**含产品代码假设（除 API §4 公开签名）。

/// 一个 scenario 的可声明描述。所有字段都可选；只有 `name` / `config` / `plan`
/// 是必填。
pub struct ScenarioCase {
    pub name: &'static str,
    pub config: TableConfig,
    /// 使用 `seed` 走默认 ChaCha20 rng；与 `holes` / `board` 互斥。
    pub seed: u64,
    /// 显式 stacked deck：长度 = `n_seats`。设置时按 D-028 反推 rng，且必须同时
    /// 提供 `board`。设为 `None` 时使用 `seed`。
    pub holes: Option<Vec<(Card, Card)>>,
    /// `(flop, turn, river)`。仅当 `holes` 也设置时启用。
    pub board: Option<([Card; 3], Card, Card)>,
    /// 按顺序的 `(预期当前行动者, 动作)`。
    pub plan: Vec<(SeatId, Action)>,
    pub expect: ScenarioExpect,
}

/// 用于 `ScenarioExpect.legal_at_end` 的合法动作集断言。
#[derive(Copy, Clone, Debug)]
pub enum LegalAtEndCheck {
    /// `raise_range == None`。典型用例：D-033-rev1 已-acted 玩家被 incomplete 不重开。
    NoRaiseRange,
    /// `raise_range.is_some()`。典型用例：D-033-rev1 still-open 玩家可加注。
    HasRaiseRange,
    /// `raise_range.is_some() && raise_range.min == amount`。
    /// 典型用例：D-035 链条 min raise 数值断言。
    RaiseMinExact(ChipAmount),
    /// `bet_range == None`。
    NoBetRange,
    /// `call.is_some() && call == amount`。
    CallExact(ChipAmount),
    /// `check == true`（无须 call）。
    CheckLegal,
    /// `all_in_amount == Some(amount)`。
    AllInExact(ChipAmount),
}

impl LegalAtEndCheck {
    pub fn assert(self, name: &str, la: &LegalActionSet) {
        match self {
            LegalAtEndCheck::NoRaiseRange => assert!(
                la.raise_range.is_none(),
                "[{name}] expected raise_range == None, got {:?}",
                la.raise_range
            ),
            LegalAtEndCheck::HasRaiseRange => assert!(
                la.raise_range.is_some(),
                "[{name}] expected raise_range.is_some(), got None"
            ),
            LegalAtEndCheck::RaiseMinExact(want) => {
                let (got_min, _) = la
                    .raise_range
                    .unwrap_or_else(|| panic!("[{name}] expected raise_range = Some, got None"));
                assert_eq!(
                    got_min, want,
                    "[{name}] raise_range.min: expected {want:?}, got {got_min:?}"
                );
            }
            LegalAtEndCheck::NoBetRange => assert!(
                la.bet_range.is_none(),
                "[{name}] expected bet_range == None, got {:?}",
                la.bet_range
            ),
            LegalAtEndCheck::CallExact(want) => {
                let got = la
                    .call
                    .unwrap_or_else(|| panic!("[{name}] expected call = Some, got None"));
                assert_eq!(got, want, "[{name}] call: expected {want:?}, got {got:?}");
            }
            LegalAtEndCheck::CheckLegal => assert!(
                la.check,
                "[{name}] expected check == true, got false (call = {:?})",
                la.call
            ),
            LegalAtEndCheck::AllInExact(want) => {
                let got = la
                    .all_in_amount
                    .unwrap_or_else(|| panic!("[{name}] expected all_in_amount = Some, got None"));
                assert_eq!(
                    got, want,
                    "[{name}] all_in_amount: expected {want:?}, got {got:?}"
                );
            }
        }
    }
}

/// 终局期望。所有字段可选 — `None` 表示该维度不断言。
#[derive(Default)]
pub struct ScenarioExpect {
    /// `Some(true)` → 必须 terminal；`Some(false)` → 必须未 terminal；`None` → 不查。
    pub terminal: Option<bool>,
    pub street: Option<Street>,
    pub board_len: Option<usize>,
    /// `(seat_id_u8, expected_net)`。`SeatId` 不直接用是为了让 const 列表更短。
    pub payouts: Option<Vec<(u8, i64)>>,
    /// 期望 `payouts` 净和为 0（`I-001` 派生）。默认 `true` — 任何 fixed scenario
    /// 都该满足；设为 `false` 表示用例显式不要求（罕见，只用于 round-trip 中间态）。
    pub payouts_zero_sum: bool,
    /// 终局摊牌顺序 (D-037)：`Some(seat0)` 表示 `showdown_order[0] == seat0`。
    pub last_aggressor_first: Option<SeatId>,
    /// `Some((seat_u8, check))`：plan 跑完后，`current_player == seat` 时按 `check`
    /// 断言 `legal_actions()`。用枚举（而非 fn 指针）以便 200+ scenarios 表中
    /// 用 const 表达式直接表达，不必每用例写一个具名函数。
    pub legal_at_end: Option<(u8, LegalAtEndCheck)>,
    /// `Some(action_to_try)`：plan 跑完后尝试 `apply(action_to_try)`，必须失败。
    /// 用于 "incomplete raise + already-acted player tries Raise" 等拒绝路径。
    pub expect_apply_err: Option<Action>,
}

impl ScenarioExpect {
    pub fn new() -> ScenarioExpect {
        ScenarioExpect {
            terminal: None,
            street: None,
            board_len: None,
            payouts: None,
            payouts_zero_sum: true,
            last_aggressor_first: None,
            legal_at_end: None,
            expect_apply_err: None,
        }
    }
}

/// Scenario 驱动器。在 plan 应用过程中：
///
/// - 每步前断言 `current_player == expected_seat`；
/// - 每步后调用 [`Invariants::check_all`]；
/// - plan 跑完后按 `expect` 断言终局 / 中间态。
///
/// 失败时 panic（标准 `#[test]` 协议）。
pub fn run_scenario(case: &ScenarioCase) {
    let total = expected_total_chips(&case.config);

    let mut state = if let (Some(holes), Some((flop, turn, river))) = (&case.holes, &case.board) {
        let n = case.config.n_seats as usize;
        assert_eq!(
            holes.len(),
            n,
            "[{}] holes 长度 {} != n_seats {}",
            case.name,
            holes.len(),
            n
        );
        let used: HashSet<u8> = holes
            .iter()
            .flat_map(|(a, b)| [a.to_u8(), b.to_u8()])
            .chain(
                [flop[0], flop[1], flop[2], *turn, *river]
                    .iter()
                    .map(|c| c.to_u8()),
            )
            .collect();
        let padding = pick_unused_padding(&used, 52 - 2 * n - 5);
        let deck = build_dealing_order(n, holes, *flop, *turn, *river, &padding);
        let mut rng = StackedDeckRng::from_target_cards(deck);
        GameState::with_rng(&case.config, case.seed, &mut rng)
    } else {
        GameState::new(&case.config, case.seed)
    };

    Invariants::check_all(&state, total)
        .unwrap_or_else(|e| panic!("[{}] initial invariant: {e}", case.name));

    for (i, (want_seat, action)) in case.plan.iter().enumerate() {
        let cp = state.current_player().unwrap_or_else(|| {
            panic!(
                "[{}] step {i}: current_player == None, expected {want_seat:?}",
                case.name
            )
        });
        assert_eq!(
            cp, *want_seat,
            "[{}] step {i}: current_player mismatch (expected {want_seat:?}, got {cp:?})",
            case.name
        );
        state
            .apply(*action)
            .unwrap_or_else(|e| panic!("[{}] step {i}: apply({action:?}) failed: {e}", case.name));
        Invariants::check_all(&state, total)
            .unwrap_or_else(|e| panic!("[{}] step {i} (after {action:?}): {e}", case.name));
    }

    if let Some(t) = case.expect.terminal {
        assert_eq!(
            state.is_terminal(),
            t,
            "[{}] expected is_terminal == {t}, got {}",
            case.name,
            state.is_terminal()
        );
    }
    if let Some(s) = case.expect.street {
        assert_eq!(
            state.street(),
            s,
            "[{}] expected street {s:?}, got {:?}",
            case.name,
            state.street()
        );
    }
    if let Some(n) = case.expect.board_len {
        assert_eq!(
            state.board().len(),
            n,
            "[{}] expected board.len() == {n}, got {}",
            case.name,
            state.board().len()
        );
    }
    if let Some(expected) = &case.expect.payouts {
        let actual = state
            .payouts()
            .unwrap_or_else(|| panic!("[{}] payouts() == None at expectations", case.name));
        for (sid, expected_net) in expected {
            let got = actual
                .iter()
                .find(|(s, _)| s.0 == *sid)
                .unwrap_or_else(|| panic!("[{}] seat {sid} missing in payouts", case.name));
            assert_eq!(
                got.1, *expected_net,
                "[{}] seat {sid} net: expected {expected_net}, got {}",
                case.name, got.1
            );
        }
        if case.expect.payouts_zero_sum {
            let sum: i64 = actual.iter().map(|(_, n)| n).sum();
            assert_eq!(sum, 0, "[{}] payouts net sum != 0: {sum}", case.name);
        }
    } else if case.expect.payouts_zero_sum {
        if let Some(actual) = state.payouts() {
            let sum: i64 = actual.iter().map(|(_, n)| n).sum();
            assert_eq!(sum, 0, "[{}] payouts net sum != 0: {sum}", case.name);
        }
    }
    if let Some(seat) = case.expect.last_aggressor_first {
        let order = &state.hand_history().showdown_order;
        assert_eq!(
            order.first(),
            Some(&seat),
            "[{}] showdown_order[0] expected {seat:?}, got {:?}",
            case.name,
            order.first()
        );
    }
    if let Some((sid, check)) = case.expect.legal_at_end {
        let cp = state.current_player().unwrap_or_else(|| {
            panic!(
                "[{}] expected current_player == seat({sid}), got None",
                case.name
            )
        });
        assert_eq!(
            cp.0, sid,
            "[{}] legal_at_end target {sid}, got {cp:?}",
            case.name
        );
        let la = state.legal_actions();
        check.assert(case.name, &la);
    }
    if let Some(bad_action) = case.expect.expect_apply_err {
        let err = state.apply(bad_action);
        assert!(
            err.is_err(),
            "[{}] expected apply({bad_action:?}) to error, got Ok",
            case.name
        );
    }
}

/// 6-max 默认配置 + 自定义 starting_stacks 列表。len 必须 = 6。
pub fn cfg_6max_with_stacks(stacks: [u64; 6]) -> TableConfig {
    let mut cfg = TableConfig::default_6max_100bb();
    for (i, v) in stacks.iter().enumerate() {
        cfg.starting_stacks[i] = ChipAmount::new(*v);
    }
    cfg
}

/// 把 `(seat_u8, action)` 字面量数组转成 plan vec。
pub fn plan(items: &[(u8, Action)]) -> Vec<(SeatId, Action)> {
    items.iter().map(|(s, a)| (SeatId(*s), *a)).collect()
}

/// 6-max 三玩家模板：UTG/MP/CO 全弃，把"3-人决战"的样板缩成一行调用。
pub fn fold_to_three_handed_prefix() -> Vec<(SeatId, Action)> {
    vec![
        (SeatId(3), Action::Fold),
        (SeatId(4), Action::Fold),
        (SeatId(5), Action::Fold),
    ]
}

/// 给定 `n_seats`，构造一个所有非按钮 / 非 SB / 非 BB 座位 fold 的 prefix。
/// 即 BTN/SB/BB 三人决战。
pub fn fold_to_button_sb_bb(cfg: &TableConfig) -> Vec<(SeatId, Action)> {
    let n = cfg.n_seats as usize;
    let btn = cfg.button_seat.0 as usize;
    let sb = (btn + 1) % n;
    let bb = (btn + 2) % n;
    let mut out = Vec::new();
    // UTG = button + 3，依次 fold 到 BTN 之前。
    for offset in 3..n {
        let s = (btn + offset) % n;
        if s != btn && s != sb && s != bb {
            out.push((SeatId(s as u8), Action::Fold));
        }
    }
    out
}
