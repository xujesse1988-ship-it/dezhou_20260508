//! B1：benchmark harness 骨架（D 类）。
//!
//! `pluribus_stage1_workflow.md` §B1：
//!
//! - criterion 配置完成
//! - 占位 benchmark：评估器 1 次调用、单手模拟 1 次
//! - **不设 SLO 断言** —— 阈值断言由 E1 加入。
//!
//! **A1 / B1 状态**：评估器与状态机均未实现，bench 体内调用会 panic。所有
//! bench 入口用 [`std::panic::catch_unwind`] 包裹，criterion 仍能跑出结果
//! （时间会接近 0 / 一致 panic 时间），但流程不崩。E2 实现落地后撤掉
//! `catch_unwind` 包装，让 criterion 测真实热路径。
//!
//! 角色边界：本文件属 `[测试]` agent。`[实现]` agent 不得修改。

use std::panic::{catch_unwind, AssertUnwindSafe};

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use poker::{Card, ChaCha20Rng, GameState, Rank, RngSource, Suit, TableConfig};

// ============================================================================
// bench：评估器单次调用（占位）
// ============================================================================

fn bench_eval5_single_call(c: &mut Criterion) {
    c.bench_function("eval5_placeholder", |b| {
        b.iter(|| {
            // A1 / B1：`Card::new` 自身 unimplemented；外层 catch_unwind 让 criterion
            // 不崩。E2 起替换为：
            //   let hand = [Card::new(Rank::Ace, Suit::Spades), ...];
            //   let _ = black_box(evaluator.eval5(black_box(&hand)));
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let hand = [
                    Card::new(Rank::Ace, Suit::Spades),
                    Card::new(Rank::King, Suit::Spades),
                    Card::new(Rank::Queen, Suit::Spades),
                    Card::new(Rank::Jack, Suit::Spades),
                    Card::new(Rank::Ten, Suit::Spades),
                ];
                black_box(hand)
            }));
        });
    });
}

// ============================================================================
// bench：单手模拟（占位）
// ============================================================================

fn bench_simulate_one_hand(c: &mut Criterion) {
    c.bench_function("simulate_one_hand_placeholder", |b| {
        b.iter(|| {
            // GameState::new / TableConfig::default_6max_100bb / ChaCha20Rng::from_seed
            // 在 A1 全部 panic；catch_unwind 让 criterion 整体不崩。E1 阶段把驱动器接入。
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let cfg = TableConfig::default_6max_100bb();
                let mut rng = ChaCha20Rng::from_seed(black_box(0));
                let _state = GameState::with_rng(black_box(&cfg), 0, &mut rng);
                rng.next_u64()
            }));
        });
    });
}

criterion_group!(baseline, bench_eval5_single_call, bench_simulate_one_hand);
criterion_main!(baseline);
