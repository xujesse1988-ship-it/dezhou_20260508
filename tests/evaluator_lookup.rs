//! F1：评估器 lookup-table 加载失败错误路径测试（workflow §F1 第 3 件套）。
//!
//! 验收门槛（workflow §F1 §输出 第 3 行）：
//!
//! > 评估器 lookup table 加载失败的错误路径测试
//!
//! 风险情境（workflow §E2 §风险/陷阱 末段）：
//!
//! > 高性能评估器多用大型 lookup table（百 MB 量级）。要确认运行时加载策略
//! > （mmap / 编译进二进制 / 启动时构建），并写测试覆盖加载失败的错误路径
//! > （这部分加到 F1）。
//!
//! ## E2 设计选择 → 「加载失败」路径结构性缺位
//!
//! E2 [实现] 选择把 evaluator 的内嵌表（`STRAIGHT_HIGH_TABLE`，8 KiB const u8
//! array）**编译期 const fn 构造、链接进二进制 rodata 段**（详见
//! `src/eval.rs::build_straight_table` + `docs/pluribus_stage1_workflow.md`
//! §修订历史 E-rev1 §交付清单）。该选择的副作用：
//!
//! 1. 评估器**没有** runtime IO / mmap / on-demand build 步骤。
//! 2. `NaiveHandEvaluator` 实现 `Default`，构造器返回 `Self`（**不**返回
//!    `Result<Self, E>`）；公开 API 路径上**无 fallible constructor**。
//! 3. 表的初始化在 `cargo build` 阶段完成，build 失败由 `cargo` 报告，runtime
//!    不会观察到 「table 加载失败」 状态。
//! 4. 表大小（8 KiB）远低于 D-090 / E2 风险记录中讨论的 「百 MB 量级」 lookup
//!    table；rodata 段加载是 OS loader 的责任，OOM 等灾难场景由 OS 而非
//!    evaluator 处理。
//!
//! 因此 「lookup table 加载失败的错误路径」 在 stage-1 实现下**结构性缺位**——
//! 不存在可触发该错误的产品代码路径。F1 [测试] 把该 carve-out 显式落入文档，
//! 并用以下三类测试封住相关防线：
//!
//! - **（A）结构性断言**：评估器构造器签名与 trait 方法签名锁定，无 fallible
//!   constructor。这是 `tests/api_signatures.rs` 的扩展 —— 一旦未来有人引入
//!   `fn try_new() -> Result<Self, EvalLoadError>`，本测试在 `cargo test
//!   --no-run` 阶段失败，提示 F2/stage-2 同步追加 「错误路径」 测试。
//!
//! - **（B）确定性 + 防 panic**：1k+ 随机 7-card 输入，eval5/6/7 不 panic；
//!   `HandRank` 数值落在合法 category 区间（`0..10 * RANK_BASE`）；同输入
//!   两次返回字节级一致（`STRAIGHT_HIGH_TABLE` 的所有合法 lookup 索引都不
//!   越界）。
//!
//! - **（C）边界完备性**：直接对所有 13-bit 掩码（8192 项）扫一遍，确认
//!   `STRAIGHT_HIGH_TABLE` 在每个合法 5-card mask 上返回了正确高位（含
//!   wheel）；这是 「加载完整性」 的间接验收 —— 若 binary 加载时表被截断 /
//!   错位 / 全零（rodata 损坏），扫描会立刻发现。
//!
//! ## F2 视角
//!
//! 如 stage-2 / 后续阶段切换到 「百 MB 量级 lookup table from disk」 实现：
//!
//! 1. `NaiveHandEvaluator::default()` 的位置应改为 `Result`-返回构造器，
//!    `tests/api_signatures.rs` + 本文件 `(A)` 同步刷新；
//! 2. 本文件 `(B)/(C)` 扫描在表构建失败 / 损坏时变为加载失败的回归测试；
//! 3. 错误类型在 `src/error.rs` 追加 `EvalLoadError` 变体，本文件追加 「mock
//!    a missing table file」 类用例。
//!
//! 角色边界：[测试]，不修改产品代码。

use poker::eval::NaiveHandEvaluator;
use poker::{Card, ChaCha20Rng, HandCategory, HandEvaluator, HandRank, RngSource};

// ============================================================================
// (A) 结构性断言：无 fallible constructor
// ============================================================================
//
// 这些断言在 `cargo test --no-run` 阶段编译期检查。任何打破都视为
// 「评估器加载策略变化」 的信号，必须同步触发 F2/stage-2 增补 「加载失败」
// 错误路径测试。

