//! 对手画像 + 剥削调整 + 持久化。
//!
//! 「累积经验」在这里变现：跨手、跨 session 记每个对手名的 VPIP/PFR/AF，分类成
//! nit/tag/station/maniac，再把它翻译成「对手 range 松紧偏移」+「我方价值/诈唬调整」。
//! 弱场里这是最大 EV 来源——纯 GTO 留在桌上的钱，剥削能拿走。

use crate::heuristic::config::ExploitConfig;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// P0-3 阶段1：per-node 面对下注的响应直方图（原始计数 + 分母 `n`；§0.5「采原始计数不采比率」）。
///
/// 语义 = 某街某池型下，该对手**作为防守方首次面对一个未平下注**时的动作分布：
/// - flop 节点 → fold-to-cbet 的原料（§C）；turn → fold-to-turn-barrel；river → fold-to-river-barrel。
/// - `raises` 桶 = raise-cbet / check-raise / river-raise 频率（§B「villain 作进攻方」极性代理，候选①）。
/// - `pre.facing_3bet` 节点 → fold-to-3bet（§A，群体级）。
///
/// 不变式：`n == folds + calls + raises`（check/其他不计入分母）。**决策路径不读**（阶段1
/// byte-equal）；比率查询时现算并按 `n` 向 field 先验收缩（§H）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FacingCounts {
    /// 机会数（分母）= folds + calls + raises。
    pub n: u32,
    pub folds: u32,
    pub calls: u32,
    pub raises: u32,
}

impl FacingCounts {
    /// 原始 fold-to-bet 率 = folds/n。`n==0` → None（无样本）。
    pub fn raw_fold_rate(&self) -> Option<f64> {
        (self.n >= 1).then(|| self.folds as f64 / self.n as f64)
    }
    /// 原始 raise 率 = raises/n（候选① 极性代理）。`n==0` → None。
    pub fn raw_raise_rate(&self) -> Option<f64> {
        (self.n >= 1).then(|| self.raises as f64 / self.n as f64)
    }
    /// Beta-Binomial 收缩后 fold 率：`(folds + prior·k)/(n + k)`，`k` = 先验伪计数强度
    /// （先验等效样本数；`n==k` 时个体/先验各半）。`n==0` → 返回 prior。
    pub fn shrunk_fold_rate(&self, prior: f64, k: f64) -> f64 {
        (self.folds as f64 + prior * k) / (self.n as f64 + k)
    }
    /// 收缩后 raise 率（同上口径）。
    pub fn shrunk_raise_rate(&self, prior: f64, k: f64) -> f64 {
        (self.raises as f64 + prior * k) / (self.n as f64 + k)
    }
}

/// P0-3 §D：per-node villain **作进攻方且摊牌**时的成手牌力构成（原始计数，§0.5 铁律：存计数
/// 不存比率，非坚果率查询时现算）。
///
/// 分母 `showdowns` = 该节点作进攻方（该街 bet/raise）且到摊牌亮牌的手数（**截断偏差**：只含
/// 「攻击被跟到摊牌」的手，被弃的攻击无亮牌 → range 是**摊牌子集**、非全攻击 range，见
/// `opponent_features_inventory.md` §D 截断偏差）。候选① river 极化门的真信号源——§B 频率代理
/// （raise/fold 率）撞车（`river.srp.facing_raise` FlushTrap 44.9% ≈ Tag 45.2%），唯 §D 摊牌 range
/// 极性能分开纯坚果攻击（FlushTrap）与混合价值攻击（Tag）。**决策路径不读**（纯增采集）。
///
/// **两档非坚果度**（务必区分）：`nonnut`（`monster_non_nut`，含**同花或顺子**）会被 FlushTrap 在
/// **成顺非成花板**的非坚果**顺子** value_raise 污染（FlushTrap 仅**成花板**纯坚果建战、成顺板打法
/// 同 Tag）→ FlushTrap river.srp `nonnut` 实测 11.3% 非 ≈0；`nonnut_flush`（**flush-only**）才是候选①
/// 真杠杆（REJECT 处方 #3「必须 flush-only，顺子降级 -EV」，`heuristic9_design.md` §6）——FlushTrap
/// 从不攻击非坚果同花 → flush-only ≈0 ≪ Tag。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ShowdownCounts {
    /// 摊牌样本数（分母）= 该节点作进攻方且亮牌的手数。
    pub showdowns: u32,
    /// 其中 `monster_non_nut` 命中数（非坚果 Monster = 可被压制的**同花或顺子**攻击）。
    pub nonnut: u32,
    /// 其中 `nonnut_flush` 命中数（**flush-only** 非坚果同花攻击）——候选① 的**真杠杆**（`nonnut`
    /// 含顺子被 FlushTrap 成顺板非坚果顺子污染，唯 flush-only 给出 FlushTrap≈0% ≪ Tag 的决定性分化）。
    /// `#[serde(default)]` 兼容早期只写 `nonnut` 的 profile。
    #[serde(default)]
    pub nonnut_flush: u32,
    /// 成手类别直方（索引 = `MadeClass::rank()`，0=Air..5=Monster）；`#[serde(default)]` 防未来
    /// 部分 JSON 缺该字段。
    #[serde(default)]
    pub by_made_class: [u32; 6],
}

