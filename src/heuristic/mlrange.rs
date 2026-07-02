//! M3 Rust 前向：单层 GRU 先验残差模型的纯 f64 手写前向 + 端到端 range 预测组合。
//!
//! 逐义移植 `tools/range_shadow.py` 的 `NpResidualGRU.residual`（输入线性+tanh → 单层
//! GRU → 零残差线性头），权重从 `artifacts/range_ml/range_v1_gru_weights.json` 载入
//! （row-major flat f64，是原 f32 的精确 f64 表示）。GRU 门序 `[r, z, n]`（PyTorch 约定）。
//!
//! [`predict`] 把因果 log m_prior（来自 [`super::range::RangeModel::causal_logpriors`]）+
//! 神经残差 + 牌面移除（`log alive` + 死类掩 −1e30）组合成归一化的 169 类预测，复刻
//! `range_train.snapshot_eval` / `range_shadow.snapshot_metrics` 的 `pred`（rscale=0.8）。

use std::path::Path;

/// 类别数（169 个起手牌等价类）。
pub const N_CLASS: usize = 169;

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// 单层 GRU 先验残差前向（纯 f64，无 torch/ndarray 依赖）。
pub struct GruForward {
    fdim: usize,
    d: usize,
    // inp: [d, fdim] row-major flat；inp_b: [d]。
    inp_w: Vec<f64>,
    inp_b: Vec<f64>,
    // gru: [3d, d] row-major flat（门序 r,z,n）；bias: [3d]。
    w_ih: Vec<f64>,
    w_hh: Vec<f64>,
    b_ih: Vec<f64>,
    b_hh: Vec<f64>,
    // head: [169, d] row-major flat；head_b: [169]。
    head_w: Vec<f64>,
    head_b: Vec<f64>,
}

// --- 权重 JSON 反序列化 ---

#[derive(serde::Deserialize)]
struct WeightsFile {
    meta: Meta,
    weights: Weights,
}

#[derive(serde::Deserialize)]
struct Meta {
    fdim: usize,
    d: usize,
    n_class: usize,
}

#[derive(serde::Deserialize)]
struct Weights {
    #[serde(rename = "inp.weight")]
    inp_weight: Vec<f64>,
    #[serde(rename = "inp.bias")]
    inp_bias: Vec<f64>,
    #[serde(rename = "gru.weight_ih_l0")]
    gru_weight_ih_l0: Vec<f64>,
    #[serde(rename = "gru.weight_hh_l0")]
    gru_weight_hh_l0: Vec<f64>,
    #[serde(rename = "gru.bias_ih_l0")]
    gru_bias_ih_l0: Vec<f64>,
    #[serde(rename = "gru.bias_hh_l0")]
    gru_bias_hh_l0: Vec<f64>,
    #[serde(rename = "head.weight")]
    head_weight: Vec<f64>,
    #[serde(rename = "head.bias")]
    head_bias: Vec<f64>,
}

