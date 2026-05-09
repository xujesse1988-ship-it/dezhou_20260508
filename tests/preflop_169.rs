//! B1 §B 类：preflop 169 lossless 完整 1326 → 169 枚举测试。
//!
//! 阶段 2 信任锚（`pluribus_stage2_workflow.md` §B1 line 228 字面：
//! "preflop 169 是阶段 2 信任锚，B1 必须完整覆盖，不能拖到 C1"）。
//!
//! 本测试不依赖 `PreflopLossless169` stub 的实现状态——基于 D-217 锁定
//! closed-form 公式 + 12 条边界锚点表（详见 `pluribus_stage2_decisions.md` §2
//! D-217 详解）独立枚举 1326 个起手牌，断言：
//!
//! 1. 每个起手牌经公式映射到恰好 1 个 169 类（覆盖 + 无重叠）。
//! 2. 每个 169 类的 hole 计数与组合数学一致：pairs 6 / suited 4 / offsuit 12。
//! 3. 169 类总 hole 计数 = 1326（13×6 + 78×4 + 78×12 = 78 + 312 + 936）。
//! 4. 169 类编号边界正确：pairs ∈ 0..13、suited ∈ 13..91、offsuit ∈ 91..169。
//! 5. 12 条 D-217 边界锚点（22, 33, AA, 32s, 42s, 43s, 52s, AKs, 32o, 42o,
//!    43o, AKo）公式输出与表格一致。
//!
//! 这些断言**完全独立**于 `PreflopLossless169::hand_class` 的实现——若 B2
//! [实现] 落地的 `hand_class` 与本文件 closed-form 公式产出不一致，B2 测试侧
//! 反弹立即可见（公式即 ground truth）。
//!
//! 此外**附加** 1 条 stub-driven 锚点测试（`hand_class_via_lossless_anchors`）
//! 在 A1 unimplemented 时 panic、B2 stub 落地后断言公式与 stub 输出 byte-equal。
//!
//! 角色边界：本文件属 `[测试]` agent 产物。

use poker::{Card, PreflopLossless169, Rank, Suit};

// ============================================================================
// 独立 closed-form 实现（D-217 锁定公式）
// ============================================================================
//
// **不**调用 `PreflopLossless169` 任一方法——本函数与产品代码独立，作为
// ground truth。`PreflopLossless169::hand_class` 的 B2 实现与本函数任一输入
// 输出对不齐，立即在 stub-driven anchor test 暴露。

/// D-217 closed-form `hand_class_169` 公式（独立实现）。
///
/// stage 1 `Rank` 枚举数值：`Two = 0, Three = 1, ..., Ace = 12`。`high` ≥ `low`
/// 通过排序保证。
fn closed_form_hand_class_169(rank_a: Rank, rank_b: Rank, suited: bool) -> u8 {
    let a = rank_a as u8;
    let b = rank_b as u8;
    let (high, low) = if a >= b { (a, b) } else { (b, a) };

    if high == low {
        // Pocket pair：class id = rank 数值（22→0, 33→1, ..., AA→12）。
        high
    } else if suited {
        // Suited：lex order on (high, low) ascending。
        13 + high * (high - 1) / 2 + low
    } else {
        // Offsuit：同 suited 顺序 + offset 78。
        91 + high * (high - 1) / 2 + low
    }
}

// ============================================================================
// 1. preflop_169_anchor_table_closed_form（独立公式 ground truth）
// ============================================================================
//
// D-217 详解 12 条边界锚点表的公式正确性。**完全独立**于 `PreflopLossless169`
// 实现——本测试不调用任何 stub。
#[test]
fn preflop_169_anchor_table_closed_form() {
    // 12 条 D-217 锚点：(rank_a, rank_b, suited, expected_class_id)。
    let anchors: [(Rank, Rank, bool, u8); 12] = [
        // pocket pairs（13 条段，0..13；锚点取 22 / 33 / AA）
        (Rank::Two, Rank::Two, false, 0),
        (Rank::Three, Rank::Three, false, 1),
        (Rank::Ace, Rank::Ace, false, 12),
        // suited（78 条段，13..91；锚点取 32s / 42s / 43s / 52s / AKs）
        (Rank::Three, Rank::Two, true, 13),
        (Rank::Four, Rank::Two, true, 14),
        (Rank::Four, Rank::Three, true, 15),
        (Rank::Five, Rank::Two, true, 16),
        (Rank::Ace, Rank::King, true, 90),
        // offsuit（78 条段，91..169；锚点取 32o / 42o / 43o / AKo）
        (Rank::Three, Rank::Two, false, 91),
        (Rank::Four, Rank::Two, false, 92),
        (Rank::Four, Rank::Three, false, 93),
        (Rank::Ace, Rank::King, false, 168),
    ];

    for (rank_a, rank_b, suited, expected) in anchors {
        let got = closed_form_hand_class_169(rank_a, rank_b, suited);
        assert_eq!(
            got, expected,
            "D-217 锚点：{rank_a:?} {rank_b:?} suited={suited} → expected {expected}, got {got}"
        );
    }
}

