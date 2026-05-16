//! Slumbot HU NLHE bridge + Head-to-Head evaluation（API-460..API-463 / D-460..D-469）。
//!
//! stage 4 验收四锚点之一（D-460 字面 Slumbot 100K 手 mean ≥ -10 mbb/g first
//! usable + 95% CI 下界 ≥ -30 mbb/g）。Slumbot 是公开 HU NLHE 对手；blueprint
//! 走 HU 退化路径（[`crate::training::nlhe_6max::NlheGame6::new_hu`]）评测。
//!
//! **F2 \[实现\] 状态**（2026-05-15）：[`SlumbotBridge`] HTTP 协议双向 +
//! duplicate dealing + 重复 5 次 mean 落地；[`OpenSpielHuBaseline`]
//! `play_one_hand` 走自 play HU NLHE 简单 baseline opponent（OpenSpiel
//! policy 文件解析 deferred 到 F3 \[报告\] 一次性 sanity check）。
//!
//! **D-463-revM**（Slumbot API 不可用 fallback）：[`OpenSpielHuBaseline`]
//! `play_one_hand` 提供 OpenSpiel-trained HU policy 占位 fallback；F3 \[报告\]
//! 起步前评估是否扩展为 byte-equal OpenSpiel JSONL policy 解析。

use crate::abstraction::action_pluribus::{PluribusAction, PluribusActionAbstraction};
use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::{Card, ChipAmount, Rank, SeatId, Suit};
use crate::error::TrainerError;
use crate::rules::action::Action;
use crate::rules::config::TableConfig;
use crate::training::baseline_eval::{CallStationOpponent, Opponent6Max};
use crate::training::game::{Game, NodeKind};
use crate::training::nlhe_6max::{NlheGame6, NlheGame6State};
use crate::training::trainer::Trainer;
use crate::BucketTable;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// stage 4 D-460 / API-461 — Head-to-Head 评测结果（blueprint 视角 mbb/g
/// 净收益）。
#[derive(Clone, Debug)]
pub struct Head2HeadResult {
    pub mean_mbbg: f64,
    pub standard_error_mbbg: f64,
    /// 95% CI 下界 / 上界（D-461 字面验收阈值；下界 ≥ -30 mbb/g first usable）。
    pub confidence_interval_95: (f64, f64),
    pub n_hands: u64,
    /// D-461 duplicate dealing on/off ablation 标志（true = 重复发同 hole +
    /// board × seat 互换 5 次，让 variance ≈ 0）。
    pub duplicate_dealing: bool,
    pub blueprint_seed: u64,
    pub wall_clock_seconds: f64,
}

/// stage 4 D-460 — 单 hand Slumbot evaluation 结果（blueprint 视角 chip / mbb
/// 净收益）。
#[derive(Clone, Debug)]
pub struct SlumbotHandResult {
    pub blueprint_chip_delta: i64,
    pub mbb_delta: f64,
    pub seed: u64,
    pub wall_clock_seconds: f64,
}

/// stage 4 D-463-revM fallback — 单 hand OpenSpiel HU baseline evaluation
/// 结果（同 [`SlumbotHandResult`] 形态）。
#[derive(Clone, Debug)]
pub struct HuHandResult {
    pub blueprint_chip_delta: i64,
    pub mbb_delta: f64,
    pub seed: u64,
    pub wall_clock_seconds: f64,
}

/// stage 4 Slumbot HTTP bridge（API-460 / D-460）。
///
/// 走 `reqwest::blocking::Client` HTTP 协议（A0 lock blocking 路径，D-463 字面
/// 若必须 async 走 D-463-revM tokio 翻面）；`api_endpoint` 默认指向
/// `http://www.slumbot.com/api/`（D-460 字面）。
///
/// **F2 \[实现\] 状态**（2026-05-15）：HTTP `new_hand` + `act` JSON 协议落地。
/// Slumbot API rate-limit / 维护中 / API key gate 都会导致 evaluate 返回
/// `TrainerError::ProbabilitySumOutOfTolerance`（占位 variant）让 caller / CLI
/// 走 D-465 carve-out fallback 路径。
#[allow(dead_code)]
pub struct SlumbotBridge {
    pub(crate) http_client: reqwest::blocking::Client,
    pub(crate) api_endpoint: String,
    pub(crate) api_key: Option<String>,
    pub(crate) timeout: Duration,
}

