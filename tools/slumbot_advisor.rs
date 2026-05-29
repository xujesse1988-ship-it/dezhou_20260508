//! 常驻 Slumbot advisor（`docs/temp/slumbot_api_bridge_plan_2026_05_29.md`）。
//!
//! Python driver 跑 HTTP/TLS/token/重连；本进程常驻，启动加载 dense blueprint +
//! v4 bucket 表一次，之后每个我方决策点收一行 JSON（hole/board/client_pos/action）、
//! 重放该手、查 blueprint、出一行 `{"incr":...}`。每决策无状态（消息自带完整手局），
//! 可重放、可单测。所有正确性逻辑留在 `cargo test`（T2..T5），crate 零网络依赖。
//!
//! **profile 对齐**（Slumbot ↔ `TableConfig::default_hu_200bb`）：SB=50 / BB=100 /
//! stack=20000 / 2 player，每手重置。Slumbot `b<N>` 的 `N = street_last_bet_to`
//! 与求解器 `Bet/Raise{to}` 同语义（本街累计到，preflop 含盲注）→ `to` 基本 1:1。
//!
//! **座位（B2）**：Slumbot pos 1 = SB/button = 求解器 `SeatId(0)`（preflop 先动）；
//! pos 0 = BB = `SeatId(1)`（postflop 先动）。`solver_seat = 1 - slumbot_pos`。
//!
//! **抽象耦合**：advisor 必须用与训练 betting tree **同一** action abstraction
//! （当前 `StreetActionAbstraction::default_6_action`，与 `nlhe.rs::nlhe_action_abstraction`
//! 对齐）。bet-size 扩张后两处须同步改，否则 tag 对齐错位。

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::nlhe_betting_tree::AbstractActionTag;
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::sampling::sample_discrete;
use poker::{
    AbstractAction, Action, ActionAbstraction, BucketTable, Card, ChaCha20Rng, ChipAmount,
    GameState, InfoSetId, Rank, RngSource, SeatId, StreetActionAbstraction, Suit, TableConfig,
};

// ===========================================================================
// Card 字符串解析（"Ac" → Card；T2）
// ===========================================================================

/// 解析一张 Slumbot 牌字符串（rank 大写 + suit 小写，如 "Ac"/"Td"/"9h"/"2s"）。
fn parse_card(s: &str) -> Result<Card, String> {
    let b = s.as_bytes();
    if b.len() != 2 {
        return Err(format!("card {s:?} 必须是 2 字符（rank+suit）"));
    }
    let rank = match b[0] {
        b'2' => Rank::Two,
        b'3' => Rank::Three,
        b'4' => Rank::Four,
        b'5' => Rank::Five,
        b'6' => Rank::Six,
        b'7' => Rank::Seven,
        b'8' => Rank::Eight,
        b'9' => Rank::Nine,
        b'T' => Rank::Ten,
        b'J' => Rank::Jack,
        b'Q' => Rank::Queen,
        b'K' => Rank::King,
        b'A' => Rank::Ace,
        other => return Err(format!("非法 rank 字符 {:?} in {s:?}", other as char)),
    };
    let suit = match b[1] {
        b'c' => Suit::Clubs,
        b'd' => Suit::Diamonds,
        b'h' => Suit::Hearts,
        b's' => Suit::Spades,
        other => return Err(format!("非法 suit 字符 {:?} in {s:?}", other as char)),
    };
    Ok(Card::new(rank, suit))
}

/// Card → Slumbot 字符串（T2 round-trip 用）。
#[cfg(test)]
fn card_to_string(c: Card) -> String {
    let r = match c.rank() {
        Rank::Two => '2',
        Rank::Three => '3',
        Rank::Four => '4',
        Rank::Five => '5',
        Rank::Six => '6',
        Rank::Seven => '7',
        Rank::Eight => '8',
        Rank::Nine => '9',
        Rank::Ten => 'T',
        Rank::Jack => 'J',
        Rank::Queen => 'Q',
        Rank::King => 'K',
        Rank::Ace => 'A',
    };
    let s = match c.suit() {
        Suit::Clubs => 'c',
        Suit::Diamonds => 'd',
        Suit::Hearts => 'h',
        Suit::Spades => 's',
    };
    format!("{r}{s}")
}

fn parse_cards(list: &[String]) -> Result<Vec<Card>, String> {
    list.iter().map(|s| parse_card(s)).collect()
}

// ===========================================================================
// action 串 token 化（k/c/f/b<N>/'/'）
// ===========================================================================

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Token {
    Check,
    Call,
    Fold,
    /// `b<N>`：本街下注到 N（绝对，含 preflop 盲注；与求解器 `to` 同语义）。
    BetTo(u64),
    /// `/`：分街分隔符（状态机会自动切街，这里仅跳过 + 校验）。
    StreetSep,
}

/// 把 Slumbot action 串切成 token 序列。语法错误（未知字符 / 缺 bet size /
/// bet size 非数字）→ Err。
fn tokenize(action: &str) -> Result<Vec<Token>, String> {
    let bytes = action.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'k' => {
                out.push(Token::Check);
                i += 1;
            }
            b'c' => {
                out.push(Token::Call);
                i += 1;
            }
            b'f' => {
                out.push(Token::Fold);
                i += 1;
            }
            b'/' => {
                out.push(Token::StreetSep);
                i += 1;
            }
            b'b' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i == start {
                    return Err(format!("'b' 后缺 bet size @ {start} in {action:?}"));
                }
                let n: u64 = action[start..i]
                    .parse()
                    .map_err(|e| format!("bet size 非整数 {:?}: {e}", &action[start..i]))?;
                out.push(Token::BetTo(n));
            }
            other => {
                return Err(format!(
                    "action 串非法字符 {:?} in {action:?}",
                    other as char
                ));
            }
        }
    }
    Ok(out)
}

