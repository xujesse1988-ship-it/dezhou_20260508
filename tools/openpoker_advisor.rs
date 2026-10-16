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
//! 但泛化到 **N 座**（`--seats`，默认 6；9 座 blueprint 传 `--seats 9`）：座位按相对 button
//! rotate 到 solver 的 `TableConfig::n_max_at_bb(N, 100)`（button=座0），OpenPoker 筹码
//! （SB10/BB20/买入2000）按 `scale = solver_BB / op_BB = 5` 换算到 solver 单位。solver 座位数 /
//! 盲注 / 码深的**唯一来源** = blueprint game 的 [`SimplifiedNlheGame::table_config`]（重放与
//! blueprint 树同一 config，不另写一份 N 座常量）。
//!
//! # 鲁棒兜底（live 不能崩 / 不能挂死）
//!
//! 任何重放失败（码深漂移 desync、结构性 gap = 对手 open-limp 进 no-limp 影子、桌面座位数 ≠ N、
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
//! # 按街裁切的 blueprint（`--cropped-checkpoint`）
//!
//! 加载 [`nlhe_dense_crop`](poker::training::nlhe_dense_crop) 产出的 crop 文件（如只含翻前 + 翻牌）
//! 替代完整 dense ckpt，全部 σ 查询经 [`Blueprint`] 一处。保留街上与完整 blueprint 逐位一致；被裁掉的
//! 街**只能实时搜索**：启动期强制 `--search` + `--search-trigger all-postflop` + 跨街复用开（
//! [`validate_crop_runtime`]，不满足直接报错退出）。搜索 range 先验不查被裁街 σ
//! （[`SubgameSearchConfig::blueprint_max_street`] 守卫）：river 先验 = turn 子树跨街后验；缓存里没有
//! 可复用的 turn 解时按需补解 turn（[`search_with_crop_backfill`]），仍不成 →
//! `source=search_giveup:crop_no_posterior:*`（check-when-free + 地板），绝不退均匀 / 不读被裁街。
//!
//! # 已知限制（blueprint 路径，`...client_design...` §4）
//!
//! - 码深 ≠ 100BB 且**未开搜索 / 未触发**：solver 树/SPR 都按 100BB 解；real `GameState` 用
//!   blueprint 的 100BB config（10000 筹码）近似，driver 靠买入锁 2000 + 栈漂出 [80,125]BB leave/rejoin 兜。
//! - 对手 open-limp：no-limp blueprint 无对应节点 → preflop 走 [`limp_heuristic`] 矩阵
//!   （好牌 iso-raise / 顶级面对加注 call / 其余免费 check 或 fold）；postflop 由脱锚搜索接管。
//! - 短桌手（N 座只发 k<N 家）：driver 送 `dealt_seats`（`table_state.seats[].in_hand` 推断）时
//!   走**幻影座映射**（[`seat_map`]：k 人局映成 N-max 树「UTG 侧前 N−k 位先 fold」的真实节点、
//!   盲注对齐；k=2 按 OpenPoker 实测 HU 约定 button=BB 映 SB/BB→树座 1/2）；占座不可判
//!   （无 table_state）→ 仍兜底。桌面座位数（`num_seats`）必须 == N，否则 `fallback:not_{N}max`
//!   （如 6 座 blueprint 上 9 座桌 = `not_6max`；不做跨座位数映射，避免静默错映射）。

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use poker::eval::NaiveHandEvaluator;
use poker::training::blueprint_advisor::{
    advance_shadow_by_applied, find_tag, outgoing_action, parse_card,
};
use poker::training::game::{Game, PlayerId};
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::nlhe_betting_tree::{
    deep_menu_for, six_max_reshape, AbstractActionTag, NodeId, SIX_MAX_RESHAPE_NAMES,
};
use poker::training::nlhe_dense_crop::CroppedBlueprint;
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::openpoker_hh::{hh_record_to_observe, hist_to_concrete, HhRecord};
use poker::training::opponent_profile::{ExploitConfig, ObserveHand, OpponentProfile, Profiler};
use poker::training::sampling::sample_discrete;
use poker::training::subgame::{
    chen_score, is_blueprint_cropped_err, set_subgame_debug, should_search, subgame_search_cached,
    subgame_search_prewarm, subgame_search_unanchored_cached_cross_exploit,
    subgame_search_unanchored_prewarm_cross, synced_prefix_decisions, ExploitPrior, ExploitShape,
    PrefixReach, ResolveRoot, SearchTrigger, SubgameSearchConfig, SubgameSolveCache,
};
use poker::{
    AbstractAction, Action, BucketTable, Card, ChaCha20Rng, ChipAmount, GameState, HandEvaluator,
    InfoSetId, SeatId, StreetActionAbstraction, StreetTag, TableConfig,
};

/// `--seats` 缺省值（6-max；不传新参数时行为与引入 `--seats` 前逐位相同）。
const DEFAULT_SEATS: u8 = 6;
/// solver / blueprint 起始码深（BB）。advisor 只支持 100BB blueprint（driver 买入锁 2000 = 100BB）。
const SOLVER_STACK_BB: u32 = 100;
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
/// 子树 solve 只轮子树根仍 Active 的座当 traverser 的生产默认（`SubgameSearchConfig::live_traversers`）。
/// 默认全座轮转下弃牌座轮到 = 零学习迭代：9-max 单挑底池 1000 次迭代只约 111 次 hero 遍历，hero 真实桶
/// 常抽不中 → 平均策略只是均匀占位（2026-10-16 实盘 22 次搜索决策 6 次均匀，
/// `docs/temp/openpoker_9max_live_spotcheck_2026_10_16.md`）。只跳过零学习迭代、不改算法，同 wall 有效迭代
/// ×n_seats/n_active。`--search-live-traversers off` 显式关（回到全座轮转，A/B 对照）。
const DEFAULT_SEARCH_LIVE_TRAVERSERS: bool = true;
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
///
/// 锚定之外还须过真码深闸（[`flop_blueprint_stack_gate`]）：100BB 影子按对称 100BB 重放，短码
/// all-in 在影子里就是 100BB all-in、照样同步，光看锚定会让 blueprint 用错底池赔率 / SPR 出牌。
/// 还须过尺寸闸（[`flop_blueprint_size_gate`]）：本街每次下注的映射误差都在 0.75–1.33 内。
const DEFAULT_SEARCH_FLOP_PREFER_BLUEPRINT: bool = false;
/// 方案 B「平跟当弃牌」的默认（`--search-limp-as-fold`，[`lockstep_replay`]）：**默认关**（旧行为
/// byte-equal）。调研（`docs/temp/openpoker_limp_handling_research_2026_10_16.md` §4）建议先做成
/// 离线开关，回放已有平跟决策点确认翻后前缀非空比例 / 兜底占比 / 均匀出牌下降后再开实盘。
const DEFAULT_SEARCH_LIMP_AS_FOLD: bool = false;
/// 子树节点上限的 advisor 默认（`--search-max-nodes`，覆盖 [`SubgameSearchConfig`] 的通用默认
/// 8000）。2026-10-16 代码审查 §4：4 人单加注池 200BB 宽菜单约 16 万节点，10 万上限下直接放弃 →
/// check / fold；提到 20 万。557k 节点约 100MB（`docs/status_v3.md`），20 万约 36MB，单次建树内存
/// 可控；更大的上限等换更高配置的机器再定。
const DEFAULT_SEARCH_MAX_NODES: usize = 200_000;
/// flop 优先 blueprint 的有效码深区间（BB，含端点；[`flop_blueprint_stack_gate`]）。hero 与每个
/// 未弃牌对手的 hand-start 有效码深都须落在区间内，否则 flop 照常实时搜索。
const FLOP_BLUEPRINT_EFF_STACK_BB_MIN: u64 = 60;
const FLOP_BLUEPRINT_EFF_STACK_BB_MAX: u64 = 140;
/// flop 优先 blueprint 的下注尺寸映射误差区间（[`flop_blueprint_size_gate`]）：本街每次下注 / 加注的
/// 真实池比例 ÷ blueprint 树里映射到的池比例，须落在 [MIN, MAX]/1000 内（含端点），否则 flop 照常
/// 实时搜索。all-1pot 的翻牌菜单只有 {1pot}，0.2–1.5 pot 的注都映射成 1pot——不加这条，面对 1/3 池
/// c-bet 就会用「面对满池注」的弃牌频率（`openpoker_code_review_findings_2026_10_16.md` §3）。
const FLOP_BLUEPRINT_SIZE_RATIO_MILLI_MIN: u64 = 750;
const FLOP_BLUEPRINT_SIZE_RATIO_MILLI_MAX: u64 = 1333;
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
    /// OpenPoker 座位（升序）。缺省 / 全 N 座 = 满桌（旧行为 byte-equal）。k∈[2,N−1] →
    /// [`seat_map`] 把 k 人局映成 N-max 树「UTG 侧前 N−k 位先 fold」的真实节点（盲注对齐：
    /// 真实 BTN/SB/BB → 树座 0/1/2；k=2 按 OpenPoker 实测 HU 约定 button=BB → SB/BB 映树座
    /// 1/2、幻影 [3..N, 0]（6 座 = [3,4,5,0]），**仅 preflop**——postflop 行动序角色反转映不进、
    /// 显式兜底）。
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

/// 搜索区降级的 `source`：crop 缺后验（[`CROP_NO_POSTERIOR`] 前缀）→ `search_giveup:crop_no_posterior:*`
/// （独立分桶）；其余 → 旧标签 `search_giveup:{kind}:{reason}`（完整 blueprint 下恒走此支，byte-equal）。
fn giveup_source(kind: &str, reason: &str) -> String {
    if reason.starts_with(CROP_NO_POSTERIOR) {
        format!("search_giveup:{reason}")
    } else {
        format!("search_giveup:{kind}:{reason}")
    }
}