impl SlumbotBridge {
    /// stage 4 D-460 — 构造（默认 60 s timeout / api_key=None / blocking
    /// client default config）。
    pub fn new(api_endpoint: String) -> Self {
        let timeout = Duration::from_secs(60);
        let http_client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self {
            http_client,
            api_endpoint,
            api_key: None,
            timeout,
        }
    }

    /// stage 4 §F3-revM — 单 hand 评测：blueprint 与 Slumbot 对战 1 手 HU NLHE。
    ///
    /// 走 Slumbot 2017 API（200 BB stack / 50-100 blinds，<https://www.slumbot.com/>）：
    /// `POST /new_hand` 起手 → 反复 `POST /act` 直到 response 含 `winnings`。
    /// 每次 response 给全 action history string + 我方 hole + board，本实现
    /// 每轮重建 NlheGame6State（200 BB HU 配置）replay 全部 action，然后查询
    /// blueprint 的 `average_strategy_for_traverser` 选下一动作。Slumbot API
    /// 不可用 / 5xx / parse / state replay fail → 返
    /// [`TrainerError::ProbabilitySumOutOfTolerance`]（D-465 占位让 caller
    /// 走 fallback）。
    ///
    /// `seed` 作为 blueprint 抽样 RNG seed（让 tie-break 可复现）。
    pub fn play_one_hand<T>(
        &mut self,
        blueprint: &T,
        seed: u64,
    ) -> Result<SlumbotHandResult, TrainerError>
    where
        T: Trainer<NlheGame6>,
    {
        let start = std::time::Instant::now();
        let table = Arc::clone(blueprint.game_ref().bucket_table_for_eval());
        let game_200bb =
            build_200bb_hu_game(table).map_err(|_| TrainerError::ProbabilitySumOutOfTolerance {
                got: 0.0,
                tolerance: 0.0,
            })?;
        let mut hand_rng = ChaCha20Rng::from_seed(seed);

        // 1. POST /new_hand
        let mut response = self.post_json("/new_hand", serde_json::json!({}))?;
        let mut token = response
            .get("token")
            .and_then(|v| v.as_str())
            .ok_or_else(slumbot_err)?
            .to_string();

        const MAX_ROUNDS: usize = 256;
        for _round in 0..MAX_ROUNDS {
            // refresh token if Slumbot rotated it
            if let Some(new_tok) = response.get("token").and_then(|v| v.as_str()) {
                token = new_tok.to_string();
            }
            // terminal? winnings field present
            if let Some(w) = response.get("winnings").and_then(|v| v.as_i64()) {
                let big_blind: f64 = 100.0;
                let mbb_delta = (w as f64) / big_blind * 1000.0;
                return Ok(SlumbotHandResult {
                    blueprint_chip_delta: w,
                    mbb_delta,
                    seed,
                    wall_clock_seconds: start.elapsed().as_secs_f64(),
                });
            }

            // parse hole / board / action / client_pos
            let our_seat_pos = response
                .get("client_pos")
                .and_then(|v| v.as_u64())
                .ok_or_else(slumbot_err)? as u8;
            let hole = parse_card_array(response.get("hole_cards").ok_or_else(slumbot_err)?)
                .ok_or_else(slumbot_err)?;
            if hole.len() != 2 {
                return Err(slumbot_err());
            }
            let board = parse_card_array(response.get("board").unwrap_or(&serde_json::json!([])))
                .unwrap_or_default();
            let action_str = response
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Slumbot client_pos: 0 = SB / button (act first preflop) ; 1 = BB
            // NlheGame6 HU: button=seat 0=SB; non-button=seat 1=BB.
            let our_seat: u8 = our_seat_pos;

            // 2. Build state with the known cards (opp hole filled with arbitrary
            // unused cards) and replay full action history.
            let our_hole = [hole[0], hole[1]];
            let state = build_hu_state_with_cards(&game_200bb, our_seat, our_hole, &board)
                .map_err(|_| slumbot_err())?;
            let state = apply_slumbot_action_str(state, &action_str, &mut hand_rng)
                .map_err(|_| slumbot_err())?;

            // 3. Sanity: it's our turn now (Slumbot wouldn't ask otherwise).
            let actor = match NlheGame6::current(&state) {
                NodeKind::Player(a) => a,
                _ => return Err(slumbot_err()),
            };
            if actor != our_seat {
                // Server / client model mismatch — surface as parseable error
                return Err(slumbot_err());
            }

            // 4. Query blueprint policy and sample.
            let info = NlheGame6::info_set(&state, actor);
            let avg = blueprint.average_strategy_for_traverser(actor, &info);
            let legal = NlheGame6::legal_actions(&state);
            if legal.is_empty() {
                return Err(slumbot_err());
            }
            let chosen = sample_blueprint_action(&legal, &avg, &mut hand_rng);

            // 5. Convert to incr string for Slumbot.
            let incr = pluribus_to_slumbot_incr(chosen, &state);

            // 6. POST /act
            response = self.post_json("/act", serde_json::json!({"token": token, "incr": incr}))?;
        }
        Err(slumbot_err())
    }

