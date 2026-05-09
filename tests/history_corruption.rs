//! F1：corrupted history 错误路径测试（workflow §F1 第 2 件套）。
//!
//! 验收门槛（workflow §F1, validation §5 末行）：
//!
//! > corrupted history 必须返回明确错误，禁止静默截断或恢复出不一致状态。
//!
//! 「明确错误」= `Err(HistoryError::*)`；不允许 panic / OOM / 算术溢出 /
//! unwrap None 把进程拉爆。
//!
//! 本文件测三类输入：
//!
//! 1. **结构性 corrupted**（随机 byte flip / 截断 / 完全随机 garbage）：fuzz
//!    路径，断言 `from_proto` 返回 `Err` 或 `Ok` 后 round-trip 字节稳定。
//!    (D1 cargo-fuzz target `history_decode` 已覆盖 `from_proto` 不 panic 不
//!    变；本文件追加不依赖 fuzz 入口的批量化断言，给 CI 跑得起。)
//!
//! 2. **域违规**（field 值在合法 wire 但语义非法）：n_seats 越界、starting_stacks
//!    长度不匹配、card 索引越界、ActionKind / Street UNSPECIFIED、card 重复 ……
//!    `from_proto` 路径直接拒绝的项目落地为默认 `#[test]`；当前 stage-1 实现
//!    在 `from_proto` 时不严校验、需走 `replay()` 才报错的项目落地为
//!    `#[ignore = "F1 → F2 carry-over"]`，留给 F2 决定是否在 `from_proto`
//!    一次性挡掉（更早失败、更明确错误）还是保留 「from_proto 通过 + replay
//!    返回 HistoryError::Rule」 的现状。
//!
//! 3. **回放语义 corrupted**：board / hole_cards 字段不匹配 seed 重放，必须
//!    返回 `ReplayDiverged` 而不是静默成功。
//!
//! 角色边界：[测试]，不修改产品代码。攻击 bytes 由本文件用 prost 派生的
//! 镜像 wire 类型（与 `proto/hand_history.proto` 1:1 对应）构造，避免暴露
//! `src/history.rs` 私有 `mod proto`。镜像 schema 漂移由 `tests/cross_lang_history.rs`
//! 与 `tools/history_reader.py` 的同步策略覆盖（任一改动需同步本文件）。

use poker::{
    Action, ChaCha20Rng, ChipAmount, GameState, HandHistory, HistoryError, LegalActionSet,
    RngSource, TableConfig,
};
use prost::Message;

mod common;
use common::{expected_total_chips, Invariants};

