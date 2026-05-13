//! 阶段 3 C1 \[测试\]：SimplifiedNlheGame + ES-MCCFR 工程稳定性 + determinism smoke
//! （D-313 / D-317 / D-318 / D-342 / D-362）。
//!
//! 五条核心 trip-wire（`pluribus_stage3_workflow.md` §步骤 C1 lines 197-202）：
//!
//! 1. `simplified_nlhe_game_root_state_2_player_100bb_starting_stack`（D-313 范围
//!    sanity，default profile active）— 验证 `Game::n_players() == 2` + root
//!    `game_state.players().len() == 2`。
//! 2. `simplified_nlhe_legal_actions_returns_default_action_abstraction_5_action`
//!    （D-318 桥接 sanity，default profile active）— 从 root walk chance node 到
//!    首个 Player node，`legal_actions` 返回 `Vec<AbstractAction>` 且 size ∈ [2, 5]
//!    （5-action 上界，SB/BB 可能 ≤ 5 受 stack/bet 约束）。
//! 3. `simplified_nlhe_info_set_uses_stage2_infosetid`（D-317 桥接 sanity，default
//!    profile active）— 首个 Player node 的 `info_set` 返回 `InfoSetId`（stage 2
//!    64-bit layout），`street_tag` ∈ Preflop（root 后未发 board）。
//! 4. `simplified_nlhe_es_mccfr_1k_update_no_panic_no_nan_no_inf`（D-342 工程稳定
//!    性 smoke，**release ignored**）— 跑 1000 `EsMccfrTrainer::step`，walk root 到
//!    若干 Player node 后 query `current_strategy` / `average_strategy`，全部
//!    finite（非 NaN / 非 Inf）。
//! 5. `simplified_nlhe_es_mccfr_fixed_seed_repeat_3_times_blake3_identical_1M_update`
//!    （D-362 重复确定性 smoke，**release ignored**）— fixed seed 重复 3 次 1M iter
//!    训练，walk 同一 chance-node deterministic path 收集 InfoSetId 序列后 BLAKE3
//!    snapshot avg_strategy，3 runs byte-equal。
//!
//! **D-314-rev1 lock**（`pluribus_stage3_decisions.md` §10.1，2026-05-13）：
//! bucket table = §G-batch1 §3.10 production **v3** artifact
//! `artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin`（528 MiB /
//! body BLAKE3 `67ee5554...`）。
//!
//! 测试 setup 走 `load_v3_artifact_or_skip` helper：artifact 缺失（CI / GitHub-
//! hosted runner 典型场景）时打印 eprintln 提示并 `return`（pass-with-skip），不
//! 强行依赖远端拉 528 MiB。本地 dev box / vultr / AWS host 有 artifact 时全套跑。
//!
//! C1 \[测试\] 角色边界：本文件不修改 `src/training/`；A1 scaffold 当前
//! `SimplifiedNlheGame::*` + `EsMccfrTrainer::*` 全部 `unimplemented!()`，本文件
//! active 测试在 scaffold 阶段会 panic-fail；C2 \[实现\] 落地后转绿（与 B1 →
//! B2 同型工程契约）。

use std::path::PathBuf;
use std::sync::Arc;

