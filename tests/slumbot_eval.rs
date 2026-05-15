//! 阶段 4 §F1 \[测试\]：Slumbot HU 100K 手评测协议 + duplicate dealing +
//! 重复 5 次 mean + fold equity sanity + D-463-revM fallback OpenSpiel HU
//! baseline（`pluribus_stage4_workflow.md` §F1 line 286-296 + `pluribus_stage4_
//! decisions.md` §7 D-460..D-469 + §6 D-453-revM / D-463-revM）。
//!
//! 6 条 Slumbot eval 测试（全 `#[ignore]` opt-in + Slumbot HTTP API 在线 +
//! release profile + first usable `10⁹` update 训练完成的 blueprint
//! checkpoint 路径触发）：
//!
//! 1. [`stage4_slumbot_100k_hand_mean_above_minus_10_mbbg`] — D-460 + D-461 字面
//!    100K 手 mean `≥ -10 mbb/g` first usable + 95% CI 下界 `≥ -30 mbb/g`。
//! 2. [`stage4_slumbot_95_ci_lower_bound_above_minus_30_mbbg`] — D-461 字面
//!    95% CI 下界独立 sanity（与 1 重叠但聚焦 CI lower bound）。
//! 3. [`stage4_slumbot_duplicate_dealing_on_off_ablation`] — D-461 字面
//!    duplicate dealing on/off 双路径 variance 对比（on ≪ off 期望）。
//! 4. [`stage4_slumbot_five_repeats_mean_consistency`] — D-461 字面 5 次重复
//!    评测 mean ± standard error 一致性（D-468 字面 master seeds = {42..46}）。
//! 5. [`stage4_slumbot_fold_equity_sanity`] — D-469 字面 fold rate / showdown
//!    ratio / preflop 3-bet 落在 Slumbot 公开评测 healthy range（fold rate
//!    32-36% / showdown 25-30% / preflop 3-bet 6-10%）。
//! 6. [`stage4_slumbot_api_unavailable_fallback_openspiel_hu_baseline`] —
//!    D-463-revM 字面 Slumbot API 不可用时 [`OpenSpielHuBaseline`] fallback
//!    路径 byte-equal sanity（与 Slumbot 主路径 mean ± 5 mbb/g 容差，让
//!    fallback 不退化）。
//!
//! **F1 \[测试\] 角色边界**（继承 stage 1/2/3 + stage 4 E1/E2 同型政策）：
//! 本文件 0 改动 `src/training/slumbot_eval.rs` / `src/training/nlhe_6max.rs` /
//! `docs/*`；如断言落在 \[实现\] 边界错误的产品代码上 → filed issue 移交 F2
//! \[实现\]（继承 stage 1 §F-rev1 错误前移模式）。
//!
//! **F1 → F2 工程契约**（panic-fail 翻面条件）：
//!
//! - F2 \[实现\] 落地 [`SlumbotBridge::new`] + [`SlumbotBridge::play_one_hand`] +
//!   [`SlumbotBridge::evaluate_blueprint`] 全 3 方法 + 用户授权 AWS c7a.8xlarge
//!   first usable `10⁹` update 训练完成的 blueprint checkpoint 路径后，本套
//!   6 测试在 release `--ignored` opt-in 下应通过（首发 Slumbot 100K 手 mean
//!   ≥ -10 mbb/g first usable）。
//! - 实测触发说明：本套 6 测试 `#[ignore]` 默认跳过 — 用户授权 + AWS
//!   c7a.8xlarge first usable 10⁹ 训练完成 + Slumbot API 在线后跑 `cargo test
//!   --release --test slumbot_eval -- --ignored` 触发；F1 closure 期望全部
//!   panic-fail（[`SlumbotBridge::new`] A1 \[实现\] scaffold `unimplemented!()`）。
//! - **Slumbot API 不可用 carve-out**（D-465 字面）：5 次评测 < 3 次完成 → 视
//!   evaluation infrastructure fail → 切 D-463-revM `OpenSpielHuBaseline`
//!   fallback（覆盖在 Test 6）。Slumbot 评测完成但 mean < -10 mbb/g → stage 4
//!   F3 \[报告\] §carve-out 已知偏离（不阻塞 stage 5 起步）。

