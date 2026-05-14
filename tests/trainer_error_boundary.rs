//! 阶段 3 F1 \[测试\]：5 类 `TrainerError` 边界覆盖（API-313 / D-324 / D-325 /
//! D-323 (UnsupportedBucketTable) / D-330 / D-351 propagation）。
//!
//! 5 类错误 + 触发路径与 `pluribus_stage3_workflow.md` §步骤 F1 line 308 字面对应：
//!
//! | 变体 | 触发路径 | 测试形态 |
//! |---|---|---|
//! | `ActionCountMismatch` | `RegretTable::accumulate` / `current_strategy` / `StrategyAccumulator::accumulate` / `average_strategy` 上 InfoSet 长度不匹配 | 4 条 panic 路径 + 1 条构造 trip-wire |
//! | `OutOfMemory` | 训练监控阈值（D-325 字面 SLO 8 GB；本变体 stage 3 路径未实际触发，留 stage 4 监控接入） | 仅构造 + Display/Debug trip-wire |
//! | `UnsupportedBucketTable` | `SimplifiedNlheGame::new(BucketTable)` schema_version != 2 或 config != (500, 500, 500) | 构造 stub + config (100, 100, 100) → Err |
//! | `ProbabilitySumOutOfTolerance` | regret matching σ sum 越界 \[1 - 1e-9, 1 + 1e-9\]（D-330；本变体 stage 3 未实际触发，留 stage 4 监控接入） | 仅构造 + Display/Debug trip-wire |
//! | `Checkpoint(...)` propagation | `Trainer::save_checkpoint` 失败时 `CheckpointError` → 通过 `#[from]` propagate 到 `TrainerError` | 构造 + `From<CheckpointError>` 路径 trip-wire |
//!
//! **stage 3 行为说明**：3 类（OutOfMemory / ProbabilitySumOutOfTolerance / Checkpoint
//! propagation）在 stage 3 \[实现\] 路径未直接触发——RegretTable 通过 panic 而非
//! `Result` 报告 ActionCountMismatch（regret.rs 注释字面 "本层 panic 是底层 trip-wire,
//! 正常调用路径不会触达"），OutOfMemory 监控接入 deferred 到 stage 4 cluster
//! orchestrator，ProbabilitySumOutOfTolerance 在 D-329 字面 "warn 不 panic" 政策下
//! 当前 stage 3 不触发。F1 \[测试\] 仍为这 3 类落 **构造 trip-wire**：变体重命名 /
//! 字段类型漂移 / `#[from]` 移除立即在 `cargo test --no-run` 失败（继承 stage 1
//! `RuleError` / stage 2 `BucketTableError` 错误追加不删 + 构造 trip-wire 同型模式）。
//!
//! **F1 \[测试\] 角色边界**：本文件不修改 `src/training/`；如发现 bug 走 F2 \[实现\]。

use std::path::PathBuf;
use std::sync::Arc;

use poker::training::kuhn::{KuhnGame, KuhnHistory, KuhnInfoSet};
use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::{
    CheckpointError, EsMccfrTrainer, GameVariant, RegretTable, StrategyAccumulator, Trainer,
    TrainerError, TrainerVariant, VanillaCfrTrainer,
};
use poker::{BucketConfig, BucketTable, ChaCha20Rng};

// ===========================================================================
// 1. ActionCountMismatch — 4 条 panic 路径 + 1 条构造 trip-wire
// ===========================================================================
//
// regret.rs / strategy.rs 内部 panic 是 D-324 "底层 trip-wire" 设计——正常 Trainer
// 路径不触达（`Game::legal_actions(state)` 在 same InfoSet 上恒定）。本块用
// `#[should_panic]` 显式 exercise 这 4 条 panic 路径，确保 D-324 不变量不退化。

fn kuhn_info_set() -> KuhnInfoSet {
    KuhnInfoSet {
        actor: 0,
        private_card: 11,
        history: KuhnHistory::Empty,
    }
}