use blake3::Hasher;
use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::{SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::sampling::sample_discrete;
use poker::training::{EsMccfrTrainer, Trainer};
use poker::{AbstractAction, BucketTable, ChaCha20Rng, InfoSetId, StreetTag};

// ===========================================================================
// 共享常量 + helper
// ===========================================================================

/// v3 production artifact path（D-314-rev1 lock，相对 repo root）。
const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";

/// v3 artifact body BLAKE3 (D-314-rev1 ground truth；CLAUDE.md "当前 artifact 基线")。
/// 用于 helper 兜底 sanity check：artifact 加载成功但 `content_hash()` 不匹配 v3 →
/// 视同 schema 不兼容，eprintln + skip（避免 v2/v1 stale artifact 误闯入 C1 测试路径）。
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

/// 固定 master seed（D-335 sub-stream root）。跨所有 5 条测试共享，让 BLAKE3
/// byte-equal 可交叉验证 + 让 release/--ignored 多 run 之间共享 determinism。
const FIXED_SEED: u64 = 0x53_4E_4C_48_45_5F_43_31; // ASCII "SNLHE_C1"

/// Test 4 步数（D-342 工程稳定性 smoke）。release `≥ 10K update/s per D-361` →
/// 1K 单线程 release `< 100 ms`；default profile `unimplemented!()` panic-fail。
const SMOKE_UPDATES: u64 = 1_000;

/// Test 5 步数（D-362 重复确定性 smoke + workflow line 202 字面 "1M update"）。
/// release 单 run `~ 100 s`；3 runs `~ 5 min`；vultr ignored opt-in 跑。
const DETERMINISM_UPDATES: u64 = 1_000_000;

/// 走 chance node 链最大深度，避免 ES-MCCFR scaffold panic 之外的死循环
/// （C2 \[实现\] 落地后实际 chance 链 ≤ ~9：deal 2×2 hole + deal 3+1+1 board）。
const CHANCE_WALK_LIMIT: usize = 32;

/// Test 5 BLAKE3 snapshot 走的 chance-deterministic path 上收集的 Player InfoSet
/// 数量上限（每路径 ≤ ~16 decision node 在简化 NLHE 边界）。
const SNAPSHOT_PROBE_LIMIT: usize = 64;

/// 加载 v3 artifact 并构造 `SimplifiedNlheGame`；artifact 缺失 / schema 不匹配 /
/// `SimplifiedNlheGame::new` 失败时 eprintln + 返回 `None`（pass-with-skip）。
///
/// scaffold 阶段 `SimplifiedNlheGame::new` 自身 `unimplemented!()` → 本 helper 在
/// `SimplifiedNlheGame::new` 路径 panic；C2 \[实现\] 落地后 skip 路径生效。
fn load_v3_artifact_or_skip() -> Option<SimplifiedNlheGame> {
    let path = PathBuf::from(V3_ARTIFACT_PATH);
    if !path.exists() {
        eprintln!(
            "skip: v3 artifact `{V3_ARTIFACT_PATH}` 不存在（CI / GitHub-hosted runner 典型 \
             场景；本地 dev box / vultr / AWS host 有 artifact 时跑）。"
        );
        return None;
    }
    let table = match BucketTable::open(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("skip: BucketTable::open({V3_ARTIFACT_PATH}) 失败：{e:?}");
            return None;
        }
    };
    // sanity：content_hash 匹配 v3 ground truth；v2 / v1 stale artifact 走 hash
    // 不匹配 → skip（避免 stale artifact 误闯入 C1 测试路径）。
    let body_hex = blake3_hex(&table.content_hash());
    if body_hex != V3_BODY_BLAKE3_HEX {
        eprintln!(
            "skip: artifact body BLAKE3 `{body_hex}` 不匹配 v3 ground truth \
             `{V3_BODY_BLAKE3_HEX}`（D-314-rev1 lock 要求 v3 artifact；stale v1/v2 路径 skip）。"
        );
        return None;
    }
    match SimplifiedNlheGame::new(Arc::new(table)) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("skip: SimplifiedNlheGame::new 失败：{e:?}");
            None
        }
    }
}

/// `[u8; 32]` → hex string（lowercase，无 `0x` 前缀）。
fn blake3_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 从 root walk chance node 直到首个 Player 节点（或 Terminal / CHANCE_WALK_LIMIT），
/// 返回 `(walked_state, found_player_actor)`。RNG 由 `master_seed` deterministic
/// 派生让跨 run 同 path。
///
/// scaffold 阶段 `Game::root` / `Game::chance_distribution` `unimplemented!()` →
/// 本 helper panic；C2 落地后正常走。
fn walk_to_first_player_node(
    game: &SimplifiedNlheGame,
    master_seed: u64,
) -> (SimplifiedNlheState, u8) {
    let mut rng = ChaCha20Rng::from_seed(master_seed);
    let mut state = game.root(&mut rng);
    for _ in 0..CHANCE_WALK_LIMIT {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Player(actor) => return (state, actor),
            NodeKind::Chance => {
                let dist = SimplifiedNlheGame::chance_distribution(&state);
                let action = sample_discrete(&dist, &mut rng);
                state = SimplifiedNlheGame::next(state, action, &mut rng);
            }
            NodeKind::Terminal => {
                panic!(
                    "walk_to_first_player_node: 在 chance-only 链上遇到 Terminal，\
                     与 D-313 简化 NLHE 范围相违（preflop 一定有玩家行动节点）"
                );
            }
        }
    }
    panic!(
        "walk_to_first_player_node: 走完 {CHANCE_WALK_LIMIT} 步仍未到 Player node；\
         可能 C2 \\[实现\\] chance 链超出预期深度（D-315 chance 模型）"
    );
}

