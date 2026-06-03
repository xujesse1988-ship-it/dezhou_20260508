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
//! # 已知限制（blueprint-only，`...client_design...` §4）
//!
//! - 码深 ≠ 100BB：solver 树/SPR 都按 100BB 解；real `GameState` 用 `default_6max_100bb`
//!   （10000 筹码）近似，driver 靠买入锁 2000 + 栈漂出 [80,125]BB 即 leave/rejoin 兜。
//! - 非 6 人桌 / 对手 open-limp：no-limp blueprint 无对应节点 → 走兜底（见上）。

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use poker::training::blueprint_advisor::{advance_shadow_by_applied, outgoing_action, parse_card};
use poker::training::game::Game;
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::nlhe_betting_tree::{
    first_small_6max, first_small_preopen_6max, first_small_preopen_small_6max,
};
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::sampling::sample_discrete;
use poker::{
    AbstractAction, Action, BucketTable, Card, ChaCha20Rng, ChipAmount, InfoSetId, SeatId,
    StreetActionAbstraction, TableConfig,
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

/// 一次决策：重放本手历史（real + abs 两态 lockstep）→ 我方决策点查 blueprint → outgoing。
/// 任何失败返回安全兜底（不 panic）。
fn decide(
    game: &SimplifiedNlheGame,
    abstraction: &StreetActionAbstraction,
    strategy_fn: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    req: &Request,
    base_seed: u64,
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

    // 查策略 + uniform 兜底（空 / 全零 / 长度不符）。
    let raw = strategy_fn(&info, legal_abs.len());
    let dist: Vec<(AbstractAction, f64)> =
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
        };

    // per-decision 确定性采样（保混合策略 + 可复现）。
    let mut sample_rng = ChaCha20Rng::from_seed(sample_seed(req, base_seed));
    let chosen = sample_discrete(&dist, &mut sample_rng);

    // —— outgoing：solver Action → OpenPoker {action, amount} ——
    let solver_action = match outgoing_action(&real, abstraction, chosen) {
        Ok(a) => a,
        Err(_) => return safe_fallback(&req.valid, "outgoing_failed"),
    };
    let resp = match solver_action {
        Action::Fold => Response {
            action: "fold".into(),
            ..Default::default()
        },
        Action::Check => {
            if req.valid.can_check {
                Response {
                    action: "check".into(),
                    ..Default::default()
                }
            } else {
                return safe_fallback(&req.valid, "check_illegal");
            }
        }
        Action::Call => {
            if req.valid.can_call {
                Response {
                    action: "call".into(),
                    ..Default::default()
                }
            } else if req.valid.can_check {
                Response {
                    action: "check".into(),
                    ..Default::default()
                }
            } else {
                return safe_fallback(&req.valid, "call_illegal");
            }
        }
        Action::Bet { to } | Action::Raise { to } => {
            // solver to → OpenPoker to（÷scale，四舍五入），夹进 [min_raise, max_raise]。
            match raise_to_op(to.as_u64(), scale, &req.valid) {
                Some(op_to) => Response {
                    action: "raise".into(),
                    amount: Some(op_to),
                    ..Default::default()
                },
                None => return safe_fallback(&req.valid, "raise_no_range"),
            }
        }
        Action::AllIn => {
            // OpenPoker all_in 无 amount（服务端归一）。无 all_in 动作则退到 max raise。
            if let Some(max) = req.valid.max_raise {
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

    Response {
        source: "blueprint".into(),
        street: Some(street_label(street).into()),
        info_set: Some(info.raw()),
        chosen: Some(action_label(&chosen)),
        ..resp
    }
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
}

#[derive(Serialize)]
struct ReadyLine {
    ready: bool,
    update_count: u64,
    reshape: String,
    n_seats: usize,
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
    };
    eprintln!(
        "[openpoker_advisor] ready reshape={} update_count={}",
        ready.reshape, ready.update_count
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
            Ok(req) => decide(game, &abstraction, &strategy_fn, &req, args.seed),
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
            other => return Err(format!("unknown arg {other}")),
        }
    }
    Ok(Args {
        checkpoint: checkpoint.ok_or("缺 --checkpoint")?,
        bucket_table: bucket_table.ok_or("缺 --bucket-table")?,
        reshape,
        postflop_cap,
        seed,
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
        };
        let resp = decide(&game, &abs, &uniform, &req, 0xC0FFEE);
        assert!(is_legal(&resp, &req.valid), "BTN 决策应合法，得 {resp:?}");
        assert_eq!(
            resp.source, "blueprint",
            "faithful 路径应由 blueprint 驱动，得 {resp:?}"
        );
        // 确定性：同输入同输出。
        let again = decide(&game, &abs, &uniform, &req, 0xC0FFEE);
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
        };
        let resp = decide(&game, &abs, &uniform, &req, 1);
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
        };
        let resp = decide(&game, &abs, &uniform, &req, 0);
        assert_eq!(resp.source, "fallback:not_6max");
        assert!(is_legal(&resp, &req.valid));
    }
}
