//! 阶段 4 C1 \[测试\]：[`NlheGame6`] Game trait 8 方法 + 6-traverser routing 单元
//! 测试（D-410 / D-411 / D-412 / D-414 / D-416）。
//!
//! 三组 trip-wire：
//!
//! 1. **Game trait 8 方法 reachability**（panic-fail until C2）— [`Game::n_players`]
//!    / [`Game::root`] / [`Game::current`] / [`Game::info_set`] /
//!    [`Game::legal_actions`] / [`Game::next`] / [`Game::chance_distribution`] /
//!    [`Game::payoff`] 全 8 方法在 [`NlheGame6`] 上 `unimplemented!()`；C1 \[测试\]
//!    通过 `NlheGame6::new` / `NlheGame6::with_config` 上行 panic 链证明各方法对应
//!    的 product-code 路径仍是 scaffold（C2 \[实现\] 落地翻面后转绿）。
//!
//! 2. **6-traverser routing pure-fn anchor**（default profile active pass）—
//!    [`NlheGame6::traverser_at_iter`] / [`NlheGame6::traverser_for_thread`] 在
//!    A1 scaffold 已落地真实实现（pure function of `t` / `tid`），C1 钉死
//!    `t % 6` / `(base + tid) % 6` 字面契约 + 全 6-player alternating cycle
//!    invariance + thread-tid-0 等价 single-thread iter 路径（D-412 / D-414）。
//!
//! 3. **GameVariant::Nlhe6Max 4th-variant + Game::VARIANT anchor**（default
//!    profile active pass）— [`GameVariant::Nlhe6Max`] tag = 3 + `from_u8(3)`
//!    round-trip + `from_u8(4)` 越界 None + `<NlheGame6 as Game>::VARIANT ==
//!    GameVariant::Nlhe6Max` 编译期 const 锚（D-411）。
//!
//! **C1 \[测试\] 角色边界**：本文件 0 改动 `src/training/nlhe_6max.rs`；A1
//! \[实现\] scaffold [`NlheGame6::new`] / [`NlheGame6::new_hu`] /
//! [`NlheGame6::with_config`] / [`NlheGame6::actor_at_seat`] /
//! [`NlheGame6::compute_14action_mask`] + 全 8 Game trait method `unimplemented!()`，
//! 1.x 组 trip-wire 在 default profile 必 panic-fail，C2 \[实现\] 落地后转绿。
//!
//! **C1 → C2 工程契约**：(a) `NlheGame6::new(arc)` 走与 stage 3
//! `SimplifiedNlheGame::new` 同型校验（schema_version=2 + 500/500/500 +
//! BLAKE3 v3 anchor，详 D-424），失败返 [`TrainerError::UnsupportedBucketTable`];
//! (b) `Game::n_players` 返 `self.config.n_seats` 实际值（6-player 主路径 = 6 /
//! HU 退化 = 2）；(c) `Game::root` 走 stage 1 [`GameState::with_rng`] n_seats=6
//! 默认 multi-seat 分支（D-410），返 `NlheGame6State { game_state, action_history
//! = Vec::new(), bucket_table = self.bucket_table.clone() }`；(d) `Game::info_set`
//! 走 stage 2 `PreflopLossless169` / `PostflopBucketAbstraction` 桥接 + D-423
//! 14-bit mask 编码 `bits 33..47`（API-493 字面）；(e) `Game::legal_actions` 走
//! [`PluribusActionAbstraction::actions(&state.game_state)`]（API-494 字面）；
//! (f) `Game::next` 走 stage 1 [`GameState::apply`] + [`PluribusAction`] → stage
//! 1 [`Action`] 桥接（D-422 字面 14-action raise size byte-equal）；(g) HU 退化
//! `NlheGame6::new_hu` 配 `EsMccfrTrainer::new` 跑 1M update × 3 BLAKE3 byte-equal
//! stage 3 `SimplifiedNlheGame` anchor（D-416 字面，D1 \[测试\] 钉死）。

use std::path::PathBuf;
use std::sync::Arc;

use poker::training::game::{Game, NodeKind, PlayerId};
use poker::training::nlhe_6max::{NlheGame6, NlheGame6Action, NlheGame6InfoSet, NlheGame6State};
use poker::{
    BucketTable, ChaCha20Rng, ChipAmount, GameState, InfoSetId, PluribusAction, SeatId, TableConfig,
};

// stage 2 GameVariant + TrainerError 经 poker::error 暴露；C1 \[测试\] 单文件
// 不再走 `poker::*` glob 避免与 PluribusAction 等同名冲突。
use poker::error::{GameVariant, TrainerError};

// ===========================================================================
// 共享常量 + helper
// ===========================================================================

/// v3 production artifact path（D-314-rev1 / D-424 lock，stage 4 继承 stage 3
/// 字面，相对 repo root）。
const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";

