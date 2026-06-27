//! 对手画像 + 剥削调整 + 持久化。
//!
//! 「累积经验」在这里变现：跨手、跨 session 记每个对手名的 VPIP/PFR/AF，分类成
//! nit/tag/station/maniac，再把它翻译成「对手 range 松紧偏移」+「我方价值/诈唬调整」。
//! 弱场里这是最大 EV 来源——纯 GTO 留在桌上的钱，剥削能拿走。

use crate::heuristic::config::ExploitConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
}

impl Default for ExploitAdjust {
    fn default() -> Self {
        ExploitAdjust {
            value_shift: 0.0,
            bluff_mult: 1.0,
            steal_bonus: 0.0,
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
            // 跟注站在场：价值放薄、诈唬大砍。
            adj.value_shift -= cap;
            adj.bluff_mult *= 0.5;
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