impl ShowdownCounts {
    /// 原始非坚果 Monster 攻击率 = nonnut/showdowns（含同花+顺子）。`showdowns==0` → None。
    pub fn raw_nonnut_rate(&self) -> Option<f64> {
        (self.showdowns >= 1).then(|| self.nonnut as f64 / self.showdowns as f64)
    }
    /// 原始 **flush-only** 非坚果攻击率 = nonnut_flush/showdowns（候选① 真杠杆信号）。`showdowns==0` → None。
    pub fn raw_nonnut_flush_rate(&self) -> Option<f64> {
        (self.showdowns >= 1).then(|| self.nonnut_flush as f64 / self.showdowns as f64)
    }
}

/// 归一化「加注类」动作判定（bet/raise/all-in 变体）。
fn is_raise_action(a: &str) -> bool {
    matches!(
        a.to_ascii_lowercase().as_str(),
        "bet" | "raise" | "allin" | "all_in" | "all-in"
    )
}

/// 池型名（与 hero 侧 `ScenarioKey.pot_type` 逐字同口径，`preflop_num_raises` 派生）。
/// `pub`：P0-3 阶段2 hero 侧节点查询用同一口径构造对偶 facing 节点 key（单一坐标系，§5.2）。
pub fn pot_type_name(pre_raises: u32) -> &'static str {
    match pre_raises {
        0 => "limped",
        1 => "srp",
        2 => "3bp",
        _ => "4bp+",
    }
}

/// 翻后街名（1=flop..3=river）。
fn street_name(street: u8) -> &'static str {
    match street {
        1 => "flop",
        2 => "turn",
        _ => "river",
    }
}

/// P0-3 阶段1：翻后 facing 节点 key 构造器 —— **§G 维度扩展的唯一接口**。
///
/// 当前粒度止步 `<街>.<池型>`（如 `"flop.srp"`）——实测 fold-to-cbet 中位仅 18/人，
/// 再叠维即噪声（`opponent_features_inventory.md` §G/§H）。**留的口子**：key 是点分
/// 限定符串，后续加 §G 维度（牌面质地 / 位置 IP-OOP / SPR 桶）= 在此函数**追加限定符**
/// （如 `format!("{}.{}.{}", street, pot, texture_bucket)` → `"flop.srp.wet"`），无需动
/// 数据结构（`BTreeMap<String,_>` 任意 key）。三处协同已为扩展就位：
/// - `node_field_prior` 按**前缀**匹配（`starts_with("flop.")`）→ 更细 key 自动继承粗先验，
///   加维不破先验查询；
/// - 消费侧（阶段2）按样本自适应**并桶**：细 key 分母不足时回退粗 key（§G「采最细、消费并桶」）；
/// - 采 §G 维度需 enrich `ObservedAction`（携 board/位置）——那是加维时的真正工作量，非本函数。
///
/// `pub`：P0-3 阶段2 hero 侧 float 门用它构造「villain 面对 hero stab 的对偶节点」key，
/// 与采集侧共用同一构造器 → 查询/采集口径逐字节一致（§5.2 节点级剥削）。
pub fn facing_node_key(street: u8, pot_type: &str) -> String {
    format!("{}.{}", street_name(street), pot_type)
}