// ===========================================================================
// 重放引擎（两态 lockstep；M3）
// ===========================================================================

/// 重放结束后我方决策点的两态快照。`abs` 提供树位置（`current_node_id`）+ 训练序
/// 合法动作集；`real` 提供真实 pot / 合法下注区间（算 outgoing 尺寸）。
struct DecisionContext {
    abs: SimplifiedNlheState,
    real: GameState,
}

const ABS_DUMMY_SEED: u64 = 0x4142_5344_554d_4d59; // "ABSDUMMY"
const REAL_DUMMY_SEED: u64 = 0x5245_414c_4453_4544; // "REALDSED"

/// 在 abs（抽象影子）+ real（真实筹码）两态上同步重放 `tokens`。两态都从 root 起、
/// 随机发牌（牌不参与重放，只用树位置 + 真实筹码）。
///
/// - `k`/`c`/`f`：两态 apply 对应动作（abs 取 `legal_actions(abs)` 里同 tag 的动作）。
/// - `b<N>`：以**抽象影子为参考系** `map_off_tree(abs.game_state, N)` 选最近 ratio →
///   投影到 abs 当前节点合法集（塌进 AllIn 兜底）→ abs apply；real apply
///   `Bet/Raise{to:N}`（N≥real all-in 或 abs 塌进 AllIn 时两态都走 AllIn，保持 lockstep）。
///
/// 不在终局报错（留给 caller 判），但任一 apply 非法 / 两态 current_player 漂移即
/// 返回 Err（desync）。
fn replay(
    game: &SimplifiedNlheGame,
    abstraction: &StreetActionAbstraction,
    tokens: &[Token],
) -> Result<DecisionContext, String> {
    let cfg = TableConfig::default_hu_200bb();
    let mut rng = ChaCha20Rng::from_seed(ABS_DUMMY_SEED);
    let mut abs = game.root(&mut rng);
    let mut real = GameState::new(&cfg, REAL_DUMMY_SEED);

    for (idx, token) in tokens.iter().enumerate() {
        if *token == Token::StreetSep {
            continue;
        }
        // 两态进终局后不应再有非分隔 token（合法 Slumbot 串里 fold / all-in-call 之后
        // 只剩 '/'）。出现即 desync。
        if real.is_terminal() || real.current_player().is_none() {
            return Err(format!(
                "token #{idx} {token:?} 出现在终局之后（real terminal）"
            ));
        }

        let (abs_action, real_action) = resolve_actions(abstraction, &abs, &real, *token)?;
        real.apply(real_action).map_err(|e| {
            format!("token #{idx} {token:?} real.apply({real_action:?}) 非法: {e:?}")
        })?;
        abs = SimplifiedNlheGame::next(abs, abs_action, &mut rng);

        // lockstep：两态 current_player 必须一致（含都 None = 都终局）。
        let abs_cp = abs.game_state.current_player();
        let real_cp = real.current_player();
        if abs_cp != real_cp {
            return Err(format!(
                "token #{idx} {token:?} 后两态 current_player 漂移: abs={abs_cp:?} real={real_cp:?}"
            ));
        }
    }

    Ok(DecisionContext { abs, real })
}

/// 给定一个 token，算出 (abs 侧抽象动作, real 侧 stage-1 动作)。
fn resolve_actions(
    abstraction: &StreetActionAbstraction,
    abs: &SimplifiedNlheState,
    real: &GameState,
    token: Token,
) -> Result<(AbstractAction, Action), String> {
    let legal_abs = SimplifiedNlheGame::legal_actions(abs);
    match token {
        Token::Check => Ok((
            find_tag(&legal_abs, AbstractActionTag::Check).ok_or("Check 在 abs 当前节点不合法")?,
            Action::Check,
        )),
        Token::Call => Ok((
            // call-of-all-in：当 call 额 == 自己的 all-in cap（HU 等额起手栈下对手 all-in 即此），
            // `abstract_actions` 按 AA-004-rev1 把该 Call 折进 AllIn 槽 → 无 Call 边，落到 AllIn。
            // real 侧 Action::Call 由规则引擎归一化为 all-in 跟注（→ 终局），与 abs 一致。
            find_tag(&legal_abs, AbstractActionTag::Call)
                .or_else(|| find_tag(&legal_abs, AbstractActionTag::AllIn))
                .ok_or("Call/AllIn 在 abs 当前节点都不合法")?,
            Action::Call,
        )),
        Token::Fold => Ok((
            find_tag(&legal_abs, AbstractActionTag::Fold).ok_or("Fold 在 abs 当前节点不合法")?,
            Action::Fold,
        )),
        Token::BetTo(n) => {
            // incoming 映射：以抽象影子为参考系选最近 ratio。
            let raw = abstraction.map_off_tree(&abs.game_state, ChipAmount::new(n));
            let raw_tag = AbstractActionTag::of(&raw);
            // 投影到 abs 当前合法集；塌进 AllIn 兜底。
            let abs_action = project_tag_onto(&legal_abs, raw_tag)?;
            let projected_tag = AbstractActionTag::of(&abs_action);

            // real all-in 判定：N ≥ real 当前 all-in 的 to。
            let real_all_in_to = real.legal_actions().all_in_amount.map(|c| c.as_u64());
            let real_is_all_in = matches!(real_all_in_to, Some(cap) if n >= cap);
            let abs_collapsed = matches!(projected_tag, AbstractActionTag::AllIn);

            if real_is_all_in || abs_collapsed {
                // 任一参考系塌进 all-in → 两态都走 AllIn（保持 lockstep；尺寸漂移已知近似）。
                let abs_all_in = find_tag(&legal_abs, AbstractActionTag::AllIn)
                    .ok_or("AllIn 在 abs 当前节点不合法（投影兜底失败）")?;
                Ok((abs_all_in, Action::AllIn))
            } else {
                // 正常档：abs 走投影出的 Bet/Raise(ratio)，real 走 Bet/Raise{to:N}。
                let real_action = match projected_tag {
                    AbstractActionTag::Bet(_) => Action::Bet {
                        to: ChipAmount::new(n),
                    },
                    AbstractActionTag::Raise(_) => Action::Raise {
                        to: ChipAmount::new(n),
                    },
                    // map_off_tree 对 N ≤ max_committed 的退化映射（理论上 b<N> 不该到这）。
                    AbstractActionTag::Call => Action::Call,
                    AbstractActionTag::Check => Action::Check,
                    AbstractActionTag::Fold => Action::Fold,
                    AbstractActionTag::AllIn => unreachable!("AllIn 已在上面分支处理"),
                };
                Ok((abs_action, real_action))
            }
        }
        Token::StreetSep => unreachable!("StreetSep 由 replay 跳过，不进 resolve_actions"),
    }
}

