//! 阶段 4 §F1 \[测试\]：1M 手 vs 3 类 baseline opponent sanity 评测协议
//! （`pluribus_stage4_workflow.md` §F1 line 286-296 + `pluribus_stage4_
//! decisions.md` §9 D-480..D-489 + §15 API-480..API-484）。
//!
//! 12 条 baseline eval 测试（3 baseline × 4 metric，全 `#[ignore]` opt-in +
//! release profile + first usable `10⁹` update 训练完成的 blueprint
//! checkpoint 路径触发）：
//!
//! - **3 类 baseline**（D-480 字面）：
//!   1. [`RandomOpponent`]：legal action 等概率随机（baseline minimum sanity）
//!   2. [`CallStationOpponent`]：99% call/check + 1% random（aggression baseline）
//!   3. [`TagOpponent`]：preflop 20% top range raise + postflop 70% c-bet
//!      （tight-aggressive 真实人类风格 baseline）
//!
//! - **4 metric**（D-481 / D-482 / D-484 字面）：
//!   1. **mean ≥ floor**（D-481 字面 random ≥ +500 / call-station ≥ +200 /
//!      TAG ≥ +50 mbb/g）→ Tests 1-3
//!   2. **per-traverser min ≥ position-floor**（D-481 字面 blueprint 占 4 或 5
//!      seats，每 seat 位置上 mbb/g 不退化，避免 1 seat 高 + 5 seat 负值 ≈ 0
//!      均值假通过）→ Tests 4-6
//!   3. **1M 手 BLAKE3 byte-equal regression**（D-484 字面 5 hash anchor：
//!      vs random / vs call-station / vs TAG / cross-traverser-average /
//!      single-traverser-best）→ Tests 7-9（每 baseline 1 个 byte-equal anchor）
//!   4. **95% CI 下界 > 0**（D-481 字面 mean ± 1.96 × SE 下界严格 > 0，
//!      统计上 blueprint 必胜该 baseline）→ Tests 10-12
//!
//! **F1 \[测试\] 角色边界**（继承 stage 1/2/3 + stage 4 E1/E2 同型政策）：
//! 本文件 0 改动 `src/training/baseline_eval.rs` / `src/training/nlhe_6max.rs` /
//! `docs/*`；如断言落在 \[实现\] 边界错误的产品代码上 → filed issue 移交 F2
//! \[实现\]（继承 stage 1 §F-rev1 错误前移模式）。
//!
//! **F1 → F2 工程契约**（panic-fail 翻面条件）：
//!
//! - F2 \[实现\] 落地 [`RandomOpponent::act`] / [`CallStationOpponent::act`] /
//!   [`TagOpponent::act`] 3 trait impl + [`evaluate_vs_baseline`] free function +
//!   用户授权 AWS c7a.8xlarge first usable `10⁹` update 训练完成的 blueprint
//!   checkpoint 路径后，本套 12 测试在 release `--ignored` opt-in 下应通过。
//! - 实测触发说明：本套 12 测试 `#[ignore]` 默认跳过 — 用户授权 + AWS c7a.8xlarge
//!   first usable 10⁹ 训练完成后跑 `cargo test --release --test baseline_eval --
//!   --ignored` 触发；F1 closure 期望全部 panic-fail（A1 \[实现\] scaffold
//!   `unimplemented!()`）。
//! - **D-489 carve-out**：3 类 baseline 任一未达阈值 → F3 \[报告\] §carve-out 已
//!   知偏离；两类或以上未达 → stage 4 出口 P0 阻塞。TAG 是 borderline（D-489
//!   字面 +50 mbb/g 阈值有 ±20% noise 余地）。

use std::path::PathBuf;
use std::sync::Arc;

use poker::training::baseline_eval::{
    evaluate_vs_baseline, CallStationOpponent, Opponent6Max, RandomOpponent, TagOpponent,
};
use poker::training::nlhe_6max::NlheGame6;
use poker::training::EsMccfrTrainer;
use poker::{BucketTable, ChaCha20Rng};

