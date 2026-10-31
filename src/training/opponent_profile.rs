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
//! **翻前局面频率**（[`Profiler::spot_freqs`]，对手范围倾斜用）：翻前每个决策按局面归格
//! （[`preflop_spots`]：未开池 / 盲位 option / 面对开池 / 平跟后面对加注 / 面对 3bet，各自再按位置等分格；
//! advisor 判本手决策所在的格也调它，局面分类只有这一份），每格数加注 / 跟注 / 弃牌，查询时逐层收缩。
//! 需要 observe 带 `button_seat` / `dealt_seats`（/ `posts`），旧 driver 不带 → 这部分不统计。
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
    /// 本手 button 座（OpenPoker 座位号）。缺省 = 旧 driver → 不统计未开池频率。
    #[serde(default)]
    pub button_seat: Option<u8>,
    /// 本手发了牌的座位（升序）。缺省 / 空 → 不统计未开池频率。
    #[serde(default)]
    pub dealt_seats: Vec<u8>,
    /// 补 live 大盲的座位（wopo 补盲）。没人加注时他们可以过牌，不算未开池决策。
    #[serde(default)]
    pub posts: Vec<u8>,
}

impl ObserveHand {
    /// 座位信息齐全时的 [`HandSeats`]（旧 driver 不带 → `None`）。
    pub fn seats(&self) -> Option<HandSeats<'_>> {
        Some(HandSeats {
            button: self.button_seat?,
            dealt: (!self.dealt_seats.is_empty()).then_some(self.dealt_seats.as_slice())?,
            posts: &self.posts,
        })
    }
}

/// 一手的座位结构（未开池局面分类用）：button、发牌座（升序）、补盲座。座位号 = OpenPoker 座位号。
#[derive(Debug, Clone, Copy)]
pub struct HandSeats<'a> {
    pub button: u8,
    pub dealt: &'a [u8],
    pub posts: &'a [u8],
}

/// 未开池局面的位置组（按发牌座环序数，空座不占位；与 `opponent_analysis/replay.py` 的位置标签 +
/// `position_class` 一致）。大盲不在其中（没人加注时只能过牌 / 加注）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpenPos {
    /// 大盲后第一位（UTG），以及 CO 往前两位之外（满 8 座 = UTG、UTG+1）。
    Early,
    /// CO 往前一、二位、且不是 UTG（满 8 座 = LJ、HJ；6 座 = HJ）。
    Middle,
    Cutoff,
    Button,
    SmallBlind,
}

/// 位置大类（`opponent_analysis/replay.py` `position_class` 同口径）：EP / MP（LJ、HJ）/ LP（CO、BTN）/
/// 盲位（SB、BB）。补盲者按他坐的位置算。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PosClass {
    Early,
    Middle,
    Late,
    Blind,
}

impl PosClass {
    const N: usize = 4;

    fn index(self) -> usize {
        self as usize
    }
}

/// 未开池频率的一格：位置组 × 前面是否已有人平跟（翻前没人加注时；平跟者在影子里映成弃牌，
/// 两者 blueprint 节点相同，但实测平跟率差得多）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnopenedCell {
    pub pos: OpenPos,
    pub limped: bool,
}

impl UnopenedCell {
    const N: usize = 10;

    fn index(self) -> usize {
        let p = match self.pos {
            OpenPos::Early => 0,
            OpenPos::Middle => 1,
            OpenPos::Cutoff => 2,
            OpenPos::Button => 3,
            OpenPos::SmallBlind => 4,
        };
        p * 2 + usize::from(self.limped)
    }
}

/// 翻前局面格（[`preflop_spots`] 分类，[`Profiler::spot_freqs`] 按格统计）。局面类与
/// `opponent_analysis/spots.py` 的 `rfi` / `vs_limp` / `blind_option` / `vs_open` / `limp_vs_raise` /
/// `vs_3bet` 一一对应（对拍差异见 [`preflop_spots`]）。其余翻前决策（面对 4bet、冷跟 3bet 等）不分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreflopSpot {
    /// 没人加注时非大盲、非补盲座的动作（首入 / 跟在平跟后面）：加注 / 平跟 / 弃牌。
    Unopened(UnopenedCell),
    /// 没人加注、轮到大盲或补盲者（能免费过牌）：加注 / 过牌。`bb` = 大盲（否则补盲者），
    /// `multi` = 前面跟注（含 SB 补齐）≥ 2 人。
    BlindOption { bb: bool, multi: bool },
    /// 本手第一个动作、面对恰好一次加注：3bet / 跟注 / 弃牌。`callers` = 加注后已有人跟注。
    VsOpen { pos: PosClass, callers: bool },
    /// 没人加注时平跟过（补盲者过牌也算），之后第一次面对恰好一次加注：再加注 / 跟注 / 弃牌。
    LimpVsRaise { pos: PosClass },
    /// 开池加注者第一次面对两次加注（3bet）：4bet / 跟注 / 弃牌。
    Vs3bet { pos: PosClass },
}

