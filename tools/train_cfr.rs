//! Stage 3 CFR / MCCFR 训练 CLI（D-372 / API-370）。
//!
//! CLI 入口骨架：
//!
//! ```text
//! cargo run --release --bin train_cfr -- [OPTIONS]
//!
//! OPTIONS:
//!     --game {kuhn,leduc,nlhe}       (required) D-372 game selection
//!     --trainer {vanilla,es-mccfr}   (optional) 默认按 game 自动推断
//!     --iter N                       (required for Kuhn/Leduc) iter 数
//!     --updates N                    (required for nlhe) update 数
//!     --seed S                       (optional, default 0) master seed
//!     --checkpoint-dir DIR           (optional, default ./artifacts/)
//!     --resume PATH                  (optional) 从 checkpoint 恢复
//!     --checkpoint-every N           (optional) 自动 checkpoint 频率
//!     --keep-last N                  (optional, default 5) backup 保留数（D-359）
//!     --bucket-table PATH            (required for nlhe) BucketTable artifact 路径
//!     --threads N                    (optional, default 1) 多线程并发数（仅 ES-MCCFR）
//!     --quiet                        (optional) 静默 progress log
//! ```
//!
//! A1 \[实现\] 阶段 main body 走 stub，仅打印 "scaffold not yet implemented"
//! 并以非零 exit code 退出（让外部脚本误调用时立即可观察）。具体 dispatch +
//! progress log + checkpoint write 由 B2 / C2 / D2 \[实现\] 落地。

fn main() {
    eprintln!(
        "[train_cfr] stage 3 A1 [实现] scaffold — CFR / MCCFR 训练 dispatch 由后续 \
         B2 / C2 / D2 [实现] 落地，详见 docs/pluribus_stage3_workflow.md §B/§C/§D。"
    );
    std::process::exit(2);
}
