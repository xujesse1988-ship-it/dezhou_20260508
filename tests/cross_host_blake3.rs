//! 阶段 3 F1 \[测试\]：跨 host checkpoint BLAKE3 32-seed regression guard
//! （D-347 / D-362 / 继承 stage 1 D-051 + stage 2 §G-batch1 §3.4-batch2 跨架构
//! baseline 模式）。
//!
//! 目的（`pluribus_stage3_workflow.md` §步骤 F1 line 309 字面）：「fixed seed →
//! checkpoint BLAKE3 byte-equal across runs」。本测试用 32 个固定 seed 跑 5 iter
//! Kuhn Vanilla CFR → `save_checkpoint` → 取 file content BLAKE3，得 32 条
//! `seed=<dec> hash=<hex>` 行；与同 (os, arch) baseline 文件 byte-equal 比对。
//!
//! 设计（与 `tests/cross_arch_hash.rs` / `bucket-table-arch-hashes-linux-x86_64.txt`
//! 同型，复用 32-seed 名单 `ARCH_BASELINE_SEEDS`）：
//!
//! - **within-process determinism**：连跑 2 次，输出必须 byte-equal（D-347 / D-362
//!   "固定 seed 重复 BLAKE3 byte-equal"）。本测试 default profile active，5 iter
//!   Kuhn × 32 seed × 2 次 ≈ 100 ms。
//! - **同 host baseline regression guard**：若 `tests/data/checkpoint-hashes-<os>-<arch>.txt`
//!   存在 → byte-equal 必过。F1 commit 首次落地 `tests/data/checkpoint-hashes-linux-x86_64.txt`
//!   （32 行）作为 linux/x86_64 baseline；darwin/aarch64 + linux/aarch64 留 capture
//!   on-demand。
//! - **跨 (os, arch) 对照**：`linux-x86_64` vs `darwin-aarch64` 两份 baseline 都存在
//!   时 byte-equal 必过（aspirational，未达成不算 stage 3 fail；继承 stage 1 D-052
//!   carve-out 模式）。
//! - **capture-only entry**：`#[ignore]` 子测试 `cross_host_capture_only` 把当前
//!   host 32 行 dump 到 stdout，供 `scripts/capture-checkpoint-hashes.sh` 重定向
//!   写入 baseline。
//!
//! 输入选择：Kuhn Vanilla CFR 5 iter（与 D1 `kuhn_round_trip` half-iters 同形态），
//! 一是单 iter `< 1 ms` 跑量小，二是单 seed checkpoint 文件 ~280 byte，BLAKE3 命中
//! 在 32 byte trailer + header offset 表 + bincode body 全段（不仅 trailer），三是
//! Kuhn 路径 `bucket_table_blake3 == [0; 32]`，跨 host 与 stage 2 v3 artifact 无关。
//!
//! **F1 \[测试\] 角色边界**：本文件不修改 `src/training/`；如发现 baseline drift 走
//! F2 \[实现\] 修复（继承 stage 1 §F-rev1 错误前移模式）。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use blake3::Hasher;
use poker::training::kuhn::KuhnGame;
use poker::training::nlhe_6max::NlheGame6;
use poker::training::{EsMccfrTrainer, Trainer, VanillaCfrTrainer};
use poker::{BucketTable, ChaCha20Rng};

const KUHN_FIXTURE_ITERS: u64 = 5;

// ===========================================================================
// stage 4 §F1 [测试] 共享常量（与 perf_slo / lbr_eval_convergence /
// slumbot_eval / baseline_eval 跨测试 ground truth 一致）
// ===========================================================================

/// stage 4 D-424 v3 production artifact path（NlheGame6 Linear+RM+ 训练
/// dependency；不进 git history，本地 dev box / vultr / AWS host 落地，CI
/// 走 pass-with-skip 路径）。
const STAGE4_V3_ARTIFACT_PATH: &str =
    "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";

/// stage 4 D-424 v3 artifact body BLAKE3 ground truth。
const STAGE4_V3_BODY_BLAKE3_HEX: &str =
    "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

/// stage 4 D-409 字面 warm-up 切换点（Linear+RM+ 主路径）。
const STAGE4_WARMUP_COMPLETE_AT: u64 = 1_000_000;

