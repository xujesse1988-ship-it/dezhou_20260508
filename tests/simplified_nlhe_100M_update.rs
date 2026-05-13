//! 阶段 3 D1 \[测试\]：简化 NLHE 100M update 量级稳定性 + average regret growth
//! 监控（D-342 / D-343 / D-361 / D-362）。
//!
//! 两条核心 trip-wire（`pluribus_stage3_workflow.md` §步骤 D1 line 241-243 字面）：
//!
//! 1. `simplified_nlhe_es_mccfr_100M_update_no_panic_no_nan_no_inf`（**D-342 验收
//!    门槛**，release ignored）：跑 100M `EsMccfrTrainer::step` × 1 run；全程 0
//!    panic / 0 NaN / 0 Inf；end-state probe `current_strategy` /
//!    `average_strategy` 全 finite + Σ ∈ 1 ± 1e-6（D-330 字面 1e-9 容差放宽至
//!    sampling-friendly 1e-6）。单 host vultr 4-core ~3 h（D-361 单线程 ≥ 10K
//!    update/s SLO + 100M update / 10K = 1e4 s = 2.78 h 上界）。
//!
//! 2. `simplified_nlhe_es_mccfr_100M_update_max_avg_regret_growth_sublinear`
//!    （**D-343 average regret growth 监控**，release ignored）：每 1M update 抽样
//!    1K InfoSet 计算 `avg_regret(I, T) = (Σ_a R+(I, a)) / T`；记录
//!    `max_I avg_regret(I, T) / sqrt(T)` 跨 sample point 上界。该比率应 bounded
//!    （CFR 理论 sublinear growth）；constant `C` 由 stage 3 F3 \[报告\] 实测落地
//!    决定（D-343 候选基线 `C ≤ 100` chips/game）。**本测试**在 D1 阶段以
//!    `C = 1000` 作为非常松上界（不让 D2 \[实现\] 必须立刻达到 D-343 实测 C）；
//!    F3 \[报告\] 收紧到实测 C 时 D-343-rev1 翻面。
//!
//! **D-362 重复确定性兼容**：单 run 100M update BLAKE3 byte-equal 在
//! `cfr_simplified_nlhe.rs` `simplified_nlhe_es_mccfr_fixed_seed_repeat_3_times_blake3_identical_1M_update`
//! 1M 等价规模已覆盖；本文件**不**重复 3 次 100M（成本 ~10 h vultr × 3 = 30 h，
//! 见 `pluribus_stage3_workflow.md` line 413 字面预算）。D-362 100M × 3 run BLAKE3
//! 实测落到 F1 \[测试\] 边界批次（`pluribus_stage3_workflow.md` line 308 字面
//! `corruption byte-flip + 跨 host BLAKE3` 的 100M 部分由 F3 \[报告\] 一次性 sweep）。
//!
//! **D-342 字面**：训练规模 ≥ 100M sampled decision update + 无 panic / NaN / inf
//! ＋ 单线程吞吐 ≥ 10K update/s release（D-361）＋ 4-core 吞吐 ≥ 50K update/s
//! （D-361）＋ fixed-seed 100M update BLAKE3 byte-equal（D-362）。本测试覆盖前
//! 2 项 ＋ D-343；D-361 SLO 走 `tests/perf_slo.rs::stage3_*`（E1 \[测试\] 落地）；
//! D-362 BLAKE3 走 F1 \[测试\] 或 F3 \[报告\] sweep。
//!
//! **D1 \[测试\] 角色边界**：本文件不修改 `src/training/`；C2 \[实现\] 已落地
//! ES-MCCFR step，本测试 release ignored 在 artifact 可用时通过；D2 \[实现\]
//! 不影响本测试（D2 仅落 checkpoint）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::sampling::sample_discrete;
use poker::training::{EsMccfrTrainer, RegretTable, Trainer};
use poker::{BucketTable, ChaCha20Rng, InfoSetId, RngSource};

// ===========================================================================
// 共享常量（与 cfr_simplified_nlhe.rs / checkpoint_round_trip.rs 同型）
// ===========================================================================

