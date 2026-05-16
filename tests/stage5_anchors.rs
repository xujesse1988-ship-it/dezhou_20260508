//! 阶段 5 B1 \[测试\] — D-560..D-563 4 anchor 实测覆盖 integration crate
//! （API-597 / D-560..D-563 / D-564..D-569 字面 measurement protocol）。
//!
//! ## 角色边界
//!
//! 本文件属 `[测试]` agent；B1 [测试] 0 改动产品代码。F1 \[测试\] / F3
//! \[报告\] 在 c6a.8xlarge 32-vCPU host 实测 4 anchor 全 PASS 后转 pass。
//!
//! ## D-560..D-563 4 anchor 字面
//!
//! - **D-560** — LBR 6-traverser average 不退化（优化后 ≤ 优化前 × 1.05）。
//!   stage 4 first usable baseline 56,231 mbb/g → stage 5 优化后 1B update wall
//!   等量 ≤ 59,000 mbb/g。**测试 host** = c6a.8xlarge 32-vCPU 上单独跑
//!   `lbr_compute --six-traverser`。
//!
//! - **D-561** — baseline 3 类 mean 不退化（Random ≥ baseline × 0.9 / CallStation
//!   ≥ baseline × 0.8 / TAG mean delta ≤ ±100 mbb/g）。stage 4 baseline:
//!   Random +1657 → ≥ 1491; CallStation +98 → ≥ 78; TAG -267 → [-367, -167]。
//!
//! - **D-562** — Slumbot mean 95% CI overlap（regression guard，stack-size
//!   mismatch 已知偏离 stage 5 不修；stage 5 上界 ≥ stage 4 下界 = -1918 即
//!   PASS）。
//!
//! - **D-563** — Checkpoint round-trip BLAKE3 self-consistency（同 binary 写
//!   + 读 + 重写 byte-equal；详 `tests/checkpoint_v3_round_trip.rs`）。
//!
//! ## D-566 字面 baseline 持久化
//!
//! `tests/data/stage5_anchor_baseline.json` 内含字段：
//! - `lbr_six_traverser_mean: 56231`（mbb/g）
//! - `baseline_random_mean: 1657`
//! - `baseline_callstation_mean: 98`
//! - `baseline_tag_mean: -267`
//! - `slumbot_mean: -1110.92`
//! - `slumbot_ci_lower: -1918.37`
//! - `slumbot_ci_upper: -303.47`
//! - `checkpoint_blake3: 388e8d84...`（stage 4 first usable artifact）
//!
//! BLAKE3 锚定走 `tests/data/stage5_anchor_baseline.blake3`（D-548 / D-566 字面）。
//!
//! ## D-567 字面 4 anchor 合成判定
//!
//! - 全 PASS = SLO PASS
//! - 任一 anchor fail = D-565 retry（同 seed 1 次）；连 2 fail = 真实 fail →
//!   D-550-revM 触发（pruning 阈值或 ε resurface 调整重测）
//! - **禁止** 部分 PASS 部分 carve-out 后继续推进

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// 共享常量（D-566 baseline reference 字面，对应 stage 4 first usable 1B
// 实测数字 = `pluribus_stage4_report.md` §F3 字面）
// ---------------------------------------------------------------------------

/// D-566 字面 — stage 4 first usable LBR 6-traverser baseline。
const STAGE4_BASELINE_LBR_SIX_TRAVERSER_MEAN: f64 = 56_231.0;

/// D-560 字面 — stage 5 LBR 6-traverser anchor 阈值（baseline × 1.05）。
const STAGE5_ANCHOR_LBR_THRESHOLD: f64 = 59_042.55; // 56_231 × 1.05

/// D-566 字面 baseline — baseline_random_mean。
const STAGE4_BASELINE_RANDOM_MEAN: f64 = 1657.0;

/// D-561 字面 — Random 阈值 = baseline × 0.9。
const STAGE5_ANCHOR_RANDOM_THRESHOLD: f64 = 1491.3; // 1657 × 0.9

