//! D1：跨架构 32-seed bucket table baseline regression guard（stage-2 workflow
//! §D1 §输出 第 3 条 / `tests/clustering_cross_host.rs`）。
//!
//! 与 stage-1 `tests/cross_arch_hash.rs::cross_arch_baselines_byte_equal_when_both_present`
//! 同形态：
//!
//! - 在 `CARGO_MANIFEST_DIR/tests/data/` 下查 `bucket-table-arch-hashes-{linux-x86_64,
//!   darwin-aarch64}.txt` 两个 baseline 文件。
//! - 都存在 → 断言 `lhs.trim() == rhs.trim()`，否则 panic 并打印前 5 行差异（D-052
//!   跨架构确定性 regression）。
//! - 任一缺失 → eprintln + return（skip 政策；validation §6 / D-052 字面仍是
//!   「期望目标」，本测试不擅自把它升级为「必过门槛」，仅作 lasting regression
//!   guard）。
//!
//! 当前状态（2026-05-10，D1 batch 1 commit）：
//!
//! - linux-x86_64 baseline 由 D1 batch 1 同 PR commit（issue #3 §出口 step 1，
//!   capture 训练成本 ~107 min release）。
//! - darwin-aarch64 baseline 缺失（未在 D1 落地 Mac runner / self-hosted；§C-rev2
//!   batch 6 carve-out / D-052 aspirational）。
//! - 当前路径：linux 存在 / darwin 缺失 → 本 test 走 "skip" 分支；darwin baseline
//!   补齐前不会 fail，但 toolchain / refactor 引入 linux baseline 漂移由
//!   `tests/clustering_determinism.rs::cross_arch_bucket_id_baseline` 抓住（同一
//!   host 自比对，不依赖 darwin 副本）。
//!
//! 角色边界：本文件属 `[测试]` agent 产物（与 stage-1 `cross_arch_hash.rs` 同
//! 形态）；任何跨架构差异由 [测试] agent 触发 D-NNN-revM / API-NNN-revM 流程，
//! 由 [实现] agent 修产品代码。

use std::fs;
use std::path::PathBuf;

#[test]
fn cross_arch_bucket_table_baselines_byte_equal_when_both_present() {
    let manifest = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[bucket-table-cross-arch-pair] CARGO_MANIFEST_DIR unset; skip");
            return;
        }
    };
    let linux = PathBuf::from(&manifest)
        .join("tests")
        .join("data")
        .join("bucket-table-arch-hashes-linux-x86_64.txt");
    let darwin = PathBuf::from(&manifest)
        .join("tests")
        .join("data")
        .join("bucket-table-arch-hashes-darwin-aarch64.txt");
    let (a, b) = match (fs::read_to_string(&linux), fs::read_to_string(&darwin)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            eprintln!(
                "[bucket-table-cross-arch-pair] one or both baselines missing; skip (linux={} darwin={})",
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
        "D-052 / stage-2 D1 regression: linux-x86_64 vs darwin-aarch64 bucket-table baselines \
         diverge:\n{}\n",
        diff.join("\n")
    );
}