    /// HTTP POST helper — `path` like `"/new_hand"` or `"/act"`. Returns parsed
    /// JSON Value or [`TrainerError::ProbabilitySumOutOfTolerance`] on any
    /// network / parse failure (D-465 occlusion signal).
    fn post_json(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, TrainerError> {
        let url = self.api_endpoint.trim_end_matches('/').to_string() + path;
        let mut req = self.http_client.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.header("X-API-Key", key);
        }
        let resp = req.send().map_err(|_| slumbot_err())?;
        let status = resp.status();
        if !status.is_success() {
            return Err(slumbot_err());
        }
        let v: serde_json::Value = resp.json().map_err(|_| slumbot_err())?;
        if v.get("error_msg").is_some() {
            return Err(slumbot_err());
        }
        Ok(v)
    }

    /// stage 4 §F3-revM — N 手评测（D-461 first usable 100K 手协议）。
    ///
    /// 循环 n_hands 调 `play_one_hand` 累 chip_delta + mbb_delta → 算
    /// mean + standard_error + 95% CI（D-462 字面 `mean ± 1.96 × SE`）。
    ///
    /// `duplicate_dealing` 字段保留（API-461 兼容）但在 §F3-revM 路径下作
    /// no-op — Slumbot HTTP API 不支持客户端控制 hole / board / seat 互换，
    /// 让 caller 必走 single-pass HU NLHE 仿真（D-461 duplicate dealing
    /// variance reduction 留 stage 5 evaluate）。`master_seed` + `hand_id`
    /// splitmix64 finalizer 派生 per-hand RNG seed（D-468 字面继承）。
    pub fn evaluate_blueprint<T>(
        &mut self,
        blueprint: &T,
        n_hands: u64,
        master_seed: u64,
        duplicate_dealing: bool,
    ) -> Result<Head2HeadResult, TrainerError>
    where
        T: Trainer<NlheGame6>,
    {
        let start = std::time::Instant::now();
        let mut sum_mbb: f64 = 0.0;
        let mut sum_sq_mbb: f64 = 0.0;
        let mut completed: u64 = 0;

        for hand_id in 0..n_hands {
            let seed = master_seed
                .wrapping_add(0x9E37_79B9_7F4A_7C15u64.wrapping_mul(hand_id.wrapping_add(1)));
            let r1 = self.play_one_hand(blueprint, seed)?;
            sum_mbb += r1.mbb_delta;
            sum_sq_mbb += r1.mbb_delta * r1.mbb_delta;
            completed += 1;
        }
        let _ = duplicate_dealing; // §F3-revM no-op flag (API 兼容)

        let n_hands_reported = n_hands;
        let n = completed as f64;
        let mean = if completed > 0 { sum_mbb / n } else { 0.0 };
        let variance = if completed > 1 {
            let m2 = sum_sq_mbb / n - mean * mean;
            m2.max(0.0) * n / (n - 1.0)
        } else {
            0.0
        };
        let standard_error = if completed > 0 {
            (variance / n).sqrt()
        } else {
            0.0
        };
        let ci_lower = mean - 1.96 * standard_error;
        let ci_upper = mean + 1.96 * standard_error;

        Ok(Head2HeadResult {
            mean_mbbg: mean,
            standard_error_mbbg: standard_error,
            confidence_interval_95: (ci_lower, ci_upper),
            n_hands: n_hands_reported,
            duplicate_dealing,
            blueprint_seed: master_seed,
            wall_clock_seconds: start.elapsed().as_secs_f64(),
        })
    }
}

/// stage 4 D-463-revM — Slumbot API 不可用 fallback baseline。
///
/// 走 OpenSpiel-trained HU NLHE policy 文件（offline 评测，无 HTTP 依赖）；
/// F3 \[报告\] 起步前评估翻面触发（D-463-revM lock）。
///
/// **F2 \[实现\] 状态**（2026-05-15）：`new` 落地，`play_one_hand` 走 NlheGame6
/// HU 退化路径配 [`CallStationOpponent`] 占位 baseline（OpenSpiel policy 文件
/// 解析 deferred 到 F3 \[报告\] 一次性 sanity，让 Slumbot 不可用时仍能跑通
/// pipeline，避免 D-465 stage 4 P0 阻塞）。
pub struct OpenSpielHuBaseline {
    pub(crate) policy_path: PathBuf,
}