/// 在合法动作集里找指定 tag 的 `AbstractAction`（带正确 `to` 标签）。
fn find_tag(legal: &[AbstractAction], tag: AbstractActionTag) -> Option<AbstractAction> {
    legal
        .iter()
        .copied()
        .find(|a| AbstractActionTag::of(a) == tag)
}

/// 把 map_off_tree 选出的 tag 投影到 abs 当前合法集：tag 在则取之；不在（该 ratio
/// 在抽象 pot 下已塌进 AllIn slot）则退到 AllIn。两者都缺 → Err（决策节点必有 ≥1 动作，
/// 不该发生）。
fn project_tag_onto(
    legal: &[AbstractAction],
    tag: AbstractActionTag,
) -> Result<AbstractAction, String> {
    if let Some(a) = find_tag(legal, tag) {
        return Ok(a);
    }
    if let Some(a) = find_tag(legal, AbstractActionTag::AllIn) {
        return Ok(a);
    }
    Err(format!(
        "投影失败：tag {tag:?} 与 AllIn 都不在 abs 合法集 {legal:?}"
    ))
}

// ===========================================================================
// outgoing 翻译（以真实 pot 算尺寸；M4）
// ===========================================================================

/// 把策略选中的抽象动作翻译成 Slumbot incr 串，尺寸以**真实** state 算。
fn outgoing_incr(
    real: &GameState,
    abstraction: &StreetActionAbstraction,
    chosen: AbstractAction,
) -> Result<String, String> {
    match chosen {
        AbstractAction::Fold => Ok("f".to_string()),
        AbstractAction::Check => Ok("k".to_string()),
        AbstractAction::Call { .. } => Ok("c".to_string()),
        AbstractAction::AllIn { .. } => {
            let to = real
                .legal_actions()
                .all_in_amount
                .ok_or("选中 AllIn 但 real 无 all_in_amount（无筹码？）")?;
            Ok(bet_or_call(real, to.as_u64()))
        }
        AbstractAction::Bet { ratio_label, .. } | AbstractAction::Raise { ratio_label, .. } => {
            // 同一抽象作用在真实 pot 上，找同 ratio 的档取其真实 `to`。
            let real_actions = abstraction.abstract_actions(real);
            for a in real_actions.iter() {
                match a {
                    AbstractAction::Bet { to, ratio_label: r }
                    | AbstractAction::Raise { to, ratio_label: r }
                        if r.as_milli() == ratio_label.as_milli() =>
                    {
                        return Ok(bet_or_call(real, to.as_u64()));
                    }
                    _ => {}
                }
            }
            // 该档在真实 pot 下已塌成 AllIn（被 abstract_actions 折叠）→ 发 all-in。
            let to = real
                .legal_actions()
                .all_in_amount
                .ok_or("选中 Bet/Raise 档但真实侧既无该档也无 all_in_amount")?;
            Ok(bet_or_call(real, to.as_u64()))
        }
    }
}

/// 把目标 `to`（本街累计下注到）翻成 Slumbot incr：`to` **严格高于**当前下注水位
/// （`max committed_this_round`）才构成合法 bet/raise（增量 > 0）→ `b<to>`；否则不是
/// 加注 = 跟注 → `c`。关键 case：对手 all-in 覆盖我方时，我方"AllIn"的 to == 对手
/// 下注水位（甚至更低，all-in for less），增量 0 会被 Slumbot 判 Illegal bet——这其实
/// 是 all-in 跟注，应发 `c`。
fn bet_or_call(real: &GameState, to: u64) -> String {
    let bet_level = real
        .players()
        .iter()
        .map(|p| p.committed_this_round.as_u64())
        .max()
        .unwrap_or(0);
    if to > bet_level {
        format!("b{to}")
    } else {
        "c".to_string()
    }
}

// ===========================================================================
// 决策主路径（重放 → 查策略 → 采样 → outgoing；M4）
// ===========================================================================

#[derive(Deserialize, Debug)]
struct Request {
    hole_cards: Vec<String>,
    #[serde(default)]
    board: Vec<String>,
    client_pos: u8,
    #[serde(default)]
    action: String,
}

