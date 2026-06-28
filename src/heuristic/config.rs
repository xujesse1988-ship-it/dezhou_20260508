//! 全部可调参数（迭代闭环的旋钮面）。
//!
//! 设计原则：**每一个会影响 EV 的魔法数都进 [`HeuristicConfig`]**，给默认值，可被 JSON
//! 覆盖（`HeuristicConfig::from_json_str`）。改启发式 = 改这里 + 跑 `heuristic9_sim` 回归门。
//!
//! 单位约定：筹码量一律 **big blind 倍数（f64）**，与桌大小/盲注无关 → 同一份 config 通吃
//! 任意 stake。Equity / 概率 / 频率都是 `[0,1]` 的 f64。

use serde::{Deserialize, Serialize};

/// 顶层启发式配置。所有子配置都实现 [`Default`]，默认值 = v0.1 经验调参起点。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HeuristicConfig {
    pub equity: EquityConfig,
    pub preflop: PreflopConfig,
    pub postflop: PostflopConfig,
    pub mixing: MixingConfig,
    pub exploit: ExploitConfig,
}

impl HeuristicConfig {
    /// 从 JSON 字符串覆盖默认值（缺字段走默认，`serde(default)`）。解析失败返回 `Err`。
    pub fn from_json_str(s: &str) -> Result<Self, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
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

/// 翻前阈值都以「手牌强度」表达——强度 = equity-vs-1-random ∈ [0,1]（见 `equity::preflop`）。
/// 位置用「相对按钮的未行动人数桶」泛化，座位无关：
/// `early`（离按钮远，身后人多）/ `mid` / `late`（CO/BTN）/ `blinds`（SB/BB）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PreflopConfig {
    /// 各位置桶的开池（first-in raise）最低强度阈值。
    pub open_early: f64,
    pub open_mid: f64,
    pub open_late: f64,
    pub open_sb: f64,
    /// 面对一个加注的 3bet 最低强度（value 3bet）。
    pub threebet: f64,
    /// 面对一个加注的冷跟（flat call）最低强度。
    pub flat_call: f64,
    /// 面对 3bet/4bet 的继续（call/4bet）最低强度。
    pub vs_threebet_continue: f64,
    pub fourbet: f64,
    /// 每多一个**已入池**对手，价值门槛上抬多少（多人收紧）。
    pub multiway_tighten: f64,
    /// 开池尺寸（bb）。9-max 标准 2.2–2.5bb。
    pub open_size_bb: f64,
    /// 每个 limper 额外加的开池尺寸（bb）。
    pub iso_extra_per_limper_bb: f64,
    /// 3bet 尺寸 = 面对的 raise-to 的倍数（IP）/（OOP）。
    pub threebet_mult_ip: f64,
    pub threebet_mult_oop: f64,
    /// 短栈（有效栈 < 此 bb）切「推/弃」模式。
    pub shortstack_bb: f64,
    /// 短栈推全下的最低强度。
    pub shove_strength: f64,
}

impl Default for PreflopConfig {
    fn default() -> Self {
        PreflopConfig {
            // equity-vs-random 标度：随机两张约 0.5；这些阈值经 9-max 常识设定。
            open_early: 0.585,
            open_mid: 0.555,
            open_late: 0.505,
            open_sb: 0.535,
            threebet: 0.64,
            flat_call: 0.57,
            vs_threebet_continue: 0.60,
            fourbet: 0.70,
            multiway_tighten: 0.012,
            open_size_bb: 2.3,
            iso_extra_per_limper_bb: 1.0,
            threebet_mult_ip: 3.0,
            threebet_mult_oop: 4.0,
            shortstack_bb: 25.0,
            shove_strength: 0.60,
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
    /// c-bet 作为翻前加注者的基础频率（heads-up、干面）。
    pub cbet_freq_dry: f64,
    pub cbet_freq_wet: f64,
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
            cbet_freq_dry: 0.80,
            cbet_freq_wet: 0.55,
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
