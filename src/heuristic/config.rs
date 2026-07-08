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

/// 池型三槽（c-bet 诈唬），语义同 [`ValueBetSplit`]。`srp` / `other` 用通用默认
/// （0.80/0.55）；`threebet` **默认更低（0.30/0.20）**——Phase C 第一轮扫参结论：3bet 池
/// 空气 c-bet 的 fold-equity 结构性低于 srp（低 SPR、大池、villain 跟 3bet 后 range 更强更
/// 粘），srp 频率放到 3bet 池是过度诈唬。造靶 wideopen-2max（hero 作 3bettor 进翻后）上
/// 0.30/0.20 vs srp 默认 0.80/0.55：9/9 seed 一致 +23 mbb/g（无 exploit 时同向 → 结构性、
/// 非画像分类假象），标准回归矩阵近不变（3bet 池在标准场稀疏）；详见
/// `docs/heuristic9_design.md` §6「Phase C 第一轮」。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CbetSplit {
    pub srp: CbetNodeConfig,
    pub threebet: CbetNodeConfig,
    pub other: CbetNodeConfig,
}

impl Default for CbetSplit {
    fn default() -> Self {
        CbetSplit {
            srp: CbetNodeConfig::default(),
            threebet: CbetNodeConfig {
                freq_dry: 0.30,
                freq_wet: 0.20,
            },
            other: CbetNodeConfig::default(),
        }
    }
}

/// `post.unopened.float` 节点级门控参数（P0-3 阶段2，`docs/scenario_heuristic_design.md` §5.2/§7）。
///
/// float = hero 无主动权、空气、本街无人下注时的延迟 stab 诈唬。其最优频率随**对手翻后
/// fold-to-stab 反号**（造靶实测：粘站 wideopen float≈0 最优、爱弃进攻方 3bettor float≈高
/// 最优，且 exploit 无关）→ 任何全局固定乘子必错（Phase B/C 两次驳回）。正确修法 = 按
/// **面对 hero stab 的 villain 在对偶 `(街.池型)` 节点的 fold 读数**单调门控。
///
/// **⚠ 避开朴素 breakeven 陷阱**：不用 `f > s/(s+P)` 直接开关（wideopen fold 47% > 0.5-pot
/// breakeven 33% 会误判「该 float」，但实测最优是 0——延迟/OOP stab 未弃的对手续街、hero
/// 空气续街≈0 equity → 朴素 breakeven 高估）。故 fold 只作**驱动信号 + 单调映射**，映射的
/// 阈值/斜率在造靶上校准使其复现已知最优（见默认值注释）。
///
/// **冷启动/未知/小样本安全**：`node_gated=false`、任一对手无名（exploit 关）、或 binding
/// villain 样本 `n < min_samples` → 回退固定 [`NodesConfig::float_freq_mult`]（= 现默认
/// 行为）→ 保标准矩阵不退、exploit 关逐字节一致。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct FloatGateConfig {
    /// 是否启用节点级门控（默认 true = 校准后新行为）。false → 恒用 `float_freq_mult` 固定乘子
    /// （= Phase B/C 的旧路径，用于 paired 对照与关闭剥削）。
    pub node_gated: bool,
    /// 信任 villain fold 读数所需的最小 facing 机会数 `n`（binding villain）。低于此 → 回退
    /// 固定乘子（§H 分特征收敛门；float 对偶 = fold-to-cbet/barrel，中位样本饥饿 → 门设小）。
    pub min_samples: u32,
    /// fold 率收缩伪计数强度 `k`（Beta-Binomial；与阶段1 dump 默认一致）。
    pub shrink_k: f64,
    /// 单调映射低锚：shrunk_fold ≤ `fold_lo` → 乘子 = `mult_lo`。
    pub fold_lo: f64,
    pub mult_lo: f64,
    /// 单调映射高锚：shrunk_fold ≥ `fold_hi` → 乘子 = `mult_hi`；两锚间线性插值。
    pub fold_hi: f64,
    pub mult_hi: f64,
}

