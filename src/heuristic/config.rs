//! 全部可调参数（迭代闭环的旋钮面）。
//!
//! 设计原则：**每一个会影响 EV 的魔法数都进 [`HeuristicConfig`]**，给默认值，可被 JSON
//! 覆盖（`HeuristicConfig::from_json_str`）。改启发式 = 改这里 + 跑 `heuristic9_sim` 回归门。
//!
//! 单位约定：筹码量一律 **big blind 倍数（f64）**，与桌大小/盲注无关 → 同一份 config 通吃
//! 任意 stake。Equity / 概率 / 频率都是 `[0,1]` 的 f64。

use serde::{Deserialize, Serialize};

/// 顶层启发式配置。所有子配置都实现 [`Default`]，默认值 = v0.1 经验调参起点。
///
/// Phase B（场景森林，`docs/scenario_heuristic_design.md` §6）起参数分两层：
/// - [`NodesConfig`]（`nodes.*`）：**按场景节点分参**——已拆分节点各持一份阈值/频率；
/// - [`PreflopConfig`] / [`PostflopConfig`]：跨节点**共享层**（尺寸、多人收紧系数、
///   赔率余量等对所有节点生效的地板参数）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HeuristicConfig {
    pub equity: EquityConfig,
    pub preflop: PreflopConfig,
    pub postflop: PostflopConfig,
    pub nodes: NodesConfig,
    pub mixing: MixingConfig,
    pub exploit: ExploitConfig,
    pub range: RangeConfig,
}

impl HeuristicConfig {
    /// 从 JSON 字符串覆盖默认值（缺字段走默认，`serde(default)`）。解析失败返回 `Err`。
    pub fn from_json_str(s: &str) -> Result<Self, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }
}

// ===========================================================================
// 在线 ML 对手 range（EV 未验证，默认 false）
// ===========================================================================

/// 在线 ML range 预测器旋钮。`use_ml_range=true` 时引擎为每个对手用离线已落地
/// （golden 对拍通过）的 range 流水线建 169 类 [`RangeVec`](super::range::RangeVec)，
/// 喂 [`equity_weighted`](super::equity::EquityEngine::equity_weighted) 取代标量
/// [`equity`](super::equity::EquityEngine::equity)。
///
/// ⚠ **EV 未验证**：默认 `false`。远端 sim A/B（mbb/g 不劣于标量路径）通过前**不翻默认**。
/// 产物缺失 → 引擎优雅回落标量路径（构造期日志）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RangeConfig {
    /// 是否用 ML range 加权 equity（默认 false = 标量阈值 equity，与历史路径逐字节一致）。
    pub use_ml_range: bool,
    /// GRU 残差权重 JSON 路径。
    pub ml_weights_path: String,
    /// 169 强度表 JSON 路径（→ `EquityEngine::from_strength` → `RangeModel`）。
    pub ml_strength_path: String,
    /// 残差温度（生产值 0.8，与离线 golden 一致）。
    pub ml_rscale: f64,
}

impl Default for RangeConfig {
    fn default() -> Self {
        RangeConfig {
            use_ml_range: false,
            ml_weights_path: "artifacts/range_ml/range_v1_gru_weights.json".to_string(),
            ml_strength_path: "artifacts/range_ml/strength169.json".to_string(),
            ml_rscale: 0.8,
        }
    }
}

// ===========================================================================
// Equity
// ===========================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct EquityConfig {
    /// 决策点蒙特卡洛 rollout 数。live 每决策有秒级预算 → 可开大；sim/单测用小值提速。
    pub samples: u32,
    /// 翻前 169-hand 强度表（equity-vs-1-random）每手的 rollout 数（启动一次性算）。
    pub preflop_table_samples: u32,
    /// equity MC 的基础 seed（与手牌状态混合，保证可复现）。
    pub seed: u64,
}

impl Default for EquityConfig {
    fn default() -> Self {
        EquityConfig {
            samples: 1200,
            preflop_table_samples: 2500,
            seed: 0x9E37_79B9_7F4A_7C15,
        }
    }
}

// ===========================================================================
// 翻前
// ===========================================================================

