//! C1：HandHistory 序列化 / 回放完整 roundtrip（API §5 + validation §5）。
//!
//! 验收门槛：
//!
//! - 随机生成 N 手牌，保存 hand history → 反序列化 → `replay()` 必须复现：
//!   board / hole_cards / final_payouts / showdown_order / content_hash 完全一致。
//! - 支持 `replay_to(action_index)` 任意 index 中间态恢复。
//! - schema 版本号在序列化中被携带；本测试不做版本兼容（留 F1）。
//!
//! 默认规模 1,000 手；`--ignored` 提供 100k 规模（workflow §C1 标线）。
//! 100k 在 naive evaluator 下约耗时数分钟；E2 之后跑得起。
//!
//! 角色边界：本文件只读 history / state；不修改产品代码。

mod common;

use std::collections::HashMap;

use poker::{
    Action, ChaCha20Rng, ChipAmount, GameState, HandHistory, LegalActionSet, RngSource, TableConfig,
};

use common::{expected_total_chips, Invariants};

// ============================================================================
// 单手随机驱动
// ============================================================================

fn play_random_hand(seed: u64) -> Result<(TableConfig, GameState), String> {
    let cfg = TableConfig::default_6max_100bb();
    let total = expected_total_chips(&cfg);
    let mut state = GameState::new(&cfg, seed);
    let mut rng = ChaCha20Rng::from_seed(seed.wrapping_add(0xC1_50));
    Invariants::check_all(&state, total)?;
    let max_actions = 256;
    for i in 0..max_actions {
        if state.is_terminal() {
            return Ok((cfg, state));
        }
        let la = state.legal_actions();
        let action =
            sample_action(&la, &mut rng).ok_or_else(|| format!("no legal action at index {i}"))?;
        state
            .apply(action)
            .map_err(|e| format!("apply #{i}: {e}"))?;
        Invariants::check_all(&state, total).map_err(|e| format!("invariant after #{i}: {e}"))?;
    }
    if !state.is_terminal() {
        return Err(format!(
            "did not terminate within {max_actions} actions (seed={seed})"
        ));
    }
    Ok((cfg, state))
}

fn sample_action(la: &LegalActionSet, rng: &mut dyn RngSource) -> Option<Action> {
    let mut candidates: Vec<Action> = Vec::with_capacity(6);
    if la.fold {
        candidates.push(Action::Fold);
    }
    if la.check {
        candidates.push(Action::Check);
    }
    if la.call.is_some() {
        candidates.push(Action::Call);
    }
    if let Some((min, max)) = la.bet_range {
        candidates.push(Action::Bet {
            to: random_in_range(min, max, rng),
        });
    }
    if let Some((min, max)) = la.raise_range {
        candidates.push(Action::Raise {
            to: random_in_range(min, max, rng),
        });
    }
    if la.all_in_amount.is_some() {
        candidates.push(Action::AllIn);
    }
    if candidates.is_empty() {
        return None;
    }
    Some(candidates[(rng.next_u64() as usize) % candidates.len()])
}

fn random_in_range(min: ChipAmount, max: ChipAmount, rng: &mut dyn RngSource) -> ChipAmount {
    let lo = min.as_u64();
    let hi = max.as_u64();
    if lo >= hi {
        return min;
    }
    ChipAmount::new(lo + rng.next_u64() % (hi - lo + 1))
}

// ============================================================================
// roundtrip 验证：to_proto → from_proto → replay → 比对
// ============================================================================