/// D-566 字面 baseline — baseline_callstation_mean。
const STAGE4_BASELINE_CALLSTATION_MEAN: f64 = 98.0;

/// D-561 字面 — CallStation 阈值 = baseline × 0.8。
const STAGE5_ANCHOR_CALLSTATION_THRESHOLD: f64 = 78.4; // 98 × 0.8

/// D-566 字面 baseline — baseline_tag_mean。
const STAGE4_BASELINE_TAG_MEAN: f64 = -267.0;

/// D-561 字面 — TAG delta tolerance = ±100 mbb/g。
const STAGE5_ANCHOR_TAG_DELTA: f64 = 100.0;

/// D-566 字面 baseline — slumbot_mean / ci_lower / ci_upper。
const STAGE4_BASELINE_SLUMBOT_MEAN: f64 = -1110.92;
const STAGE4_BASELINE_SLUMBOT_CI_LOWER: f64 = -1918.37;
const _STAGE4_BASELINE_SLUMBOT_CI_UPPER: f64 = -303.47;

/// stage 4 first usable checkpoint SHA256（CLAUDE.md ground truth）。
const STAGE4_FIRST_USABLE_CHECKPOINT_SHA256: &str =
    "388e8d841fa30bf3757cc974b685c2594fc9cc641de7ea207f2f3f28755936e7";

/// D-548 / D-566 字面 — baseline 持久化 JSON 路径。
const STAGE5_ANCHOR_BASELINE_JSON_PATH: &str = "tests/data/stage5_anchor_baseline.json";

// ---------------------------------------------------------------------------
// Group A — 常量字面 lock（active；A1 scaffold 即生效）
// ---------------------------------------------------------------------------

/// D-560 字面 — LBR threshold 计算正确（baseline × 1.05）。
#[test]
fn d560_lbr_threshold_is_baseline_times_1_05() {
    let calculated = STAGE4_BASELINE_LBR_SIX_TRAVERSER_MEAN * 1.05;
    assert!(
        (calculated - STAGE5_ANCHOR_LBR_THRESHOLD).abs() < 0.01,
        "D-560 阈值 {STAGE5_ANCHOR_LBR_THRESHOLD} ≠ baseline × 1.05 = {calculated}"
    );
}

/// D-561 字面 — baseline 3 类阈值锁。
#[test]
fn d561_baseline_thresholds_match_d561_literal() {
    assert!(
        (STAGE5_ANCHOR_RANDOM_THRESHOLD - STAGE4_BASELINE_RANDOM_MEAN * 0.9).abs() < 0.1,
        "D-561 字面 Random 阈值 = baseline × 0.9"
    );
    assert!(
        (STAGE5_ANCHOR_CALLSTATION_THRESHOLD - STAGE4_BASELINE_CALLSTATION_MEAN * 0.8).abs() < 0.1,
        "D-561 字面 CallStation 阈值 = baseline × 0.8"
    );
    assert_eq!(
        STAGE5_ANCHOR_TAG_DELTA, 100.0,
        "D-561 字面 TAG delta 容差 ±100 mbb/g"
    );
}

/// D-562 字面 — Slumbot 95% CI lower bound 锁定（stage 5 上界 ≥ -1918 即 PASS）。
///
/// `const _` anonymous block 让 const-eval 直接捕获越界（继承 stage 4 D-449
/// 测试同型模式），clippy `assertions_on_constants` 不触发。
#[test]
fn d562_slumbot_ci_lower_bound_locked() {
    // D-562 字面：stage 5 95% CI 上界 ≥ stage 4 下界 = -1918.37 即 overlap PASS。
    const _: () = {
        assert!(STAGE4_BASELINE_SLUMBOT_CI_LOWER < STAGE4_BASELINE_SLUMBOT_MEAN);
    };
    // Runtime sanity 让 `cargo test` 输出含本测试。
    let lower = STAGE4_BASELINE_SLUMBOT_CI_LOWER;
    let mean = STAGE4_BASELINE_SLUMBOT_MEAN;
    eprintln!("Slumbot CI lower {lower} < mean {mean} (D-562 字面 baseline)");
}