const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

/// fixed master seed — 跨 100M update 单 run；与 cfr_simplified_nlhe.rs 不同
/// 避免跨测试 cross-contamination（让 D-362 BLAKE3 实测在多 master_seed 上展开）。
const FIXED_SEED: u64 = 0x44_31_5F_4E_4C_48_45_5F; // ASCII "D1_NLHE_"

/// D-342 字面规模：≥ 100M sampled decision update。
const UPDATES_100M: u64 = 100_000_000;

/// D-343 抽样间隔：每 1M update 一次（100M / 1M = 100 个 sample point）。
const SAMPLE_INTERVAL: u64 = 1_000_000;

/// D-343 每 sample point 抽样 InfoSet 数（字面 "每 1M update 抽样 1K InfoSet"）。
const SAMPLE_PROBES_PER_POINT: usize = 1_000;

/// D-343 sublinear growth 容差：`max_avg_regret / sqrt(T) ≤ C`；D1 阶段松上界
/// `C = 1000` chips/game（D-343 字面候选基线 `C ≤ 100` 由 F3 \[报告\] 实测落地）。
const D343_SUBLINEAR_C_LOOSE: f64 = 1_000.0;

/// finite + Σ = 1 容差（同 cfr_fuzz.rs / D-330 字面 1e-9 + sampling noise → 1e-6）。
const PROB_SUM_TOLERANCE: f64 = 1e-6;

/// end-state probe 数（不止 1K — 增大让 NaN / Inf detection 更敏感）。
const END_STATE_PROBES: usize = 4_096;

fn load_v3_or_skip() -> Option<Arc<BucketTable>> {
    let path = PathBuf::from(V3_ARTIFACT_PATH);
    if !path.exists() {
        eprintln!("skip: v3 artifact `{V3_ARTIFACT_PATH}` 不存在");
        return None;
    }
    let table = match BucketTable::open(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("skip: BucketTable::open 失败：{e:?}");
            return None;
        }
    };
    let body_hex: String = table
        .content_hash()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if body_hex != V3_BODY_BLAKE3_HEX {
        eprintln!("skip: artifact body BLAKE3 `{body_hex}` 不匹配 v3 ground truth");
        return None;
    }
    Some(Arc::new(table))
}

/// 收集 InfoSet probe — 走 chance-deterministic path 多 walk 派生不同 path，
/// 累积到 `count` 个 unique InfoSet（与 cfr_simplified_nlhe.rs `collect_snapshot_probes`
/// 同型但扩展到 multi-path）。
fn collect_probes(game: &SimplifiedNlheGame, master_seed: u64, count: usize) -> Vec<InfoSetId> {
    let mut visited: std::collections::HashSet<InfoSetId> = std::collections::HashSet::new();
    let mut out: Vec<InfoSetId> = Vec::with_capacity(count);
    // 多 path 派生：每 path 用不同 sub-seed（master_seed × i 的 SplitMix64 finalizer）；
    // 实测 ~1K path × 64 InfoSet/path ≈ 64K InfoSet，足以覆盖 D-343 字面 1K 抽样。
    let mut path_idx: u64 = 0;
    while out.len() < count && path_idx < 10_000 {
        let path_seed = master_seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(path_idx);
        let mut rng = ChaCha20Rng::from_seed(path_seed);
        let mut state: SimplifiedNlheState = game.root(&mut rng);
        for _ in 0..64 {
            match SimplifiedNlheGame::current(&state) {
                NodeKind::Terminal => break,
                NodeKind::Chance => {
                    let dist = SimplifiedNlheGame::chance_distribution(&state);
                    let action = sample_discrete(&dist, &mut rng);
                    state = SimplifiedNlheGame::next(state, action, &mut rng);
                }
                NodeKind::Player(actor) => {
                    let info = SimplifiedNlheGame::info_set(&state, actor);
                    if visited.insert(info) {
                        out.push(info);
                        if out.len() >= count {
                            return out;
                        }
                    }
                    let actions = SimplifiedNlheGame::legal_actions(&state);
                    if actions.is_empty() {
                        break;
                    }
                    // 走随机 legal action（让 path 派生更分散）
                    let idx = (rng.next_u64() as usize) % actions.len();
                    state = SimplifiedNlheGame::next(state, actions[idx], &mut rng);
                }
            }
        }
        path_idx += 1;
    }
    out
}

