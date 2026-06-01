//! S4 6-max A3×A4 blueprint 训练 plumbing 验证（`docs/six_max_nlhe_target.md` S4）。
//!
//! 验证「6-max A3×A4 game 能用现成 N-generic trainer + dense 后端端到端训练」+
//! 「收敛监控可观测」+「checkpoint 保存恢复后 update_count 连续、策略查询一致」。
//! 桶用 **stub**（`lookup` 恒 `Some(0)`，preflop 仍 169 lossless）——本测试验**训练
//! 机制 / 监控 / checkpoint plumbing**，不验 blueprint 质量（质量需 S3 真桶 + S4 真训练，
//! 跑在 AWS 大机）。N=2（树 78,852 节点、depth 17）让默认套件跑得动；N=3 生产甜点
//! （1.15M 节点 / 230M infoset / dense ~8 GiB）跑在 AWS。
//!
//! 两个 test：
//! - `six_max_a3a4_hashmap_train_monitor_checkpoint`（默认套件，HashMap 后端、低内存）：
//!   入口 game 构造 + N-generic CFR 单线程 + 并行 + 监控 + checkpoint 往返 + resume 续训。
//! - `six_max_a3a4_dense_train_monitor`（`#[ignore]`，dense 生产路径、~1.5 GiB）：
//!   full-prealloc dense 表按 6-max 树定尺寸、LCFR、单线程 + deterministic 并行 +
//!   lockfree 并行三条写路径都能在 6-max 树上跑通 + 监控可观测。vultr 跑：
//!   ```bash
//!   cargo test --release --test six_max_a3a4_trainer six_max_a3a4_dense_train_monitor -- --ignored --nocapture
//!   ```

use std::sync::Arc;

use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_betting_tree::first_small_6max;
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::{
    ConvergenceMonitor, EsMccfrTrainer, Game, MonitorReport, StrategySnapshot, Trainer,
};
use poker::{BucketConfig, BucketTable, ChaCha20Rng, InfoSetId, RngSource, TableConfig};

/// A3×A4 postflop width-redirect cap（N=2：树小供测试；生产甜点是 N=3）。
const TEST_CAP: u8 = 2;

/// 外部 rng seed（驱动 step 的 randomness）。
const RNG_SEED: u64 = 0x36_4D_41_58_53_34_00_01; // "6MAXS4\0\1"
/// trainer master seed（仅派生 checkpoint rng 占位）。
const MASTER_SEED: u64 = 0x36_4D_41_58_53_34_00_02;

/// 构造 6-max A3×A4 N=TEST_CAP game（stub 200 桶表，preflop 169 lossless）。
/// 用 200/200/200 匹配生产桶数（Pluribus 同档、S3 复用 HU 单对手桶），顺带验证
/// `is_supported_bucket_config` 接受 200 表（生产用 `bucket_table_200_..._schemav4.bin`）。
fn build_six_max_game() -> SimplifiedNlheGame {
    let cfg = BucketConfig::new(200, 200, 200).expect("200/200/200 合法");
    let table = Arc::new(BucketTable::stub_for_postflop(cfg));
    let (abs, rules) = first_small_6max(TEST_CAP);
    SimplifiedNlheGame::new_with_abstraction(table, TableConfig::default_6max_100bb(), abs, rules)
        .expect("6-max A3×A4 game 构造（stub 200 桶表）")
}

/// 监控报告的后端无关健全性断言（指标都有限、在合理范围内）。
fn assert_report_sane(r: &MonitorReport, expect_first: bool) {
    assert_eq!(r.sample_size, 169, "样本 = preflop 根 × 169 手型类");
    assert!(r.active_in_sample <= r.sample_size, "active 不应超样本数");
    assert!(
        r.mean_entropy.is_finite() && r.mean_entropy >= 0.0 && r.mean_entropy < 3.0,
        "mean_entropy 应有限且 ∈ [0, ln(动作数)<3)，实得 {}",
        r.mean_entropy
    );
    assert!(
        r.mean_avg_positive_regret.is_finite() && r.mean_avg_positive_regret >= 0.0,
        "mean_avg_positive_regret 应有限非负，实得 {}",
        r.mean_avg_positive_regret
    );
    if expect_first {
        assert!(r.mean_strategy_drift_l1.is_none(), "首次观测无漂移基准");
    } else {
        let drift = r.mean_strategy_drift_l1.expect("非首次观测应有漂移");
        let max = r.max_strategy_drift_l1.expect("非首次观测应有 max 漂移");
        assert!(
            drift.is_finite() && (0.0..=2.0).contains(&drift),
            "L1 漂移 ∈ [0,2]（概率分布 L1 上界 2），实得 {drift}"
        );
        assert!(
            max.is_finite() && drift <= max + 1e-12,
            "mean 漂移 ≤ max 漂移"
        );
    }
}

