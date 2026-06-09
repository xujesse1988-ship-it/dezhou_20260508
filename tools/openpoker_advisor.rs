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
//! [`subgame_search`] 解到终局（`time_budget` 墙钟 anytime / 可选 LCFR）；outgoing 按**真码深**
//! `auth` 算尺寸（非「100BB 解 ÷scale」）。**搜索区解不出来 = 直接 fold**（建不了真栈树 / 子博弈
//! `Err`），**不回落 blueprint**（off-distribution 下 blueprint 解的是错游戏，§2.3）。
//!
//! **守恒不变量**：`--search` **未开**（`search=None`）时 `decide` 走原 100BB blueprint 路径、
//! 逐字节等价旧行为（测试 `search_off_byte_equal_blueprint` 钉死）。preflop + 未触发的 postflop
//! 决策即便开了 `--search` 也走 blueprint 路径（与未开等价）。
//!
//! **当前边界（v1）**：①取 `node_id` / `legal_abs` 仍靠 100BB 影子重放——**off-stack all-in 线**
//! 影子与真栈失同步时拿不到 node_id → 走 100BB fallback / fold（深码无 all-in 的 on-tree-preflop
//! 线是 v1 可搜的主场景）。②子树用 blueprint 的下注菜单（非深码 {1pot}）；深码窄菜单 = 缺口③。
//!
//! # 已知限制（blueprint 路径，`...client_design...` §4）
//!
//! - 码深 ≠ 100BB 且**未开搜索 / 未触发**：solver 树/SPR 都按 100BB 解；real `GameState` 用
//!   `default_6max_100bb`（10000 筹码）近似，driver 靠买入锁 2000 + 栈漂出 [80,125]BB leave/rejoin 兜。
//! - 非 6 人桌 / 对手 open-limp：no-limp blueprint 无对应节点 → 走兜底（见上）。

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use poker::training::blueprint_advisor::{advance_shadow_by_applied, outgoing_action, parse_card};
use poker::training::game::Game;
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::nlhe_betting_tree::{
    first_small_6max, first_small_preopen_6max, first_small_preopen_small_6max,
};
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::sampling::sample_discrete;
use poker::training::subgame::{
    should_search, subgame_search, ResolveRoot, SearchTrigger, SubgameSearchConfig,
};
use poker::{
    AbstractAction, Action, BucketTable, Card, ChaCha20Rng, ChipAmount, GameState, InfoSetId,
    SeatId, StreetActionAbstraction, TableConfig,
};

const N_SEATS: usize = 6;
const REAL_REPLAY_SEED: u64 = 0x5245_414c_3645_4d58; // "REAL6EMX"
const ABS_REPLAY_SEED: u64 = 0x4142_5336_454d_5800; // "ABS6EMX\0"

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

#[derive(Deserialize, Debug)]
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
    /// 字段）→ 退对称 100BB（blueprint 路径不读它，byte-equal 不受影响）。
    #[serde(default)]
    stacks: Vec<u64>,
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

fn expected_board_len(s: poker::Street) -> usize {
    match s {
        poker::Street::Preflop => 0,
        poker::Street::Flop => 3,
        poker::Street::Turn => 4,
        poker::Street::River | poker::Street::Showdown => 5,
    }
}

/// 把一条历史动作（已 rotate 到 solver 座、`to` 已 ×scale）译成 stage-1 [`Action`]。
/// raise/bet 按 real 当前 legal（LA-002：无前序 bet → Bet，否则 Raise）选种类。
fn hist_to_concrete(
    real: &poker::GameState,
    action: &str,
    to_solver: Option<u64>,
) -> Option<Action> {
    match action {
        "fold" => Some(Action::Fold),
        "check" => Some(Action::Check),
        "call" => Some(Action::Call),
        "all_in" | "allin" => Some(Action::AllIn),
        "raise" | "bet" => {
            let to = ChipAmount::new(to_solver?);
            if real.legal_actions().bet_range.is_some() {
                Some(Action::Bet { to })
            } else {
                Some(Action::Raise { to })
            }
        }
        _ => None,
    }
}