/// 翻前**共享层**（Phase B）：只留跨节点生效的尺寸/系数；各节点阈值移入
/// [`NodesConfig`]（`nodes.short` / `nodes.open` / `nodes.limp_behind` /
/// `nodes.vs_open|vs_3bet|vs_4bet`）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PreflopConfig {
    /// 每多一个**已入池**对手，价值门槛上抬多少（多人收紧，所有翻前节点共享）。
    pub multiway_tighten: f64,
    /// 开池尺寸（bb）。9-max 标准 2.2–2.5bb。
    pub open_size_bb: f64,
    /// 每个 limper 额外加的开池尺寸（bb）。
    pub iso_extra_per_limper_bb: f64,
    /// 3bet 尺寸 = 面对的 raise-to 的倍数（IP）/（OOP）。
    pub threebet_mult_ip: f64,
    pub threebet_mult_oop: f64,
}

impl Default for PreflopConfig {
    fn default() -> Self {
        PreflopConfig {
            multiway_tighten: 0.012,
            open_size_bb: 2.3,
            iso_extra_per_limper_bb: 1.0,
            threebet_mult_ip: 3.0,
            threebet_mult_oop: 4.0,
        }
    }
}

// ===========================================================================
// 翻后
// ===========================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PostflopConfig {
    /// 价值下注最低 equity（vs 跟注 range）。
    pub value_equity: f64,
    /// 薄价值在 IP heads-up 时下调的 equity（敢更薄）。
    pub thin_value_ip_bonus: f64,
    /// 诈唬（含半诈唬）的最低「改进 equity」——有听牌才诈唬，纯空气少诈。
    pub semibluff_min_equity: f64,
    /// 纯空气诈唬基础频率（被 fold-equity / 质地 / 人数调制）。
    pub air_bluff_freq: f64,
    /// 底池赔率跟注的 equity 余量（留隐含赔率/方差缓冲，>0 更紧）。
    pub call_equity_margin: f64,
    /// 每多一个 active 对手，价值门槛上抬、诈唬频率下调的系数。
    pub multiway_value_tighten: f64,
    pub multiway_bluff_cut: f64,
    /// 低 SPR 承诺阈值：SPR < 此值且 made-hand ≥ 顶对好踢 → 倾向 stack-off。
    pub commit_spr: f64,
    /// 下注尺寸（底池分数）候选——干面小、湿面大；引擎按质地/SPR 选。
    pub size_small: f64,
    /// ⚠ 暂未接线：`pick_bet_size` 当前只在 `size_small`↔`size_big` 间按 wetness lerp，
    /// 中尺寸档未取用。保留为后续「三档尺寸」预留，改它现在无 EV 影响。
    pub size_mid: f64,
    pub size_big: f64,
    /// overbet 尺寸（坚果 polar）。
    pub size_overbet: f64,
    /// ⚠ 暂未接线：翻后尚无独立 check-raise 逻辑（OOP 强牌走通用 value_raise）。
    /// 保留为后续 check-raise 专家预留，改它现在无 EV 影响。
    pub checkraise_freq: f64,
}

impl Default for PostflopConfig {
    fn default() -> Self {
        PostflopConfig {
            value_equity: 0.58,
            thin_value_ip_bonus: 0.05,
            semibluff_min_equity: 0.28,
            air_bluff_freq: 0.30,
            call_equity_margin: 0.02,
            multiway_value_tighten: 0.03,
            multiway_bluff_cut: 0.45,
            commit_spr: 2.5,
            size_small: 0.33,
            size_mid: 0.55,
            size_big: 0.80,
            size_overbet: 1.30,
            checkraise_freq: 0.18,
        }
    }
}

// ===========================================================================
// 场景节点参数（Phase B 场景森林，`docs/scenario_heuristic_design.md` §3/§6）
// ===========================================================================

/// `pre.short` 推/弃覆盖层参数（短栈时先于常规翻前节点评估）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PreShortNodeConfig {
    /// 有效栈低于此 bb 数进入推/弃覆盖层。
    pub max_eff_bb: f64,
    /// 推全下最低强度（equity-vs-random 标度）。
    pub shove_strength: f64,
}

impl Default for PreShortNodeConfig {
    fn default() -> Self {
        PreShortNodeConfig {
            max_eff_bb: 25.0,
            shove_strength: 0.60,
        }
    }
}

/// `pre.unopened.open` 各位置桶的开池（first-in raise）最低强度阈值
/// （equity-vs-random 标度；BB 面对 limper 复用 `mid`）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PreOpenNodeConfig {
    pub early: f64,
    pub mid: f64,
    pub late: f64,
    pub sb: f64,
}

impl Default for PreOpenNodeConfig {
    fn default() -> Self {
        PreOpenNodeConfig {
            early: 0.585,
            mid: 0.555,
            late: 0.505,
            sb: 0.535,
        }
    }
}