/// D-566 字面 — `tests/data/stage5_anchor_baseline.json` 持久化路径 lock。
///
/// **B1 [测试] scope**：D2 [实现] / F3 [报告] 起步前 ship 该 JSON 文件；本测
/// 试在 A1 scaffold 阶段允许文件不存在（路径常量 lock 即可）。
#[test]
fn d566_anchor_baseline_json_path_locked() {
    let path = PathBuf::from(STAGE5_ANCHOR_BASELINE_JSON_PATH);
    // 路径常量 sanity（不要求文件存在；F3 [报告] 起步前 ship）。
    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        "stage5_anchor_baseline.json"
    );
}

/// D-566 字面 — stage 4 first usable checkpoint SHA256 锁定（与 CLAUDE.md
/// ground truth 同 hex string 字面）。
#[test]
fn d566_stage4_first_usable_checkpoint_sha256_locked() {
    assert_eq!(STAGE4_FIRST_USABLE_CHECKPOINT_SHA256.len(), 64);
    // 仅做 hex 字符 sanity。
    assert!(
        STAGE4_FIRST_USABLE_CHECKPOINT_SHA256
            .chars()
            .all(|c| c.is_ascii_hexdigit()),
        "checkpoint SHA256 应为 hex string"
    );
}

// ---------------------------------------------------------------------------
// Group B — 4 anchor opt-in 实测（F1 [测试] / F3 [报告] c6a host run 转 pass）
// ---------------------------------------------------------------------------

/// D-560 字面 anchor #1 — LBR 6-traverser average 不退化（≤ 优化前 × 1.05）。
///
/// **B1 [测试] 状态**：F1 [测试] / F3 [报告] c6a.8xlarge 32-vCPU host run
/// `lbr_compute --six-traverser --checkpoint <final.ckpt> --bucket-table
/// <v3.bin> --n-hands 1000` 实测后 opt-in 转 pass。
///
/// **D-564 字面 measurement protocol** — 在同 c6a host run 内连续完成 4
/// anchor 测量（LBR ~10s × 6 = 1min）。
#[test]
#[ignore = "B1 scaffold; F1 [测试] / F3 [报告] c6a host LBR 实测后 opt-in 转 pass"]
fn anchor_d560_lbr_six_traverser_average_below_59000_mbb_g() {
    panic!(
        "D-560 字面 anchor #1 — LBR 6-traverser ≤ {STAGE5_ANCHOR_LBR_THRESHOLD} mbb/g \
         (baseline {STAGE4_BASELINE_LBR_SIX_TRAVERSER_MEAN} × 1.05)。\
         F1 [测试] / F3 [报告] c6a.8xlarge host 跑 lbr_compute --six-traverser 实测后 \
         移除 #[ignore] 转 pass。\
         carve-out 路径：实测 > 59,000 触发 D-565 同 seed 重测 1 次 + 连 2 fail 真实 fail \
         → D-550-revM (pruning 阈值或 ε resurface 调整重测)。"
    );
}

/// D-561 字面 anchor #2 — baseline 3 类 mean 不退化。
///
/// `eval_blueprint --baseline-hands 1000000 --opponent random,callstation,tag`
/// 串行 3 类（stage 4 既有 binary 不改）；D-564 measurement protocol 同 c6a run
/// 内 ~10min。
#[test]
#[ignore = "B1 scaffold; F1 [测试] / F3 [报告] c6a host baseline 实测后 opt-in 转 pass"]
fn anchor_d561_baseline_three_opponents_mean_thresholds() {
    panic!(
        "D-561 字面 anchor #2 — baseline 3 类阈值：\
         Random ≥ {STAGE5_ANCHOR_RANDOM_THRESHOLD} (baseline {STAGE4_BASELINE_RANDOM_MEAN}); \
         CallStation ≥ {STAGE5_ANCHOR_CALLSTATION_THRESHOLD} (baseline {STAGE4_BASELINE_CALLSTATION_MEAN}); \
         TAG delta ≤ ±{STAGE5_ANCHOR_TAG_DELTA} mbb/g (baseline {STAGE4_BASELINE_TAG_MEAN})。\
         F1 [测试] / F3 [报告] c6a host eval_blueprint 实测后 opt-in 转 pass。"
    );
}