#[derive(Serialize, Debug, Default)]
struct Response {
    #[serde(skip_serializing_if = "Option::is_none")]
    incr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Response {
    fn ok(incr: String) -> Self {
        Response {
            incr: Some(incr),
            error: None,
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        Response {
            incr: None,
            error: Some(msg.into()),
        }
    }
}

/// 期望本街公共牌数（preflop=0 / flop=3 / turn=4 / river=5）。
fn expected_board_len(street: poker::Street) -> usize {
    match street {
        poker::Street::Preflop => 0,
        poker::Street::Flop => 3,
        poker::Street::Turn => 4,
        poker::Street::River => 5,
        poker::Street::Showdown => 5,
    }
}

/// 一次决策：解析 → 重放 → 座位断言 → 查 blueprint → 采样 → outgoing。
/// `strategy_fn(info, n)` 返回该 infoset 的 `n` 维分布（空 / 全零 → 均匀兜底）。
fn decide(
    game: &SimplifiedNlheGame,
    abstraction: &StreetActionAbstraction,
    req: &Request,
    strategy_fn: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    base_seed: u64,
) -> Result<String, String> {
    if req.hole_cards.len() != 2 {
        return Err(format!(
            "hole_cards 必须 2 张，收到 {}",
            req.hole_cards.len()
        ));
    }
    let hole_vec = parse_cards(&req.hole_cards)?;
    let hole: [Card; 2] = [hole_vec[0], hole_vec[1]];
    let board = parse_cards(&req.board)?;
    let tokens = tokenize(&req.action)?;

    let ctx = replay(game, abstraction, &tokens)?;

    // 座位断言（B2）：重放后该我方动 = solver SeatId(1 - client_pos)。
    if req.client_pos > 1 {
        return Err(format!("client_pos 必须 0/1，收到 {}", req.client_pos));
    }
    let expected = SeatId(1 - req.client_pos);
    let abs_cp = ctx.abs.game_state.current_player();
    let real_cp = ctx.real.current_player();
    if abs_cp != Some(expected) || real_cp != Some(expected) {
        return Err(format!(
            "seat/parse desync: 重放后 abs={abs_cp:?} real={real_cp:?}, 期望 {expected:?} (client_pos={})",
            req.client_pos
        ));
    }

    // board 长度须与决策街一致（否则 canonical_observation_id 会 panic；这里转成 Err）。
    let street = ctx.abs.game_state.street();
    let want = expected_board_len(street);
    if board.len() != want {
        return Err(format!(
            "board 长度 {} 与决策街 {street:?} 期望 {want} 不符",
            board.len()
        ));
    }

    let node_id = ctx.abs.current_node_id;
    let legal = SimplifiedNlheGame::legal_actions(&ctx.abs);
    if legal.is_empty() {
        return Err("决策节点合法动作集为空（不该发生）".to_string());
    }
    let info = game.info_set_for_cards(node_id, hole, &board);

    // 查策略 + 均匀兜底（空 / 全零）。
    let raw_dist = strategy_fn(&info, legal.len());
    let dist: Vec<f64> = if raw_dist.is_empty() || raw_dist.iter().all(|p| *p <= 0.0) {
        vec![1.0 / legal.len() as f64; legal.len()]
    } else {
        raw_dist
    };
    if dist.len() != legal.len() {
        return Err(format!(
            "strategy length mismatch: dist {} vs legal {} @ node {node_id}",
            dist.len(),
            legal.len()
        ));
    }

    // per-decision 确定性采样：seed = hash(action, hole, board, base_seed)。
    let idx = sample_index(&dist, req, &hole, &board, base_seed);
    let chosen = legal[idx];
    outgoing_incr(&ctx.real, abstraction, chosen)
}

/// 从分布按 per-decision 确定性 seed 采样索引（保留混合策略 + 可复现）。
fn sample_index(
    dist: &[f64],
    req: &Request,
    hole: &[Card; 2],
    board: &[Card],
    base_seed: u64,
) -> usize {
    let mut hasher = blake3::Hasher::new();
    hasher.update(req.action.as_bytes());
    hasher.update(&[req.client_pos]);
    for c in hole {
        hasher.update(&[c.to_u8()]);
    }
    for c in board {
        hasher.update(&[c.to_u8()]);
    }
    hasher.update(&base_seed.to_le_bytes());
    let digest = hasher.finalize();
    let seed = u64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("blake3 ≥ 8 bytes"));
    let mut rng = ChaCha20Rng::from_seed(seed);

    let pairs: Vec<(usize, f64)> = dist
        .iter()
        .enumerate()
        .filter(|(_, p)| **p > 0.0)
        .map(|(i, p)| (i, *p))
        .collect();
    // dist 已被 decide 兜底成非全零，pairs 必非空。
    sample_discrete(&pairs, &mut rng)
}

// ===========================================================================
// blueprint 加载 + ready + stdio 主循环（M2）
// ===========================================================================

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum FallbackPolicy {
    Average,
    Current,
    Hybrid,
}

impl FallbackPolicy {
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "average" | "avg" => Ok(FallbackPolicy::Average),
            "current" | "cur" => Ok(FallbackPolicy::Current),
            "hybrid" => Ok(FallbackPolicy::Hybrid),
            other => Err(format!(
                "未知 --fallback-policy {other:?}（average|current|hybrid）"
            )),
        }
    }
}

struct Args {
    checkpoint: PathBuf,
    bucket_table: PathBuf,
    dense: bool,
    fallback_policy: FallbackPolicy,
    seed: u64,
}

#[derive(Serialize)]
struct ReadyLine {
    ready: bool,
    update_count: u64,
    strategy_blake3: String,
    fallback_policy: String,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[slumbot_advisor] fatal: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if !args.dense {
        return Err("当前仅支持 --dense（dense raw v3 checkpoint）".to_string());
    }
    eprintln!(
        "[slumbot_advisor] checkpoint={} bucket={}",
        args.checkpoint.display(),
        args.bucket_table.display()
    );

    let table = Arc::new(BucketTable::open(&args.bucket_table).map_err(|e| {
        format!(
            "BucketTable::open({}) failed: {e:?}",
            args.bucket_table.display()
        )
    })?);
    let game = SimplifiedNlheGame::new(Arc::clone(&table))
        .map_err(|e| format!("SimplifiedNlheGame::new failed: {e:?}"))?;
    let trainer =
        DenseNlheEsMccfrTrainer::load_checkpoint(&args.checkpoint, game).map_err(|e| {
            format!(
                "load dense checkpoint {} failed: {e:?}",
                args.checkpoint.display()
            )
        })?;