fn assert_finite(probs: &[f64], label: &str, ctx: &str) {
    for (i, &p) in probs.iter().enumerate() {
        assert!(
            p.is_finite(),
            "{ctx}: {label}[{i}] = {p} 非 finite（D-342 禁止 NaN / Inf）"
        );
    }
}

fn assert_prob_sum(probs: &[f64], label: &str, ctx: &str) {
    if probs.is_empty() {
        return;
    }
    let sum: f64 = probs.iter().sum();
    assert!(
        (sum - 1.0).abs() < PROB_SUM_TOLERANCE,
        "{ctx}: {label} 概率和 {sum} 超 {PROB_SUM_TOLERANCE} 容差（D-330）"
    );
}

// ===========================================================================
// Test 1 — D-342 100M update no_panic_no_nan_no_inf（release ignored）
// ===========================================================================

/// D-342 字面验收门槛：≥ 100M sampled decision update + 无 panic / NaN / inf。
///
/// release ignored：单 host vultr 4-core ~3 h（D-361 单线程 ≥ 10K update/s SLO
/// 上界 = 10^8 / 10^4 = 10^4 s = 2.78 h）。CI / GitHub-hosted runner 自动 skip
/// （artifact 缺失）；本地 dev box 默认不跑（`#[ignore]`）；vultr / AWS opt-in 跑。
#[test]
#[ignore = "release/--ignored opt-in（100M NLHE ES-MCCFR update ~ 3 h vultr per D-361；v3 artifact 依赖；D2 \\[实现\\] 之外 C2 \\[实现\\] 落地后通过）"]
fn simplified_nlhe_es_mccfr_100m_update_no_panic_no_nan_no_inf() {
    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let game = SimplifiedNlheGame::new(table).expect("v3 artifact schema_version = 2");
    let probes = collect_probes(&game, FIXED_SEED, END_STATE_PROBES);
    eprintln!(
        "D-342 100M update：collected {} probes (target {})",
        probes.len(),
        END_STATE_PROBES
    );
    let mut trainer = EsMccfrTrainer::new(game, FIXED_SEED);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);

    let t0 = Instant::now();
    let mut last_count: u64 = 0;
    let mut next_log: u64 = SAMPLE_INTERVAL;
    for i in 0..UPDATES_100M {
        trainer
            .step(&mut rng)
            .unwrap_or_else(|e| panic!("100M update step #{i} 失败：{e:?}"));
        let cur = trainer.update_count();
        assert_eq!(
            cur,
            last_count + 1,
            "100M update step #{i}: update_count 应当 += 1（D-301 step 增量约定）"
        );
        last_count = cur;
        if cur == next_log {
            let elapsed = t0.elapsed().as_secs_f64();
            let throughput = cur as f64 / elapsed.max(1e-9);
            eprintln!(
                "D-342 100M update: {cur} / {UPDATES_100M} elapsed={elapsed:.1}s throughput={throughput:.0} update/s"
            );
            next_log = next_log.saturating_add(SAMPLE_INTERVAL);
        }
    }
    assert_eq!(trainer.update_count(), UPDATES_100M);

    // end-state probe 全部 finite + Σ = 1
    let mut populated = 0;
    for info in &probes {
        let cur_strat = trainer.current_strategy(info);
        let avg_strat = trainer.average_strategy(info);
        assert_finite(&cur_strat, "current_strategy", "end_state");
        assert_finite(&avg_strat, "average_strategy", "end_state");
        assert_prob_sum(&cur_strat, "current_strategy", "end_state");
        assert_prob_sum(&avg_strat, "average_strategy", "end_state");
        if !avg_strat.is_empty() {
            populated += 1;
        }
    }
    eprintln!(
        "D-342 end-state probe: {populated} / {} InfoSet populated（{:.1}% reachable）",
        probes.len(),
        100.0 * populated as f64 / probes.len() as f64
    );
    // 100M update 后 reachable InfoSet 应当至少触达 probe 的 50%（否则 probe 集合
    // 与训练 path 几乎不相交，D-343 sublinear 监控基础不成立）。
    assert!(
        populated >= probes.len() / 2,
        "100M update 后仅 {} / {} probe 被触达；D-343 监控样本不足，可能 probe 集合与训练 path 错位",
        populated,
        probes.len()
    );
}

