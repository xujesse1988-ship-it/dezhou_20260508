//! S5② OpenPoker 常驻 advisor（`docs/temp/openpoker_client_design_2026_06_02.md` §2/§5/§6）。
//!
//! Python WS driver（`tools/openpoker_play.py`）跑网络/token/重连，本进程常驻，启动加载
//! 一个 6-max dense blueprint + 200 桶表一次；之后每个我方决策点收一行 JSON（手牌/board/
//! 座位/盲注/本手 betting 历史）、**无状态重放**该手、查 blueprint、出一行
//! `{action, amount?}`。每决策可重放、可单测，crate 零网络依赖（invariant）。
//!
//! # 与 slumbot_advisor 的关系
//!
//! 复用 [`blueprint_advisor`](poker::training::blueprint_advisor) 的 off-tree 核
//! （`advance_shadow_by_applied` incoming / `outgoing_action` outgoing / `parse_card`），
//! 但泛化到 **6 座**：座位按相对 button rotate 到 solver 的 `default_6max_100bb`（button=座0），
//! OpenPoker 筹码（SB10/BB20/买入2000）按 `scale = solver_BB / op_BB = 5` 换算到 solver 单位。
//!
//! # 鲁棒兜底（live 不能崩 / 不能挂死）
//!
//! 任何重放失败（码深漂移 desync、结构性 gap = 对手 open-limp 进 no-limp 影子、非 6 人桌、
//! 非法历史）→ **不 panic、不静默乱出**，而是从 driver 给的 `valid_actions` 出**安全合法动作**
//! （能 check 就 check、否则 fold —— 紧、不漏筹码），并在 `source` 标 `fallback:<reason>`，
//! driver 落日志统计兜底频率。faithful 路径成功时才由 blueprint 驱动。
//!
//! # 实时搜索模式（缺口②，`realtime_search_openpoker_exec_2026_06_08.md` §1/§3.2）
//!
//! `--search` 开启后，**postflop 命中触发面**（[`should_search`]）的决策点改用真码深子博弈
//! re-solve：driver 多送 `stacks[6]`（各座 hand-start 真栈），[`build_real_auth`] 在**真实
//! per-seat 栈** config 上重放本手 → 注入真实牌（[`GameState::inject_external_cards`]）→
//! [`subgame_search`] 解到终局（`time_budget` 墙钟 anytime / 可选 LCFR；**`--search-deep-menu` →
//! 子树菜单收到单一 {1pot}**，缺口③ §2.1）；outgoing 按**真码深** `auth` 算尺寸（非「100BB 解
//! ÷scale」）。**搜索区解不出来 = check-when-free**（能 check 就 check、
//! 否则 fold；建不了真栈树 / 子博弈 `Err`），**不回落 blueprint**（off-distribution 下 blueprint 解的是
//! 错游戏，§2.3）。`source=search_giveup:*` 与 blueprint `fallback:*` 分桶。
//!
//! **within-round solve 缓存（§6 #2）**：本进程常驻（driver IPC 喂请求），main loop 持有
//! [`SubgameSolveCache`]——同手同街第二次决策按「solve 全部输入」的 key 命中 → 复用已解子博弈、
//! 只重做导航：恢复「每轮恰好一个 solve」（`time_budget` anytime 下逐决策重解会停在不同迭代数 =
//! 同街两决策读不同均衡），mid-round 决策 wall ≈ 0，首决策可放心用满 time_budget。
//!
//! **守恒不变量**：`--search` **未开**（`search=None`）时 `decide` 走原 100BB blueprint 路径、
//! 逐字节等价旧行为（测试 `search_off_byte_equal_blueprint` 钉死）。preflop + 未触发的 postflop
//! 决策即便开了 `--search` 也走 blueprint 路径（与未开等价）。
//!
//! **当前边界（v1，①已收口）**：①~~取 `node_id` / `legal_abs` 仍靠 100BB 影子重放~~——
//! **已收口（2026-06-10，缺口②续）**：影子失同步（off-stack all-in 线：blueprint 树按 100BB 对称
//! 栈建、该线**结构性缺节点**，影子导航再鲁棒也修不了）且命中触发面 → 走**脱影子**搜索
//! （[`decide_search_unanchored`]→[`subgame_search_unanchored`]：触发 / 子树根 / within-round 导航
//! 全来自真栈重放，**range 先验退 uniform**、返回子树自身合法集分布，`source=search:unanchored`）；
//! 影子可用时仍走原锚定路径（blueprint range 先验更好）。②子树下注菜单：默认沿用 blueprint 菜单；
//! **`--search-deep-menu`（缺口③，2026-06-09；2026-06-10 v2 细化 = SPR 自适应）→ 子树菜单按根
//! SPR + 人数选宽（[`deep_menu_for`]：深 {1pot} 单档 / 浅 ≤4×pot 且 ≤3 Active 放宽
//! {0.5,1} 两档）**（深码 /
//! 多人解到终局控树，§2.1），此时 outgoing 也用 {1pot} 抽象算尺寸（脱影子路径同样适用）。
//!
//! # 已知限制（blueprint 路径，`...client_design...` §4）
//!
//! - 码深 ≠ 100BB 且**未开搜索 / 未触发**：solver 树/SPR 都按 100BB 解；real `GameState` 用
//!   `default_6max_100bb`（10000 筹码）近似，driver 靠买入锁 2000 + 栈漂出 [80,125]BB leave/rejoin 兜。
//! - 对手 open-limp：no-limp blueprint 无对应节点 → preflop 走 [`limp_heuristic`] 矩阵
//!   （好牌 iso-raise / 顶级面对加注 call / 其余免费 check 或 fold）；postflop 由脱锚搜索接管。
//! - 短桌手（6 座只发 k<6 家）：driver 送 `dealt_seats`（`table_state.seats[].in_hand` 推断）时
//!   走**幻影座映射**（[`seat_map`]：k 人局映成 6-max 树「UTG 侧前 6−k 位先 fold」的真实节点、
//!   盲注对齐；k=2 按 OpenPoker 实测 HU 约定 button=BB 映 SB/BB→树座 1/2）；占座不可判
//!   （无 table_state）→ 仍兜底。

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use poker::eval::NaiveHandEvaluator;
use poker::training::blueprint_advisor::{advance_shadow_by_applied, outgoing_action, parse_card};
use poker::training::game::{Game, PlayerId};
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::nlhe_betting_tree::{
    deep_menu_for, first_small_6max, first_small_preopen_6max, first_small_preopen_small_6max,
    NodeId,
};
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::openpoker_hh::hist_to_concrete;
use poker::training::opponent_profile::{ExploitConfig, ObserveHand, OpponentProfile, Profiler};
use poker::training::sampling::sample_discrete;
use poker::training::subgame::{
    set_subgame_debug, should_search, subgame_search_cached, subgame_search_prewarm,
    subgame_search_unanchored_cached_cross_exploit, subgame_search_unanchored_prewarm_cross,
    synced_prefix_decisions, ExploitPrior, ExploitShape, PrefixReach, ResolveRoot, SearchTrigger,
    SubgameSearchConfig, SubgameSolveCache,
};
use poker::{
    AbstractAction, Action, BucketTable, Card, ChaCha20Rng, ChipAmount, GameState, HandEvaluator,
    InfoSetId, SeatId, StreetActionAbstraction, TableConfig,
};

const N_SEATS: usize = 6;
const REAL_REPLAY_SEED: u64 = 0x5245_414c_3645_4d58; // "REAL6EMX"
const ABS_REPLAY_SEED: u64 = 0x4142_5336_454d_5800; // "ABS6EMX\0"
/// 搜索 range 先验平滑 λ 的生产默认（`SubgameSearchConfig::range_uniform_mix` 字段 doc）：
/// 对手 reach 估计在薄线上会塌缩成噪声窄 range（2026-06-12 searchon50 实撞：river 对手 range
/// 有效 50 组合、近乎无同花 = 封顶），λ 保底让对手 range 永不缺关键组合（0.25 实测有效组合
/// 50→86.5）= 防尾部错误的便宜保险。**诚实边界（判决 sweep：固定 150k 迭代 ×5 seeds）**：
/// 诊断点位的激进度对 λ 不敏感（对手面对 jam 弃牌率 0.73→0.76 平，对手全 uniform 仍弃 ~0.76；
/// 空气桶 jam 份额 0.44–0.95 全 λ 区间随 seed 乱跳）——该点位 jam 偏好由 hero 自身 reach range
/// 的坚果占比（同花成牌河）+ per-bucket 均衡选择噪声驱动，平滑不承诺改写它；激进度的后续杠杆
/// 见 exec 文档 §3.2（低频动作降信 / 向 blueprint 回拉）。
const DEFAULT_SEARCH_RANGE_UNIFORM_MIX: f64 = 0.25;
/// 脱锚搜索档一前缀 reach 的生产默认（`unanchored_range_design_2026_06_10` §5.1 实测拍板）：
/// 真 live 触发的深码 / 3bet 池 off-tree 点上，uniform 先验会让搜索拿中等牌 stack off 进对手窄
/// 价值 range（88 在 A 高面对 3bettor 的 flop 加注战，uniform 几乎从不弃牌、档一弃 0.63–1.0，
/// 3 seeds 一致、单点 EV 数十 BB）；`range_uniform_mix` 单独治不动（脱锚区先验本就 uniform，λ 只
/// 给地板、档一直接换真前缀 range）。`--search-unanchored-prefix-reach off` 显式关（live A/B 对照
/// 臂 / 回退）。注意 §5.1 仍是 n=1 决策级证据 → live 多手 EV 确认仍在进行（开默认是据机制 + 该证据
/// 拍板，正确性早单测硬证、是守护默认关不会变坏的旗）。
const DEFAULT_SEARCH_UNANCHORED_PREFIX_REACH: bool = true;
/// **档二′-跨街复用**的生产默认（`unanchored_range_design_2026_06_10` §1末/§4 +
/// `turn_blueprint_trim_cross_street_anchored_2026_06_19`）：上一街子树已解时，复用那棵子树的 σ 对上一街
/// 实际动作线做贝叶斯条件化 → 本街后验 range，替代断点前粗前缀（档一在 turn 丢掉 flop 断点后的加注战 =
/// §5.1 刚堵的洞下一街又开）。**flag 现同管锚定 + 脱锚两路**（turn_blueprint_trim §2.4）：锚定 river 复用
/// turn 子树后验覆盖 blueprint `estimate_range`，脱锚 river 覆盖档一前缀 reach，跨 kind 亦复用——目的是
/// 让 turn blueprint 在**所有** river 决策上都不被读（裁剪前提）。**默认开**（与档一
/// [`DEFAULT_SEARCH_UNANCHORED_PREFIX_REACH`] 同等证据等级拍板）：决策级 A/B（`six_max_cross_street_ab`，
/// 构造 deep off-tree flop→turn 加注战线、真 blueprint + 真桶、受控配对差仅 root range 不同）实测 ON 显著
/// 且按 §动机方向改 turn 决策——mean TV 0.27–0.41 / argmax 翻转 29–61% / ~100% 触发，对菜单粒度 / seed /
/// 迭代数（4000 iter 不洗掉）皆稳；方向 = ON 把档一的近乎 auto-jam 收成随牌力分化的极化策略（中等/摊牌牌
/// 从梭哈拉回、纯空气构造极化诈唬）。EV「更好」结构性不可得（自对弈触发率~0、live 功效太弱，§4 已钉），
/// 故据「强机制 + 决策级方向一致」开默认（同档一）。`--search-unanchored-cross-street off` 显式关（两路都退，
/// live A/B 对照臂 / 回退）；正确性由 search=None / cross_street=None byte-equal 单测 + 缓存 key 隔离 +
/// prev-slot 一致性硬证（默认开不会变坏的旗）。
const DEFAULT_SEARCH_UNANCHORED_CROSS_STREET: bool = true;
/// flop 街「优先 blueprint」的生产默认（`--search-flop-prefer-blueprint`）：**默认关**
/// （`false` = 保持旧行为 byte-equal——flop 锚定面命中触发就实时搜索）。`on` = 仅 flop 街改成：
/// **脱影子（unanchored）才实时搜索、锚定（lockstep Ok / 100BB 影子同步）仍走 blueprint**。
/// 动机：flop 锚定面 100BB 影子可信、blueprint 训练充分，实时搜索净收益不确定 + 吃建树/求解墙钟；
/// 脱影子 flop（off-stack all-in / 结构性缺节点）blueprint 树本就缺该节点 → 仍须搜索。turn/river
/// 不受影响（锚定面照常按 trigger 搜索）。注意脱影子路径（[`decide_search_unanchored`]）不经
/// `should_search`/此旗 → flop unanchored 恒搜索（旧行为），此旗只抑制 **flop 锚定** 搜索。
const DEFAULT_SEARCH_FLOP_PREFER_BLUEPRINT: bool = false;
/// 搜索墙钟告警阈的建树余量（ms）：最坏单线程建树 ≈ 4-way@20× 宽档 ~2.7s，留 3s。
const SEARCH_WALL_BUILD_MARGIN_MS: u128 = 3000;
/// 无 `time_budget`（固定迭代）时的告警阈回落（ms）。
const SEARCH_WALL_SLOW_FALLBACK_MS: u128 = 8000;
/// 单决策搜索墙钟告警阈：`time_budget + 建树余量`——即「比满预算 solve + 最坏建树**还**慢」才
/// 算异常（树超闸 / 近 cap 大树 / 机器争用），**不把每个满预算 cache-MISS 都误报 SLOW**（这正是
/// 8s 预算下硬编码 8000 会犯的错）。`grep SLOW` 定位真正慢的手。监控用、不改行为。
fn search_wall_slow_ms(cfg: &SubgameSearchConfig) -> u128 {
    cfg.time_budget
        .map(|d| d.as_millis() + SEARCH_WALL_BUILD_MARGIN_MS)
        .unwrap_or(SEARCH_WALL_SLOW_FALLBACK_MS)
}

// ===========================================================================
// 调试日志（--debug-log）：进程级开关，**仅 eprintln 到 stderr**——决策走的是 stdout，故 debug
// 输出绝不污染 IPC，挂/不挂 --debug-log 发往 driver 的字节逐字节一致（off=默认零开销零输出）。
// [`run`] 启动期一次性设置 DEBUG + 透传 [`set_subgame_debug`]（range / solve 中间数据走子博弈层）。
// ===========================================================================
static DEBUG: AtomicBool = AtomicBool::new(false);

fn debug_on() -> bool {
    DEBUG.load(std::sync::atomic::Ordering::Relaxed)
}

/// 决策流水线调试一行（仅 DEBUG 开时；args 惰性求值——关时只付一次原子读）。
macro_rules! dlog {
    ($($arg:tt)*) => {
        if debug_on() {
            eprintln!("[dbg-advisor] {}", format_args!($($arg)*));
        }
    };
}

// ===========================================================================
// driver ↔ advisor JSON 协议（§2）
// ===========================================================================

/// 一条本手历史动作（driver 累计 player_action 还原；§3）。`to` = 该座本街累计到额
/// （**OpenPoker 单位**），仅 raise/bet 需要；fold/check/call/all_in 不需要（call 的额
/// 由规则引擎推导、all_in 由引擎归一）。
#[derive(Deserialize, Debug, Clone)]
struct HistAction {
    seat: u8,
    action: String,
    #[serde(default)]
    to: Option<u64>,
}

/// 我方决策点 OpenPoker 合法区间（your_turn 的 valid_actions，**OpenPoker 单位**）。
/// outgoing 夹进 [min_raise, max_raise] + 兜底从此出安全动作。
#[derive(Deserialize, Debug, Clone)]
struct ValidActions {
    #[serde(default)]
    can_check: bool,
    #[serde(default)]
    can_call: bool,
    #[serde(default)]
    can_raise: bool,
    #[serde(default)]
    min_raise: Option<u64>,
    #[serde(default)]
    max_raise: Option<u64>,
}

#[derive(Deserialize, Debug, Clone)]
struct Request {
    hole: Vec<String>,
    #[serde(default)]
    board: Vec<String>,
    button_seat: u8,
    my_seat: u8,
    num_seats: u8,
    small_blind: u64,
    big_blind: u64,
    #[serde(default)]
    actions: Vec<HistAction>,
    valid: ValidActions,
    /// 缺口②：各座 **hand-start 真栈**（OpenPoker 单位，下标 = OpenPoker 座位号；driver 从
    /// `your_turn.players[].stack` + 累计本手投入还原）。**仅实时搜索读**——`--search` 开且命中
    /// 触发面时，[`build_real_auth`] 据它建真码深 `GameState`。缺省（空 = 旧 driver / 无 players
    /// 字段）→ 退对称 100BB（blueprint 路径不读它，byte-equal 不受影响）。短桌手非发牌座的
    /// 条目是 driver 的 placeholder，按 `dealt_seats` 忽略（幻影座保持 solver 默认 100BB）。
    #[serde(default)]
    stacks: Vec<u64>,
    /// 短桌幻影座映射（exec 文档「短桌 seat_mismatch ~2.4% 兜底」修复）：本手**实际发牌**的
    /// OpenPoker 座位（升序）。缺省 / 全 6 座 = 满桌（旧行为 byte-equal）。k∈[2,5] →
    /// [`seat_map`] 把 k 人局映成 6-max 树「UTG 侧前 6−k 位先 fold」的真实节点（盲注对齐：
    /// 真实 BTN/SB/BB → 树座 0/1/2；k=2 按 OpenPoker 实测 HU 约定 button=BB → SB/BB 映树座
    /// 1/2、幻影 [3,4,5,0]，**仅 preflop**——postflop 行动序角色反转映不进、显式兜底）。
    /// driver 从 `table_state.seats[].in_hand` ∪ 已行动座推断，仅本手收到过 table_state 时
    /// 才发（决策时占座唯一可判才启用）。
    #[serde(default)]
    dealt_seats: Vec<u8>,
    /// 叠加剥削（`exploit_strategy_design_2026_06_14` Tier 2）：本手 OpenPoker 座 → 玩家名。仅
    /// driver `--exploit` 开时附带（缺省 = 空 = 旧行为，blueprint/search 路径不读 → byte-equal）。
    /// 脱锚搜索据它把对手座解析成 [`Profiler`] 画像 → 翻前 range 宽度先验。
    #[serde(default)]
    names: BTreeMap<u8, String>,
}

/// 请求信封判别（与 [`Request`] 分开解析；serde 忽略未知字段 → 同一行 JSON 两个结构都能读，
/// `Request` 不加字段、既有构造/测试零改动）：`prewarm = true` → RoundStart 预热请求
/// （driver `--search-prewarm` 在街起点、hero 行动**前**发，[`prewarm`]——不出动作、只暖
/// solve 缓存，响应仅遥测、driver 丢弃）。缺省 `false` = 决策请求（旧 driver byte-equal）。
#[derive(Deserialize)]
struct RequestEnvelope {
    #[serde(default)]
    prewarm: bool,
    /// `observe = true` → driver 每手结束发来的完整手历史（[`ObserveHand`]）；advisor 喂进
    /// [`Profiler`]、返回遥测（driver 丢弃）。仅 `--exploit` 开时 driver 才发。缺省 `false`
    /// = 决策 / prewarm 请求（旧 driver byte-equal）。
    #[serde(default)]
    observe: bool,
}

/// advisor → driver 一行响应。`amount` 仅 raise 携带（= OpenPoker 单位的 raise-to 额）。
/// `source` = `blueprint` 或 `fallback:<reason>`（driver 统计兜底频率）。
#[derive(Serialize, Debug, Default, Clone, PartialEq)]
struct Response {
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    amount: Option<u64>,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    street: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    info_set: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chosen: Option<String>,
    /// 决策点完整策略分布 `[(动作, 概率)]`（正概率支撑，label 同 `chosen`；`chosen` 即从中采样）。
    /// 仅合法动作路径填（blueprint / search / search:unanchored）；fallback / giveup 是强制动作、
    /// 无分布不填。为 c_act（AIVAT v2）与人工复盘留的原始数据。概率经 [`probs_log`] 四舍五入到
    /// 4 位小数（日志可读性；<0.00005 的尾巴显示为 0.0，和可偏 1 至多 ±n×5e-5）。
    #[serde(skip_serializing_if = "Option::is_none")]
    probs: Option<Vec<(String, f64)>>,
    /// 实时搜索 solve 的更新数（[`SubgameSolveCache::entry_update_count`]；`time_budget`
    /// anytime 下 = 预算内实际完成的 ES-MCCFR update 数）。仅 search / search:unanchored
    /// 决策填；缓存命中 = 被复用 solve 的原始计数。blueprint / fallback 路径恒 `None`
    /// （serde skip → 旧输出 byte-equal）。
    #[serde(skip_serializing_if = "Option::is_none")]
    solve_updates: Option<u64>,
    /// 叠加剥削遥测（`exploit_strategy_design_2026_06_14`）：本决策实际剥削的 `[(solver_seat, vpip)]`
    /// （已收敛对手座）。仅 search:unanchored 且 `--exploit` 开、有收敛对手时填；否则 `None`
    /// （serde skip → 旧输出 byte-equal）。离线分析「哪些手剥削了谁」用，不影响决策。
    #[serde(skip_serializing_if = "Option::is_none")]
    exploit: Option<Vec<(u8, f64)>>,
}

/// 策略分布 → 日志格式：label 同 `chosen`，概率四舍五入 4 位小数（采样仍用全精度 `dist`，
/// 只影响落盘显示）。
fn probs_log(dist: &[(AbstractAction, f64)]) -> Vec<(String, f64)> {
    dist.iter()
        .map(|(a, p)| (action_label(a), (p * 1e4).round() / 1e4))
        .collect()
}

// ===========================================================================
// 决策（重放 → 查策略 → outgoing；失败兜底）
// ===========================================================================

/// 安全兜底动作：能 check 就 check、否则 fold（紧、不漏筹码），用 `valid` 判合法性。
fn safe_fallback(valid: &ValidActions, reason: &str) -> Response {
    let action = if valid.can_check { "check" } else { "fold" };
    Response {
        action: action.to_string(),
        amount: None,
        source: format!("fallback:{reason}"),
        ..Default::default()
    }
}

fn street_label(s: poker::Street) -> &'static str {
    match s {
        poker::Street::Preflop => "preflop",
        poker::Street::Flop => "flop",
        poker::Street::Turn => "turn",
        poker::Street::River => "river",
        poker::Street::Showdown => "showdown",
    }
}

/// 每次搜索决策的监控行（stderr）：solve 实际 update 数 / 街 / 本决策墙钟（build+solve；预热
/// 命中时 ≈0）/ 本决策是否命中缓存（HIT=复用预热/同街 solve；MISS=现 build+solve）/ 累计命中。
/// `wall_ms > slow_ms`（[`search_wall_slow_ms`]）追加 ` SLOW`（长跑监控 `grep SLOW`）。纯日志。
#[allow(clippy::too_many_arguments)]
fn log_search_wall(
    tag: &str,
    street: poker::Street,
    updates: u64,
    wall_ms: u128,
    slow_ms: u128,
    hit: bool,
    hits: u64,
    misses: u64,
) {
    eprintln!(
        "[openpoker_advisor] search{} solve updates={} street={} wall_ms={} cache={} ({}hit/{}miss){}",
        tag,
        updates,
        street_label(street),
        wall_ms,
        if hit { "HIT" } else { "MISS" },
        hits,
        misses,
        if wall_ms > slow_ms { " SLOW" } else { "" },
    );
}

fn expected_board_len(s: poker::Street) -> usize {
    match s {
        poker::Street::Preflop => 0,
        poker::Street::Flop => 3,
        poker::Street::Turn => 4,
        poker::Street::River | poker::Street::Showdown => 5,
    }
}

/// OpenPoker 座 → solver 树座的映射（满桌 = 旧 rotate 公式；短桌 = 幻影座映射）。
struct SeatMap {
    /// 下标 = OpenPoker 座位号；`None` = 本手未发牌（空座 / 等下一手的玩家）。
    to_tree: [Option<u8>; N_SEATS],
    /// 重放序列开头按序先 fold 的幻影树座（preflop 行动序）。k∈[3,5] = `3..3+(6−k)`
    /// （UTG 起最早行动位，恰好最先轮到）；k=2 = `[3,4,5,0]`（BTN 位也是幻影）。
    phantoms: Vec<u8>,
    /// k==2：HU 映射只对 preflop 成立（caller 在 postflop 显式兜底，见 [`seat_map`] doc）。
    hu: bool,
}

impl SeatMap {
    fn tree_seat(&self, op_seat: u8) -> Result<u8, String> {
        self.to_tree
            .get(op_seat as usize)
            .copied()
            .flatten()
            .ok_or_else(|| "actor_not_dealt".to_string())
    }
}

/// 建座位映射。`dealt_seats` 缺省 / 全 6 座 → 满桌恒等（旧 `(op + 6 − button) % 6` 公式，
/// byte-equal）。k∈[3,5] → **短桌幻影座映射**：真实 BTN/SB/BB → 树座 0/1/2（6-max 树的
/// SB/BB 固定在 button+1/+2 且必须发盲，树上不存在「盲注位发盲前 fold」节点，所以不能在
/// 空座原位插 fold、必须重映环序对齐盲注），其余真实玩家按环序占 CO 侧靠后位置
/// （ring 序 j≥3 → 树座 j+6−k），幻影座占 UTG 起最早行动位、开局先 fold——k 人桌首个
/// 行动者 ≡ 6-max 前 6−k 位弃牌后的同位置（标准短桌位置等价，blueprint 真实训练过的节点；
/// preflop 3,4,5,0,1,2 与 postflop 从 SB 顺时针两序在 k=3/4/5 下都与真实短桌严格吻合）。
///
/// k=2：OpenPoker 的 HU 盲注按环规则贴（**live 校准 2026-06-11 两轮 smoke**：button 发
/// BB、非 button 发 SB——非标准 HU），但**行动序是角色序**（preflop SB 先、postflop BB
/// 先 = 标准 HU 的角色顺序）。树是环序（fold 到 SB-vs-BB 后两条街都 SB 先），跨街反转
/// 表达不了 → **只映 preflop**（真实 SB → 树座 1、真实 BB(button) → 树座 2、幻影
/// `[3,4,5,0]` 先 fold，preflop 行动序严格吻合）；postflop 由 caller 显式兜底
/// `short_hu_postflop`（实测顺序错位必被重放 seat 校验拦下、0 漏网，门只是把原因标清楚）。
fn seat_map(req: &Request) -> Result<SeatMap, String> {
    let full = || {
        let mut m = [None; N_SEATS];
        for (op, slot) in m.iter_mut().enumerate() {
            *slot = Some(((op + N_SEATS - req.button_seat as usize) % N_SEATS) as u8);
        }
        SeatMap {
            to_tree: m,
            phantoms: Vec::new(),
            hu: false,
        }
    };
    if req.dealt_seats.is_empty() {
        return Ok(full());
    }
    let dealt = &req.dealt_seats;
    let k = dealt.len();
    if !(2..=N_SEATS).contains(&k)
        || dealt.windows(2).any(|w| w[0] >= w[1])
        || dealt.iter().any(|&s| s as usize >= N_SEATS)
    {
        return Err("bad_dealt_seats".into());
    }
    if !dealt.contains(&req.button_seat) || !dealt.contains(&req.my_seat) {
        return Err("dealt_missing_btn_or_me".into());
    }
    if k == N_SEATS {
        return Ok(full());
    }
    let btn_idx = dealt
        .iter()
        .position(|&s| s == req.button_seat)
        .expect("上面 contains 已校验");
    let mut m = [None; N_SEATS];
    if k == 2 {
        m[req.button_seat as usize] = Some(2); // OpenPoker HU：button 发 BB → 树座 2。
        m[dealt[1 - btn_idx] as usize] = Some(1); // 非 button 发 SB 先动 → 树座 1。
        return Ok(SeatMap {
            to_tree: m,
            phantoms: vec![3, 4, 5, 0],
            hu: true,
        });
    }
    for j in 0..k {
        let op = dealt[(btn_idx + j) % k] as usize;
        let tree = if j < 3 { j } else { j + (N_SEATS - k) };
        m[op] = Some(tree as u8);
    }
    Ok(SeatMap {
        to_tree: m,
        phantoms: (3..3 + (N_SEATS - k) as u8).collect(),
        hu: false,
    })
}

