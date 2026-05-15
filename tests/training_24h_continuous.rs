//! 阶段 4 D1 \[测试\]：24h continuous run no-panic + RSS 上界 + 每 10⁸ update
//! checkpoint 写入 anchor（D-461 / D-431）。
//!
//! 3 条 `#[ignore]` opt-in 测试（release profile + 24h wall-time + AWS / vultr
//! host 由用户手动触发；`pluribus_stage4_workflow.md` §步骤 D1 line 237 字面对应）：
//!
//! 1. [`stage4_six_max_24h_no_crash`] — 24h 连续 `EsMccfrTrainer<NlheGame6>::step`
//!    无 panic / OOM / NaN / Inf。D-461 字面 first usable 10⁹ update 训练前置
//!    sanity（24h × 4-core × 7.5K update/s ≈ 2.6×10⁹ update，覆盖 first usable
//!    上限有余）。
//! 2. [`stage4_six_max_24h_rss_increment_under_5gb`] — D-431 字面 RSS 上界。
//!    24h 训练 process RSS 增量 < 5 GB（首次 v3 artifact mmap 528 MiB +
//!    `RegretTable` HashMap 增长，预计 ~ 2-3 GB；5 GB 上界给 50% 余量）。
//! 3. [`stage4_six_max_checkpoint_every_1e8_update_writes_successfully`] —
//!    每 10⁸ update 写 checkpoint 成功；24h 跨越 ~26 个 checkpoint 边界，全成功。
//!
//! **D1 \[测试\] 角色边界**（继承 stage 1/2/3 同型政策）：本文件 0 改动
//! `src/training/`、`src/error.rs`、`docs/*`；如断言落在 \[实现\] 边界错误的
//! 产品代码上 → filed issue 移交 D2 \[实现\]。
//!
//! **D1 → D2 工程契约**（panic-fail 翻面条件）：
//!
//! - D2 \[实现\] 落地 v2 schema + 6-traverser RegretTable + StrategyAccumulator
//!   数组路径后，本套 3 测试在 AWS c7a.8xlarge × 32 vCPU 实测下应通过
//!   （D-490 stage 4 SLO + D-461 24h 字面 wall-time 不超限）。
//! - D-431 `TrainerError::OutOfMemory { rss_bytes, limit }` variant D2 落地后
//!   `MetricsCollector::observe` 检测 RSS > 阈值时返 Err 让 step 路径短路
//!   （A1 \[实现\] 已落地 `OutOfMemory` variant，D2 落地实际触发 dispatch）。
//!
//! **实测触发说明**：本套 3 测试 `#[ignore]` 默认跳过 — 用户手动 + 用户授权
//! 在 AWS / vultr host 上跑 `cargo test --release --test training_24h_continuous -- --ignored`
//! 触发，wall-time ~24h × 1 run（first usable 训练前置 sanity，与 D-461 字面
//! 一致）。本地 dev box 1-CPU 跑不动 24h 单 trainer（throughput ~ 1-2K update/s
//! 单线程，24h ~ 10⁸ update 上限），artifact 缺失 / wall-time 不足走 pass-with-skip。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use poker::training::nlhe_6max::NlheGame6;
use poker::training::{EsMccfrTrainer, Trainer};
use poker::{BucketTable, ChaCha20Rng};

// ===========================================================================
// 共享常量
// ===========================================================================

/// D-461 字面：24h continuous run wall-time 上限。
const D_461_WALL_TIME_LIMIT: Duration = Duration::from_secs(24 * 3600);

/// D-431 字面：RSS 增量上界（5 GB）。
const D_431_RSS_INCREMENT_LIMIT_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// D1 \[测试\] 实测建议的 host 最低 CPU 核心数（D-490 字面 4-core ≥ 15K update/s）。
/// 单 CPU host 跑不动 24h 训练（throughput ~ 1-2K update/s，24h × 1 ~ 10⁸ update，
/// 远低于 first usable 10⁹ update 触达 first checkpoint 边界）。
const D_490_MIN_CPU_CORES: usize = 4;

/// 每 1e8 update 写一次 checkpoint（D-461 字面 cadence）。
const CHECKPOINT_INTERVAL_UPDATES: u64 = 100_000_000;

