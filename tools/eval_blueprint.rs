//! Stage 4 D-461 + D-481 / API-462 + API-484 — Slumbot 100K 手 + baseline 1M
//! 手整合评测 CLI scaffold。
//!
//! 用法（F2 \[实现\] 落地后形态，A1 \[实现\] scaffold 仅占位）：
//!
//! ```text
//! cargo run --release --bin eval_blueprint -- \
//!     --checkpoint PATH \
//!     --slumbot-endpoint http://www.slumbot.com/api/ \
//!     --slumbot-hands 100000 \
//!     --baseline-hands 1000000 \
//!     --master-seed S \
//!     [--duplicate-dealing] \
//!     [--no-slumbot]
//! ```
//!
//! **A1 \[实现\] 状态**：`main()` body `unimplemented!()`，F2 \[实现\] 起步前
//! 落地走 `SlumbotBridge::evaluate_blueprint` + 3 baseline 全跑 (Random /
//! CallStation / TAG) + JSONL 输出（D-460..D-481）。

fn main() {
    unimplemented!(
        "stage 4 A1 [实现] scaffold: tools/eval_blueprint.rs main 落地 F2 [实现] D-461 / D-481 / API-462 / API-484"
    );
}
