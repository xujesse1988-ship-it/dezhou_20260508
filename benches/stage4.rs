//! 阶段 4 D-496 / API-499 — 3 bench group throughput 数据 scaffold。
//!
//! 3 个 bench group（A1 \[实现\] 仅 entry 占位；E1 / F1 \[测试\] 落地）：
//!
//! 1. `stage4/nlhe_6max_es_mccfr_linear_rm_plus_update`（E1 \[测试\] 落地）：
//!    `NlheGame6` + Linear MCCFR + RM+ 单 update throughput。SLO 关联 D-490
//!    （单线程 `≥ 5K update/s` / 4-core `≥ 15K update/s` / 32-vCPU `≥ 20K
//!    update/s`，由 E1 \[测试\] `tests/perf_slo.rs::stage4_*` 严格断言）。
//! 2. `stage4/lbr_compute_1000_hand`（E1 \[测试\] 落地）：LBR computation
//!    1000 hand throughput。SLO 关联 D-454（P95 < 30 s for 1000 hand × 6
//!    traverser）。
//! 3. `stage4/baseline_eval_1000_hand`（F1 \[测试\] 落地）：1M 手 baseline
//!    评测 throughput。SLO 关联 D-485（baseline eval 2 min wall time for
//!    3 baseline × 3 seed）。
//!
//! 本文件不做 SLO 断言，仅产生 throughput 数据。SLO 阈值断言 + 严格 fail/pass
//! 走 E1 / F1 \[测试\] 落地的 `tests/perf_slo.rs::stage4_*`（D-490 / D-454 /
//! D-485）。
//!
//! **A1 \[实现\] 状态**：3 个 bench fn 走 no-op 占位（依赖 `NlheGame6::new` /
//! `LbrEvaluator::compute` / `evaluate_vs_baseline` 全 `unimplemented!()`，
//! bench 实跑会 panic）。`cargo bench --bench stage4` 在 A1 commit 上 build
//! pass + 仅 entry stub 占位，让 E1 / F1 \[测试\] 接入时 0 改动 Cargo.toml。
//!
//! 角色边界：本文件属 `[测试]` agent。`[实现]` agent 不得修改。

use criterion::{criterion_group, criterion_main, Criterion};

fn bench_nlhe_6max_es_mccfr_linear_rm_plus_update(c: &mut Criterion) {
    // E1 \[测试\] 起步前 lock：构造 NlheGame6 + Linear+RM+ trainer + 跑单 update
    // throughput；artifact 缺失时走 no-op + eprintln 提示（继承 stage 3 stage3/
    // nlhe_es_mccfr_update 同型 fallback）。
    let _ = c;
    eprintln!(
        "stage4/nlhe_6max_es_mccfr_linear_rm_plus_update: A1 [实现] scaffold no-op; \
         E1 [测试] 起步前 lock 具体 bench fn"
    );
}

fn bench_lbr_compute_1000_hand(c: &mut Criterion) {
    // E1 \[测试\] 起步前 lock：`LbrEvaluator::compute` 1000 hand × 1 traverser
    // throughput；依赖 first usable checkpoint 存在时跑实测，缺失时 no-op。
    let _ = c;
    eprintln!("stage4/lbr_compute_1000_hand: A1 [实现] scaffold no-op; E1 [测试] 起步前 lock");
}

fn bench_baseline_eval_1000_hand(c: &mut Criterion) {
    // F1 \[测试\] 起步前 lock：3 baseline × 1000 hand throughput
    // （`RandomOpponent` / `CallStationOpponent` / `TagOpponent`）。
    let _ = c;
    eprintln!("stage4/baseline_eval_1000_hand: A1 [实现] scaffold no-op; F1 [测试] 起步前 lock");
}

criterion_group!(
    stage4_bench,
    bench_nlhe_6max_es_mccfr_linear_rm_plus_update,
    bench_lbr_compute_1000_hand,
    bench_baseline_eval_1000_hand,
);
criterion_main!(stage4_bench);
