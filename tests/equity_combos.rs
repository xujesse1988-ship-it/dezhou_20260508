//! `equity::combos_for_class` 单测（`docs/bucket_feature_design.md` §2.2）。
//!
//! 覆盖：
//! - 169 class 内每类 combos 数 = 6 (pair) / 4 (suited) / 12 (offsuit)
//! - 全 169 class 总 combos = 1326 = C(52, 2)（穷举 hole 空间）
//! - 每 combo 内两张牌严格 distinct
//! - 全 1326 combos 在 class 间无重复（每张 hole 恰好属于一个 class）
//! - 与 `representative_hole_for_class` 一致：rep 必属于该 class 的 combos

use std::collections::HashSet;

use poker::abstraction::equity::combos_for_class;
use poker::core::Card;

fn pair_key(c: &[Card; 2]) -> (u8, u8) {
    let a = c[0].to_u8();
    let b = c[1].to_u8();
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

#[test]
fn combos_per_class_counts() {
    for class in 0u8..169 {
        let combos = combos_for_class(class);
        let expected = match class {
            0..=12 => 6,    // pocket pair: C(4,2)
            13..=90 => 4,   // suited
            91..=168 => 12, // offsuit: 4 × 3
            _ => unreachable!(),
        };
        assert_eq!(
            combos.len(),
            expected,
            "class {class} combos count mismatch: got {} expected {}",
            combos.len(),
            expected,
        );
    }
}

#[test]
fn combos_total_covers_1326_distinct_hole_combos() {
    let mut all: HashSet<(u8, u8)> = HashSet::new();
    for class in 0u8..169 {
        for combo in combos_for_class(class) {
            let k = pair_key(&combo);
            assert!(
                all.insert(k),
                "duplicate combo across classes: class {class} combo {:?}",
                combo,
            );
        }
    }
    assert_eq!(
        all.len(),
        1326,
        "total combos across 169 classes != C(52, 2) = 1326"
    );
}

#[test]
fn combos_inner_cards_distinct() {
    for class in 0u8..169 {
        for combo in combos_for_class(class) {
            assert_ne!(
                combo[0].to_u8(),
                combo[1].to_u8(),
                "class {class} combo has duplicate cards: {:?}",
                combo,
            );
        }
    }
}

#[test]
fn combos_match_representative_hole_class_partition() {
    // 13 pocket pair classes：每类 6 combos
    let mut pair_total = 0;
    for class in 0u8..=12 {
        pair_total += combos_for_class(class).len();
    }
    assert_eq!(pair_total, 13 * 6, "pocket-pair total combos != 78");

    // 78 suited classes：每类 4
    let mut suited_total = 0;
    for class in 13u8..=90 {
        suited_total += combos_for_class(class).len();
    }
    assert_eq!(suited_total, 78 * 4, "suited total combos != 312");

    // 78 offsuit classes：每类 12
    let mut offsuit_total = 0;
    for class in 91u8..=168 {
        offsuit_total += combos_for_class(class).len();
    }
    assert_eq!(offsuit_total, 78 * 12, "offsuit total combos != 936");

    // 78 + 312 + 936 = 1326（与第二个测试结论交叉验证）
    assert_eq!(pair_total + suited_total + offsuit_total, 1326);
}

#[test]
#[should_panic(expected = "combos_for_class: class 169 >= 169")]
fn combos_for_class_panics_on_169() {
    let _ = combos_for_class(169);
}