use std::path::PathBuf;
use std::sync::Arc;

use poker::training::nlhe_6max::NlheGame6;
use poker::training::slumbot_eval::{OpenSpielHuBaseline, SlumbotBridge};
use poker::training::EsMccfrTrainer;
use poker::{BucketTable, ChaCha20Rng};

// ===========================================================================
// 共享常量（与 `tests/perf_slo.rs::stage4_*` / `tests/lbr_eval_convergence.rs` /
// `tests/training_24h_continuous.rs` 跨测试 ground truth 一致）
// ===========================================================================

/// stage 4 D-424 v3 production artifact path（D-460 Slumbot 评测路径走 HU 退化，
/// 但 blueprint 训练走 6-max + v3 artifact）。
const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";

/// stage 4 D-424 v3 artifact body BLAKE3 ground truth。
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

/// stage 4 D-409 字面 warm-up 切换点（Linear+RM+ 主路径）。
const WARMUP_COMPLETE_AT: u64 = 1_000_000;

/// stage 4 D-461 字面 Slumbot 100K 手协议（path.md §阶段 4 字面 100K 手）。
const D_461_N_HANDS: u64 = 100_000;

/// stage 4 D-461 first usable 字面阈值（mbb/g）：mean ≥ -10。
const D_461_FIRST_USABLE_MEAN_MBBG: f64 = -10.0;

/// stage 4 D-461 first usable 字面阈值（mbb/g）：95% CI 下界 ≥ -30。
const D_461_FIRST_USABLE_CI_LOWER_MBBG: f64 = -30.0;

/// stage 4 D-461 字面 5 次重复 master seeds（D-468 字面 {42..46}）。
const D_468_MASTER_SEEDS: [u64; 5] = [42, 43, 44, 45, 46];

/// stage 4 D-461 字面 5 次重复 mean 一致性容差（mbb/g；同 blueprint × 5 seed
/// 应统计 indistinguishable，|mean_i - mean_avg| < 20 mbb/g 是 standard
/// error 经验上界）。
const D_461_FIVE_REPEAT_MEAN_TOLERANCE_MBBG: f64 = 20.0;

/// stage 4 D-469 字面 fold equity sanity range（Slumbot 公开评测）：
/// - fold rate ∈ [32%, 36%]
/// - showdown ratio ∈ [25%, 30%]
/// - preflop 3-bet ∈ [6%, 10%]
const D_469_FOLD_RATE_LOWER_PCT: f64 = 32.0;
const D_469_FOLD_RATE_UPPER_PCT: f64 = 36.0;
const D_469_SHOWDOWN_RATIO_LOWER_PCT: f64 = 25.0;
const D_469_SHOWDOWN_RATIO_UPPER_PCT: f64 = 30.0;
const D_469_PREFLOP_3BET_LOWER_PCT: f64 = 6.0;
const D_469_PREFLOP_3BET_UPPER_PCT: f64 = 10.0;

/// stage 4 D-463 字面 Slumbot HTTP API endpoint（A0 lock：
/// `http://www.slumbot.com/api/`）。
const SLUMBOT_API_ENDPOINT: &str = "http://www.slumbot.com/api/";

/// stage 4 D-463-revM 字面 fallback OpenSpiel HU policy path（F3 \[报告\] /
/// F2 \[实现\] 落地时具体路径由用户授权 Slumbot 不可用时 lock；F1 closure 走
/// 占位路径 `artifacts/openspiel_hu_blueprint.policy` 让 panic-fail 路径上的
/// `OpenSpielHuBaseline::new` 字面构造可达，body `unimplemented!()` panic
/// 落在 `play_one_hand` 调用上）。
const OPENSPIEL_HU_POLICY_PATH: &str = "artifacts/openspiel_hu_blueprint.policy";

