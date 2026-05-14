//! E2-rev1 vultr SLO 双 fail 后的 single-thread hot path 诊断 bench
//! （E2-rev1-vultr-measured carve-out 段落 §修订历史 候选 (1)..(4) 决策依据）。
//!
//! 用 `Instant` 直接量各 hot path 100K 调用，得 ns/call 数字 → 摊算到单 update
//! 大约触发次数（DFS 深度 ~10-20 步 / postflop street ~5-10 次 lookup / showdown
//! ~10% × 1 eval call）→ 估算各组件占 single-thread 4357 update/s = 230 μs/update
//! budget 比例。
//!
//! **diagnostic-only**：本文件不做 SLO 断言，仅 eprintln 数字。E2-rev1-vultr-measured
//! carve-out closure 后由用户决策路径选完后撤回（继承 stage 2 §G-batch1 §3.10
//! diagnostic test 同型 carve-out 模式：测量 → 决策 → 撤回）。
//!
//! 触发：`cargo test --release --test e2_rev1_profile -- --ignored --nocapture`
//! 需 v3 artifact + Arc<BucketTable>。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use blake3::Hasher;
use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::sampling::sample_discrete;
use poker::training::{EsMccfrTrainer, RegretTable, Trainer};
use poker::{BucketTable, ChaCha20Rng};

const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

fn blake3_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn load_v3_or_skip() -> Option<Arc<BucketTable>> {
    let path = PathBuf::from(V3_ARTIFACT_PATH);
    if !path.exists() {
        eprintln!("skip: v3 artifact `{V3_ARTIFACT_PATH}` 不存在");
        return None;
    }
    let table = match BucketTable::open(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("skip: BucketTable::open 失败：{e:?}");
            return None;
        }
    };
    let body_hex = blake3_hex(&table.content_hash());
    if body_hex != V3_BODY_BLAKE3_HEX {
        eprintln!("skip: artifact body BLAKE3 不匹配 v3 ground truth");
        return None;
    }
    Some(Arc::new(table))
}

/// 把 ES-MCCFR 单 update 的 baseline cost 量出来。warm-up 100 update 后跑
/// `n` update，eprintln 总耗时 + 每 update ns / update/s。
#[test]
#[ignore = "diagnostic-only; release/--ignored opt-in"]
fn baseline_full_update_cost() {
    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let game = SimplifiedNlheGame::new(Arc::clone(&table)).expect("v3 game");
    let master_seed: u64 = 0xE2_E2_E2_E2_E2_E2_E2_E2;
    let mut trainer = EsMccfrTrainer::new(game, master_seed);
    let mut rng = ChaCha20Rng::from_seed(master_seed);

    // warm-up
    for _ in 0..100 {
        trainer.step(&mut rng).unwrap();
    }

    let n = 5_000;
    let t = Instant::now();
    for _ in 0..n {
        trainer.step(&mut rng).unwrap();
    }
    let elapsed = t.elapsed();
    eprintln!(
        "[profile] full_update: {n} update / {:.3} s = {:.0} update/s = {:.0} ns/update",
        elapsed.as_secs_f64(),
        n as f64 / elapsed.as_secs_f64(),
        elapsed.as_nanos() as f64 / n as f64
    );
}

/// 量 SimplifiedNlheState::clone 单次 cost（DFS recurse 每次 G::next 都 clone 全 state）。
#[test]
#[ignore = "diagnostic-only; release/--ignored opt-in"]
fn state_clone_cost() {
    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let game = SimplifiedNlheGame::new(Arc::clone(&table)).expect("v3 game");
    let master_seed: u64 = 0xC1_C1_C1_C1_C1_C1_C1_C1;
    let mut rng = ChaCha20Rng::from_seed(master_seed);
    // 深一点的 state（preflop deal + 几步 action history）
    let mut state: SimplifiedNlheState = game.root(&mut rng);
    let actions = SimplifiedNlheGame::legal_actions(&state);
    if !actions.is_empty() {
        state = SimplifiedNlheGame::next(state, actions[0], &mut rng);
    }

    let n = 1_000_000;
    let t = Instant::now();
    let mut sink = 0u64;
    for _ in 0..n {
        let s = std::hint::black_box(state.clone());
        sink = sink.wrapping_add(s.action_history.len() as u64);
    }
    let elapsed = t.elapsed();
    std::hint::black_box(sink);
    eprintln!(
        "[profile] state_clone: {n} clone / {:.3} s = {:.0} ns/clone",
        elapsed.as_secs_f64(),
        elapsed.as_nanos() as f64 / n as f64
    );
}

