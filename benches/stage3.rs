//! 阶段 3 B1 + C1 + E1 \[测试\]：CFR / MCCFR 训练 throughput benchmark framework
//! （D-367 criterion bench）。
//!
//! 3 个 bench group active（B1 落地 2 个 + C1 落地 1 个，E1 \[测试\] 维持 3
//! group active + criterion measurement per workflow §E1 line 275）：
//!
//! 1. `stage3/kuhn_cfr_iter`（B1）：Kuhn Vanilla CFR 单 step throughput。SLO 关联
//!    D-360（10K iter `< 1 s` release → 单 step `< 100 µs`，目标 throughput
//!    `≥ 10K iter/s`）。
//! 2. `stage3/leduc_cfr_iter`（B1）：Leduc Vanilla CFR 单 step throughput。SLO 关联
//!    D-360（10K iter `< 60 s` release → 单 step `< 6 ms`，目标 throughput
//!    `≥ 167 iter/s`）。
//! 3. `stage3/nlhe_es_mccfr_update`（C1）：简化 NLHE ES-MCCFR 单 update throughput。
//!    SLO 关联 D-361（单线程 `≥ 10K update/s`，4-core `≥ 50K update/s` 由 E1
//!    \[测试\] `tests/perf_slo.rs::stage3_*` 严格断言）。需要 v3 artifact（D-314-rev1
//!    lock 路径 `artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin`），
//!    artifact 缺失时 bench 走 no-op 占位 + eprintln 提示（避免在 CI 无 artifact
//!    host 上 panic）。
//!
//! 本文件不做 SLO 断言，仅产生 throughput 数据。SLO 阈值断言 + 严格 fail/pass
//! 走 E1 \[测试\] 落地的 `tests/perf_slo.rs::stage3_*`（D-369）—— D-360 / D-361
//! / D-348 共 6 条断言。
//!
//! 当前 D2 \[实现\] 已 closed 状态：[`VanillaCfrTrainer::step`] B2 落地、
//! [`EsMccfrTrainer::step`] C2 落地、`Checkpoint::{save,open}` D2 落地。3 个
//! bench group 在 release profile 全部产生有效 throughput 数据；
//! `stage3/nlhe_es_mccfr_update` 在 artifact 缺失时维持 no-op 占位（CI 无
//! artifact host 上 `cargo bench --bench stage3` 不 panic）。
//!
//! 角色边界：本文件属 `[测试]` agent。`[实现]` agent 不得修改。

use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use poker::training::kuhn::KuhnGame;
use poker::training::leduc::LeducGame;
use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::{EsMccfrTrainer, Trainer, VanillaCfrTrainer};
use poker::{BucketTable, ChaCha20Rng, RngSource};

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

// ============================================================================
// stage3/nlhe_es_mccfr_update — 简化 NLHE ES-MCCFR 单 update throughput（C1 落地）
// ============================================================================

const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";

fn bench_nlhe_es_mccfr_update(c: &mut Criterion) {
    let mut group = c.benchmark_group("stage3/nlhe_es_mccfr_update");
    group.throughput(Throughput::Elements(1));

    // D-314-rev1 lock：v3 artifact 缺失时走 no-op 占位 + eprintln 提示让 CI 无
    // artifact host 不 panic（与 tests/cfr_simplified_nlhe.rs skip-with-eprintln
    // 同型）；本地 dev box / vultr / AWS host 有 artifact 时跑真实 throughput。
    let path = PathBuf::from(V3_ARTIFACT_PATH);
    let table = if path.exists() {
        match BucketTable::open(&path) {
            Ok(t) => Some(Arc::new(t)),
            Err(e) => {
                eprintln!("stage3/nlhe_es_mccfr_update skip: BucketTable::open 失败：{e:?}");
                None
            }
        }
    } else {
        eprintln!("stage3/nlhe_es_mccfr_update skip: v3 artifact `{V3_ARTIFACT_PATH}` 不存在");
        None
    };

    group.bench_function("es_mccfr_single_update", |b| {
        let master_seed: u64 = 0x5E_5E_5E_5E_5E_5E_5E_5E;
        if let Some(ref shared_table) = table {
            // setup 每 sample 重建 trainer（与 kuhn / leduc bench 同型）；构造
            // SimplifiedNlheGame 仅 Arc 浅 clone 共享 528 MiB BucketTable 内部 mmap。
            b.iter_with_setup(
                || {
                    let game = SimplifiedNlheGame::new(Arc::clone(shared_table)).expect(
                        "D-314-rev1：v3 artifact schema_version=2 应当被 SimplifiedNlheGame::new 接受",
                    );
                    (
                        EsMccfrTrainer::new(game, master_seed),
                        ChaCha20Rng::from_seed(master_seed),
                    )
                },
                |(mut trainer, mut rng)| {
                    let rng: &mut dyn RngSource = &mut rng;
                    let _ = black_box(trainer.step(rng));
                },
            );
        } else {
            // artifact 缺失走 no-op 占位让 criterion 不因 "bencher 未使用" panic；
            // 本路径下 bench 数据无意义，仅保证 `cargo bench --bench stage3 --no-run`
            // 编译成功 + `cargo bench --bench stage3` 在无 artifact host 上不 panic。
            b.iter(|| black_box(()));
        }
    });
    group.finish();
}

criterion_group!(
    stage3_cfr_benches,
    bench_kuhn_cfr_iter,
    bench_leduc_cfr_iter,
    bench_nlhe_es_mccfr_update,
);
criterion_main!(stage3_cfr_benches);
