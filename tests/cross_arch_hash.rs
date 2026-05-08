//! D1：跨架构 hash baseline（validation §6 / D-051 / D-052）。
//!
//! 目的：验证「相同 seed + 相同 toolchain → 同一 BLAKE3 哈希」(D-051)，并提供
//! 跨架构 (x86_64 vs aarch64) 的可比 baseline 文件以让 D-052 期望目标可被人工
//! 验证。
//!
//! 设计：
//!
//! - 选定 32 个固定 seed（覆盖小数 / 大数 / 边界 / 0）。
//! - 每个 seed 走完整随机一手，输出 `HandHistory.content_hash()` 的十六进制。
//! - 在 `tests/data/arch-hashes-<os>-<arch>.txt` 维护 baseline 文件：每行
//!   `seed=<dec> hash=<hex>`。
//! - 测试在已有 baseline 的 (os, arch) 上跑：必须与 baseline byte-equal。
//! - 没有 baseline 时打印当前 (os, arch) 的输出并通过（`#[ignore]` 显式 capture
//!   流程：`scripts/capture-arch-hashes.sh`）。
//!
//! D-052 跨架构验证流程：
//!
//! 1. Linux x86_64 由 CI 跑该测试（baseline `tests/data/arch-hashes-linux-x86_64.txt`
//!    随仓库 commit）。
//! 2. macOS arm64 由开发者本机跑 `scripts/capture-arch-hashes.sh`，产出
//!    `tests/data/arch-hashes-darwin-aarch64.txt`，commit。
//! 3. F3 验收报告比较两份 baseline；一致则 D-052 达成。

mod common;

use std::fs;
use std::path::PathBuf;

use poker::{
    Action, ChaCha20Rng, ChipAmount, GameState, HandHistory, LegalActionSet, RngSource, TableConfig,
};

use common::{expected_total_chips, Invariants};

/// 32 个固定 seed：0、1、2... 加上若干常用魔数（D-028 / D-029 / chacha20 测试惯
/// 用值），覆盖小 / 大 / 边界。
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

/// 把每个 seed 的 hex hash 收成 N 行 `seed=<dec> hash=<hex>` 字符串。
fn capture_baseline() -> String {
    let mut lines = String::new();
    for seed in ARCH_BASELINE_SEEDS {
        let h = play_random_hand(seed).unwrap_or_else(|e| panic!("baseline seed={seed}: {e}"));
        let hash = h.content_hash();
        let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        lines.push_str(&format!("seed={} hash={}\n", seed, hex));
    }
    lines
}

fn baseline_path() -> Option<PathBuf> {
    // 仅在已知 (os, arch) 上有 baseline；其它组合返回 None，测试退化为 capture-only。
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
    let path = PathBuf::from(manifest)
        .join("tests")
        .join("data")
        .join(format!("arch-hashes-{}-{}.txt", os, arch));
    Some(path)
}

#[test]
fn cross_arch_hash_matches_baseline() {
    let actual = capture_baseline();
    let path = match baseline_path() {
        Some(p) => p,
        None => {
            eprintln!(
                "[cross-arch] no baseline declared for this (os, arch); current capture:\n{}",
                actual
            );
            return;
        }
    };
    let expected = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "[cross-arch] baseline missing at {}: {}\n\
                 Run scripts/capture-arch-hashes.sh on this host to seed it.\n\
                 current capture:\n{}",
                path.display(),
                e,
                actual
            );
            return;
        }
    };
    if actual.trim() != expected.trim() {
        // 最多打印 5 条不一致行
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
            "cross-arch hash baseline drift at {}:\n{}\n",
            path.display(),
            diff.join("\n")
        );
    }
}

/// 显式 capture-only 入口：开发者跑 `cargo test --release --test cross_arch_hash
/// cross_arch_hash_capture_only -- --ignored --nocapture` 把当前 host 的输出 dump
/// 到 stdout，供 `scripts/capture-arch-hashes.sh` 重定向写入 baseline 文件。
#[test]
#[ignore = "capture-only entry point — invoked by scripts/capture-arch-hashes.sh"]
fn cross_arch_hash_capture_only() {
    let out = capture_baseline();
    print!("{}", out);
}

