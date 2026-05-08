#![no_main]
//! D1：history protobuf decode fuzz target（workflow §D1 §输出 第 2 条）。
//!
//! 输入：任意 byte stream，喂给 `HandHistory::from_proto`。
//!
//! 验证：
//!
//! - 解码不 panic（任何 OOM / arithmetic overflow / unwrap None 都是产品代码 bug）
//! - 错误必须以 `HistoryError` 形式返回，而非 panic
//! - 解码成功的 history 再 to_proto + from_proto 必须 byte-equal（PB-003）
//!
//! 角色边界：本 target 属 [测试]。任何 crash artifact 移交 D2 [实现] 修复。

use libfuzzer_sys::fuzz_target;
use poker::HandHistory;

fuzz_target!(|data: &[u8]| {
    let Ok(history) = HandHistory::from_proto(data) else {
        return;
    };
    // 解码成功 → 必须能 round-trip 出相同字节流
    let bytes = history.to_proto();
    let roundtrip = HandHistory::from_proto(&bytes)
        .expect("round-trip decode must succeed if first decode succeeded");
    let bytes2 = roundtrip.to_proto();
    assert_eq!(
        bytes, bytes2,
        "history protobuf round-trip not byte-stable"
    );
});
