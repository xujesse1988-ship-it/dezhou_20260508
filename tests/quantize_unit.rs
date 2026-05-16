//! 阶段 5 B1 \[测试\] — q15 quantization helper 单元测试（API-520..API-525 /
//! D-511 字面）。
//!
//! ## 角色边界
//!
//! 本文件属 `[测试]` agent；B1 [测试] 0 改动产品代码。B2 \[实现\] 落地后所有
//! `#[ignore]` opt-in 测试转 pass。
//!
//! ## 量化数学（D-511 字面）
//!
//! ```text
//! q15 = ((value / scale) × 32768).round().clamp(-32768, 32767) as i16
//! f32 = (q as f32) × (scale / 32768.0)
//! ```
//!
//! ## 测试覆盖（≥ 6 test，B1 [测试] exit 字面）
//!
//! 1. `f32_to_q15` round-trip 在 q15 精度内（精度 = `scale / 32768`）。
//! 2. `q15_to_f32` 零输入返零。
//! 3. `compute_row_scale` = `max(|row|)`；全零 row 返 `0.0`。
//! 4. `quantize_row` padding `[14..16]` 设为 `i16::MIN`（D-513 字面 AVX2 协议）。
//! 5. `dequantize_row` 忽略 padding 不污染前 14 个 action。
//! 6. `dequantize_action` 单 action 路径与 `dequantize_row[a]` byte-equal。
//! 7. `f32_to_q15` 超 `[-scale, scale]` 时 clamp 到 `±32767`（saturating）。
//! 8. `scale == 0.0` defensive 路径返 q15 = 0。

use poker::training::quantize::{
    compute_row_scale, dequantize_action, dequantize_row, f32_to_q15, q15_to_f32, quantize_row,
};

// ---------------------------------------------------------------------------
// Group A — f32 ↔ q15 round-trip + 边界
// ---------------------------------------------------------------------------

/// API-520 / API-521 — round-trip 在 q15 精度内（精度 = `scale / 32768`）。
#[test]
#[ignore = "B1 scaffold; A1 stub `unimplemented!()`; B2 [实现] 落地后转 pass"]
fn f32_to_q15_round_trip_within_precision() {
    let scale: f32 = 100.0;
    let precision: f32 = scale / 32768.0;
    for &value in &[0.0, 1.0, -1.0, 50.0, -50.0, 99.0, -99.0, 75.5, -42.3] {
        let q = f32_to_q15(value, scale);
        let back = q15_to_f32(q, scale);
        let err = (back - value).abs();
        assert!(
            err <= precision,
            "f32 {value} → q15 {q} → f32 {back}: err {err} > 精度 {precision}"
        );
    }
}

/// API-521 — q15 = 0 + scale = 任意 → f32 = 0。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn q15_to_f32_zero_input_returns_zero() {
    for scale in &[1.0_f32, 100.0, 1e6, 1e-3] {
        let f = q15_to_f32(0, *scale);
        assert_eq!(f, 0.0, "q15 = 0, scale = {scale} → f32 应 = 0.0, 实 = {f}");
    }
}

/// API-520 — value 超 `[-scale, scale]` 时 clamp 到 `±32767`（saturating，D-511
/// 字面）。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn f32_to_q15_clamps_to_int16_bounds() {
    let scale: f32 = 100.0;
    // value = 1e9 → q15 应被 clamp 到 32767。
    let q_pos = f32_to_q15(1e9, scale);
    assert_eq!(q_pos, 32767, "value = 1e9 应 saturate 到 q15 = 32767");
    let q_neg = f32_to_q15(-1e9, scale);
    assert_eq!(q_neg, -32768, "value = -1e9 应 saturate 到 q15 = -32768");
}

/// API-520 — `scale == 0.0` defensive 返 q15 = 0（应当不发生，但 row 至少有 1
/// 个非零值时 scale > 0；defensive 路径不应 panic）。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn f32_to_q15_scale_zero_defensive_returns_zero() {
    let q = f32_to_q15(42.0, 0.0);
    assert_eq!(
        q, 0,
        "scale = 0.0 defensive 路径应返 q15 = 0（避免 NaN 传播）"
    );
}

// ---------------------------------------------------------------------------
// Group B — compute_row_scale 边界（D-511 字面 = max(|row|)）
// ---------------------------------------------------------------------------