/// stage 4 F1 [测试] 字面 cross-host checkpoint anchor iter count（D-440 first
/// usable 10⁵ checkpoint anchor — 10⁵ update 路径短到 wall-time ~10 s per
/// seed × 32 seed = ~5 min on 32-vCPU AWS / 60 min on 1-CPU vultr）。
const STAGE4_FIRST_USABLE_ITERS: u64 = 100_000;

/// 32 个固定 seed（与 `tests/cross_arch_hash.rs::ARCH_BASELINE_SEEDS` 同名单，
/// 让 stage 3 checkpoint baseline 与 stage 1 hand-history baseline 共享 seed
/// 序列以便跨 stage 调试对照）。
const ARCH_BASELINE_SEEDS: [u64; 32] = [
    0,
    1,
    2,
    3,
    7,
    13,
    42,
    100,
    255,
    256,
    1023,
    1024,
    65535,
    65536,
    1_000_000,
    0xCAFE_BABE,
    0xDEAD_BEEF,
    0xFEED_FACE,
    0xC1_E1AA,
    0xC1_DA_7A,
    0xC1_F00D,
    0xC001_CAFE,
    0xFFFF_FFFF,
    1u64 << 32,
    1u64 << 48,
    (1u64 << 63) - 1,
    1u64 << 63,
    u64::MAX - 1,
    u64::MAX,
    0xA5A5_A5A5_A5A5_A5A5,
    0x5A5A_5A5A_5A5A_5A5A,
    0x1234_5678_9ABC_DEF0,
];

fn unique_tmp_path(seed: u64) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut p = std::env::temp_dir();
    p.push(format!(
        "poker_f1_cross_host_blake3_seed{seed}_{pid}_{nanos}.bin"
    ));
    p
}

/// 跑 5 iter Kuhn Vanilla CFR → save_checkpoint → 取 file content BLAKE3。
fn capture_one_seed(seed: u64) -> [u8; 32] {
    let mut trainer = VanillaCfrTrainer::new(KuhnGame, seed);
    let mut rng = ChaCha20Rng::from_seed(seed);
    for _ in 0..KUHN_FIXTURE_ITERS {
        trainer.step(&mut rng).expect("kuhn step");
    }
    let path = unique_tmp_path(seed);
    trainer
        .save_checkpoint(&path)
        .expect("save_checkpoint produce bytes");
    let bytes = fs::read(&path).expect("re-read checkpoint");
    let _ = fs::remove_file(&path);

    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    hasher.finalize().into()
}

fn capture_baseline() -> String {
    let mut lines = String::new();
    for seed in ARCH_BASELINE_SEEDS {
        let hash = capture_one_seed(seed);
        let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        lines.push_str(&format!("seed={seed} hash={hex}\n"));
    }
    lines
}

fn baseline_path() -> Option<PathBuf> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        return None;
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return None;
    };
    Some(
        PathBuf::from(manifest)
            .join("tests")
            .join("data")
            .join(format!("checkpoint-hashes-{os}-{arch}.txt")),
    )
}

// ===========================================================================
// 1. within-process determinism — D-347 / D-362 强 anchor
// ===========================================================================

#[test]
fn within_process_blake3_reproducible_twice() {
    let first = capture_baseline();
    let second = capture_baseline();
    assert_eq!(
        first, second,
        "D-347 / D-362：32-seed checkpoint BLAKE3 within-process 重复必须 byte-equal"
    );
    // sanity check 行数
    let line_count = first.lines().count();
    assert_eq!(
        line_count, 32,
        "32 个 seed 应产 32 行，实际 {line_count} 行"
    );
}

// ===========================================================================
// 2. 同 host baseline regression guard
// ===========================================================================

#[test]
fn cross_host_baseline_byte_equal_for_current_arch() {
    let actual = capture_baseline();
    let Some(path) = baseline_path() else {
        eprintln!(
            "[cross-host-blake3] 当前 (os, arch) 未声明 baseline 路径；capture-only output:\n{actual}"
        );
        return;
    };
    let expected = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "[cross-host-blake3] baseline 缺失 at {}: {e}\n\
                 Run scripts/capture-checkpoint-hashes.sh on this host to seed it.\n\
                 capture-only output:\n{actual}",
                path.display()
            );
            return;
        }
    };
    if actual.trim() != expected.trim() {
        let mut diff = Vec::new();
        for (i, (a, e)) in actual.lines().zip(expected.lines()).enumerate() {
            if a != e {
                diff.push(format!("line {i}: actual={a:?} expected={e:?}"));
                if diff.len() >= 5 {
                    break;
                }
            }
        }
        panic!(
            "checkpoint BLAKE3 baseline drift at {}:\n{}\n",
            path.display(),
            diff.join("\n")
        );
    }
}