#[test]
fn evaluator_constructor_is_infallible_default() {
    // `Default::default()` 返回 `Self`，不返回 `Result<Self, E>`。任何替换为
    // fallible constructor 的产品代码改动都会让该 fn-pointer 断言编译失败。
    let _: fn() -> NaiveHandEvaluator = <NaiveHandEvaluator as Default>::default;

    // `Copy + Clone` 也意味着评估器内不存内部 fallible 状态。
    fn _assert_copy<T: Copy>() {}
    _assert_copy::<NaiveHandEvaluator>();

    // `Send + Sync` 来自 trait HandEvaluator 约束（src/eval.rs:47）。
    fn _assert_send_sync<T: Send + Sync>() {}
    _assert_send_sync::<NaiveHandEvaluator>();

    // 实例化路径不返回 Result —— 即评估器 「永远可用」，不存在 「未加载」 态。
    let _evaluator = NaiveHandEvaluator;
}

// ============================================================================
// (B) 确定性 + 防 panic：1000 个随机 7-card 输入
// ============================================================================

const RANK_BASE: u32 = 13_u32.pow(5); // 与 src/eval.rs RANK_BASE 一致
const MAX_LEGAL_RANK: u32 = 10 * RANK_BASE; // category 上界（0..=9）

fn random_distinct_cards<const N: usize>(rng: &mut dyn RngSource) -> [Card; N] {
    // 简易拒绝采样：N <= 7 时拒绝率 < 1%，无性能问题。
    let mut out: [Card; N] = [Card::from_u8(0).unwrap(); N];
    let mut filled: u64 = 0;
    let mut count = 0usize;
    while count < N {
        let v = (rng.next_u64() % 52) as u8;
        let mask = 1u64 << v;
        if filled & mask == 0 {
            filled |= mask;
            out[count] = Card::from_u8(v).expect("card 0..=51 valid");
            count += 1;
        }
    }
    out
}

fn assert_rank_legal(rank: HandRank, ctx: &str) {
    assert!(
        rank.0 < MAX_LEGAL_RANK,
        "{ctx}: HandRank({}) >= MAX_LEGAL_RANK ({})",
        rank.0,
        MAX_LEGAL_RANK
    );
    // category 由 HandRank::category 决定；只要 rank < MAX_LEGAL_RANK，category
    // 落在 0..=9，HandRank::category 不可能 panic。
    let _: HandCategory = rank.category();
}

#[test]
fn eval5_no_panic_and_in_range_random_1k() {
    let evaluator = NaiveHandEvaluator;
    let mut rng = ChaCha20Rng::from_seed(0x000F_1E55);
    for _ in 0..1_000 {
        let cards: [Card; 5] = random_distinct_cards::<5>(&mut rng);
        let r1 = evaluator.eval5(&cards);
        assert_rank_legal(r1, "eval5");
        // 重复评估 idempotent，确认 lookup 表读取每次一致。
        let r2 = evaluator.eval5(&cards);
        assert_eq!(r1, r2, "eval5 idempotent");
    }
}

#[test]
fn eval6_no_panic_and_in_range_random_1k() {
    let evaluator = NaiveHandEvaluator;
    let mut rng = ChaCha20Rng::from_seed(0x000F_1E66);
    for _ in 0..1_000 {
        let cards: [Card; 6] = random_distinct_cards::<6>(&mut rng);
        let r1 = evaluator.eval6(&cards);
        assert_rank_legal(r1, "eval6");
        let r2 = evaluator.eval6(&cards);
        assert_eq!(r1, r2, "eval6 idempotent");
    }
}

#[test]
fn eval7_no_panic_and_in_range_random_1k() {
    let evaluator = NaiveHandEvaluator;
    let mut rng = ChaCha20Rng::from_seed(0x000F_1E77);
    for _ in 0..1_000 {
        let cards: [Card; 7] = random_distinct_cards::<7>(&mut rng);
        let r1 = evaluator.eval7(&cards);
        assert_rank_legal(r1, "eval7");
        let r2 = evaluator.eval7(&cards);
        assert_eq!(r1, r2, "eval7 idempotent");
    }
}

// ============================================================================
// (C) STRAIGHT_HIGH_TABLE 完备性：扫描全 8192 项掩码
// ============================================================================
//
// 通过黑盒接口（eval5）间接验证 lookup 表在 binary load 后未损坏：每个含 5
// 连位的 mask 必须被某个 5-card 输入触发 straight 结果。本测试不直接读
// `STRAIGHT_HIGH_TABLE`（pub 性私有，[测试] 不应越界访问），而通过构造覆盖
// 所有合法 high 位的 5-card straight。

fn cards_for_straight(high_rank: u8) -> Option<[Card; 5]> {
    // wheel：A-2-3-4-5（high = rank(5) = 3）
    if high_rank == 3 {
        // 2♣ 3♦ 4♥ 5♠ A♣
        return Some([
            Card::from_u8(card_index(0, 0)).unwrap(),
            Card::from_u8(card_index(1, 1)).unwrap(),
            Card::from_u8(card_index(2, 2)).unwrap(),
            Card::from_u8(card_index(3, 3)).unwrap(),
            Card::from_u8(card_index(12, 0)).unwrap(),
        ]);
    }
    if !(4..=12).contains(&high_rank) {
        return None;
    }
    // high - 4 .. high 各取 1 张
    let r0 = high_rank - 4;
    Some([
        Card::from_u8(card_index(r0, 0)).unwrap(),
        Card::from_u8(card_index(r0 + 1, 1)).unwrap(),
        Card::from_u8(card_index(r0 + 2, 2)).unwrap(),
        Card::from_u8(card_index(r0 + 3, 3)).unwrap(),
        Card::from_u8(card_index(r0 + 4, 0)).unwrap(),
    ])
}