impl PreflopSpot {
    /// 格总数（[`Self::index`] 的值域）。
    const N: usize = UnopenedCell::N + 4 + PosClass::N * 2 + PosClass::N * 2;
    const OPTION_BASE: usize = UnopenedCell::N;
    const VS_OPEN_BASE: usize = Self::OPTION_BASE + 4;
    const LIMP_VS_RAISE_BASE: usize = Self::VS_OPEN_BASE + PosClass::N * 2;
    const VS_3BET_BASE: usize = Self::LIMP_VS_RAISE_BASE + PosClass::N;

    fn index(self) -> usize {
        match self {
            PreflopSpot::Unopened(c) => c.index(),
            PreflopSpot::BlindOption { bb, multi } => {
                Self::OPTION_BASE + usize::from(bb) * 2 + usize::from(multi)
            }
            PreflopSpot::VsOpen { pos, callers } => {
                Self::VS_OPEN_BASE + pos.index() * 2 + usize::from(callers)
            }
            PreflopSpot::LimpVsRaise { pos } => Self::LIMP_VS_RAISE_BASE + pos.index(),
            PreflopSpot::Vs3bet { pos } => Self::VS_3BET_BASE + pos.index(),
        }
    }

    /// 同一局面类（不分位置 / 前面有没有人跟）的全部格：[`Profiler::spot_freqs`] 第 2 层「本人跨格的
    /// 整体松紧」只在同类格之间比。
    fn kind_cells(self) -> std::ops::Range<usize> {
        match self {
            PreflopSpot::Unopened(_) => 0..Self::OPTION_BASE,
            PreflopSpot::BlindOption { .. } => Self::OPTION_BASE..Self::VS_OPEN_BASE,
            PreflopSpot::VsOpen { .. } => Self::VS_OPEN_BASE..Self::LIMP_VS_RAISE_BASE,
            PreflopSpot::LimpVsRaise { .. } => Self::LIMP_VS_RAISE_BASE..Self::VS_3BET_BASE,
            PreflopSpot::Vs3bet { .. } => Self::VS_3BET_BASE..Self::N,
        }
    }
}

/// 翻前决策的动作类。下标顺序 = [`SpotFreqs::f`] 的顺序。`Call` 含平跟、跟注、补齐和盲位过牌。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotAct {
    Raise = 0,
    Call = 1,
    Fold = 2,
}

/// 一个已分类的翻前决策（[`preflop_spots`]）：`action_idx` = 在输入动作序列里的下标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpotDecision {
    pub action_idx: usize,
    pub seat: u8,
    pub spot: PreflopSpot,
    pub act: SpotAct,
}