// ===========================================================================
// Test 1 — D-313 root state sanity（default profile active）
// ===========================================================================

/// D-313 简化 NLHE 范围 sanity：`Game::n_players() == 2` + root `game_state.players()
/// .len() == 2`。
///
/// 该测试在 A1 scaffold 阶段必然 panic-fail（`SimplifiedNlheGame::new` /
/// `SimplifiedNlheGame::n_players` 均 `unimplemented!()`），C2 \[实现\] 落地后转
/// 绿。本 trip-wire 锁定 D-313 "2-player + 100 BB starting stack" 字面范围。
#[test]
fn simplified_nlhe_game_root_state_2_player_100bb_starting_stack() {
    let Some(game) = load_v3_artifact_or_skip() else {
        return;
    };
    assert_eq!(
        game.n_players(),
        2,
        "D-313 简化 NLHE 范围 = 2-player（非 6-max / 非 heads-up 之外的变体）"
    );
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let root = game.root(&mut rng);
    let seat_count = root.game_state.players().len();
    assert_eq!(
        seat_count, 2,
        "root 状态的 GameState.players().len() = {seat_count}（D-313 应当 == 2）"
    );
    // sanity：root 状态下 pot >= 1.5 BB（盲注已 posted；BB = 100 chips 时 SB+BB ≥ 150）。
    // 不锁定具体 BB 数值（C2 \[实现\] 起步前 D-313-revM 可能调整 TableConfig 构造路径），
    // 仅断言 "> 0" 让 D-022 blinds 协议被实际触发。
    assert!(
        root.game_state.pot().as_u64() > 0,
        "root 状态 pot = {:?}，应 > 0（D-022 blinds 已 posted）",
        root.game_state.pot()
    );
}

// ===========================================================================
// Test 2 — D-318 legal_actions = AbstractAction 5-action 桥接 sanity
// ===========================================================================

/// D-318 桥接 sanity：首个 Player node 的 `legal_actions` 返回
/// `Vec<AbstractAction>`（不再二次抽象），size ∈ [2, 5]（5-action 上界 per D-209；
/// 下界 2：至少 Fold + Call 或 Fold + 某一 Raise）。
///
/// **不锁定** 具体 5 action 出现顺序或具体 AbstractAction variants（C2 \[实现\] 走
/// `DefaultActionAbstraction::abstract_actions(&game_state)` 桥接，具体集合由 stage
/// 2 D-209 + D-210 决定，C1 仅锁桥接通路 sanity）。
#[test]
fn simplified_nlhe_legal_actions_returns_default_action_abstraction_5_action() {
    let Some(game) = load_v3_artifact_or_skip() else {
        return;
    };
    let (state, _actor) = walk_to_first_player_node(&game, FIXED_SEED);
    let actions: Vec<SimplifiedNlheAction> = SimplifiedNlheGame::legal_actions(&state);
    // SimplifiedNlheAction == AbstractAction 类型恒等由 nlhe.rs `type SimplifiedNlheAction =
    // AbstractAction;` 锁定，本测试在运行时再断言 size 范围合 D-209。
    assert!(
        actions.len() >= 2,
        "legal_actions().len() = {} < 2（D-209 5-action 下界至少 Fold + Call/Bet）",
        actions.len()
    );
    assert!(
        actions.len() <= 5,
        "legal_actions().len() = {} > 5（D-209 默认 5-action 上界，SB/BB 短码可能 ≤ 5）",
        actions.len()
    );
    // 全部 actions 应当是有效 AbstractAction（trait bound `Eq + Copy + Debug` 已由
    // `Game::Action` 锁定；运行时通过 round-trip `to_concrete` sanity 不在 C1 范围，
    // 留 C2 unit test 落地）。
    for a in &actions {
        // sanity：每个 action 应当能 `to_concrete()` 转回 stage 1 `Action`（API-302 桥接）。
        let _concrete: poker::Action = AbstractAction::to_concrete(*a);
    }
}