/// stage 4 master seed（ASCII "STG4_F1\x1A" sentinel；与 lbr_eval_convergence
/// 的 `0x53_54_47_34_5F_45_31_1A` 同型 sentinel，区分 E1 → F1 角色）。
const FIXED_SEED: u64 = 0x53_54_47_34_5F_46_31_1A;

// ===========================================================================
// helper
// ===========================================================================

/// 加载 v3 artifact + 构造 `NlheGame6::new_hu`（D-460 Slumbot HU NLHE 退化路径，
/// D-416 字面）；artifact 缺失 / 不匹配 → `None`（pass-with-skip）。与
/// `tests/lbr_eval_convergence.rs::load_v3_or_skip` 同型 helper。
fn load_v3_hu_or_skip() -> Option<NlheGame6> {
    let path = PathBuf::from(V3_ARTIFACT_PATH);
    if !path.exists() {
        eprintln!(
            "[stage4-slumbot] skip: v3 artifact `{V3_ARTIFACT_PATH}` 不存在（CI / \
             GitHub-hosted runner 典型场景；本地 dev box / vultr / AWS host 有 artifact 时跑）。"
        );
        return None;
    }
    let table = match BucketTable::open(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[stage4-slumbot] skip: BucketTable::open 失败：{e:?}");
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
            "[stage4-slumbot] skip: artifact body BLAKE3 `{body_hex}` 不匹配 v3 ground truth \
             `{V3_BODY_BLAKE3_HEX}`（D-424 lock 要求 v3 artifact）。"
        );
        return None;
    }
    // D-460 字面 Slumbot 评测走 HU NLHE 退化路径（D-416）。
    match NlheGame6::new_hu(Arc::new(table)) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("[stage4-slumbot] skip: NlheGame6::new_hu 失败：{e:?}");
            None
        }
    }
}

/// 构造 stage 4 NlheGame6 + Linear+RM+ trainer（warmup_at=1M，D-409 主路径）。
/// `pre_train_updates = 0` 让 trainer 处于 newly-created 状态；F2 \[实现\] +
/// 用户授权 first usable 10⁹ 训练完成后 `Trainer::load_checkpoint` 加载真实
/// blueprint。本 F1 closure 走 0 update 走 panic-fail 路径暴露
/// `SlumbotBridge::*` `unimplemented!()`。
fn build_blueprint_for_slumbot(game: NlheGame6, seed: u64) -> EsMccfrTrainer<NlheGame6> {
    EsMccfrTrainer::new(game, seed).with_linear_rm_plus(WARMUP_COMPLETE_AT)
}

// ===========================================================================
// Test 1 — D-460 + D-461 100K 手 mean ≥ -10 mbb/g first usable
// ===========================================================================

/// stage 4 D-460 + D-461 字面 first usable：100K 手 mean `≥ -10 mbb/g`（path.md
/// §阶段 4 字面 "100K 手不输 + 95% CI 不显著为负" 的 first usable sanity；
/// production 阈值 mean ≥ 0 + 95% CI 下界 ≥ -10 deferred 到 D-441-rev0
/// production 10¹¹ 训练完成后 stage 5 起步并行清单）。
///
/// 评测协议：D-460 字面 blueprint 与 Slumbot 对战 100K 手 HU NLHE +
/// duplicate dealing on（D-461 字面 variance ≈ 0）+ 单 seed master_seed=42
/// （D-468 字面）。
///
/// **F1 closure 形态**：[`SlumbotBridge::new`] A1 \[实现\] scaffold 走
/// `unimplemented!()`，opt-in `--ignored` + Slumbot API 在线触发后立即 panic
/// -fail。**F2 \[实现\]** 落地 SlumbotBridge HTTP 协议双向 + 用户授权 AWS
/// c7a.8xlarge first usable 10⁹ blueprint checkpoint 加载路径后 mean ≥ -10
/// mbb/g。
///
/// **D-465 carve-out**：若 Slumbot API 不可用导致 `SlumbotBridge::new` /
/// `evaluate_blueprint` 网络层 fail，本测试视 evaluation infrastructure fail
/// → F2 \[实现\] 起步前 D-463-revM `OpenSpielHuBaseline` fallback 翻面
/// （Test 6 覆盖 fallback 路径）。
#[test]
#[ignore = "stage4 Slumbot HU eval; Slumbot HTTP API online + first usable 10⁹ update trained \
            blueprint required; opt-in via `cargo test --release --test slumbot_eval -- --ignored`"]