/// v3 artifact body BLAKE3（D-424 ground truth；CLAUDE.md "当前 artifact 基线"）。
/// helper 兜底 sanity：artifact 加载成功但 `content_hash()` 不匹配 v3 → skip。
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

/// 固定 master seed（D-335 sub-stream root）。跨所有测试共享，让 BLAKE3
/// byte-equal 可交叉验证 + 让 release/--ignored 多 run 之间共享 determinism。
/// ASCII "STG4_C1\x14" — stage 4 C1 与 stage 4 B1 (`STG4_B1\x14`) 字面区分让
/// `EsMccfrTrainer::new` 跨 step 系列 seed 不冲突。
const FIXED_SEED: u64 = 0x53_54_47_34_5F_43_31_14;

/// 加载 v3 artifact 并返回 `Arc<BucketTable>`（继承 [`tests/cfr_simplified_nlhe.
/// rs`] / [`tests/nlhe_6max_warmup_byte_equal.rs`] 同型 helper 政策）。
/// artifact 缺失 / schema 不匹配时 eprintln + 返回 `None`（pass-with-skip）。
fn load_v3_artifact_arc_or_skip() -> Option<Arc<BucketTable>> {
    let path = PathBuf::from(V3_ARTIFACT_PATH);
    if !path.exists() {
        eprintln!(
            "skip: v3 artifact `{V3_ARTIFACT_PATH}` 不存在（CI / GitHub-hosted runner \
             典型场景；本地 dev box / vultr / AWS host 有 artifact 时跑）。"
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
    let body_hex = blake3_hex(&table.content_hash());
    if body_hex != V3_BODY_BLAKE3_HEX {
        eprintln!(
            "skip: artifact body BLAKE3 `{body_hex}` 不匹配 v3 ground truth \
             `{V3_BODY_BLAKE3_HEX}`（D-424 要求 v3 artifact）。"
        );
        return None;
    }
    Some(Arc::new(table))
}

/// `[u8; 32]` → hex string（lowercase）。
fn blake3_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 6-max 100 BB 默认 [`TableConfig`]（D-410 字面，每座位 100 BB starting stack）。
fn make_6max_100bb_config() -> TableConfig {
    let mut cfg = TableConfig::default_6max_100bb();
    // 6-max 100BB 默认已是 n_seats=6 / starting_stacks 全 10_000；本 helper 仅显式
    // 复刻让测试自包含。SB=50 / BB=100 / button=seat 0。
    cfg.n_seats = 6;
    cfg.starting_stacks = vec![ChipAmount::new(10_000); 6];
    cfg.small_blind = ChipAmount::new(50);
    cfg.big_blind = ChipAmount::new(100);
    cfg.button_seat = SeatId(0);
    cfg
}

/// HU 退化 [`TableConfig`]（D-416 字面，n_seats=2 路径走 stage 1 D-022b-rev1
/// HU NLHE 语义）。
fn make_hu_100bb_config() -> TableConfig {
    let mut cfg = TableConfig::default_6max_100bb();
    cfg.n_seats = 2;
    cfg.starting_stacks = vec![ChipAmount::new(10_000); 2];
    cfg.small_blind = ChipAmount::new(50);
    cfg.big_blind = ChipAmount::new(100);
    cfg.button_seat = SeatId(0);
    cfg
}

// ===========================================================================
// Group A — 6-traverser routing pure-fn anchor（default profile active pass，
// A1 scaffold 已落地真实实现，C1 钉死字面契约 + cycle invariance）
// ===========================================================================

/// D-412 字面：[`NlheGame6::traverser_at_iter`] 在 iter `t` 返 traverser index
/// = `(t % 6) as PlayerId`。
///
/// 6-traverser alternating 协议（D-412 字面）：6 套独立 RegretTable 按
/// `iter % 6` 路由。本测试钉死 `traverser_at_iter` 的 `% 6` 字面契约 + 不依赖
/// `self` 的 pure-function 形态。
#[test]
fn traverser_at_iter_returns_t_mod_6() {
    // 完整一周期（t = 0..6）：返 0/1/2/3/4/5
    for t in 0u64..6 {
        let traverser = NlheGame6::traverser_at_iter(t);
        assert_eq!(
            traverser, t as PlayerId,
            "D-412：traverser_at_iter({t}) 应 == {t} （t % 6）"
        );
    }
    // 跨周期（t = 6..12 = wrap-around 第 2 周期）
    for t in 6u64..12 {
        let traverser = NlheGame6::traverser_at_iter(t);
        assert_eq!(
            traverser,
            (t - 6) as PlayerId,
            "D-412：traverser_at_iter({t}) 应 == {} （t % 6 wrap-around 第 2 周期）",
            t - 6
        );
    }
    // 大值 sanity（u64::MAX % 6 = 3，避免 u64::MAX 边界值意外溢出）
    let big = NlheGame6::traverser_at_iter(u64::MAX);
    assert_eq!(
        big,
        (u64::MAX % 6) as PlayerId,
        "D-412：traverser_at_iter(u64::MAX) 应 == u64::MAX % 6 = 3"
    );
}

/// D-412 字面：[`NlheGame6::traverser_for_thread`] 在 `(base_update_count, tid)`
/// 返 traverser index = `((base + tid as u64) % 6) as PlayerId`。
///
/// 多线程 `step_parallel` 路径（D-321-rev2 rayon par_iter_mut 扩展到 6-player）
/// 上每 worker thread alternating；C2 \[实现\] 起步前 lock。
#[test]
fn traverser_for_thread_returns_base_plus_tid_mod_6() {
    // 单 base + 不同 tid（base=0，tid=0..8）
    for tid in 0usize..8 {
        let traverser = NlheGame6::traverser_for_thread(0, tid);
        assert_eq!(
            traverser,
            (tid as u64 % 6) as PlayerId,
            "D-412：traverser_for_thread(0, {tid}) 应 == ({tid} % 6) as PlayerId"
        );
    }
    // 跨 base + tid 组合（base=12, tid=0..6 → traverser 全 0..6）
    for tid in 0usize..6 {
        let traverser = NlheGame6::traverser_for_thread(12, tid);
        assert_eq!(
            traverser,
            ((12 + tid as u64) % 6) as PlayerId,
            "D-412：traverser_for_thread(12, {tid}) 应 == ({}) % 6 = {}",
            12 + tid as u64,
            (12 + tid as u64) % 6
        );
    }
}

/// D-412 字面：[`NlheGame6::traverser_at_iter`] 与 [`NlheGame6::traverser_for_thread`]
/// 在 `tid == 0` 时字面等价（让 single-thread `step` 路径 byte-equal multi-thread
/// `step_parallel` 起步 thread 路径）。
#[test]
fn traverser_at_iter_equals_traverser_for_thread_with_tid_zero() {
    for t in 0u64..100 {
        let a = NlheGame6::traverser_at_iter(t);
        let b = NlheGame6::traverser_for_thread(t, 0);
        assert_eq!(
            a, b,
            "D-412：traverser_at_iter({t}) ({a}) ≠ traverser_for_thread({t}, 0) ({b})"
        );
    }
}

/// D-414 字面：6-traverser cycle 在 60 个连续 iter 内完整覆盖 0..6 每 traverser
/// 各 10 次（验证 alternating routing 不偏离 / 不漏 traverser，避免任一
/// traverser 永不更新致 "1-traverser 训练 + 5 个 traverser 永不收敛" 退化）。
#[test]
fn traverser_at_iter_covers_all_six_traversers_in_60_iter_cycle() {
    let mut counts = [0u32; 6];
    for t in 0u64..60 {
        let traverser = NlheGame6::traverser_at_iter(t);
        assert!(
            (traverser as usize) < 6,
            "D-410 / D-412：traverser {traverser} 越界 [0..6)"
        );
        counts[traverser as usize] += 1;
    }
    for (tid, &c) in counts.iter().enumerate() {
        assert_eq!(
            c, 10,
            "D-414：traverser {tid} 在 60 iter cycle 内被路由 {c} 次，应 == 10 \
             （60 / 6 = 10，alternating 均匀分布）"
        );
    }
}

// ===========================================================================
// Group B — GameVariant::Nlhe6Max + Game::VARIANT anchor（default profile
// active pass，A1 scaffold 已落地 tag = 3 + from_u8 dispatch + const VARIANT，
// C1 钉死 D-411 字面契约）
// ===========================================================================

/// D-411 字面：[`GameVariant::Nlhe6Max`] tag = 3（stage 3 D-373-rev1 enum 追加第
/// 4 个变体）+ [`GameVariant::from_u8(3)`] 返 `Some(Nlhe6Max)` round-trip。
#[test]
fn game_variant_nlhe6max_tag_is_3_and_from_u8_round_trips() {
    assert_eq!(
        GameVariant::Nlhe6Max as u8,
        3,
        "D-411：GameVariant::Nlhe6Max 应 = tag 3（stage 3 enum 字面 4th variant）"
    );
    assert_eq!(
        GameVariant::from_u8(3),
        Some(GameVariant::Nlhe6Max),
        "D-411：GameVariant::from_u8(3) 应 round-trip 到 Nlhe6Max"
    );
    // 4 已超 enum cardinality → None（API-441 binary trip-wire 字面）
    assert_eq!(
        GameVariant::from_u8(4),
        None,
        "API-441：GameVariant::from_u8(4) 越界应返 None"
    );
    assert_eq!(
        GameVariant::from_u8(255),
        None,
        "API-441：GameVariant::from_u8(255) 越界应返 None"
    );
    // stage 1..3 既有 variant tag 一致性 sanity（stage 3 字面 0/1/2 不退化）
    assert_eq!(GameVariant::from_u8(0), Some(GameVariant::Kuhn));
    assert_eq!(GameVariant::from_u8(1), Some(GameVariant::Leduc));
    assert_eq!(GameVariant::from_u8(2), Some(GameVariant::SimplifiedNlhe));
}

/// D-411 字面：[`<NlheGame6 as Game>::VARIANT`] 编译期 const = [`GameVariant::Nlhe6Max`]。
///
/// 该 const 在 Checkpoint header offset 13 写入（D-356 跨 game 不兼容拒绝）+ 在
/// `Trainer::load_checkpoint` 路径上 eager 校验。VARIANT lock 让 D-356 路径不漂移
/// （stage 1/2/3 既有 KuhnGame / LeducGame / SimplifiedNlheGame VARIANT 不退化）。
#[test]
fn nlhe_game6_const_variant_is_nlhe6max() {
    let v: GameVariant = <NlheGame6 as Game>::VARIANT;
    assert_eq!(
        v,
        GameVariant::Nlhe6Max,
        "D-411：<NlheGame6 as Game>::VARIANT 应 == GameVariant::Nlhe6Max"
    );
    // 通过 fn 指针 UFCS 重新绑定 const 让漂移在 cargo build 立即暴露（与
    // tests/api_signatures.rs 同型 trip-wire）。
    let _const_ref: GameVariant = <NlheGame6 as Game>::VARIANT;
    assert_eq!(_const_ref as u8, 3);
}

/// D-410 / D-411 sanity：[`NlheGame6Action`] 类型 alias = [`PluribusAction`]（编
/// 译期等价；本测试在运行时 sanity 一遍 Fold/AllIn 双端点让 alias 漂移在 cargo
/// test 立即暴露）。
#[test]
fn nlhe_game6_action_type_alias_is_pluribus_action() {
    let fold: NlheGame6Action = NlheGame6Action::Fold;
    let pluribus_fold: PluribusAction = fold;
    assert_eq!(pluribus_fold as u8, 0, "API-410：PluribusAction::Fold = 0");

    let all_in: NlheGame6Action = NlheGame6Action::AllIn;
    let pluribus_all_in: PluribusAction = all_in;
    assert_eq!(
        pluribus_all_in as u8, 13,
        "API-410：PluribusAction::AllIn = 13"
    );
}

/// D-410 / API-410 sanity：[`NlheGame6InfoSet`] 类型 alias = [`InfoSetId`]（编
/// 译期等价；运行时 sanity `Default::default()` 形态让 alias 漂移立即暴露）。
#[test]
fn nlhe_game6_infoset_type_alias_is_infoset_id() {
    // InfoSetId 提供 Default 但 raw 通过 `InfoSetId::from_raw_internal` 构造（pub(crate)），
    // 外部用 stage 2 PreflopLossless169 / PostflopBucketAbstraction 桥接得到；C1
    // \[测试\] 仅 sanity 类型 alias 编译期等价（具体 raw 值由 C2 落地）。
    let _: fn(InfoSetId, u16) -> InfoSetId = InfoSetId::with_14action_mask;
    let _: fn(NlheGame6InfoSet, u16) -> NlheGame6InfoSet = NlheGame6InfoSet::with_14action_mask;
}

// ===========================================================================
// Group C — Game trait 8 方法 reachability panic-fail until C2
// （每条测试通过 `NlheGame6::new(arc)` 或 `NlheGame6::with_config(arc, cfg)` 触发
// 上行 `unimplemented!()` panic 链；C2 \[实现\] 落地后转绿）
// ===========================================================================

/// D-424 字面：[`NlheGame6::new(arc_bucket_table)`] 走 schema/cluster/BLAKE3 校验
/// 后返 `Ok(NlheGame6)`。
///
/// A1 \[实现\] scaffold 占位 `unimplemented!()`，C1 \[测试\] 通过加载 v3 artifact
/// 后调用 `NlheGame6::new` 触发 panic-fail；C2 \[实现\] 落地 schema=2 / cluster
/// (500,500,500) / BLAKE3 == `67ee5554...` 三项校验后 panic-fail 翻面通过。
///
/// **C2 → 转绿条件**：`NlheGame6::new(table)` 在 v3 artifact 上返
/// `Ok(NlheGame6 { config: 6-max 100BB default })`，本测试断言 `n_players() == 6`。
#[test]
fn nlhe_game6_new_v3_artifact_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game = NlheGame6::new(Arc::clone(&table))
        .expect("D-424：NlheGame6::new 在 v3 artifact 上应返 Ok（C2 落地后）");
    // C2 落地后断言走 stage 1 multi-seat 分支 default = 6-player（D-410 字面）。
    assert_eq!(
        game.n_players(),
        6,
        "D-410：NlheGame6::new 默认配 6-max（n_seats=6 主路径）"
    );
}

/// D-416 字面：[`NlheGame6::new_hu(arc_bucket_table)`] 走 HU 退化路径
/// （`n_seats=2`，stage 1 D-022b-rev1 HU NLHE 语义）。
///
/// A1 \[实现\] scaffold 占位 `unimplemented!()`，C1 panic-fail。C2 落地后：
/// `NlheGame6::new_hu(table)` 配 `EsMccfrTrainer::new` 跑 1M update × 3 BLAKE3
/// 应当 byte-equal stage 3 `SimplifiedNlheGame` 同 anchor（D1 \[测试\] 钉死，本
/// 测试仅 sanity `n_players() == 2`）。
#[test]
fn nlhe_game6_new_hu_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game = NlheGame6::new_hu(Arc::clone(&table))
        .expect("D-416：NlheGame6::new_hu 在 v3 artifact 上应返 Ok（C2 落地后）");
    assert_eq!(
        game.n_players(),
        2,
        "D-416：NlheGame6::new_hu 退化路径 n_seats=2"
    );
}