impl Default for FloatGateConfig {
    fn default() -> Self {
        // 造靶校准（8000 手 2-max，villain=p1 在 flop.srp 的 shrunk_fold）：
        //   wideopen（粘站）47% → 乘子≈0（float≈0，复现 +49 最优）；
        //   3bettor（flop 爱弃）64.5% → 乘子≈高（float≈0.85，复现 +33 最优）。
        // 锚 [0.50→0.0, 0.65→3.0]：0.47<0.50 → 0（wideopen off）；0.645→t≈0.97→2.9 →
        //   float=air_bluff_freq_base(0.30)×2.9≈0.87（3bettor 高）。turn/river fold 低（且
        //   3bettor 那里高 raise 惩罚 float）→ 两靶均映射低 float（正确：延迟街 float 风险高）。
        FloatGateConfig {
            node_gated: true,
            min_samples: 30,
            shrink_k: 20.0,
            fold_lo: 0.50,
            mult_lo: 0.0,
            fold_hi: 0.65,
            mult_hi: 3.0,
        }
    }
}

/// `post.unopened.cbet_bluff.srp` 节点级门控参数（P0-3 阶段2 第三消费轮，`docs/heuristic9_design.md` §6）。
///
/// cbet_bluff = hero 作 PFR 进攻方、翻后本街无人下注、持 Air 无听牌时的 c-bet 空气诈唬。
/// **same-role**：杠杆 = villain **fold-to-cbet**（villain 作防守方），门读的正是 fold-to-cbet
/// （§C 桶 = [`crate::heuristic::opponent::facing_node_key`]`(街, "srp")`）→ 与 float 同轴（区别于
/// 跨角色被 REJECT 的 bluff_raise）。造靶实测（8000 手 2-max）srp 最优频率**随 fold-to-cbet 反号**：
/// 粘站 wideopen（fold 47.9%）低频最优、爱弃 3bpfold（fold 79.6%）高频最优 → 固定频率必错一边。
///
/// **仅门控 srp 池**：候选②（`docs/heuristic9_design.md` §6）已证 **3bp c-bet 频率跨对手粘度
/// robust（无反号）** → 3bp 保固定 0.30/0.20 不门控；`other`/limped 亦不动。
///
/// **⚠ 锚独立重标定**（不照抄 float 0.50/0.65/3.0）：cbet 是持续下注、默认频率高（0.80/0.55）→
/// 乘子上限 = 1.0（= 回退），门**只降不升**——粘站降频（+EV）、爱弃/未知保默认（默认恰坐爱弃
/// 最优端，可剥削 EV 全在粘站侧）。见默认值注释。
///
/// **冷启动/未知/小样本安全**：`node_gated=false`、任一对手无名（exploit 关）、binding villain
/// 样本 `n < min_samples`、或非 srp 池 → 乘子 **1.0** → 保留 [`CbetSplit`] 现固定频率（= 现默认
/// 行为）→ 保标准矩阵不退、exploit 关逐字节一致。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CbetBluffGateConfig {
    /// 是否启用节点级门控（默认 true = 校准后新行为）。false → 恒乘子 1.0（= 现固定频率路径，
    /// 用于 paired 对照与关闭剥削）。
    pub node_gated: bool,
    /// 信任 villain fold-to-cbet 读数所需的最小 facing 机会数 `n`（binding villain）。低于此 →
    /// 回退乘子 1.0。cbet_bluff.srp 节点密度高、flop.srp facing 样本充裕（造靶 n≥1200/8000）→ 30 充足。
    pub min_samples: u32,
    /// fold 率收缩伪计数强度 `k`（Beta-Binomial；与阶段1 dump / float 门默认一致）。
    pub shrink_k: f64,
    /// 单调映射低锚：shrunk_fold ≤ `fold_lo` → 乘子 = `mult_lo`（粘站 → 降频）。
    pub fold_lo: f64,
    pub mult_lo: f64,
    /// 单调映射高锚：shrunk_fold ≥ `fold_hi` → 乘子 = `mult_hi`；两锚间线性插值。
    pub fold_hi: f64,
    pub mult_hi: f64,
}

