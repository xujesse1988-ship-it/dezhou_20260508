//! 6-max blueprint 训练的收敛监控（`docs/six_max_nlhe_target.md` S4）。
//!
//! **为什么需要**：6-max 是多人一般和博弈，CFR 自对弈**不保证收敛 Nash**
//! （多人 Nash 计算 PPAD-hard），LBR / exploitability 失去理论意义。S4 因此把质量
//! 闸门从「打到 floor」换成**监控**——average-regret 应 sublinear、策略 entropy、
//! 动作概率震荡幅度——一旦 regret 线性增长 / entropy 不降 / 概率持续大幅震荡，
//! 就是训练发散的告警信号，必须能定位。本模块提供这套监控。
//!
//! **采样策略**：监控不扫全表（230M infoset），只盯一组**每手必访**的 preflop
//! 信息集 = betting tree 根决策节点（6-max = UTG 开池 / HU = SB）× 169 个 lossless
//! 手型类。这组信息集每手都被发到、CFR 每轮都更新，其 average strategy 的收敛
//! （「开池范围是否稳定」）是 blueprint 是否在凝固的典范信号，且查询成本 O(169)
//! 与表规模无关。
//!
//! **后端无关**：通过 [`StrategySnapshot`] trait 抽象，HashMap
//! ([`crate::training::trainer::EsMccfrTrainer`]) 与 dense
//! ([`crate::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer`]) 两后端各实现一份
//! （都在本 crate 内，可访问 `pub(crate)` 表内部）。监控只读、不触训练状态 → 不破
//! byte-equal。

use std::fmt;

use crate::abstraction::info::{InfoSetId, StreetTag};
use crate::training::nlhe::{pack_info_set_v2, SimplifiedNlheGame};

/// preflop lossless 手型类数（pairs 13 + suited 78 + offsuit 78）。
const PREFLOP_CLASSES: u32 = 169;

/// 后端无关的策略 / regret 只读快照（监控用）。
///
/// 两后端「未访问信息集」约定一致 = 返回**空 `Vec`**（HashMap 缺 key；dense 行
/// 未 touch）。监控据此把样本分成 active（已访问）/ inactive，指标只在 active 上算。
pub trait StrategySnapshot {
    /// 信息集的 average strategy（strategy_sum 归一化）；未访问返回空 `Vec`。
    fn average_strategy_for(&self, info: InfoSetId) -> Vec<f64>;
    /// 信息集的逐动作累计 regret（含负）；未访问返回空 `Vec`。
    fn regret_for(&self, info: InfoSetId) -> Vec<f64>;
    /// 全表已访问信息集总数（覆盖率诊断）。
    fn visited_infosets(&self) -> u64;
}

/// 单次 [`ConvergenceMonitor::observe`] 的监控快照。
#[derive(Clone, Debug)]
pub struct MonitorReport {
    /// 观测时的 update 数。
    pub update_count: u64,
    /// 监控样本信息集总数（= 根节点 × 169 手型类）。
    pub sample_size: usize,
    /// 样本中已被访问（average strategy 非空）的信息集数。
    pub active_in_sample: usize,
    /// 全表已访问信息集总数（覆盖率）。
    pub visited_infosets: u64,
    /// active 样本上 average strategy 的平均 Shannon entropy（nats）。
    /// 收敛中应随策略锐化而下降；持平在 `ln(n_actions)` 附近 = 没在学。
    pub mean_entropy: f64,
    /// active 样本上「平均正 regret」= mean_I( Σ_a max(0, R_I(a)) / update_count )。
    /// CFR 理论：average regret → 0（sublinear total regret）。持续不降 / 上升 = 告警。
    pub mean_avg_positive_regret: f64,
    /// 相对上次观测，average strategy 的平均 L1 漂移（动作概率震荡幅度）。
    /// 收敛中应随 update 增加而缩小；首次观测为 `None`。
    pub mean_strategy_drift_l1: Option<f64>,
    /// 同上的最大 L1 漂移（单点震荡上界）。
    pub max_strategy_drift_l1: Option<f64>,
}