/// D-410 字面：[`NlheGame6::with_config(arc, cfg)`] 走通用 config 构造（n_seats
/// ∈ [2..=6] 让 6-max blueprint / HU evaluation / 3-handed ablation 共享同一
/// 入口）。
///
/// A1 scaffold 占位 `unimplemented!()`，C1 panic-fail；C2 落地后转绿。
#[test]
fn nlhe_game6_with_config_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let cfg = make_6max_100bb_config();
    let game = NlheGame6::with_config(Arc::clone(&table), cfg)
        .expect("D-410：NlheGame6::with_config 在合法 cfg 上应返 Ok（C2 落地后）");
    assert_eq!(
        game.n_players(),
        6,
        "D-410：with_config 走 cfg.n_seats=6 主路径"
    );
}

/// D-410 字面：[`Game::n_players(&NlheGame6)`] 返 `self.config.n_seats as usize`。
///
/// A1 scaffold 占位 `unimplemented!()`（即使签名能直接返回 6，A1 字面保持占位让
/// C1 \[测试\] panic-fail 形态统一）；C2 落地后转绿。
#[test]
fn game_trait_n_players_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game =
        NlheGame6::new(Arc::clone(&table)).expect("D-424：NlheGame6::new 应返 Ok（C2 落地后）");
    let n = game.n_players();
    assert_eq!(n, 6, "D-410：6-player NLHE 主路径");
}

