//! 进程内对手画像（叠加剥削 Tier 2 的统计层，`exploit_strategy_design_2026_06_14` 实现）。
//!
//! **只用当前进程的数据、不依赖过往**（用户拍板偏离设计草案 §5.1 的跨 session profile DB）：
//! [`Profiler`] 进程启动时空，靠 driver 每手结束发来的 `observe` 消息（[`ObserveHand`]）累积
//! per-name 频率统计；**仅当某玩家行为频率有收敛趋势**（[`Profiler::profile_for`] 三条门全过）
//! 才产出 [`OpponentProfile`]，供搜索层（`subgame::apply_exploit_width_prior`）把对手子博弈
//! root range 朝其观测翻前宽度偏置。
//!
//! 口径（coarse 但几百手够，设计草案 §1）：
//! - **VPIP** = 翻前曾主动 call/bet/raise（盲注不入 `actions` → 不计）的手占比。
//! - **PFR** = 翻前曾 bet/raise 的手占比。
//! - **翻后 AF** = (bet+raise+allin)/call。v1 仅作遥测 + 未来激进度轴，**不进**翻前宽度门。
//!
//! 完整性：driver 在我方弃牌后仍续记牌桌动作到 hand_result（memory
//! `reference_openpoker_hh_jsonl_parsing`），故 observe 覆盖整手所有座位的动作（含我退出后的街），
//! 对手统计无「只看我参与的线」偏差。街由 driver 侧可靠跟踪（避开 `actions_ext.street` 错位坑）。

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::Deserialize;

/// 一条 observe 动作（driver 逐动作落 seat / driver 侧跟踪的街 / 动作类型）。
#[derive(Deserialize, Debug, Clone)]
pub struct ObsAction {
    pub seat: u8,
    /// driver 侧街名："preflop" / "flop" / "turn" / "river"（其它 → 跳过该动作）。
    pub street: String,
    /// 动作："fold" / "check" / "call" / "bet" / "raise" / "all_in"|"allin"|"all-in"。
    pub action: String,
}

/// driver 每手结束发来的 observe 载荷（`{"observe":1,"names":{seat:name},"actions":[...]}`）。
/// serde 忽略未知字段 → 与 hh_record 同行可共存（advisor 只解析它需要的子集）。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct ObserveHand {
    /// seat → 玩家名（OpenPoker 座位号）。
    #[serde(default)]
    pub names: BTreeMap<u8, String>,
    #[serde(default)]
    pub actions: Vec<ObsAction>,
}

/// 已收敛对手的画像摘要（[`Profiler::profile_for`] 仅收敛才产出）。`Copy` 便于决策路径收集成
/// per-solver-seat 向量。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpponentProfile {
    /// 翻前入池率 ∈ [0,1]（翻前宽度 tilt 唯一驱动量）。
    pub vpip: f64,
    /// 翻前加注率 ∈ [0,1]（PFR-aware 形状用：见 [`pfr_converged`](Self::pfr_converged)）。
    pub pfr: f64,
    /// PFR 是否也收敛（`se(pfr) ≤ converge_se`，分母同 VPIP 门的 `n_preflop`）。`true` 才允许搜索层
    /// 据 PFR 选 `CallBand`/`RaiseBand` 形状；`false` → 退 `TopK`（仅 VPIP）。VPIP 已收敛是前提
    /// （`profile_for` 只在 VPIP 三门全过才产出画像）。
    pub pfr_converged: bool,
    /// 翻后激进度 = (bet+raise+allin)/call（v1 仅遥测 + 未来激进度轴）。
    pub postflop_af: f64,
    /// 翻前样本量（收敛门分母）。
    pub n_preflop: u32,
    /// 翻后计入的动作数（aggr+passive）。
    pub n_postflop: u32,
}