/// D-562 字面 anchor #3 — Slumbot mean 95% CI overlap（regression guard）。
///
/// `eval_blueprint --slumbot-hands 10000 --master-seed 42`（继承 stage 4
/// §F3-rev2 10K 字面规模）；wall ~7min in D-564 measurement protocol。
///
/// **D-567 字面例外**：D-562 仅作 regression guard，stage 5 上升或持平即 PASS
/// （stack-size mismatch 已知偏离不修，但不允许变更差）。
#[test]
#[ignore = "B1 scaffold; F1 [测试] / F3 [报告] c6a host Slumbot 实测后 opt-in 转 pass"]
fn anchor_d562_slumbot_95ci_overlap_with_stage4_baseline() {
    panic!(
        "D-562 字面 anchor #3 — Slumbot 95% CI overlap：stage 5 95% CI 上界 ≥ \
         stage 4 CI 下界 {STAGE4_BASELINE_SLUMBOT_CI_LOWER} 即 PASS。\
         baseline mean = {STAGE4_BASELINE_SLUMBOT_MEAN}（stack-size mismatch 已知偏离）。\
         F1 [测试] / F3 [报告] c6a host 实测后 opt-in 转 pass。"
    );
}

/// D-563 字面 anchor #4 — Checkpoint round-trip BLAKE3 self-consistency
/// （同 binary 写 + 读 + 重写 byte-equal；schema=3 路径内部自洽）。
///
/// **D-564 measurement protocol**：`cargo test --release --test
/// checkpoint_v3_round_trip -- --ignored` 自动 BLAKE3 self-consistency。
/// 实测路径详 `tests/checkpoint_v3_round_trip.rs::
/// checkpoint_v3_save_then_open_byte_equal` 与 `checkpoint_v3_body_blake3_self_consistency`。
/// 本 file 仅 trip-wire 让 stage5_anchors 4 anchor 集合完备。
#[test]
#[ignore = "B1 scaffold; D2 [实现] 落地 Checkpoint v3 round-trip 后转 pass（详 \
            tests/checkpoint_v3_round_trip.rs）"]
fn anchor_d563_checkpoint_v3_round_trip_blake3_self_consistency() {
    panic!(
        "D-563 字面 anchor #4 — Checkpoint v3 round-trip BLAKE3 self-consistency。\
         实测路径走 `tests/checkpoint_v3_round_trip.rs::checkpoint_v3_save_then_open_byte_equal` \
         + `checkpoint_v3_body_blake3_self_consistency`。本 file trip-wire 让 4 anchor 集合完备。"
    );
}

// ---------------------------------------------------------------------------
// Group C — D-567 合成判定（all-pass-required）
// ---------------------------------------------------------------------------

/// D-567 字面 — 4 anchor 合成判定：全 PASS = SLO PASS；任一 anchor fail =
/// D-565 retry；连 2 fail = 真实 fail → D-550-revM 触发。
///
/// **B1 [测试] scope**：本测试 trip-wire 让 D-567 字面合成协议在 F1 / F3 实
/// 测时显式跑过（保证 4 anchor 全 PASS 才算 SLO PASS，禁部分 PASS 部分 carve-out
/// 继续推进）。E2 [实现] / F1 [测试] 落地后由 c6a host runner 显式串行调用
/// 上面 4 个 `anchor_d56X_*` 测试。
#[test]
#[ignore = "B1 scaffold; F1 [测试] / F3 [报告] c6a host 4 anchor 全 PASS 后转 pass"]
fn d567_all_four_anchors_pass_synthesis() {
    panic!(
        "D-567 字面 4 anchor 合成判定 — F1 [测试] / F3 [报告] c6a host run 内 4 \
         anchor 全 PASS 后转 pass。**禁止** 部分 PASS 部分 carve-out 继续推进。"
    );
}