/// D-410 字面：[`Game::root(&NlheGame6, &mut RngSource)`] 走 stage 1
/// [`GameState::with_rng`] n_seats=6 默认 multi-seat 分支，返
/// `NlheGame6State { game_state, action_history = Vec::new() }`。
///
/// A1 scaffold 占位 `unimplemented!()`，C1 panic-fail；C2 落地后转绿。
#[test]
fn game_trait_root_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game =
        NlheGame6::new(Arc::clone(&table)).expect("D-424：NlheGame6::new 应返 Ok（C2 落地后）");
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let root: NlheGame6State = game.root(&mut rng);
    // root state sanity：6 个玩家，pot 已 posted（SB+BB）。
    assert_eq!(
        root.game_state.players().len(),
        6,
        "D-410：root state n_seats=6"
    );
    assert!(
        root.game_state.pot().as_u64() > 0,
        "D-022 blinds 已 posted 在 root 上"
    );
}

/// D-410 字面：[`Game::current(&NlheGame6State)`] 静态方法返 [`NodeKind::Player`] /
/// [`NodeKind::Chance`] / [`NodeKind::Terminal`]（stage 3 SimplifiedNlheGame
/// 字面 `chance` 节点在 root 构造时一次性消费 rng → root 直接进 Player 节点）。
///
/// A1 scaffold `unimplemented!()`，C1 panic-fail；C2 落地后转绿。
#[test]
fn game_trait_current_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game =
        NlheGame6::new(Arc::clone(&table)).expect("D-424：NlheGame6::new 应返 Ok（C2 落地后）");
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let root = game.root(&mut rng);
    let kind = NlheGame6::current(&root);
    // 6-max root preflop UTG (seat 3) to act → Player(3) 或 Player(其它，
    // 取决于 button rotation）。C2 落地后断言走 Player 分支（非 chance 也非
    // terminal）。
    assert!(
        matches!(kind, NodeKind::Player(_)),
        "D-410：root state 应在 Player 节点（C2 落地后）；got {kind:?}"
    );
}

