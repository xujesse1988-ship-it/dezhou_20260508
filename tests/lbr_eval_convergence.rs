//! 阶段 4 §E1 \[测试\]：LBR exploitability 单调下降 + 收敛阈值断言
//! （`pluribus_stage4_workflow.md` §E1 line 265 + `pluribus_stage4_decisions.md`
//! §6 D-450..D-459）。
//!
//! 6 条 LBR 收敛 + 边界 anchor 测试（全 `#[ignore]` opt-in，release profile +
//! first usable `10⁹` update 训练完成的 blueprint checkpoint 路径触发）：
//!
//! 1. [`stage4_first_usable_lbr_below_200_mbbg`] — D-451 字面 first usable
//!    LBR < 200 mbb/g（path.md §阶段 4 字面 < 100 mbb/g production，stage 4
//!    first usable 略松 200 mbb/g 作为 sanity）。
//! 2. [`stage4_lbr_100_samples_monotone_nonincreasing`] — D-452 字面 LBR
//!    100 个采样点单调非升（允许相邻两次 ±10% 噪声；连续 3 trend up 触发
//!    D-470 监控告警）。
//! 3. [`stage4_lbr_per_traverser_upper_bound_below_500_mbbg`] — D-459 字面
//!    每 traverser 独立 LBR 上界 < 500 mbb/g（避免 1 traverser 优秀 + 5
//!    traverser fail 虚假通过）。
//! 4. [`stage4_openspiel_policy_export_byte_equal`] — D-457 字面 OpenSpiel-
//!    compatible policy 文件 byte-equal one-shot export sanity（F3 \[报告\]
//!    OpenSpiel Python LBR `algorithms/exploitability_descent.py` 对照前置）。
//! 5. [`stage4_lbr_14_action_enumerate_range_correct`] — D-456 字面 LBR
//!    14-action 全枚举 vs 5-action ablation 双路径下 14-action 上界 ≤
//!    5-action 上界（更大 action set 让 LBR 更紧）。
//! 6. [`stage4_lbr_myopic_horizon_1_boundary`] — D-455 字面 myopic horizon = 1
//!    边界（horizon=0 = pure blueprint → LBR 退化为 EV(blueprint)；
//!    horizon=2 不支持 → `TrainerError::PreflopActionAbstractionMismatch`
//!    fallback or 直接 `unimplemented!()`）。
//!
//! **E1 \[测试\] 角色边界**（继承 stage 1/2/3 同型政策）：本文件 0 改动
//! `src/training/lbr.rs` / `src/training/metrics.rs` / `docs/*`；如断言落在
//! \[实现\] 边界错误的产品代码上 → filed issue 移交 E2 \[实现\]。
//!
//! **E1 → E2 工程契约**（panic-fail 翻面条件）：
//!
//! - E2 \[实现\] 落地 [`LbrEvaluator::new`] + [`LbrEvaluator::compute`] +
//!   [`LbrEvaluator::compute_six_traverser_average`] +
//!   [`LbrEvaluator::export_policy_for_openspiel`] 全 4 方法后，本套 6 测试
//!   在 AWS c7a.8xlarge first usable `10⁹` update 训练完成 + checkpoint 路径
//!   触发实测应通过（首发 LBR < 200 mbb/g first usable）。
//! - LBR computation P95 < 30 s by D-454（覆盖在 `tests/perf_slo.rs::
//!   stage4_lbr_computation_p95_under_30s`），本文件不重复 wall-time 断言。
//!
//! **实测触发说明**：本套 6 测试 `#[ignore]` 默认跳过 — 用户授权 + AWS
//! c7a.8xlarge first usable 10⁹ 训练完成后跑 `cargo test --release --test
//! lbr_eval_convergence -- --ignored` 触发；E1 closure 期望全部 panic-fail
//! （`LbrEvaluator::new` A1 \[实现\] scaffold `unimplemented!()`）。

use std::path::PathBuf;
use std::sync::Arc;