// ============================================================================
// 2. preflop_169_lossless_complete_coverage_closed_form（独立公式覆盖测试）
// ============================================================================
//
// 枚举全部 1326 起手牌（C(52, 2)），独立公式映射到 169 类，断言：
//
// 1. 每类编号 ∈ 0..169；
// 2. pairs 段（0..13）每类 6 hole；suited 段（13..91）每类 4 hole；offsuit 段
//    （91..169）每类 12 hole；
// 3. 169 类总 hole 计数 = 1326；
// 4. pairs 段 13 类 / suited 段 78 类 / offsuit 段 78 类。
//
// **完全独立**于 `PreflopLossless169` 实现，是阶段 2 信任锚的最低核——若
// B2 [实现] 落地的 `hand_class` 与本测试公式不一致，B2 必须找到差异源（公式
// 锁定，[实现] 反弹）。
#[test]
fn preflop_169_lossless_complete_coverage_closed_form() {
    let mut counts = [0u32; 169];
    let mut total_hole = 0u32;

    // 枚举 (52 choose 2) = 1326 hole 起手牌。
    for a in 0..52u8 {
        for b in (a + 1)..52u8 {
            let card_a = Card::from_u8(a).expect("0..52 合法 Card");
            let card_b = Card::from_u8(b).expect("0..52 合法 Card");
            let suited = card_a.suit() == card_b.suit();
            let class = closed_form_hand_class_169(card_a.rank(), card_b.rank(), suited);
            assert!(
                class < 169,
                "D-217：hand_class < 169（card_a={card_a:?}, card_b={card_b:?}, class={class}）"
            );
            counts[class as usize] += 1;
            total_hole += 1;
        }
    }

    // 1326 总 hole 数。
    assert_eq!(total_hole, 1326, "D-217：枚举总数 = (52 choose 2) = 1326");

    // 段长校验：pairs 段（0..13）13 类 × 6 hole；suited 段（13..91）78 类 × 4
    // hole；offsuit 段（91..169）78 类 × 12 hole。
    let mut pair_classes = 0u32;
    let mut suited_classes = 0u32;
    let mut offsuit_classes = 0u32;
    for (idx, &cnt) in counts.iter().enumerate() {
        match idx {
            0..=12 => {
                assert_eq!(cnt, 6, "D-217 pair class {idx}: 期望 6 hole, got {cnt}");
                if cnt > 0 {
                    pair_classes += 1;
                }
            }
            13..=90 => {
                assert_eq!(cnt, 4, "D-217 suited class {idx}: 期望 4 hole, got {cnt}");
                if cnt > 0 {
                    suited_classes += 1;
                }
            }
            91..=168 => {
                assert_eq!(
                    cnt, 12,
                    "D-217 offsuit class {idx}: 期望 12 hole, got {cnt}"
                );
                if cnt > 0 {
                    offsuit_classes += 1;
                }
            }
            _ => unreachable!(),
        }
    }

    assert_eq!(pair_classes, 13, "D-217：pairs 段 13 类全部覆盖");
    assert_eq!(suited_classes, 78, "D-217：suited 段 78 类全部覆盖");
    assert_eq!(offsuit_classes, 78, "D-217：offsuit 段 78 类全部覆盖");

    // 13×6 + 78×4 + 78×12 = 78 + 312 + 936 = 1326 ✓
    let computed_total = 13 * 6 + 78 * 4 + 78 * 12;
    assert_eq!(computed_total, 1326, "D-217 组合数学：13×6+78×4+78×12=1326");
}