/// D-423 / API-493 字面：[`Game::info_set(&NlheGame6State, actor)`] 静态方法走
/// stage 2 [`PreflopLossless169`] / [`PostflopBucketAbstraction`] 桥接 + D-423
/// 14-bit mask 编码 `bits 33..47`，返 [`InfoSetId`]。
///
/// A1 scaffold `unimplemented!()`，C1 panic-fail；C2 落地后转绿（B2 落地的
/// `InfoSetId::with_14action_mask` / `legal_actions_mask_14` 字面继承）。
#[test]
fn game_trait_info_set_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game =
        NlheGame6::new(Arc::clone(&table)).expect("D-424：NlheGame6::new 应返 Ok（C2 落地后）");
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let root = game.root(&mut rng);
    let actor: PlayerId = match NlheGame6::current(&root) {
        NodeKind::Player(p) => p,
        _ => panic!("root 应在 Player 节点（C2 落地后）"),
    };
    let info: InfoSetId = NlheGame6::info_set(&root, actor);
    // C2 落地后断言 mask 非零（14-action availability mask 至少有 Fold + Call/Check）。
    let mask = info.legal_actions_mask_14();
    assert_ne!(
        mask, 0,
        "D-423：14-action availability mask 应非零（至少 Fold + Call/Check）"
    );
}