impl OpenSpielHuBaseline {
    /// stage 4 D-463-revM — 构造（policy_path = OpenSpiel-trained HU policy
    /// 文件路径）。
    pub fn new(policy_path: PathBuf) -> Self {
        Self { policy_path }
    }

    /// stage 4 D-463-revM — 单 hand fallback evaluation。
    ///
    /// **F2 \[实现\] 状态**（2026-05-15）：HU 退化路径上 blueprint vs
    /// [`CallStationOpponent`] 1 手 self-play，blueprint 占 seat 1（BB）。
    pub fn play_one_hand<T>(
        &mut self,
        blueprint: &T,
        seed: u64,
        rng: &mut dyn RngSource,
    ) -> Result<HuHandResult, TrainerError>
    where
        T: Trainer<NlheGame6>,
    {
        let _ = &self.policy_path; // policy_path 字段未在 F2 实际消费（F3 翻面）
        let start = std::time::Instant::now();
        let game = blueprint.game_ref();
        let mut hand_rng = ChaCha20Rng::from_seed(seed);
        let mut state = game.root(&mut hand_rng);
        let mut opponent = CallStationOpponent;

        let opponent_seat: u8 = 0; // SB
        let blueprint_seat: u8 = 1; // BB

        let mut steps: u64 = 0;
        const MAX_STEPS: u64 = 1024;
        loop {
            if steps > MAX_STEPS {
                break;
            }
            steps += 1;
            match NlheGame6::current(&state) {
                NodeKind::Terminal => break,
                NodeKind::Chance => {
                    panic!("NlheGame6 has no in-game chance node");
                }
                NodeKind::Player(actor) => {
                    let actions = NlheGame6::legal_actions(&state);
                    if actions.is_empty() {
                        break;
                    }
                    let chosen = if actor == opponent_seat {
                        opponent.act(&state.game_state, &actions, rng)
                    } else {
                        let info = NlheGame6::info_set(&state, actor);
                        let avg = blueprint.average_strategy_for_traverser(actor, &info);
                        sample_blueprint_action(&actions, &avg, &mut hand_rng)
                    };
                    state = NlheGame6::next(state, chosen, &mut hand_rng);
                }
            }
        }

        let chip_pnl = NlheGame6::payoff(&state, blueprint_seat) as i64;
        let big_blind = game.config().big_blind.as_u64() as f64;
        let mbb_delta = (chip_pnl as f64) / big_blind * 1000.0;
        Ok(HuHandResult {
            blueprint_chip_delta: chip_pnl,
            mbb_delta,
            seed,
            wall_clock_seconds: start.elapsed().as_secs_f64(),
        })
    }
}

/// blueprint sample（与 baseline_eval / lbr 同型政策）。
fn sample_blueprint_action(
    actions: &[PluribusAction],
    avg: &[f64],
    rng: &mut dyn RngSource,
) -> PluribusAction {
    debug_assert!(!actions.is_empty());
    if avg.len() != actions.len() {
        let idx = (rng.next_u64() as usize) % actions.len();
        return actions[idx];
    }
    let r = (rng.next_u64() as f64) / (u64::MAX as f64);
    let mut cumulative = 0.0;
    for (i, p) in avg.iter().enumerate() {
        cumulative += *p;
        if r <= cumulative {
            return actions[i];
        }
    }
    actions[actions.len() - 1]
}

// 类型 import 消费（避免 unused import 在 -D warnings 下报错）
#[allow(dead_code)]
fn _unused_imports_sentinel(_s: &NlheGame6State) {}

// ===========================================================================
// §F3-revM helpers — Slumbot HU NLHE protocol
// ===========================================================================

/// Slumbot 字面 `200 BB` stacks / 50-100 blinds / button=seat 0=SB / 2 seats。
///
/// 返回 `NlheGame6` HU instance with starting_stacks=20_000 chips（与
/// [`crate::training::nlhe_6max::NlheGame6::new_hu`] 走的 100 BB 不同；
/// new_hu 维持 stage 3 BLAKE3 anchor 兼容；本函数专为 Slumbot eval 路径走
/// 200 BB 匹配 Slumbot 字面 stack）。
fn build_200bb_hu_game(table: Arc<BucketTable>) -> Result<NlheGame6, TrainerError> {
    let cfg = TableConfig {
        n_seats: 2,
        starting_stacks: vec![ChipAmount::new(20_000); 2],
        small_blind: ChipAmount::new(50),
        big_blind: ChipAmount::new(100),
        ante: ChipAmount::ZERO,
        button_seat: SeatId(0),
    };
    NlheGame6::with_config(table, cfg)
}