/// 剥削配置（CLI 可调；`Copy` 纯配置）。
#[derive(Debug, Clone, Copy)]
pub struct ExploitConfig {
    /// 最小翻前样本（设计草案 §1「N≥150 可信样本」）。
    pub min_hands: u32,
    /// VPIP 标准误上限（设计草案 §1「VPIP 400 手 SE≈2%」；`se=sqrt(p(1-p)/n)`）。
    pub converge_se: f64,
    /// 近窗 VPIP 与累计 VPIP 的最大漂移（拦「行为还在漂」的玩家 = 收敛**趋势**而非仅样本量）。
    pub converge_drift: f64,
    /// 翻前宽度 tilt 的混合强度 α（搜索层用；clamp 见 `subgame::apply_exploit_width_prior`）。
    pub strength_alpha: f64,
    /// 漂移检测的近窗手数。
    pub window: usize,
    /// PFR-aware 宽度形状开关（advisor `--exploit-pfr-shape`）。`false`（默认）= 全程 `TopK`（仅 VPIP，
    /// 与现有 exploit 行为逐位 byte-equal）；`true` = 据对手 PFR 收敛性 + 本手翻前入池方式选
    /// `CallBand`（被动入池掐顶端）/`RaiseBand`（主动入池收顶端）。
    pub pfr_shape: bool,
}

impl Default for ExploitConfig {
    fn default() -> Self {
        ExploitConfig {
            min_hands: 150,
            converge_se: 0.05,
            converge_drift: 0.08,
            strength_alpha: 0.5,
            window: 50,
            pfr_shape: false,
        }
    }
}

/// 单动作的语义分类（街无关）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActKind {
    Fold,
    Check,
    Call,
    /// bet / raise / all-in（主动投注）。
    Aggr,
}

fn classify(action: &str) -> Option<ActKind> {
    match action.trim().to_ascii_lowercase().as_str() {
        "fold" => Some(ActKind::Fold),
        "check" => Some(ActKind::Check),
        "call" => Some(ActKind::Call),
        "bet" | "raise" | "all_in" | "allin" | "all-in" => Some(ActKind::Aggr),
        _ => None,
    }
}

fn is_preflop(street: &str) -> bool {
    street.eq_ignore_ascii_case("preflop")
}

fn is_postflop(street: &str) -> bool {
    matches!(
        street.trim().to_ascii_lowercase().as_str(),
        "flop" | "turn" | "river"
    )
}

#[derive(Debug, Default, Clone)]
struct NameStats {
    n_preflop: u32,
    vpip_count: u32,
    pfr_count: u32,
    postflop_aggr: u32,
    postflop_passive: u32,
    /// 近窗 VPIP 0/1（漂移检测；长度 ≤ `cfg.window`）。
    window: VecDeque<u8>,
    window_sum: u32,
}

impl NameStats {
    fn push_window(&mut self, voluntary: bool, cap: usize) {
        if cap == 0 {
            return;
        }
        let v = u8::from(voluntary);
        self.window.push_back(v);
        self.window_sum += u32::from(v);
        while self.window.len() > cap {
            if let Some(old) = self.window.pop_front() {
                self.window_sum -= u32::from(old);
            }
        }
    }
}

/// 进程内 per-name 对手画像累积器。
pub struct Profiler {
    cfg: ExploitConfig,
    stats: HashMap<String, NameStats>,
}

impl Profiler {
    pub fn new(cfg: ExploitConfig) -> Profiler {
        Profiler {
            cfg,
            stats: HashMap::new(),
        }
    }

    pub fn cfg(&self) -> &ExploitConfig {
        &self.cfg
    }

    /// 已观测的不同玩家数（遥测用）。
    pub fn names_seen(&self) -> usize {
        self.stats.len()
    }