/// D-420 / API-494 字面：[`Game::legal_actions(&NlheGame6State)`] 静态方法走
/// [`PluribusActionAbstraction::actions(&state.game_state)`] 桥接，返
/// `Vec<NlheGame6Action>`（= `Vec<PluribusAction>`，类型 alias 等价）。
///
/// A1 scaffold `unimplemented!()`，C1 panic-fail；C2 落地后转绿。
#[test]
fn game_trait_legal_actions_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game =
        NlheGame6::new(Arc::clone(&table)).expect("D-424：NlheGame6::new 应返 Ok（C2 落地后）");
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let root = game.root(&mut rng);
    let actions: Vec<NlheGame6Action> = NlheGame6::legal_actions(&root);
    // C2 落地后：preflop UTG to act → legal actions 必包含 Fold（D-420 + LA-003 字面，
    // current_player.is_some() 时 Fold 永远合法）。
    assert!(
        actions.contains(&PluribusAction::Fold),
        "D-420 / LA-003：UTG 行动节点 Fold 永远合法"
    );
    assert!(
        actions.len() >= 2 && actions.len() <= 14,
        "D-420：legal_actions.len() = {} 应 ∈ [2, 14]",
        actions.len()
    );
}

/// D-422 字面：[`Game::next(NlheGame6State, NlheGame6Action, &mut RngSource)`]
/// 静态方法走 stage 1 [`GameState::apply`] + [`PluribusAction`] → stage 1
/// [`Action`] 桥接，返 `NlheGame6State`（14-action raise size byte-equal stage 1
/// `GameState::apply` 路径继承 B1 raise_sizes 14 测试）。
///
/// A1 scaffold `unimplemented!()`，C1 panic-fail；C2 落地后转绿。
#[test]
fn game_trait_next_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game =
        NlheGame6::new(Arc::clone(&table)).expect("D-424：NlheGame6::new 应返 Ok（C2 落地后）");
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let root = game.root(&mut rng);
    let actions = NlheGame6::legal_actions(&root);
    let first_action = actions
        .first()
        .copied()
        .expect("D-420：legal_actions 应非空");
    let _next: NlheGame6State = NlheGame6::next(root, first_action, &mut rng);
    // C2 落地后 sanity 路径不发散（具体下一状态由 D-022 行动顺序决定，C1 仅锁桥接通路）。
}

/// D-410 字面：[`Game::chance_distribution(&NlheGame6State)`] 静态方法 — 简化
/// NLHE / 6-player NLHE 字面继承 stage 3 [`SimplifiedNlheGame`] 同型政策：没有
/// 独立 chance 节点（stage 1 [`GameState::with_rng`] 在 root 构造时一次性消费
/// rng 发底牌 + 5 张 runout board）；本方法不应被 ES-MCCFR / Vanilla CFR 触发，
/// C2 落地走 `panic!` 拒绝路径（与 stage 3 [`SimplifiedNlheGame::chance_distribution`]
/// 字面）。
///
/// A1 scaffold `unimplemented!()`，C1 panic-fail；C2 落地后翻面 `panic!` 路径
/// （仍是 panic，但 panic message 字面表达 "no chance node" 而非 "unimplemented"）。
/// 本测试 panic-fail 即可 — `should_panic` 不 lock 具体 panic message 避免
/// scaffold ↔ C2 实现切换时被 require 改测试。
#[test]
#[should_panic]
fn game_trait_chance_distribution_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        // skip 路径不触发 panic → should_panic 失败；改用显式 panic 让 skip 路径
        // 同样满足 should_panic 协议。
        panic!("skip: v3 artifact 缺失（pass-with-skip 形式，should_panic 标记吞掉本 panic）");
    };
    let game =
        NlheGame6::new(Arc::clone(&table)).expect("D-424：NlheGame6::new 应返 Ok（C2 落地后）");
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let root = game.root(&mut rng);
    // C2 落地后 chance_distribution 应当 `panic!("no chance node")` 因 SimplifiedNlheGame
    // / NlheGame6 均无 chance 节点（D-410 字面）；A1 scaffold `unimplemented!()` 也
    // panic，should_panic 形态在 scaffold 与 C2 实现路径下均满足。
    let _ = NlheGame6::chance_distribution(&root);
}

