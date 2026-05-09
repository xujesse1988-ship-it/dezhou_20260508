//! F1：HandHistory schema 版本兼容性测试（workflow §F1 第 1 件套）。
//!
//! 验收门槛（workflow §F1, validation §5 第 4 行）：
//!
//! > hand history 必须带显式 schema 版本号；schema 升级必须保持向后兼容或
//! > 提供升级器，旧版本 history 在新代码下能被识别（升级或拒绝），不允许
//! > 静默错读。
//!
//! 当前 schema_version = 1（D-061）。`HandHistory::from_proto` 必须：
//!
//! - 对 v1 round-trip 成功（baseline）。
//! - 对任何 != 1 的 schema_version 返回 `HistoryError::SchemaVersionMismatch`，
//!   而非静默接受、静默截断或 panic。
//! - 对 PB-002 哨兵（`ActionKind::UNSPECIFIED` / `Street::UNSPECIFIED`，proto3
//!   默认值 = 0，遭遇 0 即说明字段缺失或被截断）返回 `HistoryError::Corrupted`。
//! - 对 PB-003 deterministic：相同 `HandHistory` 的 `to_proto` 输出 byte-equal
//!   （已由 `tests/history_roundtrip.rs` content_hash 验收，本文件不重复）。
//!
//! F2 carry-over：本文件不实现升级器。当前 stage-1 schema_version 只支持 1，
//! 所有 != 1 走拒绝路径。如 stage-2 引入 schema_version = 2，F2 在产品代码
//! 端追加升级器，并把 `from_proto_rejects_schema_v2_currently` 翻为
//! `from_proto_upgrades_schema_v2_to_v1`（或显式拒绝 + 文档差异）。
//!
//! 角色边界：[测试]，不修改产品代码。
//! 攻击 bytes 由本文件用 prost wire-format 手术构造（`mutate_first_varint`），
//! 不依赖产品代码暴露 `mod proto`。

use poker::{
    Action, ChaCha20Rng, ChipAmount, GameState, HandHistory, HistoryError, LegalActionSet,
    RngSource, TableConfig,
};

mod common;
use common::{expected_total_chips, Invariants};

// ============================================================================
// Wire-format 手术辅助（proto3 minimal subset）
// ============================================================================

/// 解码 varint：返回 (value, bytes_consumed)。失败时 panic（仅用于已知合法输入）。
fn parse_varint(buf: &[u8], pos: usize) -> (u64, usize) {
    let mut n: u64 = 0;
    let mut shift = 0u32;
    let mut i = pos;
    loop {
        assert!(i < buf.len(), "truncated varint at {i}");
        let b = buf[i];
        i += 1;
        n |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 {
            return (n, i);
        }
        shift += 7;
        assert!(shift <= 63, "varint too long");
    }
}

