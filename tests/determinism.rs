//! C1：确定性测试（validation §6 / D-051）。
//!
//! 验收门槛：
//!
//! - 相同 seed 连续运行 10 次，每次输出的完整 hand history 哈希必须一致
//!   （即 same-thread / same-toolchain reproducibility）。
//! - 多线程批量模拟相同 seed 必产生与单线程一致的内容（线程乱序不影响内容）。
//! - 全程整数运算（u64/i64），无浮点。
//!
//! 跨架构（x86 vs ARM）一致性是 D-052 期望目标，本测试不强制；F3 报告中显式标注。
//!
//! 角色边界：本文件只读 hand history / state。

mod common;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread;

use poker::{
    Action, ChaCha20Rng, ChipAmount, GameState, HandHistory, LegalActionSet, RngSource, TableConfig,
};

use common::{expected_total_chips, Invariants};

// ============================================================================
// 共享：单手随机驱动（与 history_roundtrip / cross_lang_history 同一模板）
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
            to: range(min, max, rng),
        });
    }
    if let Some((min, max)) = la.raise_range {
        cands.push(Action::Raise {
            to: range(min, max, rng),
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

fn range(min: ChipAmount, max: ChipAmount, rng: &mut dyn RngSource) -> ChipAmount {
    let lo = min.as_u64();
    let hi = max.as_u64();
    if lo >= hi {
        return min;
    }
    ChipAmount::new(lo + rng.next_u64() % (hi - lo + 1))
}

// ============================================================================
// A. 同 seed 重复 10 次哈希一致
// ============================================================================

#[test]
fn same_seed_ten_runs_identical_hash() {
    // 选 20 个种子；每个种子运行 10 次必须 content_hash 完全相同。
    for seed in 0u64..20 {
        let mut hashes: HashSet<[u8; 32]> = HashSet::new();
        let mut bytes_set: HashSet<Vec<u8>> = HashSet::new();
        for _ in 0..10 {
            let h = play_random_hand(seed).unwrap_or_else(|e| panic!("seed={seed}: {e}"));
            hashes.insert(h.content_hash());
            bytes_set.insert(h.to_proto());
        }
        assert_eq!(
            hashes.len(),
            1,
            "(seed={seed}) hash drift across 10 runs: {} distinct hashes",
            hashes.len()
        );
        assert_eq!(
            bytes_set.len(),
            1,
            "(seed={seed}) proto bytes drift across 10 runs: {} distinct payloads",
            bytes_set.len()
        );
    }
}

// ============================================================================
// B. 单线程 vs 多线程批量一致性（D-051）
// ============================================================================
//
// 同一组 seed，单线程依次跑 / 多线程并行跑，每个 seed 的 content_hash 必须一致。

#[test]
fn multithread_batch_matches_singlethread() {
    let seeds: Vec<u64> = (0u64..200).map(|i| 0xC1_DE_77 + i).collect();

    // 单线程基线
    let mut single: HashMap<u64, [u8; 32]> = HashMap::new();
    for &s in &seeds {
        let h = play_random_hand(s).unwrap_or_else(|e| panic!("st seed={s}: {e}"));
        single.insert(s, h.content_hash());
    }

    // 4 线程并行（如果机器只有 1 核也没关系，thread::spawn 会 spawn）
    let chunks: Vec<Vec<u64>> = seeds
        .chunks(seeds.len().div_ceil(4))
        .map(|c| c.to_vec())
        .collect();
    let arc_chunks = Arc::new(chunks);
    let mut handles = Vec::new();
    for i in 0..arc_chunks.len() {
        let chunks = Arc::clone(&arc_chunks);
        handles.push(thread::spawn(move || {
            let mut local: HashMap<u64, [u8; 32]> = HashMap::new();
            for &s in &chunks[i] {
                let h = play_random_hand(s).unwrap_or_else(|e| panic!("mt seed={s}: {e}"));
                local.insert(s, h.content_hash());
            }
            local
        }));
    }
    let mut multi: HashMap<u64, [u8; 32]> = HashMap::new();
    for h in handles {
        let part = h.join().expect("thread panicked");
        for (k, v) in part {
            multi.insert(k, v);
        }
    }

    assert_eq!(
        single.len(),
        multi.len(),
        "single ({}) vs multi ({}) result count",
        single.len(),
        multi.len()
    );
    for (seed, st_hash) in &single {
        let mt_hash = multi.get(seed).expect("missing seed in mt");
        assert_eq!(
            st_hash, mt_hash,
            "(seed={seed}) single vs multi content_hash diverge"
        );
    }
}

// ============================================================================
// C. 不同 seed → 内容差异（sanity：seed 真的有效）
// ============================================================================

#[test]
fn different_seeds_produce_different_hashes() {
    // 起码 90% 的种子对应不同的哈希（极端情况下哈希碰撞理论可能但概率极低）。
    let mut hashes: HashMap<[u8; 32], u64> = HashMap::new();
    for seed in 0u64..200 {
        let h = play_random_hand(seed).unwrap();
        hashes.entry(h.content_hash()).or_insert(seed);
    }
    let unique_count = hashes.len();
    assert!(
        unique_count >= 180,
        "expected >= 180 distinct hashes, got {unique_count}"
    );
}

// ============================================================================
// D. proto bytes 字节稳定（PB-003）— 同 seed 多次 to_proto 必字节相同
// ============================================================================

// ============================================================================
// E. D1 出口：1M 手单线程 vs 多线程哈希一致（validation §6 / D-051）
// ============================================================================

/// D1 full-volume：1,000,000 个 seed 的 content_hash 在单线程 vs 8 线程下完全
/// 一致。每个 seed 独立产出的 hand history 必须与单线程下完全相同；整批结果
/// 只允许在 seed 顺序上不同，不允许在内容上不同（validation.md §6 line 4）。
///
/// 必须 release profile + `--ignored` 触发；debug 下耗时不可接受。
#[test]
#[ignore = "D1 full-volume — opt-in via cargo test --release -- --ignored"]
fn determinism_full_1m_hands_multithread_match() {
    const TOTAL: u64 = 1_000_000;
    const THREADS: usize = 8;

    // 单线程基线
    let mut single: HashMap<u64, [u8; 32]> = HashMap::with_capacity(TOTAL as usize);
    for s in 0..TOTAL {
        let h = play_random_hand(s).unwrap_or_else(|e| panic!("st seed={s}: {e}"));
        single.insert(s, h.content_hash());
    }

    // 8 线程并行：seed 区间均分，避免锁争用
    let chunk = TOTAL.div_ceil(THREADS as u64);
    let mut handles = Vec::with_capacity(THREADS);
    for i in 0..THREADS {
        let lo = (i as u64) * chunk;
        let hi = ((i as u64 + 1) * chunk).min(TOTAL);
        handles.push(thread::spawn(move || {
            let mut local: HashMap<u64, [u8; 32]> = HashMap::with_capacity((hi - lo) as usize);
            for s in lo..hi {
                let h = play_random_hand(s).unwrap_or_else(|e| panic!("mt seed={s}: {e}"));
                local.insert(s, h.content_hash());
            }
            local
        }));
    }
    let mut multi: HashMap<u64, [u8; 32]> = HashMap::with_capacity(TOTAL as usize);
    for h in handles {
        let part = h.join().expect("thread panicked");
        for (k, v) in part {
            multi.insert(k, v);
        }
    }

    eprintln!(
        "[determinism-1m] single={} multi={}",
        single.len(),
        multi.len()
    );
    assert_eq!(single.len() as u64, TOTAL);
    assert_eq!(multi.len() as u64, TOTAL);

    let mut diverged = Vec::new();
    for (seed, st_hash) in &single {
        let mt_hash = multi.get(seed).expect("missing seed in mt");
        if st_hash != mt_hash {
            diverged.push(*seed);
            if diverged.len() >= 5 {
                break;
            }
        }
    }
    assert!(
        diverged.is_empty(),
        "{} seeds diverge between single/multi (first up to 5: {:?})",
        diverged.len(),
        diverged
    );
}

#[test]
fn proto_bytes_byte_stable_under_repeated_serialization() {
    // 单一 hand 多次 to_proto() 必产出相同字节流。
    for seed in 0u64..30 {
        let h = play_random_hand(seed).unwrap();
        let b1 = h.to_proto();
        let b2 = h.to_proto();
        let b3 = h.to_proto();
        assert_eq!(b1, b2, "(seed={seed}) to_proto 不稳定（连续两次差异）");
        assert_eq!(b2, b3, "(seed={seed}) to_proto 三连不稳定");
    }
}