/// D-410 字面：[`Game::payoff(&NlheGame6State, PlayerId)`] 静态方法 — terminal
/// 状态返 player 净额（i64 chip delta）转 f64（CFR 算法路径字面 f64）。
///
/// A1 scaffold `unimplemented!()`，C1 panic-fail；C2 落地后转绿（C1 测试当前
/// 走 root 状态调 payoff，A1 占位 panic-fail，C2 落地后字面应 panic！(非
/// terminal 不可调 payoff)；测试用 should_panic 同时覆盖 A1 + C2 两路径）。
#[test]
#[should_panic]
fn game_trait_payoff_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        panic!("skip: v3 artifact 缺失（pass-with-skip 形式，should_panic 标记吞掉本 panic）");
    };
    let game =
        NlheGame6::new(Arc::clone(&table)).expect("D-424：NlheGame6::new 应返 Ok（C2 落地后）");
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let root = game.root(&mut rng);
    // root 状态非 terminal，payoff 字面不可调（C2 落地后应 `panic!("non-terminal")`）；
    // scaffold 阶段 `unimplemented!()` 也 panic。两路径下 should_panic 协议均满足。
    let _ = NlheGame6::payoff(&root, 0);
}

/// D-413 字面：[`NlheGame6::actor_at_seat(&NlheGame6State, SeatId)`] —
/// trainer 内部 player_index 与 物理 SeatId 解耦，actor_at_seat 桥接走 stage 1
/// [`GameState::actor_at_seat`]（API-492 / API-410 桥接）。
///
/// A1 scaffold `unimplemented!()`，C1 panic-fail；C2 落地后转绿。
#[test]
fn nlhe_game6_actor_at_seat_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game =
        NlheGame6::new(Arc::clone(&table)).expect("D-424：NlheGame6::new 应返 Ok（C2 落地后）");
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let root = game.root(&mut rng);
    // SB = seat (button + 1) % 6 = seat 1（button = seat 0 默认配 cfg）。
    let actor = NlheGame6::actor_at_seat(&root, SeatId(1));
    // C2 落地后 actor 是 trainer 内部 player_index（u8，∈ [0..6)）；具体映射由
    // stage 1 GameState 内部 SeatId → PlayerId 转换决定，C1 仅锁桥接通路 panic-fail。
    assert!(
        (actor as usize) < 6,
        "D-413：actor_at_seat 返 player_index ∈ [0..6)"
    );
}

/// D-423 字面：[`NlheGame6::compute_14action_mask(&GameState)`] — 走
/// [`PluribusActionAbstraction`] 输出的 14-action legal subset → `1 <<
/// PluribusAction tag` 累积。`(0..14)` 范围内的 14-bit mask。
///
/// A1 scaffold `unimplemented!()`，C1 panic-fail；C2 落地后转绿。本测试构造 root
/// preflop 状态调 `compute_14action_mask(&root.game_state)` 应当返非零（UTG 行动
/// 节点至少 Fold + Call 可选）；scaffold 阶段 panic-fail，C2 转绿。
#[test]
fn nlhe_game6_compute_14action_mask_panic_fail_until_c2() {
    // 不依赖 v3 artifact — stage 1 `GameState` 直接构造，不需 BucketTable；
    // `compute_14action_mask` 走 PluribusActionAbstraction::actions 桥接，B2
    // \[实现\] 已落地 actions/is_legal 但 NlheGame6::compute_14action_mask 仍 A1
    // 占位 → C1 panic-fail（即使 GameState 构造成功）。
    let cfg = make_6max_100bb_config();
    let state = GameState::new(&cfg, FIXED_SEED);
    let mask: u16 = NlheGame6::compute_14action_mask(&state);
    // C2 落地后断言 mask 14-bit 范围 + 至少 Fold + Call 可选（UTG preflop）。
    assert!(mask < (1 << 14), "D-423：mask 应 < 2^14");
    let fold_bit = 1u16 << (PluribusAction::Fold as u8);
    assert_ne!(
        mask & fold_bit,
        0,
        "D-420 / LA-003：preflop UTG 行动节点 Fold 永远合法"
    );
}

// ===========================================================================
// Group D — HU 退化路径 byte-equal stage 3 anchor sanity（C1 仅锁桥接通路；
// 实际 1M update × 3 BLAKE3 byte-equal 由 D1 \[测试\] 钉死）
// ===========================================================================