/// `poker::Street` → blueprint 街标签（Showdown 无决策 → `None`）。
fn street_tag_of(s: poker::Street) -> Option<StreetTag> {
    match s {
        poker::Street::Preflop => Some(StreetTag::Preflop),
        poker::Street::Flop => Some(StreetTag::Flop),
        poker::Street::Turn => Some(StreetTag::Turn),
        poker::Street::River => Some(StreetTag::River),
        poker::Street::Showdown => None,
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
    /// 下标 = OpenPoker 座位号（长度 = 桌面座位数 = solver 座位数 N）；`None` = 本手未发牌
    /// （空座 / 等下一手的玩家）。
    to_tree: Vec<Option<u8>>,
    /// 重放序列开头按序先 fold 的幻影树座（preflop 行动序）。k∈[3,N−1] = `3..3+(N−k)`
    /// （UTG 起最早行动位，恰好最先轮到）；k=2 = `[3..N, 0]`（BTN 位也是幻影；6 座 = [3,4,5,0]）。
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

/// 建座位映射（`n` = solver 座位数 N = blueprint 的 `table_config().n_seats`；caller 已校验桌面
/// `num_seats == n`）。solver 树的位置语义 = `(seat − button) % n`、button = 树座 0：树座 0/1/2 =
/// BTN/SB/BB，3.. = UTG 起按 preflop 行动序（9 座：3=UTG … 8=CO）。
///
/// `dealt_seats` 缺省 / 全 n 座 → 满桌恒等（旧 `(op + n − button) % n` 公式，6 座 byte-equal）。
/// k∈[3,n−1] → **短桌幻影座映射**：真实 BTN/SB/BB → 树座 0/1/2（N-max 树的
/// SB/BB 固定在 button+1/+2 且必须发盲，树上不存在「盲注位发盲前 fold」节点，所以不能在
/// 空座原位插 fold、必须重映环序对齐盲注），其余真实玩家按环序占 CO 侧靠后位置
/// （ring 序 j≥3 → 树座 j+n−k），幻影座占 UTG 起最早行动位、开局先 fold——k 人桌首个
/// 行动者 ≡ N-max 前 n−k 位弃牌后的同位置（标准短桌位置等价，blueprint 真实训练过的节点；
/// preflop 3..n−1,0,1,2 与 postflop 从 SB 顺时针两序在 3≤k<n 下都与真实短桌严格吻合）。
///
/// k=2：OpenPoker 的 HU 盲注按环规则贴（**live 校准 2026-06-11 两轮 smoke**：button 发
/// BB、非 button 发 SB——非标准 HU），但**行动序是角色序**（preflop SB 先、postflop BB
/// 先 = 标准 HU 的角色顺序）。树是环序（fold 到 SB-vs-BB 后两条街都 SB 先），跨街反转
/// 表达不了 → **只映 preflop**（真实 SB → 树座 1、真实 BB(button) → 树座 2、幻影
/// `[3..n, 0]` 先 fold，preflop 行动序严格吻合）；postflop 由 caller 显式兜底
/// `short_hu_postflop`（实测顺序错位必被重放 seat 校验拦下、0 漏网，门只是把原因标清楚）。
///
/// 脏输入（button 越界 / dealt 非升序、越界、个数不在 [2,n]、缺 button 或我）→ `Err`（caller
/// 安全兜底），绝不静默错映射。
fn seat_map(req: &Request, n: usize) -> Result<SeatMap, String> {
    debug_assert!(
        (3..=9).contains(&n),
        "solver 座位数须在 [3,9]（--seats 校验）"
    );
    if req.button_seat as usize >= n {
        // 旧实现在此 `op + n − button` 会下溢（debug panic / release 回绕成错映射）。
        return Err("bad_button_seat".into());
    }
    let full = || SeatMap {
        to_tree: (0..n)
            .map(|op| Some(((op + n - req.button_seat as usize) % n) as u8))
            .collect(),
        phantoms: Vec::new(),
        hu: false,
    };
    if req.dealt_seats.is_empty() {
        return Ok(full());
    }
    let dealt = &req.dealt_seats;
    let k = dealt.len();
    if !(2..=n).contains(&k)
        || dealt.windows(2).any(|w| w[0] >= w[1])
        || dealt.iter().any(|&s| s as usize >= n)
    {
        return Err("bad_dealt_seats".into());
    }
    if !dealt.contains(&req.button_seat) || !dealt.contains(&req.my_seat) {
        return Err("dealt_missing_btn_or_me".into());
    }
    if k == n {
        return Ok(full());
    }
    let btn_idx = dealt
        .iter()
        .position(|&s| s == req.button_seat)
        .expect("上面 contains 已校验");
    let mut m = vec![None; n];
    if k == 2 {
        m[req.button_seat as usize] = Some(2); // OpenPoker HU：button 发 BB → 树座 2。
        m[dealt[1 - btn_idx] as usize] = Some(1); // 非 button 发 SB 先动 → 树座 1。
        return Ok(SeatMap {
            to_tree: m,
            // preflop 行动序 UTG(3)..CO(n−1)、BTN(0) 全是幻影，依次先 fold 到 SB。
            phantoms: (3..n as u8).chain(std::iter::once(0)).collect(),
            hu: true,
        });
    }
    for j in 0..k {
        let op = dealt[(btn_idx + j) % k] as usize;
        let tree = if j < 3 { j } else { j + (n - k) };
        m[op] = Some(tree as u8);
    }
    Ok(SeatMap {
        to_tree: m,
        phantoms: (3..3 + (n - k) as u8).collect(),
        hu: false,
    })
}

/// 桌面座位数 ≠ solver 座位数时的兜底原因（`not_6max` / `not_9max`；6 座与旧标签逐字相同）。
/// 不做跨座位数映射：桌面 > N 时多出来的座位在树上无处可放，桌面 < N 时占座可判与否 driver 侧
/// 未建模（`dealt_seats` 下标按桌面座位），宁可显式兜底也不静默错映射。
fn not_n_max(n: usize) -> String {
    format!("not_{n}max")
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
    /// 街，锚定面（lockstep Ok / 100BB 影子同步）且过真码深闸（[`flop_blueprint_stack_gate`]：无人
    /// all-in、有效码深 60–140BB）和尺寸闸（[`flop_blueprint_size_gate`]：本街下注映射误差
    /// 0.75–1.33）时即使命中 trigger 也走 blueprint、不实时搜索；不过闸 / 脱影子
    /// flop（lockstep 失同步）照常实时搜索（[`decide_search_unanchored`] 不经此旗）。**默认关**
    /// （`false`，旧行为 byte-equal）。turn/river 锚定面不受影响。预热（[`prewarm`]）同源 skip 锚定
    /// flop 子树（不 build+solve），否则 advisor 单线程下 hero 决策白等一个 time_budget 才拿 blueprint。
    flop_prefer_blueprint: bool,
    /// `--search-limp-as-fold`（方案 B，[`DEFAULT_SEARCH_LIMP_AS_FOLD`]）：`true` = 非 SB 开池平跟在
    /// 100BB 影子里记为弃牌、真实局照常跟注（[`lockstep_replay`]），让平跟池翻后脱锚搜索拿到非空
    /// 范围前缀（平跟者 = 该位置开池范围之外的牌、hero 和盲注 = blueprint 决策），平跟者后来弃牌时
    /// 整手回到锚定路径。**翻前决策不受影响**：有映射的翻前点一律走 [`limp_heuristic`]。
    limp_as_fold: bool,
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
    prewarm_inner(game, strategy_fn, req, base_seed, search, solve_cache, 0)
}

/// [`prewarm`] 本体。`backfill_depth` = crop 补解递归深度（[`search_with_crop_backfill`]；顶层 0）。
fn prewarm_inner(
    game: &SimplifiedNlheGame,
    strategy_fn: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    req: &Request,
    base_seed: u64,
    search: Option<&SearchRuntime>,
    solve_cache: &mut SubgameSolveCache,
    backfill_depth: u8,
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
    // solver config = blueprint 树的 config（座位数 / 盲注 / 码深唯一来源；6 座 = 旧
    // `default_6max_100bb`，逐字段相同）。
    let solver_cfg = game.table_config().clone();
    let n_seats = solver_cfg.n_seats as usize;
    if req.num_seats as usize != n_seats {
        return mk(format!("prewarm:skip:{}", not_n_max(n_seats)));
    }
    if req.big_blind == 0 {
        return mk("prewarm:skip:bad_bb".into());
    }
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
    let smap = match seat_map(req, n_seats) {
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
    let hand_seed = hand_seed_for(req, base_seed, n_seats);
    // 监控：预热 build+solve 墙钟（= 藏进对手思考时间的成本；> 对手用时则 hero 决策仍会 MISS 现解）。
    let t0 = Instant::now();
    // —— 路径分类与 decide 同源：lockstep Ok → 锚定；Err（off-stack / limp 线）→ 脱影子 ——
    // 路径分类（锚定/脱影子）一次算清；下面 flop_prefer_blueprint skip 与 build+solve 共用同一份
    // 重放结果（重算两次会无谓重放历史）。
    let lockstep = lockstep_replay(
        game,
        &solver_cfg,
        &smap,
        req,
        scale,
        board.len(),
        None,
        rt.limp_as_fold,
    );
    // `--search-flop-prefer-blueprint` on：flop **锚定**面（此处 lockstep Ok）的决策走 blueprint、不搜
    // （decide `want_search` gating 同款，[`SearchRuntime::flop_prefer_blueprint`] doc）。预热该锚定 flop
    // 子树纯属浪费，且因 advisor 单线程串行处理（build+solve 排在 hero decide 请求前），会让 hero 决策
    // 白等一个 time_budget 才拿到 blueprint。故锚定 flop 直接 skip，hero 决策即时返回。脱影子 flop
    // （lockstep Err：off-stack all-in / limp 线）decide 照常搜索、不受此旗影响 → 不在此 skip，照常预热。
    // 真码深闸同 decide（[`flop_blueprint_stack_gate`]，街起点视角）：不过闸 = decide 会搜索 → 照常预热。
    // 尺寸闸同 decide（[`flop_blueprint_size_gate`]，看到目前为止的本街下注）。街内此后才出现的
    // all-in / 偏离菜单的下注预热看不到 → 那次 decide 现解（MISS），只慢不错。
    if rt.flop_prefer_blueprint
        && round_start.street() == poker::Street::Flop
        && lockstep.as_ref().is_ok_and(|(.., sizes)| {
            flop_blueprint_stack_gate(req, &solver_cfg, scale, &smap, &round_start, my_seat_solver)
                .and_then(|()| flop_blueprint_size_gate(sizes, poker::Street::Flop))
                .is_ok()
        })
    {
        return mk("prewarm:skip:flop_prefer_blueprint".into());
    }
    // crop blueprint：range 先验缺被裁街 σ（跨街后验不可得）→ 先补解上一街再重试一次
    // （[`search_with_crop_backfill`]；完整 blueprint 下等价于直接调一次）。
    let attempt = |solve_cache: &mut SubgameSolveCache| match &lockstep {
        Ok((_real, abs, ..)) => {
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
        Err(LockstepErr {
            synced_node, limp, ..
        }) => {
            // 档一前缀 reach（生产默认开）：用已同步前缀估 range——须与决策时算出同一份 ranges 才能
            // 命中 key（hero 显式传入：range 平滑「不混」座按 hero 算，见 subgame_search_unanchored_prewarm）。
            // 平跟当弃牌映射下 hero 自己那一步的过滤与决策路径同一函数（unanchored_prefix_decisions）。
            let prefix_decisions = rt
                .unanchored_prefix_reach
                .then(|| unanchored_prefix_decisions(game, *synced_node, limp, hero_pid));
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
    let result = search_with_crop_backfill(
        game,
        strategy_fn,
        req,
        base_seed,
        rt,
        solve_cache,
        backfill_depth,
        attempt,
    );
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

// ===========================================================================
// crop blueprint：range 先验缺被裁街 σ 时按需补解上一街（再走档二′跨街后验）
// ===========================================================================

/// crop 补解失败 / 补解后仍拿不到跨街后验时的 `Err` 前缀；[`decide`] 据此标
/// `source = search_giveup:crop_no_posterior:*`（与「树太大 / 未收敛」的 `search_giveup:unsolved:*` 分桶）。
const CROP_NO_POSTERIOR: &str = "crop_no_posterior";

/// 补解递归深度上限。补解只会向更早的街走（river → turn → flop），flop 子树的先验只需翻前 σ
/// （crop 至少保留翻前）→ 最多 2 层；3 是防御上限。
const CROP_BACKFILL_MAX_DEPTH: u8 = 3;

/// 跑一次搜索 `attempt`；若因 crop blueprint 缺数据失败（[`is_blueprint_cropped_err`]：本街 range
/// 先验要查被裁街的 σ、而缓存里没有可复用的上一街子树解 → 档二′跨街后验不可得），则**补解紧前一街**
/// （[`backfill_prev_street`]：截断到上一街起点的同手请求走 [`prewarm_inner`]，把上一街子树解进缓存），
/// 再重试一次——重试时跨街后验从刚补的上一街解条件化得来，不读任何被裁街 σ。
///
/// 这是「从仍有数据的街界起点重新解」：上一街子树的先验只依赖更早的街（保留 / 或递归补解），与正常
/// 流程里 hero 上一街决策时那次搜索同一机制、同一近似等级（不是均匀替代、也不丢弃上一街动作信息）。
/// 补解不成 / 重试仍缺 → `Err("crop_no_posterior:…")`，调用方显式降级（check-when-free + 地板）。
/// 其它失败原样返回。完整 blueprint（`blueprint_max_street = River`）下守卫恒不触发 → 恰调一次
/// `attempt`，行为与引入本函数前相同。
#[allow(clippy::too_many_arguments)]
fn search_with_crop_backfill<T>(
    game: &SimplifiedNlheGame,
    strategy_fn: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    req: &Request,
    base_seed: u64,
    rt: &SearchRuntime,
    solve_cache: &mut SubgameSolveCache,
    depth: u8,
    mut attempt: impl FnMut(&mut SubgameSolveCache) -> Result<T, String>,
) -> Result<T, String> {
    let reason = match attempt(solve_cache) {
        Err(reason) if is_blueprint_cropped_err(&reason) => reason,
        other => return other,
    };
    if depth >= CROP_BACKFILL_MAX_DEPTH {
        return Err(format!("{CROP_NO_POSTERIOR}:max_depth:{reason}"));
    }
    let t0 = Instant::now();
    let filled = backfill_prev_street(
        game,
        strategy_fn,
        req,
        base_seed,
        rt,
        solve_cache,
        depth + 1,
    );
    eprintln!(
        "[openpoker_advisor] crop backfill: board_len={} depth={} result={} wall_ms={}",
        req.board.len(),
        depth + 1,
        match &filled {
            Ok(()) => "stored".to_string(),
            Err(e) => format!("failed:{e}"),
        },
        t0.elapsed().as_millis()
    );
    if let Err(e) = filled {
        return Err(format!("{CROP_NO_POSTERIOR}:backfill:{e}"));
    }
    attempt(solve_cache).map_err(|e| {
        if is_blueprint_cropped_err(&e) {
            format!("{CROP_NO_POSTERIOR}:after_backfill:{e}")
        } else {
            e
        }
    })
}

/// 补解 `req` 所在街的**紧前一街**：把同手请求截断到上一街起点（[`truncate_request_to_street`]），
/// 走 [`prewarm_inner`]（与正常预热同一条路径：锚定 / 脱锚分类、range 先验、key 计算全同源）把上一街
/// 子树解存进缓存。成功 = `prewarm:stored`；其余（skip / err）→ `Err(source)`。
#[allow(clippy::too_many_arguments)]
fn backfill_prev_street(
    game: &SimplifiedNlheGame,
    strategy_fn: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    req: &Request,
    base_seed: u64,
    rt: &SearchRuntime,
    solve_cache: &mut SubgameSolveCache,
    depth: u8,
) -> Result<(), String> {
    let prev = match req.board.len() {
        4 => poker::Street::Flop,
        5 => poker::Street::Turn,
        n => return Err(format!("no_prev_postflop_street(board_len={n})")),
    };
    let trunc = truncate_request_to_street(req, game.table_config(), prev)?;
    let resp = prewarm_inner(
        game,
        strategy_fn,
        &trunc,
        base_seed,
        Some(rt),
        solve_cache,
        depth,
    );
    if resp.source == "prewarm:stored" {
        Ok(())
    } else {
        Err(resp.source)
    }
}

/// 把同手请求截断到 `target` 街**起点**：真栈重放（同 [`build_real_auth`] 口径：真栈 config +
/// 幻影 fold + [`hist_to_concrete`]）到街首次变为 `target` 的那一刻，保留此前的动作、board 截到该街
/// 长度。其余字段（hole / 座位 / 盲注 / stacks / dealt_seats / names）不变 → `hand_seed` 不变（同手）。
/// `solver_cfg` = blueprint 树的 config（[`SimplifiedNlheGame::table_config`]）。
/// 重放失败 / 没到达 `target` → `Err`。
fn truncate_request_to_street(
    req: &Request,
    solver_cfg: &TableConfig,
    target: poker::Street,
) -> Result<Request, String> {
    let solver_bb = solver_cfg.big_blind.as_u64();
    if req.big_blind == 0 || solver_bb % req.big_blind != 0 {
        return Err("truncate_bad_scale".into());
    }
    let scale = solver_bb / req.big_blind;
    let smap = seat_map(req, solver_cfg.n_seats as usize)?;
    let real_cfg = real_stacks_config(req, solver_cfg, scale, &smap)?;
    let mut st = GameState::new(&real_cfg, REAL_REPLAY_SEED);
    let apply = |st: &mut GameState, actor: u8, action: &str, to_op: Option<u64>| {
        if st.current_player() != Some(SeatId(actor)) {
            return Err("truncate_seat_mismatch".to_string());
        }
        let to_solver = match to_op.map(|t| t.checked_mul(scale)) {
            Some(Some(v)) => Some(v),
            Some(None) => return Err("truncate_to_overflow".to_string()),
            None => None,
        };
        let concrete = hist_to_concrete(st, action, to_solver).ok_or("truncate_bad_hist")?;
        st.apply(concrete)
            .map_err(|_| "truncate_replay_illegal".to_string())
    };
    for &t in &smap.phantoms {
        apply(&mut st, t, "fold", None)?;
    }
    let board_len = expected_board_len(target);
    if req.board.len() < board_len {
        return Err("truncate_board_too_short".into());
    }
    let cut = |n: usize| Request {
        actions: req.actions[..n].to_vec(),
        board: req.board[..board_len].to_vec(),
        ..req.clone()
    };
    for (i, h) in req.actions.iter().enumerate() {
        if st.street() == target {
            return Ok(cut(i));
        }
        apply(&mut st, smap.tree_seat(h.seat)?, &h.action, h.to)?;
    }
    if st.street() == target {
        return Ok(cut(req.actions.len()));
    }
    Err("truncate_target_street_not_reached".into())
}

/// 一次下注 / 加注在 lockstep 里的尺寸映射：真实尺寸与影子（blueprint 树）里映射到的尺寸，
/// 都按 [`raise_pot_milli`] 折成池比例。[`flop_blueprint_size_gate`] 用它判断 blueprint 的
/// 应对是否还可信（映射误差大 = blueprint 在应对另一个尺寸）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BetSizeMap {
    /// 该动作所在的街（apply 前的街）。
    street: poker::Street,
    /// 真实尺寸（100BB 重放态上算）。
    real_milli: u64,
    /// 影子映射尺寸（影子自身几何上算，含塌成 AllIn 的情况）。
    mapped_milli: u64,
}

/// 当前行动者的下注几何 `(本轮最高投入, 跟注后底池)`，与抽象菜单
/// `candidate_to = max_committed + ratio × pot_after_call` 同一坐标系
/// （`DefaultActionAbstraction::abstract_actions`）。无行动者 → `None`。
fn raise_geometry(state: &poker::GameState) -> Option<(u64, u64)> {
    let actor = state.current_player()?;
    let players = state.players();
    let max_committed = players
        .iter()
        .map(|p| p.committed_this_round.as_u64())
        .max()
        .unwrap_or(0);
    let own = players[actor.0 as usize].committed_this_round.as_u64();
    Some((
        max_committed,
        state.pot().as_u64() + max_committed.saturating_sub(own),
    ))
}

/// 下注 / 加注到 `to` 的池比例（milli，1 pot = 1000）：`(to − max_committed) × 1000 / pot_after_call`
/// （几何见 [`raise_geometry`]，取该动作 apply **前**的态）。`to` 不超过本轮最高投入（跟注全下）/
/// 底池为 0 → `None`。
fn raise_pot_milli((max_committed, pot_after_call): (u64, u64), to: u64) -> Option<u64> {
    if to <= max_committed {
        return None;
    }
    (to - max_committed)
        .saturating_mul(1000)
        .checked_div(pot_after_call)
}

/// 两态 lockstep 单步（真实动作与幻影 fold 共用，保两路不漂）。行动者对齐检查（影子当前
/// 行动者 == 真实当前行动者）不在这里做：平跟当弃牌映射（[`lockstep_replay`]）下真实局可能轮到
/// 影子里已弃牌的平跟者，检查由调用方按 [`shadow_drifted`] 统一判。
///
/// 返回值：真实动作是下注 / 加注 / 主动全下时记一条 [`BetSizeMap`]（跟注全下不记——那不是尺寸
/// 选择）；其余动作 `None`。
fn lockstep_step(
    real: &mut poker::GameState,
    abs: &mut SimplifiedNlheState,
    abs_rng: &mut ChaCha20Rng,
    actor: u8,
    action: &str,
    to_solver: Option<u64>,
) -> Result<Option<BetSizeMap>, String> {
    if real.current_player() != Some(SeatId(actor)) {
        // 码深漂移 / 历史错位 → 重放对不上回合。
        return Err("replay_seat_mismatch".into());
    }
    let Some(concrete) = hist_to_concrete(real, action, to_solver) else {
        return Err("bad_hist_action".into());
    };
    // 尺寸记录要 apply 前的几何（两态各自的底池 / 本轮投入）。
    let street = real.street();
    let real_to = match concrete {
        Action::Bet { to } | Action::Raise { to } => Some(to.as_u64()),
        Action::AllIn => real.legal_actions().all_in_amount.map(|t| t.as_u64()),
        _ => None,
    };
    let real_milli = real_to
        .zip(raise_geometry(real))
        .and_then(|(to, g)| raise_pot_milli(g, to));
    let shadow_geometry = raise_geometry(&abs.game_state);
    if real.apply(concrete).is_err() {
        return Err("replay_illegal".into());
    }
    let is_all_in = real.players()[actor as usize].status == poker::PlayerStatus::AllIn;
    let Ok(mapped) = advance_shadow_by_applied(abs, concrete, is_all_in, abs_rng) else {
        // 结构性 gap（如 open-limp 进 no-limp 影子）→ 失同步（不静默改 kind）。
        return Err("structural_gap".into());
    };
    let mapped_milli = match mapped {
        AbstractAction::Bet { to, .. }
        | AbstractAction::Raise { to, .. }
        | AbstractAction::AllIn { to } => {
            shadow_geometry.and_then(|g| raise_pot_milli(g, to.as_u64()))
        }
        _ => None,
    };
    Ok(match (real_milli, mapped_milli) {
        // 真实动作是跟注全下（real_milli = None）→ 不是尺寸选择，不记。
        (Some(real_milli), Some(mapped_milli)) => Some(BetSizeMap {
            street,
            real_milli,
            mapped_milli,
        }),
        // 真实是下注 / 加注，影子却不是进攻动作：映射误差无从比较 → 按最大误差记（闸必拒）。
        (Some(real_milli), None) => Some(BetSizeMap {
            street,
            real_milli,
            mapped_milli: 0,
        }),
        (None, _) => None,
    })
}

/// 影子与真实局的当前行动者是否对不上。`ghosts` = 影子里已记为弃牌、真实局仍在手的平跟者
/// （[`LimpFoldMap`]）：真实局轮到他们时影子本就没有对应行动者，不算漂移（他们下一个动作由
/// [`lockstep_replay`] 单独处理）。`ghosts` 为空时 = 旧的逐步检查。
fn shadow_drifted(real: &poker::GameState, abs: &SimplifiedNlheState, ghosts: &[u8]) -> bool {
    match real.current_player() {
        Some(SeatId(s)) if ghosts.contains(&s) => false,
        cur => abs.game_state.current_player() != cur,
    }
}

/// 该动作是否是「平跟当弃牌」要接的平跟：翻前、真实动作是不全下的 call、影子当前节点没有
/// Call（no-limp 树删掉的开池 / 跟入平跟）但有 Fold。只看影子合法集，不看真实局别的状态——
/// 影子缺 Call 只会出现在「前面没人加注、只面对大盲」的节点（面对加注时 Call 恒在树内）。
fn is_limp_gap(
    real: &poker::GameState,
    abs: &SimplifiedNlheState,
    actor: u8,
    action: &str,
) -> bool {
    if action != "call" || real.street() != poker::Street::Preflop {
        return false;
    }
    if real.current_player() != Some(SeatId(actor)) {
        return false; // 交给 lockstep_step 报 replay_seat_mismatch
    }
    // 跟注会令行动者全下（码深 ≤ 1BB）→ 不是普通平跟，保持结构性 gap。
    let stack = real.players()[actor as usize].stack.as_u64();
    let committed = real.players()[actor as usize].committed_this_round.as_u64();
    let call_to = real.legal_actions().call.map_or(0, |c| c.as_u64());
    if call_to.saturating_sub(committed) >= stack {
        return false;
    }
    let legal_abs = SimplifiedNlheGame::legal_actions(abs);
    find_tag(&legal_abs, AbstractActionTag::Call).is_none()
        && find_tag(&legal_abs, AbstractActionTag::Fold).is_some()
}

/// 方案 B（平跟当弃牌映射，`docs/temp/openpoker_limp_handling_research_2026_10_16.md` §3/§4）的
/// 记录：每次映射的 `(平跟者的决策节点, 平跟者 tree seat)`。节点 = 影子里平跟者面对的那个节点，
/// 影子在该节点走的是 Fold、真实局走的是 Call。空 = 本手没有发生映射。
///
/// 用途只有两个：① 翻前有映射 → 决策交给 [`limp_heuristic`]（翻前要不要加注、拿什么牌加不由
/// blueprint 决定）；② 翻后脱锚搜索估 range 时，hero 自己被映射的那一步从前缀里拿掉
/// （[`unanchored_prefix_decisions`]）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LimpFoldMap {
    folds: Vec<(NodeId, PlayerId)>,
}

impl LimpFoldMap {
    fn mapped(&self) -> bool {
        !self.folds.is_empty()
    }
}

/// lockstep 失同步结果：原因 + **已同步前缀**的影子节点（断点**之前**最后一个对齐的
/// `current_node_id`）。脱影子搜索的档一前缀 reach 用它取已同步前缀的决策三元组
/// （[`synced_prefix_decisions`]）估 range（`unanchored_range_design` §1）。`synced_node` 在每步
/// 失败**前**捕获 = 所有成功步之后的影子节点（断点动作及其后按无信息处理，因子 1）。
/// `limp` = 失同步前已做的平跟当弃牌映射（`--search-limp-as-fold` 关时恒空）。
#[derive(Debug)]
struct LockstepErr {
    reason: String,
    synced_node: NodeId,
    limp: LimpFoldMap,
}

/// [`lockstep_replay`] 成功结果：(100BB 真实重放态, 影子, 平跟当弃牌映射, 下注尺寸映射)。
type LockstepOk = (
    poker::GameState,
    SimplifiedNlheState,
    LimpFoldMap,
    Vec<BetSizeMap>,
);

/// 两态 lockstep 重放（[`decide`] 决策路径与 [`prewarm`] 预热共用——**锚定/脱影子分类与
/// node_id 必须同源**，否则预热可能走错路径 / 推错 key 静默失效）。`my_turn = Some(seat)` →
/// 末尾校验轮到该座（决策路径，旧行为逐字保留）；`None` → 重放到历史末尾即可（预热：该街
/// hero 行动前，轮到的是首行动者）。失同步返回 [`LockstepErr`]（原因 + 已同步前缀节点）。
///
/// `limp_as_fold`（方案 B，`--search-limp-as-fold`）：`false` = 旧行为逐位不变（平跟进 no-limp
/// 影子即 `structural_gap`）。`true` = 平跟（[`is_limp_gap`]）在影子里记为弃牌、真实局照常跟注，
/// 平跟者成为「影子已弃、真实在手」的座位，之后：
/// - 平跟者面对加注弃牌 → 只推真实局，两边重新一致（6 月数据 90% 是这种），整手回到锚定路径；
/// - 平跟者再跟注 / 加注 → 失同步 `limp_as_fold:limper_continued`，`synced_node` = 此前全部
///   已同步动作（含平跟者那一步 Fold）；
/// - 平跟者还在手就进了翻牌（如 SB 补齐 + BB 过牌）→ 失同步 `limp_as_fold:limper_in_pot`，
///   `synced_node` 取这一步**之后**（该步两边动作相同，翻前决策全部进前缀）；
/// - 其余漂移（如平跟后其余人全弃、影子已结束但真实局轮到 BB）→ 原因加 `limp_as_fold:` 前缀。
///
/// 返回的 [`LimpFoldMap`] 让调用方知道翻前发生过映射（翻前决策仍交给启发式）。翻后 Ok 时
/// 保证没有「影子已弃、真实在手」的座位（进翻牌前已拦下）。返回的 `Vec<BetSizeMap>` = 历史里
/// 每次下注 / 加注的真实与映射尺寸（按时间序，[`flop_blueprint_size_gate`] 用）。
#[allow(clippy::too_many_arguments)]
fn lockstep_replay(
    game: &SimplifiedNlheGame,
    solver_cfg: &TableConfig,
    smap: &SeatMap,
    req: &Request,
    scale: u64,
    board_len: usize,
    my_turn: Option<SeatId>,
    limp_as_fold: bool,
) -> Result<LockstepOk, LockstepErr> {
    let mut real = poker::GameState::new(solver_cfg, REAL_REPLAY_SEED);
    let mut abs_rng = ChaCha20Rng::from_seed(ABS_REPLAY_SEED);
    let mut abs: SimplifiedNlheState = game.root(&mut abs_rng);
    // synced_node 在每步**前**捕获（abs 只在 lockstep_step 内推进）→ 失败时 = 断点前最后对齐节点
    // （所有成功步之后的影子节点；断点动作及其后按无信息处理，因子 1）。
    // 短桌：幻影座（UTG 起的最早行动位）开局先 fold → 走到 blueprint 树的真实节点。
    let mut limp = LimpFoldMap::default();
    for &t in &smap.phantoms {
        let synced_node = abs.current_node_id;
        let phantom_err = |e: String| LockstepErr {
            reason: format!("phantom_{e}"),
            synced_node,
            limp: LimpFoldMap::default(),
        };
        lockstep_step(&mut real, &mut abs, &mut abs_rng, t, "fold", None).map_err(phantom_err)?;
        if shadow_drifted(&real, &abs, &[]) {
            return Err(phantom_err("lockstep_drift".into()));
        }
    }
    // 影子已弃、真实在手的平跟者（tree seat）。
    let mut ghosts: Vec<u8> = Vec::new();
    let mut sizes: Vec<BetSizeMap> = Vec::new();
    for h in &req.actions {
        let synced_node = abs.current_node_id;
        let err =
            |reason: String, synced_node: NodeId, ghosts: &[u8], limp: &LimpFoldMap| LockstepErr {
                reason: if ghosts.is_empty() {
                    reason
                } else {
                    format!("limp_as_fold:{reason}")
                },
                synced_node,
                limp: limp.clone(),
            };
        let actor = smap
            .tree_seat(h.seat)
            .map_err(|e| err(e, synced_node, &ghosts, &limp))?;
        let to_solver = h.to.map(|t| t * scale);
        if let Some(pos) = ghosts.iter().position(|&g| g == actor) {
            // 影子里已弃牌的平跟者再行动：只有弃牌能让两边重新一致。
            if h.action != "fold" {
                return Err(err("limper_continued".into(), synced_node, &ghosts, &limp));
            }
            if real.current_player() != Some(SeatId(actor)) {
                return Err(err(
                    "replay_seat_mismatch".into(),
                    synced_node,
                    &ghosts,
                    &limp,
                ));
            }
            if real.apply(Action::Fold).is_err() {
                return Err(err("replay_illegal".into(), synced_node, &ghosts, &limp));
            }
            ghosts.remove(pos);
        } else if limp_as_fold && is_limp_gap(&real, &abs, actor, &h.action) {
            // 平跟当弃牌：真实局 call、影子 fold（kind 变了，必须显式记录，见 LimpFoldMap）。
            if real.apply(Action::Call).is_err() {
                return Err(err("replay_illegal".into(), synced_node, &ghosts, &limp));
            }
            if advance_shadow_by_applied(&mut abs, Action::Fold, false, &mut abs_rng).is_err() {
                return Err(err("structural_gap".into(), synced_node, &ghosts, &limp));
            }
            limp.folds.push((synced_node, actor as PlayerId));
            ghosts.push(actor);
        } else {
            let size = lockstep_step(
                &mut real,
                &mut abs,
                &mut abs_rng,
                actor,
                &h.action,
                to_solver,
            )
            .map_err(|e| err(e, synced_node, &ghosts, &limp))?;
            sizes.extend(size);
        }
        if !ghosts.is_empty() && real.street() != poker::Street::Preflop {
            // 平跟者还在手就进了翻后：影子少一个在手座位，翻后不能当锚定用。这一步本身两边
            // 动作相同 → 取这一步之后的节点（翻前决策全部进前缀）。
            return Err(err(
                "limper_in_pot".into(),
                abs.current_node_id,
                &ghosts,
                &limp,
            ));
        }
        if shadow_drifted(&real, &abs, &ghosts) {
            return Err(err("lockstep_drift".into(), synced_node, &ghosts, &limp));
        }
    }
    // —— 到我方决策点（决策路径）/ 历史末尾（预热）——
    if let Some(seat) = my_turn {
        if real.current_player() != Some(seat) {
            return Err(LockstepErr {
                reason: "not_my_turn".into(),
                synced_node: abs.current_node_id,
                limp,
            });
        }
    }
    let street = real.street();
    if abs.game_state.street() != street || board_len != expected_board_len(street) {
        return Err(LockstepErr {
            reason: "street_board_mismatch".into(),
            synced_node: abs.current_node_id,
            limp,
        });
    }
    Ok((real, abs, limp, sizes))
}

/// 脱锚搜索档一前缀 reach 的决策三元组（[`decide_search_unanchored`] 与 [`prewarm`] 共用，两路
/// 必须算出同一份才能命中 solve 缓存 key）。在 [`synced_prefix_decisions`] 基础上去掉 **hero 自己**
/// 被平跟当弃牌映射的那一步：hero 平跟的牌由 [`limp_heuristic`] 决定（跟入平跟档），按 blueprint
/// 在该节点的弃牌概率算会把 hero 真实底牌所在的牌型压到接近 0；拿掉 = 这一步按无信息处理（因子 1）。
/// 对手平跟者那一步 Fold 保留：P(弃牌 | 牌) = 「该位置开池范围之外的牌」，正是调研 §4 建议的
/// 平跟者范围先验（再经 `range_uniform_mix` 混入均匀兜底）。
fn unanchored_prefix_decisions(
    game: &SimplifiedNlheGame,
    synced_node: NodeId,
    limp: &LimpFoldMap,
    hero: PlayerId,
) -> Vec<(NodeId, AbstractActionTag, PlayerId)> {
    let mut d = synced_prefix_decisions(game, synced_node);
    d.retain(|&(node, tag, seat)| {
        !(seat == hero && tag == AbstractActionTag::Fold && limp.folds.contains(&(node, seat)))
    });
    d
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
    // solver config = blueprint 树的 config（座位数 / 盲注 / 码深唯一来源；6 座 = 旧
    // `default_6max_100bb`，逐字段相同 → 6 座 byte-equal）。
    let solver_cfg = game.table_config().clone();
    let n_seats = solver_cfg.n_seats as usize;
    if req.num_seats as usize != n_seats {
        return safe_fallback(&req.valid, &not_n_max(n_seats));
    }
    if req.big_blind == 0 {
        return safe_fallback(&req.valid, "bad_bb");
    }
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
    let smap = match seat_map(req, n_seats) {
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

    // —— 翻前面对全下（审查 §5）：有效码深 < 60BB 不走 blueprint（100BB 影子把短码全下当 100BB
    // 全下，底池赔率 / 对手范围都错）→ [`preflop_allin_response`]。码深够的全下照常往下走；走到
    // 下面翻前兜底分支（平跟映射 / 失同步 / blueprint 占位）时同样改用它，不再只跟 P 档。——
    let allin_spot = if board.is_empty() {
        preflop_allin_spot(req, &solver_cfg, scale, &smap, my_seat_solver, hole)
    } else {
        None
    };
    let allin_response = |spot: &PreflopAllinSpot| {
        preflop_allin_response(
            req,
            hole,
            spot,
            solver_cfg.n_seats,
            sample_seed(req, base_seed, n_seats),
        )
    };
    if let Some(spot) = allin_spot.as_ref().filter(|s| preflop_allin_too_short(s)) {
        dlog!("preflop allin spot (eff too short) → heuristic: {spot:?}");
        return allin_response(spot);
    }

    // —— 两态 lockstep 重放（blueprint 路径 + 锚定搜索的 node_id / legal_abs 来源；与
    // prewarm 共用 [`lockstep_replay`]，分类 / node_id 同源）——
    // `--search-limp-as-fold`（方案 B）只在开搜索时生效：它只为翻后搜索提供范围前缀。
    let limp_as_fold = search.is_some_and(|rt| rt.limp_as_fold);
    let lockstep = lockstep_replay(
        game,
        &solver_cfg,
        &smap,
        req,
        scale,
        board.len(),
        Some(my_seat_solver),
        limp_as_fold,
    )
    .and_then(|(real, abs, limp, sizes)| {
        let legal_abs = SimplifiedNlheGame::legal_actions(&abs);
        if legal_abs.is_empty() {
            // empty_legal = 终局态（非失同步）；synced_node 取当前节点（前缀走旧分流即可）。
            return Err(LockstepErr {
                reason: "empty_legal".into(),
                synced_node: abs.current_node_id,
                limp,
            });
        }
        Ok((real, abs, legal_abs, limp, sizes))
    });
    let (real, abs, legal_abs, sizes) = match lockstep {
        // 平跟当弃牌映射后影子在翻前对上了 blueprint 节点，但翻前要不要加注、拿什么牌加仍由
        // 启发式决定（调研 §4：blueprint 开池范围默认后面的人会弃牌，前面多了一个平跟者照搬会偏宽）。
        Ok((_, _, _, limp, _)) if limp.mapped() && board.is_empty() => {
            if let Some(spot) = &allin_spot {
                return allin_response(spot);
            }
            if let Some(resp) =
                limp_preflop_response(req, hole, &solver_cfg, scale, &smap, my_seat_solver)
            {
                return resp;
            }
            // limp_heuristic 只在历史里没有首个加注前的 call 时返回 None；有映射就必有平跟，
            // 不可达。万一到达 → 与失同步同口径的兜底，绝不让 blueprint 接管翻前。
            return fallback_with_floor(
                &req.valid,
                hole,
                &board,
                preflop_pot_odds(req, &solver_cfg, scale, &smap, my_seat_solver, hole),
                "fallback:limp_as_fold_no_limp".into(),
            );
        }
        Ok((real, abs, legal_abs, _, sizes)) => (real, abs, legal_abs, sizes),
        Err(LockstepErr {
            reason,
            synced_node,
            limp,
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
                        &limp,
                        solve_cache,
                    );
                }
            }
            // 翻前面对全下（如 blueprint 树里没有的开池全下）→ 全下启发式。
            if let Some(spot) = &allin_spot {
                return allin_response(spot);
            }
            // preflop open-limp 结构 gap（或平跟当弃牌映射之后的失同步）→ 启发式（[`limp_heuristic`]，
            // 面对加注的弃牌出口内含底池赔率 floor）。
            if (reason == "structural_gap" || limp.mapped()) && board.is_empty() {
                if let Some(resp) =
                    limp_preflop_response(req, hole, &solver_cfg, scale, &smap, my_seat_solver)
                {
                    return resp;
                }
            }
            // preflop 失同步底池赔率 floor（2026-06-16 用户加）：真栈重放算 pot/to_call（限
            // preflop——postflop 失同步上面已 return 脱影子搜索 / 维持旧兜底，board 非空 → None）。
            let pre_pot_odds = if board.is_empty() {
                preflop_pot_odds(req, &solver_cfg, scale, &smap, my_seat_solver, hole)
            } else {
                None
            };
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
    // 且过真码深闸（[`flop_blueprint_stack_gate`]：无人 all-in + 有效码深在区间内）和尺寸闸
    // （[`flop_blueprint_size_gate`]：本街下注映射误差在区间内）才抑制搜索回 blueprint；真栈建不出
    // 视同不过闸。脱影子 flop 走上面 `decide_search_unanchored`，不经此 gating。
    let flop_bp_gate = match search {
        Some(rt) if rt.flop_prefer_blueprint && street == poker::Street::Flop => Some(
            build_real_auth(
                req,
                &solver_cfg,
                scale,
                &smap,
                my_seat_solver,
                hole,
                &board,
                true,
            )
            .and_then(|(auth, ..)| {
                flop_blueprint_stack_gate(req, &solver_cfg, scale, &smap, &auth, my_seat_solver)
            })
            .and_then(|()| flop_blueprint_size_gate(&sizes, street)),
        ),
        _ => None,
    };
    if let Some(Err(reason)) = &flop_bp_gate {
        dlog!("flop_prefer_blueprint gate FAIL: {reason} → search");
    }
    let want_search = matches!(search, Some(rt) if should_search(&real, rt.cfg.trigger))
        && !matches!(flop_bp_gate, Some(Ok(())));
    // 不搜索的决策先查 blueprint。crop blueprint 守卫：不搜索的决策只能查保留街的 σ。启动期约束
    // （validate_crop_runtime）保证被裁街恒触发搜索，此处不可达；万一到达 → 显式兜底，绝不把缺失
    // σ 当均匀用。完整 blueprint（River）恒不触发（byte-equal）。
    let bp_dist: Option<Option<Vec<(AbstractAction, f64)>>> = if want_search {
        None
    } else {
        let bp_max = search.map_or(StreetTag::River, |rt| rt.cfg.blueprint_max_street);
        if !matches!(street_tag_of(street), Some(t) if t as u8 <= bp_max as u8) {
            return fallback_with_floor(
                &req.valid,
                hole,
                &board,
                None,
                format!("fallback:blueprint_cropped:{}", street_label(street)),
            );
        }
        Some(blueprint_distribution(&info, &legal_abs, strategy_fn))
    };
    // blueprint 该信息集只是均匀占位（[`Blueprint::strategy`] 返回空）：不按均匀出牌。翻后且开了
    // 搜索 → 改走锚定搜索（与触发面命中时同一路径；没有预热，现解）；翻前 / 未开搜索 → 兜底。
    let bp_placeholder = matches!(bp_dist, Some(None));
    let placeholder_search = bp_placeholder && search.is_some() && !board.is_empty();
    if bp_placeholder && !placeholder_search {
        if let Some(spot) = &allin_spot {
            return allin_response(spot);
        }
        let pot_odds = board
            .is_empty()
            .then(|| preflop_pot_odds(req, &solver_cfg, scale, &smap, my_seat_solver, hole))
            .flatten();
        return fallback_with_floor(
            &req.valid,
            hole,
            &board,
            pot_odds,
            "fallback:blueprint_placeholder".into(),
        );
    }
    let want_search = want_search || placeholder_search;
    dlog!(
        "lockstep OK: street={:?} node_id={node_id:?} info_set={} legal_abs={:?} path={}",
        street,
        info.raw(),
        legal_abs,
        if placeholder_search {
            "search (anchored, blueprint placeholder)"
        } else if want_search {
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
        let hand_seed = hand_seed_for(req, base_seed, n_seats);
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
        // crop blueprint：本街 range 先验缺被裁街 σ（跨街后验不可得）→ 补解上一街后重试一次
        // （search_with_crop_backfill doc）；完整 blueprint 下恰调一次（行为不变）。
        let search_result = search_with_crop_backfill(
            game,
            strategy_fn,
            req,
            base_seed,
            rt,
            solve_cache,
            0,
            |cache| {
                subgame_search_cached(
                    Some(cache), // within-round solve 缓存：同手同街命中 → 复用 solve 只重导航。
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
                )
            },
        );
        match search_result {
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
            // crop 缺后验（补解不成）单独分桶 `search_giveup:crop_no_posterior:*`。
            Err(reason) => {
                // auth 已建（此 Err 分支未 move 走）→ 喂底池赔率 floor。
                return fallback_with_floor(
                    &req.valid,
                    hole,
                    &board,
                    pot_odds_from_auth(&auth, &solver_cfg),
                    giveup_source("unsolved", &reason),
                );
            }
        }
    } else {
        bp_dist
            .flatten()
            .expect("非搜索路径 bp_dist 已算出且非占位（占位已在上面 return / 改走搜索）")
    };

    // outgoing 基准态：搜索 → 真栈 auth（真码深尺寸）；blueprint → 100BB real（旧行为）。
    let outgoing_state: &GameState = auth_holder.as_ref().unwrap_or(&real);
    // outgoing 抽象：缺口③ 深码搜索 → 与子树同一菜单（deep_abs_holder，SPR 自适应）；否则
    // blueprint 抽象（旧行为；search=None / 非 deep 路径 byte-equal 不受影响）。
    let outgoing_abs: &StreetActionAbstraction = deep_abs_holder.as_ref().unwrap_or(abstraction);

    // per-decision 确定性采样（保混合策略 + 可复现；seed 与搜索与否无关 → search=None byte-equal）。
    let mut sample_rng = ChaCha20Rng::from_seed(sample_seed(req, base_seed, n_seats));
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
    let (mut resp, degrade) = action_to_response(solver_action, scale, &req.valid);
    if resp.source.is_empty() {
        // 合法动作：填 source（search / blueprint）+ 诊断。不合法时 action_to_response 已产
        // safe_fallback（source = fallback:...），不覆盖。bet/raise 退成 call/check 时追加降级标记。
        resp.source = format!(
            "{}{}",
            if placeholder_search {
                "search:bp_placeholder"
            } else if want_search {
                "search"
            } else {
                "blueprint"
            },
            degrade.unwrap_or("")
        );
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

/// blueprint 平均策略 → 归一 `(action, prob)`。`None` = 该信息集没有可用策略（空 / 长度不符 /
/// 无正概率；生产 [`Blueprint::strategy`] 对均匀占位行也返回空）——调用方不得按均匀出牌
/// （2026-10-16 run3：JJ 在 3 人池拿到 check / bet / allin 各 1/3）。
fn blueprint_distribution(
    info: &InfoSetId,
    legal_abs: &[AbstractAction],
    strategy_fn: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
) -> Option<Vec<(AbstractAction, f64)>> {
    let raw = strategy_fn(info, legal_abs.len());
    if raw.len() != legal_abs.len() || !raw.iter().any(|p| p.is_finite() && *p > 0.0) {
        return None;
    }
    let sum: f64 = raw.iter().filter(|p| p.is_finite() && **p > 0.0).sum();
    Some(
        legal_abs
            .iter()
            .copied()
            .zip(raw)
            .filter(|(_, p)| p.is_finite() && *p > 0.0)
            .map(|(a, p)| (a, p / sum))
            .collect(),
    )
}

/// solver [`Action`] → OpenPoker [`Response`]（blueprint / search 共享）。合法动作 → `source` 留空
/// （caller 填 blueprint/search + 诊断）；不合法（check/call 不可用、或 bet/raise 连 call/check 都
/// 退不了）→ 直接 [`safe_fallback`]（`source` 已填 fallback:...，caller 不覆盖）。尺寸按传入的
/// `scale` ÷ 真实下注（caller 已用真栈或 100BB 算出 solver `to`）。
///
/// 第二个返回值 = 降级标记：策略选了 Bet/Raise 但桌面不给加注区间（对手已 all-in / 100BB 影子与
/// 真栈不一致 / 脏区间）时，按「最接近的合法动作」退成 call（能跟）或 check（能过），`source` 仍留空
/// 走正常诊断，caller 在填好的 source 后追加此标记（`:raise_as_call` / `:raise_as_check`）供统计。
/// 旧行为是直接 check/fold——想加注的强牌面对下注会被扔掉。
fn action_to_response(
    action: Action,
    scale: u64,
    valid: &ValidActions,
) -> (Response, Option<&'static str>) {
    let resp = match action {
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
                None if valid.can_call => {
                    let resp = Response {
                        action: "call".into(),
                        ..Default::default()
                    };
                    return (resp, Some(":raise_as_call"));
                }
                None if valid.can_check => {
                    let resp = Response {
                        action: "check".into(),
                        ..Default::default()
                    };
                    return (resp, Some(":raise_as_check"));
                }
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
    };
    (resp, None)
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

/// 平跟池启发式里 hero 的位置（tree seat 口径：0=BTN 1=SB 2=BB 3..=UTG..CO；短桌幻影座占最早
/// 的位置，真实座位的 tree seat 仍按这个口径，[`seat_map`]）。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LimpPos {
    Btn,
    Sb,
    Bb,
    /// 非盲位、按钮之前还有 `k` 个座位（CO=1、HJ=2、LJ=3……）。
    BeforeBtn(u8),
}

fn limp_pos(tree_seat: u8, n_seats: u8) -> LimpPos {
    match tree_seat {
        0 => LimpPos::Btn,
        1 => LimpPos::Sb,
        2 => LimpPos::Bb,
        t => LimpPos::BeforeBtn(n_seats.saturating_sub(t).max(1)),
    }
}

/// 一家平跟、前面没人加注时各位置的范围（Chen 序 top 比例，[`chen_top_fraction`]）：
/// `(隔离加注, 被动入池上限)`。被动入池 = 非盲位跟着平跟 / SB 补齐；`0.0` = 不被动入池
/// （BB 免费过牌另算）。只有 CO / BTN 允许跟着平跟（翻后有位置）。
///
/// 数值是手写的常识范围（调研 §2 估算用的 BTN ≈ 30%、CO ≈ 22%），不是解出来的；偏价值方向——
/// 平跟者常常不弃（调研 §2 jbot 面对加注 18/18 跟注），偷盲那部分牌不该照 blueprint 开池范围加。
fn limp_ranges(pos: LimpPos) -> (f64, f64) {
    match pos {
        LimpPos::Btn => (0.30, 0.45),
        LimpPos::BeforeBtn(1) => (0.22, 0.32),
        LimpPos::BeforeBtn(2) => (0.16, 0.0),
        LimpPos::BeforeBtn(3) => (0.13, 0.0),
        LimpPos::BeforeBtn(_) => (0.11, 0.0),
        LimpPos::Sb => (0.12, 0.40),
        LimpPos::Bb => (0.12, 0.0),
    }
}

/// 每多一家平跟，隔离加注范围收紧 20%（最多收到一半）；SB 补齐上限放宽 5 个百分点（赔率更好，
/// 最多 55%）。非盲位跟着平跟的上限不变（隔离范围收紧后，多出来的牌自然落进跟注档）。
fn limp_ranges_for(pos: LimpPos, limps: u64) -> (f64, f64) {
    let (iso, passive) = limp_ranges(pos);
    let extra = limps.saturating_sub(1) as f64;
    let iso = iso * (1.0 - 0.2 * extra).max(0.5);
    let passive = if pos == LimpPos::Sb {
        (passive + 0.05 * extra).min(0.55)
    } else {
        passive
    };
    (iso, passive)
}

/// 起手牌强度序的排序键：Chen 分（[`chen_score`]）为主；Chen 分并列很多（一个并列块可达 90+
/// 组合，只按 Chen 取「top 30%」实际会落到 40%），用 对子 > 同花 > 高牌大 > 踢脚大 确定性拆开，
/// 让 [`chen_top_fraction`] 的宽度贴近名义值（误差不超过一个牌型，≤ 12 组合）。
fn chen_order_key(hole: [Card; 2]) -> (f64, bool, bool, u8, u8) {
    let (r0, r1) = (hole[0].rank() as u8, hole[1].rank() as u8);
    let (hi, lo) = if r0 >= r1 { (r0, r1) } else { (r1, r0) };
    (
        chen_score(hole),
        hi == lo,
        hole[0].suit() == hole[1].suit(),
        hi,
        lo,
    )
}

/// hero 底牌在 1326 个组合里按 [`chen_order_key`] 的位置：排在它前面的组合占比（`0.0` = 最强）。
/// 手牌在「top p」范围内 ⇔ 返回值 `< p`。不做 card removal（范围是按位置定的常识值，精度够用）。
fn chen_top_fraction(hole: [Card; 2]) -> f64 {
    let hero = chen_order_key(hole);
    let mut stronger = 0u32;
    let mut total = 0u32;
    for a in 0u8..52 {
        for b in (a + 1)..52 {
            let h = [
                Card::from_u8(a).expect("0..52 是合法 Card"),
                Card::from_u8(b).expect("0..52 是合法 Card"),
            ];
            total += 1;
            if chen_order_key(h).partial_cmp(&hero) == Some(std::cmp::Ordering::Greater) {
                stronger += 1;
            }
        }
    }
    f64::from(stronger) / f64::from(total)
}

/// preflop open-limp 池启发式（方案 A，`docs/temp/openpoker_limp_handling_research_2026_10_16.md`
/// §3/§4）：blueprint 树不含非 SB 开池平跟节点，翻前遇到平跟就由这里决定；翻后交给脱锚搜索。
/// `--search-limp-as-fold`（方案 B）开着时翻前也一样走这里——B 只给翻后搜索提供范围前缀。
///
/// - **没人加注**（只有平跟）：按位置 × 平跟人数取范围（[`limp_ranges_for`]）。
///   隔离加注档 → raise to `(3 + 平跟人数 + OOP 1)×BB`（OOP = hero 在盲位；clamp 进 [min,max]）；
///   被动档 → CO/BTN 跟着平跟（`overlimp`）、SB 补齐（`complete`）；其余 → BB 免费过牌，否则弃牌。
///   **这一支不套底池赔率 floor**：一家平跟时 SB 赔率 5:1、三家平跟时任何位置赔率都 > 4，套了就是
///   任意两张都跟（2026-10-16 抽查 §3 的 SB 任意两张补齐就是这样来的）。
/// - **有人加注**（含 hero 隔离加注或跟着平跟之后被加注）：P 档（[`limp_hand_tier`]==2）→ call；
///   其余弃牌，但底池赔率 floor（[`pot_odds_floor_hit`]，`pot_odds` 由调用方真栈重放算）命中 → call。
///
/// 返回 `None` = 历史里首个加注前没有 call（其他结构 gap 误入的防御）→ caller 维持原兜底。
/// `source = limp_heuristic:{raise,overlimp,complete,check,fold,call,pot_odds_call}`
/// （driver 按 `limp_heuristic` 前缀单独分桶）。
fn limp_heuristic(
    req: &Request,
    hole: [Card; 2],
    pos: LimpPos,
    pot_odds: Option<PotOdds>,
) -> Option<Response> {
    let mut limps = 0u64;
    let mut raised = false;
    for h in &req.actions {
        match h.action.as_str() {
            "call" if !raised => limps += 1, // 首个 raise 前的 call = open-limp / over-limp / SB 补齐
            "raise" | "all_in" | "bet" => raised = true,
            _ => {}
        }
    }
    if limps == 0 {
        return None;
    }
    let valid = &req.valid;
    let mk = |action: &str, amount: Option<u64>, case: &str| Response {
        action: action.into(),
        amount,
        source: format!("limp_heuristic:{case}"),
        street: Some("preflop".into()),
        ..Default::default()
    };
    if !raised {
        let (iso, passive) = limp_ranges_for(pos, limps);
        let frac = chen_top_fraction(hole);
        if frac < iso && valid.can_raise {
            if let (Some(min), Some(max)) = (valid.min_raise, valid.max_raise) {
                if min <= max {
                    // OpenPoker raise amount = 总 to 额；脏区间（min>max）走下面被动分支。
                    let oop = u64::from(matches!(pos, LimpPos::Sb | LimpPos::Bb));
                    let to = (3 + limps + oop) * req.big_blind;
                    return Some(mk("raise", Some(to.clamp(min, max)), "raise"));
                }
            }
        }
        if valid.can_check {
            return Some(mk("check", None, "check")); // BB 免费过牌
        }
        if frac < passive.max(iso) && valid.can_call {
            let case = if pos == LimpPos::Sb {
                "complete"
            } else {
                "overlimp"
            };
            return Some(mk("call", None, case));
        }
        return Some(mk("fold", None, "fold"));
    }
    if limp_hand_tier(hole) == 2 && valid.can_call {
        return Some(mk("call", None, "call"));
    }
    if valid.can_check {
        return Some(mk("check", None, "check")); // 防御（preflop 面对加注不该 can_check）
    }
    if valid.can_call && pot_odds.as_ref().is_some_and(pot_odds_floor_hit) {
        return Some(mk("call", None, "pot_odds_call"));
    }
    Some(mk("fold", None, "fold"))
}

/// [`limp_heuristic`] 的调用入口：算 hero 位置 + 真栈底池赔率（[`preflop_pot_odds`]）。`None` 同
/// [`limp_heuristic`]（历史里没有平跟）。
fn limp_preflop_response(
    req: &Request,
    hole: [Card; 2],
    solver_cfg: &TableConfig,
    scale: u64,
    smap: &SeatMap,
    my_seat_solver: SeatId,
) -> Option<Response> {
    let pos = limp_pos(my_seat_solver.0, solver_cfg.n_seats);
    let pot_odds = preflop_pot_odds(req, solver_cfg, scale, smap, my_seat_solver, hole);
    limp_heuristic(req, hole, pos, pot_odds)
}

// ===========================================================================
// 翻前面对全下的兜底启发式（`openpoker_code_review_findings_2026_10_16.md` §5，兜底改造第一步）
// ===========================================================================

/// 翻前面对全下时，hero 与全下者的有效码深（两人 hand-start 真栈取小）低于此值（BB）就不走
/// blueprint、改走 [`preflop_allin_response`]：blueprint 按对称 100BB 训练，短码全下在 100BB
/// 影子里就是 100BB 全下——底池赔率和对手范围都按 100BB 算，跟注范围严重偏紧。与翻牌码深闸
/// 下限（[`FLOP_BLUEPRINT_EFF_STACK_BB_MIN`]）同值。码深 ≥ 此值的全下仍走 blueprint；blueprint
/// 树里没有的全下（开池全下，`drop_preflop_open_allin`）重放失同步后同样走这里。
const PREFLOP_ALLIN_BLUEPRINT_MIN_EFF_BB: u64 = 60;
/// 全下范围宽度表（起手牌强度序 [`chen_order_key`] 的 top 比例）：`(全下者 hand-start 码深上限 BB,
/// 开池全下, 3bet/4bet 全下)`，按行从上往下取第一个「码深 ≤ 上限」的行；超过最后一行按最后一行。
/// **数值是估计**（2026-10-16 与用户确认的常识值），不是解出来的，也没有实盘数据校准。
const ALLIN_RANGE_TABLE: [(u64, f64, f64); 5] = [
    (10, 0.40, 0.25),
    (15, 0.30, 0.18),
    (25, 0.20, 0.12),
    (40, 0.12, 0.08),
    (60, 0.08, 0.05),
];
/// 已经跟了全下的其他对手：范围按全下者宽度的这个比例收紧（跟注范围比全下范围紧，估计值）。
const ALLIN_CALLER_RANGE_FACTOR: f64 = 0.5;
/// 跟注要求的胜率余量：胜率 ≥ 所需底池赔率 + 余量才跟（范围是猜的，留一点保守）；每多一个已跟注
/// 的对手再加一份（多人底池范围误差叠加）。
const ALLIN_CALL_MARGIN: f64 = 0.02;
const ALLIN_CALL_MARGIN_PER_CALLER: f64 = 0.02;
/// 胜率 Monte Carlo 样本数（标准误约 0.5%；release 下单次几毫秒）。
const ALLIN_EQUITY_SAMPLES: u32 = 10_000;

/// 全下者位置对范围宽度的乘数：位置越靠前越紧，按钮 / SB 偷盲位最宽（估计值）。
fn allin_position_factor(pos: LimpPos) -> f64 {
    match pos {
        LimpPos::Btn | LimpPos::Sb => 1.2,
        LimpPos::Bb | LimpPos::BeforeBtn(1) => 1.0,
        LimpPos::BeforeBtn(2) => 0.85,
        LimpPos::BeforeBtn(_) => 0.7,
    }
}

/// 全下者范围宽度（top 比例，`(0, 1]`）：[`ALLIN_RANGE_TABLE`] × [`allin_position_factor`]。
fn allin_range_width(shover_start: u64, big_blind: u64, open_shove: bool, pos: LimpPos) -> f64 {
    let last = ALLIN_RANGE_TABLE[ALLIN_RANGE_TABLE.len() - 1];
    let (_, open, reshove) = ALLIN_RANGE_TABLE
        .iter()
        .copied()
        .find(|&(max_bb, _, _)| shover_start <= max_bb * big_blind)
        .unwrap_or(last);
    let base = if open_shove { open } else { reshove };
    (base * allin_position_factor(pos)).min(1.0)
}

/// 全部 1326 个起手组合按 [`chen_order_key`] 从强到弱排好（进程内算一次）。
fn combos_by_strength() -> &'static [[Card; 2]] {
    static SORTED: std::sync::OnceLock<Vec<[Card; 2]>> = std::sync::OnceLock::new();
    SORTED.get_or_init(|| {
        let mut v: Vec<[Card; 2]> = Vec::with_capacity(1326);
        for a in 0u8..52 {
            for b in (a + 1)..52 {
                v.push([
                    Card::from_u8(a).expect("0..52 是合法 Card"),
                    Card::from_u8(b).expect("0..52 是合法 Card"),
                ]);
            }
        }
        // 强在前；键全相等的组合保持生成序（稳定排序 → 确定性）。
        v.sort_by(|x, y| {
            chen_order_key(*y)
                .partial_cmp(&chen_order_key(*x))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    })
}

/// top `width` 的组合（至少 1 个）。不做 card removal——抽样时再排除死牌。
fn allin_range(width: f64) -> &'static [[Card; 2]] {
    let all = combos_by_strength();
    // 减 1e-9 再向上取整：`k / 1326 × 1326` 的浮点误差不该多取一个组合。
    let n = ((width * all.len() as f64 - 1e-9).ceil() as usize).clamp(1, all.len());
    &all[..n]
}

/// hero 对若干对手范围（各自在范围内均匀，排除死牌）全下到底的底池份额期望（平分算份额）。
/// 固定 `seed` → 确定性。某样本里某对手范围的组合全被死牌挡掉（抽 64 次不中）→ 丢弃该样本；
/// 全部丢弃 → `None`。
fn allin_equity(
    hole: [Card; 2],
    opp_ranges: &[&[[Card; 2]]],
    samples: u32,
    seed: u64,
) -> Option<f64> {
    use poker::RngSource;
    let ev = NaiveHandEvaluator;
    let mut rng = ChaCha20Rng::from_seed(seed);
    let (mut share, mut used) = (0.0f64, 0u32);
    'sample: for _ in 0..samples {
        let mut dead = [false; 52];
        dead[hole[0].to_u8() as usize] = true;
        dead[hole[1].to_u8() as usize] = true;
        let mut opps: Vec<[Card; 2]> = Vec::with_capacity(opp_ranges.len());
        for range in opp_ranges {
            let mut pick = None;
            for _ in 0..64 {
                let h = range[(rng.next_u64() % range.len() as u64) as usize];
                if !dead[h[0].to_u8() as usize] && !dead[h[1].to_u8() as usize] {
                    pick = Some(h);
                    break;
                }
            }
            let Some(h) = pick else { continue 'sample };
            dead[h[0].to_u8() as usize] = true;
            dead[h[1].to_u8() as usize] = true;
            opps.push(h);
        }
        let mut deck: Vec<Card> = (0u8..52)
            .filter(|&i| !dead[i as usize])
            .map(|i| Card::from_u8(i).expect("0..52 是合法 Card"))
            .collect();
        for i in 0..5 {
            let j = i + (rng.next_u64() % (deck.len() - i) as u64) as usize;
            deck.swap(i, j);
        }
        let board = [deck[0], deck[1], deck[2], deck[3], deck[4]];
        let rank = |h: [Card; 2]| {
            ev.eval7(&[h[0], h[1], board[0], board[1], board[2], board[3], board[4]])
        };
        let hero = rank(hole);
        let best_opp = opps.iter().map(|&h| rank(h)).max().expect("至少一个对手");
        if hero > best_opp {
            share += 1.0;
        } else if hero == best_opp {
            let ties = opps.iter().filter(|&&h| rank(h) == hero).count();
            share += 1.0 / (ties + 1) as f64;
        }
        used += 1;
    }
    (used > 0).then(|| share / f64::from(used))
}

/// 翻前「hero 面对全下」局面（[`preflop_allin_spot`] 从真栈重放取）。筹码单位 = solver chip。
#[derive(Clone, Debug, PartialEq)]
struct PreflopAllinSpot {
    /// 定价的全下者（本轮投入最高、已全下）的 tree seat。
    shover: u8,
    /// 全下者 hand-start 码深。
    shover_start: u64,
    /// hero 与全下者的有效码深（hand-start 取小）。
    eff: u64,
    /// 全下前没人加注过（开池全下）；否则按 3bet/4bet 全下。
    open_shove: bool,
    /// 已经跟了全下（或更早已全下）还在池里的其他对手 tree seat。
    callers: Vec<u8>,
    /// hero 跟注要补的额（封顶 hero 剩余栈）。
    to_call: u64,
    /// hero 跟注后能赢的底池（各家投入按 hero 跟注后的总投入封顶 + hero 自己）。
    win_pot: u64,
    big_blind: u64,
}

/// 翻前轮到 hero、价格由一个已全下的对手定（本轮投入最高者已 all-in）→ `Some`。其余（翻后、
/// 没人全下、全下的人只是跟注全下而价格由别人的加注定、真栈建不出）→ `None`。全下前是否有人
/// 加注只看 `req.actions`（全下者最后一个进攻动作之前有没有 raise / bet / all_in）。
/// 后面还没行动的玩家可能再跟或再推，这里不考虑（只用已发生的动作）。
fn preflop_allin_spot(
    req: &Request,
    solver_cfg: &TableConfig,
    scale: u64,
    smap: &SeatMap,
    hero: SeatId,
    hole: [Card; 2],
) -> Option<PreflopAllinSpot> {
    let (auth, ..) = build_real_auth(req, solver_cfg, scale, smap, hero, hole, &[], true).ok()?;
    if auth.street() != poker::Street::Preflop {
        return None;
    }
    let real_cfg = real_stacks_config(req, solver_cfg, scale, smap).ok()?;
    let players = auth.players();
    let h = hero.0 as usize;
    let max_c = players
        .iter()
        .map(|p| p.committed_this_round)
        .max()
        .unwrap_or(ChipAmount::ZERO);
    let shover = players
        .iter()
        .enumerate()
        .filter(|&(i, p)| {
            i != h && p.status == poker::PlayerStatus::AllIn && p.committed_this_round == max_c
        })
        .max_by_key(|&(_, p)| p.committed_total)
        .map(|(i, _)| i)?;
    let call_to = auth.legal_actions().call?;
    let to_call = call_to
        .as_u64()
        .checked_sub(players[h].committed_this_round.as_u64())?;
    if to_call == 0 {
        return None;
    }
    let hero_total = players[h].committed_total.as_u64() + to_call;
    let win_pot = hero_total
        + players
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != h)
            .map(|(_, p)| p.committed_total.as_u64().min(hero_total))
            .sum::<u64>();
    let callers = players
        .iter()
        .enumerate()
        .filter(|&(i, p)| {
            i != h
                && i != shover
                && (p.status == poker::PlayerStatus::AllIn
                    || (p.status == poker::PlayerStatus::Active && p.committed_this_round == max_c))
        })
        .map(|(i, _)| i as u8)
        .collect();
    let is_aggr = |a: &str| matches!(a, "raise" | "bet" | "all_in" | "allin");
    let shove_idx = req
        .actions
        .iter()
        .rposition(|a| smap.tree_seat(a.seat) == Ok(shover as u8) && is_aggr(&a.action))?;
    let open_shove = !req.actions[..shove_idx].iter().any(|a| is_aggr(&a.action));
    let shover_start = real_cfg.starting_stacks[shover].as_u64();
    let eff = shover_start.min(real_cfg.starting_stacks[h].as_u64());
    Some(PreflopAllinSpot {
        shover: shover as u8,
        shover_start,
        eff,
        open_shove,
        callers,
        to_call,
        win_pot,
        big_blind: solver_cfg.big_blind.as_u64(),
    })
}

/// 有效码深是否低到不该用 blueprint（[`PREFLOP_ALLIN_BLUEPRINT_MIN_EFF_BB`]）。
fn preflop_allin_too_short(spot: &PreflopAllinSpot) -> bool {
    spot.eff < PREFLOP_ALLIN_BLUEPRINT_MIN_EFF_BB * spot.big_blind
}

/// 翻前面对全下的兜底：按全下者码深 / 位置 / 是否开池估范围（[`allin_range_width`]），已跟注的
/// 对手范围再收紧（[`ALLIN_CALLER_RANGE_FACTOR`]），Monte Carlo 算 hero 底池份额
/// （[`allin_equity`]），`份额 ≥ 跟注额 / 跟注后底池 + 余量` → call，否则 fold。只跟或弃，不再加注。
/// `source = fallback:preflop_allin:{call,fold}`（driver 按 `fallback` 前缀计兜底）；判断用的数字
/// 打一行 stderr（`[openpoker_advisor] preflop_allin ...`）供复盘。
fn preflop_allin_response(
    req: &Request,
    hole: [Card; 2],
    spot: &PreflopAllinSpot,
    n_seats: u8,
    seed: u64,
) -> Response {
    let width = allin_range_width(
        spot.shover_start,
        spot.big_blind,
        spot.open_shove,
        limp_pos(spot.shover, n_seats),
    );
    let caller_width = width * ALLIN_CALLER_RANGE_FACTOR;
    let mut ranges: Vec<&[[Card; 2]]> = vec![allin_range(width)];
    ranges.extend(spot.callers.iter().map(|_| allin_range(caller_width)));
    let equity = allin_equity(hole, &ranges, ALLIN_EQUITY_SAMPLES, seed).unwrap_or(0.0);
    let need = spot.to_call as f64 / spot.win_pot as f64;
    let margin = ALLIN_CALL_MARGIN + ALLIN_CALL_MARGIN_PER_CALLER * spot.callers.len() as f64;
    let valid = &req.valid;
    let (action, case) = if equity >= need + margin && valid.can_call {
        ("call", "call")
    } else if valid.can_check {
        ("check", "check") // 防御：面对全下不该能过牌。
    } else {
        ("fold", "fold")
    };
    let bb = spot.big_blind;
    eprintln!(
        "[openpoker_advisor] preflop_allin {} shover=seat{} {}bb eff={}bb width={width:.3} callers={} equity={equity:.3} need={need:.3} margin={margin:.2} → {action}",
        if spot.open_shove { "open" } else { "reshove" },
        spot.shover,
        spot.shover_start / bb,
        spot.eff / bb,
        spot.callers.len(),
    );
    Response {
        action: action.into(),
        source: format!("fallback:preflop_allin:{case}"),
        street: Some("preflop".into()),
        ..Default::default()
    }
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

/// `--search-flop-prefer-blueprint` 的真码深闸：flop 锚定面只有在真实码深与 blueprint 的
/// 100BB 假设相近时才可走 blueprint。`state` = 真栈重放态（决策路径传 `auth`、预热传
/// `round_start`），只读各座 status。
///
/// `Ok(())` = 可用 blueprint；`Err(reason)` = 须实时搜索：
/// - 本手到 `state` 为止有人 all-in（`all_in`）；
/// - hero 与某个未弃牌对手的 hand-start 有效码深 `min(hero, opp)` 落在
///   [[`FLOP_BLUEPRINT_EFF_STACK_BB_MIN`], [`FLOP_BLUEPRINT_EFF_STACK_BB_MAX`]] BB 外（`eff_stack`）；
/// - 真栈 config 建不出（`stacks_len` 等脏数据）。
///
/// `stacks` 缺省按 [`real_stacks_config`] 惯例 = 对称 100BB（与搜索路径同口径）→ 码深一项恒过，
/// all-in 一项照查。
fn flop_blueprint_stack_gate(
    req: &Request,
    solver_cfg: &TableConfig,
    scale: u64,
    smap: &SeatMap,
    state: &GameState,
    hero: SeatId,
) -> Result<(), String> {
    let players = state.players();
    if let Some(i) = players
        .iter()
        .position(|p| p.status == poker::PlayerStatus::AllIn)
    {
        return Err(format!("all_in:seat{i}"));
    }
    let real_cfg = real_stacks_config(req, solver_cfg, scale, smap)?;
    let bb = solver_cfg.big_blind.as_u64();
    let hero_start = real_cfg.starting_stacks[hero.0 as usize].as_u64();
    for (i, p) in players.iter().enumerate() {
        if i == hero.0 as usize || p.status == poker::PlayerStatus::Folded {
            continue;
        }
        let eff = hero_start.min(real_cfg.starting_stacks[i].as_u64());
        if eff < FLOP_BLUEPRINT_EFF_STACK_BB_MIN * bb || eff > FLOP_BLUEPRINT_EFF_STACK_BB_MAX * bb
        {
            return Err(format!("eff_stack:seat{i}:{}bb", eff / bb));
        }
    }
    Ok(())
}

/// `--search-flop-prefer-blueprint` 的尺寸闸：`street` 街上每次下注 / 加注（[`BetSizeMap`]，含 hero
/// 自己的）的「真实池比例 ÷ 映射池比例」都须落在
/// [[`FLOP_BLUEPRINT_SIZE_RATIO_MILLI_MIN`], [`FLOP_BLUEPRINT_SIZE_RATIO_MILLI_MAX`]]/1000 内，blueprint
/// 才算在应对真实发生的尺寸；本街没人下注恒过。`Err(size_map:<真实>/<映射>)` = 须实时搜索。
/// 决策路径与预热共用（[`decide`] / [`prewarm`]）。
fn flop_blueprint_size_gate(sizes: &[BetSizeMap], street: poker::Street) -> Result<(), String> {
    for m in sizes.iter().filter(|m| m.street == street) {
        let real = m.real_milli.saturating_mul(1000);
        let lo = m
            .mapped_milli
            .saturating_mul(FLOP_BLUEPRINT_SIZE_RATIO_MILLI_MIN);
        let hi = m
            .mapped_milli
            .saturating_mul(FLOP_BLUEPRINT_SIZE_RATIO_MILLI_MAX);
        if real < lo || real > hi {
            return Err(format!("size_map:{}/{}", m.real_milli, m.mapped_milli));
        }
    }
    Ok(())
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

/// 重放本手**翻前街**，返回每个 solver tree seat 的入池方式（[`PreflopEntry`]，下标 = tree seat，
/// 长度 = solver 座位数）。
/// 纯函数（只读 `req`+`smap`+`solver_cfg`）：真栈建不出 / 重放失同步 / 脏动作 / 溢出 → 全
/// `Unknown`（caller 退 `TopK`，保守安全）。只看翻前——街一进 flop 即停。与 [`build_real_auth`]
/// 同重放口径（真栈 config + 幻影 fold + [`hist_to_concrete`]），但不注入牌、不要求轮到 hero。
fn preflop_entry_kinds(
    req: &Request,
    solver_cfg: &TableConfig,
    scale: u64,
    smap: &SeatMap,
) -> Vec<PreflopEntry> {
    let unknown = vec![PreflopEntry::Unknown; solver_cfg.n_seats as usize];
    let Ok(real_cfg) = real_stacks_config(req, solver_cfg, scale, smap) else {
        return unknown;
    };
    let mut st = GameState::new(&real_cfg, REAL_REPLAY_SEED);
    let mut kinds = unknown.clone();
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
    limp: &LimpFoldMap,
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
    let n_seats = solver_cfg.n_seats as usize;
    let hand_seed = hand_seed_for(req, base_seed, n_seats);
    // 档一前缀 reach（生产默认开）：用已同步前缀估 range；off → None = uniform（byte-equal 旧行为）。
    let prefix_decisions = rt.unanchored_prefix_reach.then(|| {
        unanchored_prefix_decisions(game, synced_node, limp, my_seat_solver.0 as PlayerId)
    });
    dlog!(
        "unanchored prefix_decisions: count={} live_informed={:?} limp_folds={:?}",
        prefix_decisions.as_ref().map_or(0, |d| d.len()),
        // 前缀里有决策、且此刻仍未弃牌的座位（只有这些座位的范围不是均匀起点）。
        prefix_decisions.as_ref().map(|d| {
            let mut seats: Vec<PlayerId> = d
                .iter()
                .map(|&(_, _, s)| s)
                .filter(|&s| auth.players()[s as usize].status != poker::PlayerStatus::Folded)
                .collect();
            seats.sort_unstable();
            seats.dedup();
            seats
        }),
        limp.folds
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
            let mut profiles: Vec<Option<OpponentProfile>> = vec![None; n_seats];
            let mut shapes: Vec<ExploitShape> = vec![ExploitShape::TopK; n_seats];
            // PFR-aware（--exploit-pfr-shape）：重放翻前判各对手座本手入池方式（被动→CallBand 掐顶端 /
            // 主动→RaiseBand 收顶端），且仅当该座 PFR 也收敛才用。off → 全 Unknown → 全 TopK（仅 VPIP，
            // 与现有 exploit 逐位 byte-equal）；重放失败同样退 TopK（安全）。索引 = solver tree seat。
            let entry_kinds = if pfr_shape_on {
                preflop_entry_kinds(req, solver_cfg, scale, smap)
            } else {
                vec![PreflopEntry::Unknown; n_seats]
            };
            for op_seat in 0..smap.to_tree.len() {
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
    // crop blueprint：档一前缀含被裁街决策且跨街后验不可得 → 补解上一街后重试一次
    // （search_with_crop_backfill doc）；完整 blueprint 下恰调一次（行为不变）。
    let search_result = search_with_crop_backfill(
        game,
        strategy_fn,
        req,
        base_seed,
        rt,
        solve_cache,
        0,
        |cache| {
            let prefix_reach = prefix_decisions.as_ref().map(|d| PrefixReach {
                strategy: strategy_fn,
                decisions: d,
            });
            subgame_search_unanchored_cached_cross_exploit(
                Some(cache), // within-round solve 缓存（同锚定路径；kind 进 key、两路不串条目）。
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
            )
        },
    );
    let dist = match search_result {
        Ok(d) => d,
        Err(reason) => {
            // auth 已建（真栈河牌决策态）→ 喂底池赔率 floor（用户场景：河牌 giveup 被它接住）。
            return fallback_with_floor(
                &req.valid,
                hole,
                board,
                pot_odds_from_auth(&auth, solver_cfg),
                giveup_source("unanchored", &reason),
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
    let mut sample_rng = ChaCha20Rng::from_seed(sample_seed(req, base_seed, n_seats));
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
    let (mut resp, degrade) = action_to_response(solver_action, scale, &req.valid);
    if resp.source.is_empty() {
        resp.source = format!("search:unanchored{}", degrade.unwrap_or(""));
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
    if req.stacks.len() != smap.to_tree.len() {
        return Err("stacks_len".into()); // 下标 = OpenPoker 座位号，长度须 = 桌面座位数 N。
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
/// ordinal 下同街多决策共享字节相同的 solve（§6 #2 一致性）。`n_seats` = solver 座位数（只用来判
/// 「短桌」：dealt 非空且 ≠ 全座才掺 dealt）。
fn hand_seed_for(req: &Request, base_seed: u64, n_seats: usize) -> u64 {
    let mut hasher = blake3::Hasher::new();
    for c in &req.hole {
        hasher.update(c.as_bytes());
    }
    hasher.update(&[req.button_seat, req.my_seat, req.num_seats]);
    hasher.update(&req.small_blind.to_le_bytes());
    hasher.update(&req.big_blind.to_le_bytes());
    if !req.dealt_seats.is_empty() && req.dealt_seats.len() != n_seats {
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
/// `n_seats` 同 [`hand_seed_for`]（只判短桌）。
fn sample_seed(req: &Request, base_seed: u64, n_seats: usize) -> u64 {
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
    if !req.dealt_seats.is_empty() && req.dealt_seats.len() != n_seats {
        // 短桌：占座不同 = 不同局面（满桌不掺、保旧 seed byte-equal）。
        hasher.update(&req.dealt_seats);
    }
    hasher.update(&base_seed.to_le_bytes());
    let d = hasher.finalize();
    u64::from_le_bytes(d.as_bytes()[..8].try_into().expect("blake3 ≥ 8 bytes"))
}

// ===========================================================================
// blueprint 访问面（完整 dense checkpoint / 按街裁切的 crop 文件）
// ===========================================================================

/// advisor 唯一的 blueprint 访问面：`decide` / `prewarm` / 搜索 range 先验拿到的 `strategy_fn`
/// 全部经 [`Blueprint::strategy`]，game（树 / 抽象 / 桶表）经 [`Blueprint::game`]。
///
/// - [`Full`](Self::Full)（`--checkpoint`，dense ckpt `PLDNCKPT`）：`strategy` ≡ 原
///   `trainer.average_strategy(*info)`，与引入本枚举前**逐位相同**。
/// - [`Cropped`](Self::Cropped)（`--cropped-checkpoint`，`PLDNCROP`，[`CroppedBlueprint`]）：只含
///   `street <= keep_through` 的行。保留街上 `strategy` 与完整 blueprint **逐位一致**（未访问 =
///   空 Vec，同原 trainer 约定）。被裁掉的街**不应被查询**：转牌 / 河牌由实时搜索接管（启动期
///   [`validate_crop_runtime`] 强制），搜索 range 先验在 `blueprint_max_street` 守卫下不查被裁街
///   （[`poker::training::subgame::BLUEPRINT_CROPPED_ERR`]），blueprint 路径在 [`decide`] 里显式拦。
///   万一仍被查到（= 守卫有洞），`strategy` 计入 `cropped_queries` 并返回空 Vec；主循环见计数变化
///   即**作废该请求的输出**（换显式兜底 + 清 solve 缓存，[`run`]）——绝不把缺失 σ 当均匀悄悄用掉。
// 进程内只有一个实例、常驻整个生命周期 → 变体尺寸差无所谓。
#[allow(clippy::large_enum_variant)]
enum Blueprint {
    Full(Box<DenseNlheEsMccfrTrainer>),
    Cropped {
        bp: CroppedBlueprint,
        game: SimplifiedNlheGame,
        /// 查询落在被裁街的累计次数（安全网；正常恒 0）。
        cropped_queries: AtomicU64,
    },
}

impl Blueprint {
    fn game(&self) -> &SimplifiedNlheGame {
        match self {
            Blueprint::Full(t) => t.game(),
            Blueprint::Cropped { game, .. } => game,
        }
    }

    fn update_count(&self) -> u64 {
        match self {
            Blueprint::Full(t) => t.update_count(),
            Blueprint::Cropped { bp, .. } => bp.update_count(),
        }
    }

    /// σ 可查询到哪条街（含）：完整 = `River`；crop = 文件的 `keep_through`。搜索运行时据此填
    /// [`SubgameSearchConfig::blueprint_max_street`]。
    fn keep_through(&self) -> StreetTag {
        match self {
            Blueprint::Full(_) => StreetTag::River,
            Blueprint::Cropped { bp, .. } => bp.keep_through(),
        }
    }

    /// 安全网计数（完整 blueprint 恒 0）。
    fn cropped_queries(&self) -> u64 {
        match self {
            Blueprint::Full(_) => 0,
            Blueprint::Cropped {
                cropped_queries, ..
            } => cropped_queries.load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// `strategy_fn` 的实现（见类型 doc）。均匀占位行返回空（[`strip_uniform_placeholder`]）。
    fn strategy(&self, info: &InfoSetId) -> Vec<f64> {
        strip_uniform_placeholder(self.strategy_raw(info))
    }

    fn strategy_raw(&self, info: &InfoSetId) -> Vec<f64> {
        match self {
            Blueprint::Full(t) => t.average_strategy(*info),
            Blueprint::Cropped {
                bp,
                cropped_queries,
                ..
            } => bp.average_strategy(*info).unwrap_or_else(|| {
                // 只在首次打全量信息（range 估计一次会扫 1326 组合，逐条打会刷屏）；每个受影响请求
                // 另由主循环打一行汇总。
                let prev = cropped_queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if prev == 0 {
                    eprintln!(
                    "[openpoker_advisor] CROP VIOLATION: 查询了被裁掉街的 infoset {}（keep_through={:?}），\
                     本请求输出将作废",
                    info.raw(),
                    bp.keep_through()
                    );
                }
                Vec::new()
            }),
        }
    }
}

/// blueprint 平均策略恰好均匀（≥ 2 个动作、每项与 1/n 相差 < 1e-12）→ 空。dense trainer / crop 的
/// `average_strategy` 对「碰过但 strategy_sum 为 0」的行退成 1/n，「只被第一次遍历累计过」的行也恰好
/// 均匀——都不是解出来的混合策略（同 `subgame::read_current_strategy` 的占位判据）。训练过的行恰好
/// 均匀到 1e-12 的概率可以忽略。返回空后：决策路径（[`blueprint_distribution`]）不出牌、改走搜索或
/// 兜底；range 估计（`estimate_range`）对空 σ 本来就按 1/n 处理，与改前逐位相同。
fn strip_uniform_placeholder(v: Vec<f64>) -> Vec<f64> {
    let n = v.len();
    if n >= 2 && v.iter().all(|p| (p - 1.0 / n as f64).abs() < 1e-12) {
        Vec::new()
    } else {
        v
    }
}

/// 文件 magic 前 8 字节（错用 `--checkpoint` / `--cropped-checkpoint` 时给可读提示用）。
fn sniff_magic(path: &std::path::Path) -> Option<[u8; 8]> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut m = [0u8; 8];
    f.read_exact(&mut m).ok()?;
    Some(m)
}

/// 加载 blueprint：`cropped = false` → 完整 dense ckpt；`true` → crop 文件。两者都用同一
/// `game`（[`six_max_reshape`] 重建的树）做 layout 指纹校验，不符即拒绝。
fn load_blueprint(
    path: &std::path::Path,
    game: SimplifiedNlheGame,
    cropped: bool,
) -> Result<Blueprint, String> {
    let hint = match (cropped, sniff_magic(path)) {
        (false, Some(m)) if &m == b"PLDNCROP" => "（这是 crop 文件，请改用 --cropped-checkpoint）",
        (true, Some(m)) if &m == b"PLDNCKPT" => {
            "（这是完整 dense checkpoint，请改用 --checkpoint）"
        }
        _ => "",
    };
    if cropped {
        let bp = CroppedBlueprint::load(path, &game).map_err(|e| {
            format!(
                "load cropped blueprint {} failed{hint}: {e:?}",
                path.display()
            )
        })?;
        Ok(Blueprint::Cropped {
            bp,
            game,
            cropped_queries: AtomicU64::new(0),
        })
    } else {
        let t = DenseNlheEsMccfrTrainer::load_checkpoint(path, game)
            .map_err(|e| format!("load checkpoint {} failed{hint}: {e:?}", path.display()))?;
        Ok(Blueprint::Full(Box::new(t)))
    }
}

/// crop blueprint 的启动期约束（违反 = 直接报错退出，不静默降级）。`keep_through` = crop 保留到哪条
/// 街；`search` = 搜索运行时（crop 必须开 `--search`，parse 期已拦，这里再守一层）。
///
/// 决策点在街 S 的数据需求：S ≤ keep_through 才能走 blueprint σ；S > keep_through 必须实时搜索；
/// 搜索的 range 先验要查 S 之前各街 σ，S−1 > keep_through 时只能靠档二′跨街后验（上一街子树 σ）。
/// 由此：
/// - keep_through < River → `--search-trigger all-postflop`（FlopFirstUnraised 让 turn/river 走 blueprint）；
/// - keep_through < Turn → 档二′跨街复用必须开（river 先验不能读 turn σ）；
/// - keep_through < Flop → `--search-flop-prefer-blueprint` 不可开（它让 flop 锚定面走 blueprint）；
/// - `depth_limit`（叶子续局值表要全树 blueprint）/ `CurrentDecision`（range 含当前街 σ）不支持。
fn validate_crop_runtime(
    keep_through: StreetTag,
    search: Option<&SearchRuntime>,
) -> Result<(), String> {
    let Some(rt) = search else {
        return Err("crop blueprint 需要 --search（被裁掉的街只能实时搜索）".to_string());
    };
    let k = keep_through as u8;
    if k < StreetTag::River as u8 && rt.cfg.trigger != SearchTrigger::AllPostflop {
        return Err(format!(
            "crop blueprint 只保留到 {keep_through:?}：需要 --search-trigger all-postflop（被裁街的每个决策都必须实时搜索）"
        ));
    }
    if k < StreetTag::Turn as u8 && !rt.unanchored_cross_street {
        return Err(format!(
            "crop blueprint 只保留到 {keep_through:?}：不能 --search-unanchored-cross-street off（河牌 range 先验只能来自上一街子树后验）"
        ));
    }
    if k < StreetTag::Flop as u8 && rt.flop_prefer_blueprint {
        return Err(format!(
            "crop blueprint 只保留到 {keep_through:?}：不能 --search-flop-prefer-blueprint on（翻牌无 blueprint 数据）"
        ));
    }
    if rt.cfg.depth_limit || rt.cfg.resolve_root != ResolveRoot::RoundStart {
        return Err("crop blueprint 只支持 RoundStart + 解到终局的搜索".to_string());
    }
    if rt.cfg.blueprint_max_street != keep_through {
        return Err(format!(
            "内部错误：搜索 blueprint_max_street={:?} ≠ crop keep_through={keep_through:?}",
            rt.cfg.blueprint_max_street
        ));
    }
    Ok(())
}

// ===========================================================================
// blueprint 加载 + ready + stdio 主循环
// ===========================================================================

struct Args {
    /// blueprint 文件路径：`--checkpoint`（完整 dense ckpt）与 `--cropped-checkpoint`（按街裁切的
    /// crop 文件，[`Blueprint::Cropped`]）**二选一**。
    checkpoint: PathBuf,
    /// `true` = 路径来自 `--cropped-checkpoint`。显式分开两个 flag（而非按 magic 自动识别）：crop
    /// 模式要求「被裁街必须实时搜索」等一组启动约束（[`validate_crop_runtime`]），由操作者显式
    /// 选择更不易误用；错用时按 magic 给出改用另一 flag 的提示（[`load_blueprint`]）。
    cropped: bool,
    bucket_table: PathBuf,
    reshape: String,
    postflop_cap: u8,
    /// `--seats <3..=9>`：solver / blueprint 座位数（默认 [`DEFAULT_SEATS`] = 6）。须与 blueprint
    /// 训练时的 `train_cfr --seats` 一致（不符 → 树 layout 指纹不符 → 加载报错）；OpenPoker 桌面
    /// `num_seats` 须等于它（否则 `fallback:not_{N}max`）。
    seats: u8,
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
    /// `--search-limp-as-fold on|off`（方案 B，[`SearchRuntime`] doc）。**默认 `false`**
    /// （[`DEFAULT_SEARCH_LIMP_AS_FOLD`]）。仅 `--search` 开时可设。
    search_limp_as_fold: bool,
    /// `--exploit`（叠加剥削 Tier 2）：`Some` = 开（进程内画像 → 翻前 range 宽度先验）。**仅 `--search`
    /// 开时可设**（拒静默 guard）。`None`（默认）= 关，全程与现网 byte-equal。子旗 `--exploit-min-hands`
    /// / `--exploit-strength` / `--exploit-converge-se` / `--exploit-converge-drift` 填 [`ExploitConfig`]。
    exploit: Option<ExploitConfig>,
    /// `--warm-profile-hh <path>`：暖启动剥削画像——启动期读这个 HH JSONL（driver `--hh-log` 同一文件），
    /// 把过往牌局喂进进程内 `Profiler`（与 live observe 同口径）。**仅 `--exploit on|vpip` 时可设**
    /// （拒静默 guard）。`None`（默认）= 空表起步（旧行为）。网络丢消息 / 字段缺失 / 序列不完整的牌局
    /// 在 [`hh_record_to_observe`] 重放处 `Err` → 计数跳过（不污染统计）。
    warm_profile_hh: Option<PathBuf>,
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
    /// crop blueprint 时 = 保留到哪条街（如 `"Flop"`）；完整 blueprint = `None`（serde skip →
    /// ready 行与旧版逐字节相同）。
    #[serde(skip_serializing_if = "Option::is_none")]
    blueprint_keep_through: Option<String>,
}

/// `--reshape` → 树定义：与 `train_cfr` / `nlhe_dense_crop` 共用 [`six_max_reshape`]，保证加载时
/// 重建的树与训练时同一定义（checkpoint / crop 的 layout 指纹再兜底校验）。
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
    six_max_reshape(reshape, cap)
        .ok_or_else(|| format!("unknown reshape {reshape} (expected {SIX_MAX_RESHAPE_NAMES})"))
}

/// 按 `(--reshape, --postflop-cap, --seats)` 重建 blueprint game（树 + 抽象 + 桶表）：桌面 =
/// `TableConfig::n_max_at_bb(seats, 100)`，与 `train_cfr --profile six-max --seats <N>
/// --stack-bb 100` 同一定义（6 座 == 旧 `default_6max_100bb()`）。
fn build_game(
    table: Arc<BucketTable>,
    reshape: &str,
    cap: u8,
    seats: u8,
) -> Result<SimplifiedNlheGame, String> {
    let (abs, rules) = reshape_profile(reshape, cap)?;
    SimplifiedNlheGame::new_with_abstraction(
        table,
        TableConfig::n_max_at_bb(seats, SOLVER_STACK_BB),
        abs,
        rules,
    )
    .map_err(|e| format!("build {seats}-max game failed: {e:?}"))
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

/// 暖启动剥削画像（`--warm-profile-hh`）：逐行读 HH JSONL，把过往牌局喂进 `prof`（与 driver live
/// observe 同口径）。返回 `(fed, skipped)`：`fed` = 成功喂入的手数、`skipped` = 因数据有问题被
/// 过滤的手数（serde 解析失败 / 网络丢消息致重放 actor 失配 / 字段缺失 / 序列未到终局——全在
/// [`hh_record_to_observe`] 处 `Err`）。文件打不开 → `Err`（caller 降级空表起步，不致命）。
fn warm_profile_from_hh(path: &std::path::Path, prof: &mut Profiler) -> Result<(u64, u64), String> {
    let f = std::fs::File::open(path).map_err(|e| format!("open {} 失败: {e}", path.display()))?;
    let reader = std::io::BufReader::new(f);
    let (mut fed, mut skipped) = (0u64, 0u64);
    for line in reader.lines() {
        let line = line.map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        // 解析 → 重放转换 → 喂。任一步失败 = 该手有问题（含网络丢数据）→ 计数跳过，不污染统计。
        match serde_json::from_str::<HhRecord>(&line)
            .map_err(|e| e.to_string())
            .and_then(|rec| hh_record_to_observe(&rec))
        {
            Ok(obs) => {
                prof.observe_hand(&obs);
                fed += 1;
            }
            Err(_) => skipped += 1,
        }
    }
    Ok((fed, skipped))
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
    // solver 桌面 = `--seats` 座 100BB（`n_max_at_bb(6, 100)` == 旧 `default_6max_100bb()`，逐字段
    // 相同）。blueprint 是用某个座位数训练的：座位数不同 → 树不同 → 下面 load 的 layout 指纹校验
    // 拒绝（`--seats` 与 blueprint 不符即报错退出，不会静默按错的座位数重放）。
    let game = build_game(
        Arc::clone(&table),
        &args.reshape,
        args.postflop_cap,
        args.seats,
    )?;
    // blueprint 访问面（Blueprint doc）：完整 ckpt 或 crop 文件，二者都用同一 game 校验 layout 指纹。
    let blueprint = load_blueprint(&args.checkpoint, game, args.cropped).map_err(|e| {
        format!(
            "{e}\n（若 blueprint 训练时的座位数与 --seats {} 不同、或 --reshape / --postflop-cap / \
             --bucket-table 不同，树的 layout 指纹不符，必然加载失败）",
            args.seats
        )
    })?;
    let game = blueprint.game();
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
                // blueprint σ 覆盖街：完整 = River（默认值，行为不变）；crop = keep_through
                // （range 先验不得查被裁街，SubgameSearchConfig::blueprint_max_street doc）。
                cfg: SubgameSearchConfig {
                    blueprint_max_street: blueprint.keep_through(),
                    ..cfg
                },
                bucket_table,
                unanchored_prefix_reach: args.search_unanchored_prefix_reach,
                unanchored_cross_street: args.search_unanchored_cross_street,
                flop_prefer_blueprint: args.search_flop_prefer_blueprint,
                limp_as_fold: args.search_limp_as_fold,
                // 叠加剥削：--exploit 开 → 进程内空表 Profiler（只用本进程数据，不依赖过往）。
                exploit: args.exploit.map(|cfg| RefCell::new(Profiler::new(cfg))),
            })
        }
        None => None,
    };

    // crop 启动期约束（validate_crop_runtime doc）：不满足直接报错退出，不静默降级。
    if let Blueprint::Cropped { bp, .. } = &blueprint {
        validate_crop_runtime(bp.keep_through(), search_rt.as_ref())?;
        eprintln!(
            "[openpoker_advisor] cropped blueprint: keep_through={:?} has_regret={}（被裁街全部实时搜索；\
             river 先验 = 上一街子树后验，缺失时按需补解上一街）",
            bp.keep_through(),
            bp.has_regret()
        );
    }
    let ready = ReadyLine {
        ready: true,
        update_count: blueprint.update_count(),
        reshape: args.reshape.clone(),
        n_seats: game.table_config().n_seats as usize,
        search: search_rt.is_some(),
        blueprint_keep_through: matches!(blueprint, Blueprint::Cropped { .. })
            .then(|| format!("{:?}", blueprint.keep_through())),
    };
    if let Some(rt) = &search_rt {
        let scfg = &rt.cfg;
        eprintln!(
            "[openpoker_advisor] search ON: trigger={:?} iters={} time_budget={:?} lcfr={} deep_menu={} live_traversers={} max_nodes={} range_mix={} solve_threads={} unanchored_prefix_reach={} unanchored_cross_street={} flop_prefer_blueprint={} limp_as_fold={} bucket_table={}",
            scfg.trigger, scfg.iterations, scfg.time_budget, scfg.lcfr, scfg.deep_menu, scfg.live_traversers, scfg.max_subtree_nodes, scfg.range_uniform_mix, scfg.solve_threads, rt.unanchored_prefix_reach, rt.unanchored_cross_street, rt.flop_prefer_blueprint, rt.limp_as_fold,
            args.search_bucket_table.as_ref().map_or_else(|| "blueprint".to_string(), |p| p.display().to_string())
        );
        if let Some(cell) = &rt.exploit {
            let ec = *cell.borrow().cfg();
            eprintln!(
                "[openpoker_advisor] exploit ON (叠加剥削 Tier 2，翻前 range 宽度): min_hands={} strength_alpha={} converge_se={} converge_drift={} window={} mode={}（进程内画像、不依赖过往）",
                ec.min_hands, ec.strength_alpha, ec.converge_se, ec.converge_drift, ec.window,
                if ec.pfr_shape { "on(VPIP+PFR/CallBand·RaiseBand)" } else { "vpip(仅VPIP/byte-equal)" }
            );
            // 暖启动（--warm-profile-hh）：启动期读过往 HH JSONL，把历史牌局喂进 Profiler（与 live
            // observe 同口径）。网络丢消息 / 字段缺失 / 序列不完整的牌局在 hh_record_to_observe 重放处
            // Err → 计数跳过（不污染统计）。在写 ready 行**前**做 → 首决策即带画像。
            if let Some(path) = &args.warm_profile_hh {
                let mut prof = cell.borrow_mut();
                match warm_profile_from_hh(path, &mut prof) {
                    Ok((fed, skipped)) => eprintln!(
                        "[openpoker_advisor] warm-profile: {fed} 手喂入画像 / {skipped} 手跳过（数据有问题，已过滤）/ {names} 名玩家 from {path}",
                        path = path.display(),
                        names = prof.names_seen(),
                    ),
                    Err(e) => eprintln!(
                        "[openpoker_advisor] warm-profile 读取失败（跳过暖启动、空表起步）: {e}"
                    ),
                }
            }
        }
    }
    eprintln!(
        "[openpoker_advisor] ready reshape={} seats={} update_count={} search={}",
        ready.reshape, ready.n_seats, ready.update_count, ready.search
    );
    let mut stdout = std::io::stdout();
    writeln!(
        stdout,
        "{}",
        serde_json::to_string(&ready).map_err(|e| e.to_string())?
    )
    .map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())?;

    let strategy_fn = |info: &InfoSetId, _n: usize| -> Vec<f64> { blueprint.strategy(info) };

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
        let violations_before = blueprint.cropped_queries();
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
        // crop 安全网（Blueprint doc）：本请求查到了被裁掉的街 = 守卫有洞，输出建立在缺失的 σ 上 →
        // 作废：决策换显式兜底、预热标 err；清 solve 缓存（被污染的 range 可能已入缓存，跨街复用会
        // 继续传染）。完整 blueprint 计数恒 0、此分支不可达（byte-equal）。
        let resp = if blueprint.cropped_queries() != violations_before {
            eprintln!(
                "[openpoker_advisor] CROP VIOLATION: 本请求查询被裁街 {} 次 → 输出作废（原 source={}），清 solve 缓存",
                blueprint.cropped_queries() - violations_before,
                resp.source
            );
            solve_cache = SubgameSolveCache::new();
            match serde_json::from_str::<Request>(&line) {
                Ok(_) if is_prewarm => Response {
                    action: "none".into(),
                    source: "prewarm:err:crop_violation".into(),
                    ..Default::default()
                },
                Ok(req) => safe_fallback(&req.valid, "crop_violation"),
                Err(_) => resp,
            }
        } else {
            resp
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
    let mut cropped_checkpoint: Option<PathBuf> = None;
    let mut bucket_table: Option<PathBuf> = None;
    let mut reshape = "preopen".to_string();
    let mut postflop_cap = 3u8;
    let mut seats = DEFAULT_SEATS;
    let mut seed: u64 = 0;
    // 缺口② 实时搜索 flag（仅 --search 开时打包成 SubgameSearchConfig）。
    let mut search_on = false;
    let mut search_iters: Option<u64> = None;
    let mut search_trigger = SearchTrigger::FlopFirstUnraised;
    let mut search_time_budget_ms: Option<u64> = None;
    let mut search_lcfr = false;
    let mut search_deep_menu = false;
    let mut search_live_traversers: Option<bool> = None;
    let mut search_max_nodes: Option<usize> = None;
    let mut search_range_uniform_mix: Option<f64> = None;
    let mut search_bucket_table: Option<PathBuf> = None;
    let mut search_solve_threads: Option<usize> = None;
    let mut search_unanchored_prefix_reach: Option<bool> = None;
    let mut search_unanchored_cross_street: Option<bool> = None;
    let mut search_flop_prefer_blueprint: Option<bool> = None;
    let mut search_limp_as_fold: Option<bool> = None;
    // 叠加剥削（--exploit*，仅 --search 开时生效）。
    let mut exploit_on = false;
    let mut exploit_min_hands: Option<u32> = None;
    let mut exploit_strength: Option<f64> = None;
    let mut exploit_converge_se: Option<f64> = None;
    let mut exploit_converge_drift: Option<f64> = None;
    let mut exploit_pfr_shape = false;
    let mut warm_profile_hh: Option<PathBuf> = None;
    let mut debug_log = false;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--checkpoint" => checkpoint = Some(PathBuf::from(next_val(&mut it, &arg)?)),
            "--cropped-checkpoint" => {
                cropped_checkpoint = Some(PathBuf::from(next_val(&mut it, &arg)?))
            }
            "--bucket-table" => bucket_table = Some(PathBuf::from(next_val(&mut it, &arg)?)),
            "--reshape" => reshape = next_val(&mut it, &arg)?,
            "--postflop-cap" => {
                postflop_cap = next_val(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad cap: {e}"))?
            }
            "--seats" => {
                let v: u8 = next_val(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad seats: {e}"))?;
                // 与 train_cfr --profile six-max --seats 同范围（2 座是 HU 语义 button=SB，
                // 幻影座映射 / 位置语义不适用）。
                if !(3..=9).contains(&v) {
                    return Err(format!("--seats 须在 [3,9]，得 {v}"));
                }
                seats = v;
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
            "--search-live-traversers" => {
                // 默认开（DEFAULT_SEARCH_LIVE_TRAVERSERS）；取值 on|off 显式覆盖（A/B 用）。
                let v = next_val(&mut it, &arg)?;
                search_live_traversers = Some(match v.as_str() {
                    "on" | "true" | "1" => true,
                    "off" | "false" | "0" => false,
                    other => return Err(format!("--search-live-traversers 须 on|off，得 {other}")),
                });
            }
            "--search-max-nodes" => {
                search_max_nodes = Some(
                    next_val(&mut it, &arg)?
                        .parse()
                        .map_err(|e| format!("bad search-max-nodes: {e}"))?,
                )
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
            "--search-limp-as-fold" => {
                // 默认关（DEFAULT_SEARCH_LIMP_AS_FOLD）；取值 on|off 显式覆盖。
                let v = next_val(&mut it, &arg)?;
                search_limp_as_fold = Some(match v.as_str() {
                    "on" | "true" | "1" => true,
                    "off" | "false" | "0" => false,
                    other => return Err(format!("--search-limp-as-fold 须 on|off，得 {other}")),
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
            "--warm-profile-hh" => warm_profile_hh = Some(PathBuf::from(next_val(&mut it, &arg)?)),
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
            max_subtree_nodes: search_max_nodes.unwrap_or(DEFAULT_SEARCH_MAX_NODES),
            trigger: search_trigger,
            lcfr: search_lcfr,
            time_budget: search_time_budget_ms.map(Duration::from_millis),
            // 缺口③：深码窄菜单——子树下注菜单收到单一 {1pot}（深码 / 多人解到终局控树，§2.1）。
            deep_menu: search_deep_menu,
            // 缺口①续（限时杠杆②）：traverser 只轮子树根仍 Active 的座（弃牌/all-in 座零学习
            // 迭代跳过，同 wall 有效迭代 ×n_seats/n_active）。
            live_traversers: search_live_traversers.unwrap_or(DEFAULT_SEARCH_LIVE_TRAVERSERS),
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
            || search_live_traversers.is_some()
            || search_max_nodes.is_some()
            || search_range_uniform_mix.is_some()
            || search_bucket_table.is_some()
            || search_solve_threads.is_some()
            || search_unanchored_prefix_reach.is_some()
            || search_unanchored_cross_street.is_some()
            || search_flop_prefer_blueprint.is_some()
            || search_limp_as_fold.is_some()
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
    if warm_profile_hh.is_some() && !exploit_on {
        return Err("设了 --warm-profile-hh 但 --exploit 非 on|vpip（无画像可暖启动）".to_string());
    }
    if exploit_on && search.is_none() {
        return Err("--exploit on|vpip 需配合 --search（剥削只挂脱锚搜索路径）".to_string());
    }
    // blueprint 来源二选一；crop 必须配 --search（被裁掉的街只能实时搜索；街级约束要读 crop 的
    // keep_through，加载后由 validate_crop_runtime 再查）。
    let (checkpoint, cropped) = match (checkpoint, cropped_checkpoint) {
        (Some(p), None) => (p, false),
        (None, Some(p)) => {
            if search.is_none() {
                return Err(
                    "--cropped-checkpoint 需要配合 --search（转牌 / 河牌没有 blueprint 数据，必须实时搜索；拒绝静默降级）"
                        .to_string(),
                );
            }
            (p, true)
        }
        (Some(_), Some(_)) => {
            return Err("--checkpoint 与 --cropped-checkpoint 只能二选一".to_string())
        }
        (None, None) => return Err("缺 --checkpoint（或 --cropped-checkpoint）".to_string()),
    };
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
        checkpoint,
        cropped,
        bucket_table: bucket_table.ok_or("缺 --bucket-table")?,
        reshape,
        postflop_cap,
        seats,
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
        // 默认关（旧行为 byte-equal）；--search-limp-as-fold on 显式开。
        search_limp_as_fold: search_limp_as_fold.unwrap_or(DEFAULT_SEARCH_LIMP_AS_FOLD),
        warm_profile_hh,
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
    use poker::training::nlhe_betting_tree::{first_small_6max, first_small_preopen_6max};
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
            limp_as_fold: false,
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
            limp_as_fold: false,
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
            limp_as_fold: false,
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
            limp_as_fold: false,
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

    /// 策略选 Bet/Raise 但桌面不给加注区间（对手已 all-in 等）：能跟 → call、能过 → check（带降级
    /// 标记、source 留给 caller 填正常来源）；都不能才 safe_fallback。修前一律 check/fold，
    /// 想加注的强牌面对下注被扔掉。脏区间（min > max）同样按此退。有区间时行为不变。
    #[test]
    fn raise_without_range_degrades_to_call_or_check() {
        let va = |can_check, can_call, can_raise, min, max| ValidActions {
            can_check,
            can_call,
            can_raise,
            min_raise: min,
            max_raise: max,
        };
        let raise = Action::Raise {
            to: ChipAmount::new(1650),
        };
        let bet = Action::Bet {
            to: ChipAmount::new(1650),
        };

        // 面对下注、不能加注 → call。
        let (r, d) = action_to_response(raise, 5, &va(false, true, false, None, None));
        assert_eq!(
            (r.action.as_str(), r.amount, r.source.as_str()),
            ("call", None, "")
        );
        assert_eq!(d, Some(":raise_as_call"));
        // 脏区间（min > max）也退 call，不再 fold。
        let (r, d) = action_to_response(raise, 5, &va(false, true, true, Some(400), Some(300)));
        assert_eq!((r.action.as_str(), d), ("call", Some(":raise_as_call")));
        // 没人下注、不能下注 → check。
        let (r, d) = action_to_response(bet, 5, &va(true, false, false, None, None));
        assert_eq!((r.action.as_str(), r.source.as_str()), ("check", ""));
        assert_eq!(d, Some(":raise_as_check"));
        // call / check 都不可用 → 旧兜底（fold），source 已填、无降级标记。
        let (r, d) = action_to_response(raise, 5, &va(false, false, false, None, None));
        assert_eq!(
            (r.action.as_str(), r.source.as_str()),
            ("fold", "fallback:raise_no_range")
        );
        assert_eq!(d, None);
        // 有区间 → 正常 raise，无降级标记（行为不变）。
        let (r, d) = action_to_response(raise, 5, &va(false, true, true, Some(20), Some(1840)));
        assert_eq!((r.action.as_str(), r.amount, d), ("raise", Some(330), None));
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
            let smap = seat_map(&req, 6).expect("seat_map");
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
                    Some(my),
                    false,
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
    /// preflop 走 [`limp_heuristic`]。**没人加注**分支三格（HJ / BB）：隔离档 iso-raise
    /// to (3+limper 数)×BB、范围外 BB 免费 check、范围外非盲位 fold。
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
        // 隔离档（AKo）→ iso-raise to (3+1)×20=80（HJ 非盲位，不加 OOP 1BB）。
        let req = make(["Ah", "Kd"], full_valid());
        let resp = decide_one(&req);
        assert_eq!(resp.source, "limp_heuristic:raise", "得 {resp:?}");
        assert_eq!(resp.amount, Some(80), "iso 尺寸 (3+1)×BB，得 {resp:?}");
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
            limp_heuristic(
                &nl,
                [parse_card("Ah").unwrap(), parse_card("Kd").unwrap()],
                LimpPos::BeforeBtn(2),
                None
            )
            .is_none(),
            "无 limp 历史须返回 None → 维持原兜底"
        );
    }

    /// 平跟池启发式（方案 A）各位置的范围 / 尺寸 / 被动入池，直接钉 [`limp_heuristic`]（纯函数）。
    /// 9-max 抽查 §3 的三手（BTN K9o / Q8o / 96s 面对一家平跟）在这里有明确去向；SB 不再任意两张都补齐。
    #[test]
    fn limp_heuristic_position_ranges() {
        let card = |s: &str| parse_card(s).unwrap();
        let h = |a: &str, b: &str| [card(a), card(b)];
        // n 家平跟（UTG 起）、没人加注。
        let req = |limps: u8, can_check: bool| Request {
            hole: vec![],
            board: vec![],
            button_seat: 0,
            my_seat: 0,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions: (0..limps).map(|k| hist(3 + k, "call", Some(20))).collect(),
            valid: ValidActions {
                can_check,
                ..full_valid()
            },
            stacks: vec![],
            dealt_seats: vec![],
            names: Default::default(),
        };
        // 底池赔率 floor 必中的输入：没人加注时也不许把弃牌改成跟注。
        let odds = Some(PotOdds {
            pot: 50,
            to_call: 10,
            big_blind: 20,
        });
        let run = |limps: u8, can_check: bool, pos: LimpPos, hole: [Card; 2]| {
            let r = limp_heuristic(&req(limps, can_check), hole, pos, odds).expect("有平跟");
            (r.source, r.amount)
        };
        let src = |s: &str| format!("limp_heuristic:{s}");
        // BTN：AKo 隔离加注 (3+1)×BB；96s 跟着平跟；K9o / Q8o 弃。
        assert_eq!(
            run(1, false, LimpPos::Btn, h("Ah", "Kd")),
            (src("raise"), Some(80))
        );
        assert_eq!(
            run(1, false, LimpPos::Btn, h("9s", "6s")).0,
            src("overlimp")
        );
        assert_eq!(run(1, false, LimpPos::Btn, h("Kh", "9d")).0, src("fold"));
        assert_eq!(run(1, false, LimpPos::Btn, h("Qh", "8d")).0, src("fold"));
        // HJ 不跟着平跟：96s 弃。
        assert_eq!(
            run(1, false, LimpPos::BeforeBtn(2), h("9s", "6s")).0,
            src("fold")
        );
        // CO：KTo 一家平跟时隔离；三家平跟时隔离范围收紧 → 改跟着平跟。
        assert_eq!(
            run(1, false, LimpPos::BeforeBtn(1), h("Kh", "Td")),
            (src("raise"), Some(80))
        );
        assert_eq!(
            run(3, false, LimpPos::BeforeBtn(1), h("Kh", "Td")).0,
            src("overlimp")
        );
        // SB：72o 弃（修前底池赔率 floor 让它必补齐）；87s 补齐；AKo 隔离 (3+1+1)×BB（OOP）。
        assert_eq!(run(1, false, LimpPos::Sb, h("7h", "2d")).0, src("fold"));
        assert_eq!(run(1, false, LimpPos::Sb, h("8s", "7s")).0, src("complete"));
        assert_eq!(
            run(1, false, LimpPos::Sb, h("Ah", "Kd")),
            (src("raise"), Some(100))
        );
        // BB：72o 免费过牌；AKo 隔离 (3+2+1)×BB（UTG 平跟 + SB 补齐 = 2 家）。
        assert_eq!(run(2, true, LimpPos::Bb, h("7h", "2d")).0, src("check"));
        assert_eq!(
            run(2, true, LimpPos::Bb, h("Ah", "Kd")),
            (src("raise"), Some(120))
        );
        // 有人加注：非 P 档弃牌，底池赔率 floor 命中才跟；P 档跟。
        let mut raised = req(1, false);
        raised.actions.push(hist(4, "raise", Some(40)));
        let r = limp_heuristic(&raised, h("7h", "2d"), LimpPos::Btn, odds).unwrap();
        assert_eq!(r.source, src("pot_odds_call"));
        let r = limp_heuristic(&raised, h("7h", "2d"), LimpPos::Btn, None).unwrap();
        assert_eq!(r.source, src("fold"));
        let r = limp_heuristic(&raised, h("Ah", "Ad"), LimpPos::Btn, None).unwrap();
        assert_eq!(r.source, src("call"));
        // 位置映射：6 座 tree seat 0..6 = BTN SB BB UTG HJ CO；9 座 tree 8 = CO、3 = UTG。
        assert_eq!(limp_pos(0, 6), LimpPos::Btn);
        assert_eq!(limp_pos(1, 6), LimpPos::Sb);
        assert_eq!(limp_pos(2, 6), LimpPos::Bb);
        assert_eq!(limp_pos(5, 6), LimpPos::BeforeBtn(1));
        assert_eq!(limp_pos(3, 6), LimpPos::BeforeBtn(3));
        assert_eq!(limp_pos(8, 9), LimpPos::BeforeBtn(1));
        assert_eq!(limp_pos(3, 9), LimpPos::BeforeBtn(6));
    }

    /// Chen 序拆并列后，名义宽度与实际组合数相差不超过一个牌型（≤ 12 组合）；顺序单调。
    #[test]
    fn chen_top_fraction_width_matches_nominal() {
        let combos = all_combos_for_test();
        for p in [0.11, 0.16, 0.22, 0.30, 0.45] {
            let n = combos.iter().filter(|&&c| chen_top_fraction(c) < p).count();
            let nominal = p * 1326.0;
            assert!(
                (n as f64) >= nominal - 0.5 && (n as f64) <= nominal + 12.0,
                "top {p}: 实际 {n} 组合，名义 {nominal}"
            );
        }
        let f =
            |a: &str, b: &str| chen_top_fraction([parse_card(a).unwrap(), parse_card(b).unwrap()]);
        assert_eq!(f("As", "Ad"), 0.0);
        assert!(f("As", "Ad") < f("Ks", "Kd"));
        assert!(f("Ah", "Kh") < f("Ah", "Kd"), "同花排在非同花前");
        assert!(f("Ah", "Kd") < f("7h", "2d"));
    }

    fn all_combos_for_test() -> Vec<[Card; 2]> {
        let mut v = Vec::new();
        for a in 0u8..52 {
            for b in (a + 1)..52 {
                v.push([Card::from_u8(a).unwrap(), Card::from_u8(b).unwrap()]);
            }
        }
        v
    }

    /// 同 [`rt_prefix_reach`] 但加开方案 B（`--search-limp-as-fold`）。
    fn rt_limp_as_fold(cfg: SubgameSearchConfig) -> SearchRuntime {
        SearchRuntime {
            limp_as_fold: true,
            ..rt_prefix_reach(cfg)
        }
    }

    /// 6 座平跟池请求：button=0（tree seat = OpenPoker 座位号），对称 100BB。
    fn limp_req(my_seat: u8, hole: [&str; 2], board: &[&str], actions: Vec<HistAction>) -> Request {
        Request {
            hole: vec![hole[0].into(), hole[1].into()],
            board: board.iter().map(|s| s.to_string()).collect(),
            button_seat: 0,
            my_seat,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions,
            valid: if board.is_empty() {
                full_valid()
            } else {
                ValidActions {
                    can_check: true,
                    can_call: false,
                    can_raise: true,
                    min_raise: Some(20),
                    max_raise: Some(1900),
                }
            },
            stacks: vec![2000; 6],
            dealt_seats: vec![],
            names: Default::default(),
        }
    }

    fn replay_limp(
        game: &SimplifiedNlheGame,
        req: &Request,
        my_turn: Option<SeatId>,
        limp_as_fold: bool,
    ) -> Result<LockstepOk, LockstepErr> {
        let smap = seat_map(req, 6).expect("seat_map");
        lockstep_replay(
            game,
            game.table_config(),
            &smap,
            req,
            5,
            req.board.len(),
            my_turn,
            limp_as_fold,
        )
    }

    /// 方案 B 翻前：UTG 平跟后影子在 BTN 开池节点对上 blueprint，但翻前决策仍交给启发式——
    /// 开关 B 的翻前输出逐字节相同（B 只给翻后搜索提供范围前缀）。
    #[test]
    fn limp_as_fold_preflop_still_uses_heuristic() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let scfg = SubgameSearchConfig {
            iterations: 100,
            ..SubgameSearchConfig::default()
        };
        let acts = vec![
            hist(3, "call", Some(20)),
            hist(4, "fold", None),
            hist(5, "fold", None),
        ];
        // 关：平跟即 structural_gap。开：映射成功、影子轮到 BTN。
        let req = limp_req(0, ["9s", "6s"], &[], acts.clone());
        let off = replay_limp(&game, &req, Some(SeatId(0)), false)
            .err()
            .unwrap();
        assert_eq!(off.reason, "structural_gap");
        assert!(!off.limp.mapped());
        let (_, ab, limp, _) =
            replay_limp(&game, &req, Some(SeatId(0)), true).expect("映射后应 Ok");
        assert_eq!(limp.folds, vec![(game.tree().root_id(), 3)]);
        assert_eq!(ab.game_state.current_player(), Some(SeatId(0)));
        for (hole, want) in [
            (["9s", "6s"], "limp_heuristic:overlimp"),
            (["Ah", "Kd"], "limp_heuristic:raise"),
            (["Qh", "8d"], "limp_heuristic:fold"),
        ] {
            let req = limp_req(0, hole, &[], acts.clone());
            let run = |rt: &SearchRuntime| {
                decide(
                    &game,
                    &abs,
                    &uniform,
                    &req,
                    1,
                    Some(rt),
                    &mut SubgameSolveCache::new(),
                )
            };
            let on = run(&rt_limp_as_fold(scfg));
            assert_eq!(
                on,
                run(&rt_prefix_reach(scfg)),
                "{hole:?}：翻前开关 B 须相同"
            );
            assert_eq!(on.source, want, "{hole:?}：{on:?}");
        }
        // BB 面对平跟、其余全弃：影子已结束（BB 白赢）但真实局轮到 BB → 失同步，仍走启发式。
        let bb_acts = vec![
            hist(3, "call", Some(20)),
            hist(4, "fold", None),
            hist(5, "fold", None),
            hist(0, "fold", None),
            hist(1, "fold", None),
        ];
        let mut bb = limp_req(2, ["7h", "2d"], &[], bb_acts);
        bb.valid.can_check = true;
        bb.valid.can_call = false;
        let e = replay_limp(&game, &bb, Some(SeatId(2)), true)
            .err()
            .unwrap();
        assert_eq!(e.reason, "limp_as_fold:lockstep_drift");
        let resp = decide(
            &game,
            &abs,
            &uniform,
            &bb,
            1,
            Some(&rt_limp_as_fold(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(resp.source, "limp_heuristic:check", "{resp:?}");
    }

    /// 方案 B 翻后：平跟者面对隔离加注又跟注 → 失同步 `limp_as_fold:limper_continued`，但前缀里有
    /// 平跟者那一步 Fold（= 开池范围之外的牌）和 BTN 的开池加注；关 B 时前缀为空（全员均匀）。
    #[test]
    fn limp_as_fold_limper_continued_keeps_prefix() {
        let game = nolimp_game();
        let acts = vec![
            hist(3, "call", Some(20)),
            hist(4, "fold", None),
            hist(5, "fold", None),
            hist(0, "raise", Some(80)),
            hist(1, "fold", None),
            hist(2, "fold", None),
            hist(3, "call", Some(80)),
            hist(3, "check", None),
        ];
        let req = limp_req(0, ["Ah", "Kd"], &["7h", "2c", "Ks"], acts);
        let off = replay_limp(&game, &req, Some(SeatId(0)), false)
            .err()
            .unwrap();
        assert_eq!(off.reason, "structural_gap");
        assert!(synced_prefix_decisions(&game, off.synced_node).is_empty());
        let on = replay_limp(&game, &req, Some(SeatId(0)), true)
            .err()
            .unwrap();
        assert_eq!(on.reason, "limp_as_fold:limper_continued");
        let d = unanchored_prefix_decisions(&game, on.synced_node, &on.limp, 0);
        let seat_tags: Vec<(PlayerId, AbstractActionTag)> =
            d.iter().map(|&(_, t, s)| (s, t)).collect();
        assert!(
            seat_tags.contains(&(3, AbstractActionTag::Fold)),
            "{seat_tags:?}"
        );
        assert!(
            seat_tags.iter().any(|&(s, t)| s == 0
                && t != AbstractActionTag::Fold
                && t != AbstractActionTag::Call),
            "BTN 开池加注须在前缀里：{seat_tags:?}"
        );
        // BB 弃牌后影子已结束（BTN 白赢），影子节点停在 BB 决策前 → 前缀 5 步（BB 已弃，不影响翻后）。
        assert_eq!(d.len(), 5, "UTG/HJ/CO/BTN/SB 五步：{seat_tags:?}");
    }

    /// 方案 B：平跟者面对加注弃牌 → 两边重新一致，翻后走锚定路径（关 B 时是脱锚）。
    #[test]
    fn limp_as_fold_limper_folds_back_to_anchored() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let acts = vec![
            hist(3, "call", Some(20)),
            hist(4, "fold", None),
            hist(5, "fold", None),
            hist(0, "raise", Some(80)),
            hist(1, "fold", None),
            hist(2, "call", Some(80)),
            hist(3, "fold", None),
        ];
        let req = limp_req(2, ["Qs", "Qd"], &["7h", "2c", "Ks"], acts);
        let (_, _, limp, _) = replay_limp(&game, &req, Some(SeatId(2)), true).expect("应回到锚定");
        assert!(limp.mapped());
        let scfg = SubgameSearchConfig {
            iterations: 300,
            trigger: SearchTrigger::FlopFirstUnraised,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let run = |rt: &SearchRuntime| {
            decide(
                &game,
                &abs,
                &uniform,
                &req,
                0xB0,
                Some(rt),
                &mut SubgameSolveCache::new(),
            )
        };
        let on = run(&rt_limp_as_fold(scfg));
        assert!(
            on.source == "search" || on.source.starts_with("search_giveup:unsolved"),
            "{on:?}"
        );
        assert!(is_legal(&on, &req.valid), "{on:?}");
        let off = run(&rt_prefix_reach(scfg));
        assert!(off.source.contains("unanchored"), "{off:?}");
    }

    /// 方案 B：平跟者还在手就进了翻牌（SB 补齐 + BB 过牌）→ `limp_as_fold:limper_in_pot`，前缀含
    /// 全部翻前决策（UTG 弃牌映射、SB 补齐、BB 过牌）；flop 脱锚搜索照常出合法动作。
    #[test]
    fn limp_as_fold_limper_in_pot_prefix_includes_blinds() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let acts = vec![
            hist(3, "call", Some(20)),
            hist(4, "fold", None),
            hist(5, "fold", None),
            hist(0, "fold", None),
            hist(1, "call", Some(20)),
            hist(2, "check", None),
        ];
        let req = limp_req(1, ["Ah", "Kd"], &["7h", "2c", "Ks"], acts);
        let e = replay_limp(&game, &req, Some(SeatId(1)), true)
            .err()
            .unwrap();
        assert_eq!(e.reason, "limp_as_fold:limper_in_pot");
        let d = unanchored_prefix_decisions(&game, e.synced_node, &e.limp, 1);
        let seat_tags: Vec<(PlayerId, AbstractActionTag)> =
            d.iter().map(|&(_, t, s)| (s, t)).collect();
        for want in [
            (3, AbstractActionTag::Fold),
            (1, AbstractActionTag::Call),
            (2, AbstractActionTag::Check),
        ] {
            assert!(seat_tags.contains(&want), "缺 {want:?}：{seat_tags:?}");
        }
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
            Some(&rt_limp_as_fold(scfg)),
            &mut SubgameSolveCache::new(),
        );
        assert!(is_legal(&resp, &req.valid), "{resp:?}");
        assert!(resp.source.contains("unanchored"), "{resp:?}");
    }

    /// 方案 B：hero 自己跟着平跟（启发式 overlimp）时，hero 那一步弃牌映射从前缀里拿掉（按 blueprint
    /// 弃牌概率算会把 hero 真实底牌压到接近 0）；对手平跟者那一步保留。预热与决策同一份前缀 → 命中缓存。
    #[test]
    fn limp_as_fold_hero_overlimp_dropped_and_prewarm_hits() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let skew = |i: &InfoSetId, n: usize| {
            let mut v: Vec<f64> = (0..n)
                .map(|k| 1.0 + k as f64 + (i.raw() % 7) as f64 * 0.1)
                .collect();
            let s: f64 = v.iter().sum();
            v.iter_mut().for_each(|x| *x /= s);
            v
        };
        let pre_acts = vec![
            hist(3, "call", Some(20)),
            hist(4, "fold", None),
            hist(5, "fold", None),
            hist(0, "call", Some(20)),
            hist(1, "fold", None),
            hist(2, "check", None),
        ];
        let pre = limp_req(0, ["9s", "6s"], &["7h", "2c", "Ks"], pre_acts.clone());
        let e = replay_limp(&game, &pre, None, true).err().unwrap();
        assert_eq!(
            e.reason, "limp_as_fold:lockstep_drift",
            "SB 弃牌后影子已结束"
        );
        let raw = synced_prefix_decisions(&game, e.synced_node);
        let kept = unanchored_prefix_decisions(&game, e.synced_node, &e.limp, 0);
        assert!(raw
            .iter()
            .any(|&(_, t, s)| s == 0 && t == AbstractActionTag::Fold));
        assert!(
            !kept.iter().any(|&(_, _, s)| s == 0),
            "hero 映射步须拿掉：{kept:?}"
        );
        assert!(kept
            .iter()
            .any(|&(_, t, s)| s == 3 && t == AbstractActionTag::Fold));
        assert_eq!(kept.len(), raw.len() - 1);

        let scfg = SubgameSearchConfig {
            iterations: 300,
            trigger: SearchTrigger::FlopFirstUnraised,
            max_subtree_nodes: 1_000_000,
            range_uniform_mix: 0.25,
            ..SubgameSearchConfig::default()
        };
        let rt = rt_limp_as_fold(scfg);
        let mut cache = SubgameSolveCache::new();
        let p = prewarm(&game, &skew, &pre, 0x0E1, Some(&rt), &mut cache);
        assert_eq!(p.source, "prewarm:stored", "{p:?}");
        let mut acts = pre_acts;
        acts.push(hist(2, "check", None));
        acts.push(hist(3, "check", None));
        let req = limp_req(0, ["9s", "6s"], &["7h", "2c", "Ks"], acts);
        let r = decide(&game, &abs, &skew, &req, 0x0E1, Some(&rt), &mut cache);
        assert!(r.source.contains("unanchored"), "{r:?}");
        assert_eq!((cache.misses(), cache.hits()), (1, 1), "决策须命中预热");
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
    ///
    /// 2026-10-16 起这条线改由翻前全下启发式接（[`preflop_allin_response`]），KK 仍须 call。
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
        // 2026-10-16 起翻前失同步面对全下改走全下启发式（审查 §5），不再是 P 档地板。
        assert_eq!(
            resp.source, "fallback:preflop_allin:call",
            "KK 超深码 shove 前 replay_illegal 失同步须走全下启发式，得 {resp:?}"
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

    /// 测试里现训的小 blueprint 翻前有不少恰好均匀的占位行：这些决策走占位兜底（不按均匀出牌），
    /// 其余走 blueprint。完整 vs crop 的全等断言不受影响。
    fn bp_or_placeholder(source: &str) -> bool {
        source == "blueprint" || source.starts_with("fallback:blueprint_placeholder")
    }

    /// blueprint 均匀占位（[`Blueprint::strategy`] 对恰好均匀的行返回空）不按均匀出牌：翻后开搜索
    /// → 改走锚定搜索（`search:bp_placeholder`）；翻前 / 未开搜索 → 兜底 `fallback:blueprint_placeholder`。
    #[test]
    fn blueprint_placeholder_never_plays_uniform() {
        assert!(strip_uniform_placeholder(vec![0.5, 0.5]).is_empty());
        assert!(strip_uniform_placeholder(vec![0.25; 4]).is_empty());
        assert_eq!(
            strip_uniform_placeholder(vec![1.0]),
            vec![1.0],
            "单动作不是占位"
        );
        assert_eq!(strip_uniform_placeholder(vec![0.3, 0.7]), vec![0.3, 0.7]);

        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let empty = |_i: &InfoSetId, _n: usize| Vec::<f64>::new();
        let scfg = SubgameSearchConfig {
            iterations: 300,
            trigger: SearchTrigger::FlopFirstUnraised,
            ..SubgameSearchConfig::default()
        };
        let run = |req: &Request, rt: Option<&SearchRuntime>| {
            decide(
                &game,
                &abs,
                &empty,
                req,
                0xB1DE,
                rt,
                &mut SubgameSolveCache::new(),
            )
        };
        // 翻牌锚定面 + flop_prefer_blueprint：本该走 blueprint，占位 → 锚定搜索。
        let flop = flop_first_unraised_req(vec![2000; 6]);
        let r = run(&flop, Some(&rt_flop_prefer_blueprint(scfg)));
        assert!(
            r.source == "search:bp_placeholder" || r.source.starts_with("search_giveup:"),
            "{r:?}"
        );
        assert!(is_legal(&r, &flop.valid), "{r:?}");
        // 未开搜索：兜底。
        let r = run(&flop, None);
        assert_eq!(r.source, "fallback:blueprint_placeholder", "{r:?}");
        // 翻前：兜底（AKo 面对开池加注 → premium floor 跟注，不随机）。
        let mut pre = flop.clone();
        pre.board.clear();
        pre.my_seat = 5;
        pre.actions = vec![hist(3, "raise", Some(60)), hist(4, "fold", None)];
        pre.valid = full_valid();
        let r = run(&pre, Some(&rt_flop_prefer_blueprint(scfg)));
        assert!(
            r.source.starts_with("fallback:blueprint_placeholder"),
            "{r:?}"
        );
        assert_eq!(r.action, "call", "AKo premium floor：{r:?}");
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

    /// 全下范围宽度：表 × 位置乘数；超过 60BB 按最后一行；上限 1.0。
    #[test]
    fn allin_range_width_table_and_position() {
        let bb = 100;
        let w = |stack_bb: u64, open: bool, pos| allin_range_width(stack_bb * bb, bb, open, pos);
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(close(w(25, false, LimpPos::Sb), 0.12 * 1.2));
        assert!(close(w(10, true, LimpPos::BeforeBtn(6)), 0.40 * 0.7));
        assert!(close(w(11, true, LimpPos::BeforeBtn(1)), 0.30));
        assert!(close(w(60, true, LimpPos::BeforeBtn(2)), 0.08 * 0.85));
        assert!(close(w(100, false, LimpPos::BeforeBtn(1)), 0.05));
        assert!(close(w(5, true, LimpPos::Btn), 0.48));
    }

    /// 范围按强度序取 top：最强 6 个组合 = AA；全宽 = 1326。胜率 Monte Carlo 对已知值：AA 对随机
    /// 牌约 85%、AhKd 对 AA 约 7.4%（穷举）、72o 对随机牌约 35%、AA 对两个随机对手约 73%。
    #[test]
    fn allin_range_and_equity_sanity() {
        let top6 = allin_range(6.0 / 1326.0);
        assert_eq!(top6.len(), 6);
        assert!(top6
            .iter()
            .all(|h| h[0].rank() == poker::Rank::Ace && h[1].rank() == poker::Rank::Ace));
        assert_eq!(allin_range(1.0).len(), 1326);
        let c = |a: &str, b: &str| [parse_card(a).unwrap(), parse_card(b).unwrap()];
        let eq = |hole, ranges: &[&[[Card; 2]]]| allin_equity(hole, ranges, 10_000, 7).unwrap();
        let all = allin_range(1.0);
        let near = |x: f64, want: f64| (x - want).abs() < 0.02;
        assert!(near(eq(c("Ah", "Ad"), &[all]), 0.852));
        // AhKd 对 AA（排除 Ah 后剩 3 个组合）：穷举公共牌得底池份额 0.0743。
        assert!(near(eq(c("Ah", "Kd"), &[top6]), 0.0743));
        assert!(near(eq(c("7h", "2d"), &[all]), 0.35));
        assert!(near(eq(c("Ah", "Ad"), &[all, all]), 0.73));
    }

    /// 面对全下的请求（6-max，button=0：SB=1 BB=2 UTG=3 HJ=4 CO=5）。`stacks` = hand-start 真栈。
    fn allin_req(
        hole: [&str; 2],
        my_seat: u8,
        actions: Vec<HistAction>,
        stacks: Vec<u64>,
    ) -> Request {
        Request {
            hole: hole.iter().map(|c| c.to_string()).collect(),
            board: vec![],
            button_seat: 0,
            my_seat,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions,
            valid: ValidActions {
                can_check: false,
                can_call: true,
                can_raise: false,
                min_raise: None,
                max_raise: None,
            },
            stacks,
            dealt_seats: vec![],
            names: Default::default(),
        }
    }

    /// 翻前面对短码全下（审查 §5）：有效码深 < 60BB 不走 blueprint，改走全下启发式；用牌都离
    /// 判断线 ≥ 0.03（胜率见 [`allin_equity`] 抽样）。
    /// - BTN 开池 2.25BB、SB 25BB 3bet 全下：99 跟（旧兜底只跟 P 档）、A8o 弃；
    /// - 同一线 SB 100BB 全下：码深够 → 仍走 blueprint；
    /// - UTG 10BB 开池全下、hero BB：KJo 跟、Q7o 弃；
    /// - hero 只有 15BB、SB 100BB 3bet 全下：按 hero 码深判短 → 启发式，AKo 跟、AJo 弃；
    /// - UTG 10BB 全下 + HJ 跟注、hero BB：多人底池，99 跟、A8o 弃。
    #[test]
    fn preflop_short_allin_uses_heuristic_not_blueprint() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let run = |req: &Request, rt: Option<&SearchRuntime>| {
            decide(
                &game,
                &abs,
                &uniform,
                req,
                0xA111,
                rt,
                &mut SubgameSolveCache::new(),
            )
        };
        let reshove = |sb_stack: u64| {
            vec![
                hist(3, "fold", None),
                hist(4, "fold", None),
                hist(5, "fold", None),
                hist(0, "raise", Some(45)),
                hist(
                    1,
                    "all_in",
                    if sb_stack == 0 { None } else { Some(sb_stack) },
                ),
                hist(2, "fold", None),
            ]
        };
        let mut stacks = vec![2000; 6];
        stacks[1] = 500;
        for (hole, want) in [(["9h", "9d"], "call"), (["Ah", "8d"], "fold")] {
            let req = allin_req(hole, 0, reshove(500), stacks.clone());
            for rt in [None, Some(rt(SubgameSearchConfig::default()))] {
                let r = run(&req, rt.as_ref());
                assert_eq!(r.action, want, "{hole:?}: {r:?}");
                assert_eq!(r.source, format!("fallback:preflop_allin:{want}"), "{r:?}");
            }
        }
        // driver 不带 amount 的 all_in 同样识别。
        let r = run(
            &allin_req(["9h", "9d"], 0, reshove(0), stacks.clone()),
            None,
        );
        assert_eq!(r.source, "fallback:preflop_allin:call", "{r:?}");
        // SB 100BB 全下：码深够，照常 blueprint。
        let r = run(
            &allin_req(["9h", "9d"], 0, reshove(2000), vec![2000; 6]),
            None,
        );
        assert_eq!(r.source, "blueprint", "{r:?}");

        // UTG 10BB 开池全下，hero = BB。
        let open = vec![
            hist(3, "all_in", Some(200)),
            hist(4, "fold", None),
            hist(5, "fold", None),
            hist(0, "fold", None),
            hist(1, "fold", None),
        ];
        let mut stacks = vec![2000; 6];
        stacks[3] = 200;
        for (hole, want) in [(["Kh", "Jd"], "call"), (["Qh", "7d"], "fold")] {
            let r = run(&allin_req(hole, 2, open.clone(), stacks.clone()), None);
            assert_eq!(
                r.source,
                format!("fallback:preflop_allin:{want}"),
                "{hole:?}: {r:?}"
            );
        }

        // hero（BTN）只有 15BB，SB 100BB 3bet 全下。
        let mut stacks = vec![2000; 6];
        stacks[0] = 300;
        for (hole, want) in [(["Ah", "Kd"], "call"), (["Ah", "Jd"], "fold")] {
            let r = run(&allin_req(hole, 0, reshove(2000), stacks.clone()), None);
            assert_eq!(
                r.source,
                format!("fallback:preflop_allin:{want}"),
                "{hole:?}: {r:?}"
            );
        }

        // UTG 10BB 全下 + HJ 跟注，hero = BB。
        let multi = vec![
            hist(3, "all_in", Some(200)),
            hist(4, "call", None),
            hist(5, "fold", None),
            hist(0, "fold", None),
            hist(1, "fold", None),
        ];
        let mut stacks = vec![2000; 6];
        stacks[3] = 200;
        for (hole, want) in [(["9h", "9d"], "call"), (["Ah", "8d"], "fold")] {
            let req = allin_req(hole, 2, multi.clone(), stacks.clone());
            let smap = seat_map(&req, 6).unwrap();
            let hc = [parse_card(hole[0]).unwrap(), parse_card(hole[1]).unwrap()];
            let spot =
                preflop_allin_spot(&req, game.table_config(), 5, &smap, SeatId(2), hc).unwrap();
            assert_eq!(spot.callers, vec![4], "{spot:?}");
            assert!(spot.open_shove);
            // 跟注 900（solver 单位），底池 = 1000 + 1000 + 50 + hero 1000。
            assert_eq!((spot.to_call, spot.win_pot), (900, 3050), "{spot:?}");
            let r = run(&req, None);
            assert_eq!(
                r.source,
                format!("fallback:preflop_allin:{want}"),
                "{hole:?}: {r:?}"
            );
        }
    }

    /// 尺寸闸边界：真实 / 映射 ∈ [0.75, 1.333]（含端点）才过；只看指定街；映射不是进攻动作
    /// （`mapped_milli = 0`）必拒；本街没有下注恒过。
    #[test]
    fn flop_blueprint_size_gate_ratio_band() {
        let m = |street, real_milli, mapped_milli| BetSizeMap {
            street,
            real_milli,
            mapped_milli,
        };
        let flop = poker::Street::Flop;
        let gate = |sizes: &[BetSizeMap]| flop_blueprint_size_gate(sizes, flop);
        assert_eq!(gate(&[]), Ok(()));
        assert_eq!(gate(&[m(flop, 750, 1000)]), Ok(()));
        assert_eq!(gate(&[m(flop, 1333, 1000)]), Ok(()));
        assert_eq!(gate(&[m(flop, 500, 500)]), Ok(()));
        assert_eq!(gate(&[m(flop, 749, 1000)]), Err("size_map:749/1000".into()));
        assert_eq!(
            gate(&[m(flop, 1334, 1000)]),
            Err("size_map:1334/1000".into())
        );
        // 1/3 池 c-bet 映射成 1pot（all-1pot 翻牌菜单）。
        assert!(gate(&[m(flop, 333, 1000)]).is_err());
        // 先过后不过：任一次下注不过即拒。
        assert!(gate(&[m(flop, 1000, 1000), m(flop, 2000, 1000)]).is_err());
        assert!(gate(&[m(flop, 1000, 0)]).is_err());
        // 翻前的尺寸误差不影响翻牌闸。
        assert_eq!(
            gate(&[m(poker::Street::Preflop, 3000, 1000), m(flop, 1000, 1000)]),
            Ok(())
        );
    }

    /// 尺寸闸端到端（审查 §3）：flop 锚定面、我(SB) check 后 BB 下注，`--search-flop-prefer-blueprint`
    /// on + AllPostflop。测试树翻牌菜单 {0.5, 1}：BB 下 1pot（正好在菜单上）/ 1.2pot（映射成 1pot，
    /// 误差 1.2 在区间内）→ 仍走 blueprint，与 search=None byte-equal；1.5pot（映射成 1pot，误差 1.5）
    /// → 改走锚定搜索。lockstep 记下的尺寸按池比例核对。
    #[test]
    fn flop_prefer_blueprint_size_gate_routes_off_menu_bet_to_search() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::AllPostflop,
            ..SubgameSearchConfig::default()
        };
        // 翻牌底池 40op（SB 补齐 + BB 过牌）；BB 下注到 `bet_to`op。
        let facing_bet = |bet_to: u64| {
            let mut req = flop_first_unraised_req(vec![2000; 6]);
            req.actions.push(hist(1, "check", None));
            req.actions.push(hist(2, "bet", Some(bet_to)));
            req.valid = ValidActions {
                can_check: false,
                can_call: true,
                can_raise: true,
                min_raise: Some(bet_to * 2),
                max_raise: Some(1980),
            };
            req
        };
        let run = |req: &Request, rt: Option<&SearchRuntime>| {
            decide(
                &game,
                &abs,
                &uniform,
                req,
                0xB1DE,
                rt,
                &mut SubgameSolveCache::new(),
            )
        };
        for (bet_to, real_milli, mapped_milli, blueprint) in [
            (40, 1000, 1000, true),
            (48, 1200, 1000, true),
            (60, 1500, 1000, false),
        ] {
            let req = facing_bet(bet_to);
            let smap = seat_map(&req, 6).unwrap();
            let (.., sizes) = lockstep_replay(
                &game,
                game.table_config(),
                &smap,
                &req,
                5,
                3,
                Some(SeatId(smap.tree_seat(1).unwrap())),
                false,
            )
            .unwrap_or_else(|e| panic!("bet_to={bet_to}: 应锚定，得 {}", e.reason));
            assert_eq!(
                sizes,
                vec![BetSizeMap {
                    street: poker::Street::Flop,
                    real_milli,
                    mapped_milli,
                }],
                "bet_to={bet_to}"
            );
            let prefer = run(&req, Some(&rt_flop_prefer_blueprint(scfg)));
            assert!(is_legal(&prefer, &req.valid), "bet_to={bet_to}: {prefer:?}");
            if blueprint {
                assert_eq!(
                    prefer,
                    run(&req, None),
                    "bet_to={bet_to}: 映射误差在区间内须走 blueprint"
                );
            } else {
                assert!(
                    prefer.source == "search" || prefer.source.starts_with("search_giveup:"),
                    "bet_to={bet_to}: 映射误差超区间须改走搜索，得 {prefer:?}"
                );
            }
        }
    }

    /// 尺寸闸的预热同源：hero=SB 面对 BB 下注时预热，误差在区间内（1pot）→ skip；超区间（1.5pot）
    /// → 照常预热（decide 会搜索，不预热就白等一次现解）。
    #[test]
    fn prewarm_flop_prefer_blueprint_size_gate() {
        let game = nolimp_game();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::AllPostflop,
            ..SubgameSearchConfig::default()
        };
        for (bet_to, expect) in [
            (40, "prewarm:skip:flop_prefer_blueprint"),
            (60, "prewarm:stored"),
        ] {
            let mut req = flop_first_unraised_req(vec![2000; 6]);
            req.actions.push(hist(1, "check", None));
            req.actions.push(hist(2, "bet", Some(bet_to)));
            let mut cache = SubgameSolveCache::new();
            let p = prewarm(
                &game,
                &uniform,
                &req,
                0xF10B,
                Some(&rt_flop_prefer_blueprint(scfg)),
                &mut cache,
            );
            assert_eq!(p.source, expect, "bet_to={bet_to}: {p:?}");
        }
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
        // --warm-profile-hh：需 --exploit on|vpip；on / vpip 都接（都用画像）。
        assert!(
            parse(&["--search", "--warm-profile-hh", "x.jsonl"]).is_err(),
            "--warm-profile-hh 无 --exploit 应拒收"
        );
        assert!(
            parse(&[
                "--search",
                "--exploit",
                "off",
                "--warm-profile-hh",
                "x.jsonl"
            ])
            .is_err(),
            "--exploit off 不该接 --warm-profile-hh"
        );
        let a = parse(&[
            "--search",
            "--exploit",
            "on",
            "--warm-profile-hh",
            "hh.jsonl",
        ])
        .expect("parse Ok");
        assert_eq!(
            a.warm_profile_hh.as_deref(),
            Some(std::path::Path::new("hh.jsonl"))
        );
        assert!(parse(&[
            "--search",
            "--exploit",
            "vpip",
            "--warm-profile-hh",
            "hh.jsonl"
        ])
        .expect("parse Ok")
        .warm_profile_hh
        .is_some());
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
        let smap = seat_map(&req, 6).expect("seat_map");
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

    /// `--search-live-traversers on|off`：advisor 默认开（DEFAULT_SEARCH_LIVE_TRAVERSERS，库层
    /// `SubgameSearchConfig::default()` 仍为 false）、off 显式关、on 显式开、坏值拒收、未开 --search 拒收。
    #[test]
    fn parse_args_search_live_traversers() {
        let parse = |extra: &[&str]| {
            let argv = ["--checkpoint", "c.ckpt", "--bucket-table", "b.bin"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string());
            parse_args_from(argv)
        };
        let a = parse(&["--search"]).expect("parse Ok");
        assert!(a.search.expect("search on").live_traversers, "默认开");
        assert!(
            !SubgameSearchConfig::default().live_traversers,
            "库层默认不变"
        );
        let a = parse(&["--search", "--search-live-traversers", "off"]).expect("parse Ok");
        assert!(!a.search.expect("search on").live_traversers, "off 应关");
        let a = parse(&["--search", "--search-live-traversers", "on"]).expect("parse Ok");
        assert!(a.search.expect("search on").live_traversers, "on 应开");
        assert!(parse(&["--search", "--search-live-traversers", "maybe"]).is_err());
        // 旧的裸 flag 写法（无取值）不再接受：会把下一个参数当取值 → 报错，不静默。
        assert!(parse(&["--search", "--search-live-traversers"]).is_err());
        assert!(parse(&["--search-live-traversers", "on"]).is_err());
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

    /// `--search-max-nodes`：默认 [`DEFAULT_SEARCH_MAX_NODES`]（20 万，不是通用默认 8000）；显式值覆盖；
    /// 未开 --search 拒收（显式写默认值也拒，拒绝静默 guard）。
    #[test]
    fn parse_args_search_max_nodes() {
        let parse = |extra: &[&str]| {
            let argv = ["--checkpoint", "c.ckpt", "--bucket-table", "b.bin"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string());
            parse_args_from(argv)
        };
        let nodes = |a: Args| a.search.expect("search on").max_subtree_nodes;
        assert_eq!(nodes(parse(&["--search"]).unwrap()), 200_000);
        assert_eq!(
            nodes(parse(&["--search", "--search-max-nodes", "50000"]).unwrap()),
            50_000
        );
        assert!(parse(&["--search-max-nodes", "200000"]).is_err());
        assert!(parse(&["--search-max-nodes", "8000"]).is_err());
    }

    /// `--search-limp-as-fold on|off`（方案 B，默认关）：默认关、on/off、坏值拒收、未开 --search 拒收。
    #[test]
    fn parse_args_limp_as_fold() {
        let parse = |extra: &[&str]| {
            let argv = ["--checkpoint", "c.ckpt", "--bucket-table", "b.bin"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string());
            parse_args_from(argv)
        };
        assert!(!parse(&["--search"]).unwrap().search_limp_as_fold, "默认关");
        assert!(
            parse(&["--search", "--search-limp-as-fold", "on"])
                .unwrap()
                .search_limp_as_fold
        );
        assert!(
            !parse(&["--search", "--search-limp-as-fold", "off"])
                .unwrap()
                .search_limp_as_fold
        );
        assert!(parse(&["--search", "--search-limp-as-fold", "maybe"]).is_err());
        assert!(parse(&["--search-limp-as-fold", "on"]).is_err());
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

    // =======================================================================
    // crop blueprint（--cropped-checkpoint）：翻前 / 翻牌逐位等价 + 转牌 / 河牌只走搜索
    // =======================================================================

    use poker::training::nlhe_betting_tree::all_1pot_6max;
    use poker::training::nlhe_dense_crop::{crop_dense_checkpoint, CropOptions};

    /// crop 测试小 game：advisor `--seats` 默认 6 座 100BB（`default_6max_100bb`）→ 6-max 100BB +
    /// all-1pot N=2（dense 两表 ≈ 49 MiB）+ stub 200 桶（postflop 全落桶 0 → 小训练量即覆盖 flop/turn/river
    /// 节点，等价性不空验）。`--reshape all-1pot --postflop-cap 2` 的同一棵树（[`reshape_profile`]）。
    fn crop_test_game() -> SimplifiedNlheGame {
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::new(200, 200, 200).expect("200/200/200"),
        ));
        let (abs, rules) = reshape_profile("all-1pot", 2).expect("all-1pot");
        let g = SimplifiedNlheGame::new_with_abstraction(
            Arc::clone(&table),
            TableConfig::default_6max_100bb(),
            abs,
            rules,
        )
        .expect("all-1pot 6max game");
        let (a2, r2) = all_1pot_6max(2);
        let direct = SimplifiedNlheGame::new_with_abstraction(
            table,
            TableConfig::default_6max_100bb(),
            a2,
            r2,
        )
        .expect("direct");
        assert_eq!(
            g.tree().num_nodes(),
            direct.tree().num_nodes(),
            "advisor reshape 须与 all_1pot_6max 同一树定义"
        );
        g
    }

    /// 测试临时目录：drop（含 panic 展开）时删除，失败的测试不留 ~50 MiB 残留。
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// 一次性造 full ckpt + crop(keep_through=Flop, 不留 regret)，返回 (dir 守卫, full, crop)。
    /// LCFR period 小 → 表处于 lazy scale != 1 状态（覆盖「写逻辑值」与查询一致性）。
    fn crop_fixture(tag: &str) -> (TempDir, PathBuf, PathBuf) {
        let mut t = DenseNlheEsMccfrTrainer::new(crop_test_game(), 7).with_lcfr_period(500);
        let mut rng = ChaCha20Rng::from_seed(0xC0_0001);
        for _ in 0..2_000 {
            t.step(&mut rng).expect("step");
        }
        let dir = std::env::temp_dir().join(format!("advisor_crop_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let full = dir.join("full.ckpt");
        let crop = dir.join("pf.crop");
        t.save_checkpoint(&full).expect("save");
        crop_dense_checkpoint(
            &full,
            &crop,
            t.game(),
            CropOptions {
                keep_through: StreetTag::Flop,
                keep_regret: false,
            },
        )
        .expect("crop");
        (TempDir(dir), full, crop)
    }

    fn bits_eq(a: &[f64], b: &[f64]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
    }

    fn ha(seat: u8, action: &str, to: Option<u64>) -> HistAction {
        HistAction {
            seat,
            action: action.into(),
            to,
        }
    }

    /// SB-vs-BB 线（button=0；UTG/HJ/CO/BTN fold，SB complete、BB check）。我 = SB(seat1)，postflop
    /// 每街首个行动者。`extra` = flop 起的后续动作，`board` 决定街。对称 100BB（无 stacks → 锚定路径）。
    fn sb_line_req(
        hole: [&str; 2],
        board: &[&str],
        extra: &[HistAction],
        valid: ValidActions,
    ) -> Request {
        let mut actions = vec![
            ha(3, "fold", None),
            ha(4, "fold", None),
            ha(5, "fold", None),
            ha(0, "fold", None),
            ha(1, "call", Some(20)),
            ha(2, "check", None),
        ];
        actions.extend_from_slice(extra);
        Request {
            hole: hole.iter().map(|c| c.to_string()).collect(),
            board: board.iter().map(|c| c.to_string()).collect(),
            button_seat: 0,
            my_seat: 1,
            num_seats: 6,
            small_blind: 10,
            big_blind: 20,
            actions,
            valid,
            stacks: vec![],
            dealt_seats: vec![],
            names: Default::default(),
        }
    }

    fn check_valid() -> ValidActions {
        ValidActions {
            can_check: true,
            can_call: false,
            can_raise: true,
            min_raise: Some(20),
            max_raise: Some(1980),
        }
    }

    /// 同手四个决策：flop 首点 / turn 首点 / river 首点 / river 面对 1pot 下注（mid-round）。
    fn sb_line_hand(hole: [&str; 2]) -> Vec<Request> {
        let flop = ["7h", "2c", "Ks"];
        let turn = ["7h", "2c", "Ks", "4d"];
        let river = ["7h", "2c", "Ks", "4d", "9s"];
        let flop_x = [ha(1, "check", None), ha(2, "check", None)];
        let turn_x = [flop_x.as_slice(), &flop_x].concat();
        let river_bet = [
            turn_x.as_slice(),
            &[ha(1, "check", None), ha(2, "raise", Some(40))],
        ]
        .concat();
        vec![
            sb_line_req(hole, &flop, &[], check_valid()),
            sb_line_req(hole, &turn, &flop_x, check_valid()),
            sb_line_req(hole, &river, &turn_x, check_valid()),
            sb_line_req(
                hole,
                &river,
                &river_bet,
                ValidActions {
                    can_check: false,
                    can_call: true,
                    can_raise: true,
                    min_raise: Some(80),
                    max_raise: Some(1980),
                },
            ),
        ]
    }

    fn crop_rt(bp: &Blueprint, cfg: SubgameSearchConfig) -> SearchRuntime {
        SearchRuntime {
            cfg: SubgameSearchConfig {
                blueprint_max_street: bp.keep_through(),
                ..cfg
            },
            bucket_table: None,
            unanchored_prefix_reach: DEFAULT_SEARCH_UNANCHORED_PREFIX_REACH,
            unanchored_cross_street: DEFAULT_SEARCH_UNANCHORED_CROSS_STREET,
            flop_prefer_blueprint: false,
            limp_as_fold: false,
            exploit: None,
        }
    }

    fn crop_scfg() -> SubgameSearchConfig {
        SubgameSearchConfig {
            iterations: 300,
            trigger: SearchTrigger::AllPostflop,
            max_subtree_nodes: 1_000_000,
            range_uniform_mix: DEFAULT_SEARCH_RANGE_UNIFORM_MIX,
            ..SubgameSearchConfig::default()
        }
    }

    /// ① 翻前 / 翻牌：crop 与完整 blueprint **逐位一致**——(a) 底层 σ（`Blueprint::strategy`）在翻前 /
    /// 翻牌各决策 infoset 上 to_bits 相等且非空（非空验）；(b) advisor 输出（`Response` 全字段：
    /// 动作 / 金额 / probs / chosen / info_set / solve_updates）在 blueprint 路径（翻前、翻牌不搜、翻牌
    /// `flop_prefer_blueprint`）与翻牌锚定搜索路径（range 先验读翻前 σ）+ 翻牌预热上全等；(c) 全程
    /// crop 安全网计数 0。另：被裁街 `strategy` 计数（安全网自检）、错用 flag 的 magic 提示。
    #[test]
    fn cropped_blueprint_preflop_flop_bitwise_equal() {
        let (_dir, full_p, crop_p) = crop_fixture("eq");
        let full = load_blueprint(&full_p, crop_test_game(), false).expect("load full");
        let crop = load_blueprint(&crop_p, crop_test_game(), true).expect("load crop");
        assert_eq!(crop.keep_through(), StreetTag::Flop);
        assert_eq!(full.update_count(), crop.update_count());
        let game = full.game();
        let abs = game.abstraction().clone();
        let sf = |i: &InfoSetId, _n: usize| full.strategy(i);
        let sc = |i: &InfoSetId, _n: usize| crop.strategy(i);

        // 翻前决策：UTG 首动 / BTN 面对 3 家 fold / SB 面对 BTN open。
        let pre_reqs = [
            (3u8, vec![]),
            (
                0,
                vec![
                    ha(3, "fold", None),
                    ha(4, "fold", None),
                    ha(5, "fold", None),
                ],
            ),
            (
                1,
                vec![
                    ha(3, "fold", None),
                    ha(4, "fold", None),
                    ha(5, "fold", None),
                    ha(0, "raise", Some(70)),
                ],
            ),
        ];
        let mut nonvacuous = 0;
        for hole in [["Ah", "Kd"], ["7c", "2d"], ["Qs", "Qh"], ["9h", "8h"]] {
            for (my_seat, actions) in &pre_reqs {
                let mut req = sb_line_req(hole, &[], &[], full_valid());
                req.my_seat = *my_seat;
                req.actions = actions.clone();
                // σ 逐位（经 lockstep 取该决策的真 infoset）。
                let smap = seat_map(&req, 6).expect("seat map");
                let (_real, ab, ..) = lockstep_replay(
                    game,
                    &TableConfig::default_6max_100bb(),
                    &smap,
                    &req,
                    5,
                    0,
                    Some(SeatId(smap.tree_seat(*my_seat).unwrap())),
                    false,
                )
                .unwrap_or_else(|e| panic!("preflop 重放应 Ok: {}", e.reason));
                let hole_c = [parse_card(hole[0]).unwrap(), parse_card(hole[1]).unwrap()];
                let info = game.info_set_for_cards(ab.current_node_id, hole_c, &[]);
                assert_eq!(info.street_tag(), StreetTag::Preflop);
                let (a, b) = (full.strategy_raw(&info), crop.strategy_raw(&info));
                assert!(
                    bits_eq(&a, &b),
                    "翻前 σ 须逐位一致 @ {hole:?} seat{my_seat}"
                );
                nonvacuous += usize::from(!a.is_empty());
                for search in [None, Some(crop_rt(&full, crop_scfg()))] {
                    let rt_c = search.as_ref().map(|_| crop_rt(&crop, crop_scfg()));
                    let rf = decide(
                        game,
                        &abs,
                        &sf,
                        &req,
                        0x5EED,
                        search.as_ref(),
                        &mut SubgameSolveCache::new(),
                    );
                    let rc = decide(
                        game,
                        &abs,
                        &sc,
                        &req,
                        0x5EED,
                        rt_c.as_ref(),
                        &mut SubgameSolveCache::new(),
                    );
                    assert_eq!(rf, rc, "翻前 advisor 输出须全等 @ {hole:?} seat{my_seat}");
                    assert!(bp_or_placeholder(&rf.source), "{rf:?}");
                }
            }
        }
        assert!(
            nonvacuous >= 6,
            "翻前 σ 大多应已被训练访问（否则等价性是空验）: {nonvacuous}"
        );

        // 翻牌：σ 逐位 + blueprint 路径（不搜 / flop_prefer_blueprint）+ 锚定搜索 + 预热 全等。
        for hole in [["Ah", "Kd"], ["7c", "2d"], ["Qs", "Qh"]] {
            let flop_req = &sb_line_hand(hole)[0];
            let smap = seat_map(flop_req, 6).unwrap();
            let (_real, ab, ..) = lockstep_replay(
                game,
                &TableConfig::default_6max_100bb(),
                &smap,
                flop_req,
                5,
                3,
                Some(SeatId(1)),
                false,
            )
            .unwrap_or_else(|e| panic!("flop 重放应 Ok: {}", e.reason));
            let hole_c = [parse_card(hole[0]).unwrap(), parse_card(hole[1]).unwrap()];
            let board: Vec<Card> = flop_req
                .board
                .iter()
                .map(|c| parse_card(c).unwrap())
                .collect();
            let info = game.info_set_for_cards(ab.current_node_id, hole_c, &board);
            assert_eq!(info.street_tag(), StreetTag::Flop);
            let (a, b) = (full.strategy_raw(&info), crop.strategy_raw(&info));
            assert!(!a.is_empty(), "翻牌 infoset 应已被训练访问（stub 桶）");
            assert!(bits_eq(&a, &b), "翻牌 σ 须逐位一致 @ {hole:?}");

            let prefer = |bp: &Blueprint| SearchRuntime {
                flop_prefer_blueprint: true,
                ..crop_rt(bp, crop_scfg())
            };
            let rf = decide(
                game,
                &abs,
                &sf,
                flop_req,
                0x5EED,
                None,
                &mut SubgameSolveCache::new(),
            );
            let rc = decide(
                game,
                &abs,
                &sc,
                flop_req,
                0x5EED,
                Some(&prefer(&crop)),
                &mut SubgameSolveCache::new(),
            );
            assert!(bp_or_placeholder(&rf.source), "{rf:?}");
            assert_eq!(rf, rc, "翻牌 blueprint 路径须全等 @ {hole:?}");

            let (rt_f, rt_c) = (crop_rt(&full, crop_scfg()), crop_rt(&crop, crop_scfg()));
            let rf = decide(
                game,
                &abs,
                &sf,
                flop_req,
                0x5EED,
                Some(&rt_f),
                &mut SubgameSolveCache::new(),
            );
            let rc = decide(
                game,
                &abs,
                &sc,
                flop_req,
                0x5EED,
                Some(&rt_c),
                &mut SubgameSolveCache::new(),
            );
            assert_eq!(rf.source, "search", "翻牌锚定搜索应解成功：{rf:?}");
            assert_eq!(rf, rc, "翻牌搜索输出须全等 @ {hole:?}");

            let (mut cf, mut cc) = (SubgameSolveCache::new(), SubgameSolveCache::new());
            let pf = prewarm(game, &sf, flop_req, 0x5EED, Some(&rt_f), &mut cf);
            let pc = prewarm(game, &sc, flop_req, 0x5EED, Some(&rt_c), &mut cc);
            assert_eq!(pf.source, "prewarm:stored");
            assert_eq!(pf, pc, "翻牌预热须全等");
            let rf = decide(game, &abs, &sf, flop_req, 0x5EED, Some(&rt_f), &mut cf);
            let rc = decide(game, &abs, &sc, flop_req, 0x5EED, Some(&rt_c), &mut cc);
            assert_eq!((cf.hits(), cc.hits()), (1, 1), "预热后决策须命中");
            assert_eq!(rf, rc);
        }
        assert_eq!(crop.cropped_queries(), 0, "翻前 / 翻牌全程不应查询被裁街");

        // 安全网自检：直接查被裁街 → 空 Vec + 计数（主循环据此作废输出）。
        let tree = game.tree();
        let turn_node = (0..tree.num_nodes() as NodeId)
            .find(|&id| tree.node(id).street == StreetTag::Turn)
            .expect("树有 turn 节点");
        let turn_board: Vec<Card> = ["7h", "2c", "Ks", "4d"]
            .iter()
            .map(|c| parse_card(c).unwrap())
            .collect();
        let hole_c = [parse_card("Ah").unwrap(), parse_card("Kd").unwrap()];
        let turn_info = game.info_set_for_cards(turn_node, hole_c, &turn_board);
        assert_eq!(turn_info.street_tag(), StreetTag::Turn);
        assert!(crop.strategy(&turn_info).is_empty());
        assert_eq!(crop.cropped_queries(), 1);
        assert!(!full.strategy(&turn_info).is_empty() || full.cropped_queries() == 0);

        // 错用 flag：crop 文件走 --checkpoint / 完整 ckpt 走 --cropped-checkpoint → 拒绝 + 提示。
        let e = load_blueprint(&crop_p, crop_test_game(), false)
            .err()
            .expect("crop 当完整加载须失败");
        assert!(e.contains("--cropped-checkpoint"), "{e}");
        let e = load_blueprint(&full_p, crop_test_game(), true)
            .err()
            .expect("完整当 crop 加载须失败");
        assert!(e.contains("--checkpoint"), "{e}");
    }

    /// ② 转牌 / 河牌：crop 下全部走实时搜索、全程**零**被裁街查询。
    /// (a) 完整一手（flop → turn → river 首点 → river 面对下注，共享 solve 缓存 = 生产顺序）：每决策
    ///     `source=search`，且与完整 blueprint 同序列**逐字节相同**（river 先验 = turn 子树跨街后验，
    ///     两边都不读 turn σ；turn 先验只读翻前 + 翻牌 σ，逐位一致）。
    /// (b) 依赖确实存在（非空验）：完整 blueprint + **空缓存**直接问 river → range 先验读了 turn σ
    ///     （计数 > 0）——这正是 crop 必须绕开的读取。
    /// (c) crop + 空缓存直接问 river（跨街后验不可得）→ 按需补解 turn（预热路径）后重试：
    ///     `source=search`，缓存 2 次 miss（turn 补解 + river），零被裁街查询；关掉跨街复用则无法补救 →
    ///     `search_giveup:crop_no_posterior:*`（显式降级，不读 turn σ、不退均匀）。
    /// (d) blueprint 路径守卫：crop + 只搜 flop 的触发面（启动期本会拒绝）问 turn → 显式
    ///     `fallback:blueprint_cropped:turn`，不查 σ。
    #[test]
    fn cropped_blueprint_turn_river_search_only() {
        let (_dir, full_p, crop_p) = crop_fixture("tr");
        let full = load_blueprint(&full_p, crop_test_game(), false).expect("load full");
        let crop = load_blueprint(&crop_p, crop_test_game(), true).expect("load crop");
        let game = crop.game();
        let abs = game.abstraction().clone();
        let sf = |i: &InfoSetId, _n: usize| full.strategy(i);
        let sc = |i: &InfoSetId, _n: usize| crop.strategy(i);
        // 1000 iterations: at crop_scfg's 300 the QQ river-facing-bet infoset only ever had its
        // uniform placeholder average (own reach 0 in every hero traversal); the decision used
        // to pass on it, read_current_strategy now rejects it as unsolved.
        let scfg = SubgameSearchConfig {
            iterations: 1000,
            ..crop_scfg()
        };
        let (rt_f, rt_c) = (crop_rt(&full, scfg), crop_rt(&crop, scfg));
        validate_crop_runtime(crop.keep_through(), Some(&rt_c)).expect("生产约束满足");

        // (a) 完整一手。
        for hole in [["Ah", "Kd"], ["Qs", "Qh"]] {
            let hand = sb_line_hand(hole);
            let (mut cf, mut cc) = (SubgameSolveCache::new(), SubgameSolveCache::new());
            for (k, req) in hand.iter().enumerate() {
                let rf = decide(game, &abs, &sf, req, 0xBEEF, Some(&rt_f), &mut cf);
                let rc = decide(game, &abs, &sc, req, 0xBEEF, Some(&rt_c), &mut cc);
                assert_eq!(
                    rc.source, "search",
                    "决策 {k}（{hole:?}）crop 须走搜索：{rc:?}"
                );
                assert!(is_legal(&rc, &req.valid));
                assert_eq!(rf, rc, "决策 {k}（{hole:?}）crop 与完整须逐字节相同");
            }
            assert!(
                cc.cross_hits() >= 2,
                "turn/river 首点应命中跨街后验: {}",
                cc.cross_hits()
            );
        }
        assert_eq!(crop.cropped_queries(), 0, "整手不应查询被裁街");

        // (b) 依赖存在：完整 blueprint 空缓存问 river → 读了 turn σ。
        let river_req = &sb_line_hand(["Ah", "Kd"])[2];
        let turn_reads = std::cell::Cell::new(0u64);
        let counting = |i: &InfoSetId, _n: usize| {
            if i.street_tag() as u8 > StreetTag::Flop as u8 {
                turn_reads.set(turn_reads.get() + 1);
            }
            full.strategy(i)
        };
        let r = decide(
            game,
            &abs,
            &counting,
            river_req,
            0xBEEF,
            Some(&rt_f),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(r.source, "search");
        assert!(
            turn_reads.get() > 0,
            "完整 blueprint 的 river 先验本会读 turn σ（否则本测试空验）"
        );

        // (c) crop 空缓存问 river → 补解 turn → 重试成功。决策与预热两条入口都验。
        let mut c = SubgameSolveCache::new();
        let r = decide(game, &abs, &sc, river_req, 0xBEEF, Some(&rt_c), &mut c);
        assert_eq!(r.source, "search", "补解 turn 后 river 应解成功：{r:?}");
        assert_eq!(c.misses(), 2, "turn 补解 + river 各一次 solve");
        assert_eq!(c.cross_hits(), 1);
        let mut c = SubgameSolveCache::new();
        let p = prewarm(game, &sc, river_req, 0xBEEF, Some(&rt_c), &mut c);
        assert_eq!(p.source, "prewarm:stored", "river 预热同样补解 turn：{p:?}");
        let r2 = decide(game, &abs, &sc, river_req, 0xBEEF, Some(&rt_c), &mut c);
        assert_eq!(r2.source, "search");
        assert_eq!(r2, r, "预热补解与决策补解是同一份 turn 解 → 同 river 决策");
        // 脱锚入口（off-stack 真栈 → lockstep 失同步）：river 同样补解成功、零查询。
        let mut off = river_req.clone();
        off.stacks = vec![2000, 300, 2000, 2000, 2000, 2000];
        let r = decide(
            game,
            &abs,
            &sc,
            &off,
            0xBEEF,
            Some(&rt_c),
            &mut SubgameSolveCache::new(),
        );
        assert!(
            r.source == "search:unanchored" || r.source == "search",
            "短码 river 应搜索成功：{r:?}"
        );
        // 关跨街复用（启动期本会拒绝）：无从补救 → 显式 crop giveup。
        let no_cross = SearchRuntime {
            unanchored_cross_street: false,
            ..crop_rt(&crop, crop_scfg())
        };
        assert!(validate_crop_runtime(StreetTag::Flop, Some(&no_cross)).is_err());
        let r = decide(
            game,
            &abs,
            &sc,
            river_req,
            0xBEEF,
            Some(&no_cross),
            &mut SubgameSolveCache::new(),
        );
        assert!(
            r.source.starts_with("search_giveup:crop_no_posterior:"),
            "无跨街后验须显式降级：{r:?}"
        );
        assert!(is_legal(&r, &river_req.valid));

        // (d) blueprint 路径守卫。
        let flop_only = SearchRuntime {
            cfg: SubgameSearchConfig {
                trigger: SearchTrigger::FlopFirstUnraised,
                ..rt_c.cfg
            },
            ..crop_rt(&crop, crop_scfg())
        };
        assert!(validate_crop_runtime(StreetTag::Flop, Some(&flop_only)).is_err());
        let turn_req = &sb_line_hand(["Ah", "Kd"])[1];
        let r = decide(
            game,
            &abs,
            &sc,
            turn_req,
            0xBEEF,
            Some(&flop_only),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(r.source, "fallback:blueprint_cropped:turn", "{r:?}");

        assert_eq!(crop.cropped_queries(), 0, "全程不应查询被裁街");
    }

    /// ③ crop 启动约束：`--cropped-checkpoint` 未开 `--search` → parse 报错；与 `--checkpoint` 互斥；
    /// 开了 `--search` 但触发面 / 跨街复用 / flop-prefer 不满足 → [`validate_crop_runtime`] 报错。
    #[test]
    fn cropped_checkpoint_requires_search() {
        let parse = |extra: &[&str]| {
            let argv = ["--cropped-checkpoint", "x.crop", "--bucket-table", "b.bin"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string());
            parse_args_from(argv)
        };
        let e = parse(&[]).err().expect("crop 不开 --search 须报错");
        assert!(e.contains("--search"), "{e}");
        let a = parse(&["--search", "--search-trigger", "all-postflop"]).expect("parse Ok");
        assert!(a.cropped && a.checkpoint.as_path() == std::path::Path::new("x.crop"));
        assert!(
            parse(&["--search", "--checkpoint", "c.ckpt"]).is_err(),
            "两 flag 互斥"
        );
        let a = parse_args_from(
            ["--checkpoint", "c.ckpt", "--bucket-table", "b.bin"]
                .iter()
                .map(|s| s.to_string()),
        )
        .expect("完整 ckpt 不需 --search");
        assert!(!a.cropped);

        let rt_with = |trigger, cross: bool, prefer: bool| SearchRuntime {
            cfg: SubgameSearchConfig {
                trigger,
                blueprint_max_street: StreetTag::Flop,
                ..SubgameSearchConfig::default()
            },
            bucket_table: None,
            unanchored_prefix_reach: true,
            unanchored_cross_street: cross,
            flop_prefer_blueprint: prefer,
            limp_as_fold: false,
            exploit: None,
        };
        let k = StreetTag::Flop;
        assert!(validate_crop_runtime(k, None).is_err());
        assert!(validate_crop_runtime(
            k,
            Some(&rt_with(SearchTrigger::FlopFirstUnraised, true, false))
        )
        .is_err());
        assert!(
            validate_crop_runtime(k, Some(&rt_with(SearchTrigger::AllPostflop, false, false)))
                .is_err()
        );
        assert!(
            validate_crop_runtime(k, Some(&rt_with(SearchTrigger::AllPostflop, true, false)))
                .is_ok()
        );
        // keep_through=Flop：flop 有数据 → flop-prefer 可开。
        assert!(
            validate_crop_runtime(k, Some(&rt_with(SearchTrigger::AllPostflop, true, true)))
                .is_ok()
        );
        // keep_through=Preflop：flop 无数据 → flop-prefer 不可开。
        let mut pre = rt_with(SearchTrigger::AllPostflop, true, true);
        pre.cfg.blueprint_max_street = StreetTag::Preflop;
        assert!(validate_crop_runtime(StreetTag::Preflop, Some(&pre)).is_err());
        // blueprint_max_street 与 crop 不一致 = 内部接线错误 → 拒绝。
        assert!(validate_crop_runtime(
            StreetTag::Turn,
            Some(&rt_with(SearchTrigger::AllPostflop, true, false))
        )
        .is_err());
    }

    /// [`offstack_allin_req`]（UTG 30BB preflop 全下 → 100BB 影子首动作即失同步）续打到河牌：
    /// flop / turn 均 SB-BB check-check，返回同手 [flop, turn, river] 三个 hero(SB) 首点决策。
    /// 每街都走**脱锚**路径；已同步前缀 = 根（断点在首动作）→ 档一前缀为空、range 先验退 uniform /
    /// 跨街后验——脱锚路径在 crop 下与完整 blueprint 同样不查任何 blueprint σ。
    fn offstack_hand() -> Vec<Request> {
        let flop = offstack_allin_req();
        let turn = offstack_allin_turn_req();
        let mut river = offstack_allin_turn_req();
        river.board.push("2s".into());
        river.actions.push(ha(1, "check", None));
        river.actions.push(ha(2, "check", None));
        vec![flop, turn, river]
    }

    /// ② 续（脱锚入口）：off-stack 线每街 `search:unanchored`；crop 与完整 blueprint 同手顺序决策逐字节
    /// 相同、空缓存直接问 river 也相同（前缀为空 → 不需要 turn σ、不触发补解），全程零被裁街查询。
    /// 「档一前缀含被裁街决策」的守卫在 subgame 层单测钉死（cap-2 测试树里多人池在翻前即失同步，
    /// advisor 层造不出 turn 才失同步的脱锚线）。
    #[test]
    fn cropped_blueprint_unanchored_river() {
        let (_dir, full_p, crop_p) = crop_fixture("un");
        let full = load_blueprint(&full_p, crop_test_game(), false).expect("load full");
        let crop = load_blueprint(&crop_p, crop_test_game(), true).expect("load crop");
        let game = crop.game();
        let abs = game.abstraction().clone();
        let sf = |i: &InfoSetId, _n: usize| full.strategy(i);
        let sc = |i: &InfoSetId, _n: usize| crop.strategy(i);
        let (rt_f, rt_c) = (crop_rt(&full, crop_scfg()), crop_rt(&crop, crop_scfg()));
        let hand = offstack_hand();
        let (mut cf, mut cc) = (SubgameSolveCache::new(), SubgameSolveCache::new());
        for (k, req) in hand.iter().enumerate() {
            let rf = decide(game, &abs, &sf, req, 0xD00D, Some(&rt_f), &mut cf);
            let rc = decide(game, &abs, &sc, req, 0xD00D, Some(&rt_c), &mut cc);
            assert_eq!(rc.source, "search:unanchored", "决策 {k}：{rc:?}");
            assert!(is_legal(&rc, &req.valid), "决策 {k}：{rc:?}");
            assert_eq!(rf, rc, "决策 {k} crop 与完整须逐字节相同");
        }
        let river = &hand[2];
        let rf = decide(
            game,
            &abs,
            &sf,
            river,
            0xD00D,
            Some(&rt_f),
            &mut SubgameSolveCache::new(),
        );
        let mut c = SubgameSolveCache::new();
        let rc = decide(game, &abs, &sc, river, 0xD00D, Some(&rt_c), &mut c);
        assert_eq!(rc.source, "search:unanchored");
        assert_eq!(c.misses(), 1, "前缀为空 → 无需补解");
        assert_eq!(rf, rc);
        assert_eq!(crop.cropped_queries(), 0, "全程不应查询被裁街");
    }

    // =======================================================================
    // N 座（--seats，9-max）：座位映射 / 座位数一致性 / 端到端 / 搜索规模诊断
    // =======================================================================

    /// 通用请求骨架（N 座桌，preflop、无历史）。
    fn n_req(n: u8, button: u8, my: u8, dealt: Vec<u8>) -> Request {
        Request {
            hole: vec!["Ah".into(), "Kd".into()],
            board: vec![],
            button_seat: button,
            my_seat: my,
            num_seats: n,
            small_blind: 10,
            big_blind: 20,
            actions: vec![],
            valid: full_valid(),
            stacks: vec![],
            dealt_seats: dealt,
            names: Default::default(),
        }
    }

    /// 选择 `items` 的所有 k 元子集（升序）。
    fn subsets(n: u8, k: usize) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for mask in 0u32..(1 << n) {
            if mask.count_ones() as usize == k {
                out.push((0..n).filter(|&i| mask >> i & 1 == 1).collect());
            }
        }
        out
    }

    /// 用规则引擎**独立**验证座位映射的位置语义（不复用被测公式）：
    /// - 满桌：OpenPoker 真实牌局 = button 在 `b` 的 N 座 `GameState`；
    /// - 短桌 k∈[3,N−1]：真实牌局 = k 座紧凑 ring（ring 下标 0 = 真实 button，j ↔ 发牌座
    ///   `dealt[(btn_idx+j)%k]`，盲注跳空座 = 引擎在 k 座上的标准盲注）。
    ///
    /// solver 侧 = N 座 button=0 的 `GameState` 先按 `phantoms` 依次 fold。两局走同一动作脚本
    /// （能 check 就 check、否则 call；每个 preflop 首次进攻者先加注一次以打乱行动序）直到摊牌，
    /// **每一步**断言 `solver.current_player == tree_seat(真实行动者的 OpenPoker 座)`——preflop +
    /// 三条 postflop 街的行动序全部对上，即位置语义与 solver 的 `(seat − button) % n` 一致。
    fn verify_seat_map_by_replay(n: u8, button: u8, dealt: &[u8]) {
        let req = n_req(n, button, button, dealt.to_vec());
        let smap = seat_map(&req, n as usize).expect("合法占座应可映射");
        let k = if dealt.is_empty() {
            n as usize
        } else {
            dealt.len()
        };
        // OpenPoker 座 ↔ 真实牌局座。
        let (mut real, real_to_op): (GameState, Vec<u8>) = if k == n as usize {
            let mut cfg = TableConfig::n_max_at_bb(n, 100);
            cfg.button_seat = SeatId(button);
            (GameState::new(&cfg, 1), (0..n).collect())
        } else {
            let bi = dealt.iter().position(|&s| s == button).unwrap();
            let ring: Vec<u8> = (0..k).map(|j| dealt[(bi + j) % k]).collect();
            (
                GameState::new(&TableConfig::n_max_at_bb(k as u8, 100), 1),
                ring,
            )
        };
        let mut solver = GameState::new(&TableConfig::n_max_at_bb(n, 100), 1);
        for &t in &smap.phantoms {
            assert_eq!(solver.current_player(), Some(SeatId(t)), "幻影 fold 顺序");
            solver.apply(Action::Fold).unwrap();
        }
        let mut raised = false;
        let mut steps = 0;
        while !real.is_terminal() {
            let actor = real.current_player().expect("非终局有行动者");
            let op = real_to_op[actor.0 as usize];
            assert_eq!(
                solver.current_player(),
                Some(SeatId(smap.tree_seat(op).unwrap())),
                "n={n} button={button} dealt={dealt:?} 第 {steps} 步（街 {:?}）行动者 op{op} 映射错",
                real.street()
            );
            let la = real.legal_actions();
            let open = la
                .raise_range
                .filter(|_| !raised && real.street() == poker::Street::Preflop);
            let a = if let Some((min_to, _)) = open {
                raised = true;
                Action::Raise { to: min_to }
            } else if la.check {
                Action::Check
            } else {
                Action::Call
            };
            real.apply(a).unwrap();
            solver.apply(a).unwrap();
            steps += 1;
        }
        assert!(solver.is_terminal(), "两局应同时终局");
        assert_eq!(
            real.street(),
            poker::Street::Showdown,
            "脚本应打到摊牌（覆盖三条 postflop 街）"
        );
    }

    /// 9 座：满桌（缺省 dealt / 显式全 9 座）每个 button 恒等公式 + 引擎重放；短桌 k∈[3,8] 的
    /// **全部**占座子集 × 每个发牌 button 引擎重放（k=3..8 共 Σ C(9,k)·k 个局面）；同一套检查在
    /// 6 座上也跑（与 9 座同一份代码，防两套逻辑漂移）。
    #[test]
    fn seat_map_n_max_positions_match_engine() {
        for n in [6u8, 9] {
            for b in 0..n {
                let m = seat_map(&n_req(n, b, b, vec![]), n as usize).unwrap();
                let expect: Vec<Option<u8>> = (0..n).map(|op| Some((op + n - b) % n)).collect();
                assert_eq!(m.to_tree, expect);
                assert!(m.phantoms.is_empty() && !m.hu);
                let all: Vec<u8> = (0..n).collect();
                let m2 = seat_map(&n_req(n, b, b, all.clone()), n as usize).unwrap();
                assert_eq!(m2.to_tree, expect, "显式全座 = 满桌");
                verify_seat_map_by_replay(n, b, &[]);
            }
            let mut cases = 0;
            for k in 3..n as usize {
                for dealt in subsets(n, k) {
                    for &b in &dealt {
                        let m = seat_map(&n_req(n, b, b, dealt.clone()), n as usize).unwrap();
                        let expect_ph: Vec<u8> = (3..3 + n - k as u8).collect();
                        assert_eq!(m.phantoms, expect_ph);
                        // 发牌座单射到非幻影树座；未发牌座 None。
                        let mut seen: Vec<u8> = m.to_tree.iter().flatten().copied().collect();
                        seen.sort_unstable();
                        let mut want: Vec<u8> = (0..n).filter(|t| !expect_ph.contains(t)).collect();
                        want.sort_unstable();
                        assert_eq!(seen, want);
                        for op in 0..n {
                            assert_eq!(m.to_tree[op as usize].is_some(), dealt.contains(&op));
                        }
                        verify_seat_map_by_replay(n, b, &dealt);
                        cases += 1;
                    }
                }
            }
            assert!(cases > 0);
        }
    }

    /// 9 座 HU（k=2，OpenPoker 约定 button 发 BB）：button → 树座 2、对手 → 树座 1，幻影 =
    /// UTG..CO + BTN = [3,4,5,6,7,8,0]（6 座同规则得 [3,4,5,0]）；幻影 fold 完轮到 SB（树座 1）。
    /// postflop 显式兜底 `short_hu_postflop`。
    #[test]
    fn seat_map_nine_max_hu_and_bad_input() {
        let m = seat_map(&n_req(9, 7, 2, vec![2, 7]), 9).unwrap();
        assert!(m.hu);
        assert_eq!(m.phantoms, vec![3, 4, 5, 6, 7, 8, 0]);
        assert_eq!(m.tree_seat(7), Ok(2));
        assert_eq!(m.tree_seat(2), Ok(1));
        assert!(m.tree_seat(0).is_err());
        let m6 = seat_map(&n_req(6, 4, 1, vec![1, 4]), 6).unwrap();
        assert_eq!(
            m6.phantoms,
            vec![3, 4, 5, 0],
            "6 座 HU 幻影与旧实现逐字相同"
        );
        let mut st = GameState::new(&TableConfig::n_max_at_bb(9, 100), 1);
        for &t in &m.phantoms {
            assert_eq!(st.current_player(), Some(SeatId(t)));
            st.apply(Action::Fold).unwrap();
        }
        assert_eq!(st.current_player(), Some(SeatId(1)), "幻影 fold 完轮到 SB");

        // 脏输入：显式 Err，绝不静默错映射。
        let bad = |r: Request| seat_map(&r, 9).err().expect("应 Err");
        assert_eq!(bad(n_req(9, 9, 0, vec![])), "bad_button_seat");
        assert_eq!(bad(n_req(9, 12, 0, vec![])), "bad_button_seat");
        assert_eq!(bad(n_req(9, 0, 0, vec![0, 9])), "bad_dealt_seats");
        assert_eq!(bad(n_req(9, 0, 0, vec![0])), "bad_dealt_seats");
        assert_eq!(bad(n_req(9, 0, 0, vec![3, 0])), "bad_dealt_seats");
        assert_eq!(
            bad(n_req(9, 0, 4, vec![0, 1, 2])),
            "dealt_missing_btn_or_me"
        );
        assert_eq!(
            bad(n_req(9, 5, 1, vec![0, 1, 2])),
            "dealt_missing_btn_or_me"
        );
        // 6 座 solver 上的 9 座号（≥6）同样 Err（driver 侧本就不该发）。
        assert_eq!(
            seat_map(&n_req(6, 0, 0, vec![0, 1, 7]), 6).err().as_deref(),
            Some("bad_dealt_seats")
        );
    }

    /// `--seats`：默认 6；[3,9] 外拒绝；与其余参数正交。
    #[test]
    fn parse_args_seats() {
        let parse = |extra: &[&str]| {
            parse_args_from(
                ["--checkpoint", "c.ckpt", "--bucket-table", "b.bin"]
                    .iter()
                    .chain(extra)
                    .map(|s| s.to_string()),
            )
        };
        assert_eq!(parse(&[]).unwrap().seats, DEFAULT_SEATS);
        assert_eq!(DEFAULT_SEATS, 6);
        assert_eq!(parse(&["--seats", "9"]).unwrap().seats, 9);
        assert_eq!(parse(&["--seats", "3"]).unwrap().seats, 3);
        for bad in ["2", "10", "0", "x"] {
            assert!(parse(&["--seats", bad]).is_err(), "--seats {bad} 应拒绝");
        }
        // `build_game(…, 6)` 与旧 `default_6max_100bb()` 逐字段相同（6 座 byte-equal 的根）。
        let a = TableConfig::n_max_at_bb(DEFAULT_SEATS, SOLVER_STACK_BB);
        let b = TableConfig::default_6max_100bb();
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }

    /// 桌面座位数 ≠ solver 座位数 → 显式兜底，标签带 solver 座位数（6 座仍是旧 `not_6max`）；
    /// 预热同口径 skip。
    #[test]
    fn table_seat_count_mismatch_falls_back() {
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let g6 = preopen_game();
        let abs6 = g6.abstraction().clone();
        let r = decide(
            &g6,
            &abs6,
            &uniform,
            &n_req(9, 0, 3, vec![]),
            0,
            None,
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(r.source, "fallback:not_6max", "6 座 blueprint 上 9 座桌");
        let g9 = nine_max_test_game();
        let abs9 = g9.abstraction().clone();
        for n in [6u8, 8] {
            let r = decide(
                &g9,
                &abs9,
                &uniform,
                &n_req(n, 0, 3, vec![]),
                0,
                None,
                &mut SubgameSolveCache::new(),
            );
            assert_eq!(r.source, "fallback:not_9max", "9 座 blueprint 上 {n} 座桌");
            assert!(is_legal(&r, &full_valid()));
        }
        let rt9 = rt(SubgameSearchConfig {
            trigger: SearchTrigger::AllPostflop,
            ..SubgameSearchConfig::default()
        });
        let mut flop = n_req(6, 0, 1, vec![]);
        flop.board = vec!["7h".into(), "2c".into(), "Ks".into()];
        let p = prewarm(
            &g9,
            &uniform,
            &flop,
            0,
            Some(&rt9),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(p.source, "prewarm:skip:not_9max");
        // 9 座 game 上 9 座桌 UTG（树座 3 = op (0+3)%9）首动作：blueprint 路径正常。
        let r = decide(
            &g9,
            &abs9,
            &uniform,
            &n_req(9, 0, 3, vec![]),
            0,
            None,
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(r.source, "blueprint", "{r:?}");
    }

    /// 计数版子树 DFS（与 `PublicBettingTree::walk` 同一展开：`abs.abstract_actions` → `apply` →
    /// 非终局递归；postflop + 规则全关 / all-1pot 规则下 `filter_actions` 是恒等），内存 O(深度)。
    /// 节点数到 `cap` 即停（返回 `None` = 超过 cap）——避免为量规模而真建一棵几 GB 的树。
    fn count_subtree_nodes(
        root: &GameState,
        abs: &StreetActionAbstraction,
        cap: u64,
    ) -> Option<u64> {
        use poker::ActionAbstraction;
        fn go(st: &GameState, abs: &StreetActionAbstraction, n: &mut u64, cap: u64) -> bool {
            *n += 1;
            if *n > cap {
                return false;
            }
            for a in abs.abstract_actions(st).iter() {
                let mut nx = st.clone();
                nx.apply(a.to_concrete()).expect("抽象动作合法");
                if !nx.is_terminal() && !go(&nx, abs, n, cap) {
                    return false;
                }
            }
            true
        }
        let mut n = 0;
        go(root, abs, &mut n, cap).then_some(n)
    }

    /// k 人见 flop 的 flop 首点（N=9 座 100BB）：按 preflop 行动序前 9−k 个非盲位先 fold（UTG 起），
    /// 余下全 call（`open_to = Some(x)` = 首个留下的人先加注到 x、其余 call；`None` = limp 池），
    /// BB check / call → flop。k 人池与座位数无关（弃牌座对子树零节点），故 6-max 已实测的
    /// k≤6 规模在 9-max 上原样成立，9-max 新增的只有 k=7..9。
    fn k_way_flop_root(k: usize, open_to: Option<u64>) -> GameState {
        let mut st = GameState::new(&TableConfig::n_max_at_bb(9, 100), 1);
        let mut folds = 9 - k;
        let mut opened = false;
        while st.street() == poker::Street::Preflop {
            let seat = st.current_player().unwrap().0;
            let a = if folds > 0 && seat != 1 && seat != 2 {
                folds -= 1;
                Action::Fold
            } else if let (Some(to), false) = (open_to, opened) {
                opened = true;
                Action::Raise {
                    to: ChipAmount::new(to),
                }
            } else if st.legal_actions().check {
                Action::Check
            } else {
                Action::Call
            };
            st.apply(a).unwrap();
        }
        assert_eq!(
            st.players()
                .iter()
                .filter(|p| p.status == poker::PlayerStatus::Active)
                .count(),
            k
        );
        st
    }

    /// 计数 DFS 与真建树（`build_subtree`）逐数一致（小 k），证诊断口径 = 生产建树口径。
    #[test]
    fn subtree_count_matches_build_subtree() {
        use poker::training::nlhe_betting_tree::{deep_single_pot, PublicBettingTree};
        let (abs, rules) = deep_single_pot();
        for (k, open) in [(2, None), (3, None), (2, Some(350)), (3, Some(350))] {
            let root = k_way_flop_root(k, open);
            let built = PublicBettingTree::build_subtree(&root, &abs, rules, 0, 0).num_nodes();
            assert_eq!(
                count_subtree_nodes(&root, &abs, u64::MAX),
                Some(built as u64),
                "k={k} open={open:?}"
            );
        }
    }

    /// **9-max 实时搜索子树规模诊断**（`#[ignore]`，release 跑：`cargo test --release --bin
    /// openpoker_advisor nine_max_search_subtree_sizing -- --ignored --nocapture`）。
    ///
    /// 口径：flop 首点为根（RoundStart 子树 = 解到河牌终局），菜单 = 9-max 目标 blueprint
    /// `all-1pot` 的 postflop {1pot}（= 不开 `--search-deep-menu` 时子树菜单；开 deep_menu 时 ≥5
    /// Active 也是 {1pot}——[`DEEP_MENU_WIDE_MAX_ACTIVE`]=4 按**人数**闸，与桌面座位数无关）。
    /// 100BB 对称栈，limp 池（pot = k BB）与单加注池（3.5BB open，k 人 call）。计数 cap 2 亿节点。
    ///
    /// [`DEEP_MENU_WIDE_MAX_ACTIVE`]: poker::training::nlhe_betting_tree::DEEP_MENU_WIDE_MAX_ACTIVE
    #[test]
    #[ignore = "诊断：release 跑，打印 k-way flop 子树节点数"]
    fn nine_max_search_subtree_sizing() {
        use poker::training::nlhe_betting_tree::deep_menu_for;
        let (abs, _) = reshape_profile("all-1pot", 4).unwrap();
        const CAP: u64 = 200_000_000;
        eprintln!("k | pot | menu(deep_menu_for) | {{1pot}} nodes | wall_ms");
        for open in [None, Some(350u64)] {
            for k in 2..=9 {
                let root = k_way_flop_root(k, open);
                let wide = !matches!(
                    deep_menu_for(&root)
                        .0
                        .config_for(poker::Street::Flop)
                        .raise_pot_ratios
                        .len(),
                    1
                );
                let t0 = Instant::now();
                let n = count_subtree_nodes(&root, &abs, CAP);
                eprintln!(
                    "{k} | {} | {} | {} | {}",
                    if open.is_some() { "SRP 3.5BB" } else { "limp" },
                    if wide { "wide{0.5,1}" } else { "{1pot}" },
                    n.map_or(format!(">{CAP}"), |n| n.to_string()),
                    t0.elapsed().as_millis()
                );
            }
        }
    }

    /// 9-max 端到端测试 game：advisor 生产构造 [`build_game`]（`n_max_at_bb(9, 100)` +
    /// `--reshape all-1pot --postflop-cap 2`）+ stub 200 桶。cap 2 → 见 flop ≤2 人，dense 两表
    /// ≈ 209 MiB（实测 34,895 节点 / 6.4M 行 / 13.7M slot；cap 3 要 2.3 GiB，本机跑不动）。
    fn nine_max_test_game() -> SimplifiedNlheGame {
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::new(200, 200, 200).expect("200/200/200"),
        ));
        build_game(table, "all-1pot", 2, 9).expect("9-max game")
    }

    /// 与 [`nine_max_test_game`] 同 reshape / cap / 桶表、只换座位数的 game（错配测试用）。
    fn n_seat_test_game(seats: u8) -> SimplifiedNlheGame {
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::new(200, 200, 200).expect("200/200/200"),
        ));
        build_game(table, "all-1pot", 2, seats).expect("game")
    }

    /// 9-max fixture：训练 → 完整 ckpt + crop(keep_through=Flop, 不留 regret)。文件写到
    /// `target/tmp`（磁盘；`/tmp` 是 tmpfs，200+ MiB 的 ckpt 会直接吃内存）。trainer 存盘后即释放。
    /// `tag` 区分并行跑的测试（目录只按进程号区分时，先结束的测试会删掉别的测试正在读的文件）。
    fn nine_max_fixture(tag: &str) -> (TempDir, PathBuf, PathBuf) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/tmp")
            .join(format!("advisor_nine_max_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let guard = TempDir(dir.clone());
        let full = dir.join("full.ckpt");
        let crop = dir.join("pf.crop");
        {
            let mut t = DenseNlheEsMccfrTrainer::new(nine_max_test_game(), 7).with_lcfr_period(500);
            let mut rng = ChaCha20Rng::from_seed(0x9_0001);
            for _ in 0..3_000 {
                t.step(&mut rng).expect("step");
            }
            t.save_checkpoint(&full).expect("save");
            crop_dense_checkpoint(
                &full,
                &crop,
                t.game(),
                CropOptions {
                    keep_through: StreetTag::Flop,
                    keep_regret: false,
                },
            )
            .expect("crop");
        }
        (guard, full, crop)
    }

    /// 9 座桌 button=4 的 SB-vs-BB 线：UTG(op7)…BTN(op4) 7 家 fold，SB(op5) complete、BB(op6)
    /// check。我 = SB(op5)，postflop 每街首个行动者。`extra` = flop 起后续动作。
    fn nine_sb_line_req(
        hole: [&str; 2],
        board: &[&str],
        extra: &[HistAction],
        valid: ValidActions,
    ) -> Request {
        let mut actions: Vec<HistAction> = [7u8, 8, 0, 1, 2, 3, 4]
            .iter()
            .map(|&s| ha(s, "fold", None))
            .collect();
        actions.push(ha(5, "call", Some(20)));
        actions.push(ha(6, "check", None));
        actions.extend_from_slice(extra);
        Request {
            hole: hole.iter().map(|c| c.to_string()).collect(),
            board: board.iter().map(|c| c.to_string()).collect(),
            button_seat: 4,
            my_seat: 5,
            num_seats: 9,
            small_blind: 10,
            big_blind: 20,
            actions,
            valid,
            stacks: vec![],
            dealt_seats: vec![],
            names: Default::default(),
        }
    }

    /// 同手 flop 首点 / turn 首点 / river 首点 / river 面对 1pot 下注。
    fn nine_sb_line_hand(hole: [&str; 2]) -> Vec<Request> {
        let flop = ["7h", "2c", "Ks"];
        let turn = ["7h", "2c", "Ks", "4d"];
        let river = ["7h", "2c", "Ks", "4d", "9s"];
        let x = [ha(5, "check", None), ha(6, "check", None)];
        let turn_x = [x.as_slice(), &x].concat();
        let river_bet = [
            turn_x.as_slice(),
            &[ha(5, "check", None), ha(6, "raise", Some(40))],
        ]
        .concat();
        vec![
            nine_sb_line_req(hole, &flop, &[], check_valid()),
            nine_sb_line_req(hole, &turn, &x, check_valid()),
            nine_sb_line_req(hole, &river, &turn_x, check_valid()),
            nine_sb_line_req(
                hole,
                &river,
                &river_bet,
                ValidActions {
                    can_check: false,
                    can_call: true,
                    can_raise: true,
                    min_raise: Some(80),
                    max_raise: Some(1980),
                },
            ),
        ]
    }

    /// **9-max 端到端**（9 座训练的小 blueprint + 9 座桌请求）：
    /// ① `--seats` 与 blueprint 座位数不符 → 完整 ckpt / crop 加载都报错（layout 指纹拒绝；6 / 8 座
    ///    两档，8 座树只多一个座位也拒）；同座位数加载成功、`ready.n_seats` 来源 = 9。
    /// ② 翻前 blueprint：满桌（button=4 旋转）UTG 首动作 + 6 人短桌（幻影 fold）均走 blueprint、合法；
    ///    短桌 k 人局与「满桌前 9−k 位 fold」的等价节点 info_set / 分布逐位相同；crop == 完整。
    /// ③ 翻后搜索：flop → turn → river → river 面对下注全走实时搜索（crop 与完整逐字节相同，
    ///    转牌 / 河牌零被裁街查询）；flop 预热命中；off-stack 真栈（`stacks` 长 9）走脱锚搜索。
    #[test]
    fn nine_max_end_to_end_blueprint_and_search() {
        let (_dir, full_p, crop_p) = nine_max_fixture("e2e");

        // ① 座位数错配必拒（完整 + crop）。
        for wrong in [6u8, 8] {
            let e = load_blueprint(&full_p, n_seat_test_game(wrong), false)
                .err()
                .unwrap_or_else(|| panic!("{wrong} 座 game 加载 9 座 ckpt 须失败"));
            assert!(
                e.contains("fingerprint") || e.contains("Fingerprint"),
                "{e}"
            );
            assert!(
                load_blueprint(&crop_p, n_seat_test_game(wrong), true).is_err(),
                "{wrong} 座 game 加载 9 座 crop 须失败"
            );
        }
        let full = load_blueprint(&full_p, nine_max_test_game(), false).expect("load full");
        let crop = load_blueprint(&crop_p, nine_max_test_game(), true).expect("load crop");
        let game = full.game();
        assert_eq!(game.table_config().n_seats, 9);
        let abs = game.abstraction().clone();
        let sf = |i: &InfoSetId, _n: usize| full.strategy(i);
        let sc = |i: &InfoSetId, _n: usize| crop.strategy(i);
        // 短桌映射检查要比较 info_set（占位兜底不带 info_set）：这里只验座位映射，空 / 占位行补成
        // 均匀，让决策照常走 blueprint 并带出 info_set。
        let sf_raw = |i: &InfoSetId, n: usize| {
            let v = full.strategy_raw(i);
            if v.is_empty() {
                vec![1.0 / n as f64; n]
            } else {
                v
            }
        };
        let (rt_f, rt_c) = (crop_rt(&full, crop_scfg()), crop_rt(&crop, crop_scfg()));
        validate_crop_runtime(crop.keep_through(), Some(&rt_c)).expect("生产约束满足");

        // ② 翻前。满桌 button=4：UTG = op (4+3)%9 = 7。
        for hole in [["Ah", "Kd"], ["7c", "2d"], ["Qs", "Qh"]] {
            let mut req = n_req(9, 4, 7, vec![]);
            req.hole = hole.iter().map(|c| c.to_string()).collect();
            for search in [false, true] {
                let rf = decide(
                    game,
                    &abs,
                    &sf,
                    &req,
                    0x9EED,
                    search.then_some(&rt_f),
                    &mut SubgameSolveCache::new(),
                );
                let rc = decide(
                    game,
                    &abs,
                    &sc,
                    &req,
                    0x9EED,
                    search.then_some(&rt_c),
                    &mut SubgameSolveCache::new(),
                );
                assert!(bp_or_placeholder(&rf.source), "{rf:?}");
                assert!(is_legal(&rf, &req.valid));
                assert_eq!(rf, rc, "翻前 crop 与完整须全等");
            }
            // 6 人短桌 dealt={0,2,4,5,6,8}、button=4：ring 4,5,6,8,0,2 → UTG = op8（树座 3+3=6）。
            // 等价满桌节点：树座 3/4/5 fold 后轮到树座 6 = op (4+6)%9 = 1。
            let mut short = n_req(9, 4, 8, vec![0, 2, 4, 5, 6, 8]);
            short.hole = req.hole.clone();
            let mut equiv = n_req(9, 4, 1, vec![]);
            equiv.hole = req.hole.clone();
            equiv.actions = vec![
                ha(7, "fold", None),
                ha(8, "fold", None),
                ha(0, "fold", None),
            ];
            let rs = decide(
                game,
                &abs,
                &sf_raw,
                &short,
                0x9EED,
                None,
                &mut SubgameSolveCache::new(),
            );
            let re = decide(
                game,
                &abs,
                &sf_raw,
                &equiv,
                0x9EED,
                None,
                &mut SubgameSolveCache::new(),
            );
            assert_eq!(rs.source, "blueprint", "{rs:?}");
            assert!(is_legal(&rs, &short.valid));
            assert_eq!(
                rs.info_set, re.info_set,
                "短桌 = 满桌前 3 位 fold 的同一节点"
            );
            assert_eq!(rs.probs, re.probs);
        }

        // ③ 翻后：整手（共享缓存 = 生产顺序），crop 与完整逐字节相同、全走搜索。
        for hole in [["Ah", "Kd"], ["Qs", "Qh"]] {
            let hand = nine_sb_line_hand(hole);
            let (mut cf, mut cc) = (SubgameSolveCache::new(), SubgameSolveCache::new());
            for (k, req) in hand.iter().enumerate() {
                let rf = decide(game, &abs, &sf, req, 0x9EEF, Some(&rt_f), &mut cf);
                let rc = decide(game, &abs, &sc, req, 0x9EEF, Some(&rt_c), &mut cc);
                assert_eq!(rc.source, "search", "决策 {k}（{hole:?}）：{rc:?}");
                assert!(is_legal(&rc, &req.valid), "决策 {k}：{rc:?}");
                assert_eq!(rf, rc, "决策 {k}（{hole:?}）crop 与完整须逐字节相同");
            }
        }
        // 预热 → 决策命中。
        let flop_req = &nine_sb_line_hand(["Ah", "Kd"])[0];
        let mut c = SubgameSolveCache::new();
        let p = prewarm(game, &sc, flop_req, 0x9EEF, Some(&rt_c), &mut c);
        assert_eq!(p.source, "prewarm:stored", "{p:?}");
        let r = decide(game, &abs, &sc, flop_req, 0x9EEF, Some(&rt_c), &mut c);
        assert_eq!(r.source, "search");
        assert_eq!(c.hits(), 1, "预热后决策须命中");
        // off-stack：hero(SB, op5) 只有 15BB（≠ blueprint 100BB 树）→ 脱锚 / 锚定搜索之一，合法。
        let mut off = flop_req.clone();
        off.stacks = vec![2000; 9];
        off.stacks[5] = 300;
        let r = decide(
            game,
            &abs,
            &sc,
            &off,
            0x9EEF,
            Some(&rt_c),
            &mut SubgameSolveCache::new(),
        );
        assert!(r.source.starts_with("search"), "{r:?}");
        assert!(!r.source.starts_with("search_giveup"), "{r:?}");
        assert!(is_legal(&r, &off.valid));
        // stacks 长度 ≠ 9 → 真栈建不了 → 显式 giveup（不静默按错座位读栈）。
        let mut bad = flop_req.clone();
        bad.stacks = vec![2000; 6];
        let r = decide(
            game,
            &abs,
            &sc,
            &bad,
            0x9EEF,
            Some(&rt_c),
            &mut SubgameSolveCache::new(),
        );
        assert!(r.source.contains("stacks_len"), "{r:?}");
        assert_eq!(crop.cropped_queries(), 0, "全程不应查询被裁街");
    }

    /// **9-max 生产组合**：crop(preflop+flop) + `--search-trigger all-postflop` +
    /// `--search-flop-prefer-blueprint on` + `--search-deep-menu` 同开：
    /// 翻前 blueprint；flop 锚定面 blueprint（与 search=None 逐字节相同）；turn / river 全走搜索
    /// 不降级（river 先验来自 turn 子树后验）；全程零被裁街查询。另验真码深闸
    /// （[`flop_blueprint_stack_gate`]）：有效码深 60–140BB 内（含端点）走 blueprint、预热 skip；区间外
    /// 或有人 all-in（含 100BB 影子仍同步的短码 all-in）→ 实时搜索、预热照做。
    #[test]
    fn nine_max_crop_flop_prefer_blueprint_deep_menu() {
        let (_dir, _full_p, crop_p) = nine_max_fixture("prefer");
        let crop = load_blueprint(&crop_p, nine_max_test_game(), true).expect("load crop");
        let game = crop.game();
        let abs = game.abstraction().clone();
        let sc = |i: &InfoSetId, _n: usize| crop.strategy(i);
        let rt = SearchRuntime {
            flop_prefer_blueprint: true,
            ..crop_rt(
                &crop,
                SubgameSearchConfig {
                    deep_menu: true,
                    ..crop_scfg()
                },
            )
        };
        validate_crop_runtime(crop.keep_through(), Some(&rt)).expect("生产约束满足");

        let mut pre = n_req(9, 4, 7, vec![]);
        pre.hole = vec!["Ah".into(), "Kd".into()];
        let r = decide(
            game,
            &abs,
            &sc,
            &pre,
            0x9EED,
            Some(&rt),
            &mut SubgameSolveCache::new(),
        );
        assert!(bp_or_placeholder(&r.source), "{r:?}");

        for hole in [["Ah", "Kd"], ["Qs", "Qh"]] {
            let hand = nine_sb_line_hand(hole);
            let mut cache = SubgameSolveCache::new();
            for (k, req) in hand.iter().enumerate() {
                let r = decide(game, &abs, &sc, req, 0x9EEF, Some(&rt), &mut cache);
                assert!(is_legal(&r, &req.valid), "决策 {k}：{r:?}");
                if req.board.len() == 3 {
                    let off = decide(
                        game,
                        &abs,
                        &sc,
                        req,
                        0x9EEF,
                        None,
                        &mut SubgameSolveCache::new(),
                    );
                    assert_eq!(r.source, "blueprint", "flop 锚定面须走 blueprint：{r:?}");
                    assert_eq!(r, off, "flop 锚定面须与 search=None 逐字节相同");
                } else {
                    assert!(
                        r.source.starts_with("search") && !r.source.starts_with("search_giveup"),
                        "决策 {k}（{hole:?}）turn/river 须实时搜索：{r:?}"
                    );
                }
            }
        }

        // 真码深闸（flop_blueprint_stack_gate）：hero = SB(op5)，对手 = BB(op6)，其余翻前已弃。
        let flop_req = |hero_bb: u64, opp_bb: u64| {
            let mut r = nine_sb_line_hand(["Ah", "Kd"])[0].clone();
            r.stacks = vec![2000; 9];
            r.stacks[5] = hero_bb * 20;
            r.stacks[6] = opp_bb * 20;
            r.stacks[7] = 200; // 已弃牌座的短码不参与判断。
            r
        };
        let run = |req: &Request, rt: Option<&SearchRuntime>| {
            decide(
                game,
                &abs,
                &sc,
                req,
                0x9EEF,
                rt,
                &mut SubgameSolveCache::new(),
            )
        };
        // 区间内（含端点 60 / 140）→ blueprint，且与 search=None 逐字节相同。
        for (hero_bb, opp_bb) in [(100, 65), (100, 60), (150, 140), (100, 300)] {
            let req = flop_req(hero_bb, opp_bb);
            let r = run(&req, Some(&rt));
            assert_eq!(
                r.source, "blueprint",
                "hero {hero_bb}BB vs {opp_bb}BB：{r:?}"
            );
            assert_eq!(r, run(&req, None));
            let p = prewarm(
                game,
                &sc,
                &req,
                0x9EEF,
                Some(&rt),
                &mut SubgameSolveCache::new(),
            );
            assert_eq!(p.source, "prewarm:skip:flop_prefer_blueprint", "{p:?}");
        }
        // 区间外 → 实时搜索；预热也照常做。
        for (hero_bb, opp_bb) in [(100, 59), (100, 15), (150, 150), (145, 300)] {
            let req = flop_req(hero_bb, opp_bb);
            let r = run(&req, Some(&rt));
            assert_eq!(r.source, "search", "hero {hero_bb}BB vs {opp_bb}BB：{r:?}");
            assert!(is_legal(&r, &req.valid), "{r:?}");
            let p = prewarm(
                game,
                &sc,
                &req,
                0x9EEF,
                Some(&rt),
                &mut SubgameSolveCache::new(),
            );
            assert_eq!(p.source, "prewarm:stored", "{p:?}");
        }
        // 有人 all-in：BB 只有 15BB，hero check 后它 flop all-in。100BB 影子把它当 100BB all-in、
        // 照样同步（锚定），闸须拦下 → 实时搜索（按真实底池赔率），不再用 blueprint。
        let mut shove = nine_sb_line_req(
            ["Ah", "Kd"],
            &["7h", "2c", "Ks"],
            &[ha(5, "check", None), ha(6, "all_in", Some(300))],
            ValidActions {
                can_check: false,
                can_call: true,
                can_raise: false,
                min_raise: None,
                max_raise: None,
            },
        );
        shove.stacks = vec![2000; 9];
        shove.stacks[6] = 300;
        let r = run(&shove, Some(&rt));
        assert_eq!(r.source, "search", "{r:?}");
        assert!(is_legal(&r, &shove.valid), "{r:?}");
        // 同一手对手 100BB all-in：有效码深在区间内，但有人 all-in → 仍须搜索。
        let mut shove100 = shove.clone();
        shove100.stacks[6] = 2000;
        shove100.actions.last_mut().expect("all_in").to = Some(2000);
        let r = run(&shove100, Some(&rt));
        assert_eq!(r.source, "search", "{r:?}");

        assert_eq!(crop.cropped_queries(), 0, "全程不应查询被裁街");
    }

    /// 9 座 7 人 limp 池（no-limp blueprint 上必失同步 → 脱锚搜索）：
    /// - 100BB 真栈：7-way limp flop {1pot} 子树实测 3.07M 节点 > cap 200k → **建树前**计数即拒，
    ///   `search_giveup:unanchored:subtree_precount:7way>200000`（check-when-free），预热同口径 err；
    /// - 10BB 短码：同一线子树很小 → 闸放行、正常脱锚搜索。
    #[test]
    fn nine_max_multiway_precount_gate() {
        let game = nine_max_test_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let mut req = n_req(9, 0, 1, vec![]);
        req.board = vec!["7h".into(), "2c".into(), "Ks".into()];
        req.actions = vec![ha(3, "fold", None), ha(4, "fold", None)];
        for s in [5u8, 6, 7, 8, 0, 1] {
            req.actions.push(ha(s, "call", Some(20)));
        }
        req.actions.push(ha(2, "check", None));
        req.valid = check_valid();
        let scfg = SubgameSearchConfig {
            iterations: 200,
            trigger: SearchTrigger::AllPostflop,
            max_subtree_nodes: 200_000,
            ..SubgameSearchConfig::default()
        };
        let rt9 = rt(scfg);
        let r = decide(
            &game,
            &abs,
            &uniform,
            &req,
            1,
            Some(&rt9),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(
            r.source, "search_giveup:unanchored:subtree_precount:7way>200000",
            "{r:?}"
        );
        assert_eq!(r.action, "check");
        // 同一缓存：同街再次决策 → 直接复用拒绝结果（不再走 lookup / 计数，misses 不变）。
        let mut shared = SubgameSolveCache::new();
        let r1 = decide(&game, &abs, &uniform, &req, 1, Some(&rt9), &mut shared);
        assert_eq!(r1.source, r.source);
        let misses = shared.misses();
        assert_eq!(misses, 1, "首次被拒：一次 miss（计数后拒绝）");
        let r2 = decide(&game, &abs, &uniform, &req, 1, Some(&rt9), &mut shared);
        assert_eq!(r2.source, r.source, "{r2:?}");
        assert_eq!(shared.misses(), misses, "第二次直接复用拒绝原因，不再计数");
        let p = prewarm(
            &game,
            &uniform,
            &req,
            1,
            Some(&rt9),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(
            p.source, "prewarm:err:subtree_precount:7way>200000",
            "{p:?}"
        );

        let mut short = req.clone();
        short.stacks = vec![200; 9];
        let r = decide(
            &game,
            &abs,
            &uniform,
            &short,
            1,
            Some(&rt9),
            &mut SubgameSolveCache::new(),
        );
        assert_eq!(r.source, "search:unanchored", "{r:?}");
        assert!(is_legal(&r, &short.valid));
    }
}