/// P0-3：翻后 **fold-to-raise** 节点 key —— [`facing_node_key`] 的 `.facing_raise` 兄弟。
///
/// 语义分野（务必与 fold-to-cbet 区分）：`<街>.<池型>`（如 `"flop.srp"`）= villain **作防守方**
/// 首次面对未平注的响应（fold-to-cbet，§C）；本 key `<街>.<池型>.facing_raise`（如
/// `"flop.srp.facing_raise"`）= villain **作进攻方**（街内首注方/领打方）被防守方 re-raise 后的
/// 首次响应（fold-to-raise，§B「villain 进攻方极性」）——**独立两轴**（造靶 `DonkFold`/`DonkStick`
/// 证明可任意解耦，见 `heuristic9_design.md` §6「bluff_raise REJECT」）。它是翻前 `pre.facing_3bet`
/// 在翻后每街的镜像。
///
/// 走 [`facing_node_key`] 的 §G 扩展口子：复用其点分限定符串基座、再缀 `.facing_raise` 语义
/// 后缀，与采集侧（[`OpponentTable::observe_facing`]）共用**同一构造器** → 未来 hero 侧 bluff_raise
/// 门查询/采集口径逐字节一致（照 float 门用 `facing_node_key` 的先例）。
///
/// **前缀碰撞警示**：本 key 以 `flop.`/`turn.`/`river.` 起始，会被 [`node_field_prior`] 的
/// `starts_with("flop.")` 抢先匹配到 fold-to-cbet 先验（错）——故该函数为 `.facing_raise` 加了
/// 专用前置分支（长/专用先判，见其 doc）。
pub fn facing_raise_node_key(street: u8, pot_type: &str) -> String {
    format!("{}.facing_raise", facing_node_key(street, pot_type))
}

/// P0-3 §D：翻后 **摊牌攻击** 节点 key —— [`facing_node_key`] 的 `.attack_showdown` 兄弟。
///
/// 语义：villain **作进攻方**（该街 bet/raise）且**到摊牌亮牌**时，其牌在 **final board** 上的成手
/// 牌力构成（`MadeClass` 直方 + `monster_non_nut` 命中）。候选① river 极化门的真信号源——§B 频率代理
/// 撞车（`river.srp.facing_raise` FlushTrap 44.9% ≈ Tag 45.2%），唯 §D 摊牌 range 极性能分开纯坚果
/// 攻击（FlushTrap）与混合价值攻击（Tag），见 `heuristic9_design.md` §6 候选① 第三次 REJECT。
///
/// **与 fold-to-raise（`.facing_raise`）的结构分野（关键）**：§D 计数值 = [`ShowdownCounts`]
/// （非 [`FacingCounts`]）→ 存入 **独立** `OppStats.attack_showdown` map（非 `facing`），有独立 readout
/// （[`OpponentTable::showdown_readout`]）→ **不经 [`node_field_prior`]**（§D 无摊牌率 field 先验，全是
/// 原始计数 + 现算率）→ **天然无前缀碰撞**（比 fold-to-raise 更干净：后者共用 `facing` map 经
/// `node_field_prior`，须加 `ends_with(".facing_raise")` 专用前置分支；本 key 无此需）。
///
/// 走 [`facing_node_key`] 的 §G 扩展口子：复用点分基座 + 缀 `.attack_showdown` 语义后缀，与采集侧
/// （[`OpponentTable::observe_showdown`]）共用**同一构造器** → 未来 gate（候选① river 极化门重做）
/// 查询/采集口径逐字节一致（照 float 门用 `facing_node_key` 的先例）。
pub fn attack_showdown_node_key(street: u8, pot_type: &str) -> String {
    format!("{}.attack_showdown", facing_node_key(street, pot_type))
}

/// P0-3 阶段1：per-node 面对下注 fold/raise 率的 **field 先验**（冷启动默认读数）。
/// 数值取自 `docs/opponent_features_inventory.md`「实测样本称重」7-run 快照：
/// fold-to-cbet 45.6% / fold-to-3bet 15.1%。turn/river fold 无独立实测 → 占位（消费前须扩测校准）；
/// raise 先验（候选① 极性代理）无实测 → 保守 0.05 占位（阶段2 造靶校准）。
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct NodePrior {
    pub fold: f64,
    pub raise: f64,
}

/// 查询某 `node_id` 的 field 先验。未知节点 → 中性占位。
///
/// **前缀匹配 = §G 扩展的前向兼容机制**：按 `starts_with("flop.")` 等匹配，故更细的
/// key（如 `"flop.srp.wet"`，见 [`facing_node_key`]）自动继承 `flop.` 的粗先验——加 §G
/// 维度不破此查询。加维时如有更细的实测先验，在此按更长前缀**前置**一条分支即可（长前缀先判）。
pub fn node_field_prior(node_id: &str) -> NodePrior {
    if node_id == "pre.facing_3bet" {
        NodePrior {
            fold: 0.151,
            raise: 0.05,
        }
    } else if node_id.ends_with(".facing_raise") {
        // P0-3 fold-to-raise（villain 作**进攻方**被 re-raise 后弃牌，§B）。**专用分支须前置**：
        // key `flop.srp.facing_raise` 以 `flop.` 起始，若落到下方 `starts_with("flop.")` 会被抢先
        // 匹配到 fold-to-cbet 先验 0.456（那是**防守方** fold，非 fold-to-raise，错）。照本函数
        // doc「长/专用前缀先判」机制前置。**无实测**（`opponent_feature_weigh.py` 无 fold-to-raise
        // replay）→ 保守占位 0.30（进攻方被加注弃牌率通常低于面对首注，因下注方有范围）；
        // **占位待实测校准**（扩 weigh.py fold-to-raise replay 后订正，别照抄 0.456）。
        NodePrior {
            fold: 0.30,
            raise: 0.05,
        }
    } else if node_id.starts_with("flop.") {
        NodePrior {
            fold: 0.456,
            raise: 0.05,
        }
    } else if node_id.starts_with("turn.") {
        // 占位（无独立实测；暂用 flop 值）。
        NodePrior {
            fold: 0.456,
            raise: 0.05,
        }
    } else if node_id.starts_with("river.") {
        // 占位（无独立实测；河牌弃牌略紧）。
        NodePrior {
            fold: 0.50,
            raise: 0.05,
        }
    } else {
        NodePrior {
            fold: 0.45,
            raise: 0.05,
        }
    }
}