// ===========================================================================
// 共享常量（与 `tests/perf_slo.rs::stage4_*` / `tests/lbr_eval_convergence.rs` /
// `tests/slumbot_eval.rs` 跨测试 ground truth 一致）
// ===========================================================================

/// stage 4 D-424 v3 production artifact path。
const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";

/// stage 4 D-424 v3 artifact body BLAKE3 ground truth。
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

/// stage 4 D-409 字面 warm-up 切换点。
const WARMUP_COMPLETE_AT: u64 = 1_000_000;

/// stage 4 D-481 字面 baseline 评测每次 1M 手。
const D_481_N_HANDS: u64 = 1_000_000;

/// stage 4 D-481 字面 master seeds（3 seed × 3 baseline = 9 run）。
const D_481_MASTER_SEEDS: [u64; 3] = [42, 43, 44];

/// stage 4 D-481 字面 3 类 baseline 阈值（mbb/g first usable）。
const D_481_RANDOM_FLOOR_MBBG: f64 = 500.0;
const D_481_CALL_STATION_FLOOR_MBBG: f64 = 200.0;
const D_481_TAG_FLOOR_MBBG: f64 = 50.0;

/// stage 4 D-481 字面 per-traverser min floor（avoid 1 seat 高均值假通过）：
/// per-position min ≥ 0.5 × global floor（经验保守阈值，避免 1 seat 高 + 5 seat
/// 负值组合均值通过）。
const D_481_PER_TRAVERSER_RATIO: f64 = 0.5;

/// stage 4 D-481 字面 NlheGame6 6-player（D-410）。
const N_PLAYERS_6MAX: usize = 6;

/// stage 4 master seed（ASCII "STG4_F1\x1B" sentinel；F1 step 内多 baseline
/// fixed seed 区分用，与 slumbot_eval 的 `0x...46_31_1A` 隔开）。
const FIXED_SEED: u64 = 0x53_54_47_34_5F_46_31_1B;

// ===========================================================================
// helper
// ===========================================================================

/// 加载 v3 artifact + 构造 6-max [`NlheGame6`]（D-481 字面 6-player baseline 评测
/// 走 6-max 主路径，HU 退化由 [`NlheGame6::new_hu`] 单独路径，本测试不涉及）。
/// artifact 缺失 / 不匹配 → `None`（pass-with-skip）。与
/// `tests/perf_slo.rs::stage4_load_v3_artifact_or_skip` 同型 helper。
fn load_v3_6max_or_skip() -> Option<NlheGame6> {
    let path = PathBuf::from(V3_ARTIFACT_PATH);
    if !path.exists() {
        eprintln!(
            "[stage4-baseline] skip: v3 artifact `{V3_ARTIFACT_PATH}` 不存在（CI / \
             GitHub-hosted runner 典型场景）。"
        );
        return None;
    }
    let table = match BucketTable::open(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[stage4-baseline] skip: BucketTable::open 失败：{e:?}");
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
            "[stage4-baseline] skip: artifact body BLAKE3 `{body_hex}` 不匹配 v3 ground truth \
             `{V3_BODY_BLAKE3_HEX}`（D-424 lock 要求 v3 artifact）。"
        );
        return None;
    }
    match NlheGame6::new(Arc::new(table)) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("[stage4-baseline] skip: NlheGame6::new 失败：{e:?}");
            None
        }
    }
}

/// 构造 stage 4 NlheGame6 + Linear+RM+ trainer（warmup_at=1M，D-409 主路径）。
fn build_blueprint_6max(game: NlheGame6, seed: u64) -> EsMccfrTrainer<NlheGame6> {
    EsMccfrTrainer::new(game, seed).with_linear_rm_plus(WARMUP_COMPLETE_AT)
}

/// 计算 95% CI lower bound（D-462 字面 mean - 1.96 × SE）。
fn ci_95_lower(mean: f64, se: f64) -> f64 {
    mean - 1.96 * se
}