// ============================================================================
// 镜像 wire 类型（仅本文件 = 攻击 bytes 构造器）
// ============================================================================
//
// 与 `src/history.rs::mod proto` 字段、tag、wire type 严格一致。
// 任何 .proto schema 改动必须同步：
//   - src/history.rs::mod proto
//   - tools/history_reader.py
//   - tests/history_corruption.rs::mirror（本节）
mod mirror {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct HandHistory {
        #[prost(uint32, tag = "1")]
        pub schema_version: u32,
        #[prost(message, optional, tag = "2")]
        pub config: Option<TableConfig>,
        #[prost(uint64, tag = "3")]
        pub seed: u64,
        #[prost(message, repeated, tag = "4")]
        pub actions: Vec<RecordedAction>,
        #[prost(uint32, repeated, tag = "5")]
        pub board: Vec<u32>,
        #[prost(message, repeated, tag = "6")]
        pub hole_cards: Vec<HoleCards>,
        #[prost(message, repeated, tag = "7")]
        pub final_payouts: Vec<Payout>,
        #[prost(uint32, repeated, tag = "8")]
        pub showdown_order: Vec<u32>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct TableConfig {
        #[prost(uint32, tag = "1")]
        pub n_seats: u32,
        #[prost(uint64, repeated, tag = "2")]
        pub starting_stacks: Vec<u64>,
        #[prost(uint64, tag = "3")]
        pub small_blind: u64,
        #[prost(uint64, tag = "4")]
        pub big_blind: u64,
        #[prost(uint64, tag = "5")]
        pub ante: u64,
        #[prost(uint32, tag = "6")]
        pub button_seat: u32,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct RecordedAction {
        #[prost(uint32, tag = "1")]
        pub seq: u32,
        #[prost(uint32, tag = "2")]
        pub seat: u32,
        #[prost(int32, tag = "3")]
        pub street: i32,
        #[prost(int32, tag = "4")]
        pub kind: i32,
        #[prost(uint64, tag = "5")]
        pub to: u64,
        #[prost(uint64, tag = "6")]
        pub committed_after: u64,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct HoleCards {
        #[prost(bool, tag = "1")]
        pub present: bool,
        #[prost(uint32, tag = "2")]
        pub c0: u32,
        #[prost(uint32, tag = "3")]
        pub c1: u32,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Payout {
        #[prost(uint32, tag = "1")]
        pub seat: u32,
        #[prost(sint64, tag = "2")]
        pub amount: i64,
    }
}

// ============================================================================
// 共享：play_random_hand
// ============================================================================

fn play_random_hand(seed: u64) -> HandHistory {
    let cfg = TableConfig::default_6max_100bb();
    let total = expected_total_chips(&cfg);
    let mut state = GameState::new(&cfg, seed);
    let mut rng = ChaCha20Rng::from_seed(seed.wrapping_add(0xF1_C0));
    Invariants::check_all(&state, total).expect("init invariants");
    for _ in 0..256 {
        if state.is_terminal() {
            break;
        }
        let la = state.legal_actions();
        let a = sample_action(&la, &mut rng).expect("legal action");
        state.apply(a).expect("apply legal");
        Invariants::check_all(&state, total).expect("invariants");
    }
    assert!(state.is_terminal());
    state.hand_history().clone()
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
            to: pick(min, max, rng),
        });
    }
    if let Some((min, max)) = la.raise_range {
        cands.push(Action::Raise {
            to: pick(min, max, rng),
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

fn pick(min: ChipAmount, max: ChipAmount, rng: &mut dyn RngSource) -> ChipAmount {
    let lo = min.as_u64();
    let hi = max.as_u64();
    if lo >= hi {
        return min;
    }
    ChipAmount::new(lo + rng.next_u64() % (hi - lo + 1))
}

// ============================================================================
// 1. 结构性 corrupted：byte flip / truncation / random garbage
// ============================================================================

/// 把 `from_proto` 的返回值断言为「Err 或 Ok 后再 round-trip 字节稳定」。
/// 任一情况都不允许 panic（Rust 模型下 panic = test 自动失败，无需显式 catch）。
fn assert_no_panic_robust(bytes: &[u8]) {
    match HandHistory::from_proto(bytes) {
        Err(_) => {}
        Ok(decoded) => {
            // PB-003：解码成功后必须 round-trip 字节稳定。任何 byte flip 巧合
            // 解出合法 history 都需 round-trip 一致——否则等于「同 history 两份
            // 不同 wire 表示」，破坏跨平台哈希一致性。
            let bytes2 = decoded.to_proto();
            let decoded2 =
                HandHistory::from_proto(&bytes2).expect("re-decode of own output must succeed");
            let bytes3 = decoded2.to_proto();
            assert_eq!(bytes2, bytes3, "round-trip must be byte-stable");
        }
    }
}

#[test]
fn byte_flip_no_panic_default_2k() {
    // 2,000 trials × 1 random byte flip 每 trial。covers PB / wire-format edges。
    let h = play_random_hand(0xF1_F1);
    let original = h.to_proto();
    let mut rng = ChaCha20Rng::from_seed(0xF1_FF);
    let n = 2_000;
    for _ in 0..n {
        let pos = (rng.next_u64() as usize) % original.len();
        let xor: u8 = (rng.next_u64() & 0xFF) as u8;
        let mut mutated = original.clone();
        mutated[pos] ^= xor;
        assert_no_panic_robust(&mutated);
    }
}

#[test]
fn truncation_no_panic_default() {
    // 所有合法前缀长度都必须不 panic。
    let h = play_random_hand(0xF1_AB);
    let original = h.to_proto();
    for len in 0..=original.len() {
        assert_no_panic_robust(&original[..len]);
    }
}

#[test]
fn random_garbage_no_panic_default_1k() {
    // 1,000 trials × 完全随机 byte stream（长度 0..512）。
    let mut rng = ChaCha20Rng::from_seed(0xF1_DE_AD);
    for _ in 0..1_000 {
        let len = (rng.next_u64() as usize) % 512;
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            bytes.push((rng.next_u64() & 0xFF) as u8);
        }
        assert_no_panic_robust(&bytes);
    }
}

#[test]
fn multi_byte_flip_no_panic() {
    // 一次翻多 byte（更接近 storage corruption 的现实模式）。
    let h = play_random_hand(0xF1_57);
    let original = h.to_proto();
    let mut rng = ChaCha20Rng::from_seed(0xF1_58);
    for _ in 0..500 {
        let mut mutated = original.clone();
        let n_flips = ((rng.next_u64() as usize) % 8) + 1;
        for _ in 0..n_flips {
            let pos = (rng.next_u64() as usize) % mutated.len();
            mutated[pos] ^= (rng.next_u64() & 0xFF) as u8;
        }
        assert_no_panic_robust(&mutated);
    }
}

#[test]
#[ignore = "F1 → opt-in: 100k flip × seed × position fuzz; 默认仅 2k"]
fn byte_flip_no_panic_full_100k() {
    let h = play_random_hand(0xF1_F2);
    let original = h.to_proto();
    let mut rng = ChaCha20Rng::from_seed(0xF1_F3);
    for _ in 0..100_000 {
        let pos = (rng.next_u64() as usize) % original.len();
        let mut mutated = original.clone();
        mutated[pos] ^= (rng.next_u64() & 0xFF) as u8;
        assert_no_panic_robust(&mutated);
    }
}

// ============================================================================
// 2. 域违规：可控镜像构造器
// ============================================================================

fn baseline_mirror(seed: u64) -> mirror::HandHistory {
    let h = play_random_hand(seed);
    // 借 prost 反序列化把现成 history 转回镜像类型，省去手填 18+ 字段。
    let bytes = h.to_proto();
    mirror::HandHistory::decode(bytes.as_slice()).expect("mirror decode of valid history")
}

fn encode_mirror(m: &mirror::HandHistory) -> Vec<u8> {
    m.encode_to_vec()
}

#[test]
fn from_proto_rejects_n_seats_zero() {
    let mut m = baseline_mirror(11);
    m.config.as_mut().unwrap().n_seats = 0;
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
fn from_proto_rejects_n_seats_one() {
    let mut m = baseline_mirror(12);
    m.config.as_mut().unwrap().n_seats = 1;
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
fn from_proto_rejects_n_seats_too_large() {
    let mut m = baseline_mirror(13);
    m.config.as_mut().unwrap().n_seats = 10;
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
fn from_proto_rejects_n_seats_overflow_u8() {
    // n_seats > u8::MAX 在 src/history.rs 直接走 (2..=9) 范围检查，应 Corrupted。
    let mut m = baseline_mirror(14);
    m.config.as_mut().unwrap().n_seats = 1024;
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
fn from_proto_rejects_starting_stacks_length_mismatch() {
    let mut m = baseline_mirror(15);
    m.config.as_mut().unwrap().starting_stacks.pop();
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
fn from_proto_rejects_hole_cards_length_mismatch() {
    let mut m = baseline_mirror(16);
    m.hole_cards.pop();
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
fn from_proto_rejects_card_value_out_of_range_in_board() {
    let mut m = baseline_mirror(17);
    if m.board.is_empty() {
        m.board.push(52); // 强插 invalid card
    } else {
        m.board[0] = 52; // 52 = invalid（0..=51）
    }
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
fn from_proto_rejects_card_value_out_of_range_in_hole() {
    let mut m = baseline_mirror(18);
    // 找一个 present hole_cards 把 c0 改为 99（远超 51）。
    let hole = m
        .hole_cards
        .iter_mut()
        .find(|h| h.present)
        .expect("at least one player active to showdown");
    hole.c0 = 99;
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
fn from_proto_rejects_action_kind_unspecified() {
    // PB-002：proto3 默认值 = 0 = ActionKind::UNSPECIFIED。任何 kind=0 必拒。
    let mut m = baseline_mirror(19);
    if m.actions.is_empty() {
        // play_random_hand 至少 2 个 fold 才能终局，应不会触发，但兜底处理。
        eprintln!("[skip] empty actions vector");
        return;
    }
    m.actions[0].kind = 0;
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
fn from_proto_rejects_action_kind_out_of_range() {
    let mut m = baseline_mirror(20);
    if m.actions.is_empty() {
        eprintln!("[skip] empty actions");
        return;
    }
    m.actions[0].kind = 99;
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
fn from_proto_rejects_street_unspecified() {
    // PB-002：Street 默认值 = 0 = STREET_UNSPECIFIED，必拒。
    let mut m = baseline_mirror(21);
    if m.actions.is_empty() {
        eprintln!("[skip] empty actions");
        return;
    }
    m.actions[0].street = 0;
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
fn from_proto_rejects_street_out_of_range() {
    let mut m = baseline_mirror(22);
    if m.actions.is_empty() {
        eprintln!("[skip] empty actions");
        return;
    }
    m.actions[0].street = 99;
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
fn from_proto_rejects_missing_config() {
    let mut m = baseline_mirror(23);
    m.config = None;
    let r = HandHistory::from_proto(&encode_mirror(&m));
    match r {
        Err(HistoryError::Corrupted(msg)) => {
            assert!(
                msg.contains("missing config"),
                "expected missing config msg: {msg}"
            );
        }
        Err(e) => panic!("expected Corrupted(missing config), got: {e}"),
        Ok(_) => panic!("expected Corrupted, got Ok"),
    }
}

// ============================================================================
// 2.b 域违规（F1 → F2 carry-over）：当前 from_proto 不严校验、走 replay 才报
// ============================================================================
//
// 这些 case 当前 stage-1 实现下 from_proto 直接通过，corruption 在 replay()
// 阶段被规则引擎捕获并以 `HistoryError::Rule { source }` 返回。「不 panic」
// 已满足，但若 F2 选择把 corruption 提前到 from_proto 拒绝，断言更严的错误
// 形式更明确。当前实现满足 validation §5 「明确错误，禁止静默截断」。
//
// 标 `#[ignore = "F1 → F2"]`：默认 cargo test 跳过；`cargo test -- --ignored`
// 显式触发可看 F2 工作面。

#[test]
#[ignore = "F1 → F2: 当前 from_proto 通过，replay 时 NotPlayerTurn；F2 可前移到 from_proto"]
fn from_proto_rejects_action_seat_out_of_range() {
    let mut m = baseline_mirror(24);
    if m.actions.is_empty() {
        return;
    }
    m.actions[0].seat = m.config.as_ref().unwrap().n_seats + 5; // 远超 n_seats
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
#[ignore = "F1 → F2: button_seat 在 from_proto 时未严校验"]
fn from_proto_rejects_button_seat_out_of_range() {
    let mut m = baseline_mirror(25);
    m.config.as_mut().unwrap().button_seat = 99;
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
#[ignore = "F1 → F2: 重复牌目前由 replay() ReplayDiverged 捕获，可在 from_proto 提前拒绝"]
fn from_proto_rejects_duplicate_card_in_board() {
    let mut m = baseline_mirror(26);
    if m.board.len() < 2 {
        // 终局未到 turn → 跳过这条 case
        return;
    }
    m.board[1] = m.board[0]; // 重复
    let r = HandHistory::from_proto(&encode_mirror(&m));
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

// ============================================================================
// 3. 回放语义 corrupted：board / hole_cards 与 seed 重发不一致
// ============================================================================

#[test]
fn replay_diverged_when_board_swapped() {
    let mut m = baseline_mirror(27);
    if m.board.len() < 2 {
        return;
    }
    // swap board[0] 与一个未在 board / hole 中的 card —— 改成 51（最高位 As♠
    // 通常不出现在前两张 flop）。命中重叠时也 fine—— replay 仍 diverge。
    m.board[0] = if m.board[0] == 51 { 0 } else { 51 };
    let bytes = encode_mirror(&m);
    // from_proto 应通过（card index 合法、长度合法）。
    let h = HandHistory::from_proto(&bytes)
        .expect("from_proto must accept legal-but-semantically-wrong board");
    let r = h.replay();
    assert!(
        matches!(r, Err(HistoryError::ReplayDiverged { .. })),
        "expected ReplayDiverged, got {r:?}"
    );
}

#[test]
fn replay_diverged_when_hole_cards_swapped() {
    let mut m = baseline_mirror(28);
    let hole = m
        .hole_cards
        .iter_mut()
        .find(|h| h.present)
        .expect("at least one present hole");
    hole.c0 = if hole.c0 == 51 { 0 } else { 51 };
    let bytes = encode_mirror(&m);
    let h =
        HandHistory::from_proto(&bytes).expect("from_proto must accept legal-but-wrong hole_cards");
    let r = h.replay();
    assert!(
        matches!(r, Err(HistoryError::ReplayDiverged { .. })),
        "expected ReplayDiverged, got {r:?}"
    );
}

#[test]
fn replay_action_rejected_with_rule_error() {
    // 改第 1 个 action 的 kind 把它从合法变非法（如 Fold→Bet）让规则引擎拒绝。
    let mut m = baseline_mirror(29);
    if m.actions.is_empty() {
        return;
    }
    // 1=FOLD 2=CHECK 3=CALL 4=BET 5=RAISE
    let original = m.actions[0].kind;
    m.actions[0].kind = if original == 4 { 5 } else { 4 }; // 强插 BET/RAISE
    m.actions[0].to = 999_999_999_999_u64; // 越境 amount
    let bytes = encode_mirror(&m);
    let h = HandHistory::from_proto(&bytes)
        .expect("from_proto: legal kind + legal amount，但语义违反 → 走 replay");
    let r = h.replay();
    match r {
        Err(HistoryError::Rule { index, .. }) => {
            assert!(index <= h.actions.len(), "index in range");
        }
        Err(HistoryError::ReplayDiverged { .. }) => {
            // 也是合法的 「明确错误」 形式
        }
        Err(e) => panic!("expected Rule/ReplayDiverged, got: {e}"),
        Ok(_) => panic!("expected Err, got Ok"),
    }
}

// ============================================================================
// 4. 边界 sanity
// ============================================================================

#[test]
fn empty_bytes_is_clear_error_not_panic() {
    let r = HandHistory::from_proto(&[]);
    // schema_version 默认 0 → SchemaVersionMismatch；不 panic。
    assert!(r.is_err(), "empty bytes must error");
}

#[test]
fn replay_to_index_out_of_range_returns_error() {
    let h = play_random_hand(30);
    let r = h.replay_to(h.actions.len() + 100);
    assert!(matches!(r, Err(HistoryError::Corrupted(_))), "got {r:?}");
}

#[test]
fn double_decode_is_idempotent() {
    let h = play_random_hand(31);
    let b1 = h.to_proto();
    let h2 = HandHistory::from_proto(&b1).unwrap();
    let b2 = h2.to_proto();
    assert_eq!(b1, b2, "decode → encode must be byte-stable (PB-003)");
    let h3 = HandHistory::from_proto(&b2).unwrap();
    let b3 = h3.to_proto();
    assert_eq!(b2, b3, "second round-trip stable");
}
