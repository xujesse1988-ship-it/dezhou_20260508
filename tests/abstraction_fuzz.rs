//! D1：abstraction fuzz 规模化测试（stage-2 workflow §D1 §输出 第 2 条）。
//!
//! 三组测试，与 §D1 §输出 line 374-377 字面一一对应：
//!
//! 1. `infoset_mapping_repeat_smoke` / `_full`：`PreflopLossless169::map(state, hole)`
//!    重复一致（IA-004 deterministic 不变量；100k smoke 默认 + 1M `#[ignore]` 完整版）
//! 2. `action_abstraction_config_random_raise_sizes_smoke` / `_full`：随机 1–14 raise
//!    size 量化后量化无重复时构造成功，`DefaultActionAbstraction::abstract_actions`
//!    输出确定性（10k smoke 默认 + 1M `#[ignore]`）
//! 3. `off_tree_real_bet_stability_smoke` / `_full`：`map_off_tree(state, real_to)`
//!    重复调用 byte-equal（D-201 PHM stub 占位实现层面；100k smoke + 1M `#[ignore]`）
//!
//! 与 `tests/info_id_encoding.rs::info_abs_determinism_repeat_smoke`（B1 1k smoke）
//! 互补：本文件 100k+ 跨随机 (state, hole) 输入维度，info_id_encoding 单 (state, hole)
//! 输入 1k 重复维度。两者覆盖 IA-004 不变量的不同切面。
//!
//! 与 `tests/action_abstraction.rs::action_abs_determinism_repeat_smoke`（B1 1k smoke）
//! 互补：本文件跨 1–14 raise size 配置维度，action_abstraction 单默认 5-action 配置
//! 维度。
//!
//! 角色边界：本文件属 `[测试]` agent 产物。任一断言被 [实现] 反驳由 [测试] agent
//! 修订（§D1 §出口 line 384 字面：暴露 1–3 corner case bug → 列入 issue 移交 D2）。

use poker::{
    ActionAbstraction, ActionAbstractionConfig, Card, ChaCha20Rng, ChipAmount, ConfigError,
    DefaultActionAbstraction, GameState, InfoAbstraction, PreflopLossless169, RngSource,
    TableConfig,
};

const SMOKE_ITER: usize = 100_000;
const FULL_ITER: usize = 1_000_000;

const CONFIG_SMOKE_ITER: usize = 10_000;
const CONFIG_FULL_ITER: usize = 1_000_000;

const OFFTREE_SMOKE_ITER: usize = 100_000;
const OFFTREE_FULL_ITER: usize = 1_000_000;

const FUZZ_MASTER_SEED: u64 = 0xD1FA_2257_1200_0001;

fn pick_distinct_pair(rng: &mut dyn RngSource) -> [Card; 2] {
    let mut deck: [u8; 52] = std::array::from_fn(|i| i as u8);
    let pick0 = (rng.next_u64() % 52) as usize;
    deck.swap(0, pick0);
    let pick1 = 1 + (rng.next_u64() % 51) as usize;
    deck.swap(1, pick1);
    [
        Card::from_u8(deck[0]).expect("0..52"),
        Card::from_u8(deck[1]).expect("0..52"),
    ]
}

fn fresh_state(seed: u64) -> GameState {
    let cfg = TableConfig::default_6max_100bb();
    GameState::new(&cfg, seed)
}

// ============================================================================
// 1. InfoSet mapping repeat（IA-004 deterministic）
// ============================================================================

fn infoset_mapping_repeat(iter: usize, label: &str) {
    let abs = PreflopLossless169::new();
    let mut rng = ChaCha20Rng::from_seed(FUZZ_MASTER_SEED);
    for i in 0..iter {
        let state_seed = rng.next_u64();
        let state = fresh_state(state_seed);
        let hole = pick_distinct_pair(&mut rng);
        let id1 = abs.map(&state, hole);
        let id2 = abs.map(&state, hole);
        assert_eq!(
            id1.raw(),
            id2.raw(),
            "{label} IA-004 iter {i}：repeat byte-equal break (state_seed={state_seed:#x}, hole={:?})",
            hole
        );
    }
}

#[test]
fn infoset_mapping_repeat_smoke() {
    infoset_mapping_repeat(SMOKE_ITER, "smoke 100k");
}

#[test]
#[ignore = "D1 full: 1M iter（release ~3 s 实测 / debug 远超），与 stage-1 1M determinism opt-in 同形态"]
fn infoset_mapping_repeat_full() {
    infoset_mapping_repeat(FULL_ITER, "full 1M");
}

// ============================================================================
// 2. Random ActionAbstractionConfig 1–14 raise sizes → 输出确定性
// ============================================================================