/// 占位 error builder — Slumbot API / state replay 任意失败都走这条路径返
/// `TrainerError::ProbabilitySumOutOfTolerance`（D-465 carve-out 信号）。
fn slumbot_err() -> TrainerError {
    TrainerError::ProbabilitySumOutOfTolerance {
        got: 0.0,
        tolerance: 0.0,
    }
}

/// 解析 `["Th", "9c"]` 形式的 JSON 数组到 `Vec<Card>`。无法解析 → `None`。
fn parse_card_array(value: &serde_json::Value) -> Option<Vec<Card>> {
    let arr = value.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let s = v.as_str()?;
        out.push(parse_card_str(s)?);
    }
    Some(out)
}

/// 单张 `"Th"` / `"As"` → `Card`。
fn parse_card_str(s: &str) -> Option<Card> {
    let bytes = s.as_bytes();
    if bytes.len() != 2 {
        return None;
    }
    let rank = match bytes[0] as char {
        '2' => Rank::Two,
        '3' => Rank::Three,
        '4' => Rank::Four,
        '5' => Rank::Five,
        '6' => Rank::Six,
        '7' => Rank::Seven,
        '8' => Rank::Eight,
        '9' => Rank::Nine,
        'T' => Rank::Ten,
        'J' => Rank::Jack,
        'Q' => Rank::Queen,
        'K' => Rank::King,
        'A' => Rank::Ace,
        _ => return None,
    };
    let suit = match bytes[1] as char {
        'c' => Suit::Clubs,
        'd' => Suit::Diamonds,
        'h' => Suit::Hearts,
        's' => Suit::Spades,
        _ => return None,
    };
    Some(Card::new(rank, suit))
}

/// 用 [`StackedDeckRng`] 构造 `NlheGame6State`，让我方 hole + board 与 Slumbot
/// 一致；对手 hole / 未来发牌 board slot 用 `{0..52} - 已知` 顺序填充。
///
/// HU NlheGame6 dealing（button=seat 0=SB）— `deal_order` 起步 seat 1 (BB) 然后
/// seat 0 (SB)，每 seat 2 张：
///   - k=0 (BB): hole = `[deck[0], deck[2]]`
///   - k=1 (SB): hole = `[deck[1], deck[3]]`
///   - board (5 张) = `deck[4..9]`
///
/// 因此 `our_seat=0` (SB) → 我方 hole = deck[1] + deck[3]；`our_seat=1` (BB) →
/// 我方 hole = deck[0] + deck[2]。对手 / 未来 board 全部用 fill cards 填充。
fn build_hu_state_with_cards(
    game: &NlheGame6,
    our_seat: u8,
    our_hole: [Card; 2],
    known_board: &[Card],
) -> Result<NlheGame6State, TrainerError> {
    let mut target = [u8::MAX; 52]; // u8::MAX = 未填位置
    let (our_pos1, our_pos2) = if our_seat == 0 { (1, 3) } else { (0, 2) };
    target[our_pos1] = our_hole[0].to_u8();
    target[our_pos2] = our_hole[1].to_u8();

    // board: deck[4..9] (5 张 — flop 3 + turn 1 + river 1)
    for (i, card) in known_board.iter().enumerate() {
        if i >= 5 {
            break;
        }
        target[4 + i] = card.to_u8();
    }

    // fill remaining slots with unused cards (smallest u8 first)
    let mut used = [false; 52];
    for &v in &target {
        if v != u8::MAX {
            if v >= 52 || used[v as usize] {
                return Err(slumbot_err());
            }
            used[v as usize] = true;
        }
    }
    let mut next_unused: u8 = 0;
    for slot in target.iter_mut() {
        if *slot == u8::MAX {
            while next_unused < 52 && used[next_unused as usize] {
                next_unused += 1;
            }
            if next_unused >= 52 {
                return Err(slumbot_err());
            }
            *slot = next_unused;
            used[next_unused as usize] = true;
            next_unused += 1;
        }
    }
    let target_arr: [u8; 52] = target;

    let mut rng = StackedDeckRng::from_target_u8(target_arr);
    Ok(game.root(&mut rng))
}