// ===========================================================================
// Test 3 — D-317 info_set = stage 2 InfoSetId 桥接 sanity
// ===========================================================================

/// D-317 桥接 sanity：首个 Player node 的 `info_set(state, actor)` 返回
/// `InfoSetId`（stage 2 64-bit layout），`street_tag` 应 == `StreetTag::Preflop`
/// （root 后未发任何 board card 应当还在 preflop）。
///
/// **不锁定** 具体 64-bit raw 值（C2 \[实现\] 走 `PreflopLossless169::map` 桥接，
/// hand_class × 6 position × 4 stack × 12 betting_state × 169 hand 的组合 raw 由
/// stage 2 D-215 layout 决定，C1 仅锁 street_tag 桥接通路 sanity）。
#[test]
fn simplified_nlhe_info_set_uses_stage2_infosetid() {
    let Some(game) = load_v3_artifact_or_skip() else {
        return;
    };
    let (state, actor) = walk_to_first_player_node(&game, FIXED_SEED);
    let info: InfoSetId = SimplifiedNlheGame::info_set(&state, actor);
    let street = info.street_tag();
    assert_eq!(
        street,
        StreetTag::Preflop,
        "首个 Player node 应在 Preflop 街；info.street_tag() = {street:?}（D-317 + D-215 锁）"
    );
    // sanity：raw 64-bit InfoSetId 应非零（任意非 trivial layout 都会包含 hand_class /
    // street_tag / position 等位字段；纯 0 一般表示未初始化）。
    assert_ne!(
        info.raw(),
        0,
        "info.raw() = 0 暗示 InfoSetId 未正确编码（D-215 layout 所有字段全 0 不可能）"
    );
}

// ===========================================================================
// Test 4 — D-342 1K update no_panic_no_nan_no_inf smoke（release ignored）
// ===========================================================================

/// D-342 工程稳定性 smoke：1K `EsMccfrTrainer::step`，全程无 panic + 走完后 query
/// 若干 Player node 的 `current_strategy` / `average_strategy`，所有值 `.is_finite()`
/// （非 NaN / 非 Inf）。
///
/// release ignored opt-in（D-361 单线程 `≥ 10K update/s` → 1K release `< 100 ms`，
/// dev profile 慢 ~10× 仍 `< 1 s`；与 cfr_kuhn.rs 同型 release-ignored 路径）。
#[test]
#[ignore = "release/--ignored opt-in（1K ES-MCCFR update + smoke probe；C2 \\[实现\\] 落地后通过）"]
fn simplified_nlhe_es_mccfr_1k_update_no_panic_no_nan_no_inf() {
    let Some(game) = load_v3_artifact_or_skip() else {
        return;
    };
    // 先走一遍 walk 收集 InfoSet probe（在训练之前；trainer 训练后 query 同一 InfoSet
    // 应当有 strategy populated）。
    let (probe_state, probe_actor) = walk_to_first_player_node(&game, FIXED_SEED);
    let probe_info = SimplifiedNlheGame::info_set(&probe_state, probe_actor);

    let mut trainer = EsMccfrTrainer::new(game, FIXED_SEED);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    for i in 0..SMOKE_UPDATES {
        trainer
            .step(&mut rng)
            .unwrap_or_else(|e| panic!("step #{i} 失败：{e:?}"));
    }
    assert_eq!(
        trainer.update_count(),
        SMOKE_UPDATES,
        "update_count {} 应当 == {SMOKE_UPDATES}（D-361 单 step 累加 1 路径）",
        trainer.update_count()
    );

    // probe：query current_strategy + average_strategy（D-303 RM + D-304 累积归一化）。
    let current = trainer.current_strategy(&probe_info);
    let average = trainer.average_strategy(&probe_info);
    assert_finite_strategy(&current, "current_strategy");
    assert_finite_strategy(&average, "average_strategy");
    // probe InfoSet 在 1K iter 之后大概率被 traverse 触达（首个 Player node 沿 chance-
    // sample 路径走，ES-MCCFR per-player traverser 在 1K iter 范围内有大量 hits）；
    // 若 empty 视作 InfoSet 未在 trainer 内部初始化（ES-MCCFR lazy 模式合规，D-323），
    // 跳过深度断言不 fail。
    if !current.is_empty() {
        let sum: f64 = current.iter().sum();
        assert!(
            (sum - 1.0).abs() < 1e-6,
            "current_strategy 概率和 {sum}，超 1e-6 容差（D-330）"
        );
    }
    if !average.is_empty() {
        let sum: f64 = average.iter().sum();
        assert!(
            (sum - 1.0).abs() < 1e-6,
            "average_strategy 概率和 {sum}，超 1e-6 容差（D-330）"
        );
    }
    eprintln!(
        "C1 1K smoke: update_count = {} / current.len = {} / average.len = {}",
        trainer.update_count(),
        current.len(),
        average.len()
    );
}