// ===========================================================================
// Tests 1-3 — D-481 metric ①：mean ≥ floor（3 类 baseline）
// ===========================================================================

/// stage 4 D-481 ① / D-482 字面 vs random：mean ≥ +500 mbb/g first usable。
///
/// Random opponent 不学习，blueprint 任何 above-50% strategy 都该轻松 +500
/// mbb/g（D-482 字面参考：bot vs random 实测通常 +1000 to +3000 mbb/g 量级，
/// 500 是 floor）。**F1 closure**：[`RandomOpponent::act`] A1 scaffold
/// `unimplemented!()` → opt-in 触发后立即 panic-fail。
#[test]
#[ignore = "stage4 baseline eval; first usable 10⁹ update trained blueprint + F2 [实现] \
            baseline impl required; opt-in via `cargo test --release --test baseline_eval -- --ignored`"]
fn stage4_baseline_random_mean_above_500_mbbg() {
    let Some(game) = load_v3_6max_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_6max(game, FIXED_SEED);
    let mut opponent = RandomOpponent;
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(1));

    let result = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint,
        &mut opponent,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[0],
        &mut rng,
    )
    .expect("evaluate_vs_baseline(RandomOpponent) 期望成功（F2 [实现] 落地后）");

    eprintln!(
        "[stage4-baseline-random-mean] mean = {:.2} mbb/g（D-481 ① SLO ≥ \
         {D_481_RANDOM_FLOOR_MBBG:.0}）/ SE = {:.2} / n_hands = {} / opponent = {}",
        result.mean_mbbg, result.standard_error_mbbg, result.n_hands, result.opponent_name,
    );
    assert_eq!(result.n_hands, D_481_N_HANDS);
    assert_eq!(result.opponent_name, opponent.name());
    assert!(
        result.mean_mbbg >= D_481_RANDOM_FLOOR_MBBG,
        "D-481 ①：blueprint vs random 1M 手 mean {:.2} mbb/g < 阈值 {D_481_RANDOM_FLOOR_MBBG:.0}\
         （F1 closure A1 scaffold panic；F2 [实现] random baseline 落地 + first usable 10⁹ \
         训练完成后必须通过；D-489 carve-out 单 baseline fail → F3 [报告] §carve-out 已知偏离 \
         + 两类以上 fail → stage 4 P0 阻塞）",
        result.mean_mbbg,
    );
}

/// stage 4 D-481 ② / D-482 字面 vs call-station：mean ≥ +200 mbb/g first usable。
///
/// Call-station 不弃牌，blueprint 用 value bet thin 大量 thin value（D-482 字面
/// 参考量级 +500 to +1500 mbb/g，200 是 floor）。
#[test]
#[ignore = "stage4 baseline eval; first usable 10⁹ update trained blueprint + F2 [实现] \
            baseline impl required; opt-in via `--ignored`"]
fn stage4_baseline_call_station_mean_above_200_mbbg() {
    let Some(game) = load_v3_6max_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_6max(game, FIXED_SEED.wrapping_add(2));
    let mut opponent = CallStationOpponent;
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(3));

    let result = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint,
        &mut opponent,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[0],
        &mut rng,
    )
    .expect("evaluate_vs_baseline(CallStationOpponent) 期望成功");

    eprintln!(
        "[stage4-baseline-call-station-mean] mean = {:.2} mbb/g（D-481 ② SLO ≥ \
         {D_481_CALL_STATION_FLOOR_MBBG:.0}）/ SE = {:.2}",
        result.mean_mbbg, result.standard_error_mbbg,
    );
    assert_eq!(result.opponent_name, opponent.name());
    assert!(
        result.mean_mbbg >= D_481_CALL_STATION_FLOOR_MBBG,
        "D-481 ②：blueprint vs call_station 1M 手 mean {:.2} mbb/g < 阈值 \
         {D_481_CALL_STATION_FLOOR_MBBG:.0}（F1 closure A1 scaffold panic；F2 [实现] + first usable \
         10⁹ 训练完成后必须通过）",
        result.mean_mbbg,
    );
}