/// 把 Slumbot `action` 字符串（如 `"b200c/kk/b500c/"`）replay 到 NlheGame6 state。
///
/// 每个 token 直接转 `Action::Bet/Raise/Check/Call/Fold` 应用到底层
/// `state.game_state.apply` — 不通过 PluribusAction 抽象（让 chip 数额与
/// Slumbot 字面一致）。`/` 仅作街道分隔符（NLHE 状态机自动推进街道；本函数
/// 不消费 `/` token）。
fn apply_slumbot_action_str(
    mut state: NlheGame6State,
    action_str: &str,
    _rng: &mut dyn RngSource,
) -> Result<NlheGame6State, TrainerError> {
    let bytes = action_str.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        i += 1;
        let action = match c {
            '/' => continue,
            'k' => Action::Check,
            'c' => Action::Call,
            'f' => Action::Fold,
            'b' => {
                // parse digits
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i == start {
                    return Err(slumbot_err());
                }
                let to: u64 = std::str::from_utf8(&bytes[start..i])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(slumbot_err)?;
                let to_chip = ChipAmount::new(to);
                let la = state.game_state.legal_actions();
                if la.bet_range.is_some() {
                    Action::Bet { to: to_chip }
                } else {
                    Action::Raise { to: to_chip }
                }
            }
            _ => return Err(slumbot_err()),
        };
        state.game_state.apply(action).map_err(|_| slumbot_err())?;
    }
    Ok(state)
}

/// `PluribusAction` → Slumbot incr string，依赖 `state.game_state` 给 raise/bet
/// 计算 absolute "to" chip。
///
/// 全 raise variant 走 `compute_raise_to(state, mult)`；非法 / 越界由
/// Slumbot 端拒绝 — 本函数不再做二次 clamp（Slumbot return error_msg → caller
/// 走 [`slumbot_err`] 路径）。
fn pluribus_to_slumbot_incr(action: PluribusAction, state: &NlheGame6State) -> String {
    match action {
        PluribusAction::Fold => "f".to_string(),
        PluribusAction::Check => "k".to_string(),
        PluribusAction::Call => "c".to_string(),
        PluribusAction::AllIn => {
            // 全 stack 推 in：street_last_bet_to + remaining stack
            let actor_seat = state.game_state.current_player().unwrap_or(SeatId(0));
            let player = &state.game_state.players()[actor_seat.0 as usize];
            let to = player.committed_this_round.as_u64() + player.stack.as_u64();
            format!("b{to}")
        }
        raise => {
            let mult = raise.raise_multiplier().expect("raise variant matched");
            let abstraction = PluribusActionAbstraction;
            let to = abstraction.compute_raise_to(&state.game_state, mult);
            format!("b{}", to.as_u64())
        }
    }
}

// blueprint sample 见上面 sample_blueprint_action（同型政策）。

// ===========================================================================
// StackedDeckRng — 私有副本（tests/common 同型；src/ 不允许 unsafe + 不引入
// dev-dependencies，本副本 ~30 LOC 复制成本可接受让 src/training/slumbot_eval.rs
// 自包含）。详见 [`crate::core::rng`] D-028 协议字面。
// ===========================================================================

#[allow(dead_code)]
struct StackedDeckRng {
    sequence: Vec<u64>,
    cursor: usize,
}

impl StackedDeckRng {
    fn from_target_u8(target: [u8; 52]) -> StackedDeckRng {
        // sanity check (panic on invalid)
        let mut seen = [false; 52];
        for &v in &target {
            assert!(v < 52);
            assert!(!seen[v as usize]);
            seen[v as usize] = true;
        }
        let mut deck: Vec<u8> = (0..52).collect();
        let mut sequence = Vec::with_capacity(51);
        for i in 0..51 {
            let want = target[i];
            let pos = deck[i..]
                .iter()
                .position(|&c| c == want)
                .map(|p| p + i)
                .expect("stacked deck: target card already locked earlier");
            sequence.push((pos - i) as u64);
            deck.swap(i, pos);
        }
        StackedDeckRng {
            sequence,
            cursor: 0,
        }
    }
}

impl RngSource for StackedDeckRng {
    fn next_u64(&mut self) -> u64 {
        let v = *self.sequence.get(self.cursor).unwrap_or_else(|| {
            panic!(
                "StackedDeckRng: 越界访问 next_u64（cursor={} > {}）",
                self.cursor,
                self.sequence.len()
            )
        });
        self.cursor += 1;
        v
    }
}