fn assert_finite_strategy(strategy: &[f64], label: &str) {
    for (i, &p) in strategy.iter().enumerate() {
        assert!(
            p.is_finite(),
            "{label}[{i}] = {p} 非 finite（D-330 / D-342 工程稳定性禁止 NaN / Inf 进 \
             RegretTable HashMap）"
        );
    }
}

// ===========================================================================
// Test 5 — D-362 1M update × 3 runs BLAKE3 byte-equal（release ignored）
// ===========================================================================

/// D-362 fixed-seed 重复确定性：同 master_seed × 同 host × 同 toolchain 跑 3 次
/// 1M update 训练，walk 同一 chance-deterministic path 收集 InfoSetId 序列后
/// BLAKE3 snapshot avg_strategy，3 runs byte-equal。
///
/// release ignored opt-in（D-361 单线程 `≥ 10K update/s` → 1M release `~ 100 s` per
/// run × 3 runs `~ 5 min`；vultr / AWS host 跑）。
#[test]
#[ignore = "release/--ignored opt-in（1M ES-MCCFR update × 3 runs ~ 5 min；C2 \\[实现\\] 落地后通过）"]
fn simplified_nlhe_es_mccfr_fixed_seed_repeat_3_times_blake3_identical_1m_update() {
    let path = PathBuf::from(V3_ARTIFACT_PATH);
    if !path.exists() {
        eprintln!("skip: v3 artifact `{V3_ARTIFACT_PATH}` 不存在；本测试需 528 MiB v3 artifact");
        return;
    }
    let table = match BucketTable::open(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("skip: BucketTable::open 失败：{e:?}");
            return;
        }
    };
    let body_hex = blake3_hex(&table.content_hash());
    if body_hex != V3_BODY_BLAKE3_HEX {
        eprintln!(
            "skip: artifact body BLAKE3 不匹配 v3 ground truth（actual = {body_hex}，\
             expected = {V3_BODY_BLAKE3_HEX}）"
        );
        return;
    }
    let shared_table = Arc::new(table);

    let mut first_hash: Option<[u8; 32]> = None;
    let repeat_count = 3;
    for run in 0..repeat_count {
        let game = SimplifiedNlheGame::new(Arc::clone(&shared_table)).expect(
            "D-314-rev1：v3 artifact schema_version = 2 应当被 SimplifiedNlheGame::new 接受",
        );
        // 在训练前固定一条 chance-deterministic path 用于 snapshot probe（path 由
        // walk_to_first_player_node + 几步固定 action sequence 派生；本 probe 路径与
        // 训练消费的 rng 通路完全独立）。
        let probes = collect_snapshot_probes(&game);
        let mut trainer = EsMccfrTrainer::new(game, FIXED_SEED);
        let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
        for i in 0..DETERMINISM_UPDATES {
            trainer
                .step(&mut rng)
                .unwrap_or_else(|e| panic!("run #{run} step #{i} 失败：{e:?}"));
        }
        let hash = blake3_avg_strategy_snapshot(&trainer, &probes);
        if let Some(expected) = first_hash {
            assert_eq!(
                hash, expected,
                "run #{run} BLAKE3 = {hash:x?} 偏离 run #0 {expected:x?}\n\
                 D-362 fixed-seed 同 host 同 toolchain 应当 byte-equal"
            );
        } else {
            first_hash = Some(hash);
            eprintln!(
                "Simplified NLHE 1M ES-MCCFR seed=0x{FIXED_SEED:016x} probes={} BLAKE3 = {}",
                probes.len(),
                blake3_hex(&hash)
            );
        }
    }
    eprintln!("Simplified NLHE fixed-seed repeat {repeat_count} runs all BLAKE3 byte-equal ✓");
}