impl fmt::Display for MonitorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[monitor] update={} sample_active={}/{} visited_infosets={} \
             entropy={:.4} avg_pos_regret={:.4}",
            self.update_count,
            self.active_in_sample,
            self.sample_size,
            self.visited_infosets,
            self.mean_entropy,
            self.mean_avg_positive_regret,
        )?;
        match (self.mean_strategy_drift_l1, self.max_strategy_drift_l1) {
            (Some(mean), Some(max)) => write!(f, " drift_l1={mean:.5}(max={max:.5})"),
            _ => write!(f, " drift_l1=n/a(first)"),
        }
    }
}

/// 收敛监控器：持有 preflop 根节点信息集样本 + 上次观测的 average strategy（算漂移）。
///
/// 用法：训练前 [`ConvergenceMonitor::for_game`] 构造一次，每 report 间隔调
/// [`ConvergenceMonitor::observe`] 拿一份 [`MonitorReport`]。
pub struct ConvergenceMonitor {
    /// 监控样本信息集（根节点 × 169 手型类，按手型类升序）。
    sample: Vec<InfoSetId>,
    /// 上次观测时各样本的 average strategy（与 `sample` 同序；未访问存空 `Vec`）。
    prev_avg: Option<Vec<Vec<f64>>>,
}

impl ConvergenceMonitor {
    /// 从 game 构造监控器。样本 = betting tree 根决策节点 × 169 个 preflop 手型类。
    ///
    /// panic：根节点不是 preflop（不该发生——任何 NLHE 树根都是 preflop 首个决策）。
    pub fn for_game(game: &SimplifiedNlheGame) -> Self {
        let root_id = game.tree().root_id();
        let root_street = game.tree().node(root_id).street;
        assert_eq!(
            root_street,
            StreetTag::Preflop,
            "ConvergenceMonitor: 根节点应为 Preflop（实得 {root_street:?}）"
        );
        let sample: Vec<InfoSetId> = (0..PREFLOP_CLASSES)
            .map(|class| pack_info_set_v2(class, root_id, StreetTag::Preflop))
            .collect();
        Self {
            sample,
            prev_avg: None,
        }
    }

    /// 监控样本大小（= 169）。
    pub fn sample_size(&self) -> usize {
        self.sample.len()
    }

    /// 监控样本信息集（preflop 根 × 169 手型类，按手型类升序）。诊断 / 测试用：
    /// checkpoint 往返「策略查询一致」对照在这组信息集上做。
    pub fn sample(&self) -> &[InfoSetId] {
        &self.sample
    }

    /// 对当前训练状态做一次观测，返回 [`MonitorReport`] 并把本次 average strategy
    /// 存为下次漂移基准。指标只在 active（average strategy 非空）样本上算。
    pub fn observe<S: StrategySnapshot>(&mut self, update_count: u64, snap: &S) -> MonitorReport {
        let cur_avg: Vec<Vec<f64>> = self
            .sample
            .iter()
            .map(|&info| snap.average_strategy_for(info))
            .collect();

        let mut active = 0usize;
        let mut entropy_sum = 0.0_f64;
        let mut regret_sum = 0.0_f64;
        let denom = update_count.max(1) as f64;

        for (idx, avg) in cur_avg.iter().enumerate() {
            if avg.is_empty() {
                continue;
            }
            active += 1;
            entropy_sum += shannon_entropy(avg);
            let regret = snap.regret_for(self.sample[idx]);
            // regret 非空时与 avg 同动作数；空（理论上 active 必非空）按 0 计。
            let positive: f64 = regret.iter().map(|r| r.max(0.0)).sum();
            regret_sum += positive / denom;
        }

        let (mean_entropy, mean_avg_positive_regret) = if active > 0 {
            (entropy_sum / active as f64, regret_sum / active as f64)
        } else {
            (0.0, 0.0)
        };

        // 漂移：对上次也 active 的样本算 L1(cur, prev)。
        let (mean_drift, max_drift) = match &self.prev_avg {
            Some(prev) => {
                let mut drift_sum = 0.0_f64;
                let mut drift_max = 0.0_f64;
                let mut n = 0usize;
                for (cur, old) in cur_avg.iter().zip(prev.iter()) {
                    if cur.is_empty() || old.is_empty() || cur.len() != old.len() {
                        continue;
                    }
                    let l1: f64 = cur.iter().zip(old).map(|(a, b)| (a - b).abs()).sum();
                    drift_sum += l1;
                    if l1 > drift_max {
                        drift_max = l1;
                    }
                    n += 1;
                }
                if n > 0 {
                    (Some(drift_sum / n as f64), Some(drift_max))
                } else {
                    (None, None)
                }
            }
            None => (None, None),
        };

        self.prev_avg = Some(cur_avg);

        MonitorReport {
            update_count,
            sample_size: self.sample.len(),
            active_in_sample: active,
            visited_infosets: snap.visited_infosets(),
            mean_entropy,
            mean_avg_positive_regret,
            mean_strategy_drift_l1: mean_drift,
            max_strategy_drift_l1: max_drift,
        }
    }
}