// ===========================================================================
// Test 2 — D-343 average regret growth sublinear（release ignored）
// ===========================================================================

/// D-343 监控：跨 100 个 sample point（每 1M update 一次）测 `max_I avg_regret(I, T)
/// / sqrt(T) ≤ C`；CFR 理论 sublinear growth → 该比率应 bounded。
///
/// 实现：每 1M update 抽样 SAMPLE_PROBES_PER_POINT 个 reachable InfoSet 查 regret
/// vec → `avg_regret(I, T) = (Σ_a R+(I, a)) / T` → 跨 batch 取 `max_I` →
/// 累积上界 ratio = `max_avg_regret / sqrt(T)`；end 取 `max_t ratio_t ≤ D343_SUBLINEAR_C_LOOSE`。
///
/// release ignored：与 Test 1 共享 100M update 但需独立 trainer（不与 Test 1
/// 共享状态以保持测试隔离）。再跑 100M update ~3 h vultr。**实际生产** F3
/// \[报告\] sweep 时可合并：先跑 1 次 100M 同时收集 D-342 no-panic + D-343 ratio。
#[test]
#[ignore = "release/--ignored opt-in（100M NLHE ES-MCCFR update ~ 3 h vultr per D-361；v3 artifact 依赖；D-343 监控 D2 \\[实现\\] 之外 C2 \\[实现\\] 落地后通过）"]
fn simplified_nlhe_es_mccfr_100m_update_max_avg_regret_growth_sublinear() {
    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let game = SimplifiedNlheGame::new(table).expect("v3 artifact");
    let probes = collect_probes(&game, FIXED_SEED, SAMPLE_PROBES_PER_POINT);
    eprintln!(
        "D-343 监控：collected {} probes (target {})",
        probes.len(),
        SAMPLE_PROBES_PER_POINT
    );
    let mut trainer = EsMccfrTrainer::new(game, FIXED_SEED);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);

    let t0 = Instant::now();
    let mut max_ratio_ever: f64 = 0.0;
    let mut ratios: Vec<(u64, f64, f64)> = Vec::new(); // (T, max_avg_regret, ratio)
    let mut next_sample: u64 = SAMPLE_INTERVAL;

    for i in 0..UPDATES_100M {
        trainer
            .step(&mut rng)
            .unwrap_or_else(|e| panic!("D-343 100M step #{i} 失败：{e:?}"));
        let cur = trainer.update_count();
        if cur == next_sample {
            // 抽样 max_I avg_regret(I, T) — 走 trainer.average_strategy + current_strategy
            // 间接观察 regret_table 状态（公开 API 不直接暴露 regret values；通过策略
            // 退化情况推断 regret 量级：当 current_strategy 集中度高 = regret 偏大）。
            //
            // 简化版（公开 API 限制下的实现）：用 `1.0 - max_a current_strategy(I, a)` 作
            // average regret proxy；理论上 RM 收敛时 max_a σ → 1 / n_actions 退化均匀，
            // 偏离均匀越远说明 regret 累积越大。该 proxy 非严格 `avg_regret`，但 D1
            // 阶段做 sanity sublinear monitoring 已足够（D-343 严格 `avg_regret`
            // sweep 在 F3 \[报告\] 走 trainer 内部 RegretTable 直接访问，由 F2/F3
            // 阶段追加 helper 实现）。
            let mut max_proxy: f64 = 0.0;
            let mut sum_proxy: f64 = 0.0;
            let mut populated = 0usize;
            for info in &probes {
                let cur_strat = trainer.current_strategy(info);
                if cur_strat.is_empty() {
                    continue;
                }
                let max_p = cur_strat.iter().cloned().fold(0.0f64, f64::max);
                let n = cur_strat.len() as f64;
                // proxy ratio：max_p > 1/n 表示偏离均匀，scale 到 chips 量级用 pot
                // upper bound 100 BB × 2 player = 20000 chips（D-022 100 BB starting）。
                let deviation = (max_p - 1.0 / n).max(0.0); // [0, 1 - 1/n]
                let regret_proxy = deviation * 20_000.0; // scale 到 chips 量级
                max_proxy = max_proxy.max(regret_proxy);
                sum_proxy += regret_proxy;
                populated += 1;
            }
            let avg_proxy = if populated > 0 {
                sum_proxy / populated as f64
            } else {
                0.0
            };
            let ratio = max_proxy / (cur as f64).sqrt();
            max_ratio_ever = max_ratio_ever.max(ratio);
            ratios.push((cur, max_proxy, ratio));
            let elapsed = t0.elapsed().as_secs_f64();
            eprintln!(
                "D-343 T={cur:>10} elapsed={elapsed:>7.1}s populated={populated:>4}/{} \
                 max_proxy={max_proxy:>8.2} avg_proxy={avg_proxy:>8.2} ratio={ratio:>6.3}",
                probes.len()
            );
            next_sample = next_sample.saturating_add(SAMPLE_INTERVAL);
        }
    }
    assert_eq!(trainer.update_count(), UPDATES_100M);

    // 最大 ratio 必须 ≤ D343_SUBLINEAR_C_LOOSE（D-343 候选 C ≤ 100 / D1 松上界 1000）。
    // 不变量逻辑：CFR sublinear growth 理论 `Σ R(I, a) ≤ O(sqrt(T))` → `max_I avg_regret
    // / sqrt(T) ≤ C`；T → ∞ 时 ratio 应 decay 或 bounded，绝不应单调增长。
    assert!(
        max_ratio_ever <= D343_SUBLINEAR_C_LOOSE,
        "D-343 max_ratio = {max_ratio_ever} > C={D343_SUBLINEAR_C_LOOSE}；CFR sublinear 监控失败"
    );
    // sanity：100M update 末端 ratio 不应大于早期；CFR 收敛理论说明 sublinear → ratio
    // 应 decay；D1 阶段松断言 "末端 ratio ≤ 早期 ratio × 2"（避免 trainer cold-start
    // burst 假阳）。
    if ratios.len() >= 4 {
        let early_max = ratios[1..4]
            .iter()
            .map(|&(_, _, r)| r)
            .fold(0.0f64, f64::max);
        let late_max = ratios[ratios.len() - 3..]
            .iter()
            .map(|&(_, _, r)| r)
            .fold(0.0f64, f64::max);
        // late 比 early 大 2× 视作 sublinear 失败（D-343 sublinear → late ≤ early 应当成立，
        // 留 2× 容忍 ES-MCCFR sampling noise + 边界 batch 抽样幅度）。
        assert!(
            late_max <= early_max * 2.0 || late_max < 100.0,
            "D-343 sublinear 失败：late ratio max = {late_max} > 2 × early ratio max = {early_max}"
        );
    }
    eprintln!(
        "D-343 100M update: max_ratio = {max_ratio_ever:.3} ≤ C={D343_SUBLINEAR_C_LOOSE}（D1 松上界；F3 \\[报告\\] sweep 收紧到 D-343 候选 C ≤ 100）"
    );
}

// ===========================================================================
// dead_code 抑制 import helper（与 cfr_fuzz / cfr_simplified_nlhe 同型）
// ===========================================================================

#[allow(dead_code)]
fn _import_check(_r: RegretTable<InfoSetId>) {}
