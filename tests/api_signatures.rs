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

use std::collections::HashMap;
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
    _stage3_api_signature_assertions();
    _stage4_api_signature_assertions();
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

// ===========================================================================
// 阶段 3 trip-wire（API-300..API-392；A1 \[实现\] 阶段所有方法体 `unimplemented!()`
// 返回 `!`，与 stage 1 + stage 2 同形态——签名漂移立即在 `cargo test --no-run`
// 阶段失败。
//
// 维护规则同 stage 1 + stage 2：任何对公开 API 签名的合法修改（按
// `pluribus_stage3_api.md` §11 API-NNN-revM 流程）必须**同步本文件**，否则 PR
// review 应拒绝合入。
//
// trait 方法签名锁定继承 stage 2 §A1 模式：用 UFCS fn-pointer 绑定
// `<具体 impl as Trait>::method` 让 trait ↔ impl ↔ API 文档三者任一漂移立即在
// `cargo test --no-run` 失败。覆盖：
//   - `Game` trait 全 8 方法 × `{KuhnGame, LeducGame, SimplifiedNlheGame}` 3 impl
//   - `Trainer` trait 全 6 方法 × `{VanillaCfrTrainer<KuhnGame>,
//     EsMccfrTrainer<SimplifiedNlheGame>}` 2 instantiation
//   - `BestResponse` trait `compute` × `{KuhnBestResponse, LeducBestResponse}` 2 impl
//   - `RegretTable` / `StrategyAccumulator` 全部方法 × `KuhnInfoSet` 1 instantiation
//   - `sampling` 模块自由函数 + 6 个 op_id 常量
//   - `Checkpoint` save / open + `MAGIC` / `SCHEMA_VERSION` 常量
//   - `exploitability` 泛型函数 × `<KuhnGame, KuhnBestResponse>` 1 instantiation
//
// 不覆盖：
//   - 公开字段类型（结构体定义处由 rustc 校验）
//   - 关联类型 `Game::State` / `Action` / `InfoSet`（由 trait 定义处 rustc 校验
//     + impl `type X = Y;` 同步）
//   - `LeducHistory` 内部表示（type alias `Vec<LeducAction>` 在 A1 \[实现\] 阶段
//     替代 API-302 字面 `SmallVec<[LeducAction; 8]>`；详见 `src/training/leduc.rs`
//     模块 doc 末段；E2 \[实现\] hot path opt 时由 API-302-revM 评估是否引入
//     `smallvec` crate 第 4 个新增依赖）
// ===========================================================================

