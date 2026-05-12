//! 阶段 3 B1 \[测试\]：CFR / MCCFR 训练 throughput benchmark framework
//! （D-367 criterion bench）。
//!
//! 落地 2 个 bench group（C1 \[测试\] 补充第 3 个 `stage3/nlhe_es_mccfr_update`）：
//!
//! 1. `stage3/kuhn_cfr_iter`：Kuhn Vanilla CFR 单 step throughput。SLO 关联
//!    D-360（10K iter `< 1 s` release → 单 step `< 100 µs`，目标 throughput
//!    `≥ 10K iter/s`）。
//! 2. `stage3/leduc_cfr_iter`：Leduc Vanilla CFR 单 step throughput。SLO 关联
//!    D-360（10K iter `< 60 s` release → 单 step `< 6 ms`，目标 throughput
//!    `≥ 167 iter/s`）。
//!
//! 本文件不做 SLO 断言，仅产生 throughput 数据。SLO 阈值断言 + 严格 fail/pass
//! 走 E1 \[测试\] 落地的 `tests/perf_slo.rs::stage3_*`（D-369）。
//!
//! 当前 A1 scaffold 阶段 [`VanillaCfrTrainer::step`] `unimplemented!()`，本
//! benchmark 跑时会 panic（与 B1 `tests/cfr_*` active 测试同形态）；B2 \[实现\]
//! 落地后该 panic 转为正常 throughput 数据。
//!
//! 角色边界：本文件属 `[测试]` agent。`[实现]` agent 不得修改。

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use poker::training::kuhn::KuhnGame;
use poker::training::leduc::LeducGame;
use poker::training::{Trainer, VanillaCfrTrainer};
use poker::{ChaCha20Rng, RngSource};

// ============================================================================
// stage3/kuhn_cfr_iter — Kuhn Vanilla CFR 单 step throughput
// ============================================================================

fn bench_kuhn_cfr_iter(c: &mut Criterion) {
    let mut group = c.benchmark_group("stage3/kuhn_cfr_iter");
    group.throughput(Throughput::Elements(1));
    group.bench_function("vanilla_cfr_single_step", |b| {
        // 每个 sample 重建一个 fresh trainer，避免 step 之间累积 regret 让 cfv
        // 计算成本随 iter 漂移。fixed master_seed 跨 sample 一致，让 RngSource
        // 派生 deterministic。
        let master_seed: u64 = 0x5A_5A_5A_5A_5A_5A_5A_5A;
        b.iter_with_setup(
            || {
                (
                    VanillaCfrTrainer::new(KuhnGame, master_seed),
                    ChaCha20Rng::from_seed(master_seed),
                )
            },
            |(mut trainer, mut rng)| {
                let rng: &mut dyn RngSource = &mut rng;
                let _ = black_box(trainer.step(rng));
            },
        );
    });
    group.finish();
}

// ============================================================================
// stage3/leduc_cfr_iter — Leduc Vanilla CFR 单 step throughput
// ============================================================================

fn bench_leduc_cfr_iter(c: &mut Criterion) {
    let mut group = c.benchmark_group("stage3/leduc_cfr_iter");
    group.throughput(Throughput::Elements(1));
    group.bench_function("vanilla_cfr_single_step", |b| {
        let master_seed: u64 = 0xA5_A5_A5_A5_A5_A5_A5_A5;
        b.iter_with_setup(
            || {
                (
                    VanillaCfrTrainer::new(LeducGame, master_seed),
                    ChaCha20Rng::from_seed(master_seed),
                )
            },
            |(mut trainer, mut rng)| {
                let rng: &mut dyn RngSource = &mut rng;
                let _ = black_box(trainer.step(rng));
            },
        );
    });
    group.finish();
}

criterion_group!(
    stage3_cfr_benches,
    bench_kuhn_cfr_iter,
    bench_leduc_cfr_iter
);
criterion_main!(stage3_cfr_benches);