/// P0-3 阶段1：per-node 面对下注读数（原始计数 + 收缩率）。**阶段1 决策路径不消费**——
/// 仅供 dump/inspect 与阶段2 剥削门控。
#[derive(Clone, Debug, PartialEq)]
pub struct NodeReadout {
    pub node_id: String,
    /// 机会数（分母）。
    pub n: u32,
    /// 原始 fold 率（n==0 → None）。
    pub raw_fold: Option<f64>,
    /// 按 n 向 field 先验收缩后的 fold 率。
    pub shrunk_fold: f64,
    /// 原始 raise 率（候选① 极性代理；n==0 → None）。
    pub raw_raise: Option<f64>,
    /// 收缩后 raise 率。
    pub shrunk_raise: f64,
}

/// P0-3 §D：per-node 摊牌攻击读数（原始计数 + 现算非坚果率）。**决策路径不消费**——仅供 dump/inspect
/// 与后续 gate（候选① river 极化门重做）门控。无收缩（§D 无摊牌率 field 先验；接门时按样本再定）。
#[derive(Clone, Debug, PartialEq)]
pub struct ShowdownReadout {
    pub node_id: String,
    /// 摊牌样本数（分母）。
    pub showdowns: u32,
    /// 非坚果 Monster 攻击命中数（同花+顺子）。
    pub nonnut: u32,
    /// flush-only 非坚果攻击命中数（候选① 真杠杆）。
    pub nonnut_flush: u32,
    /// 原始非坚果率 = nonnut/showdowns（showdowns==0 → None）。
    pub raw_nonnut: Option<f64>,
    /// 原始 flush-only 非坚果率 = nonnut_flush/showdowns（候选① 真信号；showdowns==0 → None）。
    pub raw_nonnut_flush: Option<f64>,
    /// 成手类别直方（0=Air..5=Monster）。
    pub by_made_class: [u32; 6],
}

/// 单个对手的累计统计。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OppStats {
    pub hands: u32,
    /// 有翻前自愿入池机会的手数（非盲位强制）。
    pub vpip_opp: u32,
    pub vpip_cnt: u32,
    pub pfr_opp: u32,
    pub pfr_cnt: u32,
    /// 翻后激进度：下注+加注 vs 跟注。
    pub aggr_actions: u32,
    pub call_actions: u32,
    /// P0-3 阶段1：per-node（`街.池型`）面对下注响应计数。key 例：`"flop.srp"` / `"turn.3bp"`
    /// / `"pre.facing_3bet"`。`BTreeMap` → 确定性序列化（byte-stable profile JSON）。
    /// 旧 profile 无此字段 → `#[serde(default)]` 补空表（后向兼容硬约束）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub facing: BTreeMap<String, FacingCounts>,
    /// P0-3 §D：per-node（`街.池型.attack_showdown`）villain **作进攻方且摊牌**时的成手牌力构成。
    /// **独立于** `facing`（值类型 [`ShowdownCounts`] ≠ [`FacingCounts`]，且不经 `node_field_prior`
    /// → 天然无前缀碰撞）。key 例：`"river.srp.attack_showdown"` / `"flop.3bp.attack_showdown"`。
    /// 旧 profile 无此字段 → `#[serde(default)]` 补空表（后向兼容硬约束）。**决策路径不读**
    /// （纯增采集，sim byte-equal）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attack_showdown: BTreeMap<String, ShowdownCounts>,
}