impl Default for CbetBluffGateConfig {
    fn default() -> Self {
        // 造靶校准（8000 手 2-max，villain=p1 在 flop.srp 的 shrunk_fold；只扫 nodes.cbet.srp）：
        //   wideopen（粘站）47.9% → 乘子 0.15 → freq≈0.12/0.08（落粘站最优 m00-m025 带，vs 默认 +40）；
        //   3bpfold（爱弃）79.6% → 乘子 1.0 → freq=0.80/0.55（落爱弃最优 m10，= 现默认）。
        // 锚 [0.50→0.15, 0.75→1.0]：0.479<0.50 → 0.15（wideopen 降频）；0.796>0.75 → 夹取 1.0
        //   （3bpfold 保默认高频）。乘子上限 1.0（=回退）→ 只降不升：可剥削 EV 全在粘站降频侧。
        CbetBluffGateConfig {
            node_gated: true,
            min_samples: 30,
            shrink_k: 20.0,
            fold_lo: 0.50,
            mult_lo: 0.15,
            fold_hi: 0.75,
            mult_hi: 1.0,
        }
    }
}

/// `post.facing.bluff_raise` 节点级门控参数（P0-3 阶段2 第四消费轮，`docs/heuristic9_design.md` §6）。
///
/// bluff_raise = hero 持 Air、无有效听牌、**面对 villain 下注**、`can_raise`、非河时的纯诈唬加注。
/// **same-role（本轮 vs 上轮 REJECT 的唯一实质差别）**：真杠杆 = villain **fold-to-RAISE**（villain 作
/// **进攻方/开火者被加注**后弃牌，§B），门读的正是 fold-to-raise
/// （[`crate::heuristic::opponent::facing_raise_node_key`]`(街, 池型)` = `<街>.<池型>.facing_raise`）→
/// 与杠杆**同轴**。上轮（commit 4893739）门只能读 fold-to-cbet（villain 作**防守方**，
/// [`crate::heuristic::opponent::facing_node_key`]）= **跨角色代理**，两靶 fold-to-cbet 逐字节相同
/// 33.8%、最优频率却相反 → 无法区分 → REJECTED。commit fac17a8 扩采 fold-to-raise 后，门可读真信号。
///
/// **双向门（区别于 cbet「只降」）**：默认回退乘子 [`NodesConfig::bluff_raise_freq_mult`]=0.10（低）。
/// 造靶 sweep（8000 手 2-max）证最优频率随 fold-to-raise **反号**：DonkFold（`flop.srp.facing_raise`
/// shrunk_fold 86.7%）最优 m≈2.0（高频剥削爱弃对手）、DonkStick（3.3%）最优 m≈0.0（零频止损粘站对手）
/// → 门须能从 0.10 既**上调**（爱弃）又**下调**（粘站）。故乘子范围 `[mult_lo 0.0, mult_hi 2.0]`
/// （**不套 cbet 的上限 1.0**）。
///
/// **冷启动/未知/小样本安全**：`node_gated=false`、任一对手无名（exploit 关：sim `names` 空 / live
/// 缺名）、或 binding villain 样本 `n < min_samples` → 回退固定 [`NodesConfig::bluff_raise_freq_mult`]
/// (0.10) → 保标准矩阵不退、exploit 关逐字节一致（同 float 门结构：回退固定乘子，非 cbet 的 1.0）。
///
/// **binding villain**：hero 加注需下注方（及其他活对手）全弃才成 → 取**最粘者**（min shrunk_fold）
/// 作 binding（同 float/cbet；bluff_raise 尤其需全弃 → min 正确）。**语义注**：`facing_raise` = 该
/// villain 作**开火者**被加注后的弃牌，最直接相关的是**本手下注方**（开火者）的 stat；遍历活对手取
/// min 是多人池保守做法（HU 只有下注方一人 → min = 下注方）。
///
/// **门控全池型**（区别于 cbet 仅 srp）：fold-to-raise 对所有池型都采、且 bluff_raise 无「3bp robust」
/// 发现（那是 cbet 的事）→ 全池型同锚，按各自 `<街>.<池型>.facing_raise` 读。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BluffRaiseGateConfig {
    /// 是否启用节点级门控（默认 true = 校准后新行为）。false → 恒用固定
    /// [`NodesConfig::bluff_raise_freq_mult`]（= Phase B/C 旧路径，用于 paired 对照与关闭剥削）。
    pub node_gated: bool,
    /// 信任 villain fold-to-raise 读数所需的最小 facing 机会数 `n`（binding villain）。低于此 →
    /// 回退固定乘子。fold-to-raise 节点稀疏（villain 须**开火 + 被加注 + 响应**）→ 标准场多数 villain
    /// n<30 落回退（= 现固定 0.10）→ 门仅在「爱 donk/lead 进 hero」的窄场激活（预期窄剥削）。
    pub min_samples: u32,
    /// fold 率收缩伪计数强度 `k`（Beta-Binomial；与阶段1 dump / float / cbet 门默认一致）。
    pub shrink_k: f64,
    /// 单调映射低锚：shrunk_fold ≤ `fold_lo` → 乘子 = `mult_lo`（粘站 → 降频止损）。
    pub fold_lo: f64,
    pub mult_lo: f64,
    /// 单调映射高锚：shrunk_fold ≥ `fold_hi` → 乘子 = `mult_hi`（爱弃 → 升频剥削）；两锚间线性插值。
    pub fold_hi: f64,
    pub mult_hi: f64,
}

