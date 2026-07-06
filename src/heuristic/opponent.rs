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