/// 量 G::next（含 state.clone + apply action）单次 cost。
#[test]
#[ignore = "diagnostic-only; release/--ignored opt-in"]
fn game_next_cost() {
    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let game = SimplifiedNlheGame::new(Arc::clone(&table)).expect("v3 game");
    let master_seed: u64 = 0xC2_C2_C2_C2_C2_C2_C2_C2;
    let mut rng = ChaCha20Rng::from_seed(master_seed);
    let mut state: SimplifiedNlheState = game.root(&mut rng);

    let n = 200_000;
    let t = Instant::now();
    for _ in 0..n {
        let actions = SimplifiedNlheGame::legal_actions(&state);
        if actions.is_empty() {
            // re-root 重新开局
            state = game.root(&mut rng);
            continue;
        }
        let next = SimplifiedNlheGame::next(state.clone(), actions[0], &mut rng);
        state = std::hint::black_box(next);
        if matches!(SimplifiedNlheGame::current(&state), NodeKind::Terminal) {
            state = game.root(&mut rng);
        }
    }
    let elapsed = t.elapsed();
    eprintln!(
        "[profile] game_next: {n} call / {:.3} s = {:.0} ns/call",
        elapsed.as_secs_f64(),
        elapsed.as_nanos() as f64 / n as f64
    );
}

/// 量 G::info_set（含 canonical_observation_id 重算 + bucket_table.lookup）单次 cost
/// on postflop state（最 expensive 的路径）。
#[test]
#[ignore = "diagnostic-only; release/--ignored opt-in"]
fn info_set_postflop_cost() {
    use poker::AbstractAction;

    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let game = SimplifiedNlheGame::new(Arc::clone(&table)).expect("v3 game");
    let master_seed: u64 = 0xC3_C3_C3_C3_C3_C3_C3_C3;
    let mut rng = ChaCha20Rng::from_seed(master_seed);

    // walk 到 postflop：每个 player turn 优先选 Check / Call（让对手不 fold），
    // 避开 Fold 让 hand 立刻 terminate；max 100 步 safety bound。
    let mut state: SimplifiedNlheState = game.root(&mut rng);
    let mut found_postflop = false;
    'outer: for _restart in 0..50 {
        for _step in 0..100 {
            match SimplifiedNlheGame::current(&state) {
                NodeKind::Terminal => {
                    state = game.root(&mut rng);
                    break;
                }
                NodeKind::Chance => {
                    let dist = SimplifiedNlheGame::chance_distribution(&state);
                    let action = sample_discrete(&dist, &mut rng);
                    state = SimplifiedNlheGame::next(state, action, &mut rng);
                }
                NodeKind::Player(_) => {
                    if !state.game_state.board().is_empty() {
                        found_postflop = true;
                        break 'outer;
                    }
                    let actions = SimplifiedNlheGame::legal_actions(&state);
                    if actions.is_empty() {
                        state = game.root(&mut rng);
                        break;
                    }
                    // 优先 Check / Call 推进；后备走 actions[0]
                    let pick = actions
                        .iter()
                        .copied()
                        .find(|a| {
                            matches!(a, AbstractAction::Check | AbstractAction::Call { .. })
                        })
                        .unwrap_or(actions[0]);
                    state = SimplifiedNlheGame::next(state, pick, &mut rng);
                }
            }
        }
        if found_postflop {
            break;
        }
        state = game.root(&mut rng);
    }

    if !found_postflop {
        eprintln!("[profile] info_set_postflop: 无法 walk 到 postflop（fixture issue），skip");
        return;
    }

    eprintln!(
        "[profile] info_set fixture: board.len = {}, action_history.len = {}",
        state.game_state.board().len(),
        state.action_history.len()
    );

    let n = 100_000;
    let t = Instant::now();
    let mut sink = 0u64;
    for _ in 0..n {
        let info = SimplifiedNlheGame::info_set(&state, 0);
        sink = sink.wrapping_add(info.raw());
    }
    let elapsed = t.elapsed();
    std::hint::black_box(sink);
    eprintln!(
        "[profile] info_set_postflop: {n} call / {:.3} s = {:.0} ns/call",
        elapsed.as_secs_f64(),
        elapsed.as_nanos() as f64 / n as f64
    );
}

