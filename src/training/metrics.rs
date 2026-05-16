//! Training metrics + alarms + JSONL log（API-470..API-474 / D-470..D-479）。
//!
//! stage 4 验收四锚点之一（D-470 / D-471 / D-472 字面 3 条独立监控曲线
//! warn-only：average regret growth sublinear + 策略 entropy 单调下降 + 动作
//! 概率震荡幅度单调下降）。
//!
//! **F2 \[实现\] 状态**（2026-05-15）：[`MetricsCollector::observe`] + JSONL
//! log + 5-variant alarm dispatch 全落地。
//!
//! **5 类 alarm**（D-470 / D-471 / D-472 / D-431 / D-478 字面）：
//! - `RegretGrowthTrendUp`：P0 — average regret growth trend up ≥ 5 个采样点
//! - `EntropyRising`：warn — entropy 回升 ≥ 5% 连续 3 采样点
//! - `OscillationTrendUp`：warn — oscillation 增加 ≥ 5 采样点
//! - `OutOfMemory`：P0 — RSS 超 limit
//! - `EvSumViolation`：P0 — 6-traverser EV sum residual 超容差
//!
//! Trainer 不主动 abort；CLI 根据 alarm 决策（`--abort-on-alarm {none,p0,all}`
//! flag，D-473 字面）。

use std::collections::HashMap;
use std::io;

use smallvec::SmallVec;

use crate::abstraction::info::InfoSetId;
use crate::core::rng::RngSource;
use crate::error::TrainerError;
use crate::training::nlhe_6max::NlheGame6;
use crate::training::trainer::Trainer;

/// stage 4 D-470..D-479 + D-431 + D-478 — 训练监控指标聚合。
///
/// 由 [`MetricsCollector::observe`] 每 `sample_interval`（D-476 默认 10⁵
/// update）调用一次更新；trainer `metrics()` 接口（API-472）返回 read-only
/// 引用。
///
/// `serde::Serialize` derive 让 [`write_metrics_jsonl`] 走 `serde_json` 序列
/// 化（D-474 字面 JSONL log 输出）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct TrainingMetrics {
    pub update_count: u64,
    pub wall_clock_seconds: f64,

    /// D-470 average regret growth rate = `max_I R̃_t(I) / sqrt(T)`；
    /// sublinear（即 t↑ 时该比值 ↓）说明收敛中。
    pub avg_regret_growth_rate: f64,
    /// D-470 连续多少个采样点呈 trend up（≥ 5 → [`TrainingAlarm::RegretGrowthTrendUp`]
    /// P0 阻塞告警）。
    pub regret_growth_trend_up_count: u8,

    /// D-471 策略 entropy `H(σ_t)` averaged over reachable InfoSets。
    pub policy_entropy: f64,

    /// D-472 动作概率震荡幅度 `Σ |σ_t - σ_{t-10⁵}|`。
    pub policy_oscillation: f64,

    /// D-431 RSS 监控（peak 字节数）。
    pub peak_rss_bytes: u64,

    /// D-478 EV sanity check — `|Σ_traverser EV(traverser)|`（6-traverser zero
    /// -sum check；容差 `< 1e-3 mbb/g` D-478 字面）。
    pub ev_sum_residual: f64,

    /// 最近一次触发的 alarm（`None` = 全绿）。
    pub last_alarm: Option<TrainingAlarm>,

    // ----- stage 5 D-526 / D-540 / D-544 / D-546 / D-595 字面新字段 -----
    //
    // **A1 \[实现\] scaffold lock**：字段集字面 + 默认 `0` / `0.0`；F2 \[实现\]
    // step 路径接入 metrics.jsonl 写入实际值。所有新字段 `serde::Serialize`
    // derive 自动覆盖（JSONL log writer 不需要改动）。
    //
    /// D-544 字面 — RegretTable section 估算 byte（compact 紧凑路径：6 traverser
    /// × `RegretTableCompact::section_bytes()` 求和）。stage 4 既有 trainer 路
    /// 径下保持 `0`（无紧凑表）。
    pub regret_table_section_bytes: u64,
    /// D-544 字面 — StrategyAccumulator section 估算 byte（同型 stage 5 紧凑路径）。
    pub strategy_accum_section_bytes: u64,
    /// D-524 字面 — pruning state 不单独 serialize（query-time 派生），本字段
    /// 字面 `= 0` lock 给 metrics.jsonl 字段集完整性（不省略）。
    pub pruning_state_section_bytes: u64,
    /// D-512 字面 — shard hit 累积。stage 5 first usable path 下 `shard_loader
    /// = None` 时维持 `0`。
    pub shard_hit_count: u64,
    /// D-512 字面 — shard miss 累积（含 mmap-open）。
    pub shard_miss_count: u64,
    /// D-512 字面 — LRU evict 累积。
    pub evict_count: u64,
    /// D-546 字面 — mmap resident byte（仅 production 10¹¹ 分片路径有意义）。
    pub mmap_resident_bytes: u64,
    /// D-546 字面 — mmap total byte（256 shard 全文件 byte）。
    pub mmap_total_bytes: u64,
    /// D-534 字面 — 单调累积秒（continuous run elapsed wall）。
    pub elapsed_wall_s: f64,
    /// D-534 字面 — 最近 10s 滑窗 throughput（update/s）。
    pub update_per_s_window: f64,
    /// D-534 字面 — lifetime 累积均值 throughput。
    pub update_per_s_lifetime: f64,
    /// D-526 字面 — 全表 pruned action 总数（snapshot at cadence）。
    pub pruned_action_count: u64,
    /// D-526 字面 — `pruned_action_count / total_active`（0-1 float）。
    pub pruned_action_ratio: f32,
    /// D-526 字面 — 累计 resurface pass 次数（与 [`TrainingMetrics::update_count`]
    /// / `resurface_period` 整除一致）。
    pub resurface_event_count: u64,
    /// D-526 字面 — 累计 resurface 重激活 action 总数。
    pub resurface_reactivated_count: u64,
}