    let abstraction = StreetActionAbstraction::default_6_action();
    let fallback = args.fallback_policy;
    // `trainer` 不 move：`game` 借自 trainer，strategy_fn 也借 trainer（都只读，可共存）。
    let game: &SimplifiedNlheGame = trainer.game();

    // ready 行：update_count + strategy_blake3（与 nlhe_h3_report 同 probe walk，可对照
    // status_v2 记录的 blueprint hash 验证 ckpt + bucket 表加载正确）。
    let update_count = trainer.update_count();
    let strategy_blake3 = compute_strategy_blake3(&trainer, game);
    let ready = ReadyLine {
        ready: true,
        update_count,
        strategy_blake3,
        fallback_policy: fallback_slug(fallback).to_string(),
    };
    eprintln!(
        "[slumbot_advisor] ready update_count={} strategy_blake3={}",
        ready.update_count, ready.strategy_blake3
    );
    let mut stdout = std::io::stdout();
    writeln!(
        stdout,
        "{}",
        serde_json::to_string(&ready).map_err(|e| format!("serialize ready: {e}"))?
    )
    .map_err(|e| format!("write ready: {e}"))?;
    stdout.flush().map_err(|e| format!("flush ready: {e}"))?;

    let strategy_fn = |info: &InfoSetId, _n: usize| -> Vec<f64> {
        match fallback {
            FallbackPolicy::Average => trainer.average_strategy(*info),
            FallbackPolicy::Current => trainer.current_strategy(*info),
            FallbackPolicy::Hybrid => {
                if trainer.strategy_sum().row_sum_by_info(*info) <= 0.0 {
                    trainer.current_strategy(*info)
                } else {
                    trainer.average_strategy(*info)
                }
            }
        }
    };

    // stdio JSON-lines 主循环：每行一个 Request → decide → 一行 Response。
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("stdin read: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => match decide(game, &abstraction, &req, &strategy_fn, args.seed) {
                Ok(incr) => Response::ok(incr),
                Err(e) => Response::err(e),
            },
            Err(e) => Response::err(format!("bad request JSON: {e}")),
        };
        writeln!(
            stdout,
            "{}",
            serde_json::to_string(&resp).map_err(|e| format!("serialize resp: {e}"))?
        )
        .map_err(|e| format!("stdout write: {e}"))?;
        stdout.flush().map_err(|e| format!("stdout flush: {e}"))?;
    }
    Ok(())
}

fn fallback_slug(p: FallbackPolicy) -> &'static str {
    match p {
        FallbackPolicy::Average => "average",
        FallbackPolicy::Current => "current",
        FallbackPolicy::Hybrid => "hybrid",
    }
}

/// blueprint strategy 指纹：与 `nlhe_h3_report::strategy_hash` 同 probe walk + 同
/// 字节布局，因此对同一 dense 100M blueprint 复现 `status_v2.md` 记录的
/// `2fab8afe…`——advisor 加载正确性的端到端校验。
fn compute_strategy_blake3(trainer: &DenseNlheEsMccfrTrainer, game: &SimplifiedNlheGame) -> String {
    let probes = collect_strategy_probes(game);
    let mut hasher = blake3::Hasher::new();
    hasher.update(&trainer.update_count().to_le_bytes());
    hasher.update(&(probes.len() as u64).to_le_bytes());
    for info in probes {
        let strat = trainer.average_strategy(info);
        hasher.update(&info.raw().to_le_bytes());
        hasher.update(&(strat.len() as u32).to_le_bytes());
        for p in strat {
            hasher.update(&p.to_le_bytes());
        }
    }
    hex32(hasher.finalize().as_bytes())
}

/// 沿固定随机路径采 ≤ 4096 个 infoset（与 `nlhe_h3_report::collect_strategy_probes`
/// 逐行一致，含同一 seed 0x4833_5052_4f42_4553）。
fn collect_strategy_probes(game: &SimplifiedNlheGame) -> Vec<InfoSetId> {
    let mut rng = ChaCha20Rng::from_seed(0x4833_5052_4f42_4553);
    let mut state = game.root(&mut rng);
    let mut probes = Vec::with_capacity(4096);
    for _ in 0..4096 {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => break,
            NodeKind::Chance => break, // 简化 NLHE 无 in-game chance（防御）。
            NodeKind::Player(actor) => {
                probes.push(SimplifiedNlheGame::info_set(&state, actor));
                let actions = SimplifiedNlheGame::legal_actions(&state);
                if actions.is_empty() {
                    break;
                }
                let idx = (rng.next_u64() as usize) % actions.len();
                state = SimplifiedNlheGame::next(state, actions[idx], &mut rng);
            }
        }
    }
    probes
}

fn hex32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_args() -> Result<Args, String> {
    let mut checkpoint: Option<PathBuf> = None;
    let mut bucket_table: Option<PathBuf> = None;
    let mut dense = false;
    let mut fallback_policy = FallbackPolicy::Hybrid;
    let mut seed: u64 = 0;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--checkpoint" => checkpoint = Some(PathBuf::from(next_val(&mut it, &arg)?)),
            "--bucket-table" | "--artifact" => {
                bucket_table = Some(PathBuf::from(next_val(&mut it, &arg)?))
            }
            "--dense" => dense = true,
            "--fallback-policy" => {
                fallback_policy = FallbackPolicy::from_str(&next_val(&mut it, &arg)?)?
            }
            "--seed" => seed = parse_u64(&next_val(&mut it, &arg)?)?,
            other => return Err(format!("未知参数 {other}")),
        }
    }
    Ok(Args {
        checkpoint: checkpoint.ok_or("缺 --checkpoint")?,
        bucket_table: bucket_table.ok_or("缺 --bucket-table")?,
        dense,
        fallback_policy,
        seed,
    })
}