/// 一手翻前动作的局面分类（advisor 判本手各决策所在的格与 [`Profiler`] 统计都走这里，口径只有一份）。
/// `actions` = 按时间序的 `(座, 动作类)`，**只给翻前部分**。发牌少于 3 人（两人局盲注约定不同）或
/// 座位信息对不上 → 空。
///
/// 每个动作按动作前的局面归格（[`PreflopSpot`]），归不进任何格的动作不出现在结果里：
/// - 没人加注：大盲 / 补盲者 → `BlindOption`；其余 → `Unopened`（`limped` = 前面有人跟注）。
/// - 恰好一次加注：本手第一个动作 → `VsOpen`；没人加注时跟注 / 过牌过、第一次面对加注 → `LimpVsRaise`。
/// - 恰好两次加注：第一个加注者、第一次面对 → `Vs3bet`。
///
/// 动作类：加注 / 主动全下 → 加注，跟注（含短码全下只够跟注，由调用方按金额判）→ 跟注，弃牌 → 弃牌，
/// 过牌 → 跟注；没人加注时非大盲、非补盲座过牌或面对加注过牌 → 数据有问题，整手返回空，不猜。
///
/// 与 `spots.py` 的已知差异：补盲者没人加注时的加注，`spots.py` 同时记进 `rfi`（这里只记
/// `BlindOption`）；`spots.py` 跳过服务器代打（超时）过的座位，这里没有这个信息、照常分类。
pub fn preflop_spots(seats: HandSeats<'_>, actions: &[(u8, SpotActInput)]) -> Vec<SpotDecision> {
    let dealt = seats.dealt;
    let k = dealt.len();
    if k < 3 || dealt.windows(2).any(|w| w[0] >= w[1]) {
        return Vec::new();
    }
    let Some(btn_idx) = dealt.iter().position(|&s| s == seats.button) else {
        return Vec::new();
    };
    // 环序距离：button 之后第 j 个发牌座（0 = SB，1 = BB，k−1 = button）。
    let ring_pos = |seat: u8| -> Option<usize> {
        let i = dealt.iter().position(|&s| s == seat)?;
        Some((i + k - btn_idx - 1) % k)
    };
    let bb = dealt[(btn_idx + 2) % k];
    // 每座：已行动过 / 没人加注时跟注或过牌过 / 已记过 `LimpVsRaise` / 已记过 `Vs3bet`。
    #[derive(Default, Clone, Copy)]
    struct SeatState {
        acted: bool,
        limped: bool,
        limp_vs_raise: bool,
        vs_3bet: bool,
    }
    let mut st = vec![SeatState::default(); k];
    let mut out = Vec::new();
    let mut raises = 0u32;
    // 上一次加注后的跟注人数（没人加注时 = 平跟人数，含 SB 补齐）。
    let mut callers = 0u32;
    let mut first_raiser: Option<u8> = None;
    let mut aggressor: Option<u8> = None;
    for (action_idx, &(seat, kind)) in actions.iter().enumerate() {
        let Some(j) = ring_pos(seat) else {
            return Vec::new();
        };
        let option_holder = seat == bb || seats.posts.contains(&seat);
        let act = match kind {
            SpotActInput::Aggr => SpotAct::Raise,
            SpotActInput::Call => SpotAct::Call,
            SpotActInput::Fold => SpotAct::Fold,
            SpotActInput::Check if raises == 0 && option_holder => SpotAct::Call,
            SpotActInput::Check => return Vec::new(),
        };
        let class = match j {
            0 | 1 => PosClass::Blind,
            _ if j == k - 1 || j == k - 2 => PosClass::Late,
            // 大盲后第一位恒为 UTG（同 `replay.py` `assign_positions`）。
            _ if j > 2 && j + 4 >= k => PosClass::Middle,
            _ => PosClass::Early,
        };
        let s = &mut st[j];
        let spot = match raises {
            0 if option_holder => Some(PreflopSpot::BlindOption {
                bb: seat == bb,
                multi: callers >= 2,
            }),
            0 => Some(PreflopSpot::Unopened(UnopenedCell {
                pos: match j {
                    0 => OpenPos::SmallBlind,
                    _ if j == k - 1 => OpenPos::Button,
                    _ if j == k - 2 => OpenPos::Cutoff,
                    _ if j > 2 && j + 4 >= k => OpenPos::Middle,
                    _ => OpenPos::Early,
                },
                limped: callers > 0,
            })),
            1 if !s.acted => Some(PreflopSpot::VsOpen {
                pos: class,
                callers: callers > 0,
            }),
            1 if s.limped && !s.limp_vs_raise => {
                s.limp_vs_raise = true;
                Some(PreflopSpot::LimpVsRaise { pos: class })
            }
            2 if first_raiser == Some(seat) && aggressor != Some(seat) && !s.vs_3bet => {
                s.vs_3bet = true;
                Some(PreflopSpot::Vs3bet { pos: class })
            }
            _ => None,
        };
        if let Some(spot) = spot {
            out.push(SpotDecision {
                action_idx,
                seat,
                spot,
                act,
            });
        }
        if raises == 0 && act == SpotAct::Call {
            s.limped = true;
        }
        s.acted = true;
        match (act, kind) {
            (SpotAct::Raise, _) => {
                raises += 1;
                callers = 0;
                aggressor = Some(seat);
                first_raiser.get_or_insert(seat);
            }
            (SpotAct::Call, SpotActInput::Call) => callers += 1,
            _ => {}
        }
    }
    out
}

/// [`preflop_spots`] 的动作输入（调用方各自从原始动作串换过来；全下只够跟注算 `Call`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotActInput {
    Fold,
    Check,
    Call,
    Aggr,
}

/// 收缩后的翻前局面频率（[`Profiler::spot_freqs`]），`f` 按 [`SpotAct`] 下标，和为 1。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpotFreqs {
    pub f: [f64; 3],
    /// 该玩家本格样本数。
    pub n: u32,
    /// 人群（不含该玩家和 hero）本格样本数。
    pub n_pop: u32,
    /// 该玩家同一局面类其余格的样本数（第 2 层「本人整体松紧」用）。
    pub n_kind: u32,
}