impl OppStats {
    pub fn vpip(&self) -> Option<f64> {
        (self.vpip_opp >= 1).then(|| self.vpip_cnt as f64 / self.vpip_opp as f64)
    }
    pub fn pfr(&self) -> Option<f64> {
        (self.pfr_opp >= 1).then(|| self.pfr_cnt as f64 / self.pfr_opp as f64)
    }
    /// aggression factor =（下注+加注）/ 跟注。
    pub fn af(&self) -> Option<f64> {
        (self.call_actions >= 1).then(|| self.aggr_actions as f64 / self.call_actions as f64)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OppType {
    Unknown,
    Nit,
    Tag,
    Station,
    Maniac,
    Lag,
}

/// 一手观测里的单条动作（街 + 动作）。
pub struct ObservedAction<'a> {
    pub name: &'a str,
    pub street: u8, // 0=preflop
    pub action: &'a str,
    /// 翻前是否盲位强制（盲位不计 vpip 机会）。
    pub forced_blind: bool,
}

/// P0-3 §D：一条摊牌观测 —— 某亮牌 villain 的 **final-board** 成手牌力。
///
/// `made_rank`（`MadeClass::rank()`）+ `nonnut`（`monster_non_nut`）由 sim/driver 用 `features` 现算后
/// 喂入 → `opponent.rs` **不依赖 `features`/`eval`**（采集层与决策/牌型评估层解耦，口径单点定义在调用侧）。
pub struct ShowdownObs<'a> {
    pub name: &'a str,
    /// `MadeClass::rank()`（0=Air..5=Monster），在 **final board** 上评。
    pub made_rank: u8,
    /// `monster_non_nut` 命中（非坚果 Monster：可被压制的同花**或**顺子）。
    pub nonnut: bool,
    /// `nonnut_flush` 命中（**flush-only** 非坚果同花）——候选① 真杠杆（见 [`ShowdownCounts`] doc）。
    pub nonnut_flush: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpponentTable {
    pub by_name: HashMap<String, OppStats>,
}

impl OpponentTable {
    pub fn new() -> Self {
        OpponentTable::default()
    }