/// Shannon entropy（nats）：`-Σ p ln p`，跳过 `p <= 0`。输入应是归一化概率分布
/// （average strategy 已归一化）；不重新归一化以保证与策略表逐位一致。
fn shannon_entropy(dist: &[f64]) -> f64 {
    let mut h = 0.0_f64;
    for &p in dist {
        if p > 0.0 {
            h -= p * p.ln();
        }
    }
    h
}

// ===========================================================================
// 后端实现：HashMap (EsMccfrTrainer<SimplifiedNlheGame>) + dense (DenseNlheEsMccfrTrainer)
// ===========================================================================

impl StrategySnapshot for crate::training::trainer::EsMccfrTrainer<SimplifiedNlheGame> {
    fn average_strategy_for(&self, info: InfoSetId) -> Vec<f64> {
        // Trainer trait 的 average_strategy：两表都缺 key → 空 Vec（未访问约定）。
        <Self as crate::training::Trainer<SimplifiedNlheGame>>::average_strategy(self, &info)
    }

    fn regret_for(&self, info: InfoSetId) -> Vec<f64> {
        // regret_table().inner() 是 HashMap<InfoSetId, Vec<f64>>；缺 key = 未访问 → 空 Vec。
        self.regret_table()
            .inner()
            .get(&info)
            .cloned()
            .unwrap_or_default()
    }

    fn visited_infosets(&self) -> u64 {
        self.regret_table().inner().len() as u64
    }
}

impl StrategySnapshot for crate::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer {
    fn average_strategy_for(&self, info: InfoSetId) -> Vec<f64> {
        // dense average_strategy：两表都没 touch → 空 Vec（与 HashMap 同约定）。
        self.average_strategy(info)
    }

    fn regret_for(&self, info: InfoSetId) -> Vec<f64> {
        // 未 touch 的 regret 行恒为全 0；按未访问约定返回空 Vec，让监控正确判 active。
        let reg = self.regret_table();
        let row = reg.indexer().locate(info).row_index;
        if reg.touched_row(row) {
            reg.row_values_by_info(info)
        } else {
            Vec::new()
        }
    }

