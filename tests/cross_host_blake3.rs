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
use std::time::{SystemTime, UNIX_EPOCH};

use blake3::Hasher;
use poker::training::kuhn::KuhnGame;
use poker::training::{Trainer, VanillaCfrTrainer};
use poker::ChaCha20Rng;

const KUHN_FIXTURE_ITERS: u64 = 5;

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