/// `pre.unopened.limp_behind` 投机跟注参数（小对/同连 set-mine，多人且便宜）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PreLimpBehindNodeConfig {
    /// 投机跟注最低强度。
    pub min_strength: f64,
    /// 跟注上限（bb），超过不算「便宜」。
    pub max_call_bb: f64,
}

impl Default for PreLimpBehindNodeConfig {
    fn default() -> Self {
        PreLimpBehindNodeConfig {
            min_strength: 0.51,
            max_call_bb: 1.5,
        }
    }
}

/// `pre.facing_raise.*` 单节点参数形态（每个加注级别一份独立类型——保证 JSON
/// partial 覆盖时缺字段回落**该级别**的默认值，而非跨级别串参）。
/// 字段语义：`reraise` = 价值再加注（3bet/4bet/5bet）最低强度；`call_strength` =
/// 赔率线之外的「想 flat 强牌」强度补充入口；`implied_discount` = 深栈（>40bb）
/// 投机牌隐含赔率折让（所需 equity 下调量），加注级别越高折让越窄（更可能被支配）。
macro_rules! pre_facing_node_config {
    ($name:ident, $reraise:expr, $call:expr, $implied:expr) => {
        #[derive(Clone, Debug, Serialize, Deserialize)]
        #[serde(default)]
        pub struct $name {
            /// 价值再加注最低强度。
            pub reraise: f64,
            /// 强度补充平跟入口。
            pub call_strength: f64,
            /// 深栈投机牌隐含赔率折让。
            pub implied_discount: f64,
        }

        impl Default for $name {
            fn default() -> Self {
                $name {
                    reraise: $reraise,
                    call_strength: $call,
                    implied_discount: $implied,
                }
            }
        }
    };
}

pre_facing_node_config!(VsOpenNodeConfig, 0.64, 0.57, 0.05);
pre_facing_node_config!(Vs3betNodeConfig, 0.70, 0.60, 0.025);
pre_facing_node_config!(Vs4betNodeConfig, 0.70, 0.60, 0.0125);

/// `post.unopened.value_bet.*` 单节点参数（池型分参）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ValueBetNodeConfig {
    /// 价值下注最低 equity（vs 跟注 range）。
    pub value_equity: f64,
}

impl Default for ValueBetNodeConfig {
    fn default() -> Self {
        ValueBetNodeConfig { value_equity: 0.58 }
    }
}

/// `post.unopened.cbet_bluff.*` 单节点参数（池型分参，设计 §3.3 树 B 点名的拆分）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CbetNodeConfig {
    /// 空气 c-bet 基础频率（干面 / 湿面，按湿度 lerp）。
    pub freq_dry: f64,
    pub freq_wet: f64,
}

impl Default for CbetNodeConfig {
    fn default() -> Self {
        CbetNodeConfig {
            freq_dry: 0.80,
            freq_wet: 0.55,
        }
    }
}

/// 池型三槽：`srp` / `threebet`（3bet 池）/ `other`（limped 与 4bet+ ——样本稀，
/// 落 Phase A 父节点 id 兜底，§3.2 规则 2）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ValueBetSplit {
    pub srp: ValueBetNodeConfig,
    pub threebet: ValueBetNodeConfig,
    pub other: ValueBetNodeConfig,
}

/// 池型三槽（c-bet 诈唬），语义同 [`ValueBetSplit`]。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CbetSplit {
    pub srp: CbetNodeConfig,
    pub threebet: CbetNodeConfig,
    pub other: CbetNodeConfig,
}