// ============================================================================
// 3. preflop_169_lossless_via_stub（stub-driven 锚点比对，B2 driver）
// ============================================================================
//
// 调用 `PreflopLossless169::hand_class` 与公式 ground truth 比对 12 锚点。
// **B1 状态**：`hand_class` `unimplemented!()`，本测试 panic（与 A 类同形态）。
// **B2 状态**：stub 落地后断言激活，[实现] 必须让本测试通过。
#[test]
fn preflop_169_lossless_via_stub() {
    let abs = PreflopLossless169::new();

    // D-217 锚点 hole 实例（每个 hand 取 spade-hearts 组合）。
    let cases: [([Card; 2], u8); 12] = [
        // pocket pairs
        (
            [
                Card::new(Rank::Two, Suit::Spades),
                Card::new(Rank::Two, Suit::Hearts),
            ],
            0,
        ),
        (
            [
                Card::new(Rank::Three, Suit::Spades),
                Card::new(Rank::Three, Suit::Hearts),
            ],
            1,
        ),
        (
            [
                Card::new(Rank::Ace, Suit::Spades),
                Card::new(Rank::Ace, Suit::Hearts),
            ],
            12,
        ),
        // suited
        (
            [
                Card::new(Rank::Three, Suit::Spades),
                Card::new(Rank::Two, Suit::Spades),
            ],
            13,
        ),
        (
            [
                Card::new(Rank::Four, Suit::Spades),
                Card::new(Rank::Two, Suit::Spades),
            ],
            14,
        ),
        (
            [
                Card::new(Rank::Four, Suit::Spades),
                Card::new(Rank::Three, Suit::Spades),
            ],
            15,
        ),
        (
            [
                Card::new(Rank::Five, Suit::Spades),
                Card::new(Rank::Two, Suit::Spades),
            ],
            16,
        ),
        (
            [
                Card::new(Rank::Ace, Suit::Spades),
                Card::new(Rank::King, Suit::Spades),
            ],
            90,
        ),
        // offsuit
        (
            [
                Card::new(Rank::Three, Suit::Spades),
                Card::new(Rank::Two, Suit::Hearts),
            ],
            91,
        ),
        (
            [
                Card::new(Rank::Four, Suit::Spades),
                Card::new(Rank::Two, Suit::Hearts),
            ],
            92,
        ),
        (
            [
                Card::new(Rank::Four, Suit::Spades),
                Card::new(Rank::Three, Suit::Hearts),
            ],
            93,
        ),
        (
            [
                Card::new(Rank::Ace, Suit::Spades),
                Card::new(Rank::King, Suit::Hearts),
            ],
            168,
        ),
    ];

    for (hole, expected) in cases {
        let got = abs.hand_class(hole);
        assert_eq!(
            got, expected,
            "D-217 锚点 stub 比对：hole={hole:?} → expected {expected}, got {got}"
        );
    }
}

// ============================================================================
// 4. preflop_169_lossless_full_via_stub（stub-driven 完整 1326 → 169 比对）
// ============================================================================
//
// 完整 1326 起手枚举，每张比对 closed-form 公式 vs `PreflopLossless169::
// hand_class` 输出。**B1 状态**：panic（unimplemented）。**B2 状态**：stub
// 落地后必须 1326/1326 一致——这是阶段 2 信任锚的最强检验。
#[test]
fn preflop_169_lossless_full_via_stub() {
    let abs = PreflopLossless169::new();

    for a in 0..52u8 {
        for b in (a + 1)..52u8 {
            let card_a = Card::from_u8(a).expect("0..52 合法 Card");
            let card_b = Card::from_u8(b).expect("0..52 合法 Card");
            let suited = card_a.suit() == card_b.suit();

            let expected = closed_form_hand_class_169(card_a.rank(), card_b.rank(), suited);
            let got = abs.hand_class([card_a, card_b]);
            assert_eq!(
                got, expected,
                "D-217 stub 比对：{card_a:?} {card_b:?} suited={suited} \
                 → closed-form {expected}, stub {got}"
            );
        }
    }
}

// ============================================================================
// 5. preflop_169_hole_count_in_class_complete（stub-driven hole_count）
// ============================================================================
//
// `PreflopLossless169::hole_count_in_class(class)` 在 169 类全集上的输出与
// 组合数学一致：pairs 6 / suited 4 / offsuit 12。**B1 状态**：panic。
// **B2 状态**：stub 落地后 169 类断言全绿。
#[test]
fn preflop_169_hole_count_in_class_complete() {
    for class in 0u8..13 {
        assert_eq!(
            PreflopLossless169::hole_count_in_class(class),
            6,
            "D-217：pair class {class} hole_count = 6"
        );
    }
    for class in 13u8..91 {
        assert_eq!(
            PreflopLossless169::hole_count_in_class(class),
            4,
            "D-217：suited class {class} hole_count = 4"
        );
    }
    for class in 91u8..169 {
        assert_eq!(
            PreflopLossless169::hole_count_in_class(class),
            12,
            "D-217：offsuit class {class} hole_count = 12"
        );
    }
}