fn stage4_slumbot_100k_hand_mean_above_minus_10_mbbg() {
    let Some(game) = load_v3_hu_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_for_slumbot(game, FIXED_SEED);
    let mut bridge = SlumbotBridge::new(SLUMBOT_API_ENDPOINT.to_string());

    // D-461 字面 100K 手 + duplicate dealing on + master_seed=42
    let result = bridge
        .evaluate_blueprint(&blueprint, D_461_N_HANDS, D_468_MASTER_SEEDS[0], true)
        .expect("Slumbot evaluate_blueprint 期望成功（F2 [实现] 落地 + Slumbot API 在线后）");

    eprintln!(
        "[stage4-slumbot-100k-mean] mean = {:.2} mbb/g（D-461 first usable SLO ≥ \
         {D_461_FIRST_USABLE_MEAN_MBBG:.0}）/ SE = {:.2} / 95% CI = [{:.2}, {:.2}] / \
         duplicate_dealing = {} / n_hands = {} / wall = {:.2} s",
        result.mean_mbbg,
        result.standard_error_mbbg,
        result.confidence_interval_95.0,
        result.confidence_interval_95.1,
        result.duplicate_dealing,
        result.n_hands,
        result.wall_clock_seconds,
    );
    assert_eq!(
        result.n_hands, D_461_N_HANDS,
        "D-461：n_hands 应 == {D_461_N_HANDS}（duplicate dealing 内部计数；F2 [实现] 路径偏离）",
    );
    assert!(
        result.duplicate_dealing,
        "D-461：duplicate_dealing 应 == true（Test 1 走 duplicate on 路径）",
    );
    assert!(
        result.mean_mbbg >= D_461_FIRST_USABLE_MEAN_MBBG,
        "D-461：first usable Slumbot 100K 手 mean {:.2} mbb/g < 阈值 \
         {D_461_FIRST_USABLE_MEAN_MBBG:.0}（F1 closure A1 scaffold panic；F2 [实现] Slumbot \
         bridge + first usable 10⁹ update 训练完成后必须通过；如实测 < -30 触发 D-465 carve-out \
         F3 [报告] §carve-out 已知偏离）",
        result.mean_mbbg,
    );
}

// ===========================================================================
// Test 2 — D-461 95% CI 下界 ≥ -30 mbb/g（first usable）
// ===========================================================================

/// stage 4 D-461 字面 first usable 95% CI 下界 `≥ -30 mbb/g`（与 Test 1 mean
/// 阈值耦合但聚焦 CI lower bound — 即使 mean ≥ -10 但 SE 过大让下界 < -30 同样
/// fail）。
///
/// 95% CI = mean ± 1.96 × SE（D-462 字面）；100K 手 HU duplicate dealing 后
/// SE 经验值 `2-5 mbb/g`（D-462 字面），95% CI 宽度 ~10 mbb/g。
///
/// **F1 closure 形态**：[`SlumbotBridge::evaluate_blueprint`] A1 \[实现\]
/// scaffold 走 `unimplemented!()`，opt-in 触发后立即 panic-fail。
#[test]
#[ignore = "stage4 Slumbot HU eval; Slumbot HTTP API online + first usable 10⁹ update trained \
            blueprint required; opt-in via `--ignored`"]