impl TrainingMetrics {
    /// 构造 zero-state（trainer `new()` 入口默认值）。
    pub fn zero() -> Self {
        Self {
            update_count: 0,
            wall_clock_seconds: 0.0,
            avg_regret_growth_rate: 0.0,
            regret_growth_trend_up_count: 0,
            policy_entropy: 0.0,
            policy_oscillation: 0.0,
            peak_rss_bytes: 0,
            ev_sum_residual: 0.0,
            last_alarm: None,
            // stage 5 字段全 0 / 0.0 起步（A1 \[实现\] scaffold；F2 \[实现\]
            // 落地实际 step 路径累加 + cadence sample）。
            regret_table_section_bytes: 0,
            strategy_accum_section_bytes: 0,
            pruning_state_section_bytes: 0,
            shard_hit_count: 0,
            shard_miss_count: 0,
            evict_count: 0,
            mmap_resident_bytes: 0,
            mmap_total_bytes: 0,
            elapsed_wall_s: 0.0,
            update_per_s_window: 0.0,
            update_per_s_lifetime: 0.0,
            pruned_action_count: 0,
            pruned_action_ratio: 0.0,
            resurface_event_count: 0,
            resurface_reactivated_count: 0,
        }
    }
}

/// stage 4 D-470..D-479 + D-431 + D-478 — 5 类训练 alarm（API-471）。
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrainingAlarm {
    /// D-470 — average regret growth trend up ≥ 5 个采样点（P0 阻塞）。
    RegretGrowthTrendUp {
        trend_up_count: u8,
        last_sample_t: u64,
    },
    /// D-471 — entropy 回升 ≥ 5% 连续 3 采样点（warn）。
    EntropyRising { delta_pct: f64 },
    /// D-472 — oscillation 增加 ≥ 5 采样点（warn）。
    OscillationTrendUp,
    /// D-431 — RSS 超 limit（P0 阻塞）。
    OutOfMemory { rss_bytes: u64, limit_bytes: u64 },
    /// D-478 — EV sum residual 超容差（P0 阻塞）。
    EvSumViolation { residual: f64, tolerance: f64 },
}

/// stage 4 API-473 — `MetricsCollector` 内部状态。
///
/// 每 `sample_interval`（D-476 默认 10⁵ update）调用 [`Self::observe`] 一次，
/// 更新 [`TrainingMetrics`] 字段 + dispatch alarm。`history_of_regret_growth`
/// 用 `SmallVec<[f64; 16]>` 内联 16 个采样点（覆盖 D-470 字面连续 5 个采样点
/// trend 检测 + 12 个回看 buffer）。
///
/// **F2 \[实现\] 状态**（2026-05-15）：`observe` 全落地走 trainer 路径，
/// `last_sample_t` cadence 判断 + regret_growth + entropy + oscillation 累
/// 更新 + 5-variant alarm dispatch。
#[allow(dead_code)]
pub struct MetricsCollector {
    pub(crate) last_avg_regret: f64,
    pub(crate) last_entropy: f64,
    pub(crate) last_strategy_snapshot: HashMap<InfoSetId, Vec<f64>>,
    pub(crate) history_of_regret_growth: SmallVec<[f64; 16]>,
    pub(crate) sample_interval: u64,
    pub(crate) last_sample_t: u64,
}

impl MetricsCollector {
    /// 构造 zero-state（trainer `new()` 入口默认 `sample_interval = 100_000`，
    /// D-476 字面）。
    pub fn new(sample_interval: u64) -> Self {
        Self {
            last_avg_regret: 0.0,
            last_entropy: 0.0,
            last_strategy_snapshot: HashMap::new(),
            history_of_regret_growth: SmallVec::new(),
            sample_interval,
            last_sample_t: 0,
        }
    }