    /// 吃一手完整 observe：按座聚合翻前/翻后，再按 `names` 折进 per-name 统计。脏/未知动作或
    /// 无名座静默跳过（不污染、不崩，仿 advisor fallback 语义）。
    pub fn observe_hand(&mut self, hand: &ObserveHand) {
        // 按座聚合：翻前 (voluntary, raised)、翻后 (aggr, passive)。
        let mut pre: BTreeMap<u8, (bool, bool)> = BTreeMap::new();
        let mut post: BTreeMap<u8, (u32, u32)> = BTreeMap::new();
        for a in &hand.actions {
            let Some(kind) = classify(&a.action) else {
                continue;
            };
            if is_preflop(&a.street) {
                let e = pre.entry(a.seat).or_insert((false, false));
                match kind {
                    ActKind::Call => e.0 = true,
                    ActKind::Aggr => {
                        e.0 = true;
                        e.1 = true;
                    }
                    ActKind::Fold | ActKind::Check => {}
                }
            } else if is_postflop(&a.street) {
                let e = post.entry(a.seat).or_insert((0, 0));
                match kind {
                    ActKind::Aggr => e.0 += 1,
                    ActKind::Call => e.1 += 1,
                    ActKind::Fold | ActKind::Check => {}
                }
            }
        }
        let window = self.cfg.window;
        for (seat, (voluntary, raised)) in pre {
            let Some(name) = hand.names.get(&seat) else {
                continue; // 无名座：无法按名 key，跳过。
            };
            let st = self.stats.entry(name.clone()).or_default();
            st.n_preflop += 1;
            if voluntary {
                st.vpip_count += 1;
            }
            if raised {
                st.pfr_count += 1;
            }
            st.push_window(voluntary, window);
        }
        for (seat, (aggr, passive)) in post {
            let Some(name) = hand.names.get(&seat) else {
                continue;
            };
            let st = self.stats.entry(name.clone()).or_default();
            st.postflop_aggr += aggr;
            st.postflop_passive += passive;
        }
    }