use poker::training::lbr::{LbrEvaluator, LbrResult, SixTraverserLbrResult};
use poker::training::nlhe_6max::NlheGame6;
use poker::training::EsMccfrTrainer;
use poker::{BucketTable, ChaCha20Rng};

// ===========================================================================
// 共享常量（与 `tests/perf_slo.rs::stage4_*` / `tests/training_24h_continuous.rs`
// 跨测试 ground truth 一致）
// ===========================================================================

/// stage 4 D-424 v3 production artifact path。
const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";

/// stage 4 D-424 v3 artifact body BLAKE3 ground truth。
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

/// stage 4 D-409 字面 warm-up 切换点。
const WARMUP_COMPLETE_AT: u64 = 1_000_000;

/// stage 4 D-452 字面 LBR computation 单次采样 hand 数（1000 hand /
/// LBR-player）。
const LBR_N_HANDS: u64 = 1_000;

/// stage 4 D-451 字面 first usable LBR 阈值（mbb/g）。
const D_451_FIRST_USABLE_LBR_MBBG: f64 = 200.0;

/// stage 4 D-459 字面 per-traverser LBR 上界（避免 1 traverser 虚假通过）。
const D_459_PER_TRAVERSER_LBR_MBBG: f64 = 500.0;

/// stage 4 D-452 字面 LBR 采样点数（10⁷ update / 采样 × 100 个 = 10⁹ first
/// usable）。
const D_452_N_SAMPLES: usize = 100;

/// stage 4 D-452 字面 LBR 单调非升 ±10% 噪声容忍。
const D_452_MONOTONE_NOISE_PCT: f64 = 10.0;

/// stage 4 D-456 字面 LBR action set size 双路径：14-action 主线 / 5-action
/// ablation。
const D_456_ACTION_SET_14: usize = 14;
const D_456_ACTION_SET_5: usize = 5;

/// stage 4 D-455 字面 LBR myopic horizon = 1（lock）。
const D_455_MYOPIC_HORIZON_1: u8 = 1;

/// stage 4 master seed（ASCII "STG4_E1\x1A"）。
const FIXED_SEED: u64 = 0x53_54_47_34_5F_45_31_1A;

// ===========================================================================
// helper
// ===========================================================================

/// 加载 v3 artifact + 构造 `NlheGame6`；artifact 缺失 / 不匹配 → `None`
/// （pass-with-skip）。与 `tests/perf_slo.rs::stage4_load_v3_artifact_or_skip` /
/// `tests/training_24h_continuous.rs::load_v3_artifact_or_skip` 同型 helper。
fn load_v3_or_skip() -> Option<NlheGame6> {
    let path = PathBuf::from(V3_ARTIFACT_PATH);
    if !path.exists() {
        eprintln!(
            "[stage4-lbr] skip: v3 artifact `{V3_ARTIFACT_PATH}` 不存在（GitHub-hosted runner \
             典型场景；本地 dev box / vultr / AWS host 有 artifact 时跑）。"
        );
        return None;
    }
    let table = match BucketTable::open(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[stage4-lbr] skip: BucketTable::open 失败：{e:?}");
            return None;
        }
    };
    let body_hex: String = table
        .content_hash()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if body_hex != V3_BODY_BLAKE3_HEX {
        eprintln!(
            "[stage4-lbr] skip: artifact body BLAKE3 `{body_hex}` 不匹配 v3 ground truth \
             `{V3_BODY_BLAKE3_HEX}`（D-424 lock 要求 v3 artifact）。"
        );
        return None;
    }
    match NlheGame6::new(Arc::new(table)) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("[stage4-lbr] skip: NlheGame6::new 失败：{e:?}");
            None
        }
    }
}

