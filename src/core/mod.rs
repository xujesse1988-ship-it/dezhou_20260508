//! 基础类型（API §1）。
//!
//! 这里只放 stage-1 共享的"无领域逻辑"原语：牌、筹码、街、座位、玩家。
//! 随机源在子模块 [`rng`] 中。

pub mod rng;

use std::ops::{Add, AddAssign, Mul, Sub, SubAssign};

// ============================================================================
// Card / Rank / Suit
// ============================================================================

/// 整数后备的扑克牌。0..52 范围。
///
/// `Card::to_u8` 编码：`rank * 4 + suit`，保证跨平台稳定。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct Card(u8);

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum Rank {
    Two = 0,
    Three,
    Four,
    Five,
    Six,
    Seven,
    Eight,
    Nine,
    Ten,
    Jack,
    Queen,
    King,
    /// Ace = 12，Rank 比较时 `Two < Three < ... < Ace`。
    Ace,
}

impl Rank {
    /// 从 0..=12 的 u8 值还原 Rank；超出范围返回 `None`。
    pub fn from_u8(value: u8) -> Option<Rank> {
        let _ = value;
        unimplemented!()
    }
}

/// 花色不参与强度比较（NLHE 无花色优劣）。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum Suit {
    Clubs = 0,
    Diamonds,
    Hearts,
    Spades,
}

impl Suit {
    /// 从 0..=3 的 u8 值还原 Suit；超出范围返回 `None`。
    pub fn from_u8(value: u8) -> Option<Suit> {
        let _ = value;
        unimplemented!()
    }
}

impl Card {
    /// 构造一张牌。
    pub const fn new(rank: Rank, suit: Suit) -> Card {
        let _ = rank;
        let _ = suit;
        unimplemented!()
    }

    pub fn rank(self) -> Rank {
        unimplemented!()
    }

    pub fn suit(self) -> Suit {
        unimplemented!()
    }

    /// 0..52 的稳定数值表示。
    pub fn to_u8(self) -> u8 {
        unimplemented!()
    }

    pub fn from_u8(value: u8) -> Option<Card> {
        let _ = value;
        unimplemented!()
    }
}

// ============================================================================
// ChipAmount
// ============================================================================

/// 整数筹码。1 chip = 1/100 BB（D-020）。
///
/// 算术约定（D-026 / D-026b）：
/// - 仅整数路径，禁止浮点。
/// - `Sub` / `SubAssign` 在下溢时 **debug 与 release 都 panic**；
///   需要 saturating 语义的调用方必须显式用 `checked_sub`。
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct ChipAmount(pub u64);

impl ChipAmount {
    pub const ZERO: ChipAmount = ChipAmount(0);

    pub const fn new(chips: u64) -> ChipAmount {
        let _ = chips;
        unimplemented!()
    }

    pub fn as_u64(self) -> u64 {
        unimplemented!()
    }
}

impl Add for ChipAmount {
    type Output = ChipAmount;
    fn add(self, rhs: ChipAmount) -> ChipAmount {
        let _ = rhs;
        unimplemented!()
    }
}

impl AddAssign for ChipAmount {
    fn add_assign(&mut self, rhs: ChipAmount) {
        let _ = rhs;
        unimplemented!()
    }
}

impl Sub for ChipAmount {
    type Output = ChipAmount;
    /// 下溢时 panic（debug 与 release 均），见 D-026b。
    fn sub(self, rhs: ChipAmount) -> ChipAmount {
        let _ = rhs;
        unimplemented!()
    }
}

impl SubAssign for ChipAmount {
    fn sub_assign(&mut self, rhs: ChipAmount) {
        let _ = rhs;
        unimplemented!()
    }
}

impl Mul<u64> for ChipAmount {
    type Output = ChipAmount;
    fn mul(self, rhs: u64) -> ChipAmount {
        let _ = rhs;
        unimplemented!()
    }
}

// ============================================================================
// Street / Position / SeatId
// ============================================================================

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum Street {
    Preflop = 0,
    Flop = 1,
    Turn = 2,
    River = 3,
    Showdown = 4,
}

/// 6-max 标准位置。
///
/// 仅当桌面 = 6 人时使用此名称；其他桌大小用 `SeatId` 与按钮相对位置表达。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Position {
    BTN,
    SB,
    BB,
    UTG,
    MP,
    CO,
}

/// 座位号 0..n_seats。按桌面物理座位编号，不随按钮变化。
///
/// **方向约定（D-029）**：`SeatId(k+1 mod n_seats)` 是 `SeatId(k)` 的左邻。
/// 按钮轮转（D-032）、盲注推导（D-022b / D-032）、odd chip 分配（D-039）、
/// showdown 顺序（D-037）中"向左" / "按钮左侧" 均按此理解。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct SeatId(pub u8);

// ============================================================================
// Player / PlayerStatus
// ============================================================================

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum PlayerStatus {
    /// 在牌局中、未弃、未 all-in。
    Active,
    AllIn,
    Folded,
    /// 阶段 1 不使用，但保留枚举（D-032 简化方案禁用 sit-in/sit-out）。
    SittingOut,
}

#[derive(Clone, Debug)]
pub struct Player {
    pub seat: SeatId,
    /// 当前剩余筹码（不含本街已投入）。
    pub stack: ChipAmount,
    /// 本下注轮已投入金额。
    pub committed_this_round: ChipAmount,
    /// 本手全部下注轮累计已投入金额。
    pub committed_total: ChipAmount,
    /// `None` 表示尚未发或已弃。
    pub hole_cards: Option<[Card; 2]>,
    pub status: PlayerStatus,
}