impl Default for BluffRaiseGateConfig {
    fn default() -> Self {
        // 造靶校准（8000 手 2-max，villain=p1 在 `<街>.<池型>.facing_raise` 的 shrunk_fold；sweep
        // `bluff_raise_freq_mult` ∈{0,2}×seed{7,11} 复现于当前树）：
        //   DonkStick（低 fold-to-raise）flop.srp 3.3% / turn.srp 3.4% → 最优 m≈0.0（零频，止损）；
        //   DonkFold（高 fold-to-raise）flop.srp 86.7% / turn.srp 74.5% → 最优 m≈2.0（高频，剥削）。
        // 锚 [0.30→0.0, 0.80→2.0]（**双向**，不套 cbet 上限 1.0）：
        //   DonkStick 3.3%<0.30 → 0.0（下调，vs OFF 0.10 略优）；DonkFold flop 86.7%>0.80 → 2.0、
        //   turn 74.5%→t≈0.89→1.78（上调，剥削）。低锚 0.30 = fold-to-raise 占位先验（`node_field_prior`）
        //   → 「典型/未分化」villain 落地板 mult 0.0（保守，仅对**明确爱弃**对手 fold>0.30 才升频，照
        //   float 门「仅对明确爱弃对手 float」哲学）；n<min_samples 仍走回退固定 0.10 保节点存活+样本通道。
        BluffRaiseGateConfig {
            node_gated: true,
            min_samples: 30,
            shrink_k: 20.0,
            fold_lo: 0.30,
            mult_lo: 0.0,
            fold_hi: 0.80,
            mult_hi: 2.0,
        }
    }
}