fn validate_roundtrip(seed: u64) -> Result<(), String> {
    let (_cfg, terminal_state) = play_random_hand(seed)?;
    let original = terminal_state.hand_history().clone();
    let bytes = original.to_proto();
    let decoded = HandHistory::from_proto(&bytes)
        .map_err(|e| format!("from_proto failed (seed={seed}): {e}"))?;

    // 字段一致性
    if decoded.schema_version != original.schema_version {
        return Err(format!(
            "schema_version mismatch: original={}, decoded={}",
            original.schema_version, decoded.schema_version
        ));
    }
    if decoded.seed != original.seed {
        return Err(format!(
            "seed mismatch: original={}, decoded={}",
            original.seed, decoded.seed
        ));
    }
    if decoded.actions.len() != original.actions.len() {
        return Err(format!(
            "actions len mismatch: original={}, decoded={}",
            original.actions.len(),
            decoded.actions.len()
        ));
    }
    if decoded.board != original.board {
        return Err("board mismatch".into());
    }
    if decoded.hole_cards != original.hole_cards {
        return Err("hole_cards mismatch".into());
    }
    if decoded.final_payouts != original.final_payouts {
        return Err("final_payouts mismatch".into());
    }
    if decoded.showdown_order != original.showdown_order {
        return Err("showdown_order mismatch".into());
    }

    // content_hash 必须稳定
    if decoded.content_hash() != original.content_hash() {
        return Err("content_hash differs after roundtrip".into());
    }

    // replay() 必须重建一致的终局
    let replayed = decoded
        .replay()
        .map_err(|e| format!("replay() failed (seed={seed}): {e}"))?;
    if replayed.board() != terminal_state.board() {
        return Err("replay board diverged".into());
    }
    if replayed.payouts() != terminal_state.payouts() {
        return Err("replay payouts diverged".into());
    }
    if !replayed.is_terminal() {
        return Err("replay terminal != true".into());
    }

    Ok(())
}

fn run_roundtrip_batch(samples: usize, base_seed: u64) -> HashMap<&'static str, usize> {
    let mut stats: HashMap<&'static str, usize> = HashMap::new();
    let mut first_failure: Option<(u64, String)> = None;
    for s in 0..samples as u64 {
        let seed = base_seed.wrapping_add(s);
        match validate_roundtrip(seed) {
            Ok(_) => *stats.entry("ok").or_insert(0) += 1,
            Err(e) => {
                *stats.entry("fail").or_insert(0) += 1;
                if first_failure.is_none() {
                    first_failure = Some((seed, e));
                }
            }
        }
    }
    if let Some((seed, msg)) = first_failure {
        panic!("first roundtrip failure (seed={seed}): {msg}");
    }
    stats
}

#[test]
fn history_roundtrip_default_1k() {
    let stats = run_roundtrip_batch(1_000, 0xC1_DA_7A);
    eprintln!("[roundtrip-1k] {stats:?}");
    assert_eq!(*stats.get("ok").unwrap_or(&0), 1_000);
}

#[ignore = "C1 full-volume — opt-in via cargo test -- --ignored"]
#[test]
fn history_roundtrip_full_100k() {
    let stats = run_roundtrip_batch(100_000, 0xC1_DA_7A);
    eprintln!("[roundtrip-100k] {stats:?}");
    assert_eq!(*stats.get("ok").unwrap_or(&0), 100_000);
}

// ============================================================================
// replay_to(action_index) 中间态验证
// ============================================================================

#[test]
fn replay_to_intermediate_states() {
    // 对 50 个随机 seed，遍历所有 action_index 并验证：
    //   - replay_to(k) 不 panic
    //   - replay_to(k+1) 的 actions 数量 = k+1
    //   - replay_to(actions.len()) ≡ replay()
    //   - 中间态的 invariants 全部成立
    for seed in 0..50u64 {
        let (cfg, terminal_state) = play_random_hand(seed.wrapping_add(0x1F00))
            .unwrap_or_else(|e| panic!("play_random_hand seed={seed}: {e}"));
        let total = expected_total_chips(&cfg);
        let history = terminal_state.hand_history().clone();

        for k in 0..=history.actions.len() {
            let mid = history
                .replay_to(k)
                .unwrap_or_else(|e| panic!("replay_to({k}) failed (seed={seed}): {e}"));
            Invariants::check_all(&mid, total)
                .unwrap_or_else(|e| panic!("intermediate invariants k={k} (seed={seed}): {e}"));
        }
        // replay_to(actions.len()) 与 replay() 应等价
        let full = history.replay().unwrap();
        let from_to = history.replay_to(history.actions.len()).unwrap();
        assert_eq!(
            full.payouts(),
            from_to.payouts(),
            "(seed={seed}) replay vs replay_to(N) payouts"
        );
        assert_eq!(full.board(), from_to.board());
    }
}

#[test]
fn replay_to_out_of_range_returns_error() {
    let (_cfg, ts) = play_random_hand(7777).unwrap();
    let h = ts.hand_history().clone();
    let bad = h.replay_to(h.actions.len() + 5);
    assert!(
        bad.is_err(),
        "replay_to past-end must error, got Ok with {:?} actions",
        h.actions.len()
    );
}