/// D-052 跨架构 32-seed 样本一致性 regression guard（D1 [测试]）。
///
/// 直接比较 `tests/data/arch-hashes-linux-x86_64.txt` 与
/// `tests/data/arch-hashes-darwin-aarch64.txt` 两份 baseline 的字节内容；不依赖
/// 当前 host 架构，所以在任意 (os, arch) 上跑都成立。两份文件都存在且 byte-equal
/// 时通过；只要有一份缺失 → eprintln 跳过（不算失败，validation §6 要求「文档
/// 标注当前是否达到」即可）；都存在但 byte-diff → fail，把前 5 行差异 panic。
///
/// 当前状态（2026-05-08，D1 commit）：32 seeds 样本 byte-equal，**32-seed 样本
/// 上跨架构一致达成**；validation §6 / D-052 字面仍是 「期望目标」，本测试不擅
/// 自把它升级为「必过门槛」，只作 lasting regression guard：未来 toolchain 升级
/// / refactor 引入跨架构漂移时第一时间在 CI 触发。
#[test]
fn cross_arch_baselines_byte_equal_when_both_present() {
    let manifest = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[cross-arch-pair] CARGO_MANIFEST_DIR unset; skip");
            return;
        }
    };
    let linux = PathBuf::from(&manifest)
        .join("tests")
        .join("data")
        .join("arch-hashes-linux-x86_64.txt");
    let darwin = PathBuf::from(&manifest)
        .join("tests")
        .join("data")
        .join("arch-hashes-darwin-aarch64.txt");
    let (a, b) = match (fs::read_to_string(&linux), fs::read_to_string(&darwin)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            eprintln!(
                "[cross-arch-pair] one or both baselines missing; skip (linux={} darwin={})",
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
        "D-052 regression: linux-x86_64 vs darwin-aarch64 baselines diverge:\n{}\n",
        diff.join("\n")
    );
}

// ============================================================================
// 共享：随机一手驱动（与 determinism.rs / history_roundtrip.rs 同模板）
// ============================================================================

fn play_random_hand(seed: u64) -> Result<HandHistory, String> {
    let cfg = TableConfig::default_6max_100bb();
    let total = expected_total_chips(&cfg);
    let mut state = GameState::new(&cfg, seed);
    let mut rng = ChaCha20Rng::from_seed(seed.wrapping_add(0xDE7E));
    Invariants::check_all(&state, total)?;
    for _ in 0..256 {
        if state.is_terminal() {
            break;
        }
        let la = state.legal_actions();
        let a = sample(&la, &mut rng).ok_or("no legal action")?;
        state.apply(a).map_err(|e| format!("apply: {e}"))?;
        Invariants::check_all(&state, total)?;
    }
    if !state.is_terminal() {
        return Err(format!("non-terminal seed={seed}"));
    }
    Ok(state.hand_history().clone())
}

fn sample(la: &LegalActionSet, rng: &mut dyn RngSource) -> Option<Action> {
    let mut cands: Vec<Action> = Vec::with_capacity(6);
    if la.fold {
        cands.push(Action::Fold);
    }
    if la.check {
        cands.push(Action::Check);
    }
    if la.call.is_some() {
        cands.push(Action::Call);
    }
    if let Some((min, max)) = la.bet_range {
        cands.push(Action::Bet {
            to: range_pick(min, max, rng),
        });
    }
    if let Some((min, max)) = la.raise_range {
        cands.push(Action::Raise {
            to: range_pick(min, max, rng),
        });
    }
    if la.all_in_amount.is_some() {
        cands.push(Action::AllIn);
    }
    if cands.is_empty() {
        return None;
    }
    Some(cands[(rng.next_u64() as usize) % cands.len()])
}

fn range_pick(min: ChipAmount, max: ChipAmount, rng: &mut dyn RngSource) -> ChipAmount {
    let lo = min.as_u64();
    let hi = max.as_u64();
    if lo >= hi {
        return min;
    }
    ChipAmount::new(lo + rng.next_u64() % (hi - lo + 1))
}