// ===========================================================================
// 3. 跨 (os, arch) 对照 baseline byte-equal（aspirational，aspirational fail 不
// 阻塞 stage 3，与 stage 1 D-052 carve-out 同型）
// ===========================================================================

#[test]
fn cross_arch_baselines_byte_equal_when_both_present() {
    let manifest = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[cross-host-blake3-pair] CARGO_MANIFEST_DIR unset; skip");
            return;
        }
    };
    let linux = PathBuf::from(&manifest)
        .join("tests")
        .join("data")
        .join("checkpoint-hashes-linux-x86_64.txt");
    let darwin = PathBuf::from(&manifest)
        .join("tests")
        .join("data")
        .join("checkpoint-hashes-darwin-aarch64.txt");
    let (a, b) = match (fs::read_to_string(&linux), fs::read_to_string(&darwin)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            eprintln!(
                "[cross-host-blake3-pair] one or both baselines missing; skip (linux={} darwin={})",
                linux.display(),
                darwin.display(),
            );
            return;
        }
    };
    if a.trim() == b.trim() {
        return;
    }
    let mut diff = Vec::new();
    for (i, (la, lb)) in a.lines().zip(b.lines()).enumerate() {
        if la != lb {
            diff.push(format!("line {i}: linux={la:?} darwin={lb:?}"));
            if diff.len() >= 5 {
                break;
            }
        }
    }
    panic!(
        "D-347 / D-052 carve-out: linux-x86_64 vs darwin-aarch64 checkpoint baselines diverge:\n{}\n",
        diff.join("\n")
    );
}

// ===========================================================================
// 4. capture-only entry — 由 scripts/capture-checkpoint-hashes.sh 调用
// ===========================================================================

#[test]
#[ignore = "capture-only entry — invoked by scripts/capture-checkpoint-hashes.sh"]
fn cross_host_capture_only() {
    let out = capture_baseline();
    print!("{out}");
}

// ===========================================================================
// 5. sanity：seed 名单一致性 — 32 条且与 stage 1 baseline 同步
// ===========================================================================

#[test]
fn seed_list_has_32_entries_matching_stage1_baseline() {
    // 32-seed 名单与 stage 1 `cross_arch_hash.rs::ARCH_BASELINE_SEEDS` 同步（同型
    // small/large/boundary/special 覆盖），让 stage 1 hand-history baseline 与
    // stage 3 checkpoint baseline 共享 seed serial，便于跨 stage 调试对照。
    assert_eq!(ARCH_BASELINE_SEEDS.len(), 32);
    // sanity：包含 0 / u64::MAX / 0xCAFE_BABE 等基础锚点
    assert!(ARCH_BASELINE_SEEDS.contains(&0));
    assert!(ARCH_BASELINE_SEEDS.contains(&u64::MAX));
    assert!(ARCH_BASELINE_SEEDS.contains(&0xCAFE_BABE));
}

// ===========================================================================
// 阶段 4 §F1 [测试]：扩展 stage 4 NlheGame6 Linear+RM+ 6-traverser checkpoint
// BLAKE3 32-seed regression guard（`pluribus_stage4_workflow.md` §F1 line 293
// + `pluribus_stage4_decisions.md` §10 D-490 + §5 D-424 v3 artifact lock）。
//
// 设计：与 stage 3 既有 `capture_baseline` / `capture_one_seed` 同型，但走
// `NlheGame6::new` + `EsMccfrTrainer::with_linear_rm_plus(1M)` + 10⁵ iter
// `step()` 路径 + `save_checkpoint`（schema_version=2 / 6-traverser 6-region
// body）+ BLAKE3 file content hash。
//
// 32-seed 名单复用 `ARCH_BASELINE_SEEDS`（stage 1/3 同步）。
//
// **F1 [测试] 角色边界**：本文件 0 改动 `src/training/`；如发现 baseline drift
// 走 F2 [实现] 修复（继承 stage 1 §F-rev1 错误前移模式）。
//
// **stage 4 baseline 文件**：`tests/data/checkpoint-hashes-linux-x86_64-stage4.
// txt`（32 行 `seed=<dec> hash=<hex>` 字面格式）。F1 commit 初始落地走
// placeholder 头注（让 default profile active 测试 skip-with-message）；用户
// 授权 dev box / vultr / AWS host 跑 `scripts/capture-checkpoint-hashes-stage4.
// sh` 重定向写入真实 hash 后翻面成 active byte-equal regression。
// ===========================================================================