    fn visited_infosets(&self) -> u64 {
        self.regret_table().touched_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// 测试用合成快照：直接喂 (info → average, regret)，验指标数学。
    struct StubSnapshot {
        avg: HashMap<u64, Vec<f64>>,
        reg: HashMap<u64, Vec<f64>>,
        visited: u64,
    }

    impl StrategySnapshot for StubSnapshot {
        fn average_strategy_for(&self, info: InfoSetId) -> Vec<f64> {
            self.avg.get(&info.raw()).cloned().unwrap_or_default()
        }
        fn regret_for(&self, info: InfoSetId) -> Vec<f64> {
            self.reg.get(&info.raw()).cloned().unwrap_or_default()
        }
        fn visited_infosets(&self) -> u64 {
            self.visited
        }
    }

    /// entropy：均匀分布 = ln(n)，确定性 one-hot = 0。
    #[test]
    fn entropy_bounds() {
        let n = 6;
        let uniform = vec![1.0 / n as f64; n];
        let h_uniform = shannon_entropy(&uniform);
        assert!(
            (h_uniform - (n as f64).ln()).abs() < 1e-12,
            "均匀分布 entropy 应 == ln(n)，实得 {h_uniform}"
        );
        let one_hot = {
            let mut v = vec![0.0; n];
            v[2] = 1.0;
            v
        };
        assert!(
            shannon_entropy(&one_hot).abs() < 1e-12,
            "one-hot entropy 应 == 0"
        );
        // 中间值：entropy 严格在 (0, ln n) 之间。
        let skewed = vec![0.5, 0.3, 0.1, 0.05, 0.03, 0.02];
        let h = shannon_entropy(&skewed);
        assert!(
            h > 0.0 && h < (n as f64).ln(),
            "skewed entropy 应在 (0, ln n)"
        );
    }

    /// observe：active 计数、漂移计算、平均正 regret（只取正分量 / update_count）。
    #[test]
    fn observe_metrics_on_synthetic_sequence() {
        // 手搓 3 个样本信息集的 raw key，与 monitor 内部 sample 对齐：sample 是
        // pack_info_set_v2(class, root_id, Preflop)。这里不构造真 game，而是直接
        // 替换 monitor.sample 为已知 key（测试专用，验指标数学）。
        let infos: Vec<InfoSetId> = (0..3)
            .map(|c| pack_info_set_v2(c, 7, StreetTag::Preflop))
            .collect();
        let mut monitor = ConvergenceMonitor {
            sample: infos.clone(),
            prev_avg: None,
        };

        // 第 1 次观测：info0 active(均匀 3 动作)，info1 active(one-hot)，info2 未访问。
        let mut avg = HashMap::new();
        avg.insert(infos[0].raw(), vec![1.0 / 3.0; 3]);
        avg.insert(infos[1].raw(), vec![1.0, 0.0, 0.0]);
        let mut reg = HashMap::new();
        reg.insert(infos[0].raw(), vec![3.0, -1.0, 0.0]); // 正分量和 = 3
        reg.insert(infos[1].raw(), vec![10.0, 0.0, 0.0]); // 正分量和 = 10
        let snap1 = StubSnapshot {
            avg,
            reg,
            visited: 2,
        };
        let r1 = monitor.observe(100, &snap1);
        assert_eq!(r1.sample_size, 3);
        assert_eq!(r1.active_in_sample, 2, "info0/info1 active，info2 未访问");
        assert_eq!(r1.visited_infosets, 2);
        // entropy 均值 = (ln3 + 0) / 2
        let expected_entropy = ((3.0_f64).ln() + 0.0) / 2.0;
        assert!((r1.mean_entropy - expected_entropy).abs() < 1e-12);
        // 平均正 regret = ((3/100) + (10/100)) / 2 = 0.065
        assert!((r1.mean_avg_positive_regret - 0.065).abs() < 1e-12);
        // 首次无漂移
        assert!(r1.mean_strategy_drift_l1.is_none());

        // 第 2 次观测：info0 策略变到 [0.5,0.25,0.25]，info1 不变；info2 仍未访问。
        let mut avg2 = HashMap::new();
        avg2.insert(infos[0].raw(), vec![0.5, 0.25, 0.25]);
        avg2.insert(infos[1].raw(), vec![1.0, 0.0, 0.0]);
        let snap2 = StubSnapshot {
            avg: avg2,
            reg: HashMap::new(),
            visited: 2,
        };
        let r2 = monitor.observe(200, &snap2);
        // 漂移只在两次都 active 的 info0/info1 上算。
        // info0 L1 = |0.5-1/3| + |0.25-1/3| + |0.25-1/3| = 1/6 + 1/12 + 1/12 = 1/3
        // info1 L1 = 0
        let drift0 = (0.5_f64 - 1.0 / 3.0).abs() + 2.0 * (0.25_f64 - 1.0 / 3.0).abs();
        let expected_mean_drift = (drift0 + 0.0) / 2.0;
        assert!(
            (r2.mean_strategy_drift_l1.unwrap() - expected_mean_drift).abs() < 1e-12,
            "mean drift 应 == {expected_mean_drift}，实得 {:?}",
            r2.mean_strategy_drift_l1
        );
        assert!(
            (r2.max_strategy_drift_l1.unwrap() - drift0).abs() < 1e-12,
            "max drift 应 == info0 的 {drift0}"
        );
    }
}