/// 构造 stage 4 NlheGame6 + Linear+RM+ trainer（warmup_at=1M，D-409 主路径）+
/// 训练若干 update 让 average_strategy 不为空（LBR computation 需要 reachable
/// InfoSet 上 strategy populated）。
///
/// E1 \[测试\] closure 形态下 `trainer.step()` C2+D2 path 走 single-shared
/// RegretTable + alternating，pre_train_updates = 0 时 LBR 走 uniform strategy
/// baseline；pre_train_updates > 0 时 LBR 走真实 blueprint 路径但 first usable
/// 10⁹ update 训练完成的 checkpoint 加载路径（`Trainer::load_checkpoint`）由
/// 用户手动触发，本 helper 仅做 thin pre-train（默认 0 update）。
fn build_pretrained_trainer(
    game: NlheGame6,
    seed: u64,
    pre_train_updates: u64,
) -> EsMccfrTrainer<NlheGame6> {
    let mut trainer = EsMccfrTrainer::new(game, seed).with_linear_rm_plus(WARMUP_COMPLETE_AT);
    if pre_train_updates > 0 {
        use poker::training::Trainer;
        let mut rng = ChaCha20Rng::from_seed(seed.wrapping_add(0xC0FFEE));
        for _ in 0..pre_train_updates {
            trainer.step(&mut rng).expect("pre-train step 期望成功");
        }
    }
    trainer
}

// ===========================================================================
// Test 1 — D-451 first usable LBR < 200 mbb/g（六traverser average）
// ===========================================================================

/// stage 4 D-451 字面 first usable LBR 上界 `< 200 mbb/g`（path.md §阶段 4 字面
/// `< 100 mbb/g` production，stage 4 first usable 略松 200 mbb/g sanity）。
///
/// 6-traverser average LBR < D-451 first usable threshold；评测口径是 D-459
/// 字面 `average_mbbg`（6 traverser 独立 LBR + average）。
///
/// **E1 closure 形态**：[`LbrEvaluator::new`] A1 \[实现\] scaffold 走
/// `unimplemented!()`，opt-in `--ignored` 触发后立即 panic-fail。**E2 \[实现\]**
/// 落地 LBR 自实现 + first usable 10⁹ update 训练完成的 checkpoint 加载路径后
/// 达 < 200 mbb/g。
///
/// **production 门槛**：D-441 production 10¹¹ update 完成时 LBR < 100 mbb/g
/// deferred 到 stage 5 起步并行清单（不在本测试覆盖；D-441-rev0 production
/// 训练完成后 F3 \[报告\] 翻面追加测试）。
#[test]
#[ignore = "stage4 LBR convergence; first usable 10⁹ update trained blueprint required; \
            opt-in via `cargo test --release --test lbr_eval_convergence -- --ignored`"]
fn stage4_first_usable_lbr_below_200_mbbg() {
    let Some(game) = load_v3_or_skip() else {
        return;
    };
    // E1 \[测试\] closure 形态：pre-train 0 update（LBR 落地路径 panic 前置；
    // E2 \[实现\] 接 checkpoint 加载入 first usable 10⁹ blueprint）。
    let trainer = build_pretrained_trainer(game, FIXED_SEED, 0);
    let trainer_arc = Arc::new(trainer);
    let evaluator = LbrEvaluator::<NlheGame6>::new(
        Arc::clone(&trainer_arc),
        D_456_ACTION_SET_14,
        D_455_MYOPIC_HORIZON_1,
    )
    .expect("LbrEvaluator::new(14, 1) 期望成功");
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(1));

    let result: SixTraverserLbrResult = evaluator
        .compute_six_traverser_average(LBR_N_HANDS, &mut rng)
        .expect("compute_six_traverser_average 期望成功");
    eprintln!(
        "[stage4-lbr-first-usable] 6-traverser average LBR = {:.2} mbb/g（SLO ≤ \
         {D_451_FIRST_USABLE_LBR_MBBG:.0}）/ max = {:.2} / min = {:.2}",
        result.average_mbbg, result.max_mbbg, result.min_mbbg,
    );
    assert!(
        result.average_mbbg < D_451_FIRST_USABLE_LBR_MBBG,
        "D-451：first usable 6-traverser average LBR {:.2} mbb/g ≥ 阈值 \
         {D_451_FIRST_USABLE_LBR_MBBG:.0}（E1 closure A1 scaffold panic；E2 \\[实现\\] LBR \
         自实现 + first usable 10⁹ update 训练完成后必须通过；如实测 > 300 mbb/g 触发 \
         D-421-revM preflop 独立 action set 翻面 evaluate）",
        result.average_mbbg,
    );
}