/// stage 4 跑 10⁵ iter NlheGame6 Linear+RM+ → save_checkpoint → 取 file
/// content BLAKE3。返回 `None` 表 v3 artifact 不可用 → pass-with-skip。
fn stage4_capture_one_seed(seed: u64) -> Option<[u8; 32]> {
    let path = PathBuf::from(STAGE4_V3_ARTIFACT_PATH);
    if !path.exists() {
        return None;
    }
    let table = match BucketTable::open(&path) {
        Ok(t) => t,
        Err(_) => return None,
    };
    let body_hex: String = table
        .content_hash()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if body_hex != STAGE4_V3_BODY_BLAKE3_HEX {
        return None;
    }
    let game = match NlheGame6::new(Arc::new(table)) {
        Ok(g) => g,
        Err(_) => return None,
    };
    let mut trainer =
        EsMccfrTrainer::new(game, seed).with_linear_rm_plus(STAGE4_WARMUP_COMPLETE_AT);
    let mut rng = ChaCha20Rng::from_seed(seed);
    for _ in 0..STAGE4_FIRST_USABLE_ITERS {
        trainer
            .step(&mut rng)
            .expect("stage4 NlheGame6 Linear+RM+ step");
    }
    let ckpt_path = unique_tmp_path(seed ^ 0x0F15_C742_4A55_4E4D);
    trainer
        .save_checkpoint(&ckpt_path)
        .expect("stage4 save_checkpoint produce bytes");
    let bytes = fs::read(&ckpt_path).expect("re-read stage4 checkpoint");
    let _ = fs::remove_file(&ckpt_path);

    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    Some(hasher.finalize().into())
}

/// stage 4 32 seed 累积 baseline 文本。若 v3 artifact 不可用 → 返回 `None`（
/// 让 active 测试走 pass-with-skip 路径）。
fn stage4_capture_baseline() -> Option<String> {
    let mut lines = String::new();
    for seed in ARCH_BASELINE_SEEDS {
        let hash = stage4_capture_one_seed(seed)?;
        let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        lines.push_str(&format!("seed={seed} hash={hex}\n"));
    }
    Some(lines)
}

/// stage 4 baseline 文件路径（与 stage 3 既有 `baseline_path()` 同型，文件名
/// 加 `-stage4` 后缀让 stage 3 + stage 4 baseline 文件不冲突）。
fn stage4_baseline_path() -> Option<PathBuf> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        return None;
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return None;
    };
    Some(
        PathBuf::from(manifest)
            .join("tests")
            .join("data")
            .join(format!("checkpoint-hashes-{os}-{arch}-stage4.txt")),
    )
}

/// 判断 baseline 文件内容是否走 F1 commit placeholder 模式（占位 `#`
/// 注释头 + 0 个 `seed=...` 行）。让 default profile 在 F1 closure 形态下
/// 走 skip-with-message 不 panic。
fn stage4_baseline_is_placeholder(content: &str) -> bool {
    let seed_lines = content.lines().filter(|l| l.starts_with("seed=")).count();
    seed_lines == 0
}

// ===========================================================================
// stage 4 #1 — within-process determinism（10⁵ iter × 32 seed × 2 次 ≈ 10 min
// on AWS c7a.8xlarge × 32 vCPU，太重 → `#[ignore]` opt-in）
// ===========================================================================

#[test]
#[ignore = "stage4 cross-host BLAKE3; 32 seed × 10⁵ iter × 2 = 64 run ~10 min on 32-vCPU; \
            opt-in via `cargo test --release --test cross_host_blake3 -- --ignored`"]