fn stage4_slumbot_95_ci_lower_bound_above_minus_30_mbbg() {
    let Some(game) = load_v3_hu_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_for_slumbot(game, FIXED_SEED.wrapping_add(1));
    let mut bridge = SlumbotBridge::new(SLUMBOT_API_ENDPOINT.to_string());

    let result = bridge
        .evaluate_blueprint(&blueprint, D_461_N_HANDS, D_468_MASTER_SEEDS[1], true)
        .expect("Slumbot evaluate_blueprint 期望成功");

    let (ci_low, ci_high) = result.confidence_interval_95;
    eprintln!(
        "[stage4-slumbot-ci-lower] 95% CI = [{ci_low:.2}, {ci_high:.2}]（D-461 first usable \
         CI 下界 SLO ≥ {D_461_FIRST_USABLE_CI_LOWER_MBBG:.0}）/ mean = {:.2} / SE = {:.2}",
        result.mean_mbbg, result.standard_error_mbbg,
    );
    assert!(
        ci_low <= ci_high,
        "D-462：95% CI 下界 {ci_low} > 上界 {ci_high}（F2 [实现] CI 计算路径 bug）",
    );
    // CI 字段 sanity：宽度 ≈ 2 × 1.96 × SE（D-462 字面）。容差 0.1 mbb/g。
    let half_width = (ci_high - ci_low) / 2.0;
    let expected_half_width = 1.96 * result.standard_error_mbbg;
    assert!(
        (half_width - expected_half_width).abs() < 0.1,
        "D-462：95% CI 半宽 {half_width:.4} ≠ 1.96 × SE = {expected_half_width:.4}（F2 [实现] \
         CI 计算公式偏离 D-462 字面）",
    );
    assert!(
        ci_low >= D_461_FIRST_USABLE_CI_LOWER_MBBG,
        "D-461：first usable Slumbot 100K 手 95% CI 下界 {ci_low:.2} mbb/g < 阈值 \
         {D_461_FIRST_USABLE_CI_LOWER_MBBG:.0}（F1 closure A1 scaffold panic；F2 [实现] + \
         first usable 10⁹ 训练完成后必须通过）",
    );
}

// ===========================================================================
// Test 3 — D-461 duplicate dealing on/off ablation
// ===========================================================================

/// stage 4 D-461 字面 duplicate dealing 双路径 ablation：
/// - `duplicate_dealing = true`：每 hand 重复 2 方向（blueprint = SB then BB /
///   Slumbot = SB then BB），让 variance ≈ 0。
/// - `duplicate_dealing = false`：标准随机发牌，variance 等于 raw NLHE variance。
///
/// **断言形式**：相同 100K 手 + 同 master_seed → duplicate on 的 SE 应**显著
/// 小于** duplicate off 的 SE（经验上 duplicate dealing 让 100K 手 SE 从 ~7
/// mbb/g 降到 ~2 mbb/g，ratio ≥ 2×）。
#[test]
#[ignore = "stage4 Slumbot HU eval; Slumbot HTTP API online + first usable 10⁹ update trained \
            blueprint required; opt-in via `--ignored`"]