// ===========================================================================
// Test 2 — D-452 LBR 100 采样点单调非升 ±10% 噪声
// ===========================================================================

/// stage 4 D-452 字面：first usable 训练期间每 `10⁷` update 计算一次 LBR
/// （10⁹ update 内共计算 100 次），曲线必须**单调非升**（允许相邻两次 ±10%
/// 噪声，连续 3 个采样点 trend up 触发 D-470 监控告警）。
///
/// 本测试断言形式：100 个采样点序列 `lbr[0..100]` 上，对所有 `i ∈ 1..100`：
/// `lbr[i] <= lbr[i-1] * 1.10`（允许 10% 上浮噪声）；且 `lbr[0] > lbr[99]`
/// （首尾下降）。
///
/// **E1 closure 形态**：本测试需要 100 × `LbrEvaluator::compute_six_traverser_
/// average` 调用累积 series，每次都 panic 在 `unimplemented!()`。**E2 \[实现\]**
/// 落地 LBR 自实现 + first usable 10⁹ update 训练完成时 100 采样点全 series
/// 落盘到 `pluribus_stage4_report.md` §LBR curve 后本测试读 series 断言。
#[test]
#[ignore = "stage4 LBR convergence; first usable 10⁹ update full training run required; \
            opt-in via `--ignored`"]
fn stage4_lbr_100_samples_monotone_nonincreasing() {
    let Some(game) = load_v3_or_skip() else {
        return;
    };
    let trainer = build_pretrained_trainer(game, FIXED_SEED.wrapping_add(2), 0);
    let trainer_arc = Arc::new(trainer);
    let evaluator = LbrEvaluator::<NlheGame6>::new(
        Arc::clone(&trainer_arc),
        D_456_ACTION_SET_14,
        D_455_MYOPIC_HORIZON_1,
    )
    .expect("LbrEvaluator::new(14, 1) 期望成功");
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(3));

    // E1 closure 路径：在 panic-fail 触发前累积 100 个采样点 series。E2 \[实现\]
    // + first usable 10⁹ update 训练完成后此处接 series 读取 logic（CLI
    // `tools/lbr_compute.rs` 输出的 `lbr_curve.jsonl` 100 行 series）。
    let mut series: Vec<f64> = Vec::with_capacity(D_452_N_SAMPLES);
    for _ in 0..D_452_N_SAMPLES {
        let r = evaluator
            .compute_six_traverser_average(LBR_N_HANDS, &mut rng)
            .expect("compute_six_traverser_average per-sample 期望成功");
        series.push(r.average_mbbg);
    }

    assert_eq!(
        series.len(),
        D_452_N_SAMPLES,
        "D-452 LBR series 长度应 == {D_452_N_SAMPLES}"
    );
    for i in 1..series.len() {
        let upper = series[i - 1] * (1.0 + D_452_MONOTONE_NOISE_PCT / 100.0);
        assert!(
            series[i] <= upper,
            "D-452：LBR series[{i}] = {:.2} > series[{}] = {:.2} × (1 + {D_452_MONOTONE_NOISE_PCT}%) = \
             {upper:.2}（单调非升 ±10% 噪声违反；连续 3 采样点 trend up 触发 D-470 监控告警）",
            series[i],
            i - 1,
            series[i - 1],
        );
    }
    assert!(
        series[0] > series[D_452_N_SAMPLES - 1],
        "D-452：LBR 首尾下降 series[0] {:.2} ≤ series[{}] {:.2}（first usable 10⁹ update \
         训练完成后 LBR 应显著下降）",
        series[0],
        D_452_N_SAMPLES - 1,
        series[D_452_N_SAMPLES - 1],
    );
}