#[test]
#[should_panic(expected = "action_count mismatch")]
fn regret_table_accumulate_action_count_mismatch_panics() {
    // D-324：first accumulate 用 len=2 → vec![0; 2]；second accumulate 用 len=3
    // → assert_eq!(2, 3, "...action_count mismatch...") panic。
    let mut t = RegretTable::<KuhnInfoSet>::new();
    let info = kuhn_info_set();
    t.accumulate(info.clone(), &[1.0, 2.0]);
    t.accumulate(info, &[3.0, 4.0, 5.0]); // panic here
}

#[test]
#[should_panic(expected = "action_count mismatch")]
fn regret_table_current_strategy_action_count_mismatch_panics() {
    // first accumulate len=2 → vec![0; 2]；current_strategy 询问 n=3 → panic。
    let mut t = RegretTable::<KuhnInfoSet>::new();
    let info = kuhn_info_set();
    t.accumulate(info.clone(), &[1.0, 2.0]);
    let _ = t.current_strategy(&info, 3); // panic here
}

#[test]
#[should_panic(expected = "action_count mismatch")]
fn strategy_accumulator_accumulate_action_count_mismatch_panics() {
    let mut s = StrategyAccumulator::<KuhnInfoSet>::new();
    let info = kuhn_info_set();
    s.accumulate(info.clone(), &[0.5, 0.5]);
    s.accumulate(info, &[0.3, 0.3, 0.4]); // panic here
}

#[test]
#[should_panic(expected = "action_count mismatch")]
fn strategy_accumulator_average_strategy_action_count_mismatch_panics() {
    let mut s = StrategyAccumulator::<KuhnInfoSet>::new();
    let info = kuhn_info_set();
    s.accumulate(info.clone(), &[0.5, 0.5]);
    let _ = s.average_strategy(&info, 3); // panic here
}

#[test]
fn action_count_mismatch_construction_trip_wire() {
    // API-313 字段名 / 类型 lock：variant 重命名 / `info_set: String` 改类型 /
    // `expected: usize` / `got: usize` 漂移立即编译期 fail。
    let err = TrainerError::ActionCountMismatch {
        info_set: "KuhnInfoSet { actor: 0, private_card: 11, history: Empty }".to_string(),
        expected: 2,
        got: 3,
    };
    let _display = format!("{err}");
    let _debug = format!("{err:?}");
    match err {
        TrainerError::ActionCountMismatch {
            info_set,
            expected,
            got,
        } => {
            assert!(info_set.contains("KuhnInfoSet"));
            assert_eq!(expected, 2);
            assert_eq!(got, 3);
        }
        other => panic!("expected ActionCountMismatch, got {other:?}"),
    }
}

// ===========================================================================
// 2. OutOfMemory — 构造 trip-wire（stage 3 监控未接入）
// ===========================================================================

#[test]
fn out_of_memory_construction_trip_wire() {
    // API-313 字段名 / 类型 lock：`rss_bytes: u64` / `limit: u64` 漂移立即编译期 fail。
    // D-325 字面 SLO：simplified NLHE training process RSS ≤ 8 GB。监控接入 deferred
    // 到 stage 4 cluster orchestrator，stage 3 trainer 不触发。
    let err = TrainerError::OutOfMemory {
        rss_bytes: 9_000_000_000, // 9 GB > 8 GB SLO
        limit: 8_000_000_000,
    };
    let display = format!("{err}");
    let _debug = format!("{err:?}");
    assert!(
        display.contains("RSS") && display.contains("9000000000"),
        "Display 应回填字段值，实际：{display}"
    );
    match err {
        TrainerError::OutOfMemory { rss_bytes, limit } => {
            assert_eq!(rss_bytes, 9_000_000_000);
            assert_eq!(limit, 8_000_000_000);
        }
        other => panic!("expected OutOfMemory, got {other:?}"),
    }
}

// ===========================================================================
// 3. UnsupportedBucketTable — `SimplifiedNlheGame::new` 真实路径触发
// ===========================================================================