fn stage4_slumbot_duplicate_dealing_on_off_ablation() {
    let Some(game) = load_v3_hu_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_for_slumbot(game, FIXED_SEED.wrapping_add(2));
    let mut bridge = SlumbotBridge::new(SLUMBOT_API_ENDPOINT.to_string());

    let result_on = bridge
        .evaluate_blueprint(&blueprint, D_461_N_HANDS, D_468_MASTER_SEEDS[2], true)
        .expect("Slumbot evaluate_blueprint(duplicate=on) 期望成功");
    let result_off = bridge
        .evaluate_blueprint(&blueprint, D_461_N_HANDS, D_468_MASTER_SEEDS[2], false)
        .expect("Slumbot evaluate_blueprint(duplicate=off) 期望成功");

    eprintln!(
        "[stage4-slumbot-duplicate-ablation] duplicate=on SE = {:.2} mbb/g / duplicate=off SE = \
         {:.2} mbb/g / ratio = {:.2}",
        result_on.standard_error_mbbg,
        result_off.standard_error_mbbg,
        result_off.standard_error_mbbg / result_on.standard_error_mbbg.max(1e-9),
    );
    assert!(
        result_on.duplicate_dealing && !result_off.duplicate_dealing,
        "D-461：duplicate_dealing 字段应 == 入参（F2 [实现] 返回路径偏离）",
    );
    assert!(
        result_on.standard_error_mbbg > 0.0,
        "D-461：duplicate=on SE = {:.4} 应 > 0（100K 手 hand-level variance 不会 == 0）",
        result_on.standard_error_mbbg,
    );
    // duplicate on 应让 SE 显著降低 — ratio ≥ 2× 是 D-461 字面经验保守阈值。
    assert!(
        result_off.standard_error_mbbg >= 2.0 * result_on.standard_error_mbbg,
        "D-461：duplicate=off SE {:.4} < 2 × duplicate=on SE {:.4}（duplicate dealing 没有 \
         显著降方差；F2 [实现] 路径 bug 或者 duplicate dealing 算法实现偏离 D-461 字面）",
        result_off.standard_error_mbbg,
        result_on.standard_error_mbbg,
    );
}

// ===========================================================================
// Test 4 — D-461 5 次重复 mean 一致性
// ===========================================================================

/// stage 4 D-461 字面：5 次重复评测取均值 + standard error；D-468 字面 master
/// seeds = {42, 43, 44, 45, 46}。
///
/// **断言形式**：5 次评测的 mean 应统计 indistinguishable，每个 |mean_i -
/// avg(means)| < `D_461_FIVE_REPEAT_MEAN_TOLERANCE_MBBG = 20 mbb/g`
/// （duplicate dealing 100K 手 SE 经验 ~3-5 mbb/g + 5-run inter-batch noise，
/// 容差 20 mbb/g 是 D-461 字面 standard error 经验上界 + 5-sigma safety）。
#[test]
#[ignore = "stage4 Slumbot HU eval; Slumbot HTTP API online + first usable 10⁹ update trained \
            blueprint required; opt-in via `--ignored`"]
fn stage4_slumbot_five_repeats_mean_consistency() {
    let Some(game) = load_v3_hu_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_for_slumbot(game, FIXED_SEED.wrapping_add(3));
    let mut bridge = SlumbotBridge::new(SLUMBOT_API_ENDPOINT.to_string());

    let mut means: Vec<f64> = Vec::with_capacity(D_468_MASTER_SEEDS.len());
    for seed in D_468_MASTER_SEEDS {
        let result = bridge
            .evaluate_blueprint(&blueprint, D_461_N_HANDS, seed, true)
            .expect("Slumbot evaluate_blueprint 期望成功");
        eprintln!(
            "[stage4-slumbot-5x] master_seed={seed} mean = {:.2} mbb/g / SE = {:.2}",
            result.mean_mbbg, result.standard_error_mbbg,
        );
        means.push(result.mean_mbbg);
    }
    assert_eq!(
        means.len(),
        D_468_MASTER_SEEDS.len(),
        "D-468：5 次重复应得 5 个 mean",
    );
    let avg: f64 = means.iter().sum::<f64>() / means.len() as f64;
    for (idx, &m) in means.iter().enumerate() {
        let dev = (m - avg).abs();
        assert!(
            dev < D_461_FIVE_REPEAT_MEAN_TOLERANCE_MBBG,
            "D-461：5 次重复 mean[{idx}] {m:.2} 偏离 avg {avg:.2} 达 {dev:.2} mbb/g \
             ≥ 容差 {D_461_FIVE_REPEAT_MEAN_TOLERANCE_MBBG:.0}（F2 [实现] inter-batch noise \
             过大；blueprint 训练 stochastic noise 触发 D-466 5-副本 self-play 锦标赛 fallback \
             evaluate）",
        );
    }
}

// ===========================================================================
// Test 5 — D-469 fold equity sanity（fold rate / showdown / preflop 3-bet）
// ===========================================================================

