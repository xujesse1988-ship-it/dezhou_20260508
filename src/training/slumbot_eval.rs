//! Slumbot HU NLHE bridge + Head-to-Head evaluation（API-460..API-463 / D-460..D-469）。
//!
//! stage 4 验收四锚点之一（D-460 字面 Slumbot 100K 手 mean ≥ -10 mbb/g first
//! usable + 95% CI 下界 ≥ -30 mbb/g）。Slumbot 是公开 HU NLHE 对手；blueprint
//! 走 HU 退化路径（[`crate::training::nlhe_6max::NlheGame6::new_hu`]）评测。
//!
//! **A1 \[实现\] 状态**：[`SlumbotBridge`] / [`Head2HeadResult`] / [`SlumbotHandResult`] /
//! [`OpenSpielHuBaseline`] / [`HuHandResult`] struct 签名锁；`new` / `play_one_hand` /
//! `evaluate_blueprint` 全 `unimplemented!()`，F2 \[实现\] 落地走 HTTP 协议双向
//! 交互 + duplicate dealing + 重复 5 次 mean。
//!
//! **D-463-revM**（Slumbot API 不可用 fallback）：[`OpenSpielHuBaseline`] 占位
//! 走 stage 3 既有 OpenSpiel-trained HU policy；F2 \[实现\] 起步前评估翻面
//! 触发。

use crate::core::rng::RngSource;
use crate::error::TrainerError;
use crate::training::nlhe_6max::NlheGame6;
use crate::training::trainer::Trainer;
use std::path::PathBuf;

/// stage 4 D-460 / API-461 — Head-to-Head 评测结果（blueprint 视角 mbb/g
/// 净收益）。
#[derive(Clone, Debug)]
pub struct Head2HeadResult {
    pub mean_mbbg: f64,
    pub standard_error_mbbg: f64,
    /// 95% CI 下界 / 上界（D-461 字面验收阈值；下界 ≥ -30 mbb/g first usable）。
    pub confidence_interval_95: (f64, f64),
    pub n_hands: u64,
    /// D-461 duplicate dealing on/off ablation 标志（true = 重复发同 hole +
    /// board × seat 互换 5 次，让 variance ≈ 0）。
    pub duplicate_dealing: bool,
    pub blueprint_seed: u64,
    pub wall_clock_seconds: f64,
}

/// stage 4 D-460 — 单 hand Slumbot evaluation 结果（blueprint 视角 chip / mbb
/// 净收益）。
#[derive(Clone, Debug)]
pub struct SlumbotHandResult {
    pub blueprint_chip_delta: i64,
    pub mbb_delta: f64,
    pub seed: u64,
    pub wall_clock_seconds: f64,
}

/// stage 4 D-463-revM fallback — 单 hand OpenSpiel HU baseline evaluation
/// 结果（同 [`SlumbotHandResult`] 形态）。
#[derive(Clone, Debug)]
pub struct HuHandResult {
    pub blueprint_chip_delta: i64,
    pub mbb_delta: f64,
    pub seed: u64,
    pub wall_clock_seconds: f64,
}

/// stage 4 Slumbot HTTP bridge（API-460 / D-460）。
///
/// 走 `reqwest::blocking::Client` HTTP 协议（A0 lock blocking 路径，D-463 字面
/// 若必须 async 走 D-463-revM tokio 翻面）；`api_endpoint` 默认指向
/// `http://www.slumbot.com/api/`（D-460 字面）。
///
/// **A1 \[实现\] 状态**：struct 签名锁；`http_client` 字段类型走
/// `reqwest::blocking::Client`（占位让 Cargo.toml `reqwest` 依赖在 A1 \[实现\]
/// commit 立刻消费，避免 unused dependency 警告）；`new` / `play_one_hand` /
/// `evaluate_blueprint` 全 `unimplemented!()`。F2 \[实现\] 落地后字段全部
/// 在 HTTP 协议路径内消费，`#[allow(dead_code)]` 撤销。
#[allow(dead_code)]
pub struct SlumbotBridge {
    pub(crate) http_client: reqwest::blocking::Client,
    pub(crate) api_endpoint: String,
    pub(crate) api_key: Option<String>,
    pub(crate) timeout: std::time::Duration,
}