/// stage 4 D-481 ③ / D-482 字面 vs TAG：mean ≥ +50 mbb/g first usable。
///
/// TAG 是 imperfect baseline 而非 weak opponent，blueprint 需要利用 TAG 的
/// over-fold + under-bluff 漏洞获利（D-482 字面参考量级 +100 to +300 mbb/g，
/// 50 是 floor）。
///
/// **D-489 borderline carve-out**：TAG 阈值 ±20% noise 余地，实测 +30..+60
/// mbb/g range allowed（不强制 ≥ +50 mbb/g pass criteria，F3 [报告] §carve-
/// out 决定走 borderline pass / strict fail）。本测试断言走严格 ≥ +50；
/// borderline 实测后 D-489 carve-out 翻面成 ≥ +30。
#[test]
#[ignore = "stage4 baseline eval; first usable 10⁹ update trained blueprint + F2 [实现] \
            baseline impl required; opt-in via `--ignored`"]
fn stage4_baseline_tag_mean_above_50_mbbg() {
    let Some(game) = load_v3_6max_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_6max(game, FIXED_SEED.wrapping_add(4));
    let mut opponent = TagOpponent::default();
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(5));

    let result = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint,
        &mut opponent,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[0],
        &mut rng,
    )
    .expect("evaluate_vs_baseline(TagOpponent) 期望成功");

    eprintln!(
        "[stage4-baseline-tag-mean] mean = {:.2} mbb/g（D-481 ③ SLO ≥ \
         {D_481_TAG_FLOOR_MBBG:.0} ±20% noise carve-out by D-489）/ SE = {:.2}",
        result.mean_mbbg, result.standard_error_mbbg,
    );
    assert_eq!(result.opponent_name, opponent.name());
    assert!(
        result.mean_mbbg >= D_481_TAG_FLOOR_MBBG,
        "D-481 ③：blueprint vs TAG 1M 手 mean {:.2} mbb/g < 阈值 {D_481_TAG_FLOOR_MBBG:.0}\
         （F1 closure A1 scaffold panic；D-489 borderline carve-out 允许 ±20% noise → \
         F3 [报告] §carve-out 决定 borderline pass / strict fail）",
        result.mean_mbbg,
    );
}

// ===========================================================================
// Tests 4-6 — D-481 metric ②：per-traverser min ≥ position floor
// ===========================================================================

/// stage 4 D-481 ② per-traverser min（vs random）：blueprint 占 4 或 5 seats，
/// 每 seat 位置的 mbb/g 均值 ≥ `0.5 × D_481_RANDOM_FLOOR_MBBG = 250 mbb/g`。
///
/// **断言意图**：避免 1 seat 高 mbb/g + 5 seat 负值组合让 mean ≥ +500 mbb/g
/// 假通过（D-481 字面 per-traverser min ≥ floor 锚点，与 D-459 LBR
/// per-traverser 同型政策）。
///
/// **F1 closure 形态**：[`BaselineEvalResult`] API-484 字面字段不含
/// `per_traverser_mean_mbbg: [f64; 6]`（A1 \[实现\] scaffold 字段是
/// `blueprint_seats: Vec<usize>` / `opponent_seats: Vec<usize>` 表 seat 集合，
/// 不分 per-seat mbb/g）。**F2 \[实现\]** 起步前评估扩展 `BaselineEvalResult`
/// 字段或独立结构 `BaselinePerSeatBreakdown`；本 F1 测试占位让 F2 [实现] 翻面
/// 后追加断言；F1 closure 仅 sanity blueprint_seats / opponent_seats sum ==
/// `N_PLAYERS_6MAX`（6）+ blueprint_seats len ∈ {4, 5}（D-481 字面）。
#[test]
#[ignore = "stage4 baseline eval; first usable 10⁹ update trained blueprint + F2 [实现] \
            baseline impl required; opt-in via `--ignored`"]