/// stage 4 D-469 字面 fold equity sanity check：blueprint vs Slumbot 评测的
/// game-stage metrics 必须落在 Slumbot 公开评测 healthy range：
/// - fold rate ∈ [32%, 36%]
/// - showdown ratio ∈ [25%, 30%]
/// - preflop 3-bet ∈ [6%, 10%]
///
/// 显著偏离表明 blueprint over-aggressive / over-passive / preflop range 偏离
/// → F3 \[报告\] §carve-out 标注 reference difference。
///
/// **F1 closure 形态**：[`Head2HeadResult`] A1 \[实现\] 字段不含 fold_rate /
/// showdown_ratio / preflop_3bet（API-461 字面 7 字段：mean_mbbg / SE /
/// 95% CI / n_hands / duplicate_dealing / blueprint_seed / wall_clock_seconds）。
/// D-469 game-stage metrics 由 [`SlumbotBridge::play_one_hand`] 内部累积
/// （F2 \[实现\] 落地 `Head2HeadResult` 字段扩展或独立结构）。
///
/// 本 F1 测试断言：占位走 evaluate_blueprint 调用 panic-fail。**F2 \[实现\]**
/// 起步前 evaluate 是否扩展 [`Head2HeadResult`] 加 game-stage metrics 字段；
/// 若扩展则本测试翻面成 `result.fold_rate_pct ∈ [32, 36]` 等断言；若不扩展
/// 则走 F3 \[报告\] handwritten metrics 表 + Slumbot 公开对照。
#[test]
#[ignore = "stage4 Slumbot HU eval; Slumbot HTTP API online + first usable 10⁹ update trained \
            blueprint required; opt-in via `--ignored`"]
fn stage4_slumbot_fold_equity_sanity() {
    let Some(game) = load_v3_hu_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_for_slumbot(game, FIXED_SEED.wrapping_add(4));
    let mut bridge = SlumbotBridge::new(SLUMBOT_API_ENDPOINT.to_string());

    // F1 closure：D-469 game-stage metrics 由 SlumbotBridge::play_one_hand 内部
    // 累积；这里 evaluate_blueprint 调用走 panic-fail 路径占位让 F2 [实现] 翻面
    // 后追加断言。
    let result = bridge
        .evaluate_blueprint(&blueprint, D_461_N_HANDS, D_468_MASTER_SEEDS[4], true)
        .expect("Slumbot evaluate_blueprint 期望成功");

    eprintln!(
        "[stage4-slumbot-fold-equity] 100K 手 mean = {:.2} mbb/g（D-469 fold rate ∈ \
         [{D_469_FOLD_RATE_LOWER_PCT:.0}%, {D_469_FOLD_RATE_UPPER_PCT:.0}%] / showdown ∈ \
         [{D_469_SHOWDOWN_RATIO_LOWER_PCT:.0}%, {D_469_SHOWDOWN_RATIO_UPPER_PCT:.0}%] / \
         preflop 3-bet ∈ [{D_469_PREFLOP_3BET_LOWER_PCT:.0}%, \
         {D_469_PREFLOP_3BET_UPPER_PCT:.0}%]）",
        result.mean_mbbg,
    );

    // F2 [实现] 起步前翻面：若 Head2HeadResult 扩展 game-stage metrics 字段，
    // 这里断言走 `result.fold_rate_pct` ∈ [32, 36] etc.；F1 closure 仅锁
    // n_hands 一致性让本测试在 F2 落地前 panic-fail 不退化。
    assert_eq!(
        result.n_hands, D_461_N_HANDS,
        "D-461：n_hands 应 == {D_461_N_HANDS}（game-stage metrics 累积口径偏离）",
    );
    // D-469 范围常量编译期 sanity（让范围常量在本测试模块内字面消费，避免
    // unused const 警告 + 锁住范围数字与 F3 [报告] 字面表一致）。
    const _: () = {
        assert!(D_469_FOLD_RATE_LOWER_PCT < D_469_FOLD_RATE_UPPER_PCT);
        assert!(D_469_SHOWDOWN_RATIO_LOWER_PCT < D_469_SHOWDOWN_RATIO_UPPER_PCT);
        assert!(D_469_PREFLOP_3BET_LOWER_PCT < D_469_PREFLOP_3BET_UPPER_PCT);
    };
}