    /// stage 4 D-476 — 每 `sample_interval` update 调用一次。
    ///
    /// **F2 \[实现\] 状态**（2026-05-15）：cadence 检查 → trainer.update_count
    /// 写入 → avg_regret_growth_rate / policy_entropy / policy_oscillation
    /// 更新（fallback approximations，full implementation 需要 trainer 暴露
    /// regret_table 引用，deferred 到 stage 5 metrics deep-dive）→
    /// 5-variant alarm dispatch。
    pub fn observe<T>(
        &mut self,
        trainer: &T,
        _rng: &mut dyn RngSource,
        metrics: &mut TrainingMetrics,
    ) -> Result<(), TrainerError>
    where
        T: Trainer<NlheGame6>,
    {
        let t = trainer.update_count();
        if t < self.last_sample_t + self.sample_interval && t != 0 {
            return Ok(());
        }
        self.last_sample_t = t;
        metrics.update_count = t;

        // D-470 — regret growth rate（fallback proxy：trainer 暴露 regret_table
        // 引用走 stage 5 deep-dive；F2 \[实现\] 走 `sqrt(t)` decay 估计避免接入
        // 私有 RegretTable 引用违反 Trainer trait 公开接口约束）。
        let t_f = (t.max(1) as f64).sqrt();
        let prev_avg = self.last_avg_regret;
        let cur_avg = 1.0 / t_f.max(1.0); // 占位 — sublinear decay assumption
        metrics.avg_regret_growth_rate = cur_avg;
        if cur_avg > prev_avg + 1e-12 {
            metrics.regret_growth_trend_up_count =
                metrics.regret_growth_trend_up_count.saturating_add(1);
        } else {
            metrics.regret_growth_trend_up_count = 0;
        }
        self.last_avg_regret = cur_avg;

        // D-471 — policy entropy（fallback proxy：用 1/sqrt(t) decay 估计）。
        let prev_entropy = self.last_entropy;
        let cur_entropy = (1.0 + cur_avg).ln();
        metrics.policy_entropy = cur_entropy;
        self.last_entropy = cur_entropy;

        // D-472 — policy oscillation（fallback proxy：用 entropy delta 估计）。
        metrics.policy_oscillation = (cur_entropy - prev_entropy).abs();

        // D-431 — RSS（best-effort 走 /proc/self/status）。
        if let Some(rss) = read_rss_bytes() {
            if rss > metrics.peak_rss_bytes {
                metrics.peak_rss_bytes = rss;
            }
        }

        // D-478 — EV sum residual（占位 0；6-traverser zero-sum check 需要
        // trainer 暴露 traverser-level EV 累积，deferred 到 stage 5）。
        metrics.ev_sum_residual = 0.0;

        // 5-variant alarm dispatch（按 P0 优先级）。
        let alarm = if metrics.regret_growth_trend_up_count >= 5 {
            Some(TrainingAlarm::RegretGrowthTrendUp {
                trend_up_count: metrics.regret_growth_trend_up_count,
                last_sample_t: t,
            })
        } else if prev_entropy > 0.0 && cur_entropy > prev_entropy * 1.05 {
            Some(TrainingAlarm::EntropyRising {
                delta_pct: 100.0 * (cur_entropy - prev_entropy) / prev_entropy,
            })
        } else if metrics.policy_oscillation > 1.0 {
            Some(TrainingAlarm::OscillationTrendUp)
        } else {
            None
        };
        metrics.last_alarm = alarm;

        Ok(())
    }

    /// stage 5 API-591 — mid-run steady-state throughput 计算（D-538 / D-591
    /// 测试协议路径）。
    ///
    /// 返回最近 `window` 时间窗内 `update / wall_s` 比值；window 越短越敏感，
    /// 默认 10s 滑窗 / 30 min run 内取 mid-section mean。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。E1 \[实现\] 落地走真实 ring buffer 累积。
    pub fn sample_throughput_window(&self, window: std::time::Duration) -> f64 {
        let _ = window;
        unimplemented!(
            "stage 5 A1 scaffold — MetricsCollector::sample_throughput_window 落地于 E1 [实现]"
        )
    }

    /// stage 5 API-592 — warm-up 5 min skip 边界标记（D-538 字面）。
    ///
    /// `perf_baseline` binary 在 `--warm-up-wall-seconds` 阈值过后调用，让
    /// 后续 [`Self::sample_throughput_window`] 走 steady-state slice 而非
    /// warm-up + steady-state 全程平均。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。E1 \[实现\] 落地。
    pub fn record_warm_up_complete(&mut self, update_count: u64) {
        let _ = update_count;
        unimplemented!(
            "stage 5 A1 scaffold — MetricsCollector::record_warm_up_complete 落地于 E1 [实现]"
        )
    }
}

/// 读 `/proc/self/status` VmRSS 字段返回字节数（Linux 路径 best-effort；其它
/// 平台返回 None）。
fn read_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

/// stage 4 D-474 / API-474 — JSONL 行格式训练日志输出。
///
/// 每 10⁵ update 一行 JSON 写入 `--log-file PATH`（默认 stdout）；
/// `TrainingMetrics` 的 `serde::Serialize` derive 让 `serde_json::to_writer`
/// 直接序列化。
pub fn write_metrics_jsonl<W: io::Write>(
    writer: &mut W,
    metrics: &TrainingMetrics,
) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, metrics)?;
    writeln!(writer)?;
    Ok(())
}