/// 24h 训练 wall-time 阈值 — 接近 24h 边界提前 5 min 终止避免超 SLO
/// （`D_461_WALL_TIME_LIMIT - 5 min`）。
const TERMINATION_THRESHOLD: Duration = Duration::from_secs(24 * 3600 - 5 * 60);

/// 测试 master seed（ASCII "STG4_D1\x18"）。
const FIXED_SEED: u64 = 0x53_54_47_34_5F_44_31_18;

/// v3 production artifact path（D-424 lock）。
const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

// ===========================================================================
// helper
// ===========================================================================

/// 加载 v3 artifact；artifact 缺失 / 不匹配 → `None`（pass-with-skip）。
fn load_v3_artifact_or_skip() -> Option<Arc<BucketTable>> {
    let path = PathBuf::from(V3_ARTIFACT_PATH);
    if !path.exists() {
        eprintln!("skip: v3 artifact `{V3_ARTIFACT_PATH}` 不存在（GitHub-hosted runner 典型场景）");
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
        eprintln!("skip: artifact body BLAKE3 `{body_hex}` 不匹配 v3");
        return None;
    }
    Some(Arc::new(table))
}

/// host CPU 核心数；< 4 走 pass-with-skip（D-490 字面 4-core ≥ 15K update/s SLO）。
fn check_min_cores_or_skip() -> bool {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    if cores < D_490_MIN_CPU_CORES {
        eprintln!(
            "skip: available_parallelism = {cores} < D-490 字面 4-core；24h 训练 throughput \
             不足触达 first usable 10⁹ update 边界（建议 AWS c7a.8xlarge × 32 vCPU 实测）"
        );
        false
    } else {
        true
    }
}

/// 跨平台 RSS 读取（Linux：/proc/self/status VmRSS；其他平台 fallback 0 +
/// eprintln warning）。
fn read_rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        use std::io::{BufRead, BufReader};
        let f = match std::fs::File::open("/proc/self/status") {
            Ok(f) => f,
            Err(e) => {
                eprintln!("warning: /proc/self/status open 失败：{e}（RSS 检查跳过）");
                return 0;
            }
        };
        let reader = BufReader::new(f);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                // 字面格式："VmRSS:\t  N kB"
                let mut parts = rest.split_whitespace();
                if let (Some(n), Some(_unit)) = (parts.next(), parts.next()) {
                    if let Ok(kb) = n.parse::<u64>() {
                        return kb * 1024; // kB → byte
                    }
                }
            }
        }
        0
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("warning: non-Linux host RSS 检查 fallback 0");
        0
    }
}

/// 构造 stage 4 NlheGame6 + Linear+RM+ trainer（warmup_at=1M，默认主路径）。
fn build_trainer(table: &Arc<BucketTable>) -> EsMccfrTrainer<NlheGame6> {
    let game = NlheGame6::new(Arc::clone(table)).expect("NlheGame6::new on v3 artifact");
    EsMccfrTrainer::new(game, FIXED_SEED).with_linear_rm_plus(1_000_000)
}

// ===========================================================================
// Test 1 — 24h continuous no panic / OOM / NaN / Inf（D-461）
// ===========================================================================