impl SlumbotBridge {
    /// stage 4 D-460 — 构造（默认 60 s timeout / api_key=None / blocking
    /// client default config）。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，F2 \[实现\] 落地。
    pub fn new(api_endpoint: String) -> Self {
        let _ = api_endpoint;
        unimplemented!("stage 4 A1 [实现] scaffold: SlumbotBridge::new 落地 F2 [实现] D-460")
    }

    /// stage 4 D-460 — 单 hand 评测：blueprint 与 Slumbot 对战 1 手（HU NLHE）。
    ///
    /// 协议双向：
    /// 1. POST `/new_hand` to Slumbot → 返回 Slumbot 初始 state
    /// 2. blueprint act → POST blueprint_action → Slumbot reply
    /// 3. terminal: Slumbot 返回 outcome → mbb 净收益 = chip_delta / BB × 1000
    ///
    /// `seed` 用作 blueprint 内部 RNG seed（duplicate dealing 路径下两次 hand
    /// 用同 seed 让发牌一致）。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，F2 \[实现\] 落地。
    pub fn play_one_hand<T>(
        &mut self,
        blueprint: &T,
        seed: u64,
    ) -> Result<SlumbotHandResult, TrainerError>
    where
        T: Trainer<NlheGame6>,
    {
        let _ = (blueprint, seed);
        unimplemented!(
            "stage 4 A1 [实现] scaffold: SlumbotBridge::play_one_hand 落地 F2 [实现] D-460"
        )
    }

    /// stage 4 D-461 — 100K 手评测（D-460 协议 + duplicate dealing + 重复 5
    /// 次 mean）。
    ///
    /// `n_hands` 通常 100_000；`master_seed` 派生 RNG 序列（D-468 字面）；
    /// `duplicate_dealing = true` 让 variance ≈ 0（同 hole + board × seat 互
    /// 换 5 次）。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，F2 \[实现\] 落地。
    pub fn evaluate_blueprint<T>(
        &mut self,
        blueprint: &T,
        n_hands: u64,
        master_seed: u64,
        duplicate_dealing: bool,
    ) -> Result<Head2HeadResult, TrainerError>
    where
        T: Trainer<NlheGame6>,
    {
        let _ = (blueprint, n_hands, master_seed, duplicate_dealing);
        unimplemented!(
            "stage 4 A1 [实现] scaffold: SlumbotBridge::evaluate_blueprint 落地 F2 [实现] D-461"
        )
    }
}

/// stage 4 D-463-revM — Slumbot API 不可用 fallback baseline。
///
/// 走 OpenSpiel-trained HU NLHE policy 文件（offline 评测，无 HTTP 依赖）；
/// F2 \[实现\] 起步前评估翻面触发（D-463-revM lock）。
///
/// **A1 \[实现\] 状态**：struct 签名锁；`policy_path` 字段占位；`play_one_hand`
/// `unimplemented!()`，F2 \[实现\] 落地走 OpenSpiel policy 文件 byte-equal
/// load + HU 退化路径 evaluator dispatch。
#[allow(dead_code)]
pub struct OpenSpielHuBaseline {
    pub(crate) policy_path: PathBuf,
}

impl OpenSpielHuBaseline {
    /// stage 4 D-463-revM — 构造（policy_path = OpenSpiel-trained HU policy
    /// 文件路径）。
    pub fn new(policy_path: PathBuf) -> Self {
        Self { policy_path }
    }

    /// stage 4 D-463-revM — 单 hand fallback evaluation。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，F2 \[实现\] 落地。
    pub fn play_one_hand<T>(
        &mut self,
        blueprint: &T,
        seed: u64,
        rng: &mut dyn RngSource,
    ) -> Result<HuHandResult, TrainerError>
    where
        T: Trainer<NlheGame6>,
    {
        let _ = (blueprint, seed, rng);
        unimplemented!(
            "stage 4 A1 [实现] scaffold: OpenSpielHuBaseline::play_one_hand 落地 F2 [实现] D-463-revM"
        )
    }
}