fn stage4_baseline_random_per_traverser_min_above_floor() {
    let Some(game) = load_v3_6max_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_6max(game, FIXED_SEED.wrapping_add(6));
    let mut opponent = RandomOpponent;
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(7));

    let result = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint,
        &mut opponent,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[1],
        &mut rng,
    )
    .expect("evaluate_vs_baseline(RandomOpponent) 期望成功");

    // D-481 字面 seat 配置 sanity（与 6-player 总数对应）
    let total_seats = result.blueprint_seats.len() + result.opponent_seats.len();
    eprintln!(
        "[stage4-baseline-random-per-traverser] blueprint_seats = {:?} / opponent_seats = {:?} \
         / total = {total_seats}",
        result.blueprint_seats, result.opponent_seats,
    );
    assert_eq!(
        total_seats, N_PLAYERS_6MAX,
        "D-481：blueprint_seats + opponent_seats 应 == 6-player 总数 {N_PLAYERS_6MAX}",
    );
    assert!(
        result.blueprint_seats.len() == 4 || result.blueprint_seats.len() == 5,
        "D-481：blueprint_seats 长度 {} ∉ {{4, 5}}（字面要求 4 或 5 blueprint copies）",
        result.blueprint_seats.len(),
    );
    // F2 [实现] 起步前翻面：per-seat mbb/g 断言放这里。F1 closure 暂以 mean
    // 双倍下界覆盖（mean ≥ 2 × per-traverser min floor 是必要非充分）。
    let per_traverser_floor = D_481_PER_TRAVERSER_RATIO * D_481_RANDOM_FLOOR_MBBG;
    assert!(
        result.mean_mbbg >= per_traverser_floor,
        "D-481 ②：mean {:.2} < per-traverser min floor {per_traverser_floor:.0} mbb/g（F2 \
         [实现] BaselineEvalResult 扩展 per_traverser_mean_mbbg 字段后翻面成真 per-seat 断言）",
        result.mean_mbbg,
    );
}

/// stage 4 D-481 ② per-traverser min（vs call-station）。
#[test]
#[ignore = "stage4 baseline eval; opt-in via `--ignored`"]
fn stage4_baseline_call_station_per_traverser_min_above_floor() {
    let Some(game) = load_v3_6max_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_6max(game, FIXED_SEED.wrapping_add(8));
    let mut opponent = CallStationOpponent;
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(9));

    let result = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint,
        &mut opponent,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[1],
        &mut rng,
    )
    .expect("evaluate_vs_baseline(CallStationOpponent) 期望成功");

    let total_seats = result.blueprint_seats.len() + result.opponent_seats.len();
    eprintln!(
        "[stage4-baseline-call-station-per-traverser] blueprint_seats = {:?} / opponent_seats = \
         {:?} / total = {total_seats} / mean = {:.2}",
        result.blueprint_seats, result.opponent_seats, result.mean_mbbg,
    );
    assert_eq!(total_seats, N_PLAYERS_6MAX);
    let per_traverser_floor = D_481_PER_TRAVERSER_RATIO * D_481_CALL_STATION_FLOOR_MBBG;
    assert!(
        result.mean_mbbg >= per_traverser_floor,
        "D-481 ②：vs call_station mean {:.2} < per-traverser min floor {per_traverser_floor:.0}",
        result.mean_mbbg,
    );
}

/// stage 4 D-481 ② per-traverser min（vs TAG）。
#[test]
#[ignore = "stage4 baseline eval; opt-in via `--ignored`"]
fn stage4_baseline_tag_per_traverser_min_above_floor() {
    let Some(game) = load_v3_6max_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_6max(game, FIXED_SEED.wrapping_add(10));
    let mut opponent = TagOpponent::default();
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(11));

    let result = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint,
        &mut opponent,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[1],
        &mut rng,
    )
    .expect("evaluate_vs_baseline(TagOpponent) 期望成功");

    let total_seats = result.blueprint_seats.len() + result.opponent_seats.len();
    eprintln!(
        "[stage4-baseline-tag-per-traverser] blueprint_seats = {:?} / opponent_seats = {:?} \
         / total = {total_seats} / mean = {:.2}",
        result.blueprint_seats, result.opponent_seats, result.mean_mbbg,
    );
    assert_eq!(total_seats, N_PLAYERS_6MAX);
    let per_traverser_floor = D_481_PER_TRAVERSER_RATIO * D_481_TAG_FLOOR_MBBG;
    assert!(
        result.mean_mbbg >= per_traverser_floor,
        "D-481 ②：vs TAG mean {:.2} < per-traverser min floor {per_traverser_floor:.0}（D-489 \
         borderline carve-out 余地 ±20%）",
        result.mean_mbbg,
    );
}