impl GruForward {
    /// 从导出的权重 JSON 载入（serde_json）。校验各数组长度与 meta 声明一致。
    pub fn from_json(path: &Path) -> std::io::Result<GruForward> {
        let bytes = std::fs::read(path)?;
        let wf: WeightsFile = serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let fdim = wf.meta.fdim;
        let d = wf.meta.d;

        let check = |name: &str, got: usize, want: usize| -> std::io::Result<()> {
            if got != want {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{name} 长度 {got} ≠ 期望 {want}"),
                ));
            }
            Ok(())
        };
        check("n_class", wf.meta.n_class, N_CLASS)?;
        check("inp.weight", wf.weights.inp_weight.len(), d * fdim)?;
        check("inp.bias", wf.weights.inp_bias.len(), d)?;
        check(
            "gru.weight_ih_l0",
            wf.weights.gru_weight_ih_l0.len(),
            3 * d * d,
        )?;
        check(
            "gru.weight_hh_l0",
            wf.weights.gru_weight_hh_l0.len(),
            3 * d * d,
        )?;
        check("gru.bias_ih_l0", wf.weights.gru_bias_ih_l0.len(), 3 * d)?;
        check("gru.bias_hh_l0", wf.weights.gru_bias_hh_l0.len(), 3 * d)?;
        check("head.weight", wf.weights.head_weight.len(), N_CLASS * d)?;
        check("head.bias", wf.weights.head_bias.len(), N_CLASS)?;

        Ok(GruForward {
            fdim,
            d,
            inp_w: wf.weights.inp_weight,
            inp_b: wf.weights.inp_bias,
            w_ih: wf.weights.gru_weight_ih_l0,
            w_hh: wf.weights.gru_weight_hh_l0,
            b_ih: wf.weights.gru_bias_ih_l0,
            b_hh: wf.weights.gru_bias_hh_l0,
            head_w: wf.weights.head_weight,
            head_b: wf.weights.head_bias,
        })
    }

    /// 特征维度（72）。
    pub fn fdim(&self) -> usize {
        self.fdim
    }

    /// 隐层维度（192）。
    pub fn hidden(&self) -> usize {
        self.d
    }

    /// 因果前向：`feat` = flat `T*fdim` row-major → 残差 logits `[T][169]`。
    ///
    /// 逐义等价 `NpResidualGRU.residual`：
    ///   `x = tanh(feat @ inp_w.T + inp_b)`；`gi = x @ W_ih.T + b_ih`；单序列 GRU 递推
    ///   （门序 r,z,n）；`residual = H @ head_w.T + head_b`。全程 f64。
    pub fn residual(&self, feat: &[f64], t_len: usize) -> Vec<[f64; N_CLASS]> {
        let fdim = self.fdim;
        let d = self.d;
        assert_eq!(feat.len(), t_len * fdim, "feat 长度须为 T*fdim");

        // 预算输入门：gi[t] = W_ih · x[t] + b_ih，其中 x[t] = tanh(inp_w · feat[t] + inp_b)。
        // 逐 token 现算，避免额外分配整块 x。
        let mut gi = vec![0.0f64; t_len * 3 * d]; // [T, 3d]
        let mut x = vec![0.0f64; d]; // 单 token 的 x[t]
        for t in 0..t_len {
            let ft = &feat[t * fdim..(t + 1) * fdim];
            // x[t,o] = tanh(sum_k feat[t,k]*inp_w[o,k] + inp_b[o])
            for (o, xo) in x.iter_mut().enumerate() {
                let row = &self.inp_w[o * fdim..(o + 1) * fdim];
                let mut acc = self.inp_b[o];
                for (fk, rk) in ft.iter().zip(row.iter()) {
                    acc += fk * rk;
                }
                *xo = acc.tanh();
            }
            // gi[t,g] = sum_o x[t,o]*W_ih[g,o] + b_ih[g]，g in 0..3d
            let gi_t = &mut gi[t * 3 * d..(t + 1) * 3 * d];
            for (g, gt) in gi_t.iter_mut().enumerate() {
                let row = &self.w_ih[g * d..(g + 1) * d];
                let mut acc = self.b_ih[g];
                for (xo, ro) in x.iter().zip(row.iter()) {
                    acc += xo * ro;
                }
                *gt = acc;
            }
        }

        // GRU 递推 + 头线性。
        let mut out = vec![[0.0f64; N_CLASS]; t_len];
        let mut h = vec![0.0f64; d];
        let mut gh = vec![0.0f64; 3 * d];
        for t in 0..t_len {
            // gh[g] = sum_o h[o]*W_hh[g,o] + b_hh[g]
            for (g, gv) in gh.iter_mut().enumerate() {
                let row = &self.w_hh[g * d..(g + 1) * d];
                let mut acc = self.b_hh[g];
                for (ho, ro) in h.iter().zip(row.iter()) {
                    acc += ho * ro;
                }
                *gv = acc;
            }
            let gi_t = &gi[t * 3 * d..(t + 1) * 3 * d];
            // 门序 [r, z, n]。
            for o in 0..d {
                let ir = gi_t[o];
                let iz = gi_t[d + o];
                let in_ = gi_t[2 * d + o];
                let hr = gh[o];
                let hz = gh[d + o];
                let hn = gh[2 * d + o];
                let r = sigmoid(ir + hr);
                let z = sigmoid(iz + hz);
                let n = (in_ + r * hn).tanh();
                h[o] = (1.0 - z) * n + z * h[o];
            }
            // residual[t,c] = sum_o H[t,o]*head_w[c,o] + head_b[c]
            for (c, oc) in out[t].iter_mut().enumerate() {
                let row = &self.head_w[c * d..(c + 1) * d];
                let mut acc = self.head_b[c];
                for (ho, ro) in h.iter().zip(row.iter()) {
                    acc += ho * ro;
                }
                *oc = acc;
            }
        }
        out
    }
}

/// 组合因果 log m_prior + 神经残差 + 牌面移除 → 归一化 169 类预测。
///
/// 逐义复刻 `range_train.snapshot_eval` / `range_shadow.snapshot_metrics`（rscale=0.8）：
///   `la = ln(max(alive, 1))`；`logits = logprior + rscale*residual + la`；死类（alive==0）
///   掩 −1e30；`softmax`（减 max 后 exp）。
pub fn predict(
    logprior: &[f64; N_CLASS],
    residual_snap: &[f64; N_CLASS],
    alive: &[i64],
    rscale: f64,
) -> [f64; N_CLASS] {
    assert_eq!(alive.len(), N_CLASS, "alive 长度须 169");
    let mut logits = [0.0f64; N_CLASS];
    for (l, ((&lp, &r), &a)) in logits
        .iter_mut()
        .zip(logprior.iter().zip(residual_snap.iter()).zip(alive.iter()))
    {
        *l = if a == 0 {
            -1e30
        } else {
            lp + rscale * r + (a as f64).max(1.0).ln()
        };
    }
    let maxl = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut p = [0.0f64; N_CLASS];
    let mut sum = 0.0;
    for (pi, &l) in p.iter_mut().zip(logits.iter()) {
        *pi = (l - maxl).exp();
        sum += *pi;
    }
    for pi in p.iter_mut() {
        *pi /= sum;
    }
    p
}
