//! 进程内对手画像（叠加剥削 Tier 2 的统计层，`exploit_strategy_design_2026_06_14` 实现）。
//!
//! **只用当前进程的数据**（`--warm-profile-hh` 暖启动另算）：[`Profiler`] 进程启动时空，靠 driver
//! 每手结束发来的 `observe` 消息（[`ObserveHand`]）累积 per-name 频率统计。[`Profiler::profile_for`]
//! 对见过的玩家都产出 [`OpponentProfile`]，样本少时**收缩**而不是拒绝：VPIP / PFR 按 `k` 手伪样本往
//! 人群均值拉，剥削权重 `n/(n+k)` 随样本增加（`docs/temp/preflop_range_offanchor_research_2026_10_29.md`
//! §4.1）。搜索层（`subgame::apply_exploit_width_prior`）据此把对手子博弈 root range 朝其翻前宽度偏置。
//!
//! 口径（coarse 但几百手够，设计草案 §1）：
//! - **VPIP** = 翻前曾主动 call/bet/raise（盲注不入 `actions` → 不计）的手占比。
//! - **PFR** = 翻前曾 bet/raise 的手占比。
//! - **翻后 AF** = (bet+raise+allin)/call。v1 仅作遥测 + 未来激进度轴，**不进**翻前宽度门。
//!
//! 完整性：driver 在我方弃牌后仍续记牌桌动作到 hand_result（memory
//! `reference_openpoker_hh_jsonl_parsing`），故 observe 覆盖整手所有座位的动作（含我退出后的街），
//! 对手统计无「只看我参与的线」偏差。街由 driver 侧可靠跟踪（避开 `actions_ext.street` 错位坑）。

use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;

/// 一条 observe 动作（driver 逐动作落 seat / driver 侧跟踪的街 / 动作类型）。
#[derive(Deserialize, Debug, Clone)]
pub struct ObsAction {
    pub seat: u8,
    /// driver 侧街名："preflop" / "flop" / "turn" / "river"（其它 → 跳过该动作）。
    pub street: String,
    /// 动作："fold" / "check" / "call" / "bet" / "raise" / "all_in"|"allin"|"all-in"。
    pub action: String,
    /// 本动作投入的筹码（OpenPoker `contribution_delta`）。仅 driver `--exploit-4bet` 开时带；
    /// [`Profiler`] 不读，[`crate::training::fourbet_exploit`] 用它区分「全下但只是跟注」。
    #[serde(default)]
    pub contribution_delta: Option<u64>,
    /// 本动作前需补的跟注额（OpenPoker `to_call_before`，`null` = 0）。同上，仅 `--exploit-4bet` 开时带。
    #[serde(default)]
    pub to_call_before: Option<u64>,
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

/// 对手画像摘要（[`Profiler::profile_for`] 产出）。`Copy` 便于决策路径收集成 per-solver-seat 向量。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpponentProfile {
    /// 收缩后的翻前入池率 ∈ [0,1]（翻前宽度 tilt 的驱动量）：`(n·实测 + k·人群) / (n + k)`。
    pub vpip: f64,
    /// 收缩后的翻前加注率 ∈ [0,1]（PFR-aware 形状用），口径同 `vpip`；收缩前后都保持 `pfr ≤ vpip`。
    pub pfr: f64,
    /// 剥削权重 `n/(n+k)` ∈ [0,1)：搜索层的混合强度 = `strength_alpha × weight`。
    pub weight: f64,
    /// 翻后激进度 = (bet+raise+allin)/call（v1 仅遥测 + 未来激进度轴）。
    pub postflop_af: f64,
    /// 翻前样本量。
    pub n_preflop: u32,
    /// 翻后计入的动作数（aggr+passive）。
    pub n_postflop: u32,
}

/// 剥削配置（CLI 可调；`Copy` 纯配置）。
#[derive(Debug, Clone, Copy)]
pub struct ExploitConfig {
    /// 收缩伪样本数 `k`（手）：`n = k` 时实测与人群各占一半、剥削权重 0.5。
    pub shrink_k: f64,
    /// 翻前宽度 tilt 的混合强度 α（样本无穷多时的上限；搜索层再乘 `weight`，clamp 见
    /// `subgame::apply_exploit_width_prior`）。
    pub strength_alpha: f64,
    /// PFR-aware 宽度形状开关（advisor `--exploit on` 置 `true`、`--exploit vpip` 置 `false`）。
    /// `false`（默认）= 全程 `TopK`（仅 VPIP）；`true` = 据本手翻前入池方式选 `CallBand`（被动入池掐顶端）
    /// / `RaiseBand`（主动入池收顶端）。
    pub pfr_shape: bool,
}