// ===========================================================================
// Tests 7-9 — D-484 metric ③：1M 手 BLAKE3 byte-equal regression
// ===========================================================================

/// stage 4 D-484 字面 baseline 评测的 BLAKE3 byte-equal：固定 seed × 固定 1M
/// hand × 同 host 同 toolchain → blueprint vs baseline mbb/g 结果 byte-equal
/// （继承 stage 1 D-051 / stage 2 / stage 3 D-362 determinism 模式）。
///
/// **断言形式**：同 trainer + 同 opponent + 同 master_seed 调用 2 次
/// `evaluate_vs_baseline` → 2 个 `BaselineEvalResult.mean_mbbg` byte-equal
/// （f64 二进制位完全一致；用 `f64::to_bits` 比对避免 ε 浮动）。
///
/// **F1 closure 形态**：[`evaluate_vs_baseline`] A1 \[实现\] scaffold
/// `unimplemented!()` → opt-in 触发后立即 panic。**F2 \[实现\]** 落地后此
/// byte-equal regression 是 stage 4 D-484 字面 anchor（与 stage 1 D-051 /
/// stage 3 D-362 同型）。
#[test]
#[ignore = "stage4 baseline eval BLAKE3 byte-equal; opt-in via `--ignored`"]
fn stage4_baseline_random_1m_hand_byte_equal_regression() {
    let Some(game1) = load_v3_6max_or_skip() else {
        return;
    };
    let Some(game2) = load_v3_6max_or_skip() else {
        return;
    };
    let blueprint1 = build_blueprint_6max(game1, FIXED_SEED.wrapping_add(12));
    let blueprint2 = build_blueprint_6max(game2, FIXED_SEED.wrapping_add(12));
    let mut opp1 = RandomOpponent;
    let mut opp2 = RandomOpponent;
    let mut rng1 = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(13));
    let mut rng2 = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(13));

    let r1 = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint1,
        &mut opp1,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[2],
        &mut rng1,
    )
    .expect("evaluate_vs_baseline run 1 期望成功");
    let r2 = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint2,
        &mut opp2,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[2],
        &mut rng2,
    )
    .expect("evaluate_vs_baseline run 2 期望成功");

    eprintln!(
        "[stage4-baseline-random-byte-equal] run1 mean = {:.4} / run2 mean = {:.4} / SE1 = {:.4} \
         / SE2 = {:.4} / bits1 = {:#x} / bits2 = {:#x}",
        r1.mean_mbbg,
        r2.mean_mbbg,
        r1.standard_error_mbbg,
        r2.standard_error_mbbg,
        r1.mean_mbbg.to_bits(),
        r2.mean_mbbg.to_bits(),
    );
    assert_eq!(
        r1.mean_mbbg.to_bits(),
        r2.mean_mbbg.to_bits(),
        "D-484：固定 seed × 1M hand vs random 重复跑 mean_mbbg 二进制位漂移（F2 [实现] 路径 \
         non-deterministic bug；继承 stage 3 D-362 1M update × 3 BLAKE3 anchor 同型 P0）",
    );
    assert_eq!(
        r1.standard_error_mbbg.to_bits(),
        r2.standard_error_mbbg.to_bits(),
        "D-484：standard_error_mbbg 二进制位漂移",
    );
    assert_eq!(r1.n_hands, r2.n_hands);
    assert_eq!(r1.opponent_name, r2.opponent_name);
}