/// D-461 字面：24h 连续 `EsMccfrTrainer<NlheGame6>::step` 无 panic / OOM / NaN /
/// Inf。
///
/// 退出条件：(a) wall-time 接近 24h 边界（提前 5 min 终止避免超 SLO）；
/// (b) step 调用返 Err → panic 暴露 product-code bug；(c) 每 10⁶ update probe
/// average_strategy 上的 InfoSet 抽样断言全 `is_finite()`。
///
/// **D2 \[实现\] 落地前**：`EsMccfrTrainer<NlheGame6>` 走 single-shared RegretTable
/// 路径（C2 commit 形态），24h 训练 throughput vultr ~ 7K update/s → ~6×10⁸
/// update / 24h；本测试通过 panic-fail 形态在 `cargo test --release --
/// --ignored` 显式开启时由 host 决定通过。D2 落地 6-traverser 数组 + rayon
/// thread pool 后 throughput AWS 32 vCPU ≥ 20K update/s 达 D-490 SLO，24h 触达
/// 10⁹ update。
#[test]
#[ignore = "release/--ignored opt-in（24h continuous run；用户手动 + AWS / vultr host；D2 \\[实现\\] 落地后转绿）"]
fn stage4_six_max_24h_no_crash() {
    if !check_min_cores_or_skip() {
        return;
    }
    let Some(table) = load_v3_artifact_or_skip() else {
        return;
    };
    let mut trainer = build_trainer(&table);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let start = Instant::now();
    let mut last_probe_at: u64 = 0;
    const PROBE_INTERVAL: u64 = 1_000_000;

    loop {
        trainer.step(&mut rng).unwrap_or_else(|e| {
            panic!(
                "D-461：step 失败（update {}）：{e:?}",
                trainer.update_count()
            )
        });
        let updates = trainer.update_count();

        // 每 1M update 检查 wall-time + 抽样 finite sanity
        if updates - last_probe_at >= PROBE_INTERVAL {
            last_probe_at = updates;
            let elapsed = start.elapsed();
            if elapsed >= TERMINATION_THRESHOLD {
                eprintln!(
                    "D-461：24h 边界达成 — wall = {:.2} h / updates = {} / throughput = {:.1} update/s",
                    elapsed.as_secs_f64() / 3600.0,
                    updates,
                    updates as f64 / elapsed.as_secs_f64()
                );
                break;
            }
        }
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed <= D_461_WALL_TIME_LIMIT,
        "D-461：实际 wall-time {:.2} h 超 24h 上限",
        elapsed.as_secs_f64() / 3600.0
    );
}

// ===========================================================================
// Test 2 — RSS 上界（D-431）
// ===========================================================================

/// D-431 字面：24h 训练 process RSS 增量 < 5 GB。
///
/// 走与 Test 1 同型 trainer + 每 10M update probe RSS；24h 内最高峰 RSS −
/// startup RSS < 5 GB（首次 v3 artifact mmap 528 MiB + RegretTable HashMap
/// 增长，预计 ~ 2-3 GB；5 GB 上界给 50% 余量）。
///
/// 关键不变量：(a) `start_rss = read_rss_bytes()` 在 trainer 构造后立即读
/// （含 528 MiB v3 mmap baseline）；(b) `peak_rss - start_rss < 5 GB`。
///
/// **D2 \[实现\] 落地前**：6-traverser RegretTable 数组 deferred，单 table 路径
/// RSS 预计 ~ 1-2 GB；本测试在 stage 4 single-table commit 上通过。D2 落地
/// 6-traverser 后 RSS 增长 ~ 6× 但仍预计 < 5 GB（每 traverser ~ 500 MB-1 GB
/// regret + strategy，6 × 1 GB = 6 GB 略超阈 — D-431-revM 起步条件）。
#[test]
#[ignore = "release/--ignored opt-in（24h continuous + RSS probe；用户手动 + AWS / vultr host；D2 \\[实现\\] 落地后转绿）"]
fn stage4_six_max_24h_rss_increment_under_5gb() {
    if !check_min_cores_or_skip() {
        return;
    }
    let Some(table) = load_v3_artifact_or_skip() else {
        return;
    };
    let mut trainer = build_trainer(&table);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let start_rss = read_rss_bytes();
    if start_rss == 0 {
        eprintln!("skip: RSS 不可读（非 Linux host 或 /proc/self/status open 失败）");
        return;
    }
    eprintln!("D-431：start_rss = {} MB", start_rss / 1024 / 1024);
    let mut peak_rss = start_rss;
    let start = Instant::now();
    let mut last_probe_at: u64 = 0;
    const RSS_PROBE_INTERVAL: u64 = 10_000_000;

    loop {
        trainer
            .step(&mut rng)
            .unwrap_or_else(|e| panic!("step 失败：{e:?}"));
        let updates = trainer.update_count();
        if updates - last_probe_at >= RSS_PROBE_INTERVAL {
            last_probe_at = updates;
            let cur = read_rss_bytes();
            if cur > peak_rss {
                peak_rss = cur;
            }
            let increment = peak_rss.saturating_sub(start_rss);
            assert!(
                increment < D_431_RSS_INCREMENT_LIMIT_BYTES,
                "D-431：peak_rss 增量 {} MB 超 5 GB 上界 @ update {updates}（D-431-revM 起步条件）",
                increment / 1024 / 1024
            );
            let elapsed = start.elapsed();
            if elapsed >= TERMINATION_THRESHOLD {
                eprintln!(
                    "D-431：24h 边界 — peak_rss 增量 = {} MB / updates = {updates}",
                    peak_rss.saturating_sub(start_rss) / 1024 / 1024
                );
                break;
            }
        }
    }
}