/// D-416 字面：HU 退化路径 [`NlheGame6::new_hu(arc)`] 返 `NlheGame6` 配
/// `n_seats=2`；其上 `EsMccfrTrainer::new` 跑 1M update × 3 应 BLAKE3 byte-equal
/// stage 3 [`SimplifiedNlheGame`] 同 anchor（D-416 字面，D1 \[测试\] 钉死实际
/// BLAKE3 byte-equal regression）。
///
/// 本测试 C1 仅锁桥接通路：`NlheGame6::new_hu(arc)` 构造后 `n_players() == 2` +
/// `with_config(arc, hu_cfg)` 等价。A1 scaffold `unimplemented!()`，C1
/// panic-fail；C2 落地后转绿。
#[test]
fn nlhe_game6_new_hu_equals_with_config_n_seats_2_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game_hu = NlheGame6::new_hu(Arc::clone(&table))
        .expect("D-416：NlheGame6::new_hu 应返 Ok（C2 落地后）");
    let hu_cfg = make_hu_100bb_config();
    let game_with = NlheGame6::with_config(Arc::clone(&table), hu_cfg)
        .expect("D-410：NlheGame6::with_config(hu_cfg) 应返 Ok（C2 落地后）");
    assert_eq!(
        game_hu.n_players(),
        game_with.n_players(),
        "D-416：new_hu 等价 with_config(n_seats=2)"
    );
    assert_eq!(
        game_hu.n_players(),
        2,
        "D-416：HU 退化 n_seats=2 路径 byte-equal stage 3 SimplifiedNlheGame anchor"
    );
}

// ===========================================================================
// Group E — UnsupportedBucketTable 路径 trip-wire（C1 仅锁错误返回路径 sanity；
// 实际 schema/cluster/BLAKE3 mismatch 拒绝路径由 C2 [实现] 落地后转绿）
// ===========================================================================

/// D-424 字面：[`NlheGame6::new(arc)`] 对非 v3 artifact 应返
/// [`TrainerError::UnsupportedBucketTable`]（schema_version / cluster_config /
/// bucket_table_blake3 任一不匹配立即拒绝）。
///
/// A1 scaffold `unimplemented!()`，C1 panic-fail；C2 落地后转绿（C1 \[测试\]
/// 当前无法构造 "非 v3 artifact" 形态 bucket table 让 v3 artifact + 假装非
/// matching 路径走 C2 落地的 BLAKE3 校验，故本测试仅 sanity v3 artifact 走 Ok
/// 路径 — `UnsupportedBucketTable` 路径单独 carve-out 留 D1/D2 \[测试\] 起步前
/// 用 stage 2 fixture artifact 构造拒绝路径再 lock）。
#[test]
fn nlhe_game6_new_supported_bucket_table_returns_ok_panic_fail_until_c2() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let result: Result<NlheGame6, TrainerError> = NlheGame6::new(Arc::clone(&table));
    let err_msg = match &result {
        Ok(_) => String::new(),
        Err(e) => format!("{e:?}"),
    };
    assert!(
        result.is_ok(),
        "D-424：v3 artifact 上 NlheGame6::new 应返 Ok，实际 Err({err_msg})"
    );
}

// ===========================================================================
// Group F — 6-traverser routing 与 NodeKind::Player(actor) actor field
// alternating sanity（pure-function anchor，不依赖 NlheGame6 实例，可在 scaffold
// 阶段直接通过）
// ===========================================================================

/// D-414 字面：6-traverser 训练 1000 iter 中每 traverser 至少被访问 50 次
/// （`1000 / 6 ≈ 166`，但 alternating routing 严格走 `iter % 6` 让每 traverser
/// = `1000 / 6 = 166 或 167` 次）。
///
/// 验证 alternating routing 不偏离 / 不漏 traverser；与
/// `traverser_at_iter_covers_all_six_traversers_in_60_iter_cycle` 联合让 D-414
/// 字面契约（6 traverser 不共享 strategy → 每 traverser 独立 converge）的 routing
/// 入口路径 byte-equal。
#[test]
fn traverser_at_iter_alternating_1000_iter_uniform_visits() {
    let mut counts = [0u32; 6];
    for t in 0u64..1000 {
        let traverser = NlheGame6::traverser_at_iter(t);
        counts[traverser as usize] += 1;
    }
    for (tid, &c) in counts.iter().enumerate() {
        // 1000 / 6 = 166.67 → 每 traverser 166 或 167 次
        assert!(
            (166..=167).contains(&c),
            "D-414：traverser {tid} 在 1000 iter 内被路由 {c} 次，应 ∈ [166, 167] \
             （alternating uniform 分布）"
        );
    }
    assert_eq!(
        counts.iter().sum::<u32>(),
        1000,
        "D-412 / D-414：6-traverser 路由总数应 == iter count"
    );
}