/// stage 4 D-484 字面 vs call-station 1M 手 byte-equal regression。
#[test]
#[ignore = "stage4 baseline eval BLAKE3 byte-equal; opt-in via `--ignored`"]
fn stage4_baseline_call_station_1m_hand_byte_equal_regression() {
    let Some(game1) = load_v3_6max_or_skip() else {
        return;
    };
    let Some(game2) = load_v3_6max_or_skip() else {
        return;
    };
    let blueprint1 = build_blueprint_6max(game1, FIXED_SEED.wrapping_add(14));
    let blueprint2 = build_blueprint_6max(game2, FIXED_SEED.wrapping_add(14));
    let mut opp1 = CallStationOpponent;
    let mut opp2 = CallStationOpponent;
    let mut rng1 = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(15));
    let mut rng2 = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(15));

    let r1 = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint1,
        &mut opp1,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[2],
        &mut rng1,
    )
    .expect("evaluate_vs_baseline run 1 期望成功");
    let r2 = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint2,
        &mut opp2,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[2],
        &mut rng2,
    )
    .expect("evaluate_vs_baseline run 2 期望成功");

    eprintln!(
        "[stage4-baseline-call-station-byte-equal] run1 bits = {:#x} / run2 bits = {:#x}",
        r1.mean_mbbg.to_bits(),
        r2.mean_mbbg.to_bits(),
    );
    assert_eq!(
        r1.mean_mbbg.to_bits(),
        r2.mean_mbbg.to_bits(),
        "D-484：vs call_station 1M hand byte-equal regression（F2 [实现] non-deterministic bug）",
    );
}

/// stage 4 D-484 字面 vs TAG 1M 手 byte-equal regression。
#[test]
#[ignore = "stage4 baseline eval BLAKE3 byte-equal; opt-in via `--ignored`"]
fn stage4_baseline_tag_1m_hand_byte_equal_regression() {
    let Some(game1) = load_v3_6max_or_skip() else {
        return;
    };
    let Some(game2) = load_v3_6max_or_skip() else {
        return;
    };
    let blueprint1 = build_blueprint_6max(game1, FIXED_SEED.wrapping_add(16));
    let blueprint2 = build_blueprint_6max(game2, FIXED_SEED.wrapping_add(16));
    let mut opp1 = TagOpponent::default();
    let mut opp2 = TagOpponent::default();
    let mut rng1 = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(17));
    let mut rng2 = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(17));

    let r1 = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint1,
        &mut opp1,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[2],
        &mut rng1,
    )
    .expect("evaluate_vs_baseline run 1 期望成功");
    let r2 = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint2,
        &mut opp2,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[2],
        &mut rng2,
    )
    .expect("evaluate_vs_baseline run 2 期望成功");

    eprintln!(
        "[stage4-baseline-tag-byte-equal] run1 bits = {:#x} / run2 bits = {:#x}",
        r1.mean_mbbg.to_bits(),
        r2.mean_mbbg.to_bits(),
    );
    assert_eq!(
        r1.mean_mbbg.to_bits(),
        r2.mean_mbbg.to_bits(),
        "D-484：vs TAG 1M hand byte-equal regression",
    );
}

// ===========================================================================
// Tests 10-12 — D-481 metric ④：95% CI 下界 > 0
// ===========================================================================