/// 人群格往 blueprint 收的伪样本数：人群样本攒得快，几十条就基本由实测决定。
const POP_CELL_SHRINK_K: f64 = 10.0;

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
    /// 翻前局面频率按格收缩的伪样本数（[`Profiler::spot_freqs`]）：玩家本格 → 第 2 层目标，以及
    /// 玩家同类其余格 → 人群（第 2 层的「整体松紧」）。
    pub cell_shrink_k: f64,
    /// 平跟者范围倾斜开关（advisor `--exploit-limp-tilt on`）：脱锚搜索里对手平跟那一步改用「开池节点
    /// 合成的加注 / 平跟 / 弃牌参考策略 × 未开池实测频率倾斜」，代替 blueprint 弃牌概率 + 宽度修正。
    /// `false`（默认）= 旧行为逐位不变。
    pub limp_tilt: bool,
    /// 对手翻前范围倾斜流水线开关（advisor `--exploit-tilt on`）：脱锚搜索里每个对手的全部翻前决策改用
    /// 「参考策略（同步节点 / 替身节点）× 局面格实测频率倾斜」连乘，代替 blueprint 前缀 reach + 宽度修正；
    /// 开了它 `limp_tilt` 不再单独生效（平跟那一步是它的子集）。`false`（默认）= 旧行为逐位不变。
    pub tilt: bool,
}