/// 一次决策：重放本手历史（100BB real + abs 两态 lockstep）→ 我方决策点。`search == None` 时
/// 查 blueprint → outgoing（旧行为，byte-equal）；`search == Some` 且命中触发面（[`should_search`]）
/// 时建**真码深** subgame re-solve（[`subgame_search`]）→ outgoing 按真栈算尺寸，解不出来直接
/// fold（不回落 blueprint，§2.3）。任何**前置 / blueprint 路径**失败返回安全兜底（不 panic）。
fn decide(
    game: &SimplifiedNlheGame,
    abstraction: &StreetActionAbstraction,
    strategy_fn: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    req: &Request,
    base_seed: u64,
    search: Option<&SubgameSearchConfig>,
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

    // —— rotate 到 solver 座（OpenPoker button → solver 座 0，对齐 default_6max_100bb）——
    let bsolver =
        |op_seat: u8| -> u8 { (op_seat + N_SEATS as u8 - req.button_seat) % N_SEATS as u8 };
    let my_seat_solver = SeatId(bsolver(req.my_seat));

    // —— 两态 lockstep 重放 ——
    let mut real = poker::GameState::new(&solver_cfg, REAL_REPLAY_SEED);
    let mut abs_rng = ChaCha20Rng::from_seed(ABS_REPLAY_SEED);
    let mut abs: SimplifiedNlheState = game.root(&mut abs_rng);

    for h in &req.actions {
        let actor = bsolver(h.seat);
        if real.current_player() != Some(SeatId(actor)) {
            // 码深漂移 / 历史错位 → 重放对不上回合 → 兜底。
            return safe_fallback(&req.valid, "replay_seat_mismatch");
        }
        let to_solver = h.to.map(|t| t * scale);
        let Some(concrete) = hist_to_concrete(&real, &h.action, to_solver) else {
            return safe_fallback(&req.valid, "bad_hist_action");
        };
        if real.apply(concrete).is_err() {
            return safe_fallback(&req.valid, "replay_illegal");
        }
        let is_all_in = real.players()[actor as usize].status == poker::PlayerStatus::AllIn;
        if advance_shadow_by_applied(&mut abs, concrete, is_all_in, &mut abs_rng).is_err() {
            // 结构性 gap（如 open-limp 进 no-limp 影子）→ 兜底（不静默改 kind）。
            return safe_fallback(&req.valid, "structural_gap");
        }
        if abs.game_state.current_player() != real.current_player() {
            return safe_fallback(&req.valid, "lockstep_drift");
        }
    }

    // —— 到我方决策点 ——
    if real.current_player() != Some(my_seat_solver) {
        return safe_fallback(&req.valid, "not_my_turn");
    }
    let street = real.street();
    if abs.game_state.street() != street || board.len() != expected_board_len(street) {
        return safe_fallback(&req.valid, "street_board_mismatch");
    }
    let legal_abs = SimplifiedNlheGame::legal_actions(&abs);
    if legal_abs.is_empty() {
        return safe_fallback(&req.valid, "empty_legal");
    }
    let node_id = abs.current_node_id;
    let info = game.info_set_for_cards(node_id, hole, &board);

    // —— gating（设计 §1）：仅 `--search` 开 + 命中触发面才搜索；否则 blueprint。
    // should_search 只读街 + 本街是否已起注（与码深无关）→ 在 100BB `real` 上判等价真栈 auth。
    let want_search = matches!(search, Some(scfg) if should_search(&real, scfg.trigger));

    // dist + outgoing 基准态：默认 = blueprint 分布 + 100BB real 算尺寸（search=None / 未触发，
    // byte-equal 旧行为）；搜索触发 = 真码深 auth 子博弈解 + auth 算尺寸（失败 → fold，不回落）。
    let mut auth_holder: Option<GameState> = None;
    let dist: Vec<(AbstractAction, f64)> = if want_search {
        let scfg = search.expect("want_search ⇒ search.is_some()");
        // 真码深 auth + round_start（真栈重放 + 注入真实牌）。建不了 = §2.3「建不了树」→ fold。
        let (auth, round_start) =
            match build_real_auth(req, &solver_cfg, scale, my_seat_solver, hole, &board) {
                Ok(pair) => pair,
                Err(reason) => return fold_response(&format!("search_build:{reason}")),
            };
        let root_state: &GameState = match scfg.resolve_root {
            ResolveRoot::RoundStart => &round_start,
            ResolveRoot::CurrentDecision => &auth,
        };
        let hand_seed = hand_seed_for(req, base_seed);
        match subgame_search(
            &auth,
            root_state,
            game,
            &legal_abs,
            node_id,
            strategy_fn,
            scfg,
            None, // depth_limit=false 解到终局 → 无 leaf_values（§2.1）。
            hand_seed,
            req.actions.len() as u64,
        ) {
            Ok(d) => {
                auth_holder = Some(auth); // outgoing 用真栈 auth 算尺寸。
                d
            }
            // 解不出来（建不了/未访问/失同步/限时连一轮迭代都未完成）→ fold，不回落 blueprint。
            Err(reason) => return fold_response(&format!("search_unsolved:{reason}")),
        }
    } else {
        blueprint_distribution(&info, &legal_abs, strategy_fn)
    };

    // outgoing 基准态：搜索 → 真栈 auth（真码深尺寸）；blueprint → 100BB real（旧行为）。
    let outgoing_state: &GameState = auth_holder.as_ref().unwrap_or(&real);

    // per-decision 确定性采样（保混合策略 + 可复现；seed 与搜索与否无关 → search=None byte-equal）。
    let mut sample_rng = ChaCha20Rng::from_seed(sample_seed(req, base_seed));
    let chosen = sample_discrete(&dist, &mut sample_rng);

    // —— outgoing：solver Action → OpenPoker {action, amount}（blueprint / search 共享映射）——
    let solver_action = match outgoing_action(outgoing_state, abstraction, chosen) {
        Ok(a) => a,
        Err(_) => return safe_fallback(&req.valid, "outgoing_failed"),
    };
    let mut resp = action_to_response(solver_action, scale, &req.valid);
    if resp.source.is_empty() {
        // 合法动作：填 source（search / blueprint）+ 诊断。不合法时 action_to_response 已产
        // safe_fallback（source = fallback:...），不覆盖。
        resp.source = if want_search { "search" } else { "blueprint" }.into();
        resp.street = Some(street_label(street).into());
        resp.info_set = Some(info.raw());
        resp.chosen = Some(action_label(&chosen));
    }
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

/// 搜索区降级（设计 §2.3）：真解不出来（建不了真栈树 / 子博弈 `Err`）→ **直接 fold**，
/// **不回落 blueprint**（off-distribution 下 blueprint 解错游戏）。`source = search_fold:<reason>`
/// 与 blueprint 路径的 `fallback:...` 区分（driver 据此分两类统计，§4.1 fallback 护栏）。
fn fold_response(reason: &str) -> Response {
    Response {
        action: "fold".into(),
        source: format!("search_fold:{reason}"),
        ..Default::default()
    }
}

/// 缺口②：在**真码深** config 上重放本手 → 注入真实牌，产 `(auth, round_start)` 喂
/// [`subgame_search`]。`auth` = 当前决策点真栈态（query_at 索引 hero 真桶用）；`round_start` =
/// 当前街起点快照（[`ResolveRoot::RoundStart`] 子树根）。重放对不上 / 注入失败 → `Err`（caller fold）。
fn build_real_auth(
    req: &Request,
    solver_cfg: &TableConfig,
    scale: u64,
    my_seat_solver: SeatId,
    hole: [Card; 2],
    board: &[Card],
) -> Result<(GameState, GameState), String> {
    let real_cfg = real_stacks_config(req, solver_cfg, scale)?;
    let bsolver =
        |op_seat: u8| -> u8 { (op_seat + N_SEATS as u8 - req.button_seat) % N_SEATS as u8 };

    let mut auth = GameState::new(&real_cfg, REAL_REPLAY_SEED);
    // round_start 快照：街变即重 snapshot（postflop 街起点）；初始 = preflop 起点（不被搜索读）。
    let mut round_start = auth.clone();
    let mut rs_street = auth.street();
    for h in &req.actions {
        let actor = bsolver(h.seat);
        if auth.current_player() != Some(SeatId(actor)) {
            return Err("auth_seat_mismatch".into());
        }
        let to_solver = h.to.map(|t| t.checked_mul(scale).ok_or("to_overflow"));
        let to_solver = match to_solver {
            Some(Ok(v)) => Some(v),
            Some(Err(e)) => return Err(e.into()),
            None => None,
        };
        let Some(concrete) = hist_to_concrete(&auth, &h.action, to_solver) else {
            return Err("auth_bad_hist".into());
        };
        if auth.apply(concrete).is_err() {
            return Err("auth_replay_illegal".into());
        }
        if auth.street() != rs_street {
            round_start = auth.clone();
            rs_street = auth.street();
        }
    }
    if auth.current_player() != Some(my_seat_solver) {
        return Err("auth_not_my_turn".into());
    }
    // 注入真实牌（hero hole + board）到当前点 + 街起点（subgame solve / query_at 读真牌）。
    let auth = auth.inject_external_cards(my_seat_solver, hole, board)?;
    let round_start = round_start.inject_external_cards(my_seat_solver, hole, board)?;
    Ok((auth, round_start))
}

/// 真码深 [`TableConfig`]：各座起始栈 = OpenPoker hand-start 栈 × `scale`（座位按相对 button
/// rotate 到 solver 座）。盲注 / 座数 / button 沿用 `solver_cfg`（对齐 blueprint）。`stacks` 缺省
/// （旧 driver / 无 players 字段）→ 退 `solver_cfg` 对称 100BB。脏数据（长度 / 0 栈 / 溢出）→ `Err`。
fn real_stacks_config(
    req: &Request,
    solver_cfg: &TableConfig,
    scale: u64,
) -> Result<TableConfig, String> {
    let mut cfg = solver_cfg.clone();
    if req.stacks.is_empty() {
        return Ok(cfg); // 无真栈 → 对称 100BB（仍是合法的真栈解，只是不利用码深）。
    }
    if req.stacks.len() != N_SEATS {
        return Err("stacks_len".into());
    }
    let bsolver =
        |op_seat: usize| -> usize { (op_seat + N_SEATS - req.button_seat as usize) % N_SEATS };
    let mut stacks = vec![ChipAmount::ZERO; N_SEATS];
    for (op_seat, &op_stack) in req.stacks.iter().enumerate() {
        let s = op_stack.checked_mul(scale).ok_or("stack_overflow")?;
        if s == 0 {
            return Err("zero_stack".into()); // 空座 / 已破产 → 不解（边界，fold）。
        }
        stacks[bsolver(op_seat)] = ChipAmount::new(s);
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
    hasher.update(&base_seed.to_le_bytes());
    let d = hasher.finalize();
    u64::from_le_bytes(d.as_bytes()[..8].try_into().expect("blake3 ≥ 8 bytes"))
}

/// solver raise-to → OpenPoker raise-to：÷scale 四舍五入，夹进 [min_raise, max_raise]。
/// 无 raise 区间（不能加注）→ None（caller 兜底）。
fn raise_to_op(to_solver: u64, scale: u64, valid: &ValidActions) -> Option<u64> {
    if !valid.can_raise {
        return None;
    }
    let (min, max) = (valid.min_raise?, valid.max_raise?);
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

    let ready = ReadyLine {
        ready: true,
        update_count: trainer.update_count(),
        reshape: args.reshape.clone(),
        n_seats: N_SEATS,
        search: args.search.is_some(),
    };
    if let Some(scfg) = &args.search {
        eprintln!(
            "[openpoker_advisor] search ON: trigger={:?} iters={} time_budget={:?} lcfr={} max_nodes={}",
            scfg.trigger, scfg.iterations, scfg.time_budget, scfg.lcfr, scfg.max_subtree_nodes
        );
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

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => decide(
                game,
                &abstraction,
                &strategy_fn,
                &req,
                args.seed,
                args.search.as_ref(),
            ),
            // 解析失败也不崩：出 fold（最保守；没有 valid 信息可用）。
            Err(e) => Response {
                action: "fold".into(),
                source: format!("fallback:bad_request_json:{e}"),
                ..Default::default()
            },
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
    let mut checkpoint: Option<PathBuf> = None;
    let mut bucket_table: Option<PathBuf> = None;
    let mut reshape = "preopen".to_string();
    let mut postflop_cap = 3u8;
    let mut seed: u64 = 0;
    // 缺口② 实时搜索 flag（仅 --search 开时打包成 SubgameSearchConfig）。
    let mut search_on = false;
    let mut search_iters: u64 = 1000;
    let mut search_trigger = SearchTrigger::FlopFirstUnraised;
    let mut search_time_budget_ms: Option<u64> = None;
    let mut search_lcfr = false;
    let mut search_max_nodes: usize = SubgameSearchConfig::default().max_subtree_nodes;
    let mut it = std::env::args().skip(1);
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
                search_iters = next_val(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad search-iterations: {e}"))?
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
            "--search-max-nodes" => {
                search_max_nodes = next_val(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad search-max-nodes: {e}"))?
            }
            other => return Err(format!("unknown arg {other}")),
        }
    }
    // --search-* flag 仅在 --search 开时生效，避免「设了参数却忘开搜索」静默跑 blueprint。
    let search = if search_on {
        Some(SubgameSearchConfig {
            iterations: search_iters,
            max_subtree_nodes: search_max_nodes,
            trigger: search_trigger,
            lcfr: search_lcfr,
            time_budget: search_time_budget_ms.map(Duration::from_millis),
            // 解到终局（深码 / 多人 §2.1）：depth_limit / biased_leaf 均 false（默认）；
            // resolve_root / use_blueprint_range / seed 用默认（RoundStart / true / 固定基）。
            ..SubgameSearchConfig::default()
        })
    } else {
        if search_iters != 1000
            || search_trigger != SearchTrigger::FlopFirstUnraised
            || search_time_budget_ms.is_some()
            || search_lcfr
            || search_max_nodes != SubgameSearchConfig::default().max_subtree_nodes
        {
            return Err("设了 --search-* 参数但未开 --search（拒绝静默跑 blueprint）".to_string());
        }
        None
    };
    Ok(Args {
        checkpoint: checkpoint.ok_or("缺 --checkpoint")?,
        bucket_table: bucket_table.ok_or("缺 --bucket-table")?,
        reshape,
        postflop_cap,
        seed,
        search,
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
        };
        let resp = decide(&game, &abs, &uniform, &req, 0xC0FFEE, None);
        assert!(is_legal(&resp, &req.valid), "BTN 决策应合法，得 {resp:?}");
        assert_eq!(
            resp.source, "blueprint",
            "faithful 路径应由 blueprint 驱动，得 {resp:?}"
        );
        // 确定性：同输入同输出。
        let again = decide(&game, &abs, &uniform, &req, 0xC0FFEE, None);
        assert_eq!(resp, again, "同 seed 同输入应确定性");
    }

    /// 结构性 gap：对手 UTG open-limp（call to=20）→ nolimp blueprint 影子无对应节点 →
    /// 兜底（source=fallback:structural_gap、动作合法），不 panic、不静默乱出。
    #[test]
    fn opponent_open_limp_into_nolimp_falls_back() {
        let game = nolimp_game();
        let abs = game.abstraction().clone();
        let uniform = |_i: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // my_seat=4(HJ)；UTG(3) open-limp call to=20 → 我(HJ) 决策时重放撞 gap。
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
        };
        let resp = decide(&game, &abs, &uniform, &req, 1, None);
        assert!(resp.source.starts_with("fallback:"), "应兜底，得 {resp:?}");
        assert!(is_legal(&resp, &req.valid), "兜底动作须合法，得 {resp:?}");
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
        };
        let resp = decide(&game, &abs, &uniform, &req, 0, None);
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
        let off = decide(&game, &abs, &uniform, &req, 0x5EED, None);
        let on_untriggered = decide(&game, &abs, &uniform, &req, 0x5EED, Some(&scfg));
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
        let resp = decide(&game, &abs, &uniform, &req, 0xA11CE, Some(&scfg));
        assert!(is_legal(&resp, &req.valid), "搜索动作须合法，得 {resp:?}");
        // source 要么 search（解成功），要么 search_fold:*（罕见解不出来），绝不静默 blueprint。
        assert!(
            resp.source == "search" || resp.source.starts_with("search_fold:"),
            "搜索区 source 须 search / search_fold:*，得 {resp:?}"
        );
        let again = decide(&game, &abs, &uniform, &req, 0xA11CE, Some(&scfg));
        assert_eq!(resp, again, "同 seed 搜索须确定性（byte-equal 可复现）");
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
        let resp = decide(&game, &abs, &uniform, &req, 0xDEE7, Some(&scfg));
        assert!(
            is_legal(&resp, &req.valid),
            "深码搜索动作须合法，得 {resp:?}"
        );
        assert!(
            resp.source == "search" || resp.source.starts_with("search_fold:"),
            "搜索区 source，得 {resp:?}"
        );
    }
}