/// `post.unopened.value_bet.srp` **价值下注尺寸**节点级门控（P0-3 阶段2 第七消费轮，
/// `docs/heuristic9_design.md` §6）。
///
/// value_bet = hero 作 PFR 进攻方、翻后本街无人下注、持 MidPair+ 真牌的价值下注。杠杆同轴
/// （hero 进攻方价值下注、villain 防守方面对 → 读 [`crate::heuristic::opponent::facing_node_key`]`(街,"srp")`
/// 的 **fold-to-cbet**，与 cbet 频率门 / float 门同轴）。
///
/// **单向放大门（本轮 vs 上轮 CONFIRMED-ROBUST 的关键差别）**：上轮（commit 5634b62）造靶 sweep 证
/// 价值注尺寸最优值**不随** villain 粘度**反号**——粘站 WideOpener（fold 47.5%）清晰要**大**（1.60
/// 最优、+310 vs 1.0、单调↑）、爱弃 ThreebetFolder（fold 79.6%）**wash 偏大不要小**（933–957 跨度
/// 23 ≪ CI95 ±270）。双向门（含「爱弃→缩小」）实测证伪。但同轮挖出真机会：**两靶均不分类 Station**
/// （画像看翻前 VPIP/AF，漏「翻后粘 + 翻前激进」对手）→ 现 `value_size_mult` 恒 1.0、漏掉 WideOpener
/// +310 的放大增益。本门**单向捕获**这半：只做「放大」半（sweep 证有收益），不做「缩小」半（证伪）。
///
/// **本轮判定 = ship 平放大 A（flat 1.5），非节点门 B**：A/B 正面对比（`docs/heuristic9_design.md` §6）：
/// 平放大（value_bet.srp 恒 ×1.5）vs 节点门（低 fold→1.5 / 高 fold→1.0）在两靶 + 标准 11-config + **nit
/// 靶**（B「保护高 fold 尾」应最占优处）实测 **A ≥ B 逐点估**（nit 2max A +4.9 / B +0.4、6max A +22.8 /
/// B +13.5 mbb/g）。机理：价值注尺寸对**所有**对手「越大越不亏」（含 nit：nit 弃 trash 于任何尺寸、跟
/// 时持强牌付更大注），故 B「回落高 fold 保爱弃尾」是**无的放矢**（那半没损失可保），反让出放大 EV。
/// → 平放大更简单且不劣（rail #3）→ **默认 flat**（`fold_lo/fold_hi` 两锚同乘子 1.5 = 无 fold 微分）。
/// 递减单向映射能力保留（锚可配），供 live 数据后续证需时（config-only）再校准，本轮证暂无此需。
///
/// **subsume 画像乘子（避双重放大）**：激活时用 `max(mult, adj.value_size_mult)`——既保证不低于画像
/// station 1.30x（不回退已分类站），又用 `max` 而非乘积**吸收**画像乘子（不与其双重放大）。且 `map`
/// 结果下夹 1.0（`mult.max(1.0)`）→ **结构上单向**，任何锚配置都不缩小。
///
/// **仅门控 srp 池**（区别于 value_raise/commit 叶不碰；3bp/other value_bet 保画像乘子）：sweep 只在
/// value_bet.srp 叶做、密度 1648/1080 每 8000 手足。非 srp 池 → 回退 `adj.value_size_mult`（byte-equal）。
///
/// **冷启动/未知安全**：`node_gated=false`、任一对手无名（exploit 关：sim `names` 空 / live 缺名）、
/// binding villain 样本 `n < min_samples`、或非 srp 池 → 回退 `adj.value_size_mult` → 保标准矩阵不退、
/// exploit 关逐字节一致。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CbetValueSizeGateConfig {
    /// 是否启用节点级门控（默认 true = 校准后新行为）。false → 恒回退 `adj.value_size_mult`（=
    /// 现固定尺寸路径，用于 paired 对照与关闭剥削）。
    pub node_gated: bool,
    /// 信任 binding villain（有名）所需的最小 facing 机会数 `n`。低于此 → 回退 `adj.value_size_mult`。
    /// **默认 0 = 对任一有名对手即放大**（平放大 A 与 villain 读数无关：价值注越大越不亏，见结构注释；
    /// sweep/矩阵以 `min_samples=0` 验）。>0 时可要求「至少见过 N 次」再放大（保守；`fold_lo/fold_hi`
    /// 微分时才需读足样本）。
    pub min_samples: u32,
    /// fold 率收缩伪计数强度 `k`（Beta-Binomial；与阶段1 dump / float / cbet 门默认一致）。仅
    /// `fold_lo != fold_hi`（node-differentiation）时影响乘子；默认 flat（两锚同乘子）下 fold 读被忽略。
    pub shrink_k: f64,
    /// 单调（递减）映射低 fold 锚：shrunk_fold ≤ `fold_lo` → 乘子 = `mult_lo`（粘站侧）。
    /// **默认 `mult_lo == mult_hi == 1.5` → flat（无 fold 微分，= 本轮 ship 的平放大 A）**。
    pub fold_lo: f64,
    pub mult_lo: f64,
    /// 单调映射高 fold 锚：shrunk_fold ≥ `fold_hi` → 乘子 = `mult_hi`（爱弃侧）；两锚间线性插值。
    /// 映射结果结构上下夹 1.0（单向不缩小）。默认与 `mult_lo` 同 → flat。设 `mult_hi < mult_lo` 可开
    /// 「低 fold 放大 / 高 fold 回落」节点门 B（本轮证不占优，留后续 config-only 校准）。
    pub fold_hi: f64,
    pub mult_hi: f64,
}