#[allow(dead_code, clippy::type_complexity)]
fn _stage3_api_signature_assertions() {
    use poker::training::checkpoint::{MAGIC, SCHEMA_VERSION};
    use poker::training::kuhn::KuhnState;
    use poker::training::leduc::LeducState;
    use poker::training::nlhe::{SimplifiedNlheAction, SimplifiedNlheInfoSet, SimplifiedNlheState};
    use poker::training::sampling::{
        derive_substream_seed, sample_discrete, OP_CHANCE_SAMPLE, OP_KUHN_DEAL, OP_LEDUC_DEAL,
        OP_NLHE_DEAL, OP_OPP_ACTION_SAMPLE, OP_TRAVERSER_TIE,
    };

    // ===================================================================
    // training::game (api §1)
    // ===================================================================

    // Game trait 全 8 方法 UFCS × KuhnGame（同 stage 2 ActionAbstraction UFCS 理由：
    // rustc 仅校验 impl ↔ trait 一致性，不校验 trait ↔ API 文档；UFCS 把替换后
    // 的具体签名钉在调用点，trait 方法任一漂移立即在 cargo test --no-run 失败）。
    let _: for<'a> fn(&'a KuhnGame) -> usize = <KuhnGame as Game>::n_players;
    let _: for<'a, 'b> fn(&'a KuhnGame, &'b mut dyn RngSource) -> KuhnState =
        <KuhnGame as Game>::root;
    let _: for<'a> fn(&'a KuhnState) -> NodeKind = <KuhnGame as Game>::current;
    let _: for<'a> fn(&'a KuhnState, PlayerId) -> KuhnInfoSet = <KuhnGame as Game>::info_set;
    let _: for<'a> fn(&'a KuhnState) -> Vec<KuhnAction> = <KuhnGame as Game>::legal_actions;
    let _: for<'a> fn(KuhnState, KuhnAction, &'a mut dyn RngSource) -> KuhnState =
        <KuhnGame as Game>::next;
    let _: for<'a> fn(&'a KuhnState) -> Vec<(KuhnAction, f64)> =
        <KuhnGame as Game>::chance_distribution;
    let _: for<'a> fn(&'a KuhnState, PlayerId) -> f64 = <KuhnGame as Game>::payoff;

    // Game trait UFCS × LeducGame。
    let _: for<'a> fn(&'a LeducGame) -> usize = <LeducGame as Game>::n_players;
    let _: for<'a, 'b> fn(&'a LeducGame, &'b mut dyn RngSource) -> LeducState =
        <LeducGame as Game>::root;
    let _: for<'a> fn(&'a LeducState) -> NodeKind = <LeducGame as Game>::current;
    let _: for<'a> fn(&'a LeducState, PlayerId) -> LeducInfoSet = <LeducGame as Game>::info_set;
    let _: for<'a> fn(&'a LeducState) -> Vec<LeducAction> = <LeducGame as Game>::legal_actions;
    let _: for<'a> fn(LeducState, LeducAction, &'a mut dyn RngSource) -> LeducState =
        <LeducGame as Game>::next;
    let _: for<'a> fn(&'a LeducState) -> Vec<(LeducAction, f64)> =
        <LeducGame as Game>::chance_distribution;
    let _: for<'a> fn(&'a LeducState, PlayerId) -> f64 = <LeducGame as Game>::payoff;

    // Game trait UFCS × SimplifiedNlheGame。
    let _: for<'a> fn(&'a SimplifiedNlheGame) -> usize = <SimplifiedNlheGame as Game>::n_players;
    let _: for<'a, 'b> fn(&'a SimplifiedNlheGame, &'b mut dyn RngSource) -> SimplifiedNlheState =
        <SimplifiedNlheGame as Game>::root;
    let _: for<'a> fn(&'a SimplifiedNlheState) -> NodeKind = <SimplifiedNlheGame as Game>::current;
    let _: for<'a> fn(&'a SimplifiedNlheState, PlayerId) -> SimplifiedNlheInfoSet =
        <SimplifiedNlheGame as Game>::info_set;
    let _: for<'a> fn(&'a SimplifiedNlheState) -> Vec<SimplifiedNlheAction> =
        <SimplifiedNlheGame as Game>::legal_actions;
    let _: for<'a> fn(
        SimplifiedNlheState,
        SimplifiedNlheAction,
        &'a mut dyn RngSource,
    ) -> SimplifiedNlheState = <SimplifiedNlheGame as Game>::next;
    let _: for<'a> fn(&'a SimplifiedNlheState) -> Vec<(SimplifiedNlheAction, f64)> =
        <SimplifiedNlheGame as Game>::chance_distribution;
    let _: for<'a> fn(&'a SimplifiedNlheState, PlayerId) -> f64 =
        <SimplifiedNlheGame as Game>::payoff;

    // SimplifiedNlheGame::new（API-303 构造函数 + D-314 bucket table 依赖 deferred）。
    let _: fn(Arc<BucketTable>) -> Result<SimplifiedNlheGame, TrainerError> =
        SimplifiedNlheGame::new;

    // ===================================================================
    // training::regret (api §3)
    // ===================================================================

    let _: fn() -> RegretTable<KuhnInfoSet> = RegretTable::<KuhnInfoSet>::new;
    let _: fn() -> RegretTable<KuhnInfoSet> = <RegretTable<KuhnInfoSet> as Default>::default;
    let _: for<'a> fn(&'a mut RegretTable<KuhnInfoSet>, KuhnInfoSet, usize) -> &'a mut Vec<f64> =
        RegretTable::<KuhnInfoSet>::get_or_init;
    let _: for<'a, 'b> fn(&'a RegretTable<KuhnInfoSet>, &'b KuhnInfoSet, usize) -> Vec<f64> =
        RegretTable::<KuhnInfoSet>::current_strategy;
    let _: for<'a, 'b> fn(&'a mut RegretTable<KuhnInfoSet>, KuhnInfoSet, &'b [f64]) =
        RegretTable::<KuhnInfoSet>::accumulate;
    let _: for<'a> fn(&'a RegretTable<KuhnInfoSet>) -> usize = RegretTable::<KuhnInfoSet>::len;
    let _: for<'a> fn(&'a RegretTable<KuhnInfoSet>) -> bool = RegretTable::<KuhnInfoSet>::is_empty;

    let _: fn() -> StrategyAccumulator<KuhnInfoSet> = StrategyAccumulator::<KuhnInfoSet>::new;
    let _: fn() -> StrategyAccumulator<KuhnInfoSet> =
        <StrategyAccumulator<KuhnInfoSet> as Default>::default;
    let _: for<'a, 'b> fn(&'a mut StrategyAccumulator<KuhnInfoSet>, KuhnInfoSet, &'b [f64]) =
        StrategyAccumulator::<KuhnInfoSet>::accumulate;
    let _: for<'a, 'b> fn(
        &'a StrategyAccumulator<KuhnInfoSet>,
        &'b KuhnInfoSet,
        usize,
    ) -> Vec<f64> = StrategyAccumulator::<KuhnInfoSet>::average_strategy;
    let _: for<'a> fn(&'a StrategyAccumulator<KuhnInfoSet>) -> usize =
        StrategyAccumulator::<KuhnInfoSet>::len;
    let _: for<'a> fn(&'a StrategyAccumulator<KuhnInfoSet>) -> bool =
        StrategyAccumulator::<KuhnInfoSet>::is_empty;

    // ===================================================================
    // training::trainer (api §2)
    // ===================================================================

    // VanillaCfrTrainer<KuhnGame> 构造 + Trainer trait 全 6 方法 UFCS。
    let _: fn(KuhnGame, u64) -> VanillaCfrTrainer<KuhnGame> = VanillaCfrTrainer::<KuhnGame>::new;
    let _: for<'a, 'b> fn(
        &'a mut VanillaCfrTrainer<KuhnGame>,
        &'b mut dyn RngSource,
    ) -> Result<(), TrainerError> = <VanillaCfrTrainer<KuhnGame> as Trainer<KuhnGame>>::step;
    let _: for<'a, 'b> fn(&'a VanillaCfrTrainer<KuhnGame>, &'b KuhnInfoSet) -> Vec<f64> =
        <VanillaCfrTrainer<KuhnGame> as Trainer<KuhnGame>>::current_strategy;
    let _: for<'a, 'b> fn(&'a VanillaCfrTrainer<KuhnGame>, &'b KuhnInfoSet) -> Vec<f64> =
        <VanillaCfrTrainer<KuhnGame> as Trainer<KuhnGame>>::average_strategy;
    let _: for<'a> fn(&'a VanillaCfrTrainer<KuhnGame>) -> u64 =
        <VanillaCfrTrainer<KuhnGame> as Trainer<KuhnGame>>::update_count;
    let _: for<'a, 'b> fn(
        &'a VanillaCfrTrainer<KuhnGame>,
        &'b Path,
    ) -> Result<(), CheckpointError> =
        <VanillaCfrTrainer<KuhnGame> as Trainer<KuhnGame>>::save_checkpoint;
    let _: for<'a> fn(&'a Path, KuhnGame) -> Result<VanillaCfrTrainer<KuhnGame>, CheckpointError> =
        <VanillaCfrTrainer<KuhnGame> as Trainer<KuhnGame>>::load_checkpoint;

    // EsMccfrTrainer<SimplifiedNlheGame> 构造 + Trainer trait 全 6 方法 UFCS +
    // step_parallel inherent method。
    let _: fn(SimplifiedNlheGame, u64) -> EsMccfrTrainer<SimplifiedNlheGame> =
        EsMccfrTrainer::<SimplifiedNlheGame>::new;
    let _: for<'a, 'b> fn(
        &'a mut EsMccfrTrainer<SimplifiedNlheGame>,
        &'b mut [Box<dyn RngSource>],
        usize,
    ) -> Result<(), TrainerError> = EsMccfrTrainer::<SimplifiedNlheGame>::step_parallel;
    let _: for<'a, 'b> fn(
        &'a mut EsMccfrTrainer<SimplifiedNlheGame>,
        &'b mut dyn RngSource,
    ) -> Result<(), TrainerError> =
        <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::step;
    let _: for<'a, 'b> fn(
        &'a EsMccfrTrainer<SimplifiedNlheGame>,
        &'b SimplifiedNlheInfoSet,
    ) -> Vec<f64> =
        <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::current_strategy;
    let _: for<'a, 'b> fn(
        &'a EsMccfrTrainer<SimplifiedNlheGame>,
        &'b SimplifiedNlheInfoSet,
    ) -> Vec<f64> =
        <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::average_strategy;
    let _: for<'a> fn(&'a EsMccfrTrainer<SimplifiedNlheGame>) -> u64 =
        <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::update_count;
    let _: for<'a, 'b> fn(
        &'a EsMccfrTrainer<SimplifiedNlheGame>,
        &'b Path,
    ) -> Result<(), CheckpointError> =
        <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::save_checkpoint;
    let _: for<'a> fn(
        &'a Path,
        SimplifiedNlheGame,
    ) -> Result<EsMccfrTrainer<SimplifiedNlheGame>, CheckpointError> =
        <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::load_checkpoint;

    // ===================================================================
    // training::best_response (api §4)
    // ===================================================================

    // BestResponse<G>::compute UFCS × KuhnBestResponse / LeducBestResponse。
    let _: for<'a, 'b> fn(
        &'a KuhnGame,
        &'b dyn Fn(&KuhnInfoSet, usize) -> Vec<f64>,
        PlayerId,
    ) -> (HashMap<KuhnInfoSet, Vec<f64>>, f64) =
        <KuhnBestResponse as BestResponse<KuhnGame>>::compute;
    let _: for<'a, 'b> fn(
        &'a LeducGame,
        &'b dyn Fn(&LeducInfoSet, usize) -> Vec<f64>,
        PlayerId,
    ) -> (HashMap<LeducInfoSet, Vec<f64>>, f64) =
        <LeducBestResponse as BestResponse<LeducGame>>::compute;

    // exploitability<G, BR> 泛型函数 × <KuhnGame, KuhnBestResponse> 1 instantiation
    // （泛型本身无法直接绑 fn-pointer，但具体实例化后类型固定）。
    let _: for<'a, 'b> fn(&'a KuhnGame, &'b dyn Fn(&KuhnInfoSet, usize) -> Vec<f64>) -> f64 =
        exploitability::<KuhnGame, KuhnBestResponse>;
    let _: for<'a, 'b> fn(&'a LeducGame, &'b dyn Fn(&LeducInfoSet, usize) -> Vec<f64>) -> f64 =
        exploitability::<LeducGame, LeducBestResponse>;

    // ===================================================================
    // training::checkpoint (api §5)
    // ===================================================================

    let _: for<'a, 'b> fn(&'a Checkpoint, &'b Path) -> Result<(), CheckpointError> =
        Checkpoint::save;
    let _: for<'a> fn(&'a Path) -> Result<Checkpoint, CheckpointError> = Checkpoint::open;

    // MAGIC / SCHEMA_VERSION 常量值锁（任一漂移 cargo test --no-run 失败）。
    let _: [u8; 8] = MAGIC;
    let _: u32 = SCHEMA_VERSION;

    // ===================================================================
    // training::sampling (api §6)
    // ===================================================================

    // derive_substream_seed: SplitMix64 finalizer × 4 → 32 byte（API-330 / D-335）。
    let _: fn(u64, u64, u64) -> [u8; 32] = derive_substream_seed;

    // sample_discrete<A: Copy> 泛型函数 × KuhnAction 1 instantiation。
    let _: for<'a, 'b> fn(&'a [(KuhnAction, f64)], &'b mut dyn RngSource) -> KuhnAction =
        sample_discrete::<KuhnAction>;

    // 6 个 op_id 常量值锁（任一重命名 / 数值漂移 cargo test --no-run 失败）。
    let _: u64 = OP_KUHN_DEAL;
    let _: u64 = OP_LEDUC_DEAL;
    let _: u64 = OP_NLHE_DEAL;
    let _: u64 = OP_OPP_ACTION_SAMPLE;
    let _: u64 = OP_CHANCE_SAMPLE;
    let _: u64 = OP_TRAVERSER_TIE;

    // ===================================================================
    // training::error (api §2 / §5 错误枚举 + 桥接 enum)
    // ===================================================================

    // TrainerVariant / GameVariant 物理位置在 src/error.rs（D-374），逻辑路径
    // poker::{TrainerVariant, GameVariant}（顶层 re-export 自 training）。
    let _: TrainerVariant = TrainerVariant::VanillaCfr;
    let _: TrainerVariant = TrainerVariant::EsMccfr;
    let _: GameVariant = GameVariant::Kuhn;
    let _: GameVariant = GameVariant::Leduc;
    let _: GameVariant = GameVariant::SimplifiedNlhe;

    // CheckpointError 5 variant 构造 trip-wire（API-351 / D-351，D1 \[测试\] 同
    // commit 落地；继承 stage 1 + stage 2 错误枚举追加不删模式）。任一 variant
    // 重命名 / 字段类型 / 字段名漂移立即在 `cargo test --no-run` 失败。变体语义
    // 索引：
    //   ① FileNotFound { path: PathBuf }
    //   ② SchemaMismatch { expected: u32, got: u32 }
    //   ③ TrainerMismatch { expected: (TrainerVariant, GameVariant),
    //                       got: (TrainerVariant, GameVariant) }
    //   ④ BucketTableMismatch { expected: [u8; 32], got: [u8; 32] }
    //   ⑤ Corrupted { offset: u64, reason: String }
    let _: CheckpointError = CheckpointError::FileNotFound {
        path: std::path::PathBuf::from("/tmp/api-sig"),
    };
    let _: CheckpointError = CheckpointError::SchemaMismatch {
        expected: 1u32,
        got: 0u32,
    };
    let _: CheckpointError = CheckpointError::TrainerMismatch {
        expected: (TrainerVariant::VanillaCfr, GameVariant::Kuhn),
        got: (TrainerVariant::EsMccfr, GameVariant::SimplifiedNlhe),
    };
    let _: CheckpointError = CheckpointError::BucketTableMismatch {
        expected: [0u8; 32],
        got: [0u8; 32],
    };
    let _: CheckpointError = CheckpointError::Corrupted {
        offset: 0u64,
        reason: String::new(),
    };

    // TrainerError 5 variant 构造 trip-wire（API-313 / D-324 / D-325 / D-323 /
    // D-330 / D-351 propagation；F1 \[测试\] 同 commit 落地，stage 3 出口前最后一次
    // 签名 lock 入口）。继承 stage 1 + stage 2 错误枚举追加不删模式 + 与
    // CheckpointError 5 variant lock 同型。变体语义索引：
    //   ① ActionCountMismatch { info_set: String, expected: usize, got: usize }
    //   ② OutOfMemory { rss_bytes: u64, limit: u64 }
    //   ③ UnsupportedBucketTable { expected: u32, got: u32 }
    //   ④ ProbabilitySumOutOfTolerance { got: f64, tolerance: f64 }
    //   ⑤ Checkpoint(#[from] CheckpointError) — `From<CheckpointError>` 自动 dispatch
    //
    // **doc drift 备注**：`pluribus_stage3_api.md` §API-313 落地形态把 ⑤ 写为
    // `CheckpointError(#[from] CheckpointError)`（变体名同 payload 类型名）；
    // D2 \[实现\] 落地的代码形态为 `Checkpoint(#[from] CheckpointError)`（变体名简
    // 化）。本 trip-wire 锁代码形态——文档 drift 走 F2 \[实现\] 同 commit 修复
    // （继承 stage 2 §F2 字面 "0 产品代码改动 carve-out closure（合并 commit 修
    // doc drift）" 模式）。
    let _: TrainerError = TrainerError::ActionCountMismatch {
        info_set: String::new(),
        expected: 0usize,
        got: 0usize,
    };
    let _: TrainerError = TrainerError::OutOfMemory {
        rss_bytes: 0u64,
        limit: 0u64,
    };
    let _: TrainerError = TrainerError::UnsupportedBucketTable {
        expected: 0u32,
        got: 0u32,
    };
    let _: TrainerError = TrainerError::ProbabilitySumOutOfTolerance {
        got: 0.0f64,
        tolerance: 0.0f64,
    };
    let _: TrainerError = TrainerError::Checkpoint(CheckpointError::Corrupted {
        offset: 0u64,
        reason: String::new(),
    });
    // From<CheckpointError> for TrainerError 自动 dispatch trip-wire（API-313
    // `#[from]` attribute；let `?` 操作符跨 `Result<_, CheckpointError>` →
    // `Result<_, TrainerError>` 转换继续可用）。
    let _: fn(CheckpointError) -> TrainerError = <TrainerError as From<CheckpointError>>::from;

    // ===================================================================
    // training::game (api §1 trait const + 默认方法 — D2 \[实现\] 落地的 surface 扩展)
    // ===================================================================

    // Game::VARIANT const lock × 3 impl（API-300-rev1 lock 在 D2 \[实现\] 落地，
    // F1 同 commit 锁定不变量；任一 const 重命名 / 类型改动 / 数值翻面立即在
    // `cargo test --no-run` 失败）。
    let _: GameVariant = <KuhnGame as Game>::VARIANT;
    let _: GameVariant = <LeducGame as Game>::VARIANT;
    let _: GameVariant = <SimplifiedNlheGame as Game>::VARIANT;

    // Game::bucket_table_blake3 默认方法 UFCS lock × 3 impl（API-300-rev1 D2
    // 落地的默认方法；KuhnGame / LeducGame 走 default 返回 [0; 32]；
    // SimplifiedNlheGame override 返回 self.bucket_table.content_hash()）。
    let _: for<'a> fn(&'a KuhnGame) -> [u8; 32] = <KuhnGame as Game>::bucket_table_blake3;
    let _: for<'a> fn(&'a LeducGame) -> [u8; 32] = <LeducGame as Game>::bucket_table_blake3;
    let _: for<'a> fn(&'a SimplifiedNlheGame) -> [u8; 32] =
        <SimplifiedNlheGame as Game>::bucket_table_blake3;

    // ===================================================================
    // training::checkpoint (api §5 — D2 \[实现\] 落地的 pub field + helper 常量)
    // ===================================================================

    // Checkpoint pub field 类型 lock（API-350 / D-350 binary header offset 表 +
    // stage 4 D-449 新增 4 字段，§D2-revM 字面继承 D1 \[测试\] line 880-887
    // 内置 TODO 授权扩展）。任一字段类型 / 顺序漂移立即在 `cargo test --no-run` 失败。
    let ckpt = Checkpoint {
        schema_version: 1u32,
        trainer_variant: TrainerVariant::VanillaCfr,
        game_variant: GameVariant::Kuhn,
        update_count: 0u64,
        rng_state: [0u8; 32],
        bucket_table_blake3: [0u8; 32],
        regret_table_bytes: Vec::new(),
        strategy_sum_bytes: Vec::new(),
        // stage 4 D-449 / API-441 4 新字段（D1 \[测试\] line 880-887 TODO 字面）
        traverser_count: 1u8,
        linear_weighting_enabled: false,
        rm_plus_enabled: false,
        warmup_complete: false,
    };
    let _: u32 = ckpt.schema_version;
    let _: TrainerVariant = ckpt.trainer_variant;
    let _: GameVariant = ckpt.game_variant;
    let _: u64 = ckpt.update_count;
    let _: [u8; 32] = ckpt.rng_state;
    let _: [u8; 32] = ckpt.bucket_table_blake3;
    let _: Vec<u8> = ckpt.regret_table_bytes;
    let _: Vec<u8> = ckpt.strategy_sum_bytes;
    let _: u8 = ckpt.traverser_count;
    let _: bool = ckpt.linear_weighting_enabled;
    let _: bool = ckpt.rm_plus_enabled;
    let _: bool = ckpt.warmup_complete;

    // HEADER_LEN / TRAILER_LEN 常量值锁（D-350 binary layout 头号不变量；与
    // checkpoint_round_trip.rs::d350_binary_layout_offsets_lock 双重锁定）。
    use poker::training::checkpoint::{HEADER_LEN, TRAILER_LEN};
    let _: usize = HEADER_LEN;
    let _: usize = TRAILER_LEN;

    // TrainerVariant / GameVariant `from_u8` 反序列化 helper UFCS lock（D-350
    // binary header offset 12/13 → enum 解析路径）。
    let _: fn(u8) -> Option<TrainerVariant> = TrainerVariant::from_u8;
    let _: fn(u8) -> Option<GameVariant> = GameVariant::from_u8;
}