impl Default for ExploitConfig {
    fn default() -> Self {
        ExploitConfig {
            shrink_k: 50.0,
            strength_alpha: 0.5,
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

/// [`classify`] 加金额校正：带了投入额（driver `--exploit-4bet` 开时）且投入不超过跟注额 → 跟注
/// （短码全下只够跟注，不算加注；同 [`crate::training::fourbet_exploit`] 口径）。没带金额 → 只看动作名。
fn classify_obs(a: &ObsAction) -> Option<ActKind> {
    match (classify(&a.action)?, a.contribution_delta) {
        (ActKind::Aggr, Some(cd)) if cd <= a.to_call_before.unwrap_or(0) => Some(ActKind::Call),
        (k, _) => Some(k),
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

#[derive(Debug, Default, Clone, Copy)]
struct NameStats {
    n_preflop: u32,
    vpip_count: u32,
    pfr_count: u32,
    postflop_aggr: u32,
    postflop_passive: u32,
}

/// 进程内 per-name 对手画像累积器。
pub struct Profiler {
    cfg: ExploitConfig,
    stats: HashMap<String, NameStats>,
    /// 所有名字的翻前计数之和（人群均值用；`profile_for` 里扣掉目标和 hero）。
    total: NameStats,
}

impl Profiler {
    pub fn new(cfg: ExploitConfig) -> Profiler {
        Profiler {
            cfg,
            stats: HashMap::new(),
            total: NameStats::default(),
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
            let Some(kind) = classify_obs(a) else {
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
        for (seat, (voluntary, raised)) in pre {
            let Some(name) = hand.names.get(&seat) else {
                continue; // 无名座：无法按名 key，跳过。
            };
            let st = self.stats.entry(name.clone()).or_default();
            st.n_preflop += 1;
            st.vpip_count += u32::from(voluntary);
            st.pfr_count += u32::from(raised);
            self.total.n_preflop += 1;
            self.total.vpip_count += u32::from(voluntary);
            self.total.pfr_count += u32::from(raised);
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

    /// `name` 的画像；没见过（无翻前样本）→ `None`。`hero` = 我方名字（人群均值里扣掉，我方是 bot，
    /// 不代表对手人群）。
    ///
    /// 收缩：`vpip = (n·vpip_实测 + k·vpip_人群) / (n + k)`，PFR 同式，`weight = n/(n+k)`。人群 =
    /// 除目标和 hero 之外所有名字的翻前计数之和；人群为空 → 用目标自己的实测（只靠 `weight` 收缩）。
    pub fn profile_for(&self, name: &str, hero: Option<&str>) -> Option<OpponentProfile> {
        let st = self.stats.get(name)?;
        if st.n_preflop == 0 {
            return None;
        }
        let mut pool = self.total;
        let mut minus = |x: &NameStats| {
            pool.n_preflop -= x.n_preflop;
            pool.vpip_count -= x.vpip_count;
            pool.pfr_count -= x.pfr_count;
        };
        minus(st);
        if let Some(h) = hero.filter(|&h| h != name) {
            if let Some(hs) = self.stats.get(h) {
                minus(hs);
            }
        }
        let n = f64::from(st.n_preflop);
        let rate = |count: u32, of: u32| f64::from(count) / f64::from(of);
        let (vpip_raw, pfr_raw) = (
            rate(st.vpip_count, st.n_preflop),
            rate(st.pfr_count, st.n_preflop),
        );
        let (vpip_pop, pfr_pop) = if pool.n_preflop > 0 {
            (
                rate(pool.vpip_count, pool.n_preflop),
                rate(pool.pfr_count, pool.n_preflop),
            )
        } else {
            (vpip_raw, pfr_raw)
        };
        let k = self.cfg.shrink_k.max(0.0);
        let weight = n / (n + k);
        let denom = st.postflop_passive.max(1);
        Some(OpponentProfile {
            vpip: weight * vpip_raw + (1.0 - weight) * vpip_pop,
            pfr: weight * pfr_raw + (1.0 - weight) * pfr_pop,
            weight,
            postflop_af: f64::from(st.postflop_aggr) / f64::from(denom),
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
                    contribution_delta: None,
                    to_call_before: None,
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
    fn short_allin_call_counts_as_vpip_not_pfr() {
        let mut p = Profiler::new(ExploitConfig::default());
        let allin = |cd: Option<u64>, tc: Option<u64>| ObserveHand {
            names: [(0, "s".to_string())].into_iter().collect(),
            actions: vec![ObsAction {
                seat: 0,
                street: "preflop".into(),
                action: "all_in".into(),
                contribution_delta: cd,
                to_call_before: tc,
            }],
        };
        // 全下只够跟注 → 入池不算加注；全下超过跟注额 → 加注；没带金额 → 照动作名算加注（旧口径）。
        p.observe_hand(&allin(Some(80), Some(100)));
        p.observe_hand(&allin(Some(100), Some(100)));
        p.observe_hand(&allin(Some(300), Some(100)));
        p.observe_hand(&allin(None, None));
        // 只有一个名字 → 人群为空，vpip / pfr 就是实测。
        let prof = p.profile_for("s", None).expect("见过");
        assert_eq!(prof.n_preflop, 4);
        assert!((prof.vpip - 1.0).abs() < 1e-12, "vpip={}", prof.vpip);
        assert!((prof.pfr - 0.5).abs() < 1e-12, "pfr={}", prof.pfr);
    }

    /// 喂 `n` 手：前 `vpip_hands` 手平跟（入池不加注）、其余弃牌。
    fn feed(p: &mut Profiler, seat: u8, name: &str, n: u32, vpip_hands: u32) {
        for i in 0..n {
            let act = if i < vpip_hands { "call" } else { "fold" };
            p.observe_hand(&hand(&[(seat, name)], &[(seat, "preflop", act)]));
        }
    }

    #[test]
    fn unseen_name_has_no_profile() {
        let mut p = Profiler::new(ExploitConfig::default());
        feed(&mut p, 0, "a", 10, 5);
        assert!(p.profile_for("b", None).is_none());
    }

    #[test]
    fn shrinks_toward_population_and_weight_grows() {
        let mut p = Profiler::new(ExploitConfig {
            shrink_k: 50.0,
            ..Default::default()
        });
        // 人群：两个人共 200 手、VPIP 0.2。
        feed(&mut p, 1, "x", 100, 10);
        feed(&mut p, 2, "y", 100, 30);
        // 目标 50 手、VPIP 0.6 → n = k：实测与人群各半 → 0.4，权重 0.5。
        feed(&mut p, 3, "t", 50, 30);
        let prof = p.profile_for("t", None).unwrap();
        assert!((prof.weight - 0.5).abs() < 1e-12, "{prof:?}");
        assert!((prof.vpip - 0.4).abs() < 1e-12, "{prof:?}");
        assert!(prof.pfr <= prof.vpip);
        // 再 100 手同频率（共 150 手、VPIP 0.6）→ 权重 0.75，vpip = 0.75·0.6 + 0.25·0.2。
        feed(&mut p, 3, "t", 100, 60);
        let prof = p.profile_for("t", None).unwrap();
        assert!((prof.weight - 0.75).abs() < 1e-12, "{prof:?}");
        assert!((prof.vpip - 0.5).abs() < 1e-12, "{prof:?}");
    }

    #[test]
    fn hero_excluded_from_population() {
        let mut p = Profiler::new(ExploitConfig::default());
        feed(&mut p, 1, "x", 200, 40); // 人群 VPIP 0.2
        feed(&mut p, 3, "t", 50, 30); // 目标 VPIP 0.6
        let want = p.profile_for("t", None).unwrap();
        // hero 每手都入池：扣掉后人群不变，结果与没有 hero 时逐位相同。
        feed(&mut p, 0, "bot", 300, 300);
        assert_eq!(p.profile_for("t", Some("bot")), Some(want));
        assert_ne!(p.profile_for("t", None), Some(want), "不扣 hero 人群会变");
        // hero 自己也能查（扣自己一次，不重复扣）。
        assert!(p.profile_for("bot", Some("bot")).is_some());
    }

    #[test]
    fn vpip_pfr_af_counting() {
        let mut p = Profiler::new(ExploitConfig::default());
        // 一手：seat0 翻前 raise（vpip+pfr），翻后 bet（aggr）再 call（passive）。
        p.observe_hand(&hand(
            &[(0, "h")],
            &[
                (0, "preflop", "raise"),
                (0, "flop", "bet"),
                (0, "turn", "call"),
            ],
        ));
        let prof = p.profile_for("h", None).unwrap();
        assert_eq!(prof.vpip, 1.0);
        assert_eq!(prof.pfr, 1.0);
        assert_eq!(prof.n_postflop, 2);
        assert!((prof.postflop_af - 1.0).abs() < 1e-9, "aggr1/passive1=1.0");
    }

    #[test]
    fn bb_check_is_not_vpip() {
        let mut p = Profiler::new(ExploitConfig::default());
        // BB 免费过牌 → 非主动入池（盲注不入 actions，首动作 check）。
        p.observe_hand(&hand(&[(0, "bb")], &[(0, "preflop", "check")]));
        let prof = p.profile_for("bb", None).unwrap();
        assert_eq!(prof.vpip, 0.0, "BB check 不计 VPIP");
    }
}