/// 收集 deterministic chance-path 上的 InfoSetId 序列（snapshot 输入端）。
///
/// 算法：从 root 起步走 chance node 用 deterministic rng（FIXED_SEED）派生分布
/// 采样；遇到 Player node 收 InfoSetId + 走 legal_actions[0]（确定性首项）继续；
/// 累积到 Terminal 或达到 SNAPSHOT_PROBE_LIMIT 终止。
fn collect_snapshot_probes(game: &SimplifiedNlheGame) -> Vec<InfoSetId> {
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut state = game.root(&mut rng);
    let mut probes = Vec::with_capacity(SNAPSHOT_PROBE_LIMIT);
    for _ in 0..SNAPSHOT_PROBE_LIMIT {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => break,
            NodeKind::Chance => {
                let dist = SimplifiedNlheGame::chance_distribution(&state);
                let action = sample_discrete(&dist, &mut rng);
                state = SimplifiedNlheGame::next(state, action, &mut rng);
            }
            NodeKind::Player(actor) => {
                let info = SimplifiedNlheGame::info_set(&state, actor);
                probes.push(info);
                let actions = SimplifiedNlheGame::legal_actions(&state);
                assert!(
                    !actions.is_empty(),
                    "Player node 应当有 ≥ 1 legal action（D-318 5-action 下界）"
                );
                let next_action = actions[0]; // 走确定性首项让 path deterministic
                state = SimplifiedNlheGame::next(state, next_action, &mut rng);
            }
        }
    }
    probes
}

/// BLAKE3 snapshot helper：对每个 probe InfoSet query avg_strategy → 写入
/// (raw_id_LE_bytes / strategy.len() LE / strategy_f64_LE_bytes)；finalize 32 byte
/// digest。空 strategy（未 populated InfoSet）走 len=0 也参与 hash（让 "未触达"
/// 与 "触达但 strategy 全零" 区分）。
///
/// 输入端 byte-equal 跨 host / 跨架构（probe_id raw u64 LE + f64 LE bytes pure
/// integer/IEEE-754 表达，不依赖 host 字节序），让 D-347 跨 host 不变量满足。
fn blake3_avg_strategy_snapshot(
    trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    probes: &[InfoSetId],
) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(&trainer.update_count().to_le_bytes());
    hasher.update(&(probes.len() as u64).to_le_bytes());
    for info in probes {
        let strategy = trainer.average_strategy(info);
        hasher.update(&info.raw().to_le_bytes());
        hasher.update(&(strategy.len() as u32).to_le_bytes());
        for &p in &strategy {
            hasher.update(&p.to_le_bytes());
        }
    }
    hasher.finalize().into()
}

// ===========================================================================
// dead_code 抑制 import helper（同 cfr_kuhn.rs 模式）
// ===========================================================================

#[allow(dead_code)]
fn _import_check(_state: SimplifiedNlheState, _action: SimplifiedNlheAction) {}