/// HashMap 后端：入口 game 构造 + N-generic CFR 单线程 + 并行 + 监控 + checkpoint 往返 +
/// resume 续训。默认套件跑（HashMap 只存访问过的 infoset，低内存、快）。
#[test]
fn six_max_a3a4_hashmap_train_monitor_checkpoint() {
    let game = build_six_max_game();
    assert_eq!(game.n_players(), 6, "6-max n_players");
    assert_eq!(
        game.tree().num_nodes(),
        78_852,
        "N=2 A3×A4 树应 == probe 真值 78,852"
    );

    // 监控器在 game 移入 trainer 前构造（只持 InfoSetId，不借 game）。
    let mut monitor = ConvergenceMonitor::for_game(&game);
    assert_eq!(monitor.sample_size(), 169);

    let mut trainer: EsMccfrTrainer<SimplifiedNlheGame> = EsMccfrTrainer::new(game, MASTER_SEED);
    let mut rng = ChaCha20Rng::from_seed(RNG_SEED);

    // 单线程训练 + 中途观测。
    for _ in 0..3_000 {
        trainer.step(&mut rng).expect("6-max HashMap step");
    }
    let r_mid = monitor.observe(trainer.update_count(), &trainer);
    assert_report_sane(&r_mid, true);
    assert!(
        r_mid.active_in_sample > 30,
        "3000 update 后应已访问 ≥30/169 个 preflop 根信息集，实得 {}",
        r_mid.active_in_sample
    );

    for _ in 0..3_000 {
        trainer.step(&mut rng).expect("6-max HashMap step");
    }
    let r_end = monitor.observe(trainer.update_count(), &trainer);
    assert_report_sane(&r_end, false);
    assert!(
        r_end.active_in_sample >= r_mid.active_in_sample,
        "覆盖率单调不减：mid={} end={}",
        r_mid.active_in_sample,
        r_end.active_in_sample
    );
    assert_eq!(trainer.update_count(), 6_000);

    // 并行路径（N-generic step_parallel）在 6-max 上跑通：4 worker × batch 16。
    let mut pool: Vec<Box<dyn RngSource>> = (0..4u64)
        .map(|t| Box::new(ChaCha20Rng::from_seed(RNG_SEED ^ (t + 1))) as Box<dyn RngSource>)
        .collect();
    trainer
        .step_parallel(&mut pool, 4, 16)
        .expect("6-max HashMap step_parallel");
    assert_eq!(
        trainer.update_count(),
        6_000 + 4 * 16,
        "step_parallel 产 n_active × batch 个 update"
    );

    // checkpoint 往返：保存 → 用全新同构 game 加载 → update_count 连续 + 策略查询一致。
    let dir = std::env::temp_dir().join(format!("six_max_ckpt_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir temp");
    let ckpt = dir.join("six_max_hm.ckpt");
    <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::save_checkpoint(
        &trainer, &ckpt,
    )
    .expect("save 6-max HashMap checkpoint");

    let saved_update = trainer.update_count();
    // 加载前快照监控样本（preflop 根 × 169 类）的 average strategy，用于「策略查询
    // 一致」逐位对照。
    let before: Vec<(InfoSetId, Vec<f64>)> = monitor
        .sample()
        .iter()
        .map(|&i| (i, trainer.average_strategy_for(i)))
        .filter(|(_, v)| !v.is_empty())
        .collect();
    assert!(!before.is_empty(), "应有活跃样本可对照");

    let game2 = build_six_max_game();
    let loaded =
        <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::load_checkpoint(
            &ckpt, game2,
        )
        .expect("load 6-max HashMap checkpoint");
    assert_eq!(
        loaded.update_count(),
        saved_update,
        "resume 后 update_count 连续"
    );
    for (info, expect) in &before {
        let got = loaded.average_strategy_for(*info);
        assert!(
            got.len() == expect.len()
                && got
                    .iter()
                    .zip(expect)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
            "checkpoint 往返后策略查询应逐位一致 @ info {:#x}",
            info.raw()
        );
    }

    // resume 续训：曲线连续推进。
    let mut loaded = loaded;
    let mut rng2 = ChaCha20Rng::from_seed(RNG_SEED ^ 0xFFFF);
    for _ in 0..500 {
        loaded.step(&mut rng2).expect("resume step");
    }
    assert_eq!(loaded.update_count(), saved_update + 500, "续训推进 500");
    let r_resume = monitor.observe(loaded.update_count(), &loaded);
    assert_report_sane(&r_resume, false);

    std::fs::remove_dir_all(&dir).ok();
}

/// dense 生产路径：full-prealloc 表按 6-max 树定尺寸 + LCFR + 三条写路径（单线程 /
/// deterministic 并行 / lockfree 并行）都能在 6-max 上跑通 + 监控可观测。
/// `#[ignore]`：N=2 dense 两表 ~1.5 GiB，跑在 vultr / AWS。
#[test]
#[ignore]
fn six_max_a3a4_dense_train_monitor() {
    let game = build_six_max_game();
    let mut monitor = ConvergenceMonitor::for_game(&game);

    // LCFR period 在 6-max dense 上跑通（period boundary rescale 不 panic）。
    let mut trainer = DenseNlheEsMccfrTrainer::new(game, MASTER_SEED).with_lcfr_period(1_000);
    let mut rng = ChaCha20Rng::from_seed(RNG_SEED);

    // 单线程跨多个 LCFR period boundary。
    for _ in 0..3_000 {
        trainer.step(&mut rng).expect("6-max dense step");
    }
    let r1 = monitor.observe(trainer.update_count(), &trainer);
    assert_report_sane(&r1, true);
    assert!(
        r1.active_in_sample > 30,
        "dense 3000 update 后应访问 ≥30/169，实得 {}",
        r1.active_in_sample
    );
    assert!(
        r1.visited_infosets > 1_000,
        "dense touched_count 应随访问增长，实得 {}",
        r1.visited_infosets
    );

    // deterministic 并行写路径。
    let mut pool: Vec<Box<dyn RngSource>> = (0..4u64)
        .map(|t| Box::new(ChaCha20Rng::from_seed(RNG_SEED ^ (t + 1))) as Box<dyn RngSource>)
        .collect();
    trainer
        .step_parallel(&mut pool, 4, 16)
        .expect("6-max dense step_parallel (deterministic)");

    // lockfree 并行写路径（Hogwild CAS）。
    trainer
        .step_parallel_lockfree(&mut pool, 4, 16)
        .expect("6-max dense step_parallel_lockfree");

    let r2 = monitor.observe(trainer.update_count(), &trainer);
    assert_report_sane(&r2, false);
    assert_eq!(trainer.update_count(), 3_000 + 2 * 4 * 16);

    // checkpoint 往返（dense raw）：update_count 连续 + 策略查询逐位一致。
    let dir = std::env::temp_dir().join(format!("six_max_dense_ckpt_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir temp");
    let ckpt = dir.join("six_max_dense.ckpt");
    trainer.save_checkpoint(&ckpt).expect("save dense ckpt");
    let saved = trainer.update_count();

    let game2 = build_six_max_game();
    let loaded = DenseNlheEsMccfrTrainer::load_checkpoint(&ckpt, game2).expect("load dense ckpt");
    assert_eq!(
        loaded.update_count(),
        saved,
        "dense resume update_count 连续"
    );
    let r3 = monitor.observe(loaded.update_count(), &loaded);
    assert_report_sane(&r3, false);

    std::fs::remove_dir_all(&dir).ok();
}
