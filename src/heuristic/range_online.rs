//! 在线 ML 对手 range 预测器：把离线已落地（golden 对拍通过）的 range 流水线
//! （[`tokenize`](super::tokenizer::tokenize) → 因果 log m_prior
//! [`RangeModel::causal_logpriors`] → GRU 残差 [`GruForward::residual`] → 牌面移除
//! [`mlrange::predict`]）组合成一个在 live 决策点直接吃 [`GameView`] 出 [`RangeVec`]
//! 的适配器。
//!
//! ⚠ **EV 未验证**：由 [`RangeConfig::use_ml_range`](super::config::RangeConfig) 门控，
//! 默认关。远端 sim A/B（mbb/g 不劣）通过前不翻默认。

use std::path::Path;

use crate::core::Card;
use crate::heuristic::equity::{canonical_index, EquityEngine};
use crate::heuristic::mlrange::{self, GruForward, N_CLASS};
use crate::heuristic::range::{CausalResult, RangeModel, RangeVec};
use crate::heuristic::tokenizer::{tokenize, FDIM};
use crate::heuristic::view::GameView;

/// 在线 ML range 预测器（构造期一次性载入模型 + 权重，决策点反复调用）。
pub struct MlRange {
    model: RangeModel,
    gru: GruForward,
    rscale: f64,
}

impl MlRange {
    /// 从产物路径载入。任一文件缺失/解析失败/维度不符 → `None`（优雅降级，
    /// 调用方回落标量 equity 路径）。
    pub fn load(strength_path: &str, weights_path: &str, rscale: f64) -> Option<MlRange> {
        let sp = Path::new(strength_path);
        let wp = Path::new(weights_path);
        if !sp.exists() || !wp.exists() {
            return None;
        }
        let strength = load_strength(sp)?;
        let engine = EquityEngine::from_strength(strength);
        let model = RangeModel::new(&engine);
        let gru = GruForward::from_json(wp).ok()?;
        if gru.fdim() != FDIM {
            return None;
        }
        Some(MlRange { model, gru, rscale })
    }

    /// 预测 `opp_seat` 在**当前决策点**（迄今全部动作）的 169 类归一化 range。
    ///
    /// 步骤：`to_range_sample` → `tokenize`（flat row-major）→ GRU 残差 → 因果 log
    /// m_prior + 该街残差快照 + 牌面移除 → `mlrange::predict`（softmax 已归一）。
    pub fn predict(&self, view: &GameView, opp_seat: usize) -> RangeVec {
        let sample = view.to_range_sample(opp_seat, None);

        // token 特征 → flat `T*FDIM` row-major（GRU 前向输入口径）。
        let feat = tokenize(&sample);
        let t_len = feat.len();
        let mut flat = Vec::with_capacity(t_len * FDIM);
        for tok in &feat {
            flat.extend_from_slice(tok);
        }
        // tokenize 恒发 CTX+PROF ≥ 2 token；退化空序仍安全兜底成 uniform。
        if t_len == 0 {
            let mut rv = RangeVec {
                mass: [0.0f64; N_CLASS],
            };
            rv.normalize();
            return rv;
        }
        let res = self.gru.residual(&flat, t_len); // Vec<[f64; 169]>
        let cl = self.model.causal_logpriors(&sample);

        // 当前街 index（Preflop=0..River=3；Showdown clamp 到 3）。
        let s = (view.street as usize).min(3);
        // 选一个既有 logprior 又有 snap 的有效街：优先 ≤ s 的最深，否则任意最深。
        let (logprior, res_snap): ([f64; N_CLASS], [f64; N_CLASS]) =
            match pick_snapshot_street(&cl, s) {
                Some(se) => {
                    let lp =
                        cl.street_logprior[se].expect("pick_snapshot_street 保证 logprior Some");
                    let snap = cl.street_snap[se].expect("pick_snapshot_street 保证 snap Some");
                    (lp, res[snap.min(t_len - 1)])
                }
                None => {
                    // 无任何快照（异常输入）：uniform log 先验（全 0）+ 末 token 残差。
                    ([0.0f64; N_CLASS], res[t_len - 1])
                }
            };

        // alive[169]：去死牌（hero hole + 当前 board）后每类存活 combo 数（同 equity.rs 口径）。
        let alive = alive_counts(view);

        let p = mlrange::predict(&logprior, &res_snap, &alive, self.rscale);
        let mut rv = RangeVec { mass: p };
        // softmax 已归一，这里再归一一次守鲁棒（数值兜底，正常路径 no-op）。
        rv.normalize();
        rv
    }
}

/// 选一个既有 log 先验又有 snap 下标的有效街：优先 ≤ `s` 的最深，否则任意最深，否则 `None`。
fn pick_snapshot_street(cl: &CausalResult, s: usize) -> Option<usize> {
    let has_snap = |se: &usize| cl.street_logprior[*se].is_some() && cl.street_snap[*se].is_some();
    (0..=s.min(3))
        .rev()
        .find(has_snap)
        .or_else(|| (0..4).rev().find(has_snap))
}

/// 每 169 类去死牌（hero hole + 当前 board）后的存活 combo 数（口径 = `equity_weighted`
/// 的 `n_alive`：deck − dead → 枚举 avail pair → `canonical_index` 计数）。
fn alive_counts(view: &GameView) -> [i64; N_CLASS] {
    let mut dead: u64 = 0;
    if let Some(hole) = view.hole {
        for c in &hole {
            dead |= 1u64 << c.to_u8();
        }
    }
    for c in &view.board {
        dead |= 1u64 << c.to_u8();
    }
    let avail: Vec<u8> = (0..52u8).filter(|c| dead & (1u64 << c) == 0).collect();
    let mut alive = [0i64; N_CLASS];
    for i in 0..avail.len() {
        for j in (i + 1)..avail.len() {
            let ca = Card::from_u8(avail[i]).expect("avail card <52");
            let cb = Card::from_u8(avail[j]).expect("avail card <52");
            alive[canonical_index(ca, cb)] += 1;
        }
    }
    alive
}

/// 载入 169 强度表（`strength169.json` 的 `strength` 数组）。
fn load_strength(path: &Path) -> Option<[f64; N_CLASS]> {
    #[derive(serde::Deserialize)]
    struct StrengthFile {
        strength: Vec<f64>,
    }
    let bytes = std::fs::read(path).ok()?;
    let sf: StrengthFile = serde_json::from_slice(&bytes).ok()?;
    if sf.strength.len() != N_CLASS {
        return None;
    }
    let mut out = [0.0f64; N_CLASS];
    out.copy_from_slice(&sf.strength);
    Some(out)
}