/// stage 4 D-481 ④ 字面 vs random 95% CI 下界 > 0：blueprint 统计上必胜 random
/// （mean ± 1.96 × SE 下界 严格 > 0）。
///
/// **断言意图**：避免 mean ≥ +500 但 SE 过大让 CI 下界穿越 0 进入"统计 tie"
/// 区域；D-481 字面要求 strict 95% CI 下界 > 0。1M 手 SE 经验值 ~5-10 mbb/g，
/// 95% CI 半宽 ~10-20 mbb/g；blueprint vs random mean ~ +1000+ mbb/g 让 CI
/// 下界 ~ +980 ≫ 0 是合理预期。
#[test]
#[ignore = "stage4 baseline eval CI; opt-in via `--ignored`"]
fn stage4_baseline_random_ci_95_lower_above_zero() {
    let Some(game) = load_v3_6max_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_6max(game, FIXED_SEED.wrapping_add(18));
    let mut opponent = RandomOpponent;
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(19));

    let result = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint,
        &mut opponent,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[0],
        &mut rng,
    )
    .expect("evaluate_vs_baseline(RandomOpponent) 期望成功");

    let lower = ci_95_lower(result.mean_mbbg, result.standard_error_mbbg);
    eprintln!(
        "[stage4-baseline-random-ci] mean = {:.2} / SE = {:.2} / 95% CI 下界 = {lower:.2}",
        result.mean_mbbg, result.standard_error_mbbg,
    );
    assert!(
        lower > 0.0,
        "D-481 ④：vs random 95% CI 下界 {lower:.2} ≤ 0（mean = {:.2} ± {:.2}，1M 手 SE 过大 \
         让统计上 ties random — F2 [实现] 路径 SE 计算 bug 或者 blueprint 没有显著优势）",
        result.mean_mbbg,
        result.standard_error_mbbg,
    );
}

/// stage 4 D-481 ④ 字面 vs call-station 95% CI 下界 > 0。
#[test]
#[ignore = "stage4 baseline eval CI; opt-in via `--ignored`"]
fn stage4_baseline_call_station_ci_95_lower_above_zero() {
    let Some(game) = load_v3_6max_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_6max(game, FIXED_SEED.wrapping_add(20));
    let mut opponent = CallStationOpponent;
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(21));

    let result = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint,
        &mut opponent,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[0],
        &mut rng,
    )
    .expect("evaluate_vs_baseline(CallStationOpponent) 期望成功");

    let lower = ci_95_lower(result.mean_mbbg, result.standard_error_mbbg);
    eprintln!(
        "[stage4-baseline-call-station-ci] mean = {:.2} / SE = {:.2} / 95% CI 下界 = {lower:.2}",
        result.mean_mbbg, result.standard_error_mbbg,
    );
    assert!(
        lower > 0.0,
        "D-481 ④：vs call_station 95% CI 下界 {lower:.2} ≤ 0",
    );
}

/// stage 4 D-481 ④ 字面 vs TAG 95% CI 下界 > 0。
///
/// **D-489 borderline carve-out**：TAG mean +50 mbb/g ± 20% noise；SE ~5-10
/// mbb/g 让 CI 下界可能落 +30..-10 区间。本测试断言走严格 > 0；borderline
/// 实测后 D-489 carve-out 翻面成 ≥ -10 mbb/g（D-461-revM 同型 first usable
/// 略松路径）。
#[test]
#[ignore = "stage4 baseline eval CI; opt-in via `--ignored`"]
fn stage4_baseline_tag_ci_95_lower_above_zero() {
    let Some(game) = load_v3_6max_or_skip() else {
        return;
    };
    let blueprint = build_blueprint_6max(game, FIXED_SEED.wrapping_add(22));
    let mut opponent = TagOpponent::default();
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_add(23));

    let result = evaluate_vs_baseline::<NlheGame6, _, _>(
        &blueprint,
        &mut opponent,
        D_481_N_HANDS,
        D_481_MASTER_SEEDS[0],
        &mut rng,
    )
    .expect("evaluate_vs_baseline(TagOpponent) 期望成功");

    let lower = ci_95_lower(result.mean_mbbg, result.standard_error_mbbg);
    eprintln!(
        "[stage4-baseline-tag-ci] mean = {:.2} / SE = {:.2} / 95% CI 下界 = {lower:.2}\
         （D-489 borderline carve-out ±20% noise 余地）",
        result.mean_mbbg, result.standard_error_mbbg,
    );
    assert!(
        lower > 0.0,
        "D-481 ④：vs TAG 95% CI 下界 {lower:.2} ≤ 0（D-489 borderline carve-out 余地 ±20% \
         实测后翻面成 ≥ -10）",
    );
}