fn next_val(it: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{name} 需要一个值"))
}

fn parse_u64(raw: &str) -> Result<u64, String> {
    if let Some(hex) = raw.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).map_err(|e| format!("非法 hex {raw}: {e}"))
    } else {
        raw.parse().map_err(|e| format!("非法整数 {raw}: {e}"))
    }
}

// ===========================================================================
// 测试（T2 / tokenize / T3 / T4 / T5）
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use poker::BucketConfig;

    fn stub_game() -> SimplifiedNlheGame {
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ));
        SimplifiedNlheGame::new(table).expect("stub 表构造 NLHE game")
    }

    fn default_abstraction() -> StreetActionAbstraction {
        StreetActionAbstraction::default_6_action()
    }

    // ---- T2: Card 解析 round-trip + 全 52 张 ----
    #[test]
    fn t2_parse_card_specific() {
        assert_eq!(parse_card("Ac").unwrap(), Card::new(Rank::Ace, Suit::Clubs));
        assert_eq!(
            parse_card("Td").unwrap(),
            Card::new(Rank::Ten, Suit::Diamonds)
        );
        assert_eq!(
            parse_card("9h").unwrap(),
            Card::new(Rank::Nine, Suit::Hearts)
        );
        assert_eq!(
            parse_card("2s").unwrap(),
            Card::new(Rank::Two, Suit::Spades)
        );
        assert_eq!(
            parse_card("Ks").unwrap(),
            Card::new(Rank::King, Suit::Spades)
        );
    }

    #[test]
    fn t2_all_52_round_trip() {
        for v in 0u8..52 {
            let c = Card::from_u8(v).unwrap();
            let s = card_to_string(c);
            let back = parse_card(&s).unwrap_or_else(|e| panic!("{s} parse failed: {e}"));
            assert_eq!(back, c, "round-trip 失败: card {v} -> {s} -> {back:?}");
            assert_eq!(back.to_u8(), v);
        }
    }

    #[test]
    fn t2_reject_malformed() {
        assert!(parse_card("X1").is_err());
        assert!(parse_card("A").is_err());
        assert!(parse_card("Acc").is_err());
        assert!(parse_card("1c").is_err()); // '1' 非法 rank
        assert!(parse_card("Ax").is_err()); // 'x' 非法 suit
    }

    #[test]
    fn tokenize_basic() {
        assert_eq!(
            tokenize("b200c/kk/b100c").unwrap(),
            vec![
                Token::BetTo(200),
                Token::Call,
                Token::StreetSep,
                Token::Check,
                Token::Check,
                Token::StreetSep,
                Token::BetTo(100),
                Token::Call,
            ]
        );
        assert_eq!(tokenize("").unwrap(), vec![]);
        assert_eq!(
            tokenize("b20000c///").unwrap(),
            vec![
                Token::BetTo(20000),
                Token::Call,
                Token::StreetSep,
                Token::StreetSep,
                Token::StreetSep
            ]
        );
        assert!(tokenize("b").is_err());
        assert!(tokenize("x").is_err());
    }

    // ---- T3 独立对照：sample_api.py ParseAction 的忠实移植 ----
    // 与求解器的 GameState 重放是两条完全不同的实现（纯字符串语法解析 vs 完整规则
    // 引擎），二者对 (street, 谁动, 是否终局) 一致 = 重放 + 座位映射正确（invariant #7
    // 外部对照）。

    const SMALL_BLIND: i64 = 50;
    const BIG_BLIND: i64 = 100;
    const STACK_SIZE: i64 = 20_000;
    const NUM_STREETS: i32 = 4;

    /// 移植自 ericgjackson sample_api.py 的 ParseAction，返回 `(st, pos)`：
    /// st = 街 0..3；pos = 下一个动的 Slumbot 位（1=SB/0=BB），-1 = 手已结束。
    /// 逐行对照 Python（变量名 / 控制流保持），保证是独立于 GameState 的第二实现。
    fn slumbot_parse_action(action: &str) -> Result<(i32, i32), String> {
        let bytes = action.as_bytes();
        let sz = bytes.len();
        let mut st: i32 = 0;
        let mut street_last_bet_to: i64 = BIG_BLIND;
        let mut total_last_bet_to: i64 = BIG_BLIND;
        let mut last_bet_size: i64 = BIG_BLIND - SMALL_BLIND;
        let mut _last_bettor: i32 = 0;
        let mut pos: i32 = 1;
        if sz == 0 {
            return Ok((st, pos));
        }
        let mut check_or_call_ends_street = false;
        let mut i = 0usize;
        while i < sz {
            if st >= NUM_STREETS {
                return Err("Unexpected error".into());
            }
            let c = bytes[i];
            i += 1;
            match c {
                b'k' => {
                    if last_bet_size > 0 {
                        return Err("Illegal check".into());
                    }
                    if check_or_call_ends_street {
                        if st < NUM_STREETS - 1 && i < sz {
                            if bytes[i] != b'/' {
                                return Err("Missing slash".into());
                            }
                            i += 1;
                        }
                        if st == NUM_STREETS - 1 {
                            pos = -1;
                        } else {
                            pos = 0;
                            st += 1;
                        }
                        street_last_bet_to = 0;
                        check_or_call_ends_street = false;
                    } else {
                        pos = (pos + 1) % 2;
                        check_or_call_ends_street = true;
                    }
                }
                b'c' => {
                    if last_bet_size == 0 {
                        return Err("Illegal call".into());
                    }
                    if total_last_bet_to == STACK_SIZE {
                        if i != sz {
                            for _ in st..(NUM_STREETS - 1) {
                                if i == sz {
                                    return Err("Missing slash (end of string)".into());
                                }
                                let cc = bytes[i];
                                i += 1;
                                if cc != b'/' {
                                    return Err("Missing slash".into());
                                }
                            }
                        }
                        if i != sz {
                            return Err("Extra characters at end of action".into());
                        }
                        st = NUM_STREETS - 1;
                        pos = -1;
                        return Ok((st, pos));
                    }
                    if check_or_call_ends_street {
                        if st < NUM_STREETS - 1 && i < sz {
                            if bytes[i] != b'/' {
                                return Err("Missing slash".into());
                            }
                            i += 1;
                        }
                        if st == NUM_STREETS - 1 {
                            pos = -1;
                        } else {
                            pos = 0;
                            st += 1;
                        }
                        street_last_bet_to = 0;
                        check_or_call_ends_street = false;
                    } else {
                        pos = (pos + 1) % 2;
                        check_or_call_ends_street = true;
                    }
                    last_bet_size = 0;
                    _last_bettor = -1;
                }
                b'f' => {
                    if last_bet_size == 0 {
                        return Err("Illegal fold".into());
                    }
                    if i != sz {
                        return Err("Extra characters at end of action".into());
                    }
                    pos = -1;
                    return Ok((st, pos));
                }
                b'b' => {
                    let j = i;
                    while i < sz && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    if i == j {
                        return Err("Missing bet size".into());
                    }
                    let new_street_last_bet_to: i64 = action[j..i]
                        .parse()
                        .map_err(|_| "Bet size not an integer".to_string())?;
                    let new_last_bet_size = new_street_last_bet_to - street_last_bet_to;
                    let remaining = STACK_SIZE - total_last_bet_to;
                    let mut min_bet_size = if last_bet_size > 0 {
                        last_bet_size.max(BIG_BLIND)
                    } else {
                        BIG_BLIND
                    };
                    if min_bet_size > remaining {
                        min_bet_size = remaining;
                    }
                    if new_last_bet_size < min_bet_size {
                        return Err("Bet too small".into());
                    }
                    if new_last_bet_size > remaining {
                        return Err("Bet too big".into());
                    }
                    last_bet_size = new_last_bet_size;
                    street_last_bet_to = new_street_last_bet_to;
                    total_last_bet_to += last_bet_size;
                    _last_bettor = pos;
                    pos = (pos + 1) % 2;
                    check_or_call_ends_street = true;
                }
                other => return Err(format!("Unexpected character {:?}", other as char)),
            }
        }
        Ok((st, pos))
    }

    fn street_index(s: poker::Street) -> i32 {
        match s {
            poker::Street::Preflop => 0,
            poker::Street::Flop => 1,
            poker::Street::Turn => 2,
            poker::Street::River => 3,
            poker::Street::Showdown => 3,
        }
    }

    /// T3 / T4 用：tokenize + replay 一个 action 串，返回 DecisionContext。
    fn replay_str(
        game: &SimplifiedNlheGame,
        abstraction: &StreetActionAbstraction,
        action: &str,
    ) -> Result<DecisionContext, String> {
        let tokens = tokenize(action)?;
        replay(game, abstraction, &tokens)
    }

    /// T3：求解器重放后的 (街, 谁动, 终局) 与移植版 ParseAction 逐串对齐。
    #[test]
    fn t3_replay_matches_slumbot_parse_action() {
        let game = stub_game();
        let abs = default_abstraction();
        // 覆盖：空串 / limp / 单边 raise / call 进各街 / check 线 / fold / all-in。
        let battery = [
            "",
            "c",
            "ck",
            "b200",
            "b200c",
            "b300c/",
            "b300c/kk",
            "b300c/kk/",
            "b300c/kk/b200",
            "b300c/kk/b200c/",
            "b300c/kk/b200c/k",
            "ck/kk/kk/k",
            "ck/b100c/kk/b300",
            "f",
            "b300f",
            "b20000",
            "b20000c",
            "b20000c///",
        ];
        for s in battery {
            let (st, pos) =
                slumbot_parse_action(s).unwrap_or_else(|e| panic!("ParseAction({s:?}) 出错: {e}"));
            let ctx =
                replay_str(&game, &abs, s).unwrap_or_else(|e| panic!("replay({s:?}) 出错: {e}"));
            let real_cp = ctx.real.current_player();
            let abs_cp = ctx.abs.game_state.current_player();
            assert_eq!(abs_cp, real_cp, "{s:?}: abs/real lockstep 漂移");
            if pos == -1 {
                assert!(
                    real_cp.is_none(),
                    "{s:?}: ParseAction 说终局 (pos=-1)，但 real current_player={real_cp:?}"
                );
            } else {
                let want = SeatId((1 - pos) as u8);
                assert_eq!(
                    real_cp,
                    Some(want),
                    "{s:?}: 座位映射不符 ParseAction st={st} pos={pos}（want solver {want:?}）"
                );
                assert_eq!(
                    street_index(ctx.real.street()),
                    st,
                    "{s:?}: 街不符 ParseAction st={st}"
                );
            }
        }
    }

    /// T4：每个 legal 动作的 outgoing 翻译，b<to> 的 to 落在真实合法区间
    /// [min, all_in]；Fold/Check/Call 出 f/k/c。覆盖 raise spot（preflop root）+
    /// bet spot（flop open）。
    #[test]
    fn t4_outgoing_to_in_legal_range() {
        let game = stub_game();
        let abs = default_abstraction();
        for action in ["", "ck"] {
            let ctx = replay_str(&game, &abs, action).expect("replay");
            let real = &ctx.real;
            let la = real.legal_actions();
            let all_in_to = la.all_in_amount.map(|c| c.as_u64());
            let min_aggro_to = la.bet_range.or(la.raise_range).map(|(min, _)| min.as_u64());
            let legal = SimplifiedNlheGame::legal_actions(&ctx.abs);
            assert!(!legal.is_empty(), "{action:?}: 决策点应有合法动作");
            for chosen in legal {
                let incr = outgoing_incr(real, &abs, chosen)
                    .unwrap_or_else(|e| panic!("{action:?} outgoing {chosen:?}: {e}"));
                match chosen {
                    AbstractAction::Fold => assert_eq!(incr, "f"),
                    AbstractAction::Check => assert_eq!(incr, "k"),
                    AbstractAction::Call { .. } => assert_eq!(incr, "c"),
                    AbstractAction::Bet { .. }
                    | AbstractAction::Raise { .. }
                    | AbstractAction::AllIn { .. } => {
                        let to: u64 = incr
                            .strip_prefix('b')
                            .and_then(|d| d.parse().ok())
                            .unwrap_or_else(|| panic!("{action:?}: bad bet incr {incr:?}"));
                        let cap = all_in_to.expect("aggression 时应有 all_in_amount");
                        assert!(to <= cap, "{action:?} {chosen:?}: to={to} > all_in {cap}");
                        if let AbstractAction::AllIn { .. } = chosen {
                            assert_eq!(to, cap, "{action:?}: AllIn 应出 all_in to");
                        } else if let Some(min_to) = min_aggro_to {
                            assert!(
                                to >= min_to,
                                "{action:?} {chosen:?}: to={to} < min {min_to}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// T4b（回归：vultr 实打捕获的 Illegal bet 根因）：面对对手 all-in（覆盖我方），
    /// 我方 abstract「AllIn」其实是 all-in 跟注（to == 对手下注水位，增量 0），outgoing
    /// 必须发 `c` 而非 `b<to>`（否则 Slumbot 判 Illegal bet）。
    #[test]
    fn t4b_call_of_all_in_emits_c() {
        let game = stub_game();
        let abs = default_abstraction();
        // SB all-in（b20000）→ BB 面对 all-in，等额 200bb 下 BB 的 call/all-in 同额。
        let ctx = replay_str(&game, &abs, "b20000").expect("replay b20000");
        assert_eq!(
            ctx.real.current_player(),
            Some(SeatId(1)),
            "b20000 后应轮 BB(SeatId1) 动"
        );
        let legal = SimplifiedNlheGame::legal_actions(&ctx.abs);
        // 等额栈 all-in：Call 被折进 AllIn 槽，合法集应是 {Fold, AllIn}。
        let all_in = legal
            .iter()
            .copied()
            .find(|a| matches!(a, AbstractAction::AllIn { .. }))
            .expect("面对 all-in 应有 AllIn 抽象动作");
        assert_eq!(
            outgoing_incr(&ctx.real, &abs, all_in).unwrap(),
            "c",
            "面对对手 all-in，AllIn 即跟注，必须发 c 不是 b<to>"
        );
        let fold = legal
            .iter()
            .copied()
            .find(|a| matches!(a, AbstractAction::Fold))
            .expect("面对 all-in 应有 Fold");
        assert_eq!(outgoing_incr(&ctx.real, &abs, fold).unwrap(), "f");
    }

    /// T5：端到端 smoke（无网络）。canned 手局（含空串/我先动、含 `/`、含 all-in、
    /// 各街）喂 decide + 均匀策略 → 无 panic、出合法 incr token。board 长度不符 →
    /// 干净 Err（不 panic）。
    #[test]
    fn t5_decide_smoke() {
        let game = stub_game();
        let abs = default_abstraction();
        let uniform = |_info: &InfoSetId, n: usize| vec![1.0 / n as f64; n];

        // (hole, board, client_pos, action)
        let cases: [(&[&str], &[&str], u8, &str); 6] = [
            (&["Ac", "Kd"], &[], 1, ""),                      // 我=SB，preflop 先动
            (&["Ac", "Kd"], &[], 0, "b300"),                  // 我=BB，面对 SB raise
            (&["Ac", "Kd"], &["7h", "2c", "Ks"], 0, "b300c"), // flop，我=BB 先动
            (&["Ac", "Kd"], &["7h", "2c", "Ks", "9d"], 0, "b300c/kk"), // turn，我=BB
            (
                &["Ac", "Kd"],
                &["7h", "2c", "Ks", "9d", "3s"],
                0,
                "b300c/kk/kk",
            ), // river，我=BB
            (&["Ac", "Kd"], &[], 0, "b20000"),                // 面对 all-in
        ];
        for (hole, board, client_pos, action) in cases {
            let req = Request {
                hole_cards: hole.iter().map(|s| s.to_string()).collect(),
                board: board.iter().map(|s| s.to_string()).collect(),
                client_pos,
                action: action.to_string(),
            };
            let incr = decide(&game, &abs, &req, &uniform, 0xC0FFEE)
                .unwrap_or_else(|e| panic!("decide({action:?}, pos={client_pos}) 失败: {e}"));
            let ok = incr == "f"
                || incr == "k"
                || incr == "c"
                || (incr.starts_with('b') && incr[1..].parse::<u64>().is_ok());
            assert!(ok, "decide({action:?}) 出非法 incr {incr:?}");
        }

        // board 长度与街不符 → 干净 Err（不 panic）。
        let bad = Request {
            hole_cards: vec!["Ac".into(), "Kd".into()],
            board: vec![], // flop 决策却没给 board
            client_pos: 0,
            action: "b300c".into(),
        };
        assert!(decide(&game, &abs, &bad, &uniform, 0).is_err());

        // 确定性：同输入同输出。
        let req = Request {
            hole_cards: vec!["Ac".into(), "Kd".into()],
            board: vec![],
            client_pos: 1,
            action: "".into(),
        };
        let a = decide(&game, &abs, &req, &uniform, 42).unwrap();
        let b = decide(&game, &abs, &req, &uniform, 42).unwrap();
        assert_eq!(a, b, "同 seed 同输入应确定性出同 incr");
    }
}
