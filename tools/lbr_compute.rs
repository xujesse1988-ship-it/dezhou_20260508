//! Stage 4 D-450 / API-452 — LBR (Local Best Response) computation CLI scaffold.
//!
//! 用法（E2 \[实现\] 落地后形态，A1 \[实现\] scaffold 仅占位）：
//!
//! ```text
//! cargo run --release --bin lbr_compute -- \
//!     --checkpoint PATH \
//!     --n-hands 1000 \
//!     --traverser 0 \
//!     --rng-seed S
//! ```
//!
//! **A1 \[实现\] 状态**：`main()` body `unimplemented!()`，E2 \[实现\] 起步前
//! 落地走 `LbrEvaluator::new` + `compute` / `compute_six_traverser_average`
//! dispatch（D-450..D-457 + D-459 6-traverser per-traverser min/max/average
//! 输出）。

fn main() {
    unimplemented!(
        "stage 4 A1 [实现] scaffold: tools/lbr_compute.rs main 落地 E2 [实现] D-450 / API-452"
    );
}