/// API-522 — `compute_row_scale` = `max(|row|)` 非负值。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn compute_row_scale_returns_max_absolute() {
    let row: [f32; 14] = [
        1.0, -50.0, 3.0, 100.0, -120.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0,
    ];
    let s = compute_row_scale(&row);
    assert_eq!(s, 120.0, "max(|row|) = 120, 实 {s}");
}

/// API-522 — 全零 row 返 `0.0`（D-511 字面 defensive）。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn compute_row_scale_all_zero_returns_zero() {
    let row: [f32; 14] = [0.0; 14];
    let s = compute_row_scale(&row);
    assert_eq!(s, 0.0, "全零 row scale 应 = 0.0, 实 {s}");
}

// ---------------------------------------------------------------------------
// Group C — quantize_row + dequantize_row padding 协议（D-513 字面）
// ---------------------------------------------------------------------------

/// API-523 / D-513 字面 — `quantize_row` padding `[14..16]` 设为 `i16::MIN`。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn quantize_row_padding_set_to_i16_min() {
    let row: [f32; 14] = [
        1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0,
    ];
    let scale = 100.0_f32;
    let mut out = [0i16; 16];
    quantize_row(&row, scale, &mut out);
    assert_eq!(
        out[14],
        i16::MIN,
        "padding[14] 应 = i16::MIN, 实 {}",
        out[14]
    );
    assert_eq!(
        out[15],
        i16::MIN,
        "padding[15] 应 = i16::MIN, 实 {}",
        out[15]
    );
}

/// API-524 — `dequantize_row` 忽略 padding（即使 padding = i16::MIN 也不影响
/// 前 14 action 输出）。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn dequantize_row_ignores_padding() {
    let scale = 50.0_f32;
    // 构造 payload — 前 14 byte 任意 + padding = i16::MIN。
    let mut payload = [0i16; 16];
    for (i, slot) in payload.iter_mut().enumerate().take(14) {
        *slot = f32_to_q15(i as f32, scale);
    }
    payload[14] = i16::MIN;
    payload[15] = i16::MIN;
    let mut out = [0.0_f32; 14];
    dequantize_row(&payload, scale, &mut out);
    // 前 14 byte = 0..13；padding 不应污染 out。
    for (i, &v) in out.iter().enumerate() {
        let expected = i as f32;
        assert!(
            (v - expected).abs() <= scale / 32768.0,
            "dequantize_row action {i} = {v}, 期望 {expected}（padding 误污染？）"
        );
    }
}

/// API-525 — `dequantize_action` 单 action 与 `dequantize_row[a]` byte-equal。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn dequantize_action_matches_row_path() {
    let scale = 75.0_f32;
    let row: [f32; 14] = [
        1.0, -2.0, 3.5, -4.5, 5.0, -6.0, 7.5, -8.5, 9.0, -10.0, 11.5, -12.5, 13.0, -14.0,
    ];
    let mut payload = [0i16; 16];
    quantize_row(&row, scale, &mut payload);
    let mut full = [0.0_f32; 14];
    dequantize_row(&payload, scale, &mut full);
    for (a, &row_val) in full.iter().enumerate() {
        let single = dequantize_action(&payload, scale, a);
        // 单 action 与 row[a] byte-equal（同一 q15 → f32 计算路径）。
        assert_eq!(
            single.to_bits(),
            row_val.to_bits(),
            "dequantize_action({a}) {single} ≠ dequantize_row[{a}] {row_val} (bit-level)"
        );
    }
}

/// API-523 + API-524 整行 round-trip 在 q15 精度内。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn quantize_row_dequantize_row_full_round_trip() {
    let scale = 200.0_f32;
    let row: [f32; 14] = [
        0.0, 1.0, -1.0, 100.0, -100.0, 50.0, -50.0, 199.0, -199.0, 10.0, -10.0, 5.5, -5.5, 0.1,
    ];
    let mut payload = [0i16; 16];
    quantize_row(&row, scale, &mut payload);
    let mut back = [0.0_f32; 14];
    dequantize_row(&payload, scale, &mut back);
    let precision = scale / 32768.0;
    for (i, (&orig, &decoded)) in row.iter().zip(back.iter()).enumerate() {
        let err = (orig - decoded).abs();
        assert!(
            err <= precision,
            "row[{i}] {orig} → q15 → f32 {decoded}: err {err} > 精度 {precision}"
        );
    }
}