    /// 已收敛才产出画像（三条门：样本量 / VPIP 标准误 / 近窗漂移）。任一未过 → `None`（搜索层
    /// 对该座不剥削，回退 GTO）。
    pub fn profile_for(&self, name: &str) -> Option<OpponentProfile> {
        let st = self.stats.get(name)?;
        if st.n_preflop < self.cfg.min_hands {
            return None;
        }
        let n = f64::from(st.n_preflop);
        let vpip = f64::from(st.vpip_count) / n;
        let se = (vpip * (1.0 - vpip) / n).sqrt();
        if se > self.cfg.converge_se {
            return None;
        }
        if !st.window.is_empty() {
            let wlen = st.window.len() as f64;
            let wv = f64::from(st.window_sum) / wlen;
            if (wv - vpip).abs() > self.cfg.converge_drift {
                return None; // 行为还在漂 → 不算收敛趋势。
            }
        }
        let pfr = f64::from(st.pfr_count) / n;
        // PFR 也用同口径 SE 门（分母同 n_preflop，VPIP 三门已过 → n≥min_hands）。se(pfr)=0（pfr=0/1，
        // 如 3bet=0 的纯被动跟注站）天然收敛 = 我们对其「不加注」很确定。
        let se_pfr = (pfr * (1.0 - pfr) / n).sqrt();
        let pfr_converged = se_pfr <= self.cfg.converge_se;
        let denom = st.postflop_passive.max(1);
        let postflop_af = f64::from(st.postflop_aggr) / f64::from(denom);
        Some(OpponentProfile {
            vpip,
            pfr,
            pfr_converged,
            postflop_af,
            n_preflop: st.n_preflop,
            n_postflop: st.postflop_aggr + st.postflop_passive,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一手：names + (seat, street, action) 列表。
    fn hand(names: &[(u8, &str)], acts: &[(u8, &str, &str)]) -> ObserveHand {
        ObserveHand {
            names: names.iter().map(|(s, n)| (*s, n.to_string())).collect(),
            actions: acts
                .iter()
                .map(|(s, st, a)| ObsAction {
                    seat: *s,
                    street: st.to_string(),
                    action: a.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn observe_json_parses_string_seat_keys() {
        // serde_json 把 JSON 字符串键解析成 u8（advisor Request.names 同机制）。
        let j = r#"{"observe":1,"names":{"0":"alice","3":"bob"},
                    "actions":[{"seat":0,"street":"preflop","action":"raise"},
                               {"seat":3,"street":"preflop","action":"fold"}]}"#;
        let h: ObserveHand = serde_json::from_str(j).unwrap();
        assert_eq!(h.names.get(&0).map(String::as_str), Some("alice"));
        assert_eq!(h.names.get(&3).map(String::as_str), Some("bob"));
        assert_eq!(h.actions.len(), 2);
    }

    #[test]
    fn below_min_hands_not_converged() {
        let cfg = ExploitConfig {
            min_hands: 150,
            ..Default::default()
        };
        let mut p = Profiler::new(cfg);
        for _ in 0..100 {
            p.observe_hand(&hand(&[(0, "x")], &[(0, "preflop", "call")]));
        }
        assert!(p.profile_for("x").is_none(), "100 < 150 手不应收敛");
    }

    #[test]
    fn stable_loose_player_converges_with_high_vpip() {
        let cfg = ExploitConfig {
            min_hands: 150,
            converge_se: 0.05,
            converge_drift: 0.1,
            ..Default::default()
        };
        let mut p = Profiler::new(cfg);
        // 稳定 40% VPIP（每 5 手 2 主动入池、3 弃）：n=400。
        for i in 0..400 {
            let act = if i % 5 < 2 { "call" } else { "fold" };
            p.observe_hand(&hand(&[(2, "loose")], &[(2, "preflop", act)]));
        }
        let prof = p.profile_for("loose").expect("400 稳定手应收敛");
        assert_eq!(prof.n_preflop, 400);
        assert!(
            (prof.vpip - 0.4).abs() < 0.02,
            "vpip 应≈0.40，得 {}",
            prof.vpip
        );
    }

    #[test]
    fn drifting_player_not_converged() {
        // 前半极松(80%)、后半极紧(0%)：累计 vpip≈0.4 但近窗≈0 → 漂移门拦下。
        let cfg = ExploitConfig {
            min_hands: 150,
            converge_se: 0.1, // 放宽 SE，单独验漂移门
            converge_drift: 0.08,
            window: 50,
            ..Default::default()
        };
        let mut p = Profiler::new(cfg);
        for _ in 0..200 {
            p.observe_hand(&hand(&[(1, "drift")], &[(1, "preflop", "raise")]));
        }
        for _ in 0..200 {
            p.observe_hand(&hand(&[(1, "drift")], &[(1, "preflop", "fold")]));
        }
        assert!(
            p.profile_for("drift").is_none(),
            "近窗 VPIP≈0 vs 累计≈0.4，漂移应拦下"
        );
    }

    #[test]
    fn vpip_pfr_af_counting() {
        let cfg = ExploitConfig {
            min_hands: 1,
            converge_se: 1.0,
            converge_drift: 1.0,
            ..Default::default()
        };
        let mut p = Profiler::new(cfg);
        // 一手：seat0 翻前 raise（vpip+pfr），翻后 bet（aggr）再 call（passive）。
        p.observe_hand(&hand(
            &[(0, "h")],
            &[
                (0, "preflop", "raise"),
                (0, "flop", "bet"),
                (0, "turn", "call"),
            ],
        ));
        let prof = p.profile_for("h").unwrap();
        assert_eq!(prof.vpip, 1.0);
        assert_eq!(prof.pfr, 1.0);
        assert_eq!(prof.n_postflop, 2);
        assert!((prof.postflop_af - 1.0).abs() < 1e-9, "aggr1/passive1=1.0");
    }

    #[test]
    fn bb_check_is_not_vpip() {
        let cfg = ExploitConfig {
            min_hands: 1,
            converge_se: 1.0,
            converge_drift: 1.0,
            ..Default::default()
        };
        let mut p = Profiler::new(cfg);
        // BB 免费过牌 → 非主动入池（盲注不入 actions，首动作 check）。
        p.observe_hand(&hand(&[(0, "bb")], &[(0, "preflop", "check")]));
        let prof = p.profile_for("bb").unwrap();
        assert_eq!(prof.vpip, 0.0, "BB check 不计 VPIP");
    }
}