#[test]
fn simplified_nlhe_new_with_wrong_bucket_config_returns_unsupported_bucket_table() {
    // `SimplifiedNlheGame::new` 校验 `BucketTable::config() == (500, 500, 500)`
    // （D-314-rev1）；config (100, 100, 100) 走 second branch 返回
    // `UnsupportedBucketTable { expected: 2, got: 0 }`（nlhe.rs:200-203 字面：
    // got=0 让 caller 通过 schema_version 路径区分 vs config 路径）。
    let bucket_cfg = BucketConfig::new(100, 100, 100).expect("100 in [10, 10000]");
    let table = BucketTable::stub_for_postflop(bucket_cfg);
    // stub 维持 schema_version=2，所以 first branch 不触发；second branch (config 不匹配) 触发。
    let err = match SimplifiedNlheGame::new(Arc::new(table)) {
        Ok(_) => panic!("SimplifiedNlheGame::new with wrong config 必须 Err"),
        Err(e) => e,
    };
    match err {
        TrainerError::UnsupportedBucketTable { expected, got } => {
            // expected = EXPECTED_BUCKET_SCHEMA_VERSION（2）；got = 0（config 不匹配 sentinel）。
            assert_eq!(expected, 2, "expected 应为 EXPECTED_BUCKET_SCHEMA_VERSION");
            assert_eq!(got, 0, "config-mismatch 路径 got 字段是 0 sentinel");
        }
        other => panic!("expected UnsupportedBucketTable, got {other:?}"),
    }
}

#[test]
fn simplified_nlhe_new_config_500_500_too_small_returns_unsupported() {
    // 边界值：(500, 500, 100) 也应 Err（river 不匹配 even when flop/turn 匹配）。
    let bucket_cfg = BucketConfig::new(500, 500, 100).expect("100 in [10, 10000]");
    let table = BucketTable::stub_for_postflop(bucket_cfg);
    let err = match SimplifiedNlheGame::new(Arc::new(table)) {
        Ok(_) => panic!("river=100 必须 Err"),
        Err(e) => e,
    };
    assert!(
        matches!(err, TrainerError::UnsupportedBucketTable { .. }),
        "expected UnsupportedBucketTable (river=100 mismatch), got {err:?}"
    );
}

#[test]
fn unsupported_bucket_table_construction_trip_wire() {
    // API-313 字段名 / 类型 lock：`expected: u32` / `got: u32` 漂移立即编译期 fail。
    let err = TrainerError::UnsupportedBucketTable {
        expected: 2,
        got: 1,
    };
    let display = format!("{err}");
    let _debug = format!("{err:?}");
    assert!(
        display.contains("schema") && display.contains("not supported"),
        "Display 应表达 schema 不支持，实际：{display}"
    );
    match err {
        TrainerError::UnsupportedBucketTable { expected, got } => {
            assert_eq!(expected, 2);
            assert_eq!(got, 1);
        }
        other => panic!("expected UnsupportedBucketTable, got {other:?}"),
    }
}

// ===========================================================================
// 4. ProbabilitySumOutOfTolerance — 构造 trip-wire（stage 3 D-329 warn-only）
// ===========================================================================

#[test]
fn probability_sum_out_of_tolerance_construction_trip_wire() {
    // API-313 字段名 / 类型 lock：`got: f64` / `tolerance: f64` 漂移立即编译期 fail。
    // D-330 字面阈值：1e-9。本 variant 在 stage 3 `RegretTable::current_strategy`
    // 路径不触发（D-329 "warn 不 panic"）；stage 4 起步可在 Trainer::step 层加 strict
    // mode dispatch（D-330-revM 翻面）。
    let err = TrainerError::ProbabilitySumOutOfTolerance {
        got: 1.0 + 2.0e-9,
        tolerance: 1.0e-9,
    };
    let display = format!("{err}");
    let _debug = format!("{err:?}");
    assert!(
        display.contains("regret matching") && display.contains("tolerance"),
        "Display 应表达 RM 容差越界，实际：{display}"
    );
    match err {
        TrainerError::ProbabilitySumOutOfTolerance { got, tolerance } => {
            assert!((got - (1.0 + 2.0e-9)).abs() < 1.0e-15);
            assert!((tolerance - 1.0e-9).abs() < 1.0e-15);
        }
        other => panic!("expected ProbabilitySumOutOfTolerance, got {other:?}"),
    }
}