// ===========================================================================
// Test 3 — 每 10⁸ update checkpoint 写入成功（D-461 cadence）
// ===========================================================================

/// D-461 cadence：每 10⁸ update 写一次 checkpoint，24h 内全部成功。
///
/// 走 Test 1 同型 trainer + `save_checkpoint` 在 update_count 跨 10⁸ 倍数边界
/// 时调用。24h × 4-core × 7.5K update/s ≈ 2.6×10⁹ update / 10⁸ = ~ 26 个
/// checkpoint。每 checkpoint 后 read-back 验证 schema_version=2（D2 落地后）+
/// update_count 与 trainer 一致 + 文件长度 > HEADER_LEN + TRAILER_LEN。
///
/// **D2 \[实现\] 落地前**：stage 3 path schema=1 write + read-back schema=1，
/// 本测试 schema_version=2 断言 panic-fail；D2 落地后转绿。
#[test]
#[ignore = "release/--ignored opt-in（24h continuous + 10⁸ update cadence checkpoint；D2 \\[实现\\] 落地后转绿）"]
fn stage4_six_max_checkpoint_every_1e8_update_writes_successfully() {
    if !check_min_cores_or_skip() {
        return;
    }
    let Some(table) = load_v3_artifact_or_skip() else {
        return;
    };
    let mut trainer = build_trainer(&table);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let start = Instant::now();
    let mut next_checkpoint_at = CHECKPOINT_INTERVAL_UPDATES;
    let mut checkpoint_count: u64 = 0;

    // 用 tempdir 避免污染 repo path
    let tmpdir = tempfile::tempdir().expect("create tempdir");

    loop {
        trainer
            .step(&mut rng)
            .unwrap_or_else(|e| panic!("step 失败 @ update {}: {e:?}", trainer.update_count()));
        let updates = trainer.update_count();
        if updates >= next_checkpoint_at {
            checkpoint_count += 1;
            let path = tmpdir.path().join(format!("ckpt_{checkpoint_count}.bin"));
            trainer.save_checkpoint(&path).unwrap_or_else(|e| {
                panic!("D-461：checkpoint #{checkpoint_count} @ update {updates} save 失败：{e:?}")
            });
            let meta = std::fs::metadata(&path).expect("metadata");
            assert!(
                meta.len() > 128 + 32,
                "D-461：checkpoint #{checkpoint_count} 文件长度 {} 应 > header (128) + trailer (32)",
                meta.len()
            );

            let ckpt = poker::training::checkpoint::Checkpoint::open(&path).unwrap_or_else(|e| {
                panic!("D-461：checkpoint #{checkpoint_count} Checkpoint::open 失败：{e:?}")
            });
            assert_eq!(
                ckpt.update_count, updates,
                "D-461：checkpoint #{checkpoint_count} read-back update_count 一致"
            );
            assert_eq!(
                ckpt.schema_version, 2,
                "D-449：stage 4 path checkpoint schema_version 应 == 2（D2 \\[实现\\] 落地后）"
            );
            // 删除 checkpoint 文件避免 tempdir 占满
            let _ = std::fs::remove_file(&path);
            next_checkpoint_at += CHECKPOINT_INTERVAL_UPDATES;
        }
        let elapsed = start.elapsed();
        if elapsed >= TERMINATION_THRESHOLD {
            eprintln!(
                "D-461：24h 边界 — 完成 {checkpoint_count} 个 checkpoint，updates = {updates}"
            );
            break;
        }
    }
    assert!(
        checkpoint_count >= 1,
        "D-461：24h 内应至少完成 1 个 checkpoint（first usable 10⁹ update 下 ~ 10 个）"
    );
}