/// 量 RegretTable::current_strategy_smallvec 单次 cost（5-action 典型）。
/// 用真实游戏 state 取一个 SimplifiedNlheInfoSet 模拟 trainer typical 调用。
#[test]
#[ignore = "diagnostic-only; release/--ignored opt-in"]
fn current_strategy_cost_5action() {
    use poker::training::nlhe::SimplifiedNlheInfoSet;

    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let game = SimplifiedNlheGame::new(Arc::clone(&table)).expect("v3 game");
    let mut rng = ChaCha20Rng::from_seed(0xC4_C4_C4_C4_C4_C4_C4_C4);
    let state: SimplifiedNlheState = game.root(&mut rng);
    let info: SimplifiedNlheInfoSet = SimplifiedNlheGame::info_set(&state, 0);
    let actions = SimplifiedNlheGame::legal_actions(&state);
    let n_actions = actions.len();

    let mut t: RegretTable<SimplifiedNlheInfoSet> = RegretTable::new();
    let regrets: Vec<f64> = (0..n_actions).map(|i| (i as f64) - 1.5).collect();
    t.accumulate(info, &regrets);

    let n = 1_000_000;
    let start = Instant::now();
    let mut sink = 0.0_f64;
    for _ in 0..n {
        let v = t.current_strategy(&info, n_actions);
        sink += v[0];
    }
    let elapsed = start.elapsed();
    std::hint::black_box(sink);
    eprintln!(
        "[profile] current_strategy_{n_actions}action: {n} call / {:.3} s = {:.0} ns/call",
        elapsed.as_secs_f64(),
        elapsed.as_nanos() as f64 / n as f64
    );
}

/// 量 sample_discrete 5-action 单次 cost。
#[test]
#[ignore = "diagnostic-only; release/--ignored opt-in"]
fn sample_discrete_5action_cost() {
    use poker::training::nlhe::SimplifiedNlheAction;
    use poker::abstraction::action::BetRatio;
    use poker::{AbstractAction, ChipAmount};

    let dist: Vec<(SimplifiedNlheAction, f64)> = vec![
        (AbstractAction::Fold, 0.2),
        (AbstractAction::Check, 0.1),
        (AbstractAction::Call { to: ChipAmount::new(100) }, 0.3),
        (
            AbstractAction::Bet {
                to: ChipAmount::new(500),
                ratio_label: BetRatio::HALF_POT,
            },
            0.2,
        ),
        (AbstractAction::AllIn { to: ChipAmount::new(10000) }, 0.2),
    ];
    let mut rng = ChaCha20Rng::from_seed(0xD1_D1_D1_D1_D1_D1_D1_D1);

    let n = 1_000_000;
    let t = Instant::now();
    let mut sink = 0u64;
    for _ in 0..n {
        let a = sample_discrete(&dist, &mut rng);
        sink = sink.wrapping_add(format!("{a:?}").len() as u64);
    }
    let elapsed = t.elapsed();
    std::hint::black_box(sink);
    eprintln!(
        "[profile] sample_discrete_5action: {n} call / {:.3} s = {:.0} ns/call",
        elapsed.as_secs_f64(),
        elapsed.as_nanos() as f64 / n as f64
    );
}

/// 量 BLAKE3 hasher（不是直接 hot path 但作为 ns/call 锚点参考）。
#[test]
#[ignore = "diagnostic-only; release/--ignored opt-in"]
fn blake3_hash_anchor_cost() {
    let n: u64 = 1_000_000;
    let t = Instant::now();
    let mut sink = 0u64;
    for i in 0u64..n {
        let mut h = Hasher::new();
        h.update(&i.to_le_bytes());
        let out: [u8; 32] = h.finalize().into();
        sink = sink.wrapping_add(out[0] as u64);
    }
    let elapsed = t.elapsed();
    std::hint::black_box(sink);
    eprintln!(
        "[profile] blake3_8byte_anchor: {n} call / {:.3} s = {:.0} ns/call",
        elapsed.as_secs_f64(),
        elapsed.as_nanos() as f64 / n as f64
    );
}