/// 编码 u64 varint。
fn encode_varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    while value >= 0x80 {
        out.push(((value & 0x7F) as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
    out
}

/// 把 prost 输出的 HandHistory bytes 中 field 1 (schema_version) 的 varint
/// 替换为 `new_value`。如 `new_value == 0`，proto3 default 优化使该字段被
/// 整体省略（与 `prost::Message::encode_to_vec` 行为一致）。
///
/// 调用方保证 bytes 第一字段 tag 是 schema_version（field 1 varint，tag = 0x08）；
/// 这是 `HandHistory::to_proto` 的稳定行为（prost 按 tag 升序输出）。
fn mutate_schema_version(bytes: &[u8], new_value: u64) -> Vec<u8> {
    assert!(!bytes.is_empty(), "to_proto produced empty bytes");
    assert_eq!(bytes[0], 0x08, "first byte should be field 1 varint tag");
    // 跳过 0x08（tag），解析 varint 取得旧 schema_version 字段长度
    let (_old_value, after) = parse_varint(bytes, 1);
    // 重组：新 tag+varint（如 new_value == 0 则整体省略）+ 余下字段
    let mut out = Vec::with_capacity(bytes.len() + 4);
    if new_value != 0 {
        out.push(0x08);
        out.extend_from_slice(&encode_varint(new_value));
    }
    out.extend_from_slice(&bytes[after..]);
    out
}

// ============================================================================
// 共享：play_random_hand 出一手 history（取自 cross_lang_history.rs 同型）
// ============================================================================

fn play_random_hand(seed: u64) -> HandHistory {
    let cfg = TableConfig::default_6max_100bb();
    let total = expected_total_chips(&cfg);
    let mut state = GameState::new(&cfg, seed);
    let mut rng = ChaCha20Rng::from_seed(seed.wrapping_add(0xF1_5C));
    Invariants::check_all(&state, total).expect("init invariants");
    for _ in 0..256 {
        if state.is_terminal() {
            break;
        }
        let la = state.legal_actions();
        let a = sample_action(&la, &mut rng).expect("legal action available");
        state.apply(a).expect("apply legal action");
        Invariants::check_all(&state, total).expect("invariants");
    }
    assert!(state.is_terminal(), "must terminate within 256 actions");
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
// Baseline：v1 round-trip
// ============================================================================

#[test]
fn schema_v1_default_roundtrips_ok() {
    for seed in 0..16u64 {
        let h = play_random_hand(seed.wrapping_add(0xF1_DA_7A));
        assert_eq!(h.schema_version, 1, "stage-1 always writes v1 (D-061)");
        let bytes = h.to_proto();
        let decoded = HandHistory::from_proto(&bytes)
            .unwrap_or_else(|e| panic!("v1 round-trip must succeed (seed={seed}): {e}"));
        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.content_hash(), h.content_hash());
    }
}

#[test]
fn schema_v1_serialized_bytes_lead_with_field_1_tag() {
    // PB-003 wire-stability：prost 按 tag 升序输出，schema_version 永远在最前。
    // 若该断言被打破，本文件其它 mutate_* 测试需相应调整。
    let h = play_random_hand(0xBEEF);
    let bytes = h.to_proto();
    assert!(!bytes.is_empty());
    assert_eq!(bytes[0], 0x08, "first byte should be field 1 varint tag");
    assert_eq!(
        bytes[1], 0x01,
        "second byte should be schema_version=1 varint"
    );
}

// ============================================================================
// 拒绝路径：schema_version != 1
// ============================================================================

fn assert_schema_mismatch(bytes: &[u8], expected_found: u32) {
    match HandHistory::from_proto(bytes) {
        Err(HistoryError::SchemaVersionMismatch { found, supported }) => {
            assert_eq!(found, expected_found, "found mismatch");
            assert_eq!(supported, 1, "supported must always be 1 in stage-1");
        }
        Err(e) => panic!("expected SchemaVersionMismatch, got: {e}"),
        Ok(_) => panic!("expected SchemaVersionMismatch, got Ok (silent accept)"),
    }
}

#[test]
fn from_proto_rejects_schema_v0_implicit_default() {
    // proto3 默认值优化：schema_version=0 时字段被 prost 整体省略。
    // 模拟"上游写入端忘了设置版本号" / "字段被截断" 的场景。
    let h = play_random_hand(1);
    let mutated = mutate_schema_version(&h.to_proto(), 0);
    // 截掉 field 1 后，proto 长度应严格变短（两字节减少）。
    assert!(mutated.len() < h.to_proto().len());
    assert_schema_mismatch(&mutated, 0);
}

#[test]
fn from_proto_rejects_schema_v2_future() {
    // 模拟"将来 stage-2 写入了 v2，stage-1 代码读"的场景。F2 可在产品代码端
    // 选择追加 v2→v1 升级器；当前实现走显式拒绝。
    let h = play_random_hand(2);
    let mutated = mutate_schema_version(&h.to_proto(), 2);
    assert_schema_mismatch(&mutated, 2);
}

#[test]
fn from_proto_rejects_schema_v999_far_future() {
    let h = play_random_hand(3);
    let mutated = mutate_schema_version(&h.to_proto(), 999);
    assert_schema_mismatch(&mutated, 999);
}

#[test]
fn from_proto_rejects_schema_u32_max() {
    let h = play_random_hand(4);
    let mutated = mutate_schema_version(&h.to_proto(), u32::MAX as u64);
    assert_schema_mismatch(&mutated, u32::MAX);
}

#[test]
fn from_proto_rejects_schema_v1_silent_neighbors() {
    // 边界扫描：1 是唯一接受值；其它紧邻 / 远离值都必须拒绝。
    let h = play_random_hand(5);
    let bytes = h.to_proto();
    for v in [
        3u64,
        4,
        5,
        16,
        64,
        127,
        128,
        256,
        65_535,
        u32::MAX as u64 - 1,
    ] {
        let mutated = mutate_schema_version(&bytes, v);
        assert_schema_mismatch(&mutated, v as u32);
    }
}

// ============================================================================
// PB-002 哨兵：UNSPECIFIED 拒绝
// ============================================================================

#[test]
fn from_proto_rejects_empty_bytes_as_schema_zero() {
    // 空字节 → prost 解码为默认结构 → schema_version=0 → SchemaVersionMismatch。
    // 验证错误优先级：schema 检查在 missing config 检查之前（见 src/history.rs）。
    assert_schema_mismatch(&[], 0);
}

#[test]
fn from_proto_rejects_only_schema_version_v1_no_config() {
    // 只含 schema_version=1，余字段缺失 → "missing config" Corrupted。
    let bytes: &[u8] = &[0x08, 0x01];
    match HandHistory::from_proto(bytes) {
        Err(HistoryError::Corrupted(msg)) => {
            assert!(
                msg.contains("missing config"),
                "expected missing config, got: {msg}"
            );
        }
        Err(e) => panic!("expected Corrupted(missing config), got: {e}"),
        Ok(_) => panic!("expected Corrupted, got Ok"),
    }
}

// ============================================================================
// 大小端 / varint 长度漂移：超长 varint 也算合法 wire 表达，最终 schema 检查
// 仍由数值语义决定。proto3 容许 varint 多余 leading 0（continuation bit + 0）。
// ============================================================================

#[test]
fn from_proto_rejects_padded_varint_v1_overflow_to_zero() {
    // 5 字节 0 varint = 0，等价 v0 缺失（数值上）。但 wire 上是显式 0，prost
    // 会读取并填入 schema_version=0 → SchemaVersionMismatch。
    // 注意：prost 对 uint32 的 varint 仍按 u64 解码后 truncate，本测试仅断言
    // 「不 panic + 错误明确」。
    let mut bytes = vec![0x08u8, 0x80, 0x80, 0x80, 0x80, 0x00];
    bytes.extend_from_slice(b"\x12\x00"); // 加个空 config (field 2, len 0) 占位
    match HandHistory::from_proto(&bytes) {
        Err(HistoryError::SchemaVersionMismatch { found: 0, .. }) => {}
        Err(e) => panic!("expected SchemaVersionMismatch{{found=0}}, got: {e}"),
        Ok(_) => panic!("expected error, got Ok"),
    }
}