#[allow(dead_code, clippy::type_complexity, clippy::unit_arg)]
fn _stage4_api_signature_assertions() {
    // ===================================================================
    // stage 4 A1 [实现] scaffold — API-400..API-499 trip-wire（继承 stage 1 +
    // stage 2 + stage 3 同型 lock 模式）。
    //
    // 任一 stage 4 公开 API 签名漂移立即在 `cargo test --no-run` 失败。
    // ===================================================================

    use poker::abstraction::action_pluribus::{PluribusAction, PluribusActionAbstraction};
    use poker::training::lbr::{LbrEvaluator, LbrResult, SixTraverserLbrResult};
    use poker::training::metrics::{MetricsCollector, TrainingAlarm, TrainingMetrics};
    use poker::training::nlhe_6max::{NlheGame6, NlheGame6Action, NlheGame6State};
    use poker::training::slumbot_eval::{
        Head2HeadResult, HuHandResult, OpenSpielHuBaseline, SlumbotBridge, SlumbotHandResult,
    };
    use poker::training::trainer::{DecayStrategy, EsMccfrTrainer, TrainerConfig};
    use poker::{BucketTable, ChipAmount, GameState, InfoSetId, PlayerId, RngSource};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    // ---------------------------------------------------------------
    // API-411 — GameVariant::Nlhe6Max 4th variant + from_u8(3)
    // ---------------------------------------------------------------
    let _: GameVariant = GameVariant::Nlhe6Max;
    let _: Option<GameVariant> = GameVariant::from_u8(3);

    // ---------------------------------------------------------------
    // API-441 — TrainerVariant::EsMccfrLinearRmPlus 3rd variant + from_u8(2)
    // ---------------------------------------------------------------
    let _: TrainerVariant = TrainerVariant::EsMccfrLinearRmPlus;
    let _: Option<TrainerVariant> = TrainerVariant::from_u8(2);

    // ---------------------------------------------------------------
    // API-420 — PluribusAction 14-variant enum + N_ACTIONS / all() /
    // raise_multiplier() / from_u8 helper lock
    // ---------------------------------------------------------------
    let _: usize = PluribusAction::N_ACTIONS;
    let _: fn() -> [PluribusAction; 14] = PluribusAction::all;
    let _: fn(PluribusAction) -> Option<f64> = PluribusAction::raise_multiplier;
    let _: fn(u8) -> Option<PluribusAction> = PluribusAction::from_u8;
    // 14 variant byte-equal 顺序锁（与 D-420 字面 §S3 Pluribus 主论文一致）。
    let actions = PluribusAction::all();
    let _: PluribusAction = actions[0]; // Fold
    let _: PluribusAction = actions[13]; // AllIn
    let _: u8 = PluribusAction::AllIn as u8;

    // ---------------------------------------------------------------
    // API-420 / API-421 — PluribusActionAbstraction inherent method lock
    // ---------------------------------------------------------------
    let _: fn() -> PluribusActionAbstraction = <PluribusActionAbstraction as Default>::default;
    let _: for<'a, 'b> fn(&'a PluribusActionAbstraction, &'b GameState) -> Vec<PluribusAction> =
        PluribusActionAbstraction::actions;
    let _: for<'a, 'b, 'c> fn(
        &'a PluribusActionAbstraction,
        &'b PluribusAction,
        &'c GameState,
    ) -> bool = PluribusActionAbstraction::is_legal;
    let _: for<'a, 'b> fn(&'a PluribusActionAbstraction, &'b GameState, f64) -> ChipAmount =
        PluribusActionAbstraction::compute_raise_to;

    // ---------------------------------------------------------------
    // API-423 — InfoSetId::with_14action_mask / legal_actions_mask_14 lock
    // ---------------------------------------------------------------
    let _: fn(InfoSetId, u16) -> InfoSetId = InfoSetId::with_14action_mask;
    let _: fn(InfoSetId) -> u16 = InfoSetId::legal_actions_mask_14;

    // ---------------------------------------------------------------
    // API-410 — NlheGame6 inherent method lock + Game trait const VARIANT
    // ---------------------------------------------------------------
    let _: fn(Arc<BucketTable>) -> Result<NlheGame6, TrainerError> = NlheGame6::new;
    let _: fn(Arc<BucketTable>) -> Result<NlheGame6, TrainerError> = NlheGame6::new_hu;
    let _: fn(Arc<BucketTable>, poker::TableConfig) -> Result<NlheGame6, TrainerError> =
        NlheGame6::with_config;
    let _: fn(u64) -> PlayerId = NlheGame6::traverser_at_iter;
    let _: fn(u64, usize) -> PlayerId = NlheGame6::traverser_for_thread;
    let _: for<'a> fn(&'a NlheGame6State, poker::SeatId) -> PlayerId = NlheGame6::actor_at_seat;
    let _: for<'a> fn(&'a GameState) -> u16 = NlheGame6::compute_14action_mask;
    let _: GameVariant = <NlheGame6 as Game>::VARIANT;
    let _: for<'a> fn(&'a NlheGame6) -> [u8; 32] = <NlheGame6 as Game>::bucket_table_blake3;

    // NlheGame6Action / NlheGame6InfoSet 类型 alias lock（编译期 = 等价
    // PluribusAction / InfoSetId）。
    let _act: NlheGame6Action = NlheGame6Action::Fold;
    let _: PluribusAction = _act;

    // ---------------------------------------------------------------
    // C1 [测试] — NlheGame6 as Game trait 8 method UFCS lock（D-410 / D-411
    // / D-420 / D-422 / D-423 字面，C2 [实现] 起步前签名漂移立即在 cargo test
    // --no-run 暴露；A1 scaffold 8 method 全 `unimplemented!()`）。
    // ---------------------------------------------------------------
    let _: for<'a> fn(&'a NlheGame6) -> usize = <NlheGame6 as Game>::n_players;
    let _: for<'a, 'b> fn(&'a NlheGame6, &'b mut dyn RngSource) -> NlheGame6State =
        <NlheGame6 as Game>::root;
    let _: for<'a> fn(&'a NlheGame6State) -> poker::training::game::NodeKind =
        <NlheGame6 as Game>::current;
    let _: for<'a> fn(&'a NlheGame6State, PlayerId) -> NlheGame6InfoSet =
        <NlheGame6 as Game>::info_set;
    let _: for<'a> fn(&'a NlheGame6State) -> Vec<NlheGame6Action> =
        <NlheGame6 as Game>::legal_actions;
    let _: for<'a> fn(NlheGame6State, NlheGame6Action, &'a mut dyn RngSource) -> NlheGame6State =
        <NlheGame6 as Game>::next;
    let _: for<'a> fn(&'a NlheGame6State) -> Vec<(NlheGame6Action, f64)> =
        <NlheGame6 as Game>::chance_distribution;
    let _: for<'a> fn(&'a NlheGame6State, PlayerId) -> f64 = <NlheGame6 as Game>::payoff;

    // ---------------------------------------------------------------
    // C1 [测试] — Checkpoint v2 schema header layout 常量 lock（API-440 /
    // D-449 字面）。stage 3 字面 SCHEMA_VERSION = 1 / HEADER_LEN = 108；stage 4
    // D2 [实现] 起步落地 bump 到 2 / 128，本 UFCS 仅 const ref bind 让 const
    // 类型 / 名称漂移立即在 cargo test --no-run 暴露（实际数值由
    // tests/checkpoint_v2_schema.rs 验证 panic-fail 直到 D2 落地）。
    // ---------------------------------------------------------------
    let _: u32 = poker::training::checkpoint::SCHEMA_VERSION;
    let _: usize = poker::training::checkpoint::HEADER_LEN;
    let _: usize = poker::training::checkpoint::TRAILER_LEN;
    let _: [u8; 8] = poker::training::checkpoint::MAGIC;

    // ---------------------------------------------------------------
    // D1 [测试] — Checkpoint v2 schema header field byte offset 字面 sanity
    // （API-440 字面 128-byte layout）。本 const 字面只作为 trip-wire 锚点；
    // 实际数值断言落 tests/checkpoint_v2_schema.rs::
    // checkpoint_v2_layout_offsets_match_api_440_spec + tests/checkpoint_v2_round_trip.rs。
    //
    // D2 [实现] 落地 src/training/checkpoint.rs 新增 const 字段时（OFFSET_TRAVERSER_COUNT
    // = 14 / OFFSET_LINEAR_WEIGHTING = 15 / OFFSET_RM_PLUS = 16 / OFFSET_WARMUP_COMPLETE
    // = 17 / OFFSET_REGRET_OFFSET = 96 / OFFSET_STRATEGY_OFFSET = 104 / OFFSET_PAD_B
    // = 112），这里加 UFCS const ref bind 让漂移立即在 cargo test --no-run 暴露。
    // 当前 src/training/checkpoint.rs 未暴露这些 const，本块仅 1 个字面 trip-wire
    // 通过本测试模块内部 const 字面让 D1 [测试] 期望 layout 字段不漂移。
    // ---------------------------------------------------------------
    {
        // D-449 字面 6 个新字段 offset 字面 lock（与 tests/checkpoint_v2_round_trip.rs
        // 同 const 字面，D2 [实现] 落地新增 src/training/checkpoint.rs::OFFSET_*
        // 后翻面走 `let _: usize = poker::training::checkpoint::OFFSET_TRAVERSER_COUNT;`
        // 形式 UFCS bind）。
        const OFFSET_TRAVERSER_COUNT: usize = 14;
        const OFFSET_LINEAR_WEIGHTING: usize = 15;
        const OFFSET_RM_PLUS: usize = 16;
        const OFFSET_WARMUP_COMPLETE: usize = 17;
        const OFFSET_REGRET_OFFSET: usize = 96;
        const OFFSET_STRATEGY_OFFSET: usize = 104;
        const OFFSET_PAD_B: usize = 112;
        const HEADER_V2_LEN: usize = 128;
        const _: () = assert!(OFFSET_LINEAR_WEIGHTING == OFFSET_TRAVERSER_COUNT + 1);
        const _: () = assert!(OFFSET_RM_PLUS == OFFSET_LINEAR_WEIGHTING + 1);
        const _: () = assert!(OFFSET_WARMUP_COMPLETE == OFFSET_RM_PLUS + 1);
        const _: () = assert!(OFFSET_STRATEGY_OFFSET == OFFSET_REGRET_OFFSET + 8);
        const _: () = assert!(OFFSET_PAD_B == OFFSET_STRATEGY_OFFSET + 8);
        const _: () = assert!(HEADER_V2_LEN == OFFSET_PAD_B + 16);
        let _: usize = OFFSET_TRAVERSER_COUNT;
        let _: usize = OFFSET_LINEAR_WEIGHTING;
        let _: usize = OFFSET_RM_PLUS;
        let _: usize = OFFSET_WARMUP_COMPLETE;
        let _: usize = OFFSET_REGRET_OFFSET;
        let _: usize = OFFSET_STRATEGY_OFFSET;
        let _: usize = OFFSET_PAD_B;
        let _: usize = HEADER_V2_LEN;
    }

    // ---------------------------------------------------------------
    // D1 [测试] — Checkpoint pub field 类型 lock（API-441 字面）。
    // stage 3 Checkpoint struct 7 字段已在 _stage3 段 line 663-682 lock；本块
    // 锁 stage 4 D2 [实现] 起步前**预期**新增的 4 个字段（traverser_count: u8 /
    // linear_weighting_enabled: bool / rm_plus_enabled: bool / warmup_complete: bool）。
    //
    // 字段尚未落地时（A1/B2/C2 commit 形态），本块通过本测试模块内部 const 字面
    // 锁 stage 4 期望字段名 — D2 [实现] 落地 src/training/checkpoint.rs 加字段
    // 后翻面成 `let _: u8 = ckpt.traverser_count;` 形式（与 _stage3 line 675-682
    // 同型）。当前形态：4 字面 const 锁字段类型 + 数值范围 sanity。
    // ---------------------------------------------------------------
    {
        const EXPECTED_TRAVERSER_COUNT_NLHE6: u8 = 6;
        const EXPECTED_TRAVERSER_COUNT_STAGE3: u8 = 1;
        const EXPECTED_LINEAR_WEIGHTING_ON: bool = true;
        const EXPECTED_RM_PLUS_ON: bool = true;
        const EXPECTED_WARMUP_INITIAL: bool = false;
        const EXPECTED_WARMUP_POST: bool = true;
        let _: u8 = EXPECTED_TRAVERSER_COUNT_NLHE6;
        let _: u8 = EXPECTED_TRAVERSER_COUNT_STAGE3;
        let _: bool = EXPECTED_LINEAR_WEIGHTING_ON;
        let _: bool = EXPECTED_RM_PLUS_ON;
        let _: bool = EXPECTED_WARMUP_INITIAL;
        let _: bool = EXPECTED_WARMUP_POST;
        // D2 [实现] 落地后该 block 替换为：
        //   let ckpt_v2 = Checkpoint { ..stage3_fields, traverser_count: 6,
        //       linear_weighting_enabled: true, rm_plus_enabled: true,
        //       warmup_complete: true };
        //   let _: u8 = ckpt_v2.traverser_count;
        //   let _: bool = ckpt_v2.linear_weighting_enabled;
        //   ...
    }

    // ---------------------------------------------------------------
    // API-401 — TrainerConfig fields + DecayStrategy enum + Default
    // ---------------------------------------------------------------
    let cfg = TrainerConfig {
        n_threads: 1,
        checkpoint_interval: 0,
        metrics_interval: 100_000,
        linear_weighting_enabled: false,
        rm_plus_enabled: false,
        warmup_complete_at: 1_000_000,
        decay_strategy: DecayStrategy::EagerDecay,
        parallel_batch_size: 1,
    };
    let _: u8 = cfg.n_threads;
    let _: u64 = cfg.checkpoint_interval;
    let _: u64 = cfg.metrics_interval;
    let _: bool = cfg.linear_weighting_enabled;
    let _: bool = cfg.rm_plus_enabled;
    let _: u64 = cfg.warmup_complete_at;
    let _: DecayStrategy = cfg.decay_strategy;
    let _: usize = cfg.parallel_batch_size;
    let _: TrainerConfig = TrainerConfig::default();
    let _: DecayStrategy = DecayStrategy::EagerDecay;
    let _: DecayStrategy = DecayStrategy::LazyDecay;

    // ---------------------------------------------------------------
    // API-400 — EsMccfrTrainer::with_linear_rm_plus builder + config()
    // §E-rev2 / A2 — with_parallel_batch_size builder（AWS c7a.8xlarge
    // profiling 触发,详见 docs/pluribus_stage4_profiling.md）。
    // ---------------------------------------------------------------
    let _: fn(EsMccfrTrainer<NlheGame6>, u64) -> EsMccfrTrainer<NlheGame6> =
        EsMccfrTrainer::<NlheGame6>::with_linear_rm_plus;
    let _: for<'a> fn(&'a EsMccfrTrainer<NlheGame6>) -> &'a TrainerConfig =
        EsMccfrTrainer::<NlheGame6>::config;
    let _: fn(EsMccfrTrainer<NlheGame6>, usize) -> EsMccfrTrainer<NlheGame6> =
        EsMccfrTrainer::<NlheGame6>::with_parallel_batch_size;

    // ---------------------------------------------------------------
    // API-450 / API-451 — LbrEvaluator + LbrResult / SixTraverserLbrResult
    // ---------------------------------------------------------------
    let _: fn(
        Arc<EsMccfrTrainer<NlheGame6>>,
        usize,
        u8,
    ) -> Result<LbrEvaluator<NlheGame6>, TrainerError> = LbrEvaluator::<NlheGame6>::new;
    // compute / compute_six_traverser_average 入参含 trait object dyn RngSource，
    // fn 指针表达走 for<'a, 'b> closure 形态。
    let _: for<'a, 'b> fn(
        &'a LbrEvaluator<NlheGame6>,
        PlayerId,
        u64,
        &'b mut dyn RngSource,
    ) -> Result<LbrResult, TrainerError> = LbrEvaluator::<NlheGame6>::compute;
    let _: for<'a, 'b> fn(
        &'a LbrEvaluator<NlheGame6>,
        u64,
        &'b mut dyn RngSource,
    ) -> Result<SixTraverserLbrResult, TrainerError> =
        LbrEvaluator::<NlheGame6>::compute_six_traverser_average;
    let _: for<'a, 'b> fn(&'a LbrEvaluator<NlheGame6>, &'b Path) -> Result<(), TrainerError> =
        LbrEvaluator::<NlheGame6>::export_policy_for_openspiel;

    // LbrResult / SixTraverserLbrResult 字段 lock
    let lr = LbrResult {
        lbr_player: 0,
        lbr_value_mbbg: 0.0,
        standard_error_mbbg: 0.0,
        n_hands: 0,
        computation_seconds: 0.0,
    };
    let _: PlayerId = lr.lbr_player;
    let _: f64 = lr.lbr_value_mbbg;
    let _: f64 = lr.standard_error_mbbg;
    let _: u64 = lr.n_hands;
    let _: f64 = lr.computation_seconds;

    let stlr = SixTraverserLbrResult {
        per_traverser: [
            lr.clone(),
            lr.clone(),
            lr.clone(),
            lr.clone(),
            lr.clone(),
            lr.clone(),
        ],
        average_mbbg: 0.0,
        max_mbbg: 0.0,
        min_mbbg: 0.0,
    };
    let _: [LbrResult; 6] = stlr.per_traverser;
    let _: f64 = stlr.average_mbbg;
    let _: f64 = stlr.max_mbbg;
    let _: f64 = stlr.min_mbbg;

    // ---------------------------------------------------------------
    // API-460 / API-461 / API-462 — SlumbotBridge + Head2HeadResult /
    // SlumbotHandResult + OpenSpielHuBaseline / HuHandResult
    // ---------------------------------------------------------------
    let _: fn(String) -> SlumbotBridge = SlumbotBridge::new;
    // SlumbotBridge::play_one_hand / evaluate_blueprint 走 impl<T: Trainer<NlheGame6>>
    // 泛型方法，fn 指针无法表达泛型 — 单测在编译期通过 mod-level resolution 自动
    // 校验签名（与 stage 1 RngCoreAdapter::from_rng_core 同型 skip）。

    let h2h = Head2HeadResult {
        mean_mbbg: 0.0,
        standard_error_mbbg: 0.0,
        confidence_interval_95: (0.0, 0.0),
        n_hands: 0,
        duplicate_dealing: false,
        blueprint_seed: 0,
        wall_clock_seconds: 0.0,
    };
    let _: f64 = h2h.mean_mbbg;
    let _: (f64, f64) = h2h.confidence_interval_95;
    let _: bool = h2h.duplicate_dealing;

    let shr = SlumbotHandResult {
        blueprint_chip_delta: 0,
        mbb_delta: 0.0,
        seed: 0,
        wall_clock_seconds: 0.0,
    };
    let _: i64 = shr.blueprint_chip_delta;
    let _: f64 = shr.mbb_delta;

    let _: fn(PathBuf) -> OpenSpielHuBaseline = OpenSpielHuBaseline::new;

    let hu = HuHandResult {
        blueprint_chip_delta: 0,
        mbb_delta: 0.0,
        seed: 0,
        wall_clock_seconds: 0.0,
    };
    let _: i64 = hu.blueprint_chip_delta;

    // ---------------------------------------------------------------
    // API-470 / API-471 — TrainingMetrics 9 字段 + TrainingAlarm 5 variant
    // ---------------------------------------------------------------
    let _: fn() -> TrainingMetrics = TrainingMetrics::zero;
    let tm = TrainingMetrics::zero();
    let _: u64 = tm.update_count;
    let _: f64 = tm.wall_clock_seconds;
    let _: f64 = tm.avg_regret_growth_rate;
    let _: u8 = tm.regret_growth_trend_up_count;
    let _: f64 = tm.policy_entropy;
    let _: f64 = tm.policy_oscillation;
    let _: u64 = tm.peak_rss_bytes;
    let _: f64 = tm.ev_sum_residual;
    let _: Option<TrainingAlarm> = tm.last_alarm;

    // 5 variant exhaustive sample（与 trainer_error_boundary.rs::trainer_error_6
    // _variants_exhaustive_match_lock 同型 trip-wire）。
    let _alarms: [TrainingAlarm; 5] = [
        TrainingAlarm::RegretGrowthTrendUp {
            trend_up_count: 5,
            last_sample_t: 100,
        },
        TrainingAlarm::EntropyRising { delta_pct: 5.5 },
        TrainingAlarm::OscillationTrendUp,
        TrainingAlarm::OutOfMemory {
            rss_bytes: 1,
            limit_bytes: 0,
        },
        TrainingAlarm::EvSumViolation {
            residual: 1.0,
            tolerance: 1e-3,
        },
    ];

    // ---------------------------------------------------------------
    // API-473 — MetricsCollector::new constructor
    // ---------------------------------------------------------------
    let _: fn(u64) -> MetricsCollector = MetricsCollector::new;
    // MetricsCollector::observe 走 impl<T: Trainer<NlheGame6>> 泛型，fn 指针
    // skip（同 SlumbotBridge::play_one_hand）。

    // ---------------------------------------------------------------
    // API-474 — write_metrics_jsonl 入口
    // ---------------------------------------------------------------
    // write_metrics_jsonl 走 impl<W: io::Write> 泛型，fn 指针 skip。

    // ---------------------------------------------------------------
    // API-480 — Opponent6Max trait + 3 baseline impl + BaselineEvalResult
    // ---------------------------------------------------------------
    use poker::training::baseline_eval::{
        BaselineEvalResult, CallStationOpponent, Opponent6Max, RandomOpponent, TagOpponent,
    };
    let _: RandomOpponent = RandomOpponent;
    let _: CallStationOpponent = CallStationOpponent;
    let _: TagOpponent = TagOpponent::default();
    let _: fn(&RandomOpponent) -> &'static str = <RandomOpponent as Opponent6Max>::name;
    let _: fn(&CallStationOpponent) -> &'static str = <CallStationOpponent as Opponent6Max>::name;
    let _: fn(&TagOpponent) -> &'static str = <TagOpponent as Opponent6Max>::name;

    let ber = BaselineEvalResult {
        mean_mbbg: 0.0,
        standard_error_mbbg: 0.0,
        n_hands: 0,
        opponent_name: String::new(),
        blueprint_seats: Vec::new(),
        opponent_seats: Vec::new(),
    };
    let _: f64 = ber.mean_mbbg;
    let _: String = ber.opponent_name;
    let _: Vec<usize> = ber.blueprint_seats;

    // 静态消费 `actions` / `stlr` / `_alarms` / `_act` / `_l` / 等让 unused
    // binding 不触发 -D warnings。
    let _ = actions;
    let _ = stlr;
}