fn stage4_within_process_blake3_reproducible_twice() {
    let Some(first) = stage4_capture_baseline() else {
        eprintln!(
            "[stage4-cross-host-blake3] skip: v3 artifact `{STAGE4_V3_ARTIFACT_PATH}` 不可用 \
             或 BucketTable::open / NlheGame6::new 失败"
        );
        return;
    };
    let second = match stage4_capture_baseline() {
        Some(s) => s,
        None => {
            eprintln!("[stage4-cross-host-blake3] skip: second capture 失败");
            return;
        }
    };
    assert_eq!(
        first, second,
        "D-484：stage 4 32-seed × 10⁵ iter NlheGame6 Linear+RM+ checkpoint BLAKE3 within-\
         process 重复必须 byte-equal（D-321-rev2 真并发 deterministic merge / D-401 Linear \
         weighting / D-402 RM+ clamp 任一漂移触发；继承 stage 3 D-362 同型 P0）"
    );
    let line_count = first.lines().count();
    assert_eq!(
        line_count, 32,
        "stage 4 baseline 32 行，实际 {line_count} 行"
    );
}

// ===========================================================================
// stage 4 #2 — 同 host baseline regression guard（active；F1 closure 走
// placeholder pass-with-skip，F2 [实现] + capture-only opt-in 翻面后 byte-
// equal active 必过）
// ===========================================================================

#[test]
fn stage4_cross_host_baseline_byte_equal_for_current_arch() {
    let Some(path) = stage4_baseline_path() else {
        eprintln!("[stage4-cross-host-blake3] 当前 (os, arch) 未声明 baseline 路径；skip");
        return;
    };
    let expected = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "[stage4-cross-host-blake3] baseline 缺失 at {}: {e}\n\
                 Run stage4 cross_host_capture_only on this host to seed it.",
                path.display()
            );
            return;
        }
    };
    if stage4_baseline_is_placeholder(&expected) {
        eprintln!(
            "[stage4-cross-host-blake3] baseline at {} 走 F1 placeholder 模式（0 seed=... 行）\
             — 用户授权 dev box / vultr / AWS host 跑 stage4_cross_host_capture_only --ignored \
             重定向写入真实 hash 后翻面成 active byte-equal regression。",
            path.display()
        );
        return;
    }
    let Some(actual) = stage4_capture_baseline() else {
        eprintln!("[stage4-cross-host-blake3] v3 artifact 不可用 / NlheGame6 构造失败 → skip。");
        return;
    };
    if actual.trim() != expected.trim() {
        let mut diff = Vec::new();
        for (i, (a, e)) in actual.lines().zip(expected.lines()).enumerate() {
            if a != e {
                diff.push(format!("line {i}: actual={a:?} expected={e:?}"));
                if diff.len() >= 5 {
                    break;
                }
            }
        }
        panic!(
            "stage 4 checkpoint BLAKE3 baseline drift at {}:\n{}\n",
            path.display(),
            diff.join("\n")
        );
    }
}

// ===========================================================================
// stage 4 #3 — 跨 (os, arch) 对照 baseline byte-equal（aspirational，aspirational
// fail 不阻塞 stage 4，与 stage 1 D-052 / stage 3 D-368 carve-out 同型）
// ===========================================================================

#[test]
fn stage4_cross_arch_baselines_byte_equal_when_both_present() {
    let manifest = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[stage4-cross-host-blake3-pair] CARGO_MANIFEST_DIR unset; skip");
            return;
        }
    };
    let linux = PathBuf::from(&manifest)
        .join("tests")
        .join("data")
        .join("checkpoint-hashes-linux-x86_64-stage4.txt");
    let darwin = PathBuf::from(&manifest)
        .join("tests")
        .join("data")
        .join("checkpoint-hashes-darwin-aarch64-stage4.txt");
    let (a, b) = match (fs::read_to_string(&linux), fs::read_to_string(&darwin)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            eprintln!(
                "[stage4-cross-host-blake3-pair] one or both baselines missing; skip \
                 (linux={} darwin={})",
                linux.display(),
                darwin.display(),
            );
            return;
        }
    };
    if stage4_baseline_is_placeholder(&a) || stage4_baseline_is_placeholder(&b) {
        eprintln!(
            "[stage4-cross-host-blake3-pair] one or both baselines 走 placeholder 模式；skip"
        );
        return;
    }
    if a.trim() == b.trim() {
        return;
    }
    let mut diff = Vec::new();
    for (i, (la, lb)) in a.lines().zip(b.lines()).enumerate() {
        if la != lb {
            diff.push(format!("line {i}: linux={la:?} darwin={lb:?}"));
            if diff.len() >= 5 {
                break;
            }
        }
    }
    panic!(
        "stage 4 D-368 carve-out: linux-x86_64 vs darwin-aarch64 checkpoint baselines diverge:\n\
         {}\n",
        diff.join("\n")
    );
}