/// 场景森林节点参数子树（Phase B）。已拆分节点在此分参；未拆分节点沿用
/// `preflop` / `postflop` 共享层。JSON partial 覆盖照常工作（`serde(default)`）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NodesConfig {
    /// `pre.short`：短栈推/弃覆盖层。
    pub short: PreShortNodeConfig,
    /// `pre.unopened.open`：各位置开池阈值。
    pub open: PreOpenNodeConfig,
    /// `pre.unopened.limp_behind`：投机跟注。
    pub limp_behind: PreLimpBehindNodeConfig,
    /// `pre.facing_raise.*.vs_open`：面对单个 open。
    pub vs_open: VsOpenNodeConfig,
    /// `pre.facing_raise.*.vs_3bet`：面对 3bet。
    pub vs_3bet: Vs3betNodeConfig,
    /// `pre.facing_raise.*.vs_4bet`：面对 4bet+（Phase B 新拆节点；`reraise`/
    /// `call_strength` 默认与 vs_3bet 同参起步，遥测攒够样本后按 §4.3 纪律分调）。
    pub vs_4bet: Vs4betNodeConfig,
    /// `post.unopened.value_bet.{srp|3bp}`：价值下注池型分参。
    pub value_bet: ValueBetSplit,
    /// `post.unopened.cbet_bluff.{srp|3bp}`：c-bet 诈唬池型分参。
    pub cbet: CbetSplit,
    /// `post.unopened.float` 频率 = 空气诈唬基础频率 × 此乘子（run31 EV 缺口候选，
    /// 参数化待 Phase C 配靶调优；0.25 试点 sim 正负参半 → 驳回，维持 0.5）。
    pub float_freq_mult: f64,
    /// `post.facing.bluff_raise` 频率 = 空气诈唬基础频率 × 此乘子。0.10 = Phase B
    /// 扫参甜点：0.35→0.20/0.10/0.0 均 10/11 配置正（paired 同 seed），越低越好且
    /// 0.10 与 0.0 不可分；与 run31 live 遥测一致（n=13，−15.5±6.4 bb/hand 显著亏）。
    /// 保留 0.10 而非 0：节点存活才能继续攒 live 样本供 Phase C 复评（死节点无数据）。
    pub bluff_raise_freq_mult: f64,
}

impl Default for NodesConfig {
    fn default() -> Self {
        NodesConfig {
            short: PreShortNodeConfig::default(),
            open: PreOpenNodeConfig::default(),
            limp_behind: PreLimpBehindNodeConfig::default(),
            vs_open: VsOpenNodeConfig::default(),
            vs_3bet: Vs3betNodeConfig::default(),
            vs_4bet: Vs4betNodeConfig::default(),
            value_bet: ValueBetSplit::default(),
            cbet: CbetSplit::default(),
            float_freq_mult: 0.5,
            bluff_raise_freq_mult: 0.10,
        }
    }
}

// ===========================================================================
// 混合 / 受控随机
// ===========================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MixingConfig {
    /// ⚠ 暂未接线：预期语义=边界决策的混合带宽（分数差在此带内的动作按 softmax 概率混合，
    /// 防被简单针对，0=纯贪心）。当前 `policy` 仅用频率门（cbet/semibluff 等），未做边界 softmax
    /// 混合（见设计 backlog #4）。改它现在无 EV 影响。
    pub temperature: f64,
    /// 决策 RNG 的基础 seed（与手牌状态混合）。
    pub seed: u64,
}

impl Default for MixingConfig {
    fn default() -> Self {
        MixingConfig {
            temperature: 0.10,
            seed: 0xD1B5_4A32_D192_ED03,
        }
    }
}

// ===========================================================================
// 剥削（对手画像驱动）
// ===========================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ExploitConfig {
    /// 开启剥削调整需要的最少观测手数（样本不足走 GTO-ish 默认）。
    pub min_hands: u32,
    /// VPIP 高于此 = 跟注站/松，价值门槛下调、诈唬下调。
    pub station_vpip: f64,
    /// VPIP 低于此 = 紧手/nit，加大偷盲、对其下注更尊重。
    pub nit_vpip: f64,
    /// AF（aggression factor）高于此 = 激进/疯子，放宽抓诈跟注。
    pub maniac_af: f64,
    /// 各类调整的最大幅度（equity 阈值偏移）。0.12 = sim 扫参验证的甜点：station/maniac/
    /// mixed 显著提升、nit 持平；≥0.16 时对 nit 过偷反被惩罚（断崖），故止于 0.12。
    pub adjust_cap: f64,
    /// 对**已分类跟注站**的价值下注/加注尺寸乘子（仅价值动作放大，诈唬不动）。站跟更大注 →
    /// 价值注做大多榨筹码。仅在场上有分类 station 时生效（剥削层，非默认改动）；无 station 或
    /// 未开 exploit 时 = 1.0 不变。1.0 = 关闭。1.30 = sim 扫参甜点：`--field station` 6max 6/6
    /// seed 正（均 +112 mbb/g）、9max 多 seed 正（均 +48~71）、`--field mixed` 5/6 seed 正
    /// （均 +106/+73）、tag-9max +100、nit/HU byte-equal 不退；≥1.40 时 9max station 转负（过大）。
    pub station_value_size_mult: f64,
}

impl Default for ExploitConfig {
    fn default() -> Self {
        ExploitConfig {
            min_hands: 18,
            station_vpip: 0.45,
            nit_vpip: 0.16,
            maniac_af: 3.5,
            adjust_cap: 0.12,
            station_value_size_mult: 1.30,
        }
    }
}