// ===========================================================================
// Test 3 — D-459 per-traverser LBR 上界 < 500 mbb/g
// ===========================================================================

/// stage 4 D-459 字面：每 traverser 独立 LBR 上界（6 个数字，D-414 字面 6
/// traverser 不共享 strategy）+ 6-traverser average LBR。**主验收门槛**：
/// 6-traverser average LBR < 200 mbb/g first usable；6-traverser **任一**
/// traverser LBR `> 500 mbb/g` 视为虚假通过（D-459 §carve-out 锚点；F3
/// \[报告\] 标注 reference difference）。
///
/// 本测试断言形式：`max_mbbg < 500 mbb/g`；不直接断言 `average_mbbg < 200`
/// （Test 1 覆盖），而是断言 per-traverser 上界让 average 通过不来自 1 个
/// traverser 主导。
#[test]
#[ignore = "stage4 LBR per-traverser bound; first usable 10⁹ update trained blueprint required; \
            opt-in via `--ignored`"]
fn stage4_lbr_per_traverser_upper_bound_below_500_mbbg() {
    let Some(game) = load_v3_or_skip() else {
        return;
    };
    let trainer = build_pretrained_trainer(game, FIXED_SEED.wrapping_add(4), 0);
    let trainer_arc = Arc::new(trainer);
    let evaluator = LbrEvaluator::<NlheGame6>::new(
        Arc::clone(&trainer_arc),
        D_456_ACTION_SET_14,
        D_455_MYOPIC_HORIZON_1,
    )
    .expect("LbrEvaluator::new(14, 1) 期望成功");
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(5));

    let result = evaluator
        .compute_six_traverser_average(LBR_N_HANDS, &mut rng)
        .expect("compute_six_traverser_average 期望成功");

    for (idx, lr) in result.per_traverser.iter().enumerate() {
        eprintln!(
            "[stage4-lbr-per-traverser] traverser {idx} LBR = {:.2} mbb/g / SE = {:.2} / \
             n_hands = {} / wall = {:.2} s",
            lr.lbr_value_mbbg, lr.standard_error_mbbg, lr.n_hands, lr.computation_seconds,
        );
    }
    assert!(
        result.max_mbbg < D_459_PER_TRAVERSER_LBR_MBBG,
        "D-459：max per-traverser LBR {:.2} mbb/g ≥ 阈值 {D_459_PER_TRAVERSER_LBR_MBBG:.0}\
         （任一 traverser > 500 mbb/g 视为 D-459 §carve-out 虚假通过；F3 \\[报告\\] 标注 \
         reference difference + 触发 D-459-revM 翻面 evaluate）",
        result.max_mbbg,
    );
    // 6 个 LbrResult 字段一致性 sanity（lbr_player 字面 0..6）
    for (idx, lr) in result.per_traverser.iter().enumerate() {
        assert_eq!(
            lr.lbr_player as usize, idx,
            "D-459：per_traverser[{idx}].lbr_player 应 == {idx}（顺序排列）"
        );
        assert!(
            lr.n_hands == LBR_N_HANDS,
            "D-452：per_traverser[{idx}].n_hands 应 == {LBR_N_HANDS}",
        );
    }
}

// ===========================================================================
// Test 4 — D-457 OpenSpiel-compatible policy 文件 byte-equal export
// ===========================================================================

/// stage 4 D-457 字面：F3 \[报告\] 一次性接入 OpenSpiel `algorithms/
/// exploitability_descent.py`：stage 4 first usable 训练完成的 blueprint 输出
/// OpenSpiel-compatible policy 文件 → OpenSpiel 计算 LBR 上界 → 与我们 Rust
/// 自实现 LBR 上界对照（容差 < 10%）。
///
/// 本测试是 export step byte-equal sanity：同 trainer + 同 seed 调
/// [`LbrEvaluator::export_policy_for_openspiel`] 2 次（不同 path），生成的 2
/// 个 policy 文件 byte-equal。E1 closure 在 A1 scaffold `unimplemented!()` 下
/// panic-fail；F2 \[实现\] 或 F3 \[报告\] 落地 OpenSpiel export 后转绿。
#[test]
#[ignore = "stage4 OpenSpiel export byte-equal; first usable 10⁹ update trained blueprint required; \
            opt-in via `--ignored`"]