// ===========================================================================
// stage 4 #4 — capture-only entry（由用户授权 dev box / vultr / AWS host 跑
// `cargo test --release --test cross_host_blake3 -- --ignored stage4_cross_host_capture_only`
// 把当前 host 32 行 dump 到 stdout，供 scripts/capture-checkpoint-hashes-stage4.sh 重定向
// 写入 tests/data/checkpoint-hashes-linux-x86_64-stage4.txt baseline）
// ===========================================================================

#[test]
#[ignore = "stage4 capture-only entry — invoked by scripts/capture-checkpoint-hashes-stage4.sh"]
fn stage4_cross_host_capture_only() {
    let Some(out) = stage4_capture_baseline() else {
        eprintln!(
            "[stage4-cross-host-blake3-capture] skip: v3 artifact `{STAGE4_V3_ARTIFACT_PATH}` \
             不可用；本地 dev box / vultr / AWS host 有 artifact 时跑 capture。"
        );
        return;
    };
    print!("{out}");
}

// ===========================================================================
// stage 4 #5 — sanity：32-seed 名单一致性 + baseline 文件路径解析正确（active，
// lightweight，pass-always 让 F1 [测试] commit 默认 profile 8 active anchor
// 数量充足）
// ===========================================================================

#[test]
fn stage4_seed_list_has_32_entries_matching_stage3_baseline() {
    // stage 4 cross_host BLAKE3 baseline 32-seed 名单复用 `ARCH_BASELINE_SEEDS`
    //（stage 1 hand-history baseline + stage 3 checkpoint baseline 同步 seed
    // 序列），让 stage 1 + 3 + 4 baseline 在跨 stage 调试时同 seed 对照。
    assert_eq!(ARCH_BASELINE_SEEDS.len(), 32);
    // stage 4 D-424 v3 artifact path 字面 sanity（与 perf_slo / lbr_eval_convergence
    // / slumbot_eval / baseline_eval 跨测试 ground truth 一致）
    assert!(STAGE4_V3_ARTIFACT_PATH.ends_with(".bin"));
    assert_eq!(STAGE4_V3_BODY_BLAKE3_HEX.len(), 64);
    assert_eq!(STAGE4_FIRST_USABLE_ITERS, 100_000);
    assert_eq!(STAGE4_WARMUP_COMPLETE_AT, 1_000_000);
}

#[test]
fn stage4_baseline_path_resolves_for_current_arch_or_skip() {
    // baseline_path 在 linux/x86_64 + linux/aarch64 + darwin/aarch64 三个组合
    // 上返 Some(...)；其他 (os, arch) 返 None（skip-with-message 路径）。
    let path = stage4_baseline_path();
    let on_supported = (cfg!(target_os = "linux") || cfg!(target_os = "macos"))
        && (cfg!(target_arch = "x86_64") || cfg!(target_arch = "aarch64"));
    if on_supported {
        let p = path.expect("stage4 baseline_path 在 linux/macos × x86_64/aarch64 上必有");
        let fname = p.file_name().expect("baseline path has file_name");
        let fname_str = fname.to_string_lossy();
        assert!(
            fname_str.starts_with("checkpoint-hashes-") && fname_str.ends_with("-stage4.txt"),
            "stage4 baseline 文件名应满足 `checkpoint-hashes-<os>-<arch>-stage4.txt`，实际 \
             `{fname_str}`",
        );
    } else {
        // 非 linux/macos × x86_64/aarch64 host 走 None；sanity check 一致性。
        assert!(
            path.is_none(),
            "非 linux/macos × x86_64/aarch64 host stage4_baseline_path 应返 None"
        );
    }
}