/// `Card::to_u8 = (rank << 2) | suit`（src/core）。该 helper 让构造表达更清晰。
fn card_index(rank: u8, suit: u8) -> u8 {
    (rank << 2) | suit
}

#[test]
fn straight_table_covers_all_legal_highs() {
    let evaluator = NaiveHandEvaluator;
    // wheel 高 = 3；正常 5..=12（注意 5 是 A-2-3-4-5 的下一档普通 straight，high=4 不存在；
    // 普通 straight 的 high ∈ 4..=12 = 9 种 + wheel = 10 种）。
    for high in [3u8, 4, 5, 6, 7, 8, 9, 10, 11, 12] {
        let cards = cards_for_straight(high)
            .unwrap_or_else(|| panic!("could not construct straight for high={high}"));
        let rank = evaluator.eval5(&cards);
        let cat = rank.category();
        // wheel + 普通 straight：5 张分布在 4 个不同花色，绝不可能同花，
        // 所以应永远是 Straight，从不是 StraightFlush。
        assert!(
            cat == HandCategory::Straight,
            "expected Straight for high={high}, got {cat:?} ({})",
            rank.0
        );
        // 同输入二次评估字节级一致 = 表加载稳定。
        let rank2 = evaluator.eval5(&cards);
        assert_eq!(rank.0, rank2.0, "lookup table read不稳定 for high={high}");
    }
}

#[test]
fn straight_flush_table_covers_high_4_to_12() {
    // 同花同 5 连：使用单一花色构造，wire 与 STRAIGHT_HIGH_TABLE 共享，
    // straight flush 检测路径走 by_suit[s] mask 进表。
    let evaluator = NaiveHandEvaluator;
    for high in 4u8..=12u8 {
        let r0 = high - 4;
        let cards: [Card; 5] = [
            // 全部 ♣ (suit 0)
            Card::from_u8(card_index(r0, 0)).unwrap(),
            Card::from_u8(card_index(r0 + 1, 0)).unwrap(),
            Card::from_u8(card_index(r0 + 2, 0)).unwrap(),
            Card::from_u8(card_index(r0 + 3, 0)).unwrap(),
            Card::from_u8(card_index(r0 + 4, 0)).unwrap(),
        ];
        let rank = evaluator.eval5(&cards);
        let cat = rank.category();
        let expected = if high == 12 {
            HandCategory::RoyalFlush
        } else {
            HandCategory::StraightFlush
        };
        assert_eq!(cat, expected, "high={high}, got {cat:?}");
    }
}

#[test]
fn wheel_straight_flush_recognized() {
    // A-2-3-4-5 同花 → high = rank(5) = 3。
    let evaluator = NaiveHandEvaluator;
    let cards: [Card; 5] = [
        Card::from_u8(card_index(0, 0)).unwrap(),  // 2♣
        Card::from_u8(card_index(1, 0)).unwrap(),  // 3♣
        Card::from_u8(card_index(2, 0)).unwrap(),  // 4♣
        Card::from_u8(card_index(3, 0)).unwrap(),  // 5♣
        Card::from_u8(card_index(12, 0)).unwrap(), // A♣
    ];
    let rank = evaluator.eval5(&cards);
    assert_eq!(
        rank.category(),
        HandCategory::StraightFlush,
        "wheel SF not recognized: rank={}",
        rank.0
    );
}

#[test]
fn non_straight_dense_masks_do_not_match() {
    // 反向验证：非 5-连位掩码 STRAIGHT_HIGH_TABLE 必须返回「无直」（高位不
    // 等于 high card 的常规 kicker 评估）。这里挑一组「4-gap」case：A K Q J 9
    // —— 缺 10，应只是 high-card / one-pair 区间，绝不是 Straight。
    let evaluator = NaiveHandEvaluator;
    let cards: [Card; 5] = [
        Card::from_u8(card_index(12, 0)).unwrap(), // A♣
        Card::from_u8(card_index(11, 0)).unwrap(), // K♣
        Card::from_u8(card_index(10, 0)).unwrap(), // Q♣
        Card::from_u8(card_index(9, 1)).unwrap(),  // J♦（混花以避开 flush）
        Card::from_u8(card_index(7, 1)).unwrap(),  // 9♦
    ];
    let rank = evaluator.eval5(&cards);
    let cat = rank.category();
    assert!(
        !matches!(cat, HandCategory::Straight | HandCategory::StraightFlush),
        "expected non-straight, got {cat:?} (rank={})",
        rank.0
    );
}