/// 实时搜索运行时 = 触发/求解配置 + 可选**子树独立桶表**。`SubgameSearchConfig` 是 `Copy` 纯
/// 配置；桶表是资源（`Arc`），分开放避免把 Arc 塞进到处按值带的 config。
struct SearchRuntime {
    cfg: SubgameSearchConfig,
    /// `--search-bucket-table`：子树 solve + hero 读数用的独立桶表（如 500/500/500，比
    /// blueprint 200 更细——子树是独立一次性求解、桶空间与 blueprint checkpoint 解耦，见
    /// [`subgame_search_cached`] `bucket_override` doc）。`None` = 沿用 blueprint 表（旧行为
    /// byte-equal）。blueprint 路径 / range 估计永远用 blueprint 表，不受此影响。
    bucket_table: Option<Arc<BucketTable>>,
    /// `--search-unanchored-prefix-reach`（脱锚搜索档一，`unanchored_range_design` §1/§5.1）：`true`
    /// （**生产默认**，[`DEFAULT_SEARCH_UNANCHORED_PREFIX_REACH`]）= 脱影子搜索用**已同步前缀**估
    /// per-seat range 替代 uniform（跳过 AllIn-tag）；`off` 显式关 = uniform（live A/B 对照臂 / 回退）。
    /// §5.1 实测：真 live off-tree 深码 / 3bet 池上 uniform 先验致 stack-off 漏洞、档一改正确弃牌。
    /// range 估计用 blueprint σ / blueprint 表（同 `bucket_table` 注解：换表不影响 range 估计）。
    unanchored_prefix_reach: bool,
    /// `--search-unanchored-cross-street`（**档二′-跨街复用**，`unanchored_range_design` §1末/§4 +
    /// `turn_blueprint_trim_cross_street_anchored_2026_06_19`）：`on` = 上一街子树已解（within-round
    /// 缓存当前条目）时，复用其 σ 对上一街实际动作线条件化得本街后验 range，覆盖断点前粗前缀（§动机：
    /// 档一在 turn 丢掉 flop 断点后的加注战）。**flag 名保留但语义已扩**（turn_blueprint_trim §2.4 改动
    /// D）：现**同时管锚定 + 脱锚两路**——锚定 river 复用 turn 子树后验（覆盖 [`subgame_search_cached`]
    /// 的 blueprint `estimate_range`）、脱锚 river 复用（覆盖档一前缀 reach）；改动 A 后跨 kind 也复用
    /// （turn anchored → river unanchored 等）。目的 = 让 turn blueprint 在**所有** river 决策上都不被
    /// 读 → blueprint 可裁到 preflop+flop。**默认开**（[`DEFAULT_SEARCH_UNANCHORED_CROSS_STREET`]，决策
    /// 级 A/B + 机制拍板，同档一）；`off` 显式关 = 两路都退（A/B 对照臂 / 回退）。
    /// postflop turn/river 触发（锚定 [`decide`]/[`prewarm`] + 脱锚 [`decide_search_unanchored`]/脱锚预热）。
    unanchored_cross_street: bool,
    /// `--search-flop-prefer-blueprint`（[`DEFAULT_SEARCH_FLOP_PREFER_BLUEPRINT`]）：`on` = 仅 flop
    /// 街，锚定面（lockstep Ok / 100BB 影子同步）即使命中 trigger 也走 blueprint、不实时搜索；脱影子
    /// flop（lockstep 失同步）照常实时搜索（[`decide_search_unanchored`] 不经此旗）。**默认关**
    /// （`false`，旧行为 byte-equal）。turn/river 锚定面不受影响。预热（[`prewarm`]）同源 skip 锚定
    /// flop 子树（不 build+solve），否则 advisor 单线程下 hero 决策白等一个 time_budget 才拿 blueprint。
    flop_prefer_blueprint: bool,
    /// 叠加剥削（`--exploit`，`exploit_strategy_design_2026_06_14` Tier 2）：`Some` = 进程内对手画像
    /// 累积器（[`Profiler`]，**只用本进程数据、不依赖过往、启动后空表开始统计**）。`RefCell` 内部
    /// 可变：observe 路径经共享 `&SearchRuntime` 用 `borrow_mut` 累积、决策路径用 `borrow` 读已收敛
    /// 画像（单线程 advisor 循环；solve 并行不碰它，只吃决策时算好的 owned 画像快照）。`None`（默认 /
    /// `--exploit` 关）= 不剥削，[`decide_search_unanchored`] 退既有 search:unanchored（byte-equal）。
    exploit: Option<RefCell<Profiler>>,
}

/// RoundStart 预热（`Request.prewarm = true`，driver `--search-prewarm` 在街起点、hero 行动
/// **前**发）：把该街的 build+solve 提前算进 solve 缓存（[`subgame_search_prewarm`] doc——
/// RoundStart 下 solve 全部输入在街开始即已知），hero 首决策 key 命中 → 只做导航/读数，
/// build+solve wall 藏进对手行动时间。
///
/// **失败无害 / 永不出动作**：任何前置不满足 → `prewarm:skip:*`；建树/求解失败 →
/// `prewarm:err:*`——都只是放弃预热，hero 决策时 miss 现解（key 覆盖 solve 全部输入，
/// 错配不可能读错均衡）。响应仅遥测（`action="none"`，driver 丢弃），且必有一行（IPC 锁步）。
///
/// 路径分类与 [`decide`] 同源（[`lockstep_replay`]）：重放 Ok → 锚定（node_id = 街起点影子
/// 节点，hero 座位显式传入——range 平滑「不混」座按 hero 算，见 `subgame_search_prewarm`）；
/// 重放 Err（off-stack / limp 线失同步）→ 脱影子预热。gating 在街起点判（FlopFirstUnraised
/// 街起点恒未起注；街内后续起注与否未知 → 可能白预热，无害）。
fn prewarm(
    game: &SimplifiedNlheGame,
    strategy_fn: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    req: &Request,
    base_seed: u64,
    search: Option<&SearchRuntime>,
    solve_cache: &mut SubgameSolveCache,
) -> Response {
    let mk = |source: String| Response {
        action: "none".into(),
        source,
        ..Default::default()
    };
    let Some(rt) = search else {
        return mk("prewarm:skip:search_off".into());
    };
    let scfg = &rt.cfg;
    if scfg.resolve_root != ResolveRoot::RoundStart {
        return mk("prewarm:skip:not_round_start".into());
    }
    // —— 前置校验（同 decide 的口径；失败 = skip 不预热）——
    if req.hole.len() != 2 {
        return mk("prewarm:skip:hole_not_2".into());
    }
    if req.num_seats as usize != N_SEATS {
        return mk("prewarm:skip:not_6max".into());
    }
    if req.big_blind == 0 {
        return mk("prewarm:skip:bad_bb".into());
    }
    let solver_cfg = TableConfig::default_6max_100bb();
    let solver_bb = solver_cfg.big_blind.as_u64();
    if solver_bb % req.big_blind != 0 {
        return mk("prewarm:skip:scale_not_integer".into());
    }
    let scale = solver_bb / req.big_blind;
    if req.small_blind * scale != solver_cfg.small_blind.as_u64() {
        return mk("prewarm:skip:blind_ratio_mismatch".into());
    }
    let hole = match (parse_card(&req.hole[0]), parse_card(&req.hole[1])) {
        (Ok(a), Ok(b)) => [a, b],
        _ => return mk("prewarm:skip:bad_hole".into()),
    };
    let board: Vec<Card> = match req.board.iter().map(|s| parse_card(s)).collect() {
        Ok(b) => b,
        Err(_) => return mk("prewarm:skip:bad_board".into()),
    };
    if board.len() < 3 {
        return mk("prewarm:skip:preflop".into()); // preflop 不搜（§1 gating），无预热对象。
    }
    let smap = match seat_map(req) {
        Ok(m) => m,
        Err(reason) => return mk(format!("prewarm:skip:{reason}")),
    };
    if smap.hu {
        return mk("prewarm:skip:short_hu_postflop".into()); // HU 映射仅 preflop（seat_map doc）。
    }
    let my_seat_solver = match smap.tree_seat(req.my_seat) {
        Ok(t) => SeatId(t),
        Err(reason) => return mk(format!("prewarm:skip:{reason}")),
    };
    // 真栈轮起点快照（require_my_turn=false：预热正是在 hero 行动前）。`prev_within` = 紧前一街
    // 完整动作线（档二′-跨街复用，仅脱影子预热分支读）。
    let (_auth, round_start, _within, prev_within) = match build_real_auth(
        req,
        &solver_cfg,
        scale,
        &smap,
        my_seat_solver,
        hole,
        &board,
        false,
    ) {
        Ok(t) => t,
        Err(reason) => return mk(format!("prewarm:err:build:{reason}")),
    };
    // gating：街起点视角（真栈快照；should_search 只读街 + 本街是否已起注——街起点恒未起注，
    // FlopFirstUnraised 即「仅 flop 预热」）。街内后续是否起注未知 → 可能白预热，无害。
    if !should_search(&round_start, scfg.trigger) {
        return mk("prewarm:skip:not_triggered".into());
    }
    if expected_board_len(round_start.street()) != board.len() {
        return mk("prewarm:skip:street_board_mismatch".into());
    }
    // hero 须还 Active（弃牌 / all-in = 本街不会再有 hero 决策，预热纯浪费）。
    let hero_pid = my_seat_solver.0 as PlayerId;
    if round_start.players()[hero_pid as usize].status != poker::PlayerStatus::Active {
        return mk("prewarm:skip:hero_not_active".into());
    }
    let hand_seed = hand_seed_for(req, base_seed);
    // 监控：预热 build+solve 墙钟（= 藏进对手思考时间的成本；> 对手用时则 hero 决策仍会 MISS 现解）。
    let t0 = Instant::now();
    // —— 路径分类与 decide 同源：lockstep Ok → 锚定；Err（off-stack / limp 线）→ 脱影子 ——
    // 路径分类（锚定/脱影子）一次算清；下面 flop_prefer_blueprint skip 与 build+solve 共用同一份
    // 重放结果（重算两次会无谓重放历史）。
    let lockstep = lockstep_replay(game, &solver_cfg, &smap, req, scale, board.len(), None);
    // `--search-flop-prefer-blueprint` on：flop **锚定**面（此处 lockstep Ok）的决策走 blueprint、不搜
    // （decide `want_search` gating 同款，[`SearchRuntime::flop_prefer_blueprint`] doc）。预热该锚定 flop
    // 子树纯属浪费，且因 advisor 单线程串行处理（build+solve 排在 hero decide 请求前），会让 hero 决策
    // 白等一个 time_budget 才拿到 blueprint。故锚定 flop 直接 skip，hero 决策即时返回。脱影子 flop
    // （lockstep Err：off-stack all-in / limp 线）decide 照常搜索、不受此旗影响 → 不在此 skip，照常预热。
    if rt.flop_prefer_blueprint && round_start.street() == poker::Street::Flop && lockstep.is_ok() {
        return mk("prewarm:skip:flop_prefer_blueprint".into());
    }
    let result = match lockstep {
        Ok((_real, abs)) => {
            // 档二′-跨街复用（锚定预热，turn_blueprint_trim §2.4 改动 D）：与决策路径同义，复用缓存里
            // 上一街已解子树 σ 算本街后验 range。预热须与决策时算出同一份 ranges 才命中 key——两路同读
            // `cache.current()`（街起点预热时仍持上一街解）+ 同 `prev_within`。off / 非紧前街 → 自验退 estimate。
            let cross_street = rt.unanchored_cross_street.then_some(prev_within.as_slice());
            subgame_search_prewarm(
                solve_cache,
                hero_pid,
                &round_start,
                game,
                abs.current_node_id,
                strategy_fn,
                scfg,
                rt.bucket_table.as_ref(),
                cross_street,
                hand_seed,
            )
        }
        Err(LockstepErr { synced_node, .. }) => {
            // 档一前缀 reach（生产默认开）：用已同步前缀估 range——须与决策时算出同一份 ranges 才能
            // 命中 key（hero 显式传入：range 平滑「不混」座按 hero 算，见 subgame_search_unanchored_prewarm）。
            let prefix_decisions = rt
                .unanchored_prefix_reach
                .then(|| synced_prefix_decisions(game, synced_node));
            let prefix_reach = prefix_decisions.as_ref().map(|d| PrefixReach {
                strategy: strategy_fn,
                decisions: d,
            });
            // 档二′-跨街复用（默认关）：复用缓存里上一街 unanchored 解的 σ 算本街后验 range。预热须
            // 与决策时算出同一份 ranges 才命中 key——两路同读 `cache.current()`（街起点预热时仍持上一
            // 街解）+ 同 `prev_within` → 同 ranges。off / 缓存非紧前街 → 自验退档一（不破现状）。
            let cross_street = rt.unanchored_cross_street.then_some(prev_within.as_slice());
            subgame_search_unanchored_prewarm_cross(
                solve_cache,
                hero_pid,
                &round_start,
                game,
                prefix_reach,
                cross_street,
                scfg,
                rt.bucket_table.as_ref(),
                hand_seed,
            )
        }
    };
    match result {
        Ok(()) => {
            let wall_ms = t0.elapsed().as_millis();
            let updates = solve_cache.entry_update_count();
            eprintln!(
                "[openpoker_advisor] prewarm stored street={} updates={} wall_ms={} cache={}hit/{}miss{}",
                street_label(round_start.street()),
                updates.unwrap_or(0),
                wall_ms,
                solve_cache.hits(),
                solve_cache.misses(),
                if wall_ms > search_wall_slow_ms(scfg) { " SLOW" } else { "" },
            );
            let mut resp = mk("prewarm:stored".into());
            resp.street = Some(street_label(round_start.street()).into());
            resp.solve_updates = updates;
            resp
        }
        Err(reason) => mk(format!("prewarm:err:{reason}")),
    }
}

/// 两态 lockstep 单步（真实动作与幻影 fold 共用，保两路不漂）。
fn lockstep_step(
    real: &mut poker::GameState,
    abs: &mut SimplifiedNlheState,
    abs_rng: &mut ChaCha20Rng,
    actor: u8,
    action: &str,
    to_solver: Option<u64>,
) -> Result<(), String> {
    if real.current_player() != Some(SeatId(actor)) {
        // 码深漂移 / 历史错位 → 重放对不上回合。
        return Err("replay_seat_mismatch".into());
    }
    let Some(concrete) = hist_to_concrete(real, action, to_solver) else {
        return Err("bad_hist_action".into());
    };
    if real.apply(concrete).is_err() {
        return Err("replay_illegal".into());
    }
    let is_all_in = real.players()[actor as usize].status == poker::PlayerStatus::AllIn;
    if advance_shadow_by_applied(abs, concrete, is_all_in, abs_rng).is_err() {
        // 结构性 gap（如 open-limp 进 no-limp 影子）→ 失同步（不静默改 kind）。
        return Err("structural_gap".into());
    }
    if abs.game_state.current_player() != real.current_player() {
        return Err("lockstep_drift".into());
    }
    Ok(())
}

/// lockstep 失同步结果：原因 + **已同步前缀**的影子节点（断点**之前**最后一个对齐的
/// `current_node_id`）。脱影子搜索的档一前缀 reach 用它取已同步前缀的决策三元组
/// （[`synced_prefix_decisions`]）估 range（`unanchored_range_design` §1）。`synced_node` 在每步
/// 失败**前**捕获 = 所有成功步之后的影子节点（断点动作及其后按无信息处理，因子 1）。
struct LockstepErr {
    reason: String,
    synced_node: NodeId,
}

/// 两态 lockstep 重放（[`decide`] 决策路径与 [`prewarm`] 预热共用——**锚定/脱影子分类与
/// node_id 必须同源**，否则预热可能走错路径 / 推错 key 静默失效）。`my_turn = Some(seat)` →
/// 末尾校验轮到该座（决策路径，旧行为逐字保留）；`None` → 重放到历史末尾即可（预热：该街
/// hero 行动前，轮到的是首行动者）。失同步返回 [`LockstepErr`]（原因 + 已同步前缀节点）。
fn lockstep_replay(
    game: &SimplifiedNlheGame,
    solver_cfg: &TableConfig,
    smap: &SeatMap,
    req: &Request,
    scale: u64,
    board_len: usize,
    my_turn: Option<SeatId>,
) -> Result<(poker::GameState, SimplifiedNlheState), LockstepErr> {
    let mut real = poker::GameState::new(solver_cfg, REAL_REPLAY_SEED);
    let mut abs_rng = ChaCha20Rng::from_seed(ABS_REPLAY_SEED);
    let mut abs: SimplifiedNlheState = game.root(&mut abs_rng);
    // synced_node 在每步**前**捕获（abs 只在 lockstep_step 内推进）→ 失败时 = 断点前最后对齐节点
    // （所有成功步之后的影子节点；断点动作及其后按无信息处理，因子 1）。
    // 短桌：幻影座（UTG 起的最早行动位）开局先 fold → 走到 blueprint 树的真实节点。
    for &t in &smap.phantoms {
        let synced_node = abs.current_node_id;
        lockstep_step(&mut real, &mut abs, &mut abs_rng, t, "fold", None).map_err(|e| {
            LockstepErr {
                reason: format!("phantom_{e}"),
                synced_node,
            }
        })?;
    }
    for h in &req.actions {
        let synced_node = abs.current_node_id;
        let actor = smap.tree_seat(h.seat).map_err(|e| LockstepErr {
            reason: e,
            synced_node,
        })?;
        let to_solver = h.to.map(|t| t * scale);
        lockstep_step(
            &mut real,
            &mut abs,
            &mut abs_rng,
            actor,
            &h.action,
            to_solver,
        )
        .map_err(|e| LockstepErr {
            reason: e,
            synced_node,
        })?;
    }
    // —— 到我方决策点（决策路径）/ 历史末尾（预热）——
    if let Some(seat) = my_turn {
        if real.current_player() != Some(seat) {
            return Err(LockstepErr {
                reason: "not_my_turn".into(),
                synced_node: abs.current_node_id,
            });
        }
    }
    let street = real.street();
    if abs.game_state.street() != street || board_len != expected_board_len(street) {
        return Err(LockstepErr {
            reason: "street_board_mismatch".into(),
            synced_node: abs.current_node_id,
        });
    }
    Ok((real, abs))
}

/// 一次决策：重放本手历史（100BB real + abs 两态 lockstep）→ 我方决策点。`search == None` 时
/// 查 blueprint → outgoing（旧行为，byte-equal）；`search == Some` 且命中触发面（[`should_search`]）
/// 时建**真码深** subgame re-solve（[`subgame_search_cached`]；`deep_menu` → 子树用 {1pot} 单档菜单 +
/// outgoing 用 {1pot} 抽象算尺寸，缺口③）→ outgoing 按真栈算尺寸，解不出来 = check-when-free
/// （能 check 就 check、否则 fold；不回落 blueprint，§2.3）。**lockstep 失同步**（off-stack all-in
/// 线等）且 `search == Some` 且 postflop → 脱影子搜索（[`decide_search_unanchored`]，缺口②续）；
/// 其余**前置 / blueprint 路径**失败返回安全兜底（不 panic）。
///
/// `solve_cache` = within-round solve 缓存（[`SubgameSolveCache`] doc）：常驻 main loop 持有、
/// 跨请求传入——同手同街第二次决策命中即复用 solve、只重做导航（恢复「每轮恰好一个 solve」，
/// time_budget anytime 下逐决策重解会读不同均衡；mid-round wall ≈ 0）。key 覆盖 solve 全部输入
/// （solve 边界现算），跨手 / 跨街自然替换；blueprint 路径不读不写它（byte-equal 不受影响）。
fn decide(
    game: &SimplifiedNlheGame,
    abstraction: &StreetActionAbstraction,
    strategy_fn: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    req: &Request,
    base_seed: u64,
    search: Option<&SearchRuntime>,
    solve_cache: &mut SubgameSolveCache,
) -> Response {
    // —— 前置校验（不满足 = 兜底）——
    if req.hole.len() != 2 {
        return safe_fallback(&req.valid, "hole_not_2");
    }
    if req.num_seats as usize != N_SEATS {
        return safe_fallback(&req.valid, "not_6max");
    }
    if req.big_blind == 0 {
        return safe_fallback(&req.valid, "bad_bb");
    }
    let solver_cfg = TableConfig::default_6max_100bb();
    let solver_bb = solver_cfg.big_blind.as_u64();
    if solver_bb % req.big_blind != 0 {
        // OpenPoker BB 必须整除 solver BB（10/20 → ×5）；否则码深口径不一致 → 兜底。
        return safe_fallback(&req.valid, "scale_not_integer");
    }
    let scale = solver_bb / req.big_blind;
    // sb 必须按同一 scale 对齐 solver sb（默认 50；10×5=50）—— 否则盲注比例非标准、口径不一致。
    if req.small_blind * scale != solver_cfg.small_blind.as_u64() {
        return safe_fallback(&req.valid, "blind_ratio_mismatch");
    }

    let hole = match (parse_card(&req.hole[0]), parse_card(&req.hole[1])) {
        (Ok(a), Ok(b)) => [a, b],
        _ => return safe_fallback(&req.valid, "bad_hole"),
    };
    let board: Vec<Card> = match req.board.iter().map(|s| parse_card(s)).collect() {
        Ok(b) => b,
        Err(_) => return safe_fallback(&req.valid, "bad_board"),
    };
    dlog!(
        "decide: hole={:?} board={:?} my_seat={} button={} bb={} sb={} scale={} stacks={:?} dealt={:?} valid={:?} actions={:?}",
        req.hole, req.board, req.my_seat, req.button_seat, req.big_blind, req.small_blind,
        scale, req.stacks, req.dealt_seats, req.valid, req.actions
    );

    // —— 座位映射（满桌 = 旧 rotate 公式；短桌 = 幻影座映射，[`seat_map`]）——
    let smap = match seat_map(req) {
        Ok(m) => m,
        Err(reason) => {
            dlog!("seat_map FAIL → fallback:{reason}");
            return safe_fallback(&req.valid, &reason);
        }
    };
    dlog!(
        "seat_map: hu={} to_tree={:?} phantoms={:?}",
        smap.hu,
        smap.to_tree,
        smap.phantoms
    );
    if smap.hu && !board.is_empty() {
        // OpenPoker HU 行动序是角色序（postflop BB 先），树是环序（SB 先）→ postflop 映不进
        // （[`seat_map`] doc）。顺序错位本会被重放 seat 校验拦下，这里只是把原因标清楚。
        return safe_fallback(&req.valid, "short_hu_postflop");
    }
    let my_seat_solver = match smap.tree_seat(req.my_seat) {
        Ok(t) => SeatId(t),
        Err(reason) => return safe_fallback(&req.valid, &reason),
    };

    // —— 两态 lockstep 重放（blueprint 路径 + 锚定搜索的 node_id / legal_abs 来源；与
    // prewarm 共用 [`lockstep_replay`]，分类 / node_id 同源）——
    let lockstep = lockstep_replay(
        game,
        &solver_cfg,
        &smap,
        req,
        scale,
        board.len(),
        Some(my_seat_solver),
    )
    .and_then(|(real, abs)| {
        let legal_abs = SimplifiedNlheGame::legal_actions(&abs);
        if legal_abs.is_empty() {
            // empty_legal = 终局态（非失同步）；synced_node 取当前节点（前缀走旧分流即可）。
            return Err(LockstepErr {
                reason: "empty_legal".into(),
                synced_node: abs.current_node_id,
            });
        }
        Ok((real, abs, legal_abs))
    });
    let (real, abs, legal_abs) = match lockstep {
        Ok(t) => t,
        Err(LockstepErr {
            reason,
            synced_node,
        }) => {
            dlog!(
                "lockstep DESYNC: reason={reason} synced_node={synced_node:?} board_len={} search={} → {}",
                board.len(),
                search.is_some(),
                if search.is_some() && board.len() >= 3 {
                    "unanchored search"
                } else {
                    "blueprint fallback/limp-heuristic"
                }
            );
            // 100BB 影子失同步（off-stack all-in 线等：blueprint 树按 100BB 对称栈建、该线结构性
            // 缺节点）：缺口②续——`--search` 开且 postflop → **脱影子**搜索（触发 / 子树根 /
            // within-round 导航全来自真栈重放，[`subgame_search_unanchored`]；档一前缀 reach 用
            // synced_node 取已同步前缀估 range）；preflop / 未开搜索 → 维持旧兜底（preflop 走
            // blueprint 的 gating 不变，§1；search=None byte-equal）。
            if let Some(rt) = search {
                if board.len() >= 3 {
                    return decide_search_unanchored(
                        game,
                        abstraction,
                        strategy_fn,
                        req,
                        base_seed,
                        rt,
                        &solver_cfg,
                        scale,
                        &smap,
                        my_seat_solver,
                        hole,
                        &board,
                        &reason,
                        synced_node,
                        solve_cache,
                    );
                }
            }
            // preflop 失同步底池赔率 floor（2026-06-16 用户加）：真栈重放算 pot/to_call（限
            // preflop——postflop 失同步上面已 return 脱影子搜索 / 维持旧兜底，board 非空 → None）。
            let pre_pot_odds = if board.is_empty() {
                preflop_pot_odds(req, &solver_cfg, scale, &smap, my_seat_solver, hole)
            } else {
                None
            };
            // preflop open-limp 结构 gap → 启发式矩阵（[`limp_heuristic`]）；其 fold 出口（facing
            // bet）若底池赔率 floor 命中 → 改 call（保 `limp_heuristic:` 前缀，driver 分桶不变）。
            if reason == "structural_gap" && board.is_empty() {
                if let Some(mut resp) = limp_heuristic(req, hole) {
                    if resp.action == "fold"
                        && pre_pot_odds.as_ref().is_some_and(pot_odds_floor_hit)
                    {
                        resp.action = "call".into();
                        resp.amount = None;
                        resp.source = "limp_heuristic:pot_odds_call".into();
                    }
                    return resp;
                }
            }
            // 历史无 limp 的其他结构 gap / 其余 desync：喂 pre_pot_odds（preflop 命中 floor →
            // call；postflop = None，行为同改前）。
            return fallback_with_floor(
                &req.valid,
                hole,
                &board,
                pre_pot_odds,
                format!("fallback:{reason}"),
            );
        }
    };
    let street = real.street();
    let node_id = abs.current_node_id;
    let info = game.info_set_for_cards(node_id, hole, &board);

    // —— gating（设计 §1）：仅 `--search` 开 + 命中触发面才搜索；否则 blueprint。
    // should_search 只读街 + 本街是否已起注（与码深无关）→ 在 100BB `real` 上判等价真栈 auth。
    // `--search-flop-prefer-blueprint` on（[`SearchRuntime`] doc）：仅 flop 锚定面（此处 lockstep Ok）
    // 抑制搜索回 blueprint；脱影子 flop 走上面 `decide_search_unanchored`，不经此 gating，照常搜索。
    let want_search = matches!(search, Some(rt)
        if should_search(&real, rt.cfg.trigger)
            && !(rt.flop_prefer_blueprint && real.street() == poker::Street::Flop));
    dlog!(
        "lockstep OK: street={:?} node_id={node_id:?} info_set={} legal_abs={:?} path={}",
        street,
        info.raw(),
        legal_abs,
        if want_search {
            "search (anchored)"
        } else {
            "blueprint"
        }
    );
    // dist + outgoing 基准态：默认 = blueprint 分布 + 100BB real 算尺寸（search=None / 未触发，
    // byte-equal 旧行为）；搜索触发 = 真码深 auth 子博弈解 + auth 算尺寸（失败 → check-when-free，不回落）。
    // deep_abs_holder：缺口③ deep 搜索成功时记下与子树**同一**菜单（deep_menu_for(root_state)，
    // SPR 自适应：深 {1pot} / 浅 {0.5,1}）——outgoing 用它在真实 pot 上重算 to，与子树解自洽。
    let mut auth_holder: Option<GameState> = None;
    let mut deep_abs_holder: Option<StreetActionAbstraction> = None;
    let mut solve_updates: Option<u64> = None;
    let dist: Vec<(AbstractAction, f64)> = if want_search {
        let rt = search.expect("want_search ⇒ search.is_some()");
        let scfg = &rt.cfg;
        // 真码深 auth + round_start（真栈重放 + 注入真实牌）。建不了 = §2.3「建不了树」→ 安全降级。
        // 锚定搜索路径（lockstep Ok）：`prev_within` = 紧前一街完整动作线，档二′-跨街复用沿它在上一街
        // 已解子树（anchored/unanchored 皆可，turn_blueprint_trim §2.1）读 σ 算本街后验 range。
        let (auth, round_start, within, prev_within) = match build_real_auth(
            req,
            &solver_cfg,
            scale,
            &smap,
            my_seat_solver,
            hole,
            &board,
            true,
        ) {
            Ok(t) => t,
            Err(reason) => {
                // build 失败 → 无 auth → pot_odds=None。
                return fallback_with_floor(
                    &req.valid,
                    hole,
                    &board,
                    None,
                    format!("search_giveup:build:{reason}"),
                );
            }
        };
        let root_state: &GameState = match scfg.resolve_root {
            ResolveRoot::RoundStart => &round_start,
            ResolveRoot::CurrentDecision => &auth,
        };
        let hand_seed = hand_seed_for(req, base_seed);
        dlog!(
            "anchored search auth: street={:?} pot={} current={:?} stacks={:?} committed={:?} within={} prev_within={} cross_street={} hand_seed={hand_seed}",
            auth.street(),
            auth.pot().as_u64(),
            auth.current_player(),
            auth.players().iter().map(|p| p.stack.as_u64()).collect::<Vec<_>>(),
            auth.players().iter().map(|p| p.committed_total.as_u64()).collect::<Vec<_>>(),
            within.len(),
            prev_within.len(),
            rt.unanchored_cross_street
        );
        // 档二′-跨街复用（turn_blueprint_trim §2.4 改动 D，flag 现同管锚定 + 脱锚两路）：复用缓存里上一
        // 街已解子树的 σ 对 `prev_within`（上一街完整真实动作线）条件化得本街后验 range，覆盖 blueprint
        // estimate（裁掉 turn blueprint 的唯一 river 消费者）。off / 缓存非紧前街 → 子树内自验退 estimate
        // （不破现状）。预热路径（prewarm 锚定分支）同读 `cache.current()` + 同 `prev_within` → 同 key 命中。
        let cross_street = rt.unanchored_cross_street.then_some(prev_within.as_slice());
        // 监控：本决策搜索墙钟（build+solve；预热命中 ≈0）+ 是否现 build（misses 涨 = MISS）。
        let t0 = Instant::now();
        let misses_before = solve_cache.misses();
        match subgame_search_cached(
            Some(solve_cache), // within-round solve 缓存：同手同街命中 → 复用 solve 只重导航。
            &auth,
            root_state,
            game,
            &legal_abs,
            node_id,
            strategy_fn,
            scfg,
            // --search-bucket-table：子树 solve + hero 读数换独立桶表（None = blueprint 表）。
            rt.bucket_table.as_ref(),
            None, // depth_limit=false 解到终局 → 无 leaf_values（§2.1）。
            // deep_menu mid-round（AllPostflop）导航：当前街真实动作序在子树上重放（缺口③细化）。
            Some(&within),
            cross_street, // 档二′-跨街复用（上一街子树 σ 后验 range，覆盖 blueprint estimate）。
            hand_seed,
            req.actions.len() as u64,
        ) {
            Ok(d) => {
                // 遥测：本次决策用到的 solve 实际跑了多少 update（time_budget anytime 下
                // 即「5s 内迭代数」；命中 = 复用 solve 的原始计数）+ 墙钟 + 缓存命中。
                let wall_ms = t0.elapsed().as_millis();
                let hit = solve_cache.misses() == misses_before;
                solve_updates = solve_cache.entry_update_count();
                log_search_wall(
                    "",
                    street,
                    solve_updates.unwrap_or(0),
                    wall_ms,
                    search_wall_slow_ms(scfg),
                    hit,
                    solve_cache.hits(),
                    solve_cache.misses(),
                );
                if scfg.deep_menu {
                    deep_abs_holder = Some(deep_menu_for(root_state).0);
                }
                auth_holder = Some(auth); // outgoing 用真栈 auth 算尺寸。
                d
            }
            // 解不出来（建不了/未访问/失同步/限时连一轮迭代都未完成）→ check-when-free，不回落 blueprint。
            Err(reason) => {
                // auth 已建（此 Err 分支未 move 走）→ 喂底池赔率 floor。
                return fallback_with_floor(
                    &req.valid,
                    hole,
                    &board,
                    pot_odds_from_auth(&auth, &solver_cfg),
                    format!("search_giveup:unsolved:{reason}"),
                );
            }
        }
    } else {
        blueprint_distribution(&info, &legal_abs, strategy_fn)
    };

    // outgoing 基准态：搜索 → 真栈 auth（真码深尺寸）；blueprint → 100BB real（旧行为）。
    let outgoing_state: &GameState = auth_holder.as_ref().unwrap_or(&real);
    // outgoing 抽象：缺口③ 深码搜索 → 与子树同一菜单（deep_abs_holder，SPR 自适应）；否则
    // blueprint 抽象（旧行为；search=None / 非 deep 路径 byte-equal 不受影响）。
    let outgoing_abs: &StreetActionAbstraction = deep_abs_holder.as_ref().unwrap_or(abstraction);

    // per-decision 确定性采样（保混合策略 + 可复现；seed 与搜索与否无关 → search=None byte-equal）。
    let mut sample_rng = ChaCha20Rng::from_seed(sample_seed(req, base_seed));
    let chosen = sample_discrete(&dist, &mut sample_rng);

    // —— outgoing：solver Action → OpenPoker {action, amount}（blueprint / search 共享映射）——
    let solver_action = match outgoing_action(outgoing_state, outgoing_abs, chosen) {
        Ok(a) => a,
        Err(_) => {
            // 搜索路径有真栈 auth（auth_holder）→ 喂底池赔率 floor；blueprint 路径无 auth，
            // preflop 现建真栈算（postflop blueprint outgoing 失败保持 None，byte-equal）。
            let pot_odds = auth_holder
                .as_ref()
                .and_then(|a| pot_odds_from_auth(a, &solver_cfg))
                .or_else(|| {
                    board
                        .is_empty()
                        .then(|| {
                            preflop_pot_odds(req, &solver_cfg, scale, &smap, my_seat_solver, hole)
                        })
                        .flatten()
                });
            return fallback_with_floor(
                &req.valid,
                hole,
                &board,
                pot_odds,
                "fallback:outgoing_failed".into(),
            );
        }
    };
    let mut resp = action_to_response(solver_action, scale, &req.valid);
    if resp.source.is_empty() {
        // 合法动作：填 source（search / blueprint）+ 诊断。不合法时 action_to_response 已产
        // safe_fallback（source = fallback:...），不覆盖。
        resp.source = if want_search { "search" } else { "blueprint" }.into();
        resp.street = Some(street_label(street).into());
        resp.info_set = Some(info.raw());
        resp.chosen = Some(action_label(&chosen));
        resp.probs = Some(probs_log(&dist));
        resp.solve_updates = solve_updates; // blueprint 路径恒 None（serde skip，byte-equal）。
    }
    dlog!(
        "decide RESULT: source={} action={} amount={:?} chosen={:?} solve_updates={:?} dist={:?}",
        resp.source,
        resp.action,
        resp.amount,
        resp.chosen,
        resp.solve_updates,
        resp.probs
    );
    resp
}

