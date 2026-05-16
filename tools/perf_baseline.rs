//! 阶段 5 D-535 / API-580..API-589 — c6a.8xlarge on-demand 上跑 perf baseline
//! 测量 update/s + RSS + 3-trial min/mean/max 汇总（D-591 + D-592 字面
//! acceptance protocol 自动化）。
//!
//! ## CLI 用法（A1 \[实现\] scaffold lock；E1 / E2 \[实现\] 落地）
//!
//! ```text
//! cargo run --release --bin perf_baseline -- \
//!     --game nlhe-6max \
//!     --trainer es-mccfr-linear-rm-plus-compact \
//!     --abstraction pluribus-14 \
//!     --bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin \
//!     --naive-baseline tests/data/stage5_naive_baseline.json \
//!     --seed-list 42,43,44 \
//!     --run-wall-seconds 1800 \
//!     --warm-up-wall-seconds 300 \
//!     --threads 32 \
//!     --parallel-batch-size 32 \
//!     --pruning-on \
//!     --output-jsonl perf_baseline.jsonl \
//!     --acceptance-target-update-per-s 200000 \
//!     --acceptance-target-memory-ratio 0.5
//! ```
//!
//! ## D-537 preflight check 字面
//!
//! 启动前必走：
//! 1. `uptime` load average 5min < 0.5 → 否则 abort（host 非 idle）
//! 2. `cpupower frequency-info` 当前 governor == `performance` → 否则 abort
//! 3. `cat /proc/cpuinfo | grep MHz` 32 vCPU 频率一致 ± 5% → 否则 abort
//!    （检测 turbo throttling）
//!
//! preflight fail 不烧 host time；user 修复后重启 binary。`--skip-host-preflight`
//! emergency override（default false）。
//!
//! ## D-538 wall + warm-up + steady-state slicing
//!
//! - 单 run wall = 30 min 实际跑（不含 host boot / build）
//! - warm-up skip = 前 5 min（或前 5e7 update，取后者；5 min wall 更宽容）
//! - steady-state slice = `[warm_up_end, run_end]` 内 metrics.jsonl 每行
//!   `update_per_s_window` 字段 mean
//!
//! ## D-539 SLO PASS 判据
//!
//! `min(3 trials) ≥ 200_000` update/s。**不是 mean ≥ 200K**（防 outlier 通过）。
//! 3 trial fail 任一触发 D-536 同 seed 重测 1 次 retry；连 2 fail 真 fail。
//!
//! ## A1 \[实现\] 状态
//!
//! main 走 stub 退出码 2 + stderr 提示。CLI flag parse + preflight check +
//! run_trial + aggregate_trials + write_acceptance_jsonl 全 `unimplemented!()`
//! 占位。E1 / E2 \[实现\] 起步前落地。

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!(
        "[perf_baseline] stage 5 A1 \\[实现\\] scaffold — actual 3-trial acceptance run\n\
         路径走 E1 / E2 \\[实现\\] 起步前落地（API-580..API-589 字面，详\n\
         `docs/pluribus_stage5_api.md` §6 + `docs/pluribus_stage5_workflow.md` §10\n\
         E1 / E2 entry/exit）。\n\
         \n\
         本 binary 当前仅暴露 16 CLI flag spec + preflight check 字面，**不**\n\
         触发实际计算路径；试图运行将 panic with `unimplemented!()` 提示。"
    );
    ExitCode::from(2)
}