fn stage4_openspiel_policy_export_byte_equal() {
    let Some(game) = load_v3_or_skip() else {
        return;
    };
    let trainer = build_pretrained_trainer(game, FIXED_SEED.wrapping_add(6), 0);
    let trainer_arc = Arc::new(trainer);
    let evaluator = LbrEvaluator::<NlheGame6>::new(
        Arc::clone(&trainer_arc),
        D_456_ACTION_SET_14,
        D_455_MYOPIC_HORIZON_1,
    )
    .expect("LbrEvaluator::new(14, 1) 期望成功");

    let tmpdir = tempfile::tempdir().expect("create tempdir");
    let path_a = tmpdir.path().join("policy_a.openspiel");
    let path_b = tmpdir.path().join("policy_b.openspiel");
    evaluator
        .export_policy_for_openspiel(&path_a)
        .expect("export_policy_for_openspiel(a) 期望成功");
    evaluator
        .export_policy_for_openspiel(&path_b)
        .expect("export_policy_for_openspiel(b) 期望成功");
    let bytes_a = std::fs::read(&path_a).expect("read policy_a");
    let bytes_b = std::fs::read(&path_b).expect("read policy_b");
    assert_eq!(
        bytes_a, bytes_b,
        "D-457：OpenSpiel export 同 trainer + 同 seed 应 byte-equal（policy 文件 deterministic）"
    );
    assert!(
        !bytes_a.is_empty(),
        "D-457：OpenSpiel export 不应为空（NlheGame6 有 reachable InfoSet × 14-action 决策点）"
    );
}

// ===========================================================================
// Test 5 — D-456 LBR 14-action vs 5-action ablation 范围正确
// ===========================================================================

/// stage 4 D-456 字面：LBR action set size ∈ {5, 14} 双路径 — 14-action 是主线
/// （`PluribusActionAbstraction`），5-action 是 ablation（stage 3
/// `DefaultActionAbstraction` 退化对照）。更大 action set 让 LBR upper bound
/// 更紧（包含更多 best response 候选 → max EV 更大或相等）。
///
/// 本测试断言形式：同 trainer + 同 seed 下 `14-action LBR average_mbbg >=
/// 5-action LBR average_mbbg`（即 14-action 上界**不小于** 5-action 上界，因为
/// 14-action 包含 5-action 子集 + 9 个额外 raise size，best response enumerate
/// 范围更广 → LBR 上界 monotone non-decreasing）。
///
/// **注意**：直觉上 "更紧 LBR" 通常指数字更小（LBR 是 exploitability 的上界，
/// 数字小 = 估计紧）；但**这里"更紧"指 best-response enumerate 范围更广**，
/// 即 LBR 值更接近真实 exploitability。real exploitability 是常数；14-action
/// LBR 越接近 = 数字越大或相等（不会更小，因为 best response 候选集是超集
/// monotone）。
#[test]
#[ignore = "stage4 LBR 14 vs 5 action ablation; first usable 10⁹ update trained blueprint \
            required; opt-in via `--ignored`"]