/// blueprint 平均策略 → 归一 `(action, prob)`（空 / 全零 / 长度不符 → uniform 兜底）。逐字保留
/// 原 `decide` 内联逻辑（search=None 路径 byte-equal）。
fn blueprint_distribution(
    info: &InfoSetId,
    legal_abs: &[AbstractAction],
    strategy_fn: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
) -> Vec<(AbstractAction, f64)> {
    let raw = strategy_fn(info, legal_abs.len());
    if raw.len() == legal_abs.len() && raw.iter().any(|p| p.is_finite() && *p > 0.0) {
        let sum: f64 = raw.iter().filter(|p| p.is_finite() && **p > 0.0).sum();
        legal_abs
            .iter()
            .copied()
            .zip(raw)
            .filter(|(_, p)| p.is_finite() && *p > 0.0)
            .map(|(a, p)| (a, p / sum))
            .collect()
    } else {
        let p = 1.0 / legal_abs.len() as f64;
        legal_abs.iter().copied().map(|a| (a, p)).collect()
    }
}

/// solver [`Action`] → OpenPoker [`Response`]（blueprint / search 共享）。合法动作 → `source` 留空
/// （caller 填 blueprint/search + 诊断）；不合法（check/call/raise 区间缺）→ 直接 [`safe_fallback`]
/// （`source` 已填 fallback:...，caller 不覆盖）。尺寸按传入的 `scale` ÷ 真实下注（caller 已用真栈
/// 或 100BB 算出 solver `to`）。
fn action_to_response(action: Action, scale: u64, valid: &ValidActions) -> Response {
    match action {
        Action::Fold => Response {
            action: "fold".into(),
            ..Default::default()
        },
        Action::Check => {
            if valid.can_check {
                Response {
                    action: "check".into(),
                    ..Default::default()
                }
            } else {
                safe_fallback(valid, "check_illegal")
            }
        }
        Action::Call => {
            if valid.can_call {
                Response {
                    action: "call".into(),
                    ..Default::default()
                }
            } else if valid.can_check {
                Response {
                    action: "check".into(),
                    ..Default::default()
                }
            } else {
                safe_fallback(valid, "call_illegal")
            }
        }
        Action::Bet { to } | Action::Raise { to } => {
            // solver to → OpenPoker to（÷scale，四舍五入），夹进 [min_raise, max_raise]。
            match raise_to_op(to.as_u64(), scale, valid) {
                Some(op_to) => Response {
                    action: "raise".into(),
                    amount: Some(op_to),
                    ..Default::default()
                },
                None => safe_fallback(valid, "raise_no_range"),
            }
        }
        Action::AllIn => {
            // OpenPoker all_in 无 amount（服务端归一）。无 all_in 动作则退到 max raise。
            if let Some(max) = valid.max_raise {
                Response {
                    action: "raise".into(),
                    amount: Some(max),
                    ..Default::default()
                }
            } else {
                Response {
                    action: "all_in".into(),
                    ..Default::default()
                }
            }
        }
    }
}

// ===========================================================================
// 兜底「别扔好牌」地板（用户 2026-06-14）
// ===========================================================================
//
// 搜索区降级（设计 §2.3，2026-06-09 改 check-when-free）的 `source = search_giveup:<reason>`
// 现由 [`fallback_with_floor`] 统一产出（caller 拼好 `search_giveup:...` 前缀传入），与 blueprint
// 路径的 `fallback:...` 仍分桶（driver §4.1 护栏不变）；地板未命中时行为 = 旧 search_giveup
// （能 check 就 check、否则 fold——紧、不漏筹码、不回落 blueprint）。

/// postflop 兜底地板的「接近坚果」阈值（用户 2026-06-14 拍板 ≥95%，2026-06-15 下调为 **>93%**）：
/// hero 在**当前 board**上击败-或-打平的对手两张组合占比 **>** 此值 → 面对下注时改 fold 为 call。
const NUT_CALL_THRESHOLD: f64 = 0.93;

