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
use std::path::Path;
use std::sync::Arc;

use poker::*;

#[test]
fn api_signatures_locked() {
    // 函数体留空：所有断言在编译期完成。本 `#[test]` 仅用于让 `cargo test`
    // 报告一次"通过"。
    _api_signature_assertions();
    _stage2_api_signature_assertions();
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
    // API-005-rev1（§E-rev1 §9 R1 procedural follow-through，F1 [测试] 同 PR 落地）：
    // `RngSource::fill_u64s` default-impl trait 方法 UFCS 绑定，让 trait↔impl 任一漂移
    // 立即在 `cargo test --no-run` 失败。
    let _: for<'a, 'b> fn(&'a mut ChaCha20Rng, &'b mut [u64]) =
        <ChaCha20Rng as RngSource>::fill_u64s;

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

// ===========================================================================
// 阶段 2 trip-wire（API-200..API-302；A1 阶段所有方法体 `unimplemented!()` 返回
// `!`，与 stage 1 同形态——签名漂移立即在 `cargo test --no-run` 阶段失败。
//
// 维护规则同阶段 1：任何对公开 API 签名的合法修改（按
// `pluribus_stage2_api.md` §9 API-NNN-revM 流程）必须**同步本文件**，否则 PR
// review 应拒绝合入。
//
// trait 方法签名锁定（batch 8 review 收紧）：阶段 1 的「trait 方法由 trait 定义
// 处由 rustc 校验」措辞**不充分**——rustc 只校验 impl ↔ trait 一致性，不校验
// trait ↔ API 文档；若 API spec / trait / impl 三者一起漂移（典型反模式：
// `EquityCalculator::equity` 从 `Result<f64, EquityError>` 改回 `f64` 同时改
// trait 与 impl），现有 trip-wire 会静默通过。本文件用 UFCS fn-pointer 绑定
// `<具体 impl as Trait>::method` 的方式把替换后的签名钉在调用点，trait 方法
// 任一漂移立即在 `cargo test --no-run` 失败。覆盖 `ActionAbstraction` (3 方法
// × `DefaultActionAbstraction`) + `InfoAbstraction` (`map` × `PreflopLossless169`
// + `PostflopBucketAbstraction`) + `EquityCalculator` (4 方法 ×
// `MonteCarloEquity`) 共 9 条 trait 方法 + `InfoSetId::from_game_state` 用
// `PreflopLossless169` 实例化锁住泛型。
//
// 不覆盖：
// - 公开字段类型（结构体定义处由 rustc 校验）
// ===========================================================================

#[allow(dead_code, clippy::type_complexity)]
fn _stage2_api_signature_assertions() {
    // ===================================================================
    // abstraction::action (api §1)
    // ===================================================================

    // BetRatio
    let _: fn(f64) -> Option<BetRatio> = BetRatio::from_f64;
    let _: fn(BetRatio) -> u32 = BetRatio::as_milli;
    let _: BetRatio = BetRatio::HALF_POT;
    let _: BetRatio = BetRatio::FULL_POT;

    // AbstractActionSet
    let _: for<'a> fn(&'a AbstractActionSet) -> std::slice::Iter<'a, AbstractAction> =
        AbstractActionSet::iter;
    let _: for<'a> fn(&'a AbstractActionSet) -> usize = AbstractActionSet::len;
    let _: for<'a> fn(&'a AbstractActionSet) -> bool = AbstractActionSet::is_empty;
    let _: for<'a> fn(&'a AbstractActionSet, AbstractAction) -> bool = AbstractActionSet::contains;
    let _: for<'a> fn(&'a AbstractActionSet) -> &'a [AbstractAction] = AbstractActionSet::as_slice;

    // ActionAbstractionConfig
    let _: fn() -> ActionAbstractionConfig = ActionAbstractionConfig::default_5_action;
    let _: fn(Vec<f64>) -> Result<ActionAbstractionConfig, ConfigError> =
        ActionAbstractionConfig::new;
    let _: for<'a> fn(&'a ActionAbstractionConfig) -> usize = ActionAbstractionConfig::raise_count;

    // DefaultActionAbstraction
    let _: fn(ActionAbstractionConfig) -> DefaultActionAbstraction = DefaultActionAbstraction::new;
    let _: fn() -> DefaultActionAbstraction = DefaultActionAbstraction::default_5_action;

    // ActionAbstraction trait 方法 UFCS 绑到具体 impl，锁住 trait + impl 联合签名
    // （rustc 仅校验 impl ↔ trait 一致性，不校验 trait ↔ API 文档；UFCS 把替换后的
    // 具体签名钉在调用点，trait 方法漂移立即在 cargo test --no-run 失败）。
    let _: for<'a, 'b> fn(&'a DefaultActionAbstraction, &'b GameState) -> AbstractActionSet =
        <DefaultActionAbstraction as ActionAbstraction>::abstract_actions;
    let _: for<'a, 'b> fn(
        &'a DefaultActionAbstraction,
        &'b GameState,
        ChipAmount,
    ) -> AbstractAction = <DefaultActionAbstraction as ActionAbstraction>::map_off_tree;
    let _: for<'a> fn(&'a DefaultActionAbstraction) -> &'a ActionAbstractionConfig =
        <DefaultActionAbstraction as ActionAbstraction>::config;

    // §7 桥接：AbstractAction::to_concrete
    let _: fn(AbstractAction) -> Action = AbstractAction::to_concrete;

    // ===================================================================
    // abstraction::info (api §2)
    // ===================================================================

    let _: fn(InfoSetId) -> u64 = InfoSetId::raw;
    let _: fn(InfoSetId) -> StreetTag = InfoSetId::street_tag;
    let _: fn(InfoSetId) -> u8 = InfoSetId::position_bucket;
    let _: fn(InfoSetId) -> u8 = InfoSetId::stack_bucket;
    let _: fn(InfoSetId) -> BettingState = InfoSetId::betting_state;
    let _: fn(InfoSetId) -> u32 = InfoSetId::bucket_id;

    // InfoSetId::from_game_state<A> 用 PreflopLossless169 实例化锁住泛型签名
    // （泛型本身无法直接绑 fn-pointer，但具体实例化后类型固定）。
    let _: for<'a> fn(&'a GameState, [Card; 2], &'a PreflopLossless169) -> InfoSetId =
        InfoSetId::from_game_state::<PreflopLossless169>;

    // InfoAbstraction::map UFCS 绑到 preflop / postflop 两个 impl，锁住 trait + impl
    // 联合签名（同 ActionAbstraction 段落理由）。
    let _: for<'a, 'b> fn(&'a PreflopLossless169, &'b GameState, [Card; 2]) -> InfoSetId =
        <PreflopLossless169 as InfoAbstraction>::map;
    let _: for<'a, 'b> fn(&'a PostflopBucketAbstraction, &'b GameState, [Card; 2]) -> InfoSetId =
        <PostflopBucketAbstraction as InfoAbstraction>::map;

    // ===================================================================
    // abstraction::preflop (api §2 + helper)
    // ===================================================================

    let _: fn([Card; 2]) -> u32 = canonical_hole_id;
    let _: fn() -> PreflopLossless169 = PreflopLossless169::new;
    let _: for<'a> fn(&'a PreflopLossless169, [Card; 2]) -> u8 = PreflopLossless169::hand_class;
    let _: fn(u8) -> u8 = PreflopLossless169::hole_count_in_class;

    // ===================================================================
    // abstraction::postflop (api §2 + helper)
    // ===================================================================

    let _: for<'a> fn(StreetTag, &'a [Card], [Card; 2]) -> u32 = canonical_observation_id;
    let _: fn(BucketTable) -> PostflopBucketAbstraction = PostflopBucketAbstraction::new;
    let _: for<'a> fn(&'a PostflopBucketAbstraction, &'a GameState, [Card; 2]) -> u32 =
        PostflopBucketAbstraction::bucket_id;
    let _: for<'a> fn(&'a PostflopBucketAbstraction) -> BucketConfig =
        PostflopBucketAbstraction::config;

    // ===================================================================
    // abstraction::equity (api §3)
    // ===================================================================

    let _: fn(Arc<dyn HandEvaluator>) -> MonteCarloEquity = MonteCarloEquity::new;
    let _: fn(MonteCarloEquity, u32) -> MonteCarloEquity = MonteCarloEquity::with_iter;
    let _: fn(MonteCarloEquity, u8) -> MonteCarloEquity = MonteCarloEquity::with_opp_clusters;
    let _: for<'a> fn(&'a MonteCarloEquity) -> u32 = MonteCarloEquity::iter;
    let _: for<'a> fn(&'a MonteCarloEquity) -> u8 = MonteCarloEquity::n_opp_clusters;

    // EquityCalculator trait 全 4 方法 UFCS 绑到 MonteCarloEquity，锁住 trait + impl
    // 联合签名（同 ActionAbstraction 段落理由）。`&mut dyn RngSource` 高阶生命周期：
    // self / board / rng 三条独立 borrow，统一以 for<'a, 'b, 'c> 显式表达。
    let _: for<'a, 'b, 'c> fn(
        &'a MonteCarloEquity,
        [Card; 2],
        &'b [Card],
        &'c mut dyn RngSource,
    ) -> Result<f64, EquityError> = <MonteCarloEquity as EquityCalculator>::equity;
    let _: for<'a, 'b, 'c> fn(
        &'a MonteCarloEquity,
        [Card; 2],
        [Card; 2],
        &'b [Card],
        &'c mut dyn RngSource,
    ) -> Result<f64, EquityError> = <MonteCarloEquity as EquityCalculator>::equity_vs_hand;
    let _: for<'a, 'b, 'c> fn(
        &'a MonteCarloEquity,
        [Card; 2],
        &'b [Card],
        &'c mut dyn RngSource,
    ) -> Result<f64, EquityError> = <MonteCarloEquity as EquityCalculator>::ehs_squared;
    let _: for<'a, 'b, 'c> fn(
        &'a MonteCarloEquity,
        [Card; 2],
        &'b [Card],
        &'c mut dyn RngSource,
    ) -> Result<Vec<f64>, EquityError> = <MonteCarloEquity as EquityCalculator>::ochs;

    // ===================================================================
    // abstraction::bucket_table (api §4)
    // ===================================================================

    let _: fn() -> BucketConfig = BucketConfig::default_500_500_500;
    let _: fn(u32, u32, u32) -> Result<BucketConfig, ConfigError> = BucketConfig::new;

    let _: for<'a> fn(&'a Path) -> Result<BucketTable, BucketTableError> = BucketTable::open;
    let _: for<'a> fn(&'a BucketTable, StreetTag, u32) -> Option<u32> = BucketTable::lookup;
    let _: for<'a> fn(&'a BucketTable) -> u32 = BucketTable::schema_version;
    let _: for<'a> fn(&'a BucketTable) -> u32 = BucketTable::feature_set_id;
    let _: for<'a> fn(&'a BucketTable) -> BucketConfig = BucketTable::config;
    let _: for<'a> fn(&'a BucketTable) -> u64 = BucketTable::training_seed;
    let _: for<'a> fn(&'a BucketTable, StreetTag) -> u32 = BucketTable::bucket_count;
    let _: for<'a> fn(&'a BucketTable, StreetTag) -> u32 = BucketTable::n_canonical_observation;
    let _: for<'a> fn(&'a BucketTable) -> [u8; 32] = BucketTable::content_hash;

    // ===================================================================
    // abstraction::cluster::rng_substream (D-228 公开 contract)
    // ===================================================================

    let _: fn(u64, u32, u32) -> u64 = rng_substream::derive_substream_seed;
    // op_id 常量（15 个）：lib.rs 顶层 `pub use abstraction::cluster::rng_substream;`
    // 暴露后通过 `rng_substream::*` 访问。任何常量重命名 / 数值漂移立即在
    // `cargo test --no-run` 失败。
    let _: u32 = rng_substream::OCHS_WARMUP;
    let _: u32 = rng_substream::CLUSTER_MAIN_FLOP;
    let _: u32 = rng_substream::CLUSTER_MAIN_TURN;
    let _: u32 = rng_substream::CLUSTER_MAIN_RIVER;
    let _: u32 = rng_substream::KMEANS_PP_INIT_FLOP;
    let _: u32 = rng_substream::KMEANS_PP_INIT_TURN;
    let _: u32 = rng_substream::KMEANS_PP_INIT_RIVER;
    let _: u32 = rng_substream::EMPTY_CLUSTER_SPLIT_FLOP;
    let _: u32 = rng_substream::EMPTY_CLUSTER_SPLIT_TURN;
    let _: u32 = rng_substream::EMPTY_CLUSTER_SPLIT_RIVER;
    let _: u32 = rng_substream::EQUITY_MONTE_CARLO;
    let _: u32 = rng_substream::EHS2_INNER_EQUITY_FLOP;
    let _: u32 = rng_substream::EHS2_INNER_EQUITY_TURN;
    let _: u32 = rng_substream::EHS2_INNER_EQUITY_RIVER;
    let _: u32 = rng_substream::OCHS_FEATURE_INNER;
}
