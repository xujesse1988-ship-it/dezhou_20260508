//! 阶段 5 q15 quantization helper（API-520..API-525 / D-511 字面）。
//!
//! `f32 ↔ q15`（i16）双向转换 + per-row scale 计算策略。stage 5 紧凑 RegretTable
//! / StrategyAccumulator 内部 14-action row 共享一个 f32 scale factor，row 量化
//! 范围 = `[-scale, scale)`，q15 精度 = `scale / 32768`。Row total = 16 × 2 byte
//! int16 + 4 byte scale = **36 byte** vs naive 14 × 4 byte f32 = 56 byte，
//! **36% 节省**，叠加 HashMap overhead 满足 D-540 ≥ 50% memory ↓ SLO。
//!
//! ## 量化数学（D-511 字面）
//!
//! ```text
//! q15 = ((f32_value / scale) × 32768.0).round().clamp(-32768, 32767) as i16
//! f32_value = (q15 as i16 as f32) × (scale / 32768.0)
//! ```
//!
//! ## A1 \[实现\] 状态
//!
//! 所有 helper fn 走 `unimplemented!()` 占位。B2 \[实现\] 落地。

#![allow(clippy::needless_pass_by_value)]

/// API-520 — f32 → q15 单值量化。
///
/// `scale == 0.0` 时返回 0（defensive；应当不发生，row 至少有 1 个非零值时 scale > 0）。
///
/// # A1 \[实现\] 状态
///
/// `unimplemented!()` 占位；B2 \[实现\] 落地 `((value / scale) × 32768.0).round()
/// .clamp(-32768.0, 32767.0) as i16`。
pub fn f32_to_q15(value: f32, scale: f32) -> i16 {
    let _ = (value, scale);
    unimplemented!("stage 5 A1 scaffold — quantize::f32_to_q15 落地于 B2 [实现]")
}

/// API-521 — q15 → f32 单值反量化。
///
/// # A1 \[实现\] 状态
///
/// `unimplemented!()` 占位；B2 \[实现\] 落地 `(q as f32) × (scale / 32768.0)`。
pub fn q15_to_f32(q: i16, scale: f32) -> f32 {
    let _ = (q, scale);
    unimplemented!("stage 5 A1 scaffold — quantize::q15_to_f32 落地于 B2 [实现]")
}

/// API-522 — 14-action row 的 scale factor（= max(|row|) 或 0.0 若全 0）。
///
/// 给 add_regret 路径下 row scale 初始化 / 重算用。
///
/// # A1 \[实现\] 状态
///
/// `unimplemented!()` 占位；B2 \[实现\] 落地。
pub fn compute_row_scale(row_values: &[f32; 14]) -> f32 {
    let _ = row_values;
    unimplemented!("stage 5 A1 scaffold — quantize::compute_row_scale 落地于 B2 [实现]")
}

/// API-523 — 14-action row 整行 quantize。
///
/// padding `[14..16]` 设为 `i16::MIN` 保证不会被误 sample（D-513 字面 AVX2
/// padding 协议）。
///
/// # A1 \[实现\] 状态
///
/// `unimplemented!()` 占位；B2 \[实现\] 落地。
pub fn quantize_row(row_values: &[f32; 14], scale: f32, out: &mut [i16; 16]) {
    let _ = (row_values, scale, out);
    unimplemented!("stage 5 A1 scaffold — quantize::quantize_row 落地于 B2 [实现]")
}

/// API-524 — 14-action row 整行 dequantize。
///
/// 忽略 padding `[14..16]`。
///
/// # A1 \[实现\] 状态
///
/// `unimplemented!()` 占位；B2 \[实现\] 落地。
pub fn dequantize_row(payload: &[i16; 16], scale: f32, out: &mut [f32; 14]) {
    let _ = (payload, scale, out);
    unimplemented!("stage 5 A1 scaffold — quantize::dequantize_row 落地于 B2 [实现]")
}

/// API-525 — 单 action q15 dequant（给 should_prune inline check 用，避免整行
/// dequant overhead）。
///
/// # A1 \[实现\] 状态
///
/// `unimplemented!()` 占位；B2 \[实现\] 落地。
pub fn dequantize_action(payload: &[i16; 16], scale: f32, action: usize) -> f32 {
    let _ = (payload, scale, action);
    unimplemented!("stage 5 A1 scaffold — quantize::dequantize_action 落地于 B2 [实现]")
}
