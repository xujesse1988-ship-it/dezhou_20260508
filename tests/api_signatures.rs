//! API 签名类型断言（A1 评审后修订）。
//!
//! 阶段 1 骨架函数体一律 `unimplemented!()` 返回 `!`，由于 `!` 与任意返回类型
//! unify，误改公开签名（如 `Card::to_u8(self) -> u8` 改为 `-> u32`）不会导致
//! `cargo build` / `clippy` / `doc` 失败。本测试文件用 `let _: fn(...) -> ... =
//! T::method;` 把所有公开方法签名锁成编译期断言：任一签名漂移立即在 `cargo
//! test --no-run` 阶段失败。
//!
//! 维护规则：任何对公开 API 签名的合法修改（按 `pluribus_stage1_api.md` §11
//! API-NNN-revM 流程）必须**同步本文件**，否则 PR review 应拒绝合入。
//!
//! 不覆盖：
//! - trait 方法签名（在 trait 定义处由 rustc 校验）
//! - 泛型方法 `RngCoreAdapter::from_rng_core<R>`（fn 指针无法表达泛型）
//! - 公开字段类型（结构体定义处由 rustc 校验，且字段广泛被 spec 引用）

use std::ops::{Add, AddAssign, Mul, Sub, SubAssign};

use poker::*;

#[test]
fn api_signatures_locked() {
    // 函数体留空：所有断言在编译期完成。本 `#[test]` 仅用于让 `cargo test`
    // 报告一次"通过"。
    _api_signature_assertions();
}

#[allow(dead_code, clippy::type_complexity)]
fn _api_signature_assertions() {
    // ===================================================================
    // core (api §1)
    // ===================================================================

    // Card
    let _: fn(Rank, Suit) -> Card = Card::new;
    let _: fn(Card) -> Rank = Card::rank;
    let _: fn(Card) -> Suit = Card::suit;
    let _: fn(Card) -> u8 = Card::to_u8;
    let _: fn(u8) -> Option<Card> = Card::from_u8;

    // Rank / Suit
    let _: fn(u8) -> Option<Rank> = Rank::from_u8;
    let _: fn(u8) -> Option<Suit> = Suit::from_u8;

    // ChipAmount
    let _: fn(u64) -> ChipAmount = ChipAmount::new;
    let _: fn(ChipAmount) -> u64 = ChipAmount::as_u64;
    let _: ChipAmount = ChipAmount::ZERO;
    // 算术 trait（D-026 / D-026b）
    let _: fn(ChipAmount, ChipAmount) -> ChipAmount = <ChipAmount as Add>::add;
    let _: fn(ChipAmount, ChipAmount) -> ChipAmount = <ChipAmount as Sub>::sub;
    let _: fn(ChipAmount, u64) -> ChipAmount = <ChipAmount as Mul<u64>>::mul;
    let _: for<'a> fn(&'a mut ChipAmount, ChipAmount) = <ChipAmount as AddAssign>::add_assign;
    let _: for<'a> fn(&'a mut ChipAmount, ChipAmount) = <ChipAmount as SubAssign>::sub_assign;
    let _: fn() -> ChipAmount = <ChipAmount as Default>::default;

    // ===================================================================
    // core::rng (api §7)
    // ===================================================================

    let _: fn(u64) -> ChaCha20Rng = ChaCha20Rng::from_seed;
    // RngSource::next_u64 是 trait 方法，rustc 在 trait 定义处校验签名。
    // RngCoreAdapter::from_rng_core 是泛型方法，fn 指针无法表达，跳过。

    // ===================================================================
    // rules::action / config (api §2 / §3)
    // ===================================================================

    let _: fn() -> TableConfig = TableConfig::default_6max_100bb;

    // ===================================================================
    // rules::state (api §4 + API-001-rev1)
    // ===================================================================

    let _: fn(&TableConfig, u64) -> GameState = GameState::new;
    let _: fn(&TableConfig, u64, &mut dyn RngSource) -> GameState = GameState::with_rng;
    let _: for<'a> fn(&'a GameState) -> Option<SeatId> = GameState::current_player;
    let _: for<'a> fn(&'a GameState) -> LegalActionSet = GameState::legal_actions;
    let _: for<'a> fn(&'a mut GameState, Action) -> Result<(), RuleError> = GameState::apply;
    let _: for<'a> fn(&'a GameState) -> Street = GameState::street;
    let _: for<'a> fn(&'a GameState) -> &'a [Card] = GameState::board;
    let _: for<'a> fn(&'a GameState) -> ChipAmount = GameState::pot;
    let _: for<'a> fn(&'a GameState) -> &'a [Player] = GameState::players;
    let _: for<'a> fn(&'a GameState) -> bool = GameState::is_terminal;
    let _: for<'a> fn(&'a GameState) -> Option<Vec<(SeatId, i64)>> = GameState::payouts;
    let _: for<'a> fn(&'a GameState) -> &'a HandHistory = GameState::hand_history;

    // ===================================================================
    // eval (api §6)
    // ===================================================================

    let _: fn(HandRank) -> HandCategory = HandRank::category;
    // HandEvaluator trait 方法（eval5 / eval6 / eval7）由 trait 定义处校验。

    // ===================================================================
    // history (api §5 + API-001-rev1)
    // ===================================================================

    let _: for<'a> fn(&'a HandHistory) -> Vec<u8> = HandHistory::to_proto;
    let _: for<'a> fn(&'a [u8]) -> Result<HandHistory, HistoryError> = HandHistory::from_proto;
    let _: for<'a> fn(&'a HandHistory) -> Result<GameState, HistoryError> = HandHistory::replay;
    let _: for<'a> fn(&'a HandHistory, usize) -> Result<GameState, HistoryError> =
        HandHistory::replay_to;
    let _: for<'a> fn(&'a HandHistory) -> [u8; 32] = HandHistory::content_hash;
}