// ===========================================================================
// 5. CheckpointError propagation —— `#[from]` 自动转换 trip-wire
// ===========================================================================

#[test]
fn checkpoint_error_propagates_via_from_trait() {
    // API-313 第 5 类：`#[from] CheckpointError` 让 `Result<_, CheckpointError>`
    // 可用 `?` 自动转 `Result<_, TrainerError>`。本测试断言 5 类 CheckpointError
    // 任意一个都能 propagate。
    let cases: [CheckpointError; 5] = [
        CheckpointError::FileNotFound {
            path: PathBuf::from("/tmp/f1-trainer-error-fixture-not-real"),
        },
        CheckpointError::SchemaMismatch {
            expected: 1,
            got: 2,
        },
        CheckpointError::TrainerMismatch {
            expected: (TrainerVariant::VanillaCfr, GameVariant::Kuhn),
            got: (TrainerVariant::EsMccfr, GameVariant::SimplifiedNlhe),
        },
        CheckpointError::BucketTableMismatch {
            expected: [0u8; 32],
            got: [0xAA; 32],
        },
        CheckpointError::Corrupted {
            offset: 42,
            reason: "trip-wire".to_string(),
        },
    ];

    for ckpt_err in cases {
        // 先把 CheckpointError clone 出来给 ground truth assert
        let display_ckpt = format!("{ckpt_err}");
        // From trait dispatch
        let trainer_err: TrainerError = ckpt_err.into();
        // Display propagates underlying CheckpointError Display
        let display_trainer = format!("{trainer_err}");
        assert!(
            display_trainer.contains(&display_ckpt) || display_trainer.contains("checkpoint"),
            "Trainer Display 应包装 underlying CheckpointError 文本：trainer=`{display_trainer}` ckpt=`{display_ckpt}`"
        );
        // std::error::Error::source() 应链回 CheckpointError
        let source = std::error::Error::source(&trainer_err);
        assert!(
            source.is_some(),
            "Checkpoint(...) 应 expose source() 返回 Some(&CheckpointError)；CheckpointError 包装 lost"
        );
    }
}