/// hero 手牌在**当前 board**上的「坚果度」：hero 最强 5/6/7-card 牌力 **击败-或-打平**的
/// 对手两张组合（从剩余牌堆取，排除 hole + board）占全部组合的比例。**只算当前 board，不
/// 预判后续公牌**（用户 2026-06-14 拍板；flop/turn 因此偏保守、是「当前」坚果度）。打平算
/// 进分子（同牌力分池、call 不亏）。board 非 3/4/5 张（非 postflop）→ 返回 0.0（不触发地板）。
fn current_board_nuttiness(hole: [Card; 2], board: &[Card]) -> f64 {
    if !(3..=5).contains(&board.len()) {
        return 0.0;
    }
    let ev = NaiveHandEvaluator;
    // dead = hole + board；对手两张从剩余 52−dead 张取。
    let mut dead = [false; 52];
    for c in hole.iter().chain(board.iter()) {
        dead[c.to_u8() as usize] = true;
    }
    let rank_of = |h0: Card, h1: Card| match board.len() {
        3 => ev.eval5(&[h0, h1, board[0], board[1], board[2]]),
        4 => ev.eval6(&[h0, h1, board[0], board[1], board[2], board[3]]),
        _ => ev.eval7(&[h0, h1, board[0], board[1], board[2], board[3], board[4]]),
    };
    let hero = rank_of(hole[0], hole[1]);
    let remaining: Vec<Card> = (0u8..52)
        .filter(|&i| !dead[i as usize])
        .map(|i| Card::from_u8(i).expect("0..52 是合法 Card"))
        .collect();
    let (mut beat_or_tie, mut total) = (0u64, 0u64);
    for i in 0..remaining.len() {
        for j in (i + 1)..remaining.len() {
            total += 1;
            if hero >= rank_of(remaining[i], remaining[j]) {
                beat_or_tie += 1;
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        beat_or_tie as f64 / total as f64
    }
}

/// fallback「底池赔率」floor 的输入（2026-06-15 用户加）：facing bet 时若 `pot/to_call > 4`
/// 且 `to_call ≤ 30BB` → call（好赔率 + 注额不大就别白弃）。`pot` / `to_call` / `big_blind` 同
/// 单位（solver chip）—— 比值 `pot/to_call` 与单位无关、`to_call` vs `30×BB` 同单位 → 自洽。
/// `pot` 取真栈 pot（含对手当前注），call-for-less（封顶全下跟）时含未被跟到的余注 → 略偏乐观，
/// 对「别白弃」这条 floor 方向一致、可接受（不做侧池/退注精算）。
#[derive(Copy, Clone, Debug)]
struct PotOdds {
    pot: u64,
    to_call: u64,
    big_blind: u64,
}

/// 从真栈 `auth` 状态取底池赔率 floor 输入（solver 单位）。当前行动者面对下注（`call` 合法、
/// 需补差 `> 0`）才返回 `Some`；能 check / 已盖牌 / 终局 → `None`（该 floor 不触发）。仅在能重建
/// `auth` 的兜底点可调；build 失败 / 无 `auth` 的兜底点传 `None`。
fn pot_odds_from_auth(auth: &GameState, solver_cfg: &TableConfig) -> Option<PotOdds> {
    let seat = auth.current_player()?;
    // `legal_actions().call` = 绝对跟注额（call-to 总额，封顶全下时 = cap）；补差 = 减本街已投入。
    let call_to = auth.legal_actions().call?;
    let committed = auth.players()[seat.0 as usize].committed_this_round;
    let to_call = call_to.as_u64().checked_sub(committed.as_u64())?;
    (to_call > 0).then_some(PotOdds {
        pot: auth.pot().as_u64(),
        to_call,
        big_blind: solver_cfg.big_blind.as_u64(),
    })
}

/// 底池赔率 floor 判据（[`PotOdds`]，2026-06-15 用户加；2026-06-16 扩到 preflop）：facing bet
/// 时 `pot/to_call > 4` 且 `to_call ≤ 30BB` → 命中（好赔率 + 注额不大 → 别白弃）。**单一口径
/// 来源**——[`fallback_with_floor`] 的 ② 与 preflop [`limp_heuristic`] 拦截层共用，避免 `>4` /
/// `≤30BB` 两处漂移。
fn pot_odds_floor_hit(po: &PotOdds) -> bool {
    po.to_call > 0 && po.pot as f64 > 4.0 * po.to_call as f64 && po.to_call <= 30 * po.big_blind
}

/// preflop 失同步/兜底点的底池赔率 floor 输入：真栈重放（limp 池真栈合法可建；off-stack 等
/// 建不了 → `None` 不触发，安全降级，同现有 postflop floor「无 auth 不触发」）→
/// [`pot_odds_from_auth`]。**仅 preflop 调**（board 必空；postflop 失同步走脱影子搜索或维持
/// 旧兜底，不喂此值，byte-equal 不受影响）。无 `req.stacks` 时 [`real_stacks_config`] 退对称
/// 100BB，pot/to_call 比值仍正确。
fn preflop_pot_odds(
    req: &Request,
    solver_cfg: &TableConfig,
    scale: u64,
    smap: &SeatMap,
    my_seat_solver: SeatId,
    hole: [Card; 2],
) -> Option<PotOdds> {
    build_real_auth(
        req,
        solver_cfg,
        scale,
        smap,
        my_seat_solver,
        hole,
        &[],
        true,
    )
    .ok()
    .and_then(|(auth, _, _, _)| pot_odds_from_auth(&auth, solver_cfg))
}

/// 决策阶段兜底动作 + 「别扔好牌」地板（用户 2026-06-14；2026-06-15 加底池赔率 floor）。
/// **只在面对下注**（不能免费 check 且能 call）时把 fold 改成 call，两条 floor 依次判：
///  ① preflop 拿 AA/KK/QQ/JJ/AK/AQ（[`limp_hand_tier`]==2）→ call；postflop 手牌坚果度
///     **>** [`NUT_CALL_THRESHOLD`] → call。
///  ② ①不命中时看底池赔率（[`PotOdds`]，2026-06-15 用户加）：`pot/to_call > 4` 且
///     `to_call ≤ 30BB` → call（`pot_odds=None` 的兜底点不触发，行为同改前）。
/// 其余一律原 check-when-free（能 check 就 check、否则 fold——紧、不漏筹码）；能免费 check 的
/// 局面不为这两条地板主动下注（用户只要「别扔/跟注」）。命中时 `source` 在 caller 给的完整
/// source 后追加 `:premium_call` / `:nut_call`（①）或 `:pot_odds_call`（②），**前缀不变**
/// （driver 仍按 `fallback:` / `search_giveup:` 分桶，`starts_with` 不变量保持）。仅用于已完成
/// 座位映射的决策阶段兜底（座位映射 / 入参校验失败仍走原 [`safe_fallback`]，那里状态不可信）。
fn fallback_with_floor(
    valid: &ValidActions,
    hole: [Card; 2],
    board: &[Card],
    pot_odds: Option<PotOdds>,
    source: String,
) -> Response {
    if !valid.can_check && valid.can_call {
        // ① 坚果度 / preflop premium。
        let nut = if board.is_empty() {
            (limp_hand_tier(hole) == 2).then_some("premium_call")
        } else {
            (current_board_nuttiness(hole, board) > NUT_CALL_THRESHOLD).then_some("nut_call")
        };
        // ② 底池赔率（①不命中才看）。
        let suffix = nut.or_else(|| pot_odds.filter(pot_odds_floor_hit).map(|_| "pot_odds_call"));
        if let Some(sfx) = suffix {
            return Response {
                action: "call".into(),
                source: format!("{source}:{sfx}"),
                ..Default::default()
            };
        }
    }
    let action = if valid.can_check { "check" } else { "fold" };
    Response {
        action: action.into(),
        source,
        ..Default::default()
    }
}

// ===========================================================================
// preflop open-limp 池启发式（结构 gap 残余收口，2026-06-12 矩阵）
// ===========================================================================

/// 手牌档位（169 类，写死常识集合）：`2` = P 档（AA KK QQ JJ / AKs AKo / AQs AQo，面对加注
/// 继续 call——2026-06-14 补 JJ/AQ，原只 AA/KK/QQ/AK，fallback/limp 路径会白弃 JJ/AQ）；
/// `1` = S∖P（TT 99 / AJs ATs / KQs，连同 P 构成 iso-raise 档）；`0` = 其余。
fn limp_hand_tier(hole: [Card; 2]) -> u8 {
    use poker::Rank::{Ace, Jack, King, Nine, Queen, Ten};
    let (mut hi, mut lo) = (hole[0].rank(), hole[1].rank());
    if hi < lo {
        std::mem::swap(&mut hi, &mut lo);
    }
    let suited = hole[0].suit() == hole[1].suit();
    let pair = hi == lo;
    // P 档（面对加注继续 call）：QQ+ 与 JJ、AK、AQ（2026-06-14 补 JJ/AQ）。
    if (pair && hi >= Jack) || (hi == Ace && (lo == King || lo == Queen)) {
        return 2;
    }
    // S∖P（连同 P 构成 iso-raise 档）：TT/99、ATs/AJs、KQs（JJ+/AQ 已 return 2）。
    let s = (pair && hi >= Nine)
        || (hi == Ace && lo >= Ten && suited)
        || (hi == King && lo == Queen && suited);
    if s {
        1
    } else {
        0
    }
}

/// preflop open-limp 池启发式：blueprint 树不含 open-limp 节点（no-limp 抽象），preflop 撞
/// `structural_gap` 后原兜底（check-when-free/fold）会把 JJ@BB 这类好牌白扔。preflop 子树
/// 搜索（建树规模）与 limp 线重训（树预算）均不可行 → 显式启发式收口，职责一句话：
/// **别把好牌扔掉、别用烂牌烧钱，把局面送进 flop 交给脱锚搜索**（limp 池 postflop 已由
/// [`subgame_search_unanchored`] 接管）。
///
/// 矩阵（档位见 [`limp_hand_tier`]）：
/// - **没人加注**：S（tier≥1）→ iso-raise to `(4+limper 数)×BB`（clamp 进 [min,max]）；
///   其余 can_check（BB）→ check 免费，否则 fold（SB 损 0.5BB 死盲认了）。
/// - **有人加注**（含 hero 启发式 raise 后被 re-raise 又轮回——同分支处理，递归自然终止）：
///   P（tier==2）→ call（不设金额 cap，对 bot 池先收数据再调）；其余 → fold。
///
/// 返回 `None` = 历史里没有 open-limp（其他结构 gap 误入的防御）→ caller 维持原兜底。
/// `source = limp_heuristic:{raise,check,call,fold}`（driver 单独分桶，不混 blueprint/search）。
fn limp_heuristic(req: &Request, hole: [Card; 2]) -> Option<Response> {
    let mut limps = 0u64;
    let mut raised = false;
    for h in &req.actions {
        match h.action.as_str() {
            "call" if !raised => limps += 1, // 首个 raise 前的 call = open-limp / over-limp
            "raise" | "all_in" | "bet" => raised = true,
            _ => {}
        }
    }
    if limps == 0 {
        return None;
    }
    let tier = limp_hand_tier(hole);
    let valid = &req.valid;
    let mk = |action: &str, amount: Option<u64>, case: &str| Response {
        action: action.into(),
        amount,
        source: format!("limp_heuristic:{case}"),
        street: Some("preflop".into()),
        ..Default::default()
    };
    if !raised {
        if tier >= 1 && valid.can_raise {
            if let (Some(min), Some(max)) = (valid.min_raise, valid.max_raise) {
                if min <= max {
                    // OpenPoker raise amount = 总 to 额；脏区间（min>max）走下面被动分支。
                    let to = (4 + limps) * req.big_blind;
                    return Some(mk("raise", Some(to.clamp(min, max)), "raise"));
                }
            }
        }
        // 非 S 档（或 raise 不可用的防御）：BB 免费 check，否则 fold。
        return Some(if valid.can_check {
            mk("check", None, "check")
        } else {
            mk("fold", None, "fold")
        });
    }
    if tier == 2 && valid.can_call {
        return Some(mk("call", None, "call"));
    }
    Some(if valid.can_check {
        mk("check", None, "check") // 防御（preflop 面对加注不该 can_check）
    } else {
        mk("fold", None, "fold")
    })
}

/// 缺口②：在**真码深** config 上重放本手 → 注入真实牌，产 `(auth, round_start, within, prev_within)`
/// 喂 [`subgame_search`] / [`subgame_search_unanchored`]。`auth` = 当前决策点真栈态（query_at 索引
/// hero 真桶用）；`round_start` = 当前街起点快照（[`ResolveRoot::RoundStart`] 子树根）；`within` =
/// **当前街**真实动作序 `(动作, 是否令行动者 all-in)`（街变清空；脱影子路径的 within-round 导航
/// 输入，锚定路径不读）；`prev_within` = **紧前一街完整**真实动作线（**含收街动作**——档二′-跨街复用
/// 沿它在上一街子树读 σ，[`cross_street_posterior_range`]）。重放对不上 / 注入失败 → `Err`
/// （caller 安全降级）。
///
/// `require_my_turn`：决策路径 `true`（重放末尾须轮到 hero，旧行为）；[`prewarm`] 传 `false`
/// （预热在 hero 行动**前**，轮到的是该街首行动者——只取 `round_start` 快照）。
///
/// **`within` / `round_start` / `auth` 输出与加 `prev_within` 前逐字节相同**（街变分支只多「收街动作
/// 补进 within 再 `take` 进 prev_within」，`within` 仍以空收尾、`round_start` 仍重快照）。
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn build_real_auth(
    req: &Request,
    solver_cfg: &TableConfig,
    scale: u64,
    smap: &SeatMap,
    my_seat_solver: SeatId,
    hole: [Card; 2],
    board: &[Card],
    require_my_turn: bool,
) -> Result<
    (
        GameState,
        GameState,
        Vec<(Action, bool)>,
        Vec<(Action, bool)>,
    ),
    String,
> {
    let real_cfg = real_stacks_config(req, solver_cfg, scale, smap)?;

    let mut auth = GameState::new(&real_cfg, REAL_REPLAY_SEED);
    // round_start 快照：街变即重 snapshot（postflop 街起点）；初始 = preflop 起点（不被搜索读）。
    let mut round_start = auth.clone();
    let mut rs_street = auth.street();
    let mut within: Vec<(Action, bool)> = Vec::new();
    // 紧前一街**完整**动作线（含收街动作）：街变时由当前 within + 收街动作组成（档二′-跨街复用）。
    let mut prev_within: Vec<(Action, bool)> = Vec::new();
    // 短桌幻影 fold + 真实动作走同一条重放（[`seat_map`]；幻影 fold 在 preflop 开局，
    // 不可能收街——盲注两座还没行动，within 推进与真实动作同口径即可）。
    let phantom = smap.phantoms.iter().map(|&t| (t, "fold".to_string(), None));
    let acts = req
        .actions
        .iter()
        .map(|h| {
            Ok((
                smap.tree_seat(h.seat).map_err(|e| format!("auth_{e}"))?,
                h.action.clone(),
                h.to,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    for (actor, action, to_op) in phantom.chain(acts) {
        if auth.current_player() != Some(SeatId(actor)) {
            return Err("auth_seat_mismatch".into());
        }
        let to_solver = to_op.map(|t| t.checked_mul(scale).ok_or("to_overflow"));
        let to_solver = match to_solver {
            Some(Ok(v)) => Some(v),
            Some(Err(e)) => return Err(e.into()),
            None => None,
        };
        let Some(concrete) = hist_to_concrete(&auth, &action, to_solver) else {
            return Err("auth_bad_hist".into());
        };
        if auth.apply(concrete).is_err() {
            return Err("auth_replay_illegal".into());
        }
        let became_all_in = auth.players()[actor as usize].status == poker::PlayerStatus::AllIn;
        if auth.street() != rs_street {
            // 收街动作属上一街：补进 within 凑齐上一街**完整**线 → take 进 prev_within（within 随之
            // 空，与旧 `within.clear()` 同收尾）、重 snapshot。
            within.push((concrete, became_all_in));
            prev_within = std::mem::take(&mut within);
            round_start = auth.clone();
            rs_street = auth.street();
        } else {
            within.push((concrete, became_all_in));
        }
    }
    if require_my_turn && auth.current_player() != Some(my_seat_solver) {
        return Err("auth_not_my_turn".into());
    }
    if auth.is_terminal() || auth.current_player().is_none() {
        return Err("auth_terminal".into()); // 预热路径防御（决策路径被上一检查覆盖）。
    }
    // 注入真实牌（hero hole + board）到当前点 + 街起点（subgame solve / query_at 读真牌）。
    let auth = auth.inject_external_cards(my_seat_solver, hole, board)?;
    let round_start = round_start.inject_external_cards(my_seat_solver, hole, board)?;
    Ok((auth, round_start, within, prev_within))
}

/// PFR-aware 形状用：一个对手座本手**翻前**的入池方式（[`preflop_entry_kinds`] 重放判定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreflopEntry {
    /// 无翻前动作记录 / 重放失败 / 该座弃牌 → 不据 PFR 选形状（退 `TopK`）。
    Unknown,
    /// 仅 limp/call/check 进池（无主动加注）→ `CallBand`（掐顶端会加注的强牌）。
    Passive,
    /// 翻前 raise/bet/all_in 过（主动入池）→ `RaiseBand`（收到顶端加注 range）。
    Aggressive,
}

/// 重放本手**翻前街**，返回每个 solver tree seat 的入池方式（[`PreflopEntry`]，下标 = tree seat）。
/// 纯函数（只读 `req`+`smap`+`solver_cfg`）：真栈建不出 / 重放失同步 / 脏动作 / 溢出 → 全
/// `Unknown`（caller 退 `TopK`，保守安全）。只看翻前——街一进 flop 即停。与 [`build_real_auth`]
/// 同重放口径（真栈 config + 幻影 fold + [`hist_to_concrete`]），但不注入牌、不要求轮到 hero。
fn preflop_entry_kinds(
    req: &Request,
    solver_cfg: &TableConfig,
    scale: u64,
    smap: &SeatMap,
) -> [PreflopEntry; N_SEATS] {
    let unknown = [PreflopEntry::Unknown; N_SEATS];
    let Ok(real_cfg) = real_stacks_config(req, solver_cfg, scale, smap) else {
        return unknown;
    };
    let mut st = GameState::new(&real_cfg, REAL_REPLAY_SEED);
    let mut kinds = unknown;
    let phantom = smap.phantoms.iter().map(|&t| (t, "fold".to_string(), None));
    let acts: Vec<(u8, String, Option<u64>)> = match req
        .actions
        .iter()
        .map(|h| {
            Ok((
                smap.tree_seat(h.seat).map_err(|e| e.to_string())?,
                h.action.clone(),
                h.to,
            ))
        })
        .collect::<Result<Vec<_>, String>>()
    {
        Ok(v) => v,
        Err(_) => return unknown,
    };
    for (actor, action, to_op) in phantom.chain(acts) {
        if st.street() != poker::Street::Preflop {
            break; // 只看翻前街
        }
        if st.current_player() != Some(SeatId(actor)) {
            return unknown; // 失同步 → 保守退 TopK
        }
        // 在 apply 前分类（此动作属翻前）：raise/bet/all_in 锁 Aggressive；call/check 升 Passive
        // （若未锁 Aggressive，含 BB option check）；fold 不改（弃牌座翻后不在 root range、不被剥削）。
        let slot = &mut kinds[actor as usize];
        match action.as_str() {
            "raise" | "bet" | "all_in" => *slot = PreflopEntry::Aggressive,
            "call" | "check" if *slot != PreflopEntry::Aggressive => *slot = PreflopEntry::Passive,
            _ => {}
        }
        let to_solver = match to_op.map(|t| t.checked_mul(scale)) {
            Some(Some(v)) => Some(v),
            Some(None) => return unknown, // 溢出
            None => None,
        };
        let Some(concrete) = hist_to_concrete(&st, &action, to_solver) else {
            return unknown;
        };
        if st.apply(concrete).is_err() {
            return unknown;
        }
    }
    kinds
}

/// `(--exploit-pfr-shape 开关, 该座 PFR 是否收敛, 本手翻前入池方式)` → 宽度形状。off / PFR 未收敛 /
/// 入池方式未知 → `TopK`（仅 VPIP，与现有 exploit 逐位 byte-equal）。
fn pick_shape(on: bool, pfr_converged: bool, entry: PreflopEntry) -> ExploitShape {
    if !on || !pfr_converged {
        return ExploitShape::TopK;
    }
    match entry {
        PreflopEntry::Passive => ExploitShape::CallBand,
        PreflopEntry::Aggressive => ExploitShape::RaiseBand,
        PreflopEntry::Unknown => ExploitShape::TopK,
    }
}

/// 缺口②续（v1 边界①收口）：**影子失同步区**的脱影子搜索。off-stack all-in 线上 100BB 影子 /
/// blueprint 全局树**结构性缺节点**（树按 100BB 对称栈建：短码 shove 在树里是全栈 all-in，
/// 「raise-over / call 完还活着」的后续节点不存在）→ lockstep 重放必失同步、拿不到 node_id。
/// 本路径把触发判定 / 子树根 / within-round 导航全改从**真栈重放**取
/// （[`subgame_search_unanchored`]），返回子树自身合法集分布，outgoing 按真栈 `auth` + 与子树
/// 同一抽象算尺寸（`source = search:unanchored`）。非搜索区（真栈判未命中触发面）→ 维持旧兜底
/// `fallback:<lockstep 原因>`（blueprint 区由影子承载、这里修不了）；真解不出来 → check-when-free
/// （`search_giveup:*`，不回落 blueprint，§2.3）。
///
/// **range 先验**：`rt.unanchored_prefix_reach` 开（**生产默认**，§5.1 实测拍板）= 档一前缀
/// reach——用 `synced_node`（失同步前已同步的影子节点）取已同步前缀的决策三元组
/// （[`synced_prefix_decisions`]）+ `strategy_fn`（blueprint σ）估 per-seat range 替代 uniform
/// （[`PrefixReach`]）；`off`（A/B 对照臂）= uniform（既有行为）。算出的 reach 进 solve 缓存 key，
/// 开/关自动 cache miss（`unanchored_range_design` §1/§5.1）。
#[allow(clippy::too_many_arguments)]
fn decide_search_unanchored(
    game: &SimplifiedNlheGame,
    abstraction: &StreetActionAbstraction,
    strategy_fn: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    req: &Request,
    base_seed: u64,
    rt: &SearchRuntime,
    solver_cfg: &TableConfig,
    scale: u64,
    smap: &SeatMap,
    my_seat_solver: SeatId,
    hole: [Card; 2],
    board: &[Card],
    shadow_reason: &str,
    synced_node: NodeId,
    solve_cache: &mut SubgameSolveCache,
) -> Response {
    let scfg = &rt.cfg;
    // 真栈重放（auth / 轮起点快照 / 当前街真实动作序 / 紧前一街完整动作线）。建不了 = §2.3「建不了
    // 树」→ 安全降级。`prev_within` = 档二′-跨街复用沿它在上一街子树读 σ（默认关）。
    let (auth, round_start, within, prev_within) = match build_real_auth(
        req,
        solver_cfg,
        scale,
        smap,
        my_seat_solver,
        hole,
        board,
        true,
    ) {
        Ok(t) => t,
        Err(reason) => {
            // build 失败 → 无 auth → pot_odds=None。
            return fallback_with_floor(
                &req.valid,
                hole,
                board,
                None,
                format!("search_giveup:unanchored_build:{reason}"),
            );
        }
    };
    dlog!(
        "unanchored search auth: shadow_reason={shadow_reason} synced_node={synced_node:?} street={:?} pot={} current={:?} stacks={:?} committed={:?} within={} prev_within={} prefix_reach={} cross_street={} exploit={}",
        auth.street(),
        auth.pot().as_u64(),
        auth.current_player(),
        auth.players().iter().map(|p| p.stack.as_u64()).collect::<Vec<_>>(),
        auth.players().iter().map(|p| p.committed_total.as_u64()).collect::<Vec<_>>(),
        within.len(),
        prev_within.len(),
        rt.unanchored_prefix_reach,
        rt.unanchored_cross_street,
        rt.exploit.is_some()
    );
    // gating 在真栈 auth 上判（影子失同步、100BB real 不可得；should_search 只读街+本街是否起注）。
    // 未命中 / board 与真栈街对不上 → 维持旧兜底（非搜索区，labels 与 search=None 同）。
    if !should_search(&auth, scfg.trigger) || board.len() != expected_board_len(auth.street()) {
        dlog!(
            "unanchored gating MISS (street={:?} board_len={}) → fallback:{shadow_reason}",
            auth.street(),
            board.len()
        );
        return fallback_with_floor(
            &req.valid,
            hole,
            board,
            pot_odds_from_auth(&auth, solver_cfg),
            format!("fallback:{shadow_reason}"),
        );
    }
    let hand_seed = hand_seed_for(req, base_seed);
    // 档一前缀 reach（生产默认开）：用已同步前缀估 range；off → None = uniform（byte-equal 旧行为）。
    let prefix_decisions = rt
        .unanchored_prefix_reach
        .then(|| synced_prefix_decisions(game, synced_node));
    let prefix_reach = prefix_decisions.as_ref().map(|d| PrefixReach {
        strategy: strategy_fn,
        decisions: d,
    });
    dlog!(
        "unanchored prefix_decisions: count={}",
        prefix_decisions.as_ref().map_or(0, |d| d.len())
    );
    // 档二′-跨街复用（默认关）：上一街本身 unanchored、子树已解（within-round 缓存当前条目）时，复用其
    // σ 对 `prev_within`（上一街完整真实动作线）条件化得本街后验 range，覆盖档一前缀 reach。off / 缓存
    // 非紧前街 unanchored 解 → 自验退档一（不破现状）。算出 ranges 进 solve key → 开/关自动 miss。
    let cross_street = rt.unanchored_cross_street.then_some(prev_within.as_slice());
    // 叠加剥削 Tier 2（exploit_strategy_design §2/§4）：组 per-solver-seat 画像 + 强度 α（仅 --exploit
    // 开、对手已收敛）。owned 快照，`borrow` 随即释放（solve 并行不碰 Profiler，只吃此快照）。hero 座
    // 按 OpenPoker my_seat 跳过（subgame apply_exploit_width_prior 再守一层）。`None` = 不剥削 →
    // _cross_exploit 走 exploit=None 分支，与既有 search:unanchored 逐位 byte-equal。
    let exploit_owned: Option<(Vec<Option<OpponentProfile>>, f64, Vec<ExploitShape>)> =
        rt.exploit.as_ref().map(|cell| {
            let prof = cell.borrow();
            let alpha = prof.cfg().strength_alpha;
            let pfr_shape_on = prof.cfg().pfr_shape;
            let mut profiles: Vec<Option<OpponentProfile>> = vec![None; N_SEATS];
            let mut shapes: Vec<ExploitShape> = vec![ExploitShape::TopK; N_SEATS];
            // PFR-aware（--exploit-pfr-shape）：重放翻前判各对手座本手入池方式（被动→CallBand 掐顶端 /
            // 主动→RaiseBand 收顶端），且仅当该座 PFR 也收敛才用。off → 全 Unknown → 全 TopK（仅 VPIP，
            // 与现有 exploit 逐位 byte-equal）；重放失败同样退 TopK（安全）。索引 = solver tree seat。
            let entry_kinds = if pfr_shape_on {
                preflop_entry_kinds(req, solver_cfg, scale, smap)
            } else {
                [PreflopEntry::Unknown; N_SEATS]
            };
            for op_seat in 0..N_SEATS {
                if op_seat == req.my_seat as usize {
                    continue; // hero
                }
                let Some(tree) = smap.to_tree[op_seat] else {
                    continue; // 未发牌座（短桌幻影 / 空座）
                };
                if let Some(name) = req.names.get(&(op_seat as u8)) {
                    if let Some(p) = prof.profile_for(name) {
                        profiles[tree as usize] = Some(p);
                        shapes[tree as usize] =
                            pick_shape(pfr_shape_on, p.pfr_converged, entry_kinds[tree as usize]);
                    }
                }
            }
            (profiles, alpha, shapes)
        });
    let exploit_prior = exploit_owned
        .as_ref()
        .map(|(profiles, alpha, shapes)| ExploitPrior {
            profiles: profiles.as_slice(),
            alpha: *alpha,
            shapes: shapes.as_slice(), // off / 未收敛 PFR / 重放失败 → 全 TopK（byte-equal）。
        });
    let t0 = Instant::now();
    let misses_before = solve_cache.misses();
    let dist = match subgame_search_unanchored_cached_cross_exploit(
        Some(solve_cache), // within-round solve 缓存（同锚定路径；kind 进 key、两路不串条目）。
        &auth,
        &round_start,
        game,
        &within,
        scfg,
        rt.bucket_table.as_ref(), // --search-bucket-table：子树独立桶表（None = blueprint 表）。
        prefix_reach,
        cross_street,
        exploit_prior, // 叠加剥削先验（None = 不剥削，byte-equal）。
        hand_seed,
    ) {
        Ok(d) => d,
        Err(reason) => {
            // auth 已建（真栈河牌决策态）→ 喂底池赔率 floor（用户场景：河牌 giveup 被它接住）。
            return fallback_with_floor(
                &req.valid,
                hole,
                board,
                pot_odds_from_auth(&auth, solver_cfg),
                format!("search_giveup:unanchored:{reason}"),
            );
        }
    };
    let wall_ms = t0.elapsed().as_millis();
    let hit = solve_cache.misses() == misses_before;
    let solve_updates = solve_cache.entry_update_count();
    log_search_wall(
        ":unanchored",
        auth.street(),
        solve_updates.unwrap_or(0),
        wall_ms,
        search_wall_slow_ms(scfg),
        hit,
        solve_cache.hits(),
        solve_cache.misses(),
    );
    // outgoing：真栈 auth + 与子树同一抽象（deep_menu → deep_menu_for(round_start)，SPR 自适应：
    // 深 {1pot} / 浅 {0.5,1}；否则 blueprint 菜单）——子树自身合法集契约（同 deep 路径）：
    // chosen 的 ratio 在真实 pot 上重算 to，自洽。round_start = unanchored 子树根（同一 SPR 输入）。
    let deep_abs: Option<StreetActionAbstraction> =
        scfg.deep_menu.then(|| deep_menu_for(&round_start).0);
    let outgoing_abs: &StreetActionAbstraction = deep_abs.as_ref().unwrap_or(abstraction);
    let mut sample_rng = ChaCha20Rng::from_seed(sample_seed(req, base_seed));
    let chosen = sample_discrete(&dist, &mut sample_rng);
    let solver_action = match outgoing_action(&auth, outgoing_abs, chosen) {
        Ok(a) => a,
        Err(_) => {
            return fallback_with_floor(
                &req.valid,
                hole,
                board,
                pot_odds_from_auth(&auth, solver_cfg),
                "fallback:outgoing_failed".into(),
            )
        }
    };
    let mut resp = action_to_response(solver_action, scale, &req.valid);
    if resp.source.is_empty() {
        resp.source = "search:unanchored".into();
        resp.street = Some(street_label(auth.street()).into());
        resp.chosen = Some(action_label(&chosen));
        resp.probs = Some(probs_log(&dist));
        resp.solve_updates = solve_updates;
        // 叠加剥削遥测：本决策实际剥削了哪些对手座（已收敛）+ 其 VPIP。无收敛对手 → None
        // （serde skip → byte-equal）。
        resp.exploit = exploit_owned.as_ref().and_then(|(profiles, _, _)| {
            let tel: Vec<(u8, f64)> = profiles
                .iter()
                .enumerate()
                .filter_map(|(s, p)| p.map(|p| (s as u8, p.vpip)))
                .collect();
            (!tel.is_empty()).then_some(tel)
        });
    }
    dlog!(
        "unanchored RESULT: source={} action={} amount={:?} chosen={:?} solve_updates={:?} exploit={:?} dist={:?}",
        resp.source, resp.action, resp.amount, resp.chosen, resp.solve_updates, resp.exploit,
        resp.probs
    );
    resp
}

/// 真码深 [`TableConfig`]：各座起始栈 = OpenPoker hand-start 栈 × `scale`（座位按 [`seat_map`]
/// 映到 solver 座）。盲注 / 座数 / button 沿用 `solver_cfg`（对齐 blueprint）。`stacks` 缺省
/// （旧 driver / 无 players 字段）→ 退 `solver_cfg` 对称 100BB。短桌：非发牌座的条目是 driver
/// placeholder、跳过不读；幻影树座保持 solver 默认 100BB（开局即 fold，不影响底池）。
/// 脏数据（长度 / 发牌座 0 栈 / 溢出）→ `Err`。
fn real_stacks_config(
    req: &Request,
    solver_cfg: &TableConfig,
    scale: u64,
    smap: &SeatMap,
) -> Result<TableConfig, String> {
    let mut cfg = solver_cfg.clone();
    if req.stacks.is_empty() {
        return Ok(cfg); // 无真栈 → 对称 100BB（仍是合法的真栈解，只是不利用码深）。
    }
    if req.stacks.len() != N_SEATS {
        return Err("stacks_len".into());
    }
    let mut stacks = cfg.starting_stacks.clone();
    for (op_seat, &op_stack) in req.stacks.iter().enumerate() {
        let Some(tree) = smap.to_tree[op_seat] else {
            continue; // 非发牌座（短桌空座 / 等局玩家）：placeholder 不读。
        };
        let s = op_stack.checked_mul(scale).ok_or("stack_overflow")?;
        if s == 0 {
            return Err("zero_stack".into()); // 发牌座 0 栈 → 不解（边界，fold）。
        }
        stacks[tree as usize] = ChipAmount::new(s);
    }
    cfg.starting_stacks = stacks;
    Ok(cfg)
}

/// 手内稳定的 subgame solve 基 seed：hash(hole, button, my_seat, num_seats, blinds, base_seed)
/// —— **不含 actions / board**，故同一手多次决策同 seed → [`ResolveRoot::RoundStart`] 的街索引
/// ordinal 下同街多决策共享字节相同的 solve（§6 #2 一致性）。
fn hand_seed_for(req: &Request, base_seed: u64) -> u64 {
    let mut hasher = blake3::Hasher::new();
    for c in &req.hole {
        hasher.update(c.as_bytes());
    }
    hasher.update(&[req.button_seat, req.my_seat, req.num_seats]);
    hasher.update(&req.small_blind.to_le_bytes());
    hasher.update(&req.big_blind.to_le_bytes());
    if !req.dealt_seats.is_empty() && req.dealt_seats.len() != N_SEATS {
        // 短桌：占座不同 = 不同局面（满桌不掺、保旧 seed byte-equal）。
        hasher.update(&req.dealt_seats);
    }
    hasher.update(&base_seed.to_le_bytes());
    let d = hasher.finalize();
    u64::from_le_bytes(d.as_bytes()[..8].try_into().expect("blake3 ≥ 8 bytes"))
}

/// solver raise-to → OpenPoker raise-to：÷scale 四舍五入，夹进 [min_raise, max_raise]。
/// 无 raise 区间（不能加注）或区间不自洽（min > max，脏 valid_actions）→ None（caller 兜底）。
fn raise_to_op(to_solver: u64, scale: u64, valid: &ValidActions) -> Option<u64> {
    if !valid.can_raise {
        return None;
    }
    let (min, max) = (valid.min_raise?, valid.max_raise?);
    if min > max {
        // 脏区间：u64::clamp 在 min > max 时 panic，会杀死常驻进程（driver 此后每决策
        // fold = 永久弃牌机）。当作 raise 不可用 → caller safe_fallback（live 不能崩）。
        return None;
    }
    let mut op_to = (to_solver + scale / 2) / scale; // round-half-up
    op_to = op_to.clamp(min, max);
    Some(op_to)
}

fn action_label(a: &AbstractAction) -> String {
    match a {
        AbstractAction::Fold => "fold".into(),
        AbstractAction::Check => "check".into(),
        AbstractAction::Call { .. } => "call".into(),
        AbstractAction::Bet { ratio_label, .. } => {
            format!("bet{}pot", ratio_label.as_milli() as f64 / 1000.0)
        }
        AbstractAction::Raise { ratio_label, .. } => {
            format!("raise{}pot", ratio_label.as_milli() as f64 / 1000.0)
        }
        AbstractAction::AllIn { .. } => "allin".into(),
    }
}

/// per-decision 确定性 seed：hash(hole, board, actions, base_seed)。保混合策略 + 可复现。
fn sample_seed(req: &Request, base_seed: u64) -> u64 {
    let mut hasher = blake3::Hasher::new();
    for c in &req.hole {
        hasher.update(c.as_bytes());
    }
    for c in &req.board {
        hasher.update(c.as_bytes());
    }
    hasher.update(&[req.button_seat, req.my_seat, req.num_seats]);
    for h in &req.actions {
        hasher.update(&[h.seat]);
        hasher.update(h.action.as_bytes());
        hasher.update(&h.to.unwrap_or(0).to_le_bytes());
    }
    if !req.dealt_seats.is_empty() && req.dealt_seats.len() != N_SEATS {
        // 短桌：占座不同 = 不同局面（满桌不掺、保旧 seed byte-equal）。
        hasher.update(&req.dealt_seats);
    }
    hasher.update(&base_seed.to_le_bytes());
    let d = hasher.finalize();
    u64::from_le_bytes(d.as_bytes()[..8].try_into().expect("blake3 ≥ 8 bytes"))
}

// ===========================================================================
// blueprint 加载 + ready + stdio 主循环
// ===========================================================================

struct Args {
    checkpoint: PathBuf,
    bucket_table: PathBuf,
    reshape: String,
    postflop_cap: u8,
    seed: u64,
    /// 缺口②实时搜索：`Some` = `--search` 开（postflop 触发面 re-solve 真码深子博弈）；`None`
    /// （默认）= 纯 blueprint（旧行为 byte-equal）。其余 search 字段由 `--search-*` flag 填。
    search: Option<SubgameSearchConfig>,
    /// `--search-bucket-table`：子树 solve 用的独立桶表路径（[`SearchRuntime`] doc；`None` =
    /// 沿用 `--bucket-table`）。仅 `--search` 开时可设（同其余 `--search-*` 的拒静默 guard）。
    search_bucket_table: Option<PathBuf>,
    /// `--search-unanchored-prefix-reach on|off`（档一，[`SearchRuntime`] doc）：脱影子搜索 range
    /// 先验从 uniform 升级为已同步前缀 reach。**默认 `true`**（[`DEFAULT_SEARCH_UNANCHORED_PREFIX_REACH`]，
    /// §5.1 实测拍板）；`off` 显式关（A/B 对照臂 / 回退）。
    search_unanchored_prefix_reach: bool,
    /// `--search-unanchored-cross-street on|off`（档二′-跨街复用，[`SearchRuntime`] doc）：复用上一街
    /// 已解 unanchored 子树 σ 算本街后验 range。**默认 `true`**（[`DEFAULT_SEARCH_UNANCHORED_CROSS_STREET`]，
    /// 决策级 A/B + 机制拍板，同档一）；`off` 显式关（A/B 对照臂 / 回退）。
    search_unanchored_cross_street: bool,
    /// `--search-flop-prefer-blueprint on|off`（[`SearchRuntime`] doc）：仅 flop 锚定面优先 blueprint
    /// （脱影子 flop 仍搜索）。**默认 `false`**（[`DEFAULT_SEARCH_FLOP_PREFER_BLUEPRINT`]，旧行为
    /// byte-equal）；`on` 显式开。
    search_flop_prefer_blueprint: bool,
    /// `--exploit`（叠加剥削 Tier 2）：`Some` = 开（进程内画像 → 翻前 range 宽度先验）。**仅 `--search`
    /// 开时可设**（拒静默 guard）。`None`（默认）= 关，全程与现网 byte-equal。子旗 `--exploit-min-hands`
    /// / `--exploit-strength` / `--exploit-converge-se` / `--exploit-converge-drift` 填 [`ExploitConfig`]。
    exploit: Option<ExploitConfig>,
    /// `--debug-log`：打印决策流水线 + range / solve 中间数据到 **stderr**（与 `--search` 正交，
    /// blueprint 路径也打）。`false`（默认）= 静默、零开销；on 也不动 stdout（IPC byte-equal）。
    debug_log: bool,
}

#[derive(Serialize)]
struct ReadyLine {
    ready: bool,
    update_count: u64,
    reshape: String,
    n_seats: usize,
    /// 缺口②：是否开了实时搜索（driver 据此知道 source 可能是 `search` / `search_fold:*`）。
    search: bool,
}

fn reshape_profile(
    reshape: &str,
    cap: u8,
) -> Result<
    (
        StreetActionAbstraction,
        poker::training::nlhe_betting_tree::BettingAbstractionRules,
    ),
    String,
> {
    Ok(match reshape {
        "none" => first_small_6max(cap),
        "nolimp" => {
            let (a, mut r) = first_small_6max(cap);
            r.no_open_limp = true;
            (a, r)
        }
        "preopen" => first_small_preopen_6max(cap),
        "preopen-small" => first_small_preopen_small_6max(cap),
        other => return Err(format!("unknown reshape {other}")),
    })
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[openpoker_advisor] fatal: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    // 调试日志（--debug-log）：进程级开关 + 透传子博弈层（range / solve 中间数据）。仅 stderr。
    DEBUG.store(args.debug_log, std::sync::atomic::Ordering::Relaxed);
    set_subgame_debug(args.debug_log);
    if args.debug_log {
        eprintln!(
            "[dbg-advisor] --debug-log ON：打印决策流水线 + range/solve 中间数据到 stderr（stdout/IPC 字节不变）"
        );
    }
    if !matches!(args.postflop_cap, 2..=4) {
        return Err(format!(
            "--postflop-cap must be 2/3/4, got {}",
            args.postflop_cap
        ));
    }
    let table = Arc::new(BucketTable::open(&args.bucket_table).map_err(|e| {
        format!(
            "BucketTable::open({}) failed: {e:?}",
            args.bucket_table.display()
        )
    })?);
    let (abs, rules) = reshape_profile(&args.reshape, args.postflop_cap)?;
    let game = SimplifiedNlheGame::new_with_abstraction(
        Arc::clone(&table),
        TableConfig::default_6max_100bb(),
        abs,
        rules,
    )
    .map_err(|e| format!("build six-max game failed: {e:?}"))?;
    let trainer =
        DenseNlheEsMccfrTrainer::load_checkpoint(&args.checkpoint, game).map_err(|e| {
            format!(
                "load checkpoint {} failed: {e:?}",
                args.checkpoint.display()
            )
        })?;
    let game = trainer.game();
    let abstraction = game.abstraction().clone();

    // --search-bucket-table：子树 solve 的独立桶表（SearchRuntime doc）。启动期 open（坏路径
    // 早 fail，不等首次搜索才炸）；blueprint 表 / range 估计不受影响。
    let search_rt: Option<SearchRuntime> = match args.search {
        Some(cfg) => {
            let bucket_table = match &args.search_bucket_table {
                Some(p) => Some(Arc::new(BucketTable::open(p).map_err(|e| {
                    format!("BucketTable::open({}) failed: {e:?}", p.display())
                })?)),
                None => None,
            };
            Some(SearchRuntime {
                cfg,
                bucket_table,
                unanchored_prefix_reach: args.search_unanchored_prefix_reach,
                unanchored_cross_street: args.search_unanchored_cross_street,
                flop_prefer_blueprint: args.search_flop_prefer_blueprint,
                // 叠加剥削：--exploit 开 → 进程内空表 Profiler（只用本进程数据，不依赖过往）。
                exploit: args.exploit.map(|cfg| RefCell::new(Profiler::new(cfg))),
            })
        }
        None => None,
    };

    let ready = ReadyLine {
        ready: true,
        update_count: trainer.update_count(),
        reshape: args.reshape.clone(),
        n_seats: N_SEATS,
        search: search_rt.is_some(),
    };
    if let Some(rt) = &search_rt {
        let scfg = &rt.cfg;
        eprintln!(
            "[openpoker_advisor] search ON: trigger={:?} iters={} time_budget={:?} lcfr={} deep_menu={} live_traversers={} max_nodes={} range_mix={} solve_threads={} unanchored_prefix_reach={} unanchored_cross_street={} flop_prefer_blueprint={} bucket_table={}",
            scfg.trigger, scfg.iterations, scfg.time_budget, scfg.lcfr, scfg.deep_menu, scfg.live_traversers, scfg.max_subtree_nodes, scfg.range_uniform_mix, scfg.solve_threads, rt.unanchored_prefix_reach, rt.unanchored_cross_street, rt.flop_prefer_blueprint,
            args.search_bucket_table.as_ref().map_or_else(|| "blueprint".to_string(), |p| p.display().to_string())
        );
        if let Some(cell) = &rt.exploit {
            let ec = *cell.borrow().cfg();
            eprintln!(
                "[openpoker_advisor] exploit ON (叠加剥削 Tier 2，翻前 range 宽度): min_hands={} strength_alpha={} converge_se={} converge_drift={} window={} mode={}（进程内画像、不依赖过往）",
                ec.min_hands, ec.strength_alpha, ec.converge_se, ec.converge_drift, ec.window,
                if ec.pfr_shape { "on(VPIP+PFR/CallBand·RaiseBand)" } else { "vpip(仅VPIP/byte-equal)" }
            );
        }
    }
    eprintln!(
        "[openpoker_advisor] ready reshape={} update_count={} search={}",
        ready.reshape, ready.update_count, ready.search
    );
    let mut stdout = std::io::stdout();
    writeln!(
        stdout,
        "{}",
        serde_json::to_string(&ready).map_err(|e| e.to_string())?
    )
    .map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())?;

    let strategy_fn = |info: &InfoSetId, _n: usize| -> Vec<f64> { trainer.average_strategy(*info) };

    // within-round solve 缓存（进程常驻，跨请求）：同手同街第二次决策命中 → 复用 solve 只重
    // 导航——恢复「每轮恰好一个 solve」一致性（time_budget anytime 下重解会读不同均衡）+
    // mid-round wall ≈ 0。容量 1，跨手 / 跨街 key 自然替换。
    let mut solve_cache = SubgameSolveCache::new();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        // 信封判别（RequestEnvelope doc）：observe = 每手结束的画像观测（喂 Profiler、返回遥测、
        // driver 丢弃）；prewarm = 街起点预热（不出动作、只暖 solve 缓存）；否则决策请求。
        // 必有一行响应（IPC 锁步）。
        let envelope = serde_json::from_str::<RequestEnvelope>(&line).ok();
        let is_observe = envelope.as_ref().map(|e| e.observe).unwrap_or(false);
        let is_prewarm = envelope.as_ref().map(|e| e.prewarm).unwrap_or(false);
        let resp = if is_observe {
            // 叠加剥削：吃一手 observe 进进程内 Profiler（仅 --exploit 开时有；脏数据静默跳过、
            // 不崩）。observe 永不出动作——返回遥测 ack（driver `_drain_pending` 丢弃）。
            if let Some(cell) = search_rt.as_ref().and_then(|rt| rt.exploit.as_ref()) {
                if let Ok(obs) = serde_json::from_str::<ObserveHand>(&line) {
                    cell.borrow_mut().observe_hand(&obs);
                }
            }
            Response {
                action: "none".into(),
                source: "observe:ack".into(),
                ..Default::default()
            }
        } else {
            match serde_json::from_str::<Request>(&line) {
                Ok(req) if is_prewarm => prewarm(
                    game,
                    &strategy_fn,
                    &req,
                    args.seed,
                    search_rt.as_ref(),
                    &mut solve_cache,
                ),
                Ok(req) => decide(
                    game,
                    &abstraction,
                    &strategy_fn,
                    &req,
                    args.seed,
                    search_rt.as_ref(),
                    &mut solve_cache,
                ),
                // 解析失败也不崩：出 fold（最保守；没有 valid 信息可用）。
                Err(e) => Response {
                    action: "fold".into(),
                    source: format!("fallback:bad_request_json:{e}"),
                    ..Default::default()
                },
            }
        };
        writeln!(
            stdout,
            "{}",
            serde_json::to_string(&resp).map_err(|e| e.to_string())?
        )
        .map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut checkpoint: Option<PathBuf> = None;
    let mut bucket_table: Option<PathBuf> = None;
    let mut reshape = "preopen".to_string();
    let mut postflop_cap = 3u8;
    let mut seed: u64 = 0;
    // 缺口② 实时搜索 flag（仅 --search 开时打包成 SubgameSearchConfig）。
    let mut search_on = false;
    let mut search_iters: Option<u64> = None;
    let mut search_trigger = SearchTrigger::FlopFirstUnraised;
    let mut search_time_budget_ms: Option<u64> = None;
    let mut search_lcfr = false;
    let mut search_deep_menu = false;
    let mut search_live_traversers = false;
    let mut search_max_nodes: usize = SubgameSearchConfig::default().max_subtree_nodes;
    let mut search_range_uniform_mix: Option<f64> = None;
    let mut search_bucket_table: Option<PathBuf> = None;
    let mut search_solve_threads: Option<usize> = None;
    let mut search_unanchored_prefix_reach: Option<bool> = None;
    let mut search_unanchored_cross_street: Option<bool> = None;
    let mut search_flop_prefer_blueprint: Option<bool> = None;
    // 叠加剥削（--exploit*，仅 --search 开时生效）。
    let mut exploit_on = false;
    let mut exploit_min_hands: Option<u32> = None;
    let mut exploit_strength: Option<f64> = None;
    let mut exploit_converge_se: Option<f64> = None;
    let mut exploit_converge_drift: Option<f64> = None;
    let mut exploit_pfr_shape = false;
    let mut debug_log = false;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--checkpoint" => checkpoint = Some(PathBuf::from(next_val(&mut it, &arg)?)),
            "--bucket-table" => bucket_table = Some(PathBuf::from(next_val(&mut it, &arg)?)),
            "--reshape" => reshape = next_val(&mut it, &arg)?,
            "--postflop-cap" => {
                postflop_cap = next_val(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad cap: {e}"))?
            }
            "--seed" => {
                let raw = next_val(&mut it, &arg)?;
                seed = raw
                    .strip_prefix("0x")
                    .map(|h| u64::from_str_radix(h, 16))
                    .unwrap_or_else(|| raw.parse())
                    .map_err(|e| format!("bad seed: {e}"))?;
            }
            "--search" => search_on = true,
            "--search-iterations" => {
                search_iters = Some(
                    next_val(&mut it, &arg)?
                        .parse()
                        .map_err(|e| format!("bad search-iterations: {e}"))?,
                )
            }
            "--search-trigger" => {
                let v = next_val(&mut it, &arg)?;
                search_trigger = match v.as_str() {
                    "flop-first-unraised" => SearchTrigger::FlopFirstUnraised,
                    "all-postflop" => SearchTrigger::AllPostflop,
                    other => return Err(format!("unknown --search-trigger {other}")),
                };
            }
            "--search-time-budget-ms" => {
                search_time_budget_ms = Some(
                    next_val(&mut it, &arg)?
                        .parse()
                        .map_err(|e| format!("bad search-time-budget-ms: {e}"))?,
                )
            }
            "--search-lcfr" => search_lcfr = true,
            "--search-deep-menu" => search_deep_menu = true,
            "--search-live-traversers" => search_live_traversers = true,
            "--search-max-nodes" => {
                search_max_nodes = next_val(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad search-max-nodes: {e}"))?
            }
            "--search-range-uniform-mix" => {
                let v: f64 = next_val(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad search-range-uniform-mix: {e}"))?;
                if !(0.0..=1.0).contains(&v) {
                    return Err(format!("--search-range-uniform-mix 须在 [0,1]，得 {v}"));
                }
                search_range_uniform_mix = Some(v);
            }
            "--search-bucket-table" => {
                search_bucket_table = Some(PathBuf::from(next_val(&mut it, &arg)?))
            }
            "--search-solve-threads" => {
                let v: usize = next_val(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad search-solve-threads: {e}"))?;
                if v == 0 {
                    return Err("--search-solve-threads 须 ≥1（1 = 单线程既有行为）".to_string());
                }
                search_solve_threads = Some(v);
            }
            "--search-unanchored-prefix-reach" => {
                // 默认开（DEFAULT_SEARCH_UNANCHORED_PREFIX_REACH）；取值 on|off 显式覆盖（A/B 用）。
                let v = next_val(&mut it, &arg)?;
                search_unanchored_prefix_reach = Some(match v.as_str() {
                    "on" | "true" | "1" => true,
                    "off" | "false" | "0" => false,
                    other => {
                        return Err(format!(
                            "--search-unanchored-prefix-reach 须 on|off，得 {other}"
                        ))
                    }
                });
            }
            "--search-unanchored-cross-street" => {
                // 默认开（DEFAULT_SEARCH_UNANCHORED_CROSS_STREET）；取值 on|off 显式覆盖（A/B 用）。
                let v = next_val(&mut it, &arg)?;
                search_unanchored_cross_street = Some(match v.as_str() {
                    "on" | "true" | "1" => true,
                    "off" | "false" | "0" => false,
                    other => {
                        return Err(format!(
                            "--search-unanchored-cross-street 须 on|off，得 {other}"
                        ))
                    }
                });
            }
            "--search-flop-prefer-blueprint" => {
                // 默认关（DEFAULT_SEARCH_FLOP_PREFER_BLUEPRINT）；取值 on|off 显式覆盖。
                let v = next_val(&mut it, &arg)?;
                search_flop_prefer_blueprint = Some(match v.as_str() {
                    "on" | "true" | "1" => true,
                    "off" | "false" | "0" => false,
                    other => {
                        return Err(format!(
                            "--search-flop-prefer-blueprint 须 on|off，得 {other}"
                        ))
                    }
                });
            }
            "--debug-log" => debug_log = true,
            // —— 叠加剥削（Tier 2）：--exploit 三态总开关（on=VPIP+PFR 形状 / vpip=仅 VPIP，与
            // 现网逐位 byte-equal / off=关），子旗调 ExploitConfig ——
            "--exploit" => match next_val(&mut it, &arg)?.as_str() {
                "on" => {
                    exploit_on = true;
                    exploit_pfr_shape = true; // VPIP + PFR-aware 形状（CallBand/RaiseBand）
                }
                "vpip" => {
                    exploit_on = true;
                    exploit_pfr_shape = false; // 仅 VPIP 宽度（TopK，与现网逐位 byte-equal）
                }
                "off" => {
                    exploit_on = false;
                    exploit_pfr_shape = false;
                }
                other => return Err(format!("--exploit 须 on|vpip|off，得 {other}")),
            },
            "--exploit-min-hands" => {
                exploit_min_hands = Some(
                    next_val(&mut it, &arg)?
                        .parse()
                        .map_err(|e| format!("bad exploit-min-hands: {e}"))?,
                )
            }
            "--exploit-strength" => {
                let v: f64 = next_val(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad exploit-strength: {e}"))?;
                if !(0.0..=1.0).contains(&v) {
                    return Err(format!("--exploit-strength 须在 [0,1]，得 {v}"));
                }
                exploit_strength = Some(v);
            }
            "--exploit-converge-se" => {
                let v: f64 = next_val(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad exploit-converge-se: {e}"))?;
                if !(0.0..=1.0).contains(&v) {
                    return Err(format!("--exploit-converge-se 须在 [0,1]，得 {v}"));
                }
                exploit_converge_se = Some(v);
            }
            "--exploit-converge-drift" => {
                let v: f64 = next_val(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad exploit-converge-drift: {e}"))?;
                if !(0.0..=1.0).contains(&v) {
                    return Err(format!("--exploit-converge-drift 须在 [0,1]，得 {v}"));
                }
                exploit_converge_drift = Some(v);
            }
            other => return Err(format!("unknown arg {other}")),
        }
    }
    // --search-* flag 仅在 --search 开时生效，避免「设了参数却忘开搜索」静默跑 blueprint。
    let search = if search_on {
        // 设了 time_budget 而未显式给 --search-iterations → iterations 抬到 u64::MAX 当纯安全
        // 上界、求解全由墙钟截断（老默认 1000 在 ~10–30ms 处先撞迭代上限，预算永远绑不上 =
        // 静默失效；2026-06-11 live searchon50 实撞）。LCFR period 不随之爆炸：solve_subgame
        // 在 budgeted 下 cap period（subgame.rs lcfr_period）。
        let iterations = search_iters.unwrap_or(if search_time_budget_ms.is_some() {
            u64::MAX
        } else {
            1000
        });
        Some(SubgameSearchConfig {
            iterations,
            max_subtree_nodes: search_max_nodes,
            trigger: search_trigger,
            lcfr: search_lcfr,
            time_budget: search_time_budget_ms.map(Duration::from_millis),
            // 缺口③：深码窄菜单——子树下注菜单收到单一 {1pot}（深码 / 多人解到终局控树，§2.1）。
            deep_menu: search_deep_menu,
            // 缺口①续（限时杠杆②）：traverser 只轮子树根仍 Active 的座（弃牌/all-in 座零学习
            // 迭代跳过，同 wall 有效迭代 ×n_seats/n_active）。
            live_traversers: search_live_traversers,
            // range 先验平滑（2026-06-12 searchon50 修复，SubgameSearchConfig 字段 doc）：薄线
            // blueprint reach 噪声 range 会被无约束重解放大成 max-exploit（空气 99.98% 下注 /
            // 73% jam 实撞），λ 混合 uniform 给合法组合保底权重。生产默认开
            // （DEFAULT_SEARCH_RANGE_UNIFORM_MIX）；--search-range-uniform-mix 0 显式关。
            range_uniform_mix: search_range_uniform_mix.unwrap_or(DEFAULT_SEARCH_RANGE_UNIFORM_MIX),
            // 限时杠杆③：solve update 并行（SubgameSearchConfig::solve_threads doc）。同预算
            // update 数 ≈ ×核数；只助 solve 侧，建树仍单线程。默认 1 = 既有单线程 byte-equal。
            solve_threads: search_solve_threads.unwrap_or(1),
            // 解到终局（深码 / 多人 §2.1）：depth_limit / biased_leaf 均 false（默认）；
            // resolve_root / use_blueprint_range / seed 用默认（RoundStart / true / 固定基）。
            ..SubgameSearchConfig::default()
        })
    } else {
        if search_iters.is_some()
            || search_trigger != SearchTrigger::FlopFirstUnraised
            || search_time_budget_ms.is_some()
            || search_lcfr
            || search_deep_menu
            || search_live_traversers
            || search_max_nodes != SubgameSearchConfig::default().max_subtree_nodes
            || search_range_uniform_mix.is_some()
            || search_bucket_table.is_some()
            || search_solve_threads.is_some()
            || search_unanchored_prefix_reach.is_some()
            || search_unanchored_cross_street.is_some()
            || search_flop_prefer_blueprint.is_some()
        {
            return Err("设了 --search-* 参数但未开 --search（拒绝静默跑 blueprint）".to_string());
        }
        None
    };
    // --exploit* guard：子旗需 --exploit on|vpip；--exploit on|vpip 需 --search（剥削仅作用脱锚
    // 搜索路径）。pfr_shape 现由 --exploit 取值派生（off 时恒 false），不再单列入此 guard。
    if !exploit_on
        && (exploit_min_hands.is_some()
            || exploit_strength.is_some()
            || exploit_converge_se.is_some()
            || exploit_converge_drift.is_some())
    {
        return Err("设了 --exploit-* 子旗但 --exploit 非 on|vpip".to_string());
    }
    if exploit_on && search.is_none() {
        return Err("--exploit on|vpip 需配合 --search（剥削只挂脱锚搜索路径）".to_string());
    }
    let exploit = exploit_on.then(|| {
        let d = ExploitConfig::default();
        ExploitConfig {
            min_hands: exploit_min_hands.unwrap_or(d.min_hands),
            converge_se: exploit_converge_se.unwrap_or(d.converge_se),
            converge_drift: exploit_converge_drift.unwrap_or(d.converge_drift),
            strength_alpha: exploit_strength.unwrap_or(d.strength_alpha),
            window: d.window,
            pfr_shape: exploit_pfr_shape,
        }
    });
    Ok(Args {
        checkpoint: checkpoint.ok_or("缺 --checkpoint")?,
        bucket_table: bucket_table.ok_or("缺 --bucket-table")?,
        reshape,
        postflop_cap,
        seed,
        search,
        search_bucket_table,
        exploit,
        // 默认开（§5.1 实测拍板）；--search-unanchored-prefix-reach off 显式关。
        search_unanchored_prefix_reach: search_unanchored_prefix_reach
            .unwrap_or(DEFAULT_SEARCH_UNANCHORED_PREFIX_REACH),
        // 默认开（决策级 A/B + 机制拍板，同档一）；--search-unanchored-cross-street off 显式关。
        search_unanchored_cross_street: search_unanchored_cross_street
            .unwrap_or(DEFAULT_SEARCH_UNANCHORED_CROSS_STREET),
        // 默认关（旧行为 byte-equal）；--search-flop-prefer-blueprint on 显式开。
        search_flop_prefer_blueprint: search_flop_prefer_blueprint
            .unwrap_or(DEFAULT_SEARCH_FLOP_PREFER_BLUEPRINT),
        debug_log,
    })
}

fn next_val(it: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{name} 需要一个值"))
}

// ===========================================================================
// 测试（stdio decide：canned 6-max 请求 → 合法输出 + 结构 gap 兜底；vultr 跑）
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use poker::BucketConfig;

    /// 把纯 config 包成 [`SearchRuntime`]（子树桶表 = None，沿用 blueprint 表——既有搜索测试
    /// 全不换表，行为与改前 byte-equal）。前缀 reach 取关（uniform）= 既有脱影子行为（生产默认已
    /// 改开，测试辅助仍取关以保持既有用例 byte-equal；开档一用 [`rt_prefix_reach`]）。
    fn rt(cfg: SubgameSearchConfig) -> SearchRuntime {
        SearchRuntime {
            cfg,
            bucket_table: None,
            unanchored_prefix_reach: false,
            unanchored_cross_street: false,
            flop_prefer_blueprint: false,
            exploit: None,
        }
    }

    /// 同 [`rt`] 但开档一前缀 reach（脱影子 range 先验 = 已同步前缀，A/B 用）。
    fn rt_prefix_reach(cfg: SubgameSearchConfig) -> SearchRuntime {
        SearchRuntime {
            cfg,
            bucket_table: None,
            unanchored_prefix_reach: true,
            unanchored_cross_street: false,
            flop_prefer_blueprint: false,
            exploit: None,
        }
    }

    /// 同 [`rt_prefix_reach`] 但**加开**档二′-跨街复用（脱影子 range 先验 = 上一街已解子树 σ，A/B 用）。
    fn rt_cross_street(cfg: SubgameSearchConfig) -> SearchRuntime {
        SearchRuntime {
            cfg,
            bucket_table: None,
            unanchored_prefix_reach: true,
            unanchored_cross_street: true,
            flop_prefer_blueprint: false,
            exploit: None,
        }
    }

    /// 同 [`rt`] 但开「flop 锚定面优先 blueprint」（脱影子档位仍取关，与 [`rt`] 一致——此旗只抑制
    /// flop 锚定搜索）。
    fn rt_flop_prefer_blueprint(cfg: SubgameSearchConfig) -> SearchRuntime {
        SearchRuntime {
            cfg,
            bucket_table: None,
            unanchored_prefix_reach: false,
            unanchored_cross_street: false,
            flop_prefer_blueprint: true,
            exploit: None,
        }
    }

    // N=2 redirect（debug 建树快、6 座不变）；preopen 含 0.5/1.0 开池档。
    fn preopen_game() -> SimplifiedNlheGame {
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ));
        let (abs, rules) = first_small_preopen_6max(2);
        SimplifiedNlheGame::new_with_abstraction(
            table,
            TableConfig::default_6max_100bb(),
            abs,
            rules,
        )
        .expect("preopen game")
    }
    fn nolimp_game() -> SimplifiedNlheGame {
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ));
        let (a, mut r) = first_small_6max(2);
        r.no_open_limp = true;
        SimplifiedNlheGame::new_with_abstraction(table, TableConfig::default_6max_100bb(), a, r)
            .expect("nolimp game")
    }

    fn full_valid() -> ValidActions {
        ValidActions {
            can_check: false,
            can_call: true,
            can_raise: true,
            min_raise: Some(40),
            max_raise: Some(2000),
        }
    }

    fn is_legal(resp: &Response, valid: &ValidActions) -> bool {
        match resp.action.as_str() {
            "fold" => true,
            "check" => valid.can_check,
            "call" => valid.can_call,
            "all_in" => true,
            "raise" => match (resp.amount, valid.min_raise, valid.max_raise) {
                (Some(a), Some(lo), Some(hi)) => a >= lo && a <= hi,
                _ => false,
            },
            _ => false,
        }
    }

    /// 脏 valid_actions（min_raise > max_raise）不能 panic 杀死常驻进程（「live 不能崩」）：
    /// `raise_to_op` 须返回 None（caller 走 safe_fallback）。修前 `u64::clamp(min>max)` panic，
    /// driver 此后每决策 fold = 永久弃牌机。
    #[test]
    fn raise_to_op_inconsistent_interval_no_panic() {
        let dirty = ValidActions {
            can_check: false,
            can_call: true,
            can_raise: true,
            min_raise: Some(400),
            max_raise: Some(300),
        };
        assert_eq!(raise_to_op(1650, 5, &dirty), None);
        // 自洽区间行为不变：1650 solver ÷5 = 330 op，落在 [20, 1840] 内原样返回。
        let ok = ValidActions {
            can_check: true,
            can_call: false,
            can_raise: true,
            min_raise: Some(20),
            max_raise: Some(1840),
        };
        assert_eq!(raise_to_op(1650, 5, &ok), Some(330));
    }

    /// 回归（生产实采，2026-06-14）：TT@SB 在 UTG-open → SB-3bet → UTG-4bet 这条标准 preflop
    /// 线上**不再** `lockstep_drift`。修前：小 3bet 被 [`map_off_tree`] 选 0.5pot 档、
    /// [`project_tag_onto`] 误塌 AllIn → 影子比真实多一个 all-in → 影子提前进 Showdown、真实仍
    /// 轮到 hero → `current_player` 失同步 → fallback 盲弃 TT。修后：被剪的 0.5pot 档向上投到
    /// 合法的 1.0pot → 重放 Ok、决策走 blueprint。lockstep 只依赖下注树结构 + 固定 seed（与
    /// blueprint 策略 / 桶表无关），故 stub 桶 + uniform 策略即可锁死。遍历生产可能 cap。
    ///
    /// [`map_off_tree`]: poker::abstraction::action::ActionAbstraction::map_off_tree
    /// [`project_tag_onto`]: poker::training::blueprint_advisor::project_tag_onto
    #[test]
    fn three_bet_four_bet_no_lockstep_drift() {
        let mk_req = || Request {
            hole: vec!["Td".into(), "Th".into()],
            board: vec![],
            button_seat: 0,
            my_seat: 1,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                HistAction {
                    seat: 3,
                    action: "raise".into(),
                    to: Some(56),
                },
                HistAction {
                    seat: 4,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 5,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 0,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 1,
                    action: "raise".into(),
                    to: Some(188),
                },
                HistAction {
                    seat: 2,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 3,
                    action: "raise".into(),
                    to: Some(396),
                },
            ],
            valid: ValidActions {
                can_check: false,
                can_call: true,
                can_raise: true,
                min_raise: Some(604),
                max_raise: Some(2402),
            },
            stacks: vec![1588, 2402, 1241, 2000, 108265, 1340],
            dealt_seats: vec![],
            names: Default::default(),
        };
        let solver_cfg = TableConfig::default_6max_100bb();
        let scale = solver_cfg.big_blind.as_u64() / 20; // 5
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];

        for cap in [2u8, 3, 4] {
            let table = Arc::new(BucketTable::stub_for_postflop(
                BucketConfig::default_500_500_500(),
            ));
            let (abs, rules) = first_small_preopen_6max(cap);
            let game = SimplifiedNlheGame::new_with_abstraction(
                table,
                TableConfig::default_6max_100bb(),
                abs,
                rules,
            )
            .expect("preopen game");
            let abstraction = game.abstraction().clone();
            let req = mk_req();
            let smap = seat_map(&req).expect("seat_map");
            let my = SeatId(smap.tree_seat(req.my_seat).unwrap());

            // lockstep 不再 drift。
            assert!(
                lockstep_replay(
                    &game,
                    &solver_cfg,
                    &smap,
                    &req,
                    scale,
                    req.board.len(),
                    Some(my)
                )
                .is_ok(),
                "cap{cap}: 标准 3bet/4bet 线 lockstep 不该 drift"
            );
            // 端到端：决策走 blueprint（不再 fallback 盲弃），且合法。
            let resp = decide(
                &game,
                &abstraction,
                &uniform,
                &req,
                1,
                None,
                &mut SubgameSolveCache::new(),
            );
            assert_eq!(
                resp.source, "blueprint",
                "cap{cap}: 应走 blueprint，得 {resp:?}"
            );
            assert!(is_legal(&resp, &req.valid), "cap{cap}: 须合法，得 {resp:?}");
        }
    }

    /// folds-to-BTN：UTG/HJ/CO fold → BTN(我) 决策。faithful 路径出**合法** raise/fold
    /// （preopen 开池位无 limp）+ source=blueprint。
    #[test]
    fn folds_to_btn_blueprint_legal() {
        let game = preopen_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // OpenPoker button=0；UTG=(0+3)%6=3, HJ=4, CO=5 先 fold；my_seat=0(BTN)。
        let req = Request {
            hole: vec!["Ah".into(), "Kd".into()],
            board: vec![],
            button_seat: 0,
            my_seat: 0,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                HistAction {
                    seat: 3,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 4,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 5,
                    action: "fold".into(),
                    to: None,
                },
            ],
            valid: full_valid(),
            stacks: vec![],
            dealt_seats: vec![],
            names: Default::default(),
        };
        let resp = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0xC0FFEE,
            None,
            &mut SubgameSolveCache::new(),
        );
        assert!(is_legal(&resp, &req.valid), "BTN 决策应合法，得 {resp:?}");
        assert_eq!(
            resp.source, "blueprint",
            "faithful 路径应由 blueprint 驱动，得 {resp:?}"
        );
        // 确定性：同输入同输出。
        let again = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0xC0FFEE,
            None,
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(resp, again, "同 seed 同输入应确定性");
    }

    /// 结构性 gap：对手 UTG open-limp（call to=20）→ nolimp blueprint 影子无对应节点 →
    /// preflop 走 [`limp_heuristic`] 矩阵（2026-06-12）。**没人加注**分支三格：S 档 iso-raise
    /// to (4+limper 数)×BB、非 S 档 BB 免费 check、非 S 档非盲位 fold。
    #[test]
    fn opponent_open_limp_heuristic_no_raise_cells() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // my_seat=4(HJ)；UTG(3) open-limp call to=20 → 我(HJ) 决策时重放撞 gap。
        let make = |hole: [&str; 2], valid: ValidActions| Request {
            hole: vec![hole[0].into(), hole[1].into()],
            board: vec![],
            button_seat: 0,
            my_seat: 4,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![HistAction {
                seat: 3,
                action: "call".into(),
                to: Some(20),
            }],
            valid,
            stacks: vec![],
            dealt_seats: vec![],
            names: Default::default(),
        };
        let decide_one = |req: &Request| {
            decide(
                &game,
                &abs,
                &uniform,
                req,
                1,
                None,
                &mut SubgameSolveCache::new(),
            )
        };
        // S 档（AKo）→ iso-raise to (4+1)×20=100。
        let req = make(["Ah", "Kd"], full_valid());
        let resp = decide_one(&req);
        assert_eq!(resp.source, "limp_heuristic:raise", "得 {resp:?}");
        assert_eq!(resp.amount, Some(100), "iso 尺寸 (4+1)×BB，得 {resp:?}");
        assert!(is_legal(&resp, &req.valid), "须合法，得 {resp:?}");
        // 非 S 档（72o）非盲位（can_check=false）→ fold。
        let resp = decide_one(&make(["7h", "2d"], full_valid()));
        assert_eq!(resp.source, "limp_heuristic:fold", "得 {resp:?}");
        assert_eq!(resp.action, "fold");
        // 非 S 档 BB（can_check=true）→ 免费 check，绝不 fold。
        let bb_valid = ValidActions {
            can_check: true,
            ..full_valid()
        };
        let resp = decide_one(&make(["7h", "2d"], bb_valid));
        assert_eq!(resp.source, "limp_heuristic:check", "得 {resp:?}");
        assert_eq!(resp.action, "check");
        // 历史无 limp → 启发式不接（None 防御）；构造不出无 limp 的 preflop 结构 gap，
        // 直接钉 limp_heuristic 单元行为。
        let mut nl = make(["Ah", "Kd"], full_valid());
        nl.actions.clear();
        assert!(
            limp_heuristic(&nl, [parse_card("Ah").unwrap(), parse_card("Kd").unwrap()]).is_none(),
            "无 limp 历史须返回 None → 维持原兜底"
        );
    }

    /// [`limp_heuristic`] **有人加注**分支（含 hero raise 后被 re-raise 的递归终止格）：
    /// P 档 call、S∖P fold、烂牌 fold。
    #[test]
    fn opponent_limp_raise_heuristic_vs_raise_cells() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // UTG(3) limp → CO(5) raise to 120 → 我(BTN=0) 面对加注。
        let make = |hole: [&str; 2]| Request {
            hole: vec![hole[0].into(), hole[1].into()],
            board: vec![],
            button_seat: 0,
            my_seat: 0,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                HistAction {
                    seat: 3,
                    action: "call".into(),
                    to: Some(20),
                },
                HistAction {
                    seat: 4,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 5,
                    action: "raise".into(),
                    to: Some(120),
                },
            ],
            valid: full_valid(),
            stacks: vec![],
            dealt_seats: vec![],
            names: Default::default(),
        };
        let decide_one = |req: &Request| {
            decide(
                &game,
                &abs,
                &uniform,
                req,
                1,
                None,
                &mut SubgameSolveCache::new(),
            )
        };
        // P 档（AA / JJ / AQ，2026-06-14 补 JJ/AQ）→ call（不设金额 cap）。
        for hole in [["As", "Ad"], ["Jh", "Jc"], ["Ah", "Qd"]] {
            let resp = decide_one(&make(hole));
            assert_eq!(resp.source, "limp_heuristic:call", "{hole:?} 得 {resp:?}");
            assert_eq!(resp.action, "call", "{hole:?}");
        }
        // S∖P（TT）→ fold（iso 档面对加注不继续）。
        let resp = decide_one(&make(["Th", "Tc"]));
        assert_eq!(resp.source, "limp_heuristic:fold", "得 {resp:?}");
        // 烂牌（94o）→ fold。
        let resp = decide_one(&make(["9h", "4c"]));
        assert_eq!(resp.source, "limp_heuristic:fold", "得 {resp:?}");
    }

    /// 兜底「别扔好牌」地板（用户 2026-06-14）：面对下注（!can_check && can_call）时
    /// preflop AA/KK/QQ/JJ/AK/AQ → call、postflop（接近）坚果 → call；其余 fold；能免费 check
    /// → check（地板不触发）。直接钉 [`fallback_with_floor`] / [`current_board_nuttiness`]
    /// 单元行为（纯函数，确定性）。
    #[test]
    fn fallback_floor_premium_and_nut_call() {
        let card = |s: &str| parse_card(s).unwrap();
        let hole = |a: &str, b: &str| [card(a), card(b)];
        // 面对下注：不能免费 check、能 call。
        let facing_bet = ValidActions {
            can_check: false,
            can_call: true,
            can_raise: true,
            min_raise: Some(40),
            max_raise: Some(2000),
        };
        // 免费 check：能 check。
        let free = ValidActions {
            can_check: true,
            ..facing_bet.clone()
        };

        // —— preflop（board 空）——
        // AA / KK / QQ / JJ / AKs / AKo / AQs / AQo 面对下注 → call（:premium_call）。
        // （2026-06-14 补 JJ/AQ：原只 AA/KK/QQ/AK，fallback/limp 路径白弃 JJ/AQ。）
        for (a, b) in [
            ("As", "Ad"),
            ("Kh", "Kc"),
            ("Qh", "Qs"),
            ("Jh", "Jc"),
            ("Ah", "Kh"),
            ("Ad", "Ks"),
            ("Ah", "Qd"),
            ("As", "Qs"),
        ] {
            let r = fallback_with_floor(&facing_bet, hole(a, b), &[], None, "fallback:x".into());
            assert_eq!(r.action, "call", "{a}{b} 面对下注须 call，得 {r:?}");
            assert_eq!(
                r.source, "fallback:x:premium_call",
                "{a}{b} source，得 {r:?}"
            );
        }
        // 非 premium（TT / KQo / 72o）面对下注 → fold（前缀不变、无后缀）。
        for (a, b) in [("Th", "Tc"), ("Kh", "Qd"), ("7h", "2d")] {
            let r = fallback_with_floor(&facing_bet, hole(a, b), &[], None, "fallback:x".into());
            assert_eq!(r.action, "fold", "{a}{b} 非 premium 须 fold，得 {r:?}");
            assert_eq!(r.source, "fallback:x", "{a}{b} 不该加后缀，得 {r:?}");
        }
        // AA 但能免费 check → check（地板只改 fold，不为它主动下注）。
        let r = fallback_with_floor(&free, hole("As", "Ad"), &[], None, "fallback:x".into());
        assert_eq!(r.action, "check", "免费局面须 check，得 {r:?}");
        assert_eq!(r.source, "fallback:x", "免费 check 不加后缀，得 {r:?}");

        // —— postflop ——
        // 河牌皇家同花顺 = 绝对坚果（nuttiness == 1.0）→ call（:nut_call），且 source 前缀保留。
        let royal_board = [card("Ah"), card("Kh"), card("Qh"), card("2c"), card("7d")];
        let royal_hole = hole("Jh", "Th");
        assert!(
            (current_board_nuttiness(royal_hole, &royal_board) - 1.0).abs() < 1e-9,
            "皇家同花顺坚果度须 = 1.0"
        );
        let r = fallback_with_floor(
            &facing_bet,
            royal_hole,
            &royal_board,
            None,
            "search_giveup:unsolved:foo".into(),
        );
        assert_eq!(r.action, "call", "坚果面对下注须 call，得 {r:?}");
        assert_eq!(r.source, "search_giveup:unsolved:foo:nut_call", "得 {r:?}");
        assert!(
            r.source.starts_with("search_giveup:"),
            "search 前缀须保留（driver 分桶不变），得 {r:?}"
        );
        // 同花面板上的空气（无对无听）= 远离坚果 → fold。
        let trash_hole = hole("2s", "8d");
        assert!(
            current_board_nuttiness(trash_hole, &royal_board) < NUT_CALL_THRESHOLD,
            "空气坚果度须 < 阈值"
        );
        let r = fallback_with_floor(
            &facing_bet,
            trash_hole,
            &royal_board,
            None,
            "fallback:y".into(),
        );
        assert_eq!(r.action, "fold", "空气面对下注须 fold，得 {r:?}");
        assert_eq!(r.source, "fallback:y", "未命中不加后缀，得 {r:?}");
        // flop 顶 set（A-K-Q rainbow 上的 AA）= 接近坚果（≥95%）→ call。
        let set_board = [card("Ah"), card("Kd"), card("Qc")];
        let set_hole = hole("As", "Ac");
        assert!(
            current_board_nuttiness(set_hole, &set_board) >= NUT_CALL_THRESHOLD,
            "broadway 顶 set 须 ≥ 阈值，得 {}",
            current_board_nuttiness(set_hole, &set_board)
        );
        let r = fallback_with_floor(&facing_bet, set_hole, &set_board, None, "fallback:z".into());
        assert_eq!(r.action, "call", "顶 set 接近坚果须 call，得 {r:?}");
        assert_eq!(r.source, "fallback:z:nut_call", "得 {r:?}");
    }

    /// 坚果度阈值 2026-06-15 下调为 **>93%** + 底池赔率 floor（用户报的 4h5h@8h6d7c7h3d 河牌 giveup）。
    #[test]
    fn fallback_floor_93pct_and_pot_odds() {
        let card = |s: &str| parse_card(s).unwrap();
        let hole = |a: &str, b: &str| [card(a), card(b)];
        let facing_bet = ValidActions {
            can_check: false,
            can_call: true,
            can_raise: true,
            min_raise: Some(40),
            max_raise: Some(2000),
        };
        let free = ValidActions {
            can_check: true,
            ..facing_bet.clone()
        };

        // —— ① 坚果度 >93%：用户那手 4h5h@8h6d7c7h3d（带对子面 8 高顺）实测 ≈0.9434。
        // 旧 0.95 阈值会 fold、新 >0.93 → call（:nut_call）。卡在 (0.93, 0.95) 之间正是阈值改动有效区。
        let board45 = [card("8h"), card("6d"), card("7c"), card("7h"), card("3d")];
        let nut45 = current_board_nuttiness(hole("4h", "5h"), &board45);
        assert!(
            (0.93..0.95).contains(&nut45),
            "4h5h 坚果度须落在 (0.93,0.95)，得 {nut45}"
        );
        let r = fallback_with_floor(
            &facing_bet,
            hole("4h", "5h"),
            &board45,
            None,
            "search_giveup:unanchored:foo".into(),
        );
        assert_eq!(r.action, "call", "4h5h >93% 须 call，得 {r:?}");
        assert_eq!(
            r.source, "search_giveup:unanchored:foo:nut_call",
            "得 {r:?}"
        );

        // —— ② 底池赔率 floor：非坚果手（空气）+ 好赔率 + 注额 ≤30BB → call（:pot_odds_call）。
        // pot/to_call = 700/100 = 7 > 4，to_call 100 ≤ 30×bb(20)=600。坚果度不命中（空气）→ 看 ②。
        let air = hole("2s", "8d");
        let air_board = [card("Ah"), card("Kh"), card("Qh"), card("2c"), card("7d")];
        assert!(current_board_nuttiness(air, &air_board) <= NUT_CALL_THRESHOLD);
        let po = Some(PotOdds {
            pot: 700,
            to_call: 100,
            big_blind: 20,
        });
        let r = fallback_with_floor(&facing_bet, air, &air_board, po, "fallback:g".into());
        assert_eq!(r.action, "call", "好赔率小注须 call，得 {r:?}");
        assert_eq!(r.source, "fallback:g:pot_odds_call", "得 {r:?}");

        // pot/to_call 恰 = 4（严格 >4 不含）→ 不触发 → fold。
        let exactly4 = Some(PotOdds {
            pot: 400,
            to_call: 100,
            big_blind: 20,
        });
        let r = fallback_with_floor(&facing_bet, air, &air_board, exactly4, "fallback:g".into());
        assert_eq!(r.action, "fold", "赔率恰 4:1（非 >4）须 fold，得 {r:?}");
        assert_eq!(r.source, "fallback:g", "未命中不加后缀，得 {r:?}");

        // 赔率够但注额 >30BB（to_call 100 > 30×bb(3)=90）→ 不触发 → fold。
        let too_big = Some(PotOdds {
            pot: 700,
            to_call: 100,
            big_blind: 3,
        });
        let r = fallback_with_floor(&facing_bet, air, &air_board, too_big, "fallback:g".into());
        assert_eq!(r.action, "fold", "注额 >30BB 须 fold，得 {r:?}");

        // 注额恰 = 30BB（to_call 600 ≤ 30×bb(20)=600，含）+ 好赔率 → call（用户场景边界：河牌 to_call=600）。
        let boundary = Some(PotOdds {
            pot: 6345,
            to_call: 600,
            big_blind: 20,
        });
        let r = fallback_with_floor(&facing_bet, air, &air_board, boundary, "fallback:g".into());
        assert_eq!(r.action, "call", "注额恰 30BB（含）须 call，得 {r:?}");
        assert_eq!(r.source, "fallback:g:pot_odds_call", "得 {r:?}");

        // 能免费 check（!facing bet）→ 两条 floor 都不主动下注，check。
        let r = fallback_with_floor(&free, air, &air_board, po, "fallback:g".into());
        assert_eq!(r.action, "check", "免费局面须 check，得 {r:?}");
        assert_eq!(r.source, "fallback:g", "免费 check 不加后缀，得 {r:?}");

        // —— preflop（board 空）也吃底池赔率 floor（2026-06-16 加）：非 premium（72o，tier 0）
        // + 好赔率小注 → pot_odds_call（① premium 不命中才看 ②）。
        let trash_pf = hole("7h", "2d");
        assert_eq!(limp_hand_tier(trash_pf), 0, "72o 须非 premium（tier 0）");
        let r = fallback_with_floor(&facing_bet, trash_pf, &[], po, "fallback:h".into());
        assert_eq!(r.action, "call", "preflop 好赔率小注须 call，得 {r:?}");
        assert_eq!(r.source, "fallback:h:pot_odds_call", "得 {r:?}");
        // preflop premium（AA）仍走 ①（premium_call 优先于 pot_odds_call）。
        let r = fallback_with_floor(&facing_bet, hole("As", "Ad"), &[], po, "fallback:h".into());
        assert_eq!(
            r.source, "fallback:h:premium_call",
            "preflop premium 走 ①，得 {r:?}"
        );
    }

    /// [`pot_odds_from_auth`] 从真栈 `GameState` 正确提取 pot / to_call（座位索引 +
    /// `call_to − committed_this_round` 口径）。
    #[test]
    fn pot_odds_from_auth_extracts_correctly() {
        let cfg = TableConfig::default_6max_100bb(); // SB 50 / BB 100 / 10000 栈。
        let bb = cfg.big_blind.as_u64();
        // UTG 先动开池到 300（min full-raise to = 100+100 = 200，合法）。
        let mut gs = GameState::new(&cfg, 7);
        gs.apply(Action::Raise {
            to: ChipAmount::new(300),
        })
        .expect("UTG open 300 合法");
        // 下一行动者（非盲位、本街已投入 0）面对 300：to_call = call_to(300) − committed(0) = 300；
        // pot = SB 50 + BB 100 + open 300 = 450。
        let po = pot_odds_from_auth(&gs, &cfg).expect("面对下注 → Some");
        assert_eq!(po.pot, 450, "pot = SB50 + BB100 + open300");
        assert_eq!(po.to_call, 300, "to_call = call_to(300) − committed(0)");
        assert_eq!(po.big_blind, bb);
        // 该手 pot/to_call = 450/300 = 1.5 < 4 → floor 不触发（验证比值口径端到端接得通）。
        let facing = ValidActions {
            can_check: false,
            can_call: true,
            can_raise: true,
            min_raise: Some(400),
            max_raise: Some(10000),
        };
        let r = fallback_with_floor(
            &facing,
            [parse_card("7h").unwrap(), parse_card("2d").unwrap()], // 72o 非 premium
            &[],
            Some(po),
            "fallback:e".into(),
        );
        assert_eq!(r.action, "fold", "1.5:1（<4）须 fold，得 {r:?}");
    }

    /// 真实 live 手回归（2026-06-14 用户报）：KK（BB）对超深码对手（seat4=99614 ≈ 5000BB）
    /// 3bet→4bet→5bet 后面对其全下。100BB 影子栈 10000 表示不了 5000BB shove（raise 到
    /// 99614×scale(5) ≫ 10000）→ `real.apply` 失败 → lockstep `replay_illegal`（site 796 失同步）。
    /// **修前**兜底 check-when-free 面对全下 = fold → 把 KK 白弃（live 实测 `fallback:replay_illegal`
    /// + fold）；**地板**（preflop tier-2 facing bet）须把它改成 call。
    #[test]
    fn live_deep_stack_kk_replay_illegal_calls_not_folds() {
        let game = preopen_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let req = Request {
            hole: vec!["Ks".into(), "Kd".into()],
            board: vec![],
            button_seat: 0,
            my_seat: 2,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                hist(3, "fold", None),
                hist(4, "raise", Some(50)),
                hist(5, "fold", None),
                hist(0, "fold", None),
                hist(1, "fold", None),
                hist(2, "raise", Some(160)),
                hist(4, "raise", Some(435)),
                hist(2, "raise", Some(1315)),
                hist(4, "raise", Some(99614)),
            ],
            valid: ValidActions {
                can_check: false,
                can_call: true,
                can_raise: false,
                min_raise: None,
                max_raise: None,
            },
            stacks: vec![2302, 1420, 1917, 628, 99614, 722],
            dealt_seats: vec![],
            names: Default::default(),
        };
        let resp = decide(
            &game,
            &abs,
            &uniform,
            &req,
            7,
            None,
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(
            resp.source, "fallback:replay_illegal:premium_call",
            "KK 超深码 shove 前 replay_illegal 失同步须命中地板，得 {resp:?}"
        );
        assert_eq!(
            resp.action, "call",
            "KK 面对全下须 call、不白弃，得 {resp:?}"
        );
        assert!(is_legal(&resp, &req.valid));
    }

    /// 非 6 人桌 → 兜底（不崩）。
    #[test]
    fn non_6max_falls_back() {
        let game = preopen_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let req = Request {
            hole: vec!["Ah".into(), "Kd".into()],
            board: vec![],
            button_seat: 0,
            my_seat: 0,
            num_seats: 4,
            small_blind: 10,
            big_blind: 20,
            actions: vec![],
            valid: ValidActions {
                can_check: true,
                ..full_valid()
            },
            stacks: vec![],
            dealt_seats: vec![],
            names: Default::default(),
        };
        let resp = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0,
            None,
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(resp.source, "fallback:not_6max");
        assert!(is_legal(&resp, &req.valid));
    }

    // —— 缺口② 实时搜索测试 ——

    /// 一个 SB(我) 在 flop 首点（folds-to-SB preflop：BTN/其余 fold，SB 补盲、BB check 进 flop）
    /// 的请求；可选 `stacks`（OpenPoker 单位）。用于搜索路径（FlopFirstUnraised 命中）。
    fn flop_first_unraised_req(stacks: Vec<u64>) -> Request {
        // OpenPoker button=0：SB=1, BB=2, UTG=3, HJ=4, CO=5。preflop：UTG/HJ/CO/BTN fold，
        // SB(1) complete(call to 20)、BB(2) check → flop。我 = SB(seat1)，flop 首个行动者、未起注。
        Request {
            hole: vec!["Ah".into(), "Kd".into()],
            board: vec!["7h".into(), "2c".into(), "Ks".into()],
            button_seat: 0,
            my_seat: 1,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                HistAction {
                    seat: 3,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 4,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 5,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 0,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 1,
                    action: "call".into(),
                    to: Some(20),
                },
                HistAction {
                    seat: 2,
                    action: "check".into(),
                    to: None,
                },
            ],
            valid: ValidActions {
                can_check: true,
                can_call: false,
                can_raise: true,
                min_raise: Some(20),
                max_raise: Some(1980),
            },
            stacks,
            dealt_seats: vec![],
            names: Default::default(),
        }
    }

    /// **核心不变量**：`search=None`（旧行为）与 `search=Some` 但**未命中触发面**（preflop /
    /// 非 flop-首点）逐字节相同——搜索只在触发点改输出，其余一律 byte-equal blueprint。
    #[test]
    fn search_off_byte_equal_blueprint() {
        let game = preopen_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // preflop 决策（folds-to-BTN）：should_search 对 preflop 恒 false → 即便开搜索也走 blueprint。
        let mut req = flop_first_unraised_req(vec![]);
        req.board = vec![]; // 改成 preflop：我 = SB，只有 UTG/HJ/CO/BTN fold（不进 flop）。
        req.actions = vec![
            HistAction {
                seat: 3,
                action: "fold".into(),
                to: None,
            },
            HistAction {
                seat: 4,
                action: "fold".into(),
                to: None,
            },
            HistAction {
                seat: 5,
                action: "fold".into(),
                to: None,
            },
            HistAction {
                seat: 0,
                action: "fold".into(),
                to: None,
            },
        ];
        req.valid = full_valid();
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::AllPostflop, // 即便最宽触发面，preflop 仍不搜。
            ..SubgameSearchConfig::default()
        };
        let off = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0x5EED,
            None,
            &mut SubgameSolveCache::new(),
        );
        let on_untriggered = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0x5EED,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(
            off, on_untriggered,
            "preflop（未触发）：search=Some 须与 search=None byte-equal，得 {off:?} vs {on_untriggered:?}"
        );
        assert_eq!(off.source, "blueprint");
    }

    /// 搜索路径端到端（flop 首点命中 FlopFirstUnraised）：真栈 100BB（stacks=2000×6）下
    /// subgame re-solve 出**合法**动作 + source=search；同 seed 两次确定性（plumbing 可复现）。
    #[test]
    fn search_flop_first_unraised_legal_and_reproducible() {
        let game = nolimp_game(); // nolimp：SB complete + BB check 是干净 on-tree 线（无 limp gap）。
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let req = flop_first_unraised_req(vec![2000, 2000, 2000, 2000, 2000, 2000]); // 对称 100BB。
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::FlopFirstUnraised,
            ..SubgameSearchConfig::default()
        };
        let resp = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0xA11CE,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert!(is_legal(&resp, &req.valid), "搜索动作须合法，得 {resp:?}");
        // source 要么 search（解成功），要么 search_giveup:*（罕见解不出来 → check-when-free），绝不静默 blueprint。
        assert!(
            resp.source == "search" || resp.source.starts_with("search_giveup:"),
            "搜索区 source 须 search / search_giveup:*，得 {resp:?}"
        );
        if resp.source == "search" {
            let probs = resp.probs.as_ref().expect("search 决策须带策略分布");
            let sum: f64 = probs.iter().map(|(_, p)| *p).sum();
            // 4 位舍入（probs_log）后和可偏 ±n×5e-5 → 容差 1e-3。
            assert!(
                (sum - 1.0).abs() < 1e-3,
                "probs 须归一（舍入容差内），和={sum}"
            );
            assert!(
                probs.iter().any(|(a, _)| Some(a) == resp.chosen.as_ref()),
                "chosen 须在 probs 支撑内，得 {resp:?}"
            );
        }
        let again = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0xA11CE,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(resp, again, "同 seed 搜索须确定性（byte-equal 可复现）");
    }

    /// `--debug-log`（[`set_subgame_debug`] + [`DEBUG`]）只往 **stderr** 打中间数据、绝不改决策：
    /// 同一锚定搜索 + 脱锚搜索场景，debug off / on 的 [`Response`] 必逐字节相等（stdout=IPC 不变，
    /// range / solve 全 byte-equal）。守「调试开关纯展示」契约——给 advisor 加 dlog!、给 subgame 加
    /// range dump 都不许动求解结果。
    #[test]
    fn debug_log_does_not_change_decision() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::FlopFirstUnraised,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        // 锚定搜索（flop 首点 100BB 对称）+ 脱锚搜索（off-stack all-in 线）两条路径都验 range dump。
        let cases = [
            flop_first_unraised_req(vec![2000, 2000, 2000, 2000, 2000, 2000]),
            offstack_allin_req(),
        ];
        let set_dbg = |on: bool| {
            set_subgame_debug(on);
            DEBUG.store(on, std::sync::atomic::Ordering::Relaxed);
        };
        for req in &cases {
            set_dbg(false); // off（默认）。
            let off = decide(
                &game,
                &abs,
                &uniform,
                req,
                0xDB6,
                Some(&rt(scfg)),
                &mut SubgameSolveCache::new(),
            );
            set_dbg(true); // on：打中间数据到 stderr。
            let on = decide(
                &game,
                &abs,
                &uniform,
                req,
                0xDB6,
                Some(&rt(scfg)),
                &mut SubgameSolveCache::new(),
            );
            set_dbg(false); // 复位（不给其它并行测试留 stderr 噪声）。
            assert_eq!(
                off, on,
                "--debug-log 须纯展示：off/on 决策须 byte-equal，得 {off:?} vs {on:?}"
            );
        }
    }

    /// `--search-flop-prefer-blueprint on`：同一 **flop 锚定**触发面（100BB 对称、影子同步 = lockstep
    /// Ok），开旗后须改走 blueprint（`source=blueprint`）而非搜索，且与 `search=None` byte-equal——
    /// 此旗只抑制 flop 锚定搜索、不改 blueprint 输出。对照：不开旗（[`rt`]）同输入是搜索区
    /// （`search` / `search_giveup:*`），证明改的确实是 flop 锚定路径。
    #[test]
    fn flop_prefer_blueprint_anchored_flop_goes_blueprint() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let req = flop_first_unraised_req(vec![2000, 2000, 2000, 2000, 2000, 2000]); // 对称 100BB → 锚定。
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::FlopFirstUnraised,
            ..SubgameSearchConfig::default()
        };
        // search=None 基准（纯 blueprint）。
        let off = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0xB1DE,
            None,
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(off.source, "blueprint");
        // flop_prefer_blueprint on：flop 锚定面抑制搜索 → 与 search=None byte-equal。
        let prefer = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0xB1DE,
            Some(&rt_flop_prefer_blueprint(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(
            off, prefer,
            "flop 锚定面 + flop_prefer_blueprint on 须与 search=None byte-equal，得 {off:?} vs {prefer:?}"
        );
        // 对照：不开旗，同输入是搜索区（证明 flop 锚定路径本会搜，旗确实抑制了它）。
        let searched = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0xB1DE,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert!(
            searched.source == "search" || searched.source.starts_with("search_giveup:"),
            "不开旗：flop 锚定面本应搜索（search / search_giveup:*），得 {searched:?}"
        );
    }

    /// 真码深（非对称深码：我 SB 600BB vs 其余浅）下搜索仍出合法动作（喂真栈，不 panic）。
    /// 钉「per-seat stacks 真喂进 subgame_search」——build_real_auth 在真栈 config 上重放成功。
    #[test]
    fn search_asymmetric_deep_stacks_legal() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // 我(SB seat1) 12000(600BB)，BB(seat2) 4000(200BB)，其余 2000；只有 SB/BB 入池。
        let req = flop_first_unraised_req(vec![2000, 12000, 4000, 2000, 2000, 2000]);
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::FlopFirstUnraised,
            max_subtree_nodes: 4_000_000, // 深码 SPR 大、树更大，放宽 cap（不爆即可）。
            ..SubgameSearchConfig::default()
        };
        let resp = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0xDEE7,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert!(
            is_legal(&resp, &req.valid),
            "深码搜索动作须合法，得 {resp:?}"
        );
        assert!(
            resp.source == "search" || resp.source.starts_with("search_giveup:"),
            "搜索区 source，得 {resp:?}"
        );
    }

    /// 搜索降级 = **check-when-free**（非直接 fold）：强制子博弈失败（`max_subtree_nodes=1` →
    /// 子树越界 `Err`），在可 check 的 flop 首点须出 **check**（不白丢免费 check），source=search_giveup:*
    /// （不回落 blueprint）。锁住 2026-06-09 「fold → check-when-free」的行为改动。
    #[test]
    fn search_giveup_checks_when_free() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let req = flop_first_unraised_req(vec![2000; 6]); // flop 首点 can_check=true。
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::FlopFirstUnraised,
            max_subtree_nodes: 1, // 任何 flop 子树 >1 节点 → subgame_search Err → 降级。
            ..SubgameSearchConfig::default()
        };
        let resp = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0xF01D,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(
            resp.action, "check",
            "可 check 的局面降级应 check（非 fold），得 {resp:?}"
        );
        assert!(
            resp.source.starts_with("search_giveup:"),
            "降级 source 须 search_giveup:*（不回落 blueprint），得 {resp:?}"
        );
    }

    /// 缺口③ 端到端：`--search-deep-menu`（deep_menu）下，深码不对称栈的搜索区子树用 {1pot} 单档
    /// 菜单仍出**合法**动作 + **source=search**（菜单与 blueprint legal_abs 不同也不降级——证
    /// subgame_search deep_menu 返回子树自身分布、advisor 用 {1pot} 抽象 outgoing 自洽）+ 可复现。
    #[test]
    fn search_deep_menu_legal_and_reproducible() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // 深码不对称：我(SB seat1) 12000(600BB) vs BB(seat2) 4000(200BB)，其余 2000。
        let req = flop_first_unraised_req(vec![2000, 12000, 4000, 2000, 2000, 2000]);
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::FlopFirstUnraised,
            deep_menu: true,              // 缺口③：子树用 {1pot} 单档菜单。
            max_subtree_nodes: 4_000_000, // 深码 SPR 大，放宽 cap（{1pot} 仍小，不会爆）。
            ..SubgameSearchConfig::default()
        };
        let resp = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0xDEE9,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert!(
            is_legal(&resp, &req.valid),
            "深码 {{1pot}} 搜索动作须合法，得 {resp:?}"
        );
        assert_eq!(
            resp.source, "search",
            "deep_menu 应解出（{{1pot}} 子树小、stub 桶 root 必累积）= source search，得 {resp:?}"
        );
        let again = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0xDEE9,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(resp, again, "同 seed deep_menu 搜索须确定性（可复现）");
    }

    /// 缺口③ v2 细化端到端：浅 SPR（9BB 栈，flop 第二大 Active 栈 = 4×pot，远 < ≤3-way 的
    /// 40×pot 阈值）下 `--search-deep-menu` 的子树菜单经 [`deep_menu_for`] 放宽到 {0.5,1} 两档——advisor outgoing
    /// 必须用**同一**自适应菜单（deep_abs_holder）算尺寸，否则 0.5pot 档在 {1pot} 抽象下找不到
    /// 对应、会塌成 all-in（错动作）。验：合法 + source=search/giveup + 可复现，且若出 raise，
    /// 尺寸在 valid 区间内（非无脑 all-in 才有意义——can_check 时 raise 须有界）。
    #[test]
    fn search_deep_menu_shallow_spr_wide_menu_legal() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // 全员 180op（9BB）：folds-to-SB，complete+check 进 flop → pot 40op=200 solver，
        // SB/BB 各剩 160op=800 solver = 4×pot（恰边界 → 宽菜单）。
        let mut req = flop_first_unraised_req(vec![180; 6]);
        req.valid = ValidActions {
            can_check: true,
            can_call: false,
            can_raise: true,
            min_raise: Some(20),
            max_raise: Some(160),
        };
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::FlopFirstUnraised,
            deep_menu: true,
            max_subtree_nodes: 4_000_000,
            ..SubgameSearchConfig::default()
        };
        let resp = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0xDEEA,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert!(
            is_legal(&resp, &req.valid),
            "浅码宽菜单搜索动作须合法，得 {resp:?}"
        );
        assert!(
            resp.source == "search" || resp.source.starts_with("search_giveup:"),
            "搜索区 source 须 search / search_giveup:*，得 {resp:?}"
        );
        let again = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0xDEEA,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(resp, again, "同 seed 浅码宽菜单搜索须确定性（可复现）");
    }

    /// within-round solve 缓存端到端（§6 #2「每轮恰好一个 solve」）：同手同街两个决策
    /// （flop 首点我 check → BB bet 1pot → 回到我）共享常驻缓存——第二决策**命中**（hits 计数
    /// 硬证不重解、只重导航），且命中输出与从头重解 **byte-equal**（固定迭代确定性 → 缓存不改
    /// 任何输出，只省 wall；time_budget 下则额外恢复「同街读同一均衡」的一致性）。
    #[test]
    fn search_within_round_cache_hits_and_byte_equal() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let req1 = flop_first_unraised_req(vec![2000; 6]);
        // 决策 2 = 同街 mid-round：我(SB) check、BB bet to 40op（=200 solver = 1.0pot，on-menu）。
        let mut req2 = flop_first_unraised_req(vec![2000; 6]);
        req2.actions.push(HistAction {
            seat: 1,
            action: "check".into(),
            to: None,
        });
        req2.actions.push(HistAction {
            seat: 2,
            action: "bet".into(),
            to: Some(40),
        });
        req2.valid = ValidActions {
            can_check: false,
            can_call: true,
            can_raise: true,
            min_raise: Some(80),
            max_raise: Some(1980),
        };
        let scfg = SubgameSearchConfig {
            iterations: 400,
            trigger: SearchTrigger::AllPostflop, // mid-round 也触发（RoundStart 默认）。
            ..SubgameSearchConfig::default()
        };
        let mut cache = SubgameSolveCache::new();
        let r1 = decide(
            &game,
            &abs,
            &uniform,
            &req1,
            0xCAC4E,
            Some(&rt(scfg)),
            &mut cache,
        );
        assert_eq!(r1.source, "search", "决策 1 应解出，得 {r1:?}");
        assert_eq!((cache.misses(), cache.hits()), (1, 0), "决策 1 = 首 solve");
        let r2_shared = decide(
            &game,
            &abs,
            &uniform,
            &req2,
            0xCAC4E,
            Some(&rt(scfg)),
            &mut cache,
        );
        assert_eq!(
            (cache.misses(), cache.hits()),
            (1, 1),
            "同手同街第二决策须命中缓存（不重解）"
        );
        assert_eq!(
            r2_shared.source, "search",
            "决策 2 应解出，得 {r2_shared:?}"
        );
        assert!(is_legal(&r2_shared, &req2.valid), "得 {r2_shared:?}");
        let r2_fresh = decide(
            &game,
            &abs,
            &uniform,
            &req2,
            0xCAC4E,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(
            r2_shared, r2_fresh,
            "缓存命中输出须 byte-equal 从头重解（固定迭代）"
        );
    }

    // —— RoundStart 预热（street-start prewarm：hero 行动前暖 solve 缓存）——

    /// 锚定预热端到端：街起点（SB 先行动、hero=BB **未轮到**）发预热 → 首 solve 入缓存；
    /// SB check 后 hero 首决策**命中**（misses 不增）、输出 byte-equal 无预热现解。
    /// `range_uniform_mix=0.25` + 非均匀 σ 钉「『不混』座 = hero（my_seat）而非街首行动者」
    /// 在 advisor 层接对（接错 → ranges 不同 → key miss → 命中断言 fail）。
    #[test]
    fn prewarm_anchored_then_decision_hits_cache() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        // 非均匀 σ（确定性）：均匀 σ 下 reach 均匀、混不混向量相同，钉不住 hero 接线。
        let skew = |i: &InfoSetId, n: usize| {
            let mut v: Vec<f64> = (0..n)
                .map(|k| 1.0 + k as f64 + (i.raw() % 7) as f64 * 0.1)
                .collect();
            let s: f64 = v.iter().sum();
            v.iter_mut().for_each(|x| *x /= s);
            v
        };
        let scfg = SubgameSearchConfig {
            iterations: 400,
            trigger: SearchTrigger::AllPostflop,
            range_uniform_mix: 0.25,
            ..SubgameSearchConfig::default()
        };
        // hero = BB（seat 2）：flop 首行动者是 SB（seat 1）→ 预热请求 = 街起点历史、未轮到 hero。
        let mut pre = flop_first_unraised_req(vec![2000; 6]);
        pre.my_seat = 2;
        pre.hole = vec!["Qs".into(), "Qd".into()];
        let mut cache = SubgameSolveCache::new();
        let p = prewarm(&game, &skew, &pre, 0xCAC4E, Some(&rt(scfg)), &mut cache);
        assert_eq!(p.source, "prewarm:stored", "得 {p:?}");
        assert_eq!((cache.misses(), cache.hits()), (1, 0), "预热 = 首 solve");

        // 决策：SB check → 轮到 hero（BB）。
        let mut req = flop_first_unraised_req(vec![2000; 6]);
        req.my_seat = 2;
        req.hole = vec!["Qs".into(), "Qd".into()];
        req.actions.push(HistAction {
            seat: 1,
            action: "check".into(),
            to: None,
        });
        let r = decide(
            &game,
            &abs,
            &skew,
            &req,
            0xCAC4E,
            Some(&rt(scfg)),
            &mut cache,
        );
        assert_eq!(r.source, "search", "得 {r:?}");
        assert_eq!(
            (cache.misses(), cache.hits()),
            (1, 1),
            "hero 首决策须命中预热 solve（hero 接线回归时此处 fail）"
        );
        let r_fresh = decide(
            &game,
            &abs,
            &skew,
            &req,
            0xCAC4E,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(r, r_fresh, "预热只省 wall、不改输出（byte-equal 现解）");

        // 边界：search off → skip；preflop（board 空）→ skip。
        let off = prewarm(&game, &skew, &pre, 0xCAC4E, None, &mut cache);
        assert_eq!(off.source, "prewarm:skip:search_off");
        let mut pre_pf = pre.clone();
        pre_pf.board = vec![];
        pre_pf.actions.truncate(4); // 只剩 folds-to-SB，preflop 中途。
        let pf = prewarm(&game, &skew, &pre_pf, 0xCAC4E, Some(&rt(scfg)), &mut cache);
        assert_eq!(pf.source, "prewarm:skip:preflop");
    }

    /// `--search-flop-prefer-blueprint on` + 预热：flop **锚定**面（lockstep Ok）的决策本会走
    /// blueprint、不搜（[`flop_prefer_blueprint_anchored_flop_goes_blueprint`]）。advisor 单线程串行
    /// 处理 → 若仍预热该子树（build+solve），hero 决策会白等一个 time_budget 才拿到 blueprint。故须
    /// `prewarm:skip:flop_prefer_blueprint`、缓存零增长。对照①：旗关（[`rt`]）同输入照常 `prewarm:stored`
    /// （证 skip 确由旗驱动，非别的前置不满足）。对照②：脱影子 flop（off-stack all-in 线、lockstep
    /// Err）decide 走 `decide_search_unanchored`、不经此旗 → 即使开旗预热仍 `prewarm:stored`（证只 skip
    /// 锚定 flop、不误伤脱影子，否则决策会 MISS 现解、白丢预热）。
    #[test]
    fn prewarm_flop_prefer_blueprint_anchored_flop_skips() {
        let game = nolimp_game();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::FlopFirstUnraised,
            ..SubgameSearchConfig::default()
        };
        // hero=BB（seat 2）：SB(1) flop 首行动者 → 预热 = 街起点、hero 未轮到；对称 100BB → 锚定。
        let mut pre = flop_first_unraised_req(vec![2000; 6]);
        pre.my_seat = 2;
        pre.hole = vec!["Qs".into(), "Qd".into()];

        // 开旗：锚定 flop 预热 → skip，缓存零增长（无 build+solve）。
        let mut cache = SubgameSolveCache::new();
        let p = prewarm(
            &game,
            &uniform,
            &pre,
            0xF10B,
            Some(&rt_flop_prefer_blueprint(scfg)),
            &mut cache,
        );
        assert_eq!(p.source, "prewarm:skip:flop_prefer_blueprint", "得 {p:?}");
        assert_eq!(
            (cache.misses(), cache.hits()),
            (0, 0),
            "skip 不该建/解任何子树"
        );

        // 对照①：旗关同输入照常预热（证 skip 由旗驱动）。
        let mut cache_off = SubgameSolveCache::new();
        let off = prewarm(
            &game,
            &uniform,
            &pre,
            0xF10B,
            Some(&rt(scfg)),
            &mut cache_off,
        );
        assert_eq!(off.source, "prewarm:stored", "旗关须照常预热，得 {off:?}");
        assert_eq!((cache_off.misses(), cache_off.hits()), (1, 0));

        // 对照②：脱影子 flop（off-stack all-in 线、lockstep Err）即使开旗也照常预热。
        let un_scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::FlopFirstUnraised,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let un_req = offstack_allin_req();
        let mut cache_un = SubgameSolveCache::new();
        let un = prewarm(
            &game,
            &uniform,
            &un_req,
            0xF10B,
            Some(&rt_flop_prefer_blueprint(un_scfg)),
            &mut cache_un,
        );
        assert_eq!(
            un.source, "prewarm:stored",
            "脱影子 flop 开旗仍须照常预热，得 {un:?}"
        );
        assert_eq!((cache_un.misses(), cache_un.hits()), (1, 0));
    }

    /// 脱影子预热端到端：off-stack all-in 线（lockstep 必失同步）→ 预热走 unanchored 路径
    /// 入缓存；决策（`search:unanchored`）命中、输出 byte-equal 无预热现解。
    #[test]
    fn prewarm_unanchored_then_decision_hits_cache() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let req = offstack_allin_req();
        let scfg = SubgameSearchConfig {
            iterations: 300,
            trigger: SearchTrigger::FlopFirstUnraised,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let mut cache = SubgameSolveCache::new();
        let p = prewarm(&game, &uniform, &req, 0x0FF7, Some(&rt(scfg)), &mut cache);
        assert_eq!(p.source, "prewarm:stored", "得 {p:?}");
        assert_eq!((cache.misses(), cache.hits()), (1, 0), "预热 = 首 solve");
        let r = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0x0FF7,
            Some(&rt(scfg)),
            &mut cache,
        );
        assert_eq!(r.source, "search:unanchored", "得 {r:?}");
        assert_eq!(
            (cache.misses(), cache.hits()),
            (1, 1),
            "脱影子首决策须命中预热 solve"
        );
        let r_fresh = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0x0FF7,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(r, r_fresh, "预热只省 wall、不改输出（byte-equal 现解）");
    }

    // —— 缺口②续：脱影子搜索（off-stack all-in 线，v1 边界①收口）——

    /// **off-stack all-in 线**请求：UTG 短码 30BB（600op）open-shove → HJ/CO/BTN fold →
    /// SB raise-over 到 60BB（1200op，真栈下合法）→ BB call → flop（UTG capped、SB/BB live、
    /// SB=我 首个行动、未起注）。100BB 影子里 UTG 的 all-in 是全栈（10000 solver）→ SB 的
    /// raise to 6000 < 10000 非法 → lockstep 必失同步（replay_illegal），旧路径拿不到 node_id。
    fn offstack_allin_req() -> Request {
        Request {
            hole: vec!["Ah".into(), "Kd".into()],
            board: vec!["7h".into(), "2c".into(), "Ks".into()],
            button_seat: 0,
            my_seat: 1,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                HistAction {
                    seat: 3,
                    action: "all_in".into(),
                    to: None,
                },
                HistAction {
                    seat: 4,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 5,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 0,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 1,
                    action: "raise".into(),
                    to: Some(1200),
                },
                HistAction {
                    seat: 2,
                    action: "call".into(),
                    to: None,
                },
            ],
            valid: ValidActions {
                can_check: true,
                can_call: false,
                can_raise: true,
                min_raise: Some(20),
                max_raise: Some(2800),
            },
            // op 单位 hand-start 真栈：UTG 600=30BB、SB/BB 4000=200BB、其余 2000=100BB。
            stacks: vec![2000, 4000, 4000, 600, 2000, 2000],
            dealt_seats: vec![],
            names: Default::default(),
        }
    }

    /// [`offstack_allin_req`] **续打到 turn**：flop（SB=1 / BB=2 check-check）走完 → turn（board
    /// 4 张、SB 首行动）。flop 本身在 100BB 影子上失同步（off-stack）→ flop 解是 unanchored；
    /// turn 决策时档二′-跨街复用沿 `prev_within`（flop check-check）读上一街子树 σ。
    fn offstack_allin_turn_req() -> Request {
        let mut req = offstack_allin_req();
        // flop 三张 + turn 一张（与 hero hole Ah/Kd / 各家牌不撞）。
        req.board = vec!["7h".into(), "2c".into(), "Ks".into(), "9d".into()];
        req.actions.push(HistAction {
            seat: 1,
            action: "check".into(),
            to: None,
        }); // SB check flop
        req.actions.push(HistAction {
            seat: 2,
            action: "check".into(),
            to: None,
        }); // BB check flop
            // turn 起点：SB 首行动、未起注（can_check）。max_raise = SB turn 剩余栈（4000−1200=2800op）。
        req.valid = ValidActions {
            can_check: true,
            can_call: false,
            can_raise: true,
            min_raise: Some(20),
            max_raise: Some(2800),
        };
        req
    }

    /// 端到端钉缺口②续：off-stack all-in 线上 ①search=None（旧路径）确证 lockstep 失同步 →
    /// 兜底（这条线在 100BB 影子上**拿不到 node_id**，测试前提）；②`--search` 开 → 脱影子真栈
    /// 搜索接管（source=search:unanchored、动作合法、同 seed 可复现）。
    #[test]
    fn offstack_allin_line_searches_unanchored() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let req = offstack_allin_req();
        // 前提确证：旧路径（search=None）必兜底。
        let off = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0x0FF5,
            None,
            &mut SubgameSolveCache::new(),
        );
        assert!(
            off.source.starts_with("fallback:"),
            "100BB 影子在 raise-over 短码 all-in 线上应失同步 → 兜底，得 {off:?}"
        );
        // --search 开：脱影子搜索接管（FlopFirstUnraised 在真栈 auth 上命中）。
        let scfg = SubgameSearchConfig {
            iterations: 300,
            trigger: SearchTrigger::FlopFirstUnraised,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let resp = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0x0FF5,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert!(
            is_legal(&resp, &req.valid),
            "脱影子搜索动作须合法，得 {resp:?}"
        );
        assert_eq!(
            resp.source, "search:unanchored",
            "off-stack all-in 线应由脱影子搜索接管（stub 桶 root 必累积），得 {resp:?}"
        );
        let again = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0x0FF5,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(resp, again, "同 seed 脱影子搜索须确定性（可复现）");
    }

    /// 档一前缀 reach 端到端（`--search-unanchored-prefix-reach`）：off-stack all-in 线 → ①前缀
    /// 关（uniform）仍由脱影子搜索接管；②前缀开 → 已同步前缀（UTG all-in + 3 fold，全 preflop）
    /// 非空 → ranges 进 solve 缓存 key → 同一 cache 与 uniform 不同 key 必 miss（证前缀真流进
    /// solve、非 no-op；stub 桶下 ranges≈uniform，key 差异仍可证）；③前缀开同 seed 可复现、合法、
    /// source=search:unanchored（不回落 blueprint）。
    #[test]
    fn unanchored_prefix_reach_flows_into_solve_key() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let req = offstack_allin_req();
        let scfg = SubgameSearchConfig {
            iterations: 300,
            trigger: SearchTrigger::FlopFirstUnraised,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let mut cache = SubgameSolveCache::new();
        // ① 前缀关（uniform）→ 脱影子搜索接管（miss + store）。
        let off = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0x0FF5,
            Some(&rt(scfg)),
            &mut cache,
        );
        assert_eq!(
            off.source, "search:unanchored",
            "前缀关：脱影子搜索接管，得 {off:?}"
        );
        assert!(is_legal(&off, &req.valid), "前缀关动作须合法，得 {off:?}");
        let misses_off = cache.misses();
        // ② 前缀开（同 cache）→ ranges 进 key → 必 miss（不复用 uniform 解）。
        let on = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0x0FF5,
            Some(&rt_prefix_reach(scfg)),
            &mut cache,
        );
        assert_eq!(
            on.source, "search:unanchored",
            "前缀开：仍脱影子搜索接管，得 {on:?}"
        );
        assert!(is_legal(&on, &req.valid), "前缀开动作须合法，得 {on:?}");
        assert!(
            cache.misses() > misses_off,
            "前缀 reach 开 → ranges 进 key → 必 miss（前缀真流进 solve），misses {misses_off}→{}",
            cache.misses()
        );
        // ③ 前缀开同 seed 可复现（独立 cache）。
        let on2 = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0x0FF5,
            Some(&rt_prefix_reach(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(on, on2, "前缀 reach 同 seed 须可复现");
    }

    /// 脱影子 × 缺口③ deep_menu：同一 off-stack all-in 线，{1pot} 单档子树仍解出 + 合法 +
    /// source=search:unanchored（子树抽象与 outgoing 自洽，{1pot} 在真栈 pot 上重算 to）。
    #[test]
    fn offstack_allin_unanchored_deep_menu_legal() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let req = offstack_allin_req();
        let scfg = SubgameSearchConfig {
            iterations: 300,
            trigger: SearchTrigger::FlopFirstUnraised,
            deep_menu: true,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let resp = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0x0FF6,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert!(
            is_legal(&resp, &req.valid),
            "脱影子 {{1pot}} 搜索动作须合法，得 {resp:?}"
        );
        assert_eq!(resp.source, "search:unanchored", "得 {resp:?}");
    }

    /// 影子失同步但 **preflop**（结构 gap：open-limp 进 nolimp）：即便开 `--search` 也不搜
    /// （preflop 走 blueprint 的 gating 不变，§1）→ 与 search=None 完全相同的处理（byte-equal；
    /// 2026-06-12 起两边都走 [`limp_heuristic`]，启发式与搜索开关无关）。
    #[test]
    fn preflop_shadow_gap_with_search_still_falls_back() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // 同 opponent_open_limp_into_nolimp_falls_back：UTG open-limp → 结构性 gap。
        let req = Request {
            hole: vec!["Ah".into(), "Kd".into()],
            board: vec![],
            button_seat: 0,
            my_seat: 4,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![HistAction {
                seat: 3,
                action: "call".into(),
                to: Some(20),
            }],
            valid: full_valid(),
            stacks: vec![],
            dealt_seats: vec![],
            names: Default::default(),
        };
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::AllPostflop,
            ..SubgameSearchConfig::default()
        };
        let off = decide(
            &game,
            &abs,
            &uniform,
            &req,
            1,
            None,
            &mut SubgameSolveCache::new(),
        );
        let on = decide(
            &game,
            &abs,
            &uniform,
            &req,
            1,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(
            off, on,
            "preflop 影子失同步：开关搜索须 byte-equal（都走 limp 启发式）"
        );
        assert_eq!(
            on.source, "limp_heuristic:raise",
            "preflop limp 池 S 档（AKo）→ iso-raise，得 {on:?}"
        );
    }

    /// preflop limp 池底池赔率 floor（2026-06-16 用户加）：[`limp_heuristic`] 在 raised 分支对非
    /// P 档牌（72o）本会 fold，但 facing bet 的底池赔率 floor 命中（`pot/to_call>4` 且
    /// `to_call≤30BB`，真栈重放算）→ 改 call（`source=limp_heuristic:pot_odds_call`，前缀不变）；
    /// 坏赔率仍维持 fold。`pre_pot_odds` 经 [`build_real_auth`] 真栈重放（limp 池合法可建）。
    #[test]
    fn preflop_limp_pot_odds_floor_intercepts_fold() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // 座位 0=BTN 1=SB(hero) 2=BB 3=UTG 4=HJ 5=CO。UTG/HJ/CO open-limp → BTN raise → SB facing。
        let mk_req = |btn_to: u64| Request {
            hole: vec!["7h".into(), "2d".into()], // 72o，非 P 档（tier 0）
            board: vec![],
            button_seat: 0,
            my_seat: 1,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                HistAction {
                    seat: 3,
                    action: "call".into(),
                    to: Some(20),
                },
                HistAction {
                    seat: 4,
                    action: "call".into(),
                    to: Some(20),
                },
                HistAction {
                    seat: 5,
                    action: "call".into(),
                    to: Some(20),
                },
                HistAction {
                    seat: 0,
                    action: "raise".into(),
                    to: Some(btn_to),
                },
            ],
            valid: ValidActions {
                can_check: false,
                can_call: true,
                can_raise: true,
                min_raise: Some(btn_to * 2),
                max_raise: Some(2000),
            },
            stacks: vec![2000; 6],
            dealt_seats: vec![],
            names: Default::default(),
        };
        // 命中：BTN min-raise to 40 → pot=130、SB to_call=30（已投 10），130/30≈4.33>4、30=1.5BB≤30BB。
        let hit = decide(
            &game,
            &abs,
            &uniform,
            &mk_req(40),
            0x5A1,
            None,
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(
            hit.action, "call",
            "好赔率 limp 池非 P 档须 call，得 {hit:?}"
        );
        assert_eq!(
            hit.source, "limp_heuristic:pot_odds_call",
            "floor 命中须改 call、保前缀，得 {hit:?}"
        );
        // 不命中：BTN raise to 600 → pot=690、to_call=590，690/590≈1.17<4 → 维持 fold。
        let miss = decide(
            &game,
            &abs,
            &uniform,
            &mk_req(600),
            0x5A1,
            None,
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(miss.action, "fold", "坏赔率须维持 fold，得 {miss:?}");
        assert_eq!(miss.source, "limp_heuristic:fold", "得 {miss:?}");
    }

    /// **limp 池进搜索**（脱影子的重要副作用，S5 结构 gap 在触发区收口）：UTG open-limp 在
    /// nolimp 影子上 preflop 即 structural_gap（旧路径整手只能兜底），但 limp 在真栈重放里
    /// 完全合法 → flop 触发点现在走脱影子搜索（source=search:unanchored）。limp 多人池是
    /// 真实分布最常见形态——这是脱影子带来的最大覆盖增量，钉死别回退。
    #[test]
    fn limped_pot_flop_searches_unanchored() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // UTG(3) open-limp → HJ/CO/BTN fold → SB(我) complete → BB check → flop 3-way，
        // SB 首个行动、未起注（FlopFirstUnraised 命中）。
        let req = Request {
            hole: vec!["Ah".into(), "Kd".into()],
            board: vec!["7h".into(), "2c".into(), "Ks".into()],
            button_seat: 0,
            my_seat: 1,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                HistAction {
                    seat: 3,
                    action: "call".into(),
                    to: Some(20),
                },
                HistAction {
                    seat: 4,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 5,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 0,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 1,
                    action: "call".into(),
                    to: Some(20),
                },
                HistAction {
                    seat: 2,
                    action: "check".into(),
                    to: None,
                },
            ],
            valid: ValidActions {
                can_check: true,
                can_call: false,
                can_raise: true,
                min_raise: Some(20),
                max_raise: Some(1980),
            },
            stacks: vec![2000; 6],
            dealt_seats: vec![],
            names: Default::default(),
        };
        // 前提确证：旧路径在 limp 池上必兜底（结构 gap）。
        let off = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0x11B9,
            None,
            &mut SubgameSolveCache::new(),
        );
        assert!(
            off.source.starts_with("fallback:"),
            "limp 进 nolimp 影子应 structural_gap → 兜底，得 {off:?}"
        );
        let scfg = SubgameSearchConfig {
            iterations: 300,
            trigger: SearchTrigger::FlopFirstUnraised,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let resp = decide(
            &game,
            &abs,
            &uniform,
            &req,
            0x11B9,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert!(
            is_legal(&resp, &req.valid),
            "limp 池搜索动作须合法，得 {resp:?}"
        );
        assert_eq!(
            resp.source, "search:unanchored",
            "limp 池 flop 触发点应由脱影子搜索接管，得 {resp:?}"
        );
    }

    // —— 短桌幻影座映射测试（exec 文档「短桌 seat_mismatch ~2.4% 兜底」修复）——

    fn hist(seat: u8, action: &str, to: Option<u64>) -> HistAction {
        HistAction {
            seat,
            action: action.into(),
            to,
        }
    }

    /// 短桌 k=4 preflop：两种不同空座布局（空座在 BB 后 / 空座夹在盲注位之间）都必须映到
    /// 与「满桌 UTG/HJ 先 fold、轮到 CO」**同一个树节点**（info_set 相等 = 映射逐位钉死；
    /// 动作可因 sample_seed 不同而不同，不比动作）。修前这两个请求都是 fallback:replay_seat_mismatch。
    #[test]
    fn short_handed_maps_to_phantom_fold_node() {
        let game = preopen_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let mut cache = SubgameSolveCache::new();
        // 满桌等价基准：button=0，UTG(3)/HJ(4) fold → CO(5)=我 决策。
        let full = Request {
            hole: vec!["Ah".into(), "Kd".into()],
            board: vec![],
            button_seat: 0,
            my_seat: 5,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![hist(3, "fold", None), hist(4, "fold", None)],
            valid: full_valid(),
            stacks: vec![],
            dealt_seats: vec![],
            names: Default::default(),
        };
        let full_resp = decide(&game, &abs, &uniform, &full, 7, None, &mut cache);
        assert_eq!(
            full_resp.source, "blueprint",
            "基准应走通，得 {full_resp:?}"
        );

        // 短桌 A：dealt={0,2,4,5}，button=4 → 环序 4(BTN),5(SB),0(BB),2(j=3→树5)。我=op2。
        // 4 人桌首个行动者 ≡ 满桌 UTG/HJ 弃牌后的 CO（位置等价）。
        let short_a = Request {
            my_seat: 2,
            button_seat: 4,
            actions: vec![],
            dealt_seats: vec![0, 2, 4, 5],
            names: Default::default(),
            hole: full.hole.clone(),
            board: vec![],
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            valid: full_valid(),
            stacks: vec![],
        };
        let resp_a = decide(&game, &abs, &uniform, &short_a, 7, None, &mut cache);
        assert_eq!(
            resp_a.source, "blueprint",
            "短桌 A 应走 blueprint，得 {resp_a:?}"
        );
        assert!(is_legal(&resp_a, &short_a.valid), "得 {resp_a:?}");
        assert_eq!(
            resp_a.info_set, full_resp.info_set,
            "短桌 A 必须映到满桌等价节点"
        );

        // 短桌 B（盲注对齐关键例）：dealt={0,3,4,5}，button=5 → SB=op0、BB=op3，**空座
        // op1/op2 夹在 SB 与 BB 之间**——原位插 fold 在树上非法，必须重映环序。我=op4（树5）。
        let short_b = Request {
            my_seat: 4,
            button_seat: 5,
            actions: vec![],
            dealt_seats: vec![0, 3, 4, 5],
            names: Default::default(),
            hole: full.hole.clone(),
            board: vec![],
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            valid: full_valid(),
            stacks: vec![],
        };
        let resp_b = decide(&game, &abs, &uniform, &short_b, 7, None, &mut cache);
        assert_eq!(
            resp_b.source, "blueprint",
            "短桌 B 应走 blueprint，得 {resp_b:?}"
        );
        assert_eq!(
            resp_b.info_set, full_resp.info_set,
            "短桌 B（盲注间空座）必须映到同一节点"
        );
    }

    /// 短桌 flop（多街 lockstep 贯通）：4 人桌 preflop 真实动作 + 幻影 fold 重放进 flop，
    /// info_set 与满桌等价请求（folds-to-SB complete + BB check）一致。
    #[test]
    fn short_handed_flop_blueprint_matches_full_equivalent() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let mut cache = SubgameSolveCache::new();
        let full = flop_first_unraised_req(vec![]);
        let full_resp = decide(&game, &abs, &uniform, &full, 7, None, &mut cache);
        assert_eq!(
            full_resp.source, "blueprint",
            "基准应走通，得 {full_resp:?}"
        );
        // 短桌：dealt={0,1,2,5}，button=0 → 首个行动者 op5（树5），BTN/SB/BB = op0/1/2 原位。
        let short = Request {
            my_seat: 1,
            actions: vec![
                hist(5, "fold", None),
                hist(0, "fold", None),
                hist(1, "call", Some(20)),
                hist(2, "check", None),
            ],
            dealt_seats: vec![0, 1, 2, 5],
            names: Default::default(),
            hole: full.hole.clone(),
            board: full.board.clone(),
            button_seat: 0,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            valid: full.valid.clone(),
            stacks: vec![],
        };
        let resp = decide(&game, &abs, &uniform, &short, 7, None, &mut cache);
        assert_eq!(
            resp.source, "blueprint",
            "短桌 flop 应走 blueprint，得 {resp:?}"
        );
        assert_eq!(
            resp.info_set, full_resp.info_set,
            "短桌 flop 必须映到满桌等价节点（多街 lockstep 贯通）"
        );
    }

    /// 短桌搜索路径：真栈（非发牌座 placeholder 必须被忽略）+ 幻影 fold 进 build_real_auth，
    /// flop 触发点解出合法动作且同 seed 确定性。
    #[test]
    fn short_handed_search_flop_legal_and_reproducible() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let mut full = flop_first_unraised_req(vec![2000, 12000, 4000, 2000, 2000, 2000]);
        full.my_seat = 1;
        full.actions = vec![
            hist(5, "fold", None),
            hist(0, "fold", None),
            hist(1, "call", Some(20)),
            hist(2, "check", None),
        ];
        full.dealt_seats = vec![0, 1, 2, 5]; // 空座 3/4 的 stacks=2000 是 placeholder。
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::FlopFirstUnraised,
            max_subtree_nodes: 4_000_000,
            ..SubgameSearchConfig::default()
        };
        let resp = decide(
            &game,
            &abs,
            &uniform,
            &full,
            0xA11CE,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert!(is_legal(&resp, &full.valid), "得 {resp:?}");
        assert!(
            resp.source == "search" || resp.source.starts_with("search_giveup:"),
            "短桌搜索区 source 须 search / search_giveup:*，得 {resp:?}"
        );
        let again = decide(
            &game,
            &abs,
            &uniform,
            &full,
            0xA11CE,
            Some(&rt(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(resp, again, "同 seed 短桌搜索须确定性");
    }

    /// k=2（OpenPoker HU 约定 = button 发 BB、非 button 发 SB 先动，live 2026-06-11 smoke
    /// 校准）：HU 手映到「满桌 fold 到 SB-vs-BB」的真实节点——info_set 与满桌等价请求一致。
    /// 占座表自相矛盾（button / 行动者不在 dealt）兜底。
    #[test]
    fn short_handed_hu_maps_and_bad_dealt_falls_back() {
        let game = preopen_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let mut cache = SubgameSolveCache::new();
        // 满桌等价基准：button=0，UTG/HJ/CO/BTN fold → SB(1)=我 决策（facing BB）。
        let full = Request {
            hole: vec!["Ah".into(), "Kd".into()],
            board: vec![],
            button_seat: 0,
            my_seat: 1,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                hist(3, "fold", None),
                hist(4, "fold", None),
                hist(5, "fold", None),
                hist(0, "fold", None),
            ],
            valid: full_valid(),
            stacks: vec![],
            dealt_seats: vec![],
            names: Default::default(),
        };
        let full_resp = decide(&game, &abs, &uniform, &full, 9, None, &mut cache);
        assert_eq!(
            full_resp.source, "blueprint",
            "基准应走通，得 {full_resp:?}"
        );
        // HU：dealt={1,4}，button=4（OpenPoker 约定 = button 发 BB → 树座 2），我=op1
        // （非 button = SB 先动 → 树座 1）。actions=[] = 幻影 [3,4,5,0] fold 后轮我。
        let hu = Request {
            button_seat: 4,
            my_seat: 1,
            dealt_seats: vec![1, 4],
            names: Default::default(),
            actions: vec![],
            hole: full.hole.clone(),
            board: vec![],
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            valid: full_valid(),
            stacks: vec![],
        };
        let resp = decide(&game, &abs, &uniform, &hu, 9, None, &mut cache);
        assert_eq!(
            resp.source, "blueprint",
            "HU 应映进树走 blueprint，得 {resp:?}"
        );
        assert!(is_legal(&resp, &hu.valid));
        assert_eq!(
            resp.info_set, full_resp.info_set,
            "HU 必须映到满桌 fold-to-SB 等价节点"
        );

        // HU postflop：OpenPoker 行动序是角色序（postflop BB 先），树是环序（SB 先），
        // 跨街反转映不进 → 显式兜底（live smoke2 实测 10 决策全被该门/重放校验拦下）。
        let hu_flop = Request {
            board: vec!["7h".into(), "2c".into(), "Ks".into()],
            actions: vec![hist(1, "call", None), hist(4, "check", None)],
            hole: full.hole.clone(),
            button_seat: 4,
            my_seat: 1,
            dealt_seats: vec![1, 4],
            names: Default::default(),
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            valid: ValidActions {
                can_check: true,
                ..full_valid()
            },
            stacks: vec![],
        };
        let resp = decide(&game, &abs, &uniform, &hu_flop, 9, None, &mut cache);
        assert_eq!(resp.source, "fallback:short_hu_postflop");
        assert!(is_legal(&resp, &hu_flop.valid));

        // button 不在 dealt：
        let bad_btn = Request {
            dealt_seats: vec![0, 4, 5],
            names: Default::default(),
            button_seat: 1,
            my_seat: 4,
            hole: full.hole.clone(),
            board: vec![],
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![],
            valid: full_valid(),
            stacks: vec![],
        };
        let resp = decide(&game, &abs, &uniform, &bad_btn, 0, None, &mut cache);
        assert_eq!(resp.source, "fallback:dealt_missing_btn_or_me");

        // 行动者不在 dealt（占座推断漏了人）→ 重放期失同步兜底（site 796 lockstep desync）。
        // hole = AKo（premium）且 facing bet → 「别扔好牌」地板把 fold 改成 call（valid/hole 来自
        // 服务端 your_turn、重放失同步也可信）；source 前缀仍含 actor_not_dealt（失同步检测不变）。
        let bad_actor = Request {
            dealt_seats: vec![0, 1, 2, 5],
            names: Default::default(),
            button_seat: 0,
            my_seat: 1,
            actions: vec![hist(3, "fold", None)],
            hole: full.hole.clone(),
            board: vec![],
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            valid: full_valid(),
            stacks: vec![],
        };
        let resp = decide(&game, &abs, &uniform, &bad_actor, 0, None, &mut cache);
        assert_eq!(resp.source, "fallback:actor_not_dealt:premium_call");
        assert_eq!(resp.action, "call");
        assert!(is_legal(&resp, &bad_actor.valid));

        // 乱序 / 重复：
        let unsorted = Request {
            dealt_seats: vec![2, 0, 5],
            names: Default::default(),
            button_seat: 0,
            my_seat: 2,
            hole: full.hole.clone(),
            board: vec![],
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![],
            valid: full_valid(),
            stacks: vec![],
        };
        let resp = decide(&game, &abs, &uniform, &unsorted, 0, None, &mut cache);
        assert_eq!(resp.source, "fallback:bad_dealt_seats");
    }

    /// 满桌显式送 `dealt_seats=[0..5]` 必须与缺省**逐字节**一致（含 sample_seed 不掺 dealt）。
    #[test]
    fn full_table_explicit_dealt_byte_equal() {
        let game = preopen_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let make = |dealt: Vec<u8>| Request {
            hole: vec!["Ah".into(), "Kd".into()],
            board: vec![],
            button_seat: 2,
            my_seat: 2,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                hist(5, "fold", None),
                hist(0, "fold", None),
                hist(1, "fold", None),
            ],
            valid: full_valid(),
            stacks: vec![],
            dealt_seats: dealt,
            names: Default::default(),
        };
        let implicit = decide(
            &game,
            &abs,
            &uniform,
            &make(vec![]),
            0xBEEF,
            None,
            &mut SubgameSolveCache::new(),
        );
        let explicit = decide(
            &game,
            &abs,
            &uniform,
            &make(vec![0, 1, 2, 3, 4, 5]),
            0xBEEF,
            None,
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(implicit, explicit, "满桌 dealt 显式/缺省须 byte-equal");
        assert_eq!(implicit.source, "blueprint");
        let probs = implicit.probs.as_ref().expect("blueprint 决策须带策略分布");
        let sum: f64 = probs.iter().map(|(_, p)| *p).sum();
        // 4 位舍入（probs_log）后和可偏 ±n×5e-5 → 容差 1e-3。
        assert!(
            (sum - 1.0).abs() < 1e-3,
            "probs 须归一（舍入容差内），和={sum}"
        );
        assert!(
            probs
                .iter()
                .any(|(a, _)| Some(a) == implicit.chosen.as_ref()),
            "chosen 须在 probs 支撑内，得 {implicit:?}"
        );
    }

    /// `--search-time-budget-ms` 不配显式 `--search-iterations` 时 iterations 须抬到
    /// `u64::MAX`（纯墙钟截断）。老默认 1000 让预算静默失效（solve 在 ~10–30ms 撞迭代
    /// 上限，2026-06-11 live searchon50 实撞：5s 预算下还出「iterations 内未采样到」giveup）。
    #[test]
    fn parse_args_time_budget_defaults_iterations_unbounded() {
        let parse = |extra: &[&str]| {
            let argv = ["--checkpoint", "c.ckpt", "--bucket-table", "b.bin"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string());
            parse_args_from(argv)
        };
        // budget 无显式 iterations → u64::MAX（墙钟是唯一截断）。
        let a = parse(&["--search", "--search-time-budget-ms", "5000"]).expect("parse Ok");
        assert_eq!(a.search.expect("search on").iterations, u64::MAX);
        // 显式 iterations 优先于 budget 默认。
        let a = parse(&[
            "--search",
            "--search-time-budget-ms",
            "5000",
            "--search-iterations",
            "2000000",
        ])
        .expect("parse Ok");
        assert_eq!(a.search.expect("search on").iterations, 2_000_000);
        // 无 budget → 固定迭代老默认 1000（既有行为不变）。
        let a = parse(&["--search"]).expect("parse Ok");
        assert_eq!(a.search.expect("search on").iterations, 1000);
        // 拒绝静默 guard 对显式 iterations（含恰好 1000）仍生效。
        assert!(parse(&["--search-iterations", "1000"]).is_err());
    }

    /// 叠加剥削 CLI：`--exploit on|vpip|off` 三态（on=VPIP+PFR / vpip=仅 VPIP / off=关，默认关）；
    /// on|vpip 需配 `--search`；子旗需 `--exploit on|vpip`；缺参数 / 脏值 / 出界拒收；子旗覆盖默认。
    #[test]
    fn parse_args_exploit() {
        let parse = |extra: &[&str]| {
            let argv = ["--checkpoint", "c.ckpt", "--bucket-table", "b.bin"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string());
            parse_args_from(argv)
        };
        // 省略 / off → 关。
        assert!(parse(&[]).expect("parse Ok").exploit.is_none());
        assert!(parse(&["--search", "--exploit", "off"])
            .expect("parse Ok")
            .exploit
            .is_none());
        // --exploit 缺参数 / 脏值拒收。
        assert!(
            parse(&["--search", "--exploit"]).is_err(),
            "--exploit 缺参数应拒收"
        );
        assert!(
            parse(&["--search", "--exploit", "x"]).is_err(),
            "--exploit 脏值应拒收"
        );
        // --exploit on|vpip 需 --search。
        assert!(
            parse(&["--exploit", "on"]).is_err(),
            "--exploit on 无 --search 应拒收"
        );
        // 子旗需 --exploit on|vpip。
        assert!(
            parse(&["--search", "--exploit-strength", "0.3"]).is_err(),
            "--exploit-* 无 --exploit 应拒收"
        );
        // --search --exploit on → Some(默认 + PFR 形状开)。
        let d = ExploitConfig::default();
        let a = parse(&["--search", "--exploit", "on"]).expect("parse Ok");
        let e = a.exploit.expect("exploit on");
        assert_eq!(e.min_hands, d.min_hands);
        assert!((e.strength_alpha - d.strength_alpha).abs() < 1e-12);
        assert!(e.pfr_shape, "--exploit on → VPIP+PFR 形状");
        // --exploit vpip → 仅 VPIP（pfr_shape 关，与现网逐位 byte-equal）。
        let e = parse(&["--search", "--exploit", "vpip"])
            .expect("parse Ok")
            .exploit
            .expect("exploit on");
        assert!(!e.pfr_shape, "--exploit vpip → 仅 VPIP（TopK）");
        // 子旗覆盖（on 接子旗）。
        let a = parse(&[
            "--search",
            "--exploit",
            "on",
            "--exploit-min-hands",
            "300",
            "--exploit-strength",
            "0.3",
        ])
        .expect("parse Ok");
        let e = a.exploit.expect("exploit on");
        assert_eq!(e.min_hands, 300);
        assert!((e.strength_alpha - 0.3).abs() < 1e-12);
        assert!(e.pfr_shape);
        // 出界拒收。
        assert!(parse(&["--search", "--exploit", "on", "--exploit-strength", "1.5"]).is_err());
    }

    #[test]
    fn pick_shape_guards_byte_equal() {
        use PreflopEntry::*;
        // off → 恒 TopK（与现有 exploit byte-equal），无论入池方式 / PFR 收敛。
        assert_eq!(pick_shape(false, true, Passive), ExploitShape::TopK);
        assert_eq!(pick_shape(false, true, Aggressive), ExploitShape::TopK);
        // on 但 PFR 未收敛 → TopK（退仅 VPIP）。
        assert_eq!(pick_shape(true, false, Passive), ExploitShape::TopK);
        // on + PFR 收敛 → 按入池方式选。
        assert_eq!(pick_shape(true, true, Passive), ExploitShape::CallBand);
        assert_eq!(pick_shape(true, true, Aggressive), ExploitShape::RaiseBand);
        assert_eq!(pick_shape(true, true, Unknown), ExploitShape::TopK);
    }

    #[test]
    fn preflop_entry_kinds_classifies_aggressor_vs_caller() {
        // 满 6-max：UTG(3) 开池加注、HJ(4) 平跟、BB(2) 平跟、其余弃 → UTG=Aggressive、HJ/BB=Passive。
        let req = Request {
            hole: vec!["Td".into(), "Th".into()],
            board: vec![],
            button_seat: 0,
            my_seat: 1,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: vec![
                HistAction {
                    seat: 3,
                    action: "raise".into(),
                    to: Some(60),
                },
                HistAction {
                    seat: 4,
                    action: "call".into(),
                    to: None,
                },
                HistAction {
                    seat: 5,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 0,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 1,
                    action: "fold".into(),
                    to: None,
                },
                HistAction {
                    seat: 2,
                    action: "call".into(),
                    to: None,
                },
            ],
            valid: ValidActions {
                can_check: true,
                can_call: false,
                can_raise: true,
                min_raise: Some(200),
                max_raise: Some(10000),
            },
            stacks: vec![2000; 6],
            dealt_seats: vec![],
            names: Default::default(),
        };
        let solver_cfg = TableConfig::default_6max_100bb();
        let scale = solver_cfg.big_blind.as_u64() / req.big_blind; // 5
        let smap = seat_map(&req).expect("seat_map");
        let kinds = preflop_entry_kinds(&req, &solver_cfg, scale, &smap);
        let kind_of = |op_seat: u8| kinds[smap.tree_seat(op_seat).unwrap() as usize];
        assert_eq!(kind_of(3), PreflopEntry::Aggressive, "UTG 开池加注");
        assert_eq!(kind_of(4), PreflopEntry::Passive, "HJ 平跟");
        assert_eq!(kind_of(2), PreflopEntry::Passive, "BB 平跟");
        assert_eq!(kind_of(5), PreflopEntry::Unknown, "弃牌座不分类");
    }

    /// `Request` 解析 `names`（serde 默认；缺省 = 空 → 旧请求 byte-equal）。
    #[test]
    fn request_parses_names() {
        let j = r#"{"hole":["Ah","Kh"],"board":[],"button_seat":0,"my_seat":1,"num_seats":6,
                    "small_blind":10,"big_blind":20,"actions":[],"valid":{"can_check":false},
                    "names":{"2":"villain"}}"#;
        let req: Request = serde_json::from_str(j).unwrap();
        assert_eq!(req.names.get(&2).map(String::as_str), Some("villain"));
        // 缺省 names = 空。
        let j2 = r#"{"hole":["Ah","Kh"],"board":[],"button_seat":0,"my_seat":1,"num_seats":6,
                     "small_blind":10,"big_blind":20,"actions":[],"valid":{"can_check":false}}"#;
        let req2: Request = serde_json::from_str(j2).unwrap();
        assert!(req2.names.is_empty());
    }

    /// `--search-range-uniform-mix`（range 先验平滑 λ）：生产默认 = 开
    /// （DEFAULT_SEARCH_RANGE_UNIFORM_MIX）、显式 0 = 关、出 [0,1] 拒收、未开 --search 拒收
    /// （拒绝静默 guard）。
    #[test]
    fn parse_args_range_uniform_mix() {
        let parse = |extra: &[&str]| {
            let argv = ["--checkpoint", "c.ckpt", "--bucket-table", "b.bin"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string());
            parse_args_from(argv)
        };
        // 默认（未给 flag）= 生产默认开。
        let a = parse(&["--search"]).expect("parse Ok");
        assert_eq!(
            a.search.expect("search on").range_uniform_mix,
            DEFAULT_SEARCH_RANGE_UNIFORM_MIX
        );
        // 显式 0 = 关（旧行为 A/B 用）。
        let a = parse(&["--search", "--search-range-uniform-mix", "0"]).expect("parse Ok");
        assert_eq!(a.search.expect("search on").range_uniform_mix, 0.0);
        // 显式覆盖。
        let a = parse(&["--search", "--search-range-uniform-mix", "0.5"]).expect("parse Ok");
        assert_eq!(a.search.expect("search on").range_uniform_mix, 0.5);
        // 出 [0,1] 拒收。
        assert!(parse(&["--search", "--search-range-uniform-mix", "1.5"]).is_err());
        assert!(parse(&["--search", "--search-range-uniform-mix", "-0.1"]).is_err());
        // 拒绝静默 guard：设了 flag 未开 --search → Err。
        assert!(parse(&["--search-range-uniform-mix", "0.25"]).is_err());
    }

    /// `--search-bucket-table`（子树独立桶表路径）：默认 None（沿用 blueprint 表）、显式给 =
    /// 透传路径、未开 --search 拒收（拒绝静默 guard——设了表却跑纯 blueprint 是配置错误）。
    #[test]
    fn parse_args_search_bucket_table() {
        let parse = |extra: &[&str]| {
            let argv = ["--checkpoint", "c.ckpt", "--bucket-table", "b.bin"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string());
            parse_args_from(argv)
        };
        // 默认 = None（搜索沿用 blueprint 表，旧行为）。
        let a = parse(&["--search"]).expect("parse Ok");
        assert!(a.search_bucket_table.is_none());
        // 显式给 → 路径透传。
        let a = parse(&["--search", "--search-bucket-table", "fine_500.bin"]).expect("parse Ok");
        assert_eq!(
            a.search_bucket_table.as_deref(),
            Some(std::path::Path::new("fine_500.bin"))
        );
        // 拒绝静默 guard：设了表未开 --search → Err。
        assert!(parse(&["--search-bucket-table", "fine_500.bin"]).is_err());
    }

    /// `--search-solve-threads`（solve update 并行，限时杠杆③）：默认 1（单线程既有行为）、
    /// 显式给 = 进 cfg、0 拒收（无意义档位）、未开 --search 拒收（拒绝静默 guard）。
    #[test]
    fn parse_args_search_solve_threads() {
        let parse = |extra: &[&str]| {
            let argv = ["--checkpoint", "c.ckpt", "--bucket-table", "b.bin"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string());
            parse_args_from(argv)
        };
        // 默认 = 1（单线程，与既有基线 byte-equal）。
        let a = parse(&["--search"]).expect("parse Ok");
        assert_eq!(a.search.expect("search on").solve_threads, 1);
        // 显式给 → 进 cfg。
        let a = parse(&["--search", "--search-solve-threads", "4"]).expect("parse Ok");
        assert_eq!(a.search.expect("search on").solve_threads, 4);
        // 0 拒收。
        assert!(parse(&["--search", "--search-solve-threads", "0"]).is_err());
        // 拒绝静默 guard：设了 flag 未开 --search → Err。
        assert!(parse(&["--search-solve-threads", "4"]).is_err());
    }

    /// `--search-unanchored-prefix-reach on|off`（档一，§5.1 拍板默认开）：默认开、off 显式关、
    /// on 显式开、坏值拒收、未开 --search 拒收。
    #[test]
    fn parse_args_unanchored_prefix_reach() {
        let parse = |extra: &[&str]| {
            let argv = ["--checkpoint", "c.ckpt", "--bucket-table", "b.bin"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string());
            parse_args_from(argv)
        };
        // 默认开（§5.1 实测拍板：脱锚 off-tree 点 uniform 先验致 stack-off 漏洞）。
        let a = parse(&["--search"]).expect("parse Ok");
        assert!(a.search_unanchored_prefix_reach, "默认开");
        // off 显式关（A/B 对照臂）。
        let a = parse(&["--search", "--search-unanchored-prefix-reach", "off"]).expect("parse Ok");
        assert!(!a.search_unanchored_prefix_reach, "off 应关");
        // on 显式开。
        let a = parse(&["--search", "--search-unanchored-prefix-reach", "on"]).expect("parse Ok");
        assert!(a.search_unanchored_prefix_reach, "on 应开");
        // 坏值拒收。
        assert!(parse(&["--search", "--search-unanchored-prefix-reach", "maybe"]).is_err());
        // 拒绝静默 guard：设了 flag 未开 --search → Err。
        assert!(parse(&["--search-unanchored-prefix-reach", "off"]).is_err());
    }

    /// `--search-unanchored-cross-street on|off`（档二′-跨街复用，决策级 A/B + 机制拍板默认开）：
    /// 默认开、on 显式开、off 显式关、坏值拒收、未开 --search 拒收。
    #[test]
    fn parse_args_unanchored_cross_street() {
        let parse = |extra: &[&str]| {
            let argv = ["--checkpoint", "c.ckpt", "--bucket-table", "b.bin"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string());
            parse_args_from(argv)
        };
        // 默认开（决策级 A/B + 机制拍板，同档一）。
        let a = parse(&["--search"]).expect("parse Ok");
        assert!(a.search_unanchored_cross_street, "默认开");
        // on 显式开。
        let a = parse(&["--search", "--search-unanchored-cross-street", "on"]).expect("parse Ok");
        assert!(a.search_unanchored_cross_street, "on 应开");
        // off 显式关（A/B 对照臂 / 回退）。
        let a = parse(&["--search", "--search-unanchored-cross-street", "off"]).expect("parse Ok");
        assert!(!a.search_unanchored_cross_street, "off 应关");
        // 坏值拒收。
        assert!(parse(&["--search", "--search-unanchored-cross-street", "maybe"]).is_err());
        // 拒绝静默 guard：设了 flag 未开 --search → Err。
        assert!(parse(&["--search-unanchored-cross-street", "on"]).is_err());
    }

    /// `--search-flop-prefer-blueprint on|off`（仅 flop 锚定面优先 blueprint，默认关）：默认关、
    /// on 显式开、off 显式关、坏值拒收、未开 --search 拒收（拒绝静默 guard）。
    #[test]
    fn parse_args_flop_prefer_blueprint() {
        let parse = |extra: &[&str]| {
            let argv = ["--checkpoint", "c.ckpt", "--bucket-table", "b.bin"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string());
            parse_args_from(argv)
        };
        // 默认关（旧行为 byte-equal：flop 锚定面命中触发仍搜索）。
        let a = parse(&["--search"]).expect("parse Ok");
        assert!(!a.search_flop_prefer_blueprint, "默认关");
        // on 显式开（仅 flop 锚定面回 blueprint）。
        let a = parse(&["--search", "--search-flop-prefer-blueprint", "on"]).expect("parse Ok");
        assert!(a.search_flop_prefer_blueprint, "on 应开");
        // off 显式关。
        let a = parse(&["--search", "--search-flop-prefer-blueprint", "off"]).expect("parse Ok");
        assert!(!a.search_flop_prefer_blueprint, "off 应关");
        // 坏值拒收。
        assert!(parse(&["--search", "--search-flop-prefer-blueprint", "maybe"]).is_err());
        // 拒绝静默 guard：设了 flag 未开 --search → Err。
        assert!(parse(&["--search-flop-prefer-blueprint", "on"]).is_err());
    }

    /// 档二′-跨街复用端到端（脱影子 `decide` 路径，trigger=AllPostflop 才搜 turn）：先 flop 决策解
    /// flop 子树入缓存 → turn 决策复用其 σ（`build_real_auth` 重建 `prev_within` = flop 完整动作线 →
    /// `decide_search_unanchored` → `subgame_search_unanchored_cached_cross`）。stub 桶下钉 plumbing：
    /// 两决策均 `search:unanchored`、无 panic、同序列可复现（gate②）。真桶级「复用真改 range」由
    /// [`cross_street_changes_solve_key`](poker::training::subgame) 在 subgame 层确定性钉死。
    #[test]
    fn cross_street_decide_flop_then_turn() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let scfg = SubgameSearchConfig {
            iterations: 300,
            trigger: SearchTrigger::AllPostflop, // 档二′ 在 turn/river → 须 AllPostflop（FlopFirstUnraised 不搜 turn）。
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let flop_req = offstack_allin_req();
        let turn_req = offstack_allin_turn_req();
        let seed = 0xC505_0FF5;
        // 序列：flop 决策（解 flop 入缓存）→ turn 决策（跨街复用 flop 解）。
        let mut cache = SubgameSolveCache::new();
        let r_flop = decide(
            &game,
            &abs,
            &uniform,
            &flop_req,
            seed,
            Some(&rt_cross_street(scfg)),
            &mut cache,
        );
        assert_eq!(
            r_flop.source, "search:unanchored",
            "flop 脱影子搜索：{r_flop:?}"
        );
        let r_turn = decide(
            &game,
            &abs,
            &uniform,
            &turn_req,
            seed,
            Some(&rt_cross_street(scfg)),
            &mut cache,
        );
        assert_eq!(
            r_turn.source, "search:unanchored",
            "turn 脱影子搜索（跨街复用）：{r_turn:?}"
        );
        // 可复现：另起缓存跑同序列 → 同 turn 决策（gate②，跨街复用是请求的确定性函数）。
        let mut cache2 = SubgameSolveCache::new();
        let _ = decide(
            &game,
            &abs,
            &uniform,
            &flop_req,
            seed,
            Some(&rt_cross_street(scfg)),
            &mut cache2,
        );
        let r_turn2 = decide(
            &game,
            &abs,
            &uniform,
            &turn_req,
            seed,
            Some(&rt_cross_street(scfg)),
            &mut cache2,
        );
        assert_eq!(r_turn, r_turn2, "跨街复用 turn 决策须可复现");
    }
}