    /// 从 JSON 加载（持久化经验）。失败返回空表。
    pub fn from_json_str(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// 喂一手观测（每条动作）。`new_hand_names` 用于 hands 计数（每手每名 +1）。
    pub fn observe_hand(&mut self, actions: &[ObservedAction], names_in_hand: &[String]) {
        for nm in names_in_hand {
            self.by_name.entry(nm.clone()).or_default().hands += 1;
        }
        // 翻前首动作判 vpip/pfr（每手每名只记第一次翻前自愿机会）。
        let mut counted_pre: HashMap<&str, bool> = HashMap::new();
        for a in actions {
            let st = self.by_name.entry(a.name.to_string()).or_default();
            if a.street == 0 {
                if !a.forced_blind && !counted_pre.get(a.name).copied().unwrap_or(false) {
                    st.vpip_opp += 1;
                    st.pfr_opp += 1;
                    match a.action {
                        "call" | "bet" | "raise" | "allin" | "all_in" => {
                            st.vpip_cnt += 1;
                            if a.action != "call" {
                                st.pfr_cnt += 1;
                            }
                        }
                        _ => {}
                    }
                    counted_pre.insert(a.name, true);
                }
            } else {
                match a.action {
                    "bet" | "raise" | "allin" | "all_in" => st.aggr_actions += 1,
                    "call" => st.call_actions += 1,
                    _ => {}
                }
            }
        }
        // P0-3 阶段1：per-node 面对下注响应重建（纯增采集，决策路径不读 → byte-equal）。
        self.observe_facing(actions);
    }

    /// P0-3 阶段1：从有序 `(name, street, action)` 序列重建 facing 上下文，填 `OppStats.facing`。
    ///
    /// 采三类节点：(a) 翻前 fold-to-3bet（`pre.facing_3bet`，§A）；(b) 翻后 fold-to-cbet
    /// （`<街>.<池型>`，villain 作**防守方**，§C）；(c) 翻后 **fold-to-raise**
    /// （`<街>.<池型>.facing_raise`，villain 作**进攻方**被 re-raise，§B——(a) 的翻后镜像）。
    ///
    /// 二值响应无需 amount/seat：name 在一手内唯一（与上面 `counted_pre` 同键口径），
    /// actions 已按时序排列（sim `obs` 按 play 序、live driver `observe_record` 按动作序）。
    /// fold-to-cbet 口径与 `tools/opponent_feature_weigh.py` `replay` 一致（7-run 实测校准的权威
    /// 口径）；fold-to-raise weigh.py 无对应 replay（改用造靶 `DonkFold`/`DonkStick` 方向自校验）。
    /// **决策路径不调用此函数，也不读它写入的 `facing`** —— 纯增采集，sim byte-equal。
    fn observe_facing(&mut self, actions: &[ObservedAction]) {
        // 池型：翻前 raise 次数（= `preflop_num_raises` 语义；首个 raiser = opener）。
        let mut pre_raises = 0u32;
        let mut opener: Option<&str> = None;
        for a in actions {
            if a.street == 0 && is_raise_action(a.action) {
                if pre_raises == 0 {
                    opener = Some(a.name);
                }
                pre_raises += 1;
            }
        }
        let pot_type = pot_type_name(pre_raises);

        // 翻前 fold-to-3bet：opener 面对站住的 3bet（链上恰好 2 个加注）的首次响应（§A）。
        {
            let mut raises = 0u32;
            let mut recorded = false;
            for a in actions.iter().filter(|a| a.street == 0) {
                if raises == 2 && Some(a.name) == opener && !recorded {
                    self.record_facing(a.name, "pre.facing_3bet", a.action);
                    recorded = true;
                }
                if is_raise_action(a.action) {
                    raises += 1;
                }
            }
        }

        // 翻后每街：街首注「开火」→ 其后每个防守方**首次面对该未平注**的响应（§C）。
        for street in 1u8..=3 {
            // key 经 `facing_node_key`（§G 扩展的唯一接口，见其文档）。
            let node = facing_node_key(street, pot_type);
            let mut opened = false;
            let mut recorded: Vec<&str> = Vec::new();
            for a in actions.iter().filter(|a| a.street == street) {
                if opened && !recorded.contains(&a.name) {
                    // 防守方面对未平注：记首个响应（每名每街一次）。
                    self.record_facing(a.name, &node, a.action);
                    recorded.push(a.name);
                    if is_raise_action(a.action) {
                        // 被 raise → 本街线重开；阶段1 只记「首次面对首注」，不追加注链
                        // （与 weigh.py fold-to-cbet 口径一致）。
                        break;
                    }
                }
                if is_raise_action(a.action) {
                    opened = true; // 开火者本身在此轮不入 facing（他是进攻方）。
                }
            }
        }

        // 翻后 fold-to-raise（§B「villain 作进攻方被加注」极性）：把翻前 `pre.facing_3bet` 镜像到
        // 翻后每街——街内**开火者**（首个 raiser = 领打/c-bet 方）面对站住的 re-raise（街内恰好 2 个
        // 加注）的**首次响应**。上面的 fold-to-cbet 循环在防守方 re-raise 处 `break` 且开火者本身不入
        // facing → fold-to-raise 从未进表，本循环补之（独立节点 key，与 fold-to-cbet 采集互不干扰）。
        // 节点 key 经 [`facing_raise_node_key`]（§G 扩展唯一接口，见其 doc）。`raises` 桶 = 开火者被
        // 加注后 re-raise（4bet/3bet-back）= 更高极性信号。**止步首次响应**（`recorded` 守卫，不追
        // 4bet/5bet 链，照 `pre.facing_3bet` 同纪律）。
        for street in 1u8..=3 {
            let node = facing_raise_node_key(street, pot_type);
            let mut raises = 0u32;
            let mut fire_starter: Option<&str> = None;
            let mut recorded = false;
            for a in actions.iter().filter(|a| a.street == street) {
                if raises == 2 && Some(a.name) == fire_starter && !recorded {
                    self.record_facing(a.name, &node, a.action);
                    recorded = true;
                }
                if is_raise_action(a.action) {
                    if raises == 0 {
                        fire_starter = Some(a.name);
                    }
                    raises += 1;
                }
            }
        }
    }

    /// 把一次「防守方面对未平注」的响应记进 `facing[node]`。check/其他不计入分母
    /// （保 `n = folds+calls+raises` 恒等）。
    fn record_facing(&mut self, name: &str, node: &str, action: &str) {
        let fc = self
            .by_name
            .entry(name.to_string())
            .or_default()
            .facing
            .entry(node.to_string())
            .or_default();
        let al = action.to_ascii_lowercase();
        match al.as_str() {
            "fold" => {
                fc.n += 1;
                fc.folds += 1;
            }
            "call" => {
                fc.n += 1;
                fc.calls += 1;
            }
            "bet" | "raise" | "allin" | "all_in" | "all-in" => {
                fc.n += 1;
                fc.raises += 1;
            }
            _ => {}
        }
    }

    /// P0-3 §D：喂一手摊牌观测 —— 为每个亮牌 villain（`showdowns`），把其 **final-board** 成手牌力归到
    /// 「该 villain 在各街**作进攻方**（该街 bet/raise）」的 `<街>.<池型>.attack_showdown` 节点。
    ///
    /// 与 [`Self::observe_facing`] **独立并存**：facing 采「面对下注的响应」（fold-to-cbet / fold-to-raise
    /// 桶）；本函数采「攻击方摊牌的牌力构成」（候选① range 极性真信号，§B 频率代理撞车的解药）。池型
    /// 口径同 observe_facing（翻前 raise 数派生）。
    ///
    /// **多街归属**：villain triple-barrel 摊牌 → flop/turn/river 三节点各记一次（**同一 final-board
    /// 牌力**）。river 节点是候选① 主用（final board = river 攻击时的板，语义精确）；flop/turn 为副
    /// （final board ≠ 攻击时该街的板，用于将来别的 street 极性度量）。
    ///
    /// **选择偏差（务必显式）**：`showdowns` 分母只含「攻击被跟到摊牌」的手——被弃的攻击无亮牌 → §D
    /// range 是**摊牌子集**、非全攻击 range（`opponent_features_inventory.md` §D 截断偏差）。对候选① 无害：
    /// FlushTrap 的坚果同花攻击会被 hero 非坚果同花跟到摊牌（正是要度量的 spot）。
    ///
    /// **决策路径不调用此函数，也不读它写入的 `attack_showdown`** —— 纯增采集，sim byte-equal。
    pub fn observe_showdown(&mut self, actions: &[ObservedAction], showdowns: &[ShowdownObs]) {
        // 池型：翻前 raise 次数（= observe_facing 同口径 `preflop_num_raises`）。
        let mut pre_raises = 0u32;
        for a in actions {
            if a.street == 0 && is_raise_action(a.action) {
                pre_raises += 1;
            }
        }
        let pot_type = pot_type_name(pre_raises);

        for sd in showdowns {
            for street in 1u8..=3 {
                // 该 villain 在本街是否作进攻方（该街有任一 bet/raise）。
                let attacked = actions
                    .iter()
                    .any(|a| a.name == sd.name && a.street == street && is_raise_action(a.action));
                if attacked {
                    let node = attack_showdown_node_key(street, pot_type);
                    self.record_showdown(sd.name, &node, sd);
                }
            }
        }
    }

    /// 记一次「攻击方摊牌」到 `attack_showdown[node]`（§0.5 原始计数：分母 + 非坚果命中 + 成手直方）。
    fn record_showdown(&mut self, name: &str, node: &str, sd: &ShowdownObs) {
        let sc = self
            .by_name
            .entry(name.to_string())
            .or_default()
            .attack_showdown
            .entry(node.to_string())
            .or_default();
        sc.showdowns += 1;
        if sd.nonnut {
            sc.nonnut += 1;
        }
        if sd.nonnut_flush {
            sc.nonnut_flush += 1;
        }
        let idx = (sd.made_rank as usize).min(5);
        sc.by_made_class[idx] += 1;
    }

    /// 据累计画像分类对手。主阈值取自 [`ExploitConfig`]（`min_hands`/`nit_vpip`/`station_vpip`/
    /// `maniac_af`）——默认值与历史硬编码字面量一致（0.16/0.45/3.5）→ 默认行为 byte-equal。
    /// 次级判据（station 的 AF 上限 1.5、maniac/lag 的 vpip-pfr 组合）暂无独立旋钮，保留经验字面量。
    pub fn classify(&self, name: &str, cfg: &ExploitConfig) -> OppType {
        let st = match self.by_name.get(name) {
            Some(s) if s.hands >= cfg.min_hands => s,
            _ => return OppType::Unknown,
        };
        let vpip = st.vpip().unwrap_or(0.25);
        let pfr = st.pfr().unwrap_or(0.15);
        let af = st.af().unwrap_or(1.5);
        let gap = (vpip - pfr).max(0.0);
        if vpip < cfg.nit_vpip {
            OppType::Nit
        } else if vpip > cfg.station_vpip && af < 1.5 {
            OppType::Station
        } else if af > cfg.maniac_af || (vpip > 0.40 && pfr > 0.30) {
            OppType::Maniac
        } else if gap > 0.18 && vpip > 0.30 {
            OppType::Lag
        } else {
            OppType::Tag
        }
    }

    /// P0-3 阶段1：per-node 面对下注读数（原始计数 + 按 `n` 向 field 先验收缩）。
    /// `k` = 收缩伪计数强度（先验等效样本数；默认建议 20 ≈ 实测 fold-to-cbet 中位样本）。
    /// **阶段1 决策路径不调用此访问器** —— 仅供 dump/inspect 与阶段2 门控。
    pub fn node_readout(&self, name: &str, node_id: &str, k: f64) -> NodeReadout {
        let prior = node_field_prior(node_id);
        let fc = self
            .by_name
            .get(name)
            .and_then(|s| s.facing.get(node_id))
            .cloned()
            .unwrap_or_default();
        NodeReadout {
            node_id: node_id.to_string(),
            n: fc.n,
            raw_fold: fc.raw_fold_rate(),
            shrunk_fold: fc.shrunk_fold_rate(prior.fold, k),
            raw_raise: fc.raw_raise_rate(),
            shrunk_raise: fc.shrunk_raise_rate(prior.raise, k),
        }
    }

    /// P0-3 §D：per-node 摊牌攻击读数（原始计数 + 现算非坚果率）。**决策路径不调用此访问器** ——
    /// 仅供 dump/inspect 与后续 gate（候选① river 极化门重做）。无 field 先验/收缩（§D 无摊牌率先验；
    /// 接门时按样本再定收缩口径）。
    pub fn showdown_readout(&self, name: &str, node_id: &str) -> ShowdownReadout {
        let sc = self
            .by_name
            .get(name)
            .and_then(|s| s.attack_showdown.get(node_id))
            .cloned()
            .unwrap_or_default();
        ShowdownReadout {
            node_id: node_id.to_string(),
            showdowns: sc.showdowns,
            nonnut: sc.nonnut,
            nonnut_flush: sc.nonnut_flush,
            raw_nonnut: sc.raw_nonnut_rate(),
            raw_nonnut_flush: sc.raw_nonnut_flush_rate(),
            by_made_class: sc.by_made_class,
        }
    }

    /// 对手 range 强度阈值偏移（station/松 → 负=放宽；nit/紧 → 正=收紧）。
    pub fn range_threshold_shift(&self, name: &str, cfg: &ExploitConfig) -> f64 {
        let cap = cfg.adjust_cap;
        match self.classify(name, cfg) {
            OppType::Nit => cap,
            OppType::Tag => 0.0,
            OppType::Station => -cap,
            OppType::Maniac => -cap * 0.8,
            OppType::Lag => -cap * 0.4,
            OppType::Unknown => 0.0,
        }
    }
}

/// 一组活跃对手对我方决策的聚合剥削调整。
#[derive(Copy, Clone, Debug)]
pub struct ExploitAdjust {
    /// 价值门槛偏移（负 = 敢更薄价值，对跟注站）。
    pub value_shift: f64,
    /// 诈唬频率乘子（<1 = 少诈，对跟注站；>1 = 多诈，对紧手）。
    pub bluff_mult: f64,
    /// 偷盲加成（对身后全是紧手时多偷）。
    pub steal_bonus: f64,
    /// 价值下注/加注尺寸乘子（>1 = 对跟注站把价值注做大，多榨筹码）。仅价值动作放大，诈唬不动
    /// （诈唬放大会增亏，且站不会因尺寸不平衡而弃）。1.0 = 不变。
    pub value_size_mult: f64,
}

impl Default for ExploitAdjust {
    fn default() -> Self {
        ExploitAdjust {
            value_shift: 0.0,
            bluff_mult: 1.0,
            steal_bonus: 0.0,
            value_size_mult: 1.0,
        }
    }
}

impl OpponentTable {
    /// 据一组活跃对手名聚合剥削调整。
    pub fn aggregate_exploit(&self, active_names: &[String], cfg: &ExploitConfig) -> ExploitAdjust {
        let cap = cfg.adjust_cap;
        let mut adj = ExploitAdjust::default();
        if active_names.is_empty() {
            return adj;
        }
        let mut any_station = false;
        let mut any_maniac = false;
        let mut all_nit = true;
        let mut known = 0;
        for nm in active_names {
            let t = self.classify(nm, cfg);
            if t != OppType::Unknown {
                known += 1;
            }
            match t {
                OppType::Station => any_station = true,
                OppType::Maniac => any_maniac = true,
                OppType::Nit => {}
                _ => all_nit = false,
            }
            if t != OppType::Nit && t != OppType::Unknown {
                all_nit = false;
            }
        }
        if any_station {
            // 跟注站在场：价值放薄、诈唬大砍、价值注做大（站会跟更大注 → 多榨）。
            adj.value_shift -= cap;
            adj.bluff_mult *= 0.5;
            adj.value_size_mult *= cfg.station_value_size_mult;
        }
        if any_maniac {
            // 疯子：抓诈跟注放松（价值门槛略降），自己少主动诈。
            adj.value_shift -= cap * 0.4;
            adj.bluff_mult *= 0.8;
        }
        if all_nit && known >= 1 {
            // 全紧手：多偷、多诈。
            adj.steal_bonus += cap;
            adj.bluff_mult *= 1.3;
        }
        adj
    }
}