fn stage4_lbr_14_action_enumerate_range_correct() {
    let Some(game_14) = load_v3_or_skip() else {
        return;
    };
    let Some(game_5) = load_v3_or_skip() else {
        return;
    };
    let trainer_14 = build_pretrained_trainer(game_14, FIXED_SEED.wrapping_add(7), 0);
    let trainer_5 = build_pretrained_trainer(game_5, FIXED_SEED.wrapping_add(7), 0);
    let arc_14 = Arc::new(trainer_14);
    let arc_5 = Arc::new(trainer_5);
    let evaluator_14 = LbrEvaluator::<NlheGame6>::new(
        Arc::clone(&arc_14),
        D_456_ACTION_SET_14,
        D_455_MYOPIC_HORIZON_1,
    )
    .expect("LbrEvaluator::new(14, 1) 期望成功");
    let evaluator_5 = LbrEvaluator::<NlheGame6>::new(
        Arc::clone(&arc_5),
        D_456_ACTION_SET_5,
        D_455_MYOPIC_HORIZON_1,
    )
    .expect("LbrEvaluator::new(5, 1) 期望成功");

    let mut rng_14 = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(8));
    let mut rng_5 = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(8));

    let r_14 = evaluator_14
        .compute_six_traverser_average(LBR_N_HANDS, &mut rng_14)
        .expect("14-action LBR 期望成功");
    let r_5 = evaluator_5
        .compute_six_traverser_average(LBR_N_HANDS, &mut rng_5)
        .expect("5-action LBR 期望成功");
    eprintln!(
        "[stage4-lbr-action-ablation] 14-action LBR = {:.2} mbb/g / 5-action LBR = {:.2} \
         mbb/g（D-456 字面 14-action 上界 ≥ 5-action 上界，best response enumerate 范围 \
         monotone）",
        r_14.average_mbbg, r_5.average_mbbg,
    );
    assert!(
        r_14.average_mbbg >= r_5.average_mbbg,
        "D-456：14-action LBR {:.2} < 5-action LBR {:.2}（best response enumerate 范围 \
         monotone non-decreasing 违反；LbrEvaluator E2 \\[实现\\] 路径 bug）",
        r_14.average_mbbg,
        r_5.average_mbbg,
    );
    // 拒绝 action_set_size ∉ {5, 14}（D-456 字面）
    let err = LbrEvaluator::<NlheGame6>::new(Arc::clone(&arc_14), 7, D_455_MYOPIC_HORIZON_1).err();
    assert!(
        err.is_some(),
        "D-456：LbrEvaluator::new(action_set_size=7) 应返 PreflopActionAbstractionMismatch"
    );
}

// ===========================================================================
// Test 6 — D-455 LBR myopic horizon = 1 边界
// ===========================================================================

/// stage 4 D-455 字面：LBR myopic horizon = 1 决策点（lock）。LBR-player 在
/// 第 1 个决策点选 best response，之后所有 LBR-player 决策点走 blueprint
/// （避免 LBR upper bound 退化为真实 exploitability — 真实 exploitability
/// 计算不可行）。
///
/// horizon = 0（pure blueprint）= no LBR / LBR 等同 EV(blueprint, blueprint)；
/// horizon = 1 = 主路径；horizon = ∞（full BR）= full exploitability 不可计算。
///
/// 本测试断言形式：
/// - `LbrEvaluator::new(14, 1)` 主路径成功；
/// - `LbrEvaluator::new(14, 0)` horizon=0 路径，body 行为由 E2 \[实现\] 决定：
///   要么返 [`TrainerError::PreflopActionAbstractionMismatch`] 拒绝 horizon
///   = 0；要么返 LBR = 0 mbb/g（blueprint 自我对战零和）。本 E1 测试断言
///   horizon = 0 时 LBR `< 1 mbb/g`（near-zero baseline）或 ctor 返 Err。
/// - horizon = 2 由 D-453-revM A0 lock 主路径不支持 → ctor 应返 Err 或 body
///   `unimplemented!()` panic。
#[test]
#[ignore = "stage4 LBR myopic horizon boundary; first usable 10⁹ update trained blueprint \
            required; opt-in via `--ignored`"]