impl Default for CbetValueSizeGateConfig {
    fn default() -> Self {
        // 本轮 ship = **平放大 A**（value_bet.srp 恒 ×1.5，对任一有名对手）。A/B 对比（§6）证 A ≥ B
        // 逐点估（含 nit 靶），平放大更简单且不劣（rail #3）→ 默认 flat：两 fold 锚同乘子 1.5 → 线性插值
        // 恒 1.5（无 fold 微分）；`min_samples=0` → 对任一有名对手即放大（价值注越大越不亏，与读数无关）。
        // 激活时 `max(1.5, adj.value_size_mult)` → subsume 画像 station 1.30x（不双重放大、不回退分类站）。
        // 递减节点门能力保留（设 `mult_hi<mult_lo` + `min_samples=30`）供后续 live 校准，本轮证暂无此需。
        CbetValueSizeGateConfig {
            node_gated: true,
            min_samples: 0,
            shrink_k: 20.0,
            fold_lo: 0.50,
            mult_lo: 1.5,
            fold_hi: 0.80,
            mult_hi: 1.5,
        }
    }
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
    /// `post.unopened.cbet_bluff.srp` 节点级门控（P0-3 阶段2 第三消费轮）：按 binding villain
    /// 翻后 fold-to-cbet 单调映射 srp c-bet 诈唬频率乘子（粘站降频 / 爱弃保默认）。仅门控 srp
    /// （3bp robust 不动，候选②）；冷启动/未知/小样本/`node_gated=false` 回退乘子 1.0（= [`CbetSplit`]
    /// 现固定频率）。
    pub cbet_gate: CbetBluffGateConfig,
    /// `post.unopened.float` **回退**频率乘子（冷启动/未知/小样本/`float.node_gated=false`
    /// 时用）：float 频率 = 空气诈唬基础频率 × 此乘子。0.5 = Phase B/C 两次驳回一刀切调整后
    /// 维持的默认（0.25 试点 sim 正负参半）。P0-3 阶段2 起，样本足的已知 villain 走
    /// [`FloatGateConfig`] 节点级门控取代此固定乘子。
    pub float_freq_mult: f64,
    /// `post.unopened.float` 节点级门控（P0-3 阶段2）：按 villain 翻后 fold-to-stab 单调映射
    /// float 频率乘子，取代固定 `float_freq_mult`（冷启动/未知/小样本回退固定乘子）。
    pub float: FloatGateConfig,
    /// `post.facing.bluff_raise` **回退**频率乘子（冷启动/未知/小样本/`bluff_raise_gate.node_gated=false`
    /// 时用）：bluff_raise 频率 = 空气诈唬基础频率 × 此乘子。0.10 = Phase B 扫参甜点（对 sticky/常见场
    /// 近最优，sweep 低频侧）。P0-3 阶段2 第四消费轮起，样本足的已知 villain 走 [`BluffRaiseGateConfig`]
    /// 节点级门控（读 fold-to-raise 真信号）取代此固定乘子；未分化对手仍走此回退保节点存活 + 样本通道。
    pub bluff_raise_freq_mult: f64,
    /// `post.facing.bluff_raise` 节点级门控（P0-3 阶段2 第四消费轮）：按 binding villain 翻后
    /// **fold-to-raise**（真杠杆，same-role）单调映射 bluff_raise 频率乘子——**双向**（爱弃升频剥削 /
    /// 粘站降频止损，区别于 cbet 只降）。冷启动/未知/小样本/`node_gated=false` 回退固定
    /// `bluff_raise_freq_mult`。上轮跨角色代理（读 fold-to-cbet）被 REJECT，本轮读 fold-to-raise 同轴。
    pub bluff_raise_gate: BluffRaiseGateConfig,
    /// `post.unopened.value_bet.srp` **价值下注尺寸**放大门（P0-3 阶段2 第七消费轮）：对任一有名对手把
    /// srp 价值注尺寸乘子放大 **~1.5**（捕获画像漏分类的「翻后粘 + 翻前激进」对手 +310 增益；A/B 对比证
    /// 平放大 ≥ 节点门、含 nit，见 §6）。激活时 `max(1.5, adj.value_size_mult)` subsume 画像 station
    /// 1.30x（**不双重放大**）。仅门控 srp；冷启动/无名/非 srp/`node_gated=false` 回退 `adj.value_size_mult`
    /// （byte-equal）。递减节点门（低 fold 放大 / 高 fold 回落）能力保留（锚可配），本轮证不占优。
    pub cbet_value_size_gate: CbetValueSizeGateConfig,
    /// `post.unopened.cbet_bluff.srp` 在 **turn / river** 街的空气 barrel **基础频率修正**乘子
    /// （P0-3 阶段2 第八消费轮 = A1，SHIPPED）。**门前现状**：turn/river barrel 频率随 srp flop 锚
    /// 同映射——base = `nc.cbet.srp`(0.80/0.55) 对三街通用，`cbet_gate` 读数虽已按街但锚 flop 标定、
    /// 且 mult 上限 1.0（只降不升）+ turn/river 每对手样本稀（n<30 频回退 1.0）→ live 对未知/稀疏对手
    /// barrel 空气 @0.80（run34 `cbet_bluff.srp` −4.05 主因）。**A1 结论（非门，base 修正）**：空气 barrel
    /// 在后街 fold-equity 结构性衰减（river 空气 eq≈0.08）→ 降 turn/river base 频率对**全对手**（含未知/
    /// 稀疏，门够不到的人群）broadly +EV。纪律 A sweep（gate-OFF）：粘/跟场单调↑（limpy +115~+149、
    /// wideopen +65~+96、station +38、mixed +21、nit +26），唯纯弃场(3bpfold) −4~−10 小 wash；exploit-ON
    /// 标准矩阵 0/N（门已处理有名密样对手）。**这是 base 策略修正**（作用于 exploit on+off；exploit-OFF
    /// byte-equal 有意打破 = base 应变）；乘子设 1.0 可逐字节回退 HEAD（rollback/隔离证明）。等效度按街分：
    /// river 空气更纯 → 砍更狠（等价梯度）。作用点在 [`policy::barrel_street_freq_mult`]、
    /// `roll = rng.unit()` **之前**（掷点位置不动 → RNG 消费序列不变）；仅 srp 池（3bp robust / other 稀疏）。
    pub barrel_turn_freq_mult: f64,
    pub barrel_river_freq_mult: f64,
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
            cbet_gate: CbetBluffGateConfig::default(),
            float_freq_mult: 0.5,
            float: FloatGateConfig::default(),
            bluff_raise_freq_mult: 0.10,
            bluff_raise_gate: BluffRaiseGateConfig::default(),
            cbet_value_size_gate: CbetValueSizeGateConfig::default(),
            // A1 SHIPPED：turn/river 空气 barrel base 频率修正（等价梯度：river 空气更纯砍更狠）。
            barrel_turn_freq_mult: 0.6,
            barrel_river_freq_mult: 0.3,
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