impl Default for ExploitConfig {
    fn default() -> Self {
        ExploitConfig {
            shrink_k: 50.0,
            strength_alpha: 0.5,
            pfr_shape: false,
            cell_shrink_k: 30.0,
            limp_tilt: false,
            tilt: false,
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

/// [`classify_obs`] 换成 [`preflop_spots`] 的输入类型。
fn spot_input(a: &ObsAction) -> Option<SpotActInput> {
    Some(match classify_obs(a)? {
        ActKind::Fold => SpotActInput::Fold,
        ActKind::Check => SpotActInput::Check,
        ActKind::Call => SpotActInput::Call,
        ActKind::Aggr => SpotActInput::Aggr,
    })
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
    /// 翻前局面决策计数：`[格][加注, 跟注, 弃牌]`（[`PreflopSpot::index`]）。
    spots: [[u32; 3]; PreflopSpot::N],
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
        self.observe_spots(hand);
    }

    /// 翻前局面决策计数（[`preflop_spots`]）。座位信息缺 / 有动作分不了类 → 整手不计。
    fn observe_spots(&mut self, hand: &ObserveHand) {
        let Some(seats) = hand.seats() else {
            return;
        };
        let pre: Option<Vec<(u8, SpotActInput)>> = hand
            .actions
            .iter()
            .take_while(|a| is_preflop(&a.street))
            .map(|a| Some((a.seat, spot_input(a)?)))
            .collect();
        let Some(pre) = pre else {
            return;
        };
        for d in preflop_spots(seats, &pre) {
            let Some(name) = hand.names.get(&d.seat) else {
                continue;
            };
            let (c, a) = (d.spot.index(), d.act as usize);
            self.stats.entry(name.clone()).or_default().spots[c][a] += 1;
            self.total.spots[c][a] += 1;
        }
    }

    /// `name` 在 `spot` 这一格的翻前动作频率（[加注, 跟注, 弃牌]），逐层收缩
    /// （`docs/temp/preflop_range_offanchor_research_2026_10_29.md` §4.3）：
    ///
    /// 1. 人群本格（不含该玩家和 `hero`）按 [`POP_CELL_SHRINK_K`] 条伪样本往 `prior`（blueprint 在该节点的
    ///    边际频率）收，得 `pop_f`；
    /// 2. 本人整体松紧：该玩家同一局面类**其余格**的实测，与「人群在这些格、按他的格分布加权」的期望比，
    ///    比值按 `cell_shrink_k` 往 1 收，乘到 `pop_f` 上再归一（= 对 `pop_f` 做指数倾斜，θ = ln 比值）；
    ///    只用其余格，本格样本不在两层里重复计；
    /// 3. 玩家本格按 `cell_shrink_k` 往第 2 层的结果收。
    ///
    /// 哪一层没有样本，那一层就等于上一层，不需要按数据量挑格子粗细。本人（本格和同类其余格）和人群本格
    /// 都没有样本 → `None`（没有任何实测，调用方不倾斜）。
    pub fn spot_freqs(
        &self,
        name: &str,
        hero: Option<&str>,
        spot: PreflopSpot,
        prior: [f64; 3],
    ) -> Option<SpotFreqs> {
        let empty = [[0u32; 3]; PreflopSpot::N];
        let own = self.stats.get(name).map_or(&empty, |st| &st.spots);
        let mut pop = self.total.spots;
        let mut minus = |x: &[[u32; 3]; PreflopSpot::N]| {
            for (pc, xc) in pop.iter_mut().zip(x) {
                for (p, v) in pc.iter_mut().zip(xc) {
                    *p -= v;
                }
            }
        };
        minus(own);
        if let Some(h) = hero.filter(|&h| h != name) {
            if let Some(hs) = self.stats.get(h) {
                minus(&hs.spots);
            }
        }
        let c = spot.index();
        let others: Vec<usize> = spot.kind_cells().filter(|&x| x != c).collect();
        let total = |x: &[u32; 3]| x.iter().sum::<u32>();
        let (n, n_pop) = (total(&own[c]), total(&pop[c]));
        let n_kind: u32 = others.iter().map(|&x| total(&own[x])).sum();
        if n + n_pop + n_kind == 0 {
            return None;
        }
        let k = self.cfg.cell_shrink_k.max(0.0);
        let pop_f = shrink(pop[c], POP_CELL_SHRINK_K, prior);
        let ratio = own_kind_ratio(own, &pop, &others, k);
        let tilted: [f64; 3] = std::array::from_fn(|a| pop_f[a] * ratio[a]);
        let z: f64 = tilted.iter().sum();
        let target = if z > 0.0 {
            tilted.map(|x| x / z)
        } else {
            pop_f
        };
        let f = shrink(own[c], k, target);
        Some(SpotFreqs {
            f,
            n,
            n_pop,
            n_kind,
        })
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

/// `(计数 + k·目标) / (样本数 + k)`：`k` 条伪样本往 `target` 收。
fn shrink(counts: [u32; 3], k: f64, target: [f64; 3]) -> [f64; 3] {
    let t = f64::from(counts.iter().sum::<u32>());
    std::array::from_fn(|i| (f64::from(counts[i]) + k * target[i]) / (t + k))
}

/// [`Profiler::spot_freqs`] 第 2 层：玩家在 `others`（同一局面类的其余格）上的实测，相对「人群在这些格的
/// 频率按他每格的样本数加权」的期望，每个动作类的比值（间接标准化，消掉他各格样本分布不同的影响）。
/// 实测先按 `k` 条伪样本往期望收，所以样本少时比值接近 1。人群各格频率先按 [`POP_CELL_SHRINK_K`] 往
/// 人群同类合计收（这里没有其余格的 blueprint 边际，只能用人群自己）。玩家其余格没样本 / 人群同类没样本 /
/// 某个动作类期望为 0 → 该类比值 1。
fn own_kind_ratio(
    own: &[[u32; 3]; PreflopSpot::N],
    pop: &[[u32; 3]; PreflopSpot::N],
    others: &[usize],
    k: f64,
) -> [f64; 3] {
    let mut pop_kind = [0u32; 3];
    for &c in others {
        (0..3).for_each(|a| pop_kind[a] += pop[c][a]);
    }
    let pop_kind_n: u32 = pop_kind.iter().sum();
    if pop_kind_n == 0 {
        return [1.0; 3];
    }
    let pop_kind_f = pop_kind.map(|x| f64::from(x) / f64::from(pop_kind_n));
    let (mut obs, mut exp, mut n) = ([0.0f64; 3], [0.0f64; 3], 0u32);
    for &c in others {
        let nc: u32 = own[c].iter().sum();
        if nc == 0 {
            continue;
        }
        let e = shrink(pop[c], POP_CELL_SHRINK_K, pop_kind_f);
        for a in 0..3 {
            obs[a] += f64::from(own[c][a]);
            exp[a] += f64::from(nc) * e[a];
        }
        n += nc;
    }
    if n == 0 {
        return [1.0; 3];
    }
    let n = f64::from(n);
    std::array::from_fn(|a| {
        let e = exp[a] / n;
        if e > 0.0 {
            (obs[a] + k * e) / (n + k) / e
        } else {
            1.0
        }
    })
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
            ..Default::default()
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
            ..Default::default()
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

    use SpotActInput::{Aggr, Call, Check, Fold};

    fn dec(seat: u8, spot: PreflopSpot, act: SpotAct, idx: usize) -> SpotDecision {
        SpotDecision {
            action_idx: idx,
            seat,
            spot,
            act,
        }
    }

    fn unopened(pos: OpenPos, limped: bool) -> PreflopSpot {
        PreflopSpot::Unopened(UnopenedCell { pos, limped })
    }

    /// 8 人满桌（button = 7 → SB 0、BB 1、UTG 2 …）：没人加注时位置组按环序数，平跟后的格标 limped，
    /// 大盲过牌记 option（前面 ≥2 人跟注 = multi）。
    #[test]
    fn spots_unopened_full_ring() {
        let dealt: Vec<u8> = (0..8).collect();
        let seats = HandSeats {
            button: 7,
            dealt: &dealt,
            posts: &[],
        };
        let acts = [
            (2, Fold),
            (3, Call),
            (4, Fold),
            (5, Call),
            (6, Fold),
            (7, Call),
            (0, Call),
            (1, Check),
        ];
        assert_eq!(
            preflop_spots(seats, &acts),
            vec![
                dec(2, unopened(OpenPos::Early, false), SpotAct::Fold, 0),
                dec(3, unopened(OpenPos::Early, false), SpotAct::Call, 1),
                dec(4, unopened(OpenPos::Middle, true), SpotAct::Fold, 2),
                dec(5, unopened(OpenPos::Middle, true), SpotAct::Call, 3),
                dec(6, unopened(OpenPos::Cutoff, true), SpotAct::Fold, 4),
                dec(7, unopened(OpenPos::Button, true), SpotAct::Call, 5),
                dec(0, unopened(OpenPos::SmallBlind, true), SpotAct::Call, 6),
                dec(
                    1,
                    PreflopSpot::BlindOption {
                        bb: true,
                        multi: true
                    },
                    SpotAct::Call,
                    7
                ),
            ]
        );
    }

    /// 面对加注的三类：本手第一个动作面对开池 → `VsOpen`（加注后有人跟 = callers）；平跟过的人面对
    /// 隔离加注 → `LimpVsRaise`；开池者面对 3bet → `Vs3bet`（只记第一次）。冷跟 3bet、面对 4bet 不分类。
    #[test]
    fn spots_facing_raises() {
        let dealt: Vec<u8> = (0..8).collect();
        let seats = HandSeats {
            button: 7,
            dealt: &dealt,
            posts: &[],
        };
        let vs_open = |pos, callers| PreflopSpot::VsOpen { pos, callers };
        // 2 开池、3 弃、4 跟、5 弃、6 3bet、7/0/1 弃（冷面对 3bet，不分类）、2 跟、4 弃（不分类）。
        let acts = [
            (2, Aggr),
            (3, Fold),
            (4, Call),
            (5, Fold),
            (6, Aggr),
            (7, Fold),
            (0, Fold),
            (1, Fold),
            (2, Call),
            (4, Fold),
        ];
        assert_eq!(
            preflop_spots(seats, &acts),
            vec![
                dec(2, unopened(OpenPos::Early, false), SpotAct::Raise, 0),
                dec(3, vs_open(PosClass::Early, false), SpotAct::Fold, 1),
                dec(4, vs_open(PosClass::Middle, false), SpotAct::Call, 2),
                dec(5, vs_open(PosClass::Middle, true), SpotAct::Fold, 3),
                dec(6, vs_open(PosClass::Late, true), SpotAct::Raise, 4),
                dec(
                    2,
                    PreflopSpot::Vs3bet {
                        pos: PosClass::Early
                    },
                    SpotAct::Call,
                    8
                ),
            ]
        );
        // 2 平跟、5 隔离加注、大盲跟（面对开池，前面没人跟）、2 再加注（平跟后面对加注）、5 面对「3bet」。
        let acts = [
            (2, Call),
            (3, Fold),
            (4, Fold),
            (5, Aggr),
            (6, Fold),
            (7, Fold),
            (0, Fold),
            (1, Call),
            (2, Aggr),
            (5, Fold),
        ];
        assert_eq!(
            preflop_spots(seats, &acts)[3..],
            [
                dec(5, unopened(OpenPos::Middle, true), SpotAct::Raise, 3),
                dec(6, vs_open(PosClass::Late, false), SpotAct::Fold, 4),
                dec(7, vs_open(PosClass::Late, false), SpotAct::Fold, 5),
                dec(0, vs_open(PosClass::Blind, false), SpotAct::Fold, 6),
                dec(1, vs_open(PosClass::Blind, false), SpotAct::Call, 7),
                dec(
                    2,
                    PreflopSpot::LimpVsRaise {
                        pos: PosClass::Early
                    },
                    SpotAct::Raise,
                    8
                ),
                dec(
                    5,
                    PreflopSpot::Vs3bet {
                        pos: PosClass::Middle
                    },
                    SpotAct::Fold,
                    9
                ),
            ]
        );
        // SB 补齐、大盲加注、SB 面对 → 平跟后面对加注（盲位）。
        let acts = [
            (2, Fold),
            (3, Fold),
            (4, Fold),
            (5, Fold),
            (6, Fold),
            (7, Fold),
            (0, Call),
            (1, Aggr),
            (0, Call),
        ];
        let got = preflop_spots(seats, &acts);
        assert_eq!(
            got[6..],
            [
                dec(0, unopened(OpenPos::SmallBlind, false), SpotAct::Call, 6),
                dec(
                    1,
                    PreflopSpot::BlindOption {
                        bb: true,
                        multi: false
                    },
                    SpotAct::Raise,
                    7
                ),
                dec(
                    0,
                    PreflopSpot::LimpVsRaise {
                        pos: PosClass::Blind
                    },
                    SpotAct::Call,
                    8
                ),
            ]
        );
        // 面对加注过牌 = 数据有问题 → 整手不记。
        assert!(preflop_spots(seats, &[(2, Aggr), (3, Check)]).is_empty());
    }

    /// 短桌（空座不占位）+ 补盲：补盲座没人加注时的过牌记 option（非大盲）；非盲位过牌 = 数据有问题 →
    /// 整手不记；两人局不分类。
    #[test]
    fn spots_short_table_posts_and_bad_check() {
        let dealt = [1u8, 3, 4, 6, 8];
        let seats = HandSeats {
            button: 8,
            dealt: &dealt,
            posts: &[6],
        };
        // 环序：SB 1、BB 3、4（大盲后第一位 = UTG → Early）、6（CO，补盲）、8（BTN）。
        let acts = [(4, Call), (6, Check), (8, Fold), (1, Fold), (3, Check)];
        let option = |bb| PreflopSpot::BlindOption { bb, multi: false };
        assert_eq!(
            preflop_spots(seats, &acts),
            vec![
                dec(4, unopened(OpenPos::Early, false), SpotAct::Call, 0),
                dec(6, option(false), SpotAct::Call, 1),
                dec(8, unopened(OpenPos::Button, true), SpotAct::Fold, 2),
                dec(1, unopened(OpenPos::SmallBlind, true), SpotAct::Fold, 3),
                dec(3, option(true), SpotAct::Call, 4),
            ]
        );
        // 补盲者过牌后面对加注 = 平跟后面对加注（CO → LP）。
        let acts = [
            (4, Fold),
            (6, Check),
            (8, Aggr),
            (1, Fold),
            (3, Fold),
            (6, Call),
        ];
        assert_eq!(
            preflop_spots(seats, &acts)[5],
            dec(
                6,
                PreflopSpot::LimpVsRaise {
                    pos: PosClass::Late
                },
                SpotAct::Call,
                5
            )
        );
        let no_posts = HandSeats {
            posts: &[],
            ..seats
        };
        assert!(preflop_spots(no_posts, &[(4, Call), (6, Check)]).is_empty());
        let two = [1u8, 3];
        let hu = HandSeats {
            button: 1,
            dealt: &two,
            posts: &[],
        };
        assert!(preflop_spots(hu, &[(1, Call)]).is_empty(), "两人局不分类");
    }

    /// 8 人满桌（button 7）、发牌座全齐的一手翻前。
    fn ring(names: &[(u8, &str)], acts: &[(u8, &str)]) -> ObserveHand {
        let acts: Vec<(u8, &str, &str)> = acts.iter().map(|&(s, a)| (s, "preflop", a)).collect();
        ObserveHand {
            button_seat: Some(7),
            dealt_seats: (0..8).collect(),
            ..hand(names, &acts)
        }
    }

    /// 一手 8 人满桌（button 7）：seat 2 动作 `a2`、seat 3 动作 `a3`，其余弃到大盲。
    fn ring_hand(names: &[(u8, &str)], a2: &str, a3: &str) -> ObserveHand {
        let mut acts = vec![(2, a2), (3, a3)];
        acts.extend([4, 5, 6, 7, 0].map(|s| (s, "fold")));
        ring(names, &acts)
    }

    /// 第 1、3 层：人群本格往 prior 收 10 条伪样本、玩家本格往收过的人群收 `cell_shrink_k` 条；人群扣掉
    /// 目标和 hero；没有任何样本 → None；旧 driver 不带座位 → 不统计。下面两次查询里人群在玩家其余格都
    /// 没有样本，第 2 层比值为 1。
    #[test]
    fn spot_freqs_layered_shrinkage() {
        let mut p = Profiler::new(ExploitConfig {
            cell_shrink_k: 30.0,
            ..Default::default()
        });
        let early = |limped| unopened(OpenPos::Early, limped);
        let prior = [0.2, 0.0, 0.8];
        assert_eq!(p.spot_freqs("t", None, early(false), prior), None);
        // 人群 x：UTG 位 20 手全平跟。目标 t：UTG+1 位、前面有人平跟，10 手全加注。
        for _ in 0..20 {
            p.observe_hand(&ring_hand(&[(2, "x")], "call", "fold"));
        }
        for _ in 0..10 {
            p.observe_hand(&ring_hand(&[(3, "t")], "call", "raise"));
        }
        // 旧 driver（没有座位信息）：不进局面统计。
        p.observe_hand(&hand(&[(2, "x")], &[(2, "preflop", "raise")]));
        let pop = [0.2 * 10.0 / 30.0, 20.0 / 30.0, 0.8 * 10.0 / 30.0];
        let got = p.spot_freqs("t", None, early(false), prior).unwrap();
        assert_eq!((got.n, got.n_pop, got.n_kind), (0, 20, 10));
        for (g, w) in got.f.iter().zip(pop) {
            assert!((g - w).abs() < 1e-12, "{got:?}");
        }
        let got = p.spot_freqs("t", None, early(true), prior).unwrap();
        assert_eq!((got.n, got.n_pop, got.n_kind), (10, 0, 0));
        let want = [(10.0 + 30.0 * 0.2) / 40.0, 0.0, 30.0 * 0.8 / 40.0];
        for (g, w) in got.f.iter().zip(want) {
            assert!((g - w).abs() < 1e-12, "{got:?}");
        }
        assert!((got.f.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        // x 当 hero：人群扣掉 x 后为空，t 在 UTG 格没有样本、同类其余格有 → 仍有结果（= prior）。
        let got = p.spot_freqs("t", Some("x"), early(false), prior).unwrap();
        assert_eq!((got.n, got.n_pop), (0, 0));
        for (g, w) in got.f.iter().zip(prior) {
            assert!((g - w).abs() < 1e-12, "{got:?}");
        }
        // 只在 hero 或目标自己身上有样本时照样只扣一次。
        let own = p.spot_freqs("x", Some("x"), early(false), prior).unwrap();
        assert_eq!((own.n, own.n_pop), (20, 0));
        // 别的局面类互不影响：没人有面对开池的样本 → None。
        let vs_open = PreflopSpot::VsOpen {
            pos: PosClass::Early,
            callers: false,
        };
        assert_eq!(p.spot_freqs("t", None, vs_open, prior), None);
    }

    /// 第 2 层：玩家在 BTN 比人群在 BTN 松（加注 16/20 对 10/20），查他没有样本的 UTG 格时，人群 UTG 频率
    /// 按「他在其余格相对人群的比值」倾斜；比值先按 `cell_shrink_k` 往 1 收。
    #[test]
    fn spot_freqs_own_kind_ratio() {
        let mut p = Profiler::new(ExploitConfig {
            cell_shrink_k: 30.0,
            ..Default::default()
        });
        let folds_to =
            |seat: u8| -> Vec<(u8, &'static str)> { (2..seat).map(|s| (s, "fold")).collect() };
        for i in 0..20 {
            let mut a = folds_to(2);
            a.push((2, if i < 2 { "raise" } else { "fold" }));
            p.observe_hand(&ring(&[(2, "x")], &a));
            let mut a = folds_to(7);
            a.push((7, if i < 10 { "raise" } else { "fold" }));
            p.observe_hand(&ring(&[(7, "y")], &a));
            let mut a = folds_to(7);
            a.push((7, if i < 16 { "raise" } else { "fold" }));
            p.observe_hand(&ring(&[(7, "t")], &a));
        }
        let prior = [0.2, 0.0, 0.8];
        let utg = unopened(OpenPos::Early, false);
        let got = p.spot_freqs("t", None, utg, prior).unwrap();
        assert_eq!((got.n, got.n_pop, got.n_kind), (0, 20, 20));
        let pop_f = [4.0 / 30.0, 0.0, 26.0 / 30.0];
        // 人群 BTN = y 的 10/20 → 期望 [0.5, 0, 0.5]；t 实测 [16, 0, 4] 收 30 条 → [0.62, 0, 0.38]。
        let ratio = [0.62 / 0.5, 1.0, 0.38 / 0.5];
        let t: Vec<f64> = (0..3).map(|a| pop_f[a] * ratio[a]).collect();
        let z: f64 = t.iter().sum();
        for (g, x) in got.f.iter().zip(&t) {
            assert!((g - x / z).abs() < 1e-12, "{got:?}");
        }
        assert!(got.f[0] > pop_f[0], "整体更松 → UTG 加注率高于人群");
    }
}