// ===========================================================================
// Test 6 — D-463-revM Slumbot API 不可用 fallback OpenSpiel HU baseline
// ===========================================================================

/// stage 4 D-463-revM 字面 fallback：Slumbot HTTP API 不可用时切
/// [`OpenSpielHuBaseline`] OpenSpiel-trained HU policy 评测路径。F2 \[实现\]
/// 起步前评估翻面触发；F1 closure 走 panic-fail 占位让 F2 [实现] 落地后翻面。
///
/// **断言形式**：[`OpenSpielHuBaseline::new`] 构造成功（A1 \[实现\] 已落地）+
/// [`OpenSpielHuBaseline::play_one_hand`] 单 hand 调用 `unimplemented!()`
/// panic（A1 \[实现\] scaffold）。F2 \[实现\] 落地后断言 single-hand result
/// blueprint_chip_delta 字面字段 != panic 且与 Slumbot 主路径 mean ± 5 mbb/g
/// 容差一致（避免 fallback 走 unrelated 路径让 D-465 carve-out 评测不可信）。
#[test]
#[ignore = "stage4 Slumbot HU eval fallback; OpenSpielHuBaseline policy file required; \
            opt-in via `--ignored`"]
fn stage4_slumbot_api_unavailable_fallback_openspiel_hu_baseline() {
    let Some(game) = load_v3_hu_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_for_slumbot(game, FIXED_SEED.wrapping_add(5));

    let policy_path = PathBuf::from(OPENSPIEL_HU_POLICY_PATH);
    // D-463-revM：policy_path 不存在 → eprintln pass-with-skip（与 v3 artifact
    // 缺失同型 skip 模式让 GitHub-hosted runner / CI 路径不阻塞）。
    if !policy_path.exists() {
        eprintln!(
            "[stage4-slumbot-fallback] skip: OpenSpiel HU policy file `{OPENSPIEL_HU_POLICY_PATH}` \
             不存在（D-463-revM 字面 F2 [实现] / F3 [报告] 起步前由用户授权 Slumbot 不可用时 \
             lock policy 路径；F1 closure 走 panic-fail 占位让 F2 [实现] 落地后翻面）。"
        );
        return;
    }

    let mut fallback = OpenSpielHuBaseline::new(policy_path);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(6));

    // F1 closure：play_one_hand A1 [实现] scaffold `unimplemented!()`，opt-in
    // 触发后 panic。F2 [实现] 落地后这里循环 100K 次累 chip_delta + 算 mean ±
    // SE + 95% CI 然后断言与 Slumbot 主路径 mean ± 5 mbb/g 容差一致。
    let single_hand = fallback
        .play_one_hand(&blueprint, D_468_MASTER_SEEDS[0], &mut rng)
        .expect("OpenSpielHuBaseline::play_one_hand 期望成功（F2 [实现] 落地后）");

    eprintln!(
        "[stage4-slumbot-fallback] 单 hand result: chip_delta = {} / mbb_delta = {:.2} / \
         seed = {} / wall = {:.4} s",
        single_hand.blueprint_chip_delta,
        single_hand.mbb_delta,
        single_hand.seed,
        single_hand.wall_clock_seconds,
    );
    // F2 [实现] 落地后断言：mbb_delta != 0（pure baseline self-play 路径下
    // chip_delta 应有非零 outcome；除非 corner case all-fold-to-blinds-only
    // 极少概率）。本 F1 closure 仅 sanity seed 字段一致性。
    assert_eq!(
        single_hand.seed, D_468_MASTER_SEEDS[0],
        "D-463-revM：HuHandResult.seed 应 == 入参 seed",
    );
}