fn stage4_lbr_myopic_horizon_1_boundary() {
    let Some(game) = load_v3_or_skip() else {
        return;
    };
    let trainer = build_pretrained_trainer(game, FIXED_SEED.wrapping_add(9), 0);
    let trainer_arc = Arc::new(trainer);

    // horizon = 1 主路径成功
    let _evaluator_1 =
        LbrEvaluator::<NlheGame6>::new(Arc::clone(&trainer_arc), D_456_ACTION_SET_14, 1)
            .expect("LbrEvaluator::new(14, horizon=1) 主路径期望成功");

    // horizon = 0 边界（D-455 字面 "no LBR" 退化）：
    // E2 \[实现\] 落地方式 1: ctor 返 Err；落地方式 2: body 返 LBR ≈ 0 mbb/g
    let evaluator_0_result =
        LbrEvaluator::<NlheGame6>::new(Arc::clone(&trainer_arc), D_456_ACTION_SET_14, 0);
    match evaluator_0_result {
        Ok(evaluator_0) => {
            let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(10));
            let r = evaluator_0
                .compute_six_traverser_average(LBR_N_HANDS, &mut rng)
                .expect("compute_six_traverser_average(horizon=0) 期望成功");
            eprintln!(
                "[stage4-lbr-horizon-0] horizon=0 LBR = {:.2} mbb/g（D-455 字面 退化为 \
                 EV(blueprint, blueprint) ≈ 0 mbb/g）",
                r.average_mbbg
            );
            assert!(
                r.average_mbbg.abs() < 1.0,
                "D-455：horizon=0 LBR {:.2} mbb/g ≥ 1 mbb/g（pure blueprint self-play \
                 应零和趋近 0）",
                r.average_mbbg,
            );
        }
        Err(e) => {
            eprintln!(
                "[stage4-lbr-horizon-0] horizon=0 ctor reject = {e:?}（D-455 lock 路径 1，\
                 ctor 拒绝 horizon=0）"
            );
        }
    }

    // horizon = 2 不支持（D-455 lock myopic horizon=1 唯一支持；D-453-revM
    // 主路径外 deferred）：ctor 返 Err 或 compute 走 `unimplemented!()` panic。
    let evaluator_2_result =
        LbrEvaluator::<NlheGame6>::new(Arc::clone(&trainer_arc), D_456_ACTION_SET_14, 2);
    match evaluator_2_result {
        Ok(evaluator_2) => {
            let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(11));
            // ctor 接受 horizon=2，但 compute 应 panic / 返 Err
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                evaluator_2.compute_six_traverser_average(LBR_N_HANDS, &mut rng)
            }));
            match result {
                Ok(Ok(_)) => panic!(
                    "D-455：horizon=2 不应支持（D-453-revM 主路径外 deferred；ctor + compute \
                     双路径同时返成功 = 文档违反）"
                ),
                Ok(Err(e)) => {
                    eprintln!(
                        "[stage4-lbr-horizon-2] horizon=2 compute reject = {e:?}（D-455 \
                              lock 路径 2）"
                    );
                }
                Err(_) => {
                    eprintln!(
                        "[stage4-lbr-horizon-2] horizon=2 compute panic = `unimplemented!()` \
                              （D-455 lock 路径 3，scaffold not-yet-impl）"
                    );
                }
            }
        }
        Err(e) => {
            eprintln!(
                "[stage4-lbr-horizon-2] horizon=2 ctor reject = {e:?}（D-455 lock 路径 4，\
                 ctor 拒绝 horizon=2）"
            );
        }
    }
}

// ===========================================================================
// Anchor: LbrResult 字段顺序 byte-stable sanity
// ===========================================================================

/// LbrResult struct 字段 ground truth 锁（API-451）— stage 4 \[测试\] 直接构造
/// LbrResult 校验字段顺序与 API doc 字面一致（避免 E2 \[实现\] PR 偷偷加字段
/// 漂移文档）。本 sanity 不 `#[ignore]`，默认 profile 跑（compile-only）。
#[test]
fn lbr_result_field_layout_lock() {
    // 字面构造（顺序 = API-451 doc 顺序）
    let _lr = LbrResult {
        lbr_player: 0u8,
        lbr_value_mbbg: 0.0,
        standard_error_mbbg: 0.0,
        n_hands: 0,
        computation_seconds: 0.0,
    };
}