#[test]
fn trainer_load_checkpoint_propagates_file_not_found_via_question_mark() {
    // 真实路径触发：`Trainer::load_checkpoint(&nonexistent_path, KuhnGame)` 返回
    // `Result<_, CheckpointError>` 直接 == `Err(FileNotFound)`；本测试断言 trait
    // 签名是 CheckpointError（不是 TrainerError）—— D2 \[实现\] load_checkpoint 选择
    // 不通过 `?` 上提到 TrainerError，而保持 CheckpointError 直接返回（API-310 字面）。
    // 同时验证 propagation 链：用户代码 `let _: Result<_, TrainerError> = trainer
    // .load_checkpoint(...).map_err(Into::into);` 仍可走 `From<CheckpointError>`。
    let nonexistent = std::env::temp_dir().join(format!(
        "f1_trainer_no_such_file_{}_{}.bin",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    assert!(!nonexistent.exists(), "fixture path 必须不存在");

    // 直接 load_checkpoint 返回 CheckpointError（VanillaCfrTrainer 不 derive Debug，
    // 所以走 match 而不是 .expect_err）。
    let direct: Result<VanillaCfrTrainer<KuhnGame>, CheckpointError> =
        VanillaCfrTrainer::<KuhnGame>::load_checkpoint(&nonexistent, KuhnGame);
    let direct_err = match direct {
        Ok(_) => panic!("load_checkpoint on nonexistent path 必须 Err"),
        Err(e) => e,
    };
    assert!(
        matches!(direct_err, CheckpointError::FileNotFound { .. }),
        "expected FileNotFound, got {direct_err:?}"
    );

    // 用户侧手动 propagate 到 TrainerError 的等价路径（API-313 #[from] 入口）
    let propagated: Result<VanillaCfrTrainer<KuhnGame>, TrainerError> =
        VanillaCfrTrainer::<KuhnGame>::load_checkpoint(&nonexistent, KuhnGame).map_err(Into::into);
    let propagated_err = match propagated {
        Ok(_) => panic!("propagated load_checkpoint 必须 Err"),
        Err(e) => e,
    };
    let source = std::error::Error::source(&propagated_err);
    assert!(
        source.is_some(),
        "TrainerError::Checkpoint(...) 必须 expose source()"
    );
}

#[test]
fn checkpoint_error_propagation_construction_trip_wire() {
    // 显式构造 5 类 CheckpointError 各一份，每个都通过 From 转 TrainerError；
    // 编译通过 ⇔ `#[from] CheckpointError` 仍存在 + variant 命名仍包装 CheckpointError。
    let _: TrainerError = TrainerError::from(CheckpointError::FileNotFound {
        path: PathBuf::from("/x"),
    });
    let _: TrainerError = TrainerError::from(CheckpointError::SchemaMismatch {
        expected: 1,
        got: 2,
    });
    let _: TrainerError = TrainerError::from(CheckpointError::TrainerMismatch {
        expected: (TrainerVariant::VanillaCfr, GameVariant::Kuhn),
        got: (TrainerVariant::EsMccfr, GameVariant::Leduc),
    });
    let _: TrainerError = TrainerError::from(CheckpointError::BucketTableMismatch {
        expected: [0u8; 32],
        got: [0xFF; 32],
    });
    let _: TrainerError = TrainerError::from(CheckpointError::Corrupted {
        offset: 0,
        reason: String::new(),
    });
}

// ===========================================================================
// 6. 6 类 TrainerError exhaustive match — 添加第 7 类 variant 必须显式同步
//
// stage 4 A1 \[实现\]（2026-05-14）追加 [`TrainerError::PreflopActionAbstractionMismatch`]
// 第 6 个 variant（D-456 字面 `LbrEvaluator::new` action_set_size 越界拒绝）；
// stage 3 既有 5 variant 路径不变（API-313 字面），exhaustive match 自适应 stage
// 4 新 variant 让 stage 3 `cargo test --test trainer_error_boundary` byte-equal
// 维持。
// ===========================================================================

fn assert_one_of_six_known_trainer_variants(err: &TrainerError) {
    match err {
        TrainerError::ActionCountMismatch { .. }
        | TrainerError::OutOfMemory { .. }
        | TrainerError::UnsupportedBucketTable { .. }
        | TrainerError::ProbabilitySumOutOfTolerance { .. }
        | TrainerError::PreflopActionAbstractionMismatch
        | TrainerError::Checkpoint(_) => {}
    }
}

#[test]
fn trainer_error_6_variants_exhaustive_match_lock() {
    // 与 `tests/checkpoint_round_trip.rs::checkpoint_error_5_variants_exhaustive_match_lock`
    // 同型：构造 6 个变体 minimum sample，让 match 闭门枚举编译期 trip-wire 触发。
    // stage 4 A1 \[实现\] 追加 PreflopActionAbstractionMismatch 第 6 variant。
    let samples: [TrainerError; 6] = [
        TrainerError::ActionCountMismatch {
            info_set: "test".to_string(),
            expected: 2,
            got: 3,
        },
        TrainerError::OutOfMemory {
            rss_bytes: 1,
            limit: 0,
        },
        TrainerError::UnsupportedBucketTable {
            expected: 2,
            got: 1,
        },
        TrainerError::ProbabilitySumOutOfTolerance {
            got: 1.5,
            tolerance: 1.0e-9,
        },
        TrainerError::PreflopActionAbstractionMismatch,
        TrainerError::Checkpoint(CheckpointError::Corrupted {
            offset: 0,
            reason: "test".to_string(),
        }),
    ];
    for s in &samples {
        assert_one_of_six_known_trainer_variants(s);
        let _ = format!("{s}");
        let _ = format!("{s:?}");
    }
}

// ===========================================================================
// 7. dead_code 抑制 import helper（同 cfr_kuhn / cfr_simplified_nlhe 模式）
// ===========================================================================

#[allow(dead_code)]
fn _import_check(
    _t: VanillaCfrTrainer<KuhnGame>,
    _es: EsMccfrTrainer<SimplifiedNlheGame>,
    _rng: ChaCha20Rng,
    _v: GameVariant,
    _tv: TrainerVariant,
) {
}