/// 生成 [0.05, 4.999] 范围内一个浮点 ratio（D-202-rev1 / BetRatio::from_f64-rev1
/// 量化后 milli ∈ [50, 4999]）。
fn random_ratio(rng: &mut dyn RngSource) -> f64 {
    let raw = (rng.next_u64() % 4_950 + 50) as f64; // [50, 4999]
    raw / 1_000.0
}

fn config_random_raise_sizes(iter: usize, label: &str) {
    let mut rng = ChaCha20Rng::from_seed(FUZZ_MASTER_SEED.wrapping_add(0xAB57_2400));
    let state = fresh_state(0x00DE_ADBE_EFC0_FFEE);
    let mut built = 0usize;
    let mut rejected = 0usize;
    for i in 0..iter {
        let n = (rng.next_u64() % 14 + 1) as usize; // 1..=14（D-202 字面）
        let raises: Vec<f64> = (0..n).map(|_| random_ratio(&mut rng)).collect();
        match ActionAbstractionConfig::new(raises.clone()) {
            Ok(cfg) => {
                built += 1;
                let aa = DefaultActionAbstraction::new(cfg);
                let s1 = aa.abstract_actions(&state);
                let s2 = aa.abstract_actions(&state);
                assert_eq!(
                    s1.as_slice(),
                    s2.as_slice(),
                    "{label} iter {i}: abstract_actions repeat 输出不一致 (raises={:?})",
                    raises
                );
                // AA-005 上界：输出 ≤ raise_count + 4（fold/check/call/AllIn）。
                assert!(
                    s1.len() <= n + 4,
                    "{label} iter {i}: AA-005 上界破坏 |out| = {} > raise_count {} + 4",
                    s1.len(),
                    n
                );
            }
            Err(e) => {
                rejected += 1;
                match e {
                    ConfigError::DuplicateRatio { .. }
                    | ConfigError::RaiseRatioInvalid(_)
                    | ConfigError::RaiseCountOutOfRange(_) => {}
                    ConfigError::BucketCountOutOfRange { .. } => panic!(
                        "{label} iter {i}: ActionAbstractionConfig::new 不应返回 BucketCountOutOfRange (raises={:?})",
                        raises
                    ),
                }
            }
        }
    }
    assert!(
        built > 0,
        "{label}: built=0 / rejected={rejected}，1–14 raise config 全 rejected"
    );
}

#[test]
fn action_abstraction_config_random_raise_sizes_smoke() {
    config_random_raise_sizes(CONFIG_SMOKE_ITER, "config-smoke 10k");
}

#[test]
#[ignore = "D1 full: 1M iter（release ~3 s 实测 / debug 远超），与 stage-1 1M determinism opt-in 同形态"]
fn action_abstraction_config_random_raise_sizes_full() {
    config_random_raise_sizes(CONFIG_FULL_ITER, "config-full 1M");
}

// ============================================================================
// 3. 100k 随机 off-tree real_bet → 抽象动作映射稳定（D-201 PHM stub）
// ============================================================================

fn off_tree_real_bet(iter: usize, label: &str) {
    let aa = DefaultActionAbstraction::default_5_action();
    let mut rng = ChaCha20Rng::from_seed(FUZZ_MASTER_SEED.wrapping_add(0x77CC_E000));
    let state = fresh_state(0x00C1_FACE_F00D_0000);
    for i in 0..iter {
        let real_to_raw = rng.next_u64() % 100_001;
        let real_to = ChipAmount::new(real_to_raw);
        let m1 = aa.map_off_tree(&state, real_to);
        let m2 = aa.map_off_tree(&state, real_to);
        assert_eq!(
            m1, m2,
            "{label} iter {i}: map_off_tree repeat 不一致 (real_to={real_to_raw})"
        );
    }
}

#[test]
#[ignore = "D2: D-201 PHM stub 占位实现待 D2 落地（src/abstraction/action.rs:379 当前 \
            unimplemented!()，§D1 §出口预期暴露 issue → 见 GitHub issue #8）"]
fn off_tree_real_bet_stability_smoke() {
    off_tree_real_bet(OFFTREE_SMOKE_ITER, "offtree-smoke 100k");
}

#[test]
#[ignore = "D2: D-201 PHM stub 占位实现待 D2 落地（同 _smoke ignore reason；D2 闭合后切到 \
            release --ignored opt-in，与 stage-1 1M determinism opt-in 同形态；issue #8）"]
fn off_tree_real_bet_stability_full() {
    off_tree_real_bet(OFFTREE_FULL_ITER, "offtree-full 1M");
}
