#![no_main]
//! D1：随机动作 fuzz target（workflow §D1 §输出 第 1/2 条）。
//!
//! 输入：任意 byte stream。前 8 字节解释为 u64 seed；其余被忽略（动作流由 seed
//! 驱动的 ChaCha20Rng 决定，与 `tests/fuzz_smoke.rs::run_one_hand` 同模板）。
//!
//! 验证：
//!
//! - GameState 路径不 panic
//! - 每步 apply 后筹码守恒（I-001 / pot = sum committed_total）
//! - 终局 payouts 零和（I-005）
//! - 重复牌检查（I-003）
//!
//! 角色边界：本 target 属 [测试]；任何 panic / invariant 违反由 cargo fuzz 写
//! crash artifact，由 D2 [实现] agent 修产品代码。

use libfuzzer_sys::fuzz_target;
use poker::{
    Action, ChaCha20Rng, ChipAmount, GameState, LegalActionSet, PlayerStatus, RngSource,
    TableConfig,
};
use std::collections::HashSet;

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let seed = u64::from_le_bytes(data[..8].try_into().expect("8 bytes"));
    let _ = run_one_hand(seed, 256);
});

fn run_one_hand(seed: u64, max_actions: usize) -> Result<(), String> {
    let cfg = TableConfig::default_6max_100bb();
    let total: u64 = cfg.starting_stacks.iter().map(|c| c.as_u64()).sum();
    let mut state = GameState::new(&cfg, seed);
    let mut rng = ChaCha20Rng::from_seed(seed.wrapping_add(0xDEAD_BEEF));

    check_invariants(&state, total)?;

    for i in 0..max_actions {
        if state.is_terminal() {
            check_terminal(&state)?;
            return Ok(());
        }
        let la = state.legal_actions();
        let Some(action) = sample_action(&la, &mut rng) else {
            return Err(format!("no legal action at step {i}"));
        };
        let cp = state
            .current_player()
            .ok_or_else(|| format!("current_player=None at step {i}"))?;
        let active = state
            .players()
            .iter()
            .find(|p| p.seat == cp)
            .map(|p| p.status == PlayerStatus::Active)
            .unwrap_or(false);
        if !active {
            return Err(format!("current_player {cp:?} not Active at step {i}"));
        }
        state
            .apply(action)
            .map_err(|e| format!("apply #{i} {action:?}: {e}"))?;
        check_invariants(&state, total).map_err(|e| format!("after #{i} {action:?}: {e}"))?;
    }
    if state.is_terminal() {
        check_terminal(&state)?;
    }
    Ok(())
}

fn check_invariants(state: &GameState, expected: u64) -> Result<(), String> {
    // I-001: chip conservation
    let stack_sum: u64 = state.players().iter().map(|p| p.stack.as_u64()).sum();
    let pot = state.pot().as_u64();
    if stack_sum + pot != expected {
        return Err(format!(
            "I-001: stack={stack_sum} + pot={pot} != {expected}"
        ));
    }
    // pot = sum committed_total
    let committed: u64 = state
        .players()
        .iter()
        .map(|p| p.committed_total.as_u64())
        .sum();
    if pot != committed {
        return Err(format!("pot={pot} != sum(committed)={committed}"));
    }
    // I-003: no duplicate cards
    let mut seen: HashSet<u8> = HashSet::new();
    for p in state.players() {
        if let Some([a, b]) = p.hole_cards {
            for c in [a, b] {
                if !seen.insert(c.to_u8()) {
                    return Err(format!("I-003: dup card {}", c.to_u8()));
                }
            }
        }
    }
    for c in state.board() {
        if !seen.insert(c.to_u8()) {
            return Err(format!("I-003: dup board {}", c.to_u8()));
        }
    }
    Ok(())
}

fn check_terminal(state: &GameState) -> Result<(), String> {
    let Some(payouts) = state.payouts() else {
        return Err("terminal but payouts=None".into());
    };
    let net_sum: i64 = payouts.iter().map(|(_, n)| n).sum();
    if net_sum != 0 {
        return Err(format!("I-005: payouts net_sum={net_sum}"));
    }
    Ok(())
}

fn sample_action(la: &LegalActionSet, rng: &mut dyn RngSource) -> Option<Action> {
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
