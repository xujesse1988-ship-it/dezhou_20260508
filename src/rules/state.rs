//! 游戏状态机（API §4）。

use std::collections::BTreeSet;

use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::{Card, ChipAmount, Player, PlayerStatus, SeatId, Street};
use crate::error::RuleError;
use crate::eval;
use crate::history::{HandHistory, RecordedAction};
use crate::rules::action::{Action, LegalActionSet};
use crate::rules::config::TableConfig;

/// NLHE 6-max 状态机。
///
/// 内部字段不公开（API §4）。**关键不变量**（实现 agent 必须保证、测试 agent
/// 应在 invariant suite 中验证；详见 `docs/pluribus_stage1_api.md` §4）：
///
/// - **I-001** 任意时刻 `sum(player.stack) + pot() = sum(starting_stacks)`。
///   `TableConfig.starting_stacks` 是发盲注 / ante **之前** 的座位栈（D-024）；
///   盲注 / ante 是 stack→pot 的转移，总量守恒。
/// - **I-002** 任意 `Player.stack >= 0`（用 `u64` 表达自然成立，但减法路径
///   必须有下溢检查，见 [`ChipAmount`] D-026b 的 panic 语义）。
/// - **I-003** 任意一手内不出现重复 `Card`。
/// - **I-004** 每个 betting round 结束时，所有 `Active` 状态玩家的
///   `committed_this_round` 相等。
/// - **I-005** `apply` 失败时 `GameState` 不变（错误返回为纯函数式语义）。
/// - **I-006** 全员 all-in（除 ≤ 1 名 `Active` 外）后 `current_player == None`；
///   状态机会在同一 `apply` 调用内连续发完剩余公共牌、切 `Showdown`、设
///   `is_terminal == true`（多街快进，D-036）。
/// - **I-007** 终局必有获胜者（pot 必有归属）。
#[derive(Clone, Debug)]
pub struct GameState {
    config: TableConfig,
    players: Vec<Player>,
    street: Street,
    board: Vec<Card>,
    runout_board: [Card; 5],
    current_player: Option<SeatId>,
    terminal: bool,
    final_payouts: Option<Vec<(SeatId, i64)>>,
    history: HandHistory,
    raise_option_open: Vec<bool>,
    last_full_raise_size: ChipAmount,
    last_aggressor: Option<SeatId>,
}

impl GameState {
    /// 初始化一手新牌（生产路径）。
    ///
    /// 内部以 `ChaCha20Rng::from_seed(seed)` 构造 rng，按 D-028 发牌协议抽牌、
    /// 布盲、按钮位由 `config` 指定。`HandHistory.seed` 自动记为该 `seed`，
    /// `replay()` 即可复现。
    pub fn new(config: &TableConfig, seed: u64) -> GameState {
        let mut rng = ChaCha20Rng::from_seed(seed);
        GameState::with_rng(config, seed, &mut rng)
    }

    /// 初始化一手新牌（测试 / fuzz 路径）。
    ///
    /// 注入自定义 [`RngSource`]，典型用于 stacked deck（构造指定牌序，参见 D-028）。
    /// `seed` 仅作为 `HandHistory.seed` 的标签写入，**不参与发牌**；调用方需自负
    /// rng 与 seed 的语义一致性 —— 若期望 `replay()` 能复现，则注入的 rng 必须等价于
    /// `ChaCha20Rng::from_seed(seed)`，否则 `replay()` 在底牌 / 公共牌校验阶段
    /// 会返回 [`HistoryError::ReplayDiverged`].
    ///
    /// **B1 推荐用法**：fuzz / 单元测试中使用 stacked rng + 固定 sentinel seed
    /// （如 `0`），并不要求 `replay()` 复现 —— stacked rng 用于构造指定牌序的
    /// fixed scenario，与 `replay()` 复现是两个独立用途。
    ///
    /// [`HistoryError::ReplayDiverged`]: crate::error::HistoryError::ReplayDiverged
    pub fn with_rng(config: &TableConfig, seed: u64, rng: &mut dyn RngSource) -> GameState {
        validate_config(config);

        let n = config.n_seats as usize;
        let mut deck = [Card::from_u8(0).expect("0 is a valid card"); 52];
        for (i, slot) in deck.iter_mut().enumerate() {
            *slot = Card::from_u8(i as u8).expect("0..52 are valid cards");
        }
        for i in 0..51 {
            let j = i + (rng.next_u64() % ((52 - i) as u64)) as usize;
            deck.swap(i, j);
        }

        let mut players = Vec::with_capacity(n);
        for seat in 0..n {
            players.push(Player {
                seat: SeatId(seat as u8),
                stack: config.starting_stacks[seat],
                committed_this_round: ChipAmount::ZERO,
                committed_total: ChipAmount::ZERO,
                hole_cards: None,
                status: PlayerStatus::Active,
            });
        }

        let deal_order = seat_order(config.button_seat, n, 1);
        let mut history_holes = vec![None; n];
        for (k, seat) in deal_order.iter().enumerate() {
            let idx = seat.0 as usize;
            let hole = [deck[k], deck[n + k]];
            players[idx].hole_cards = Some(hole);
            history_holes[idx] = Some(hole);
        }

        let runout_board = [
            deck[2 * n],
            deck[2 * n + 1],
            deck[2 * n + 2],
            deck[2 * n + 3],
            deck[2 * n + 4],
        ];

        let mut state = GameState {
            config: config.clone(),
            players,
            street: Street::Preflop,
            board: Vec::with_capacity(5),
            runout_board,
            current_player: None,
            terminal: false,
            final_payouts: None,
            history: HandHistory {
                schema_version: 1,
                config: config.clone(),
                seed,
                // 6-max NLHE 单手实测分布的 99 分位 < 32 actions（E1 / D1 1M
                // fuzz 数据），预分配避免 simulate 热路径上的多次 Vec realloc。
                actions: Vec::with_capacity(32),
                board: Vec::with_capacity(5),
                hole_cards: history_holes,
                final_payouts: Vec::new(),
                showdown_order: Vec::new(),
            },
            raise_option_open: vec![true; n],
            last_full_raise_size: config.big_blind,
            last_aggressor: None,
        };

        state.post_forced_bets();
        for idx in 0..state.players.len() {
            state.raise_option_open[idx] = state.players[idx].status == PlayerStatus::Active;
        }
        let first = state.next_seat(state.big_blind_seat());
        state.current_player = state.next_player_needing_action_from(first);
        state
    }

    /// 当前要行动的玩家。手牌结束 / 全员 all-in 跳轮时返回 `None`。
    pub fn current_player(&self) -> Option<SeatId> {
        self.current_player
    }

    /// 当前合法动作集合。无玩家行动时返回"空集合"（所有字段 false / None，LA-008）。
    pub fn legal_actions(&self) -> LegalActionSet {
        let Some(seat) = self.current_player else {
            return empty_legal_actions();
        };
        if self.terminal {
            return empty_legal_actions();
        }
        let idx = seat.0 as usize;
        let player = &self.players[idx];
        if player.status != PlayerStatus::Active {
            return empty_legal_actions();
        }

        let max_committed = self.max_committed_this_round();
        let cap = player.committed_this_round + player.stack;
        let check = player.committed_this_round == max_committed;
        let call = if player.committed_this_round < max_committed {
            Some(std::cmp::min(max_committed, cap))
        } else {
            None
        };

        let bet_range = if max_committed == ChipAmount::ZERO && player.stack > ChipAmount::ZERO {
            if cap >= self.config.big_blind {
                Some((self.config.big_blind, cap))
            } else {
                None
            }
        } else {
            None
        };

        let raise_range = if max_committed > ChipAmount::ZERO
            && self.raise_option_open[idx]
            && cap > max_committed
        {
            let min_to = max_committed + self.last_full_raise_size;
            if cap >= min_to {
                Some((min_to, cap))
            } else {
                None
            }
        } else {
            None
        };

        LegalActionSet {
            fold: true,
            check,
            call,
            bet_range,
            raise_range,
            all_in_amount: (player.stack > ChipAmount::ZERO).then_some(cap),
        }
    }

    /// 应用一个动作。失败时返回错误，状态不改变（I-005）。
    ///
    /// **E2 注**：旧实现先 `self.clone()` 再 `apply_inner`，以「克隆-提交」给
    /// I-005 兜底；克隆每手 GameState（含 `HandHistory.actions`、`config`、
    /// `players`、`hole_cards`、`raise_option_open` 等约 5–10 个 Vec / 长度
    /// 与 n_seats 等比的字段）在 simulate 热路径上会扣掉 ~50% 吞吐。E2 把
    /// `apply_inner` 改为「严格先校验、后变更」原子语义后克隆失去用途——
    /// 任何返回 `Err` 的子路径都在 mutation 之前 early-return，状态不变。
    /// 详见 `docs/pluribus_stage1_workflow.md` §修订历史 E-rev1。
    pub fn apply(&mut self, action: Action) -> Result<(), RuleError> {
        self.apply_inner(action)
    }

    pub fn street(&self) -> Street {
        self.street
    }

    /// 当前桌面公共牌（Flop=3, Turn=4, River=5）。
    pub fn board(&self) -> &[Card] {
        &self.board
    }

    /// 当前总 pot（含主池 + 所有 side pot）。
    pub fn pot(&self) -> ChipAmount {
        ChipAmount::new(
            self.players
                .iter()
                .map(|p| p.committed_total.as_u64())
                .sum(),
        )
    }

    /// 当前所有玩家状态快照（按 `SeatId` 排序）。
    pub fn players(&self) -> &[Player] {
        &self.players
    }

    /// 牌局是否结束（已 showdown 或全员弃牌）。
    pub fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// 终局每个玩家的净收益（正 = 赢、负 = 输）。仅 `is_terminal` 后有效。
    pub fn payouts(&self) -> Option<Vec<(SeatId, i64)>> {
        self.final_payouts.clone()
    }

    /// 当前 hand history 的引用，可随时序列化或回放。
    pub fn hand_history(&self) -> &HandHistory {
        &self.history
    }

    /// 当前 `TableConfig` 的只读引用（API-NNN-rev1，stage 2 D-211-rev1 需要
    /// `TableConfig::initial_stack(seat)` 计算 `stack_bucket`）。
    ///
    /// 引入动机：stage 2 `InfoAbstraction::map` 实现需要按 D-211-rev1 钉死
    /// `stack_bucket` 来源到 `TableConfig::initial_stack(seat) / big_blind`，
    /// 不允许从 `state.player(seat).stack`（当前剩余筹码，已被盲注 / call /
    /// raise 扣减）反推。该 getter 让 stage 2 抽象层无需克隆 `TableConfig`
    /// 即可读取起手筹码 / 盲注 / 按钮位等不变量。
    ///
    /// 与 `hand_history().config` 等价（`HandHistory.config` 为 `TableConfig`
    /// 的克隆，用于 replay）；本 getter 直接借用 `GameState` 内部字段，
    /// 避免热路径上的克隆开销。
    pub fn config(&self) -> &TableConfig {
        &self.config
    }

    fn apply_inner(&mut self, action: Action) -> Result<(), RuleError> {
        if self.terminal {
            return Err(RuleError::HandTerminated);
        }
        let seat = self.current_player.ok_or(RuleError::HandTerminated)?;
        let idx = seat.0 as usize;
        if self.players[idx].status != PlayerStatus::Active {
            return Err(RuleError::NotPlayerTurn);
        }

        let street = self.street;
        let (normalized, committed_after) = self.apply_action_for_player(idx, action)?;
        self.record_action(seat, street, normalized, committed_after);
        self.advance_after_action(seat);
        Ok(())
    }

    fn apply_action_for_player(
        &mut self,
        idx: usize,
        action: Action,
    ) -> Result<(Action, ChipAmount), RuleError> {
        match action {
            Action::Fold => {
                let committed = self.players[idx].committed_this_round;
                self.players[idx].status = PlayerStatus::Folded;
                self.players[idx].hole_cards = None;
                self.raise_option_open[idx] = false;
                Ok((Action::Fold, committed))
            }
            Action::Check => self.apply_check(idx),
            Action::Call => self.apply_call(idx),
            Action::Bet { to } => self.apply_bet(idx, to),
            Action::Raise { to } => self.apply_raise(idx, to),
            Action::AllIn => self.apply_all_in(idx),
        }
    }

    fn apply_check(&mut self, idx: usize) -> Result<(Action, ChipAmount), RuleError> {
        let player = &self.players[idx];
        if player.committed_this_round != self.max_committed_this_round() {
            return Err(RuleError::WrongActionForState {
                action: Action::Check,
                reason: "call required",
            });
        }
        let committed = player.committed_this_round;
        self.raise_option_open[idx] = false;
        Ok((Action::Check, committed))
    }

    fn apply_call(&mut self, idx: usize) -> Result<(Action, ChipAmount), RuleError> {
        let max_committed = self.max_committed_this_round();
        let player = &self.players[idx];
        if player.committed_this_round >= max_committed {
            return Err(RuleError::WrongActionForState {
                action: Action::Call,
                reason: "nothing to call",
            });
        }
        let cap = player.committed_this_round + player.stack;
        let to = std::cmp::min(max_committed, cap);
        self.move_chips_to(idx, to)?;
        self.raise_option_open[idx] = false;
        Ok((Action::Call, to))
    }

    fn apply_bet(&mut self, idx: usize, to: ChipAmount) -> Result<(Action, ChipAmount), RuleError> {
        if self.max_committed_this_round() != ChipAmount::ZERO {
            return Err(RuleError::WrongActionForState {
                action: Action::Bet { to },
                reason: "bet already exists",
            });
        }
        self.validate_to_amount(idx, to)?;
        let cap = self.players[idx].committed_this_round + self.players[idx].stack;
        if to < self.config.big_blind && to != cap {
            return Err(RuleError::MinRaiseViolation {
                required: self.config.big_blind,
                got: to,
            });
        }
        self.move_chips_to(idx, to)?;
        if to >= self.config.big_blind {
            // Full bet: open raise option for un-acted players, close own.
            self.last_full_raise_size = to;
            self.open_raise_options_after_full_raise(idx, to);
            self.raise_option_open[idx] = false;
        }
        // Incomplete short all-in bet (to < big_blind && to == cap): per D-033-rev1
        // #4(b) leave every player's raise_option_open flag untouched (the bettor
        // is unconditionally going AllIn so the flag is dormant either way).
        self.last_aggressor = Some(self.players[idx].seat);
        Ok((Action::Bet { to }, to))
    }

    fn apply_raise(
        &mut self,
        idx: usize,
        to: ChipAmount,
    ) -> Result<(Action, ChipAmount), RuleError> {
        let old_max = self.max_committed_this_round();
        if old_max == ChipAmount::ZERO {
            return Err(RuleError::WrongActionForState {
                action: Action::Raise { to },
                reason: "no bet to raise",
            });
        }
        if to <= old_max {
            return Err(RuleError::InvalidAmount(to));
        }
        if !self.raise_option_open[idx] {
            return Err(RuleError::RaiseOptionNotReopened);
        }
        self.validate_to_amount(idx, to)?;

        let cap = self.players[idx].committed_this_round + self.players[idx].stack;
        let min_to = old_max + self.last_full_raise_size;
        if to < min_to && to != cap {
            return Err(RuleError::MinRaiseViolation {
                required: min_to,
                got: to,
            });
        }

        let raise_size = to - old_max;
        let full_raise = raise_size >= self.last_full_raise_size;
        self.move_chips_to(idx, to)?;
        if full_raise {
            // Full raise: open raise option for un-acted players, close own.
            self.last_full_raise_size = raise_size;
            self.open_raise_options_after_full_raise(idx, to);
            self.raise_option_open[idx] = false;
        }
        // Incomplete raise (`to < min_to && to == cap`): per D-033-rev1 #4(b)
        // leave every player's raise_option_open flag untouched, including the
        // raiser's own. The raiser is unconditionally going AllIn here so the
        // flag is dormant; preserving it keeps the spec contract crisp for
        // future status-state extensions (sit-out / cap state in C1+).
        self.last_aggressor = Some(self.players[idx].seat);
        Ok((Action::Raise { to }, to))
    }

    fn apply_all_in(&mut self, idx: usize) -> Result<(Action, ChipAmount), RuleError> {
        if self.players[idx].stack == ChipAmount::ZERO {
            return Err(RuleError::InsufficientStack);
        }
        let max_committed = self.max_committed_this_round();
        let cap = self.players[idx].committed_this_round + self.players[idx].stack;
        if max_committed == ChipAmount::ZERO {
            self.apply_bet(idx, cap)
        } else if cap <= max_committed {
            self.move_chips_to(idx, cap)?;
            self.raise_option_open[idx] = false;
            Ok((Action::Call, cap))
        } else {
            self.apply_raise(idx, cap)
        }
    }

    fn validate_to_amount(&self, idx: usize, to: ChipAmount) -> Result<(), RuleError> {
        let player = &self.players[idx];
        if to <= player.committed_this_round {
            return Err(RuleError::InvalidAmount(to));
        }
        let cap = player.committed_this_round + player.stack;
        if to > cap {
            return Err(RuleError::InvalidAmount(to));
        }
        Ok(())
    }

    fn move_chips_to(&mut self, idx: usize, to: ChipAmount) -> Result<(), RuleError> {
        self.validate_to_amount(idx, to)?;
        let delta = to - self.players[idx].committed_this_round;
        if delta > self.players[idx].stack {
            return Err(RuleError::InsufficientStack);
        }
        self.players[idx].stack -= delta;
        self.players[idx].committed_this_round = to;
        self.players[idx].committed_total += delta;
        if self.players[idx].stack == ChipAmount::ZERO {
            self.players[idx].status = PlayerStatus::AllIn;
        }
        Ok(())
    }

    fn record_action(
        &mut self,
        seat: SeatId,
        street: Street,
        action: Action,
        committed_after: ChipAmount,
    ) {
        let seq = self.history.actions.len() as u32;
        self.history.actions.push(RecordedAction {
            seq,
            seat,
            street,
            action,
            committed_after,
        });
    }

    fn advance_after_action(&mut self, acted: SeatId) {
        if self.finish_if_only_one_player_remains() {
            return;
        }
        if self.should_run_out_all_in() {
            self.run_out_to_showdown();
            return;
        }
        if self.round_complete() {
            self.finish_betting_round();
            return;
        }
        self.current_player = self.next_player_needing_action_from(self.next_seat(acted));
    }

    fn finish_if_only_one_player_remains(&mut self) -> bool {
        let live: Vec<usize> = self
            .players
            .iter()
            .enumerate()
            .filter(|(_, p)| p.status != PlayerStatus::Folded)
            .map(|(idx, _)| idx)
            .collect();
        if live.len() != 1 {
            return false;
        }
        self.return_uncalled_bets();
        self.finalize_terminal(false);
        true
    }

    fn should_run_out_all_in(&self) -> bool {
        let live_count = self
            .players
            .iter()
            .filter(|p| p.status != PlayerStatus::Folded)
            .count();
        if live_count <= 1 {
            return false;
        }
        let active: Vec<&Player> = self
            .players
            .iter()
            .filter(|p| p.status == PlayerStatus::Active)
            .collect();
        if active.len() > 1 {
            return false;
        }
        let max_committed = self.max_committed_this_round();
        active
            .iter()
            .all(|p| p.committed_this_round >= max_committed)
    }

    fn run_out_to_showdown(&mut self) {
        self.deal_board_to(5);
        self.street = Street::Showdown;
        self.return_uncalled_bets();
        self.finalize_terminal(true);
    }

    fn finish_betting_round(&mut self) {
        match self.street {
            Street::Preflop => {
                self.reset_round_for_next_street();
                self.deal_board_to(3);
                self.street = Street::Flop;
                self.current_player = self.first_active_from(self.small_blind_seat());
            }
            Street::Flop => {
                self.reset_round_for_next_street();
                self.deal_board_to(4);
                self.street = Street::Turn;
                self.current_player = self.first_active_from(self.small_blind_seat());
            }
            Street::Turn => {
                self.reset_round_for_next_street();
                self.deal_board_to(5);
                self.street = Street::River;
                self.current_player = self.first_active_from(self.small_blind_seat());
            }
            Street::River => {
                self.reset_committed_this_round();
                self.street = Street::Showdown;
                self.return_uncalled_bets();
                self.finalize_terminal(true);
            }
            Street::Showdown => {
                self.finalize_terminal(true);
            }
        }
    }

    fn finalize_terminal(&mut self, showdown: bool) {
        self.current_player = None;
        self.terminal = true;
        self.history.board = self.board.clone();
        self.history.showdown_order = if showdown {
            self.compute_showdown_order()
        } else {
            Vec::new()
        };
        let payouts = self.compute_payouts();
        self.history.final_payouts = payouts.clone();
        self.final_payouts = Some(payouts);
    }

    fn reset_round_for_next_street(&mut self) {
        self.reset_committed_this_round();
        self.last_full_raise_size = self.config.big_blind;
        for (idx, player) in self.players.iter().enumerate() {
            self.raise_option_open[idx] = player.status == PlayerStatus::Active;
        }
        // D-037-rev1: showdown_order 起点 = **最后一条 betting round** 的
        // 最后一次 voluntary bet/raise（不是整手内最后一次）。匹配 PokerKit
        // 0.4.14 `_begin_betting` (state.py:3381) 在每条街起手清 opener_index
        // 的语义。空缺时 compute_showdown_order 回退到 SB。
        self.last_aggressor = None;
    }

    fn reset_committed_this_round(&mut self) {
        for player in &mut self.players {
            player.committed_this_round = ChipAmount::ZERO;
        }
    }

    fn round_complete(&self) -> bool {
        self.next_player_needing_action_from(SeatId(0)).is_none()
    }

    fn next_player_needing_action_from(&self, start: SeatId) -> Option<SeatId> {
        let n = self.players.len();
        let max_committed = self.max_committed_this_round();
        for offset in 0..n {
            let seat = SeatId(((start.0 as usize + offset) % n) as u8);
            let idx = seat.0 as usize;
            let player = &self.players[idx];
            if player.status != PlayerStatus::Active {
                continue;
            }
            if player.committed_this_round < max_committed || self.raise_option_open[idx] {
                return Some(seat);
            }
        }
        None
    }

    fn first_active_from(&self, start: SeatId) -> Option<SeatId> {
        let n = self.players.len();
        for offset in 0..n {
            let seat = SeatId(((start.0 as usize + offset) % n) as u8);
            if self.players[seat.0 as usize].status == PlayerStatus::Active {
                return Some(seat);
            }
        }
        None
    }

    fn max_committed_this_round(&self) -> ChipAmount {
        self.players
            .iter()
            .map(|p| p.committed_this_round)
            .max()
            .unwrap_or(ChipAmount::ZERO)
    }

    fn open_raise_options_after_full_raise(&mut self, raiser_idx: usize, new_max: ChipAmount) {
        for (idx, player) in self.players.iter().enumerate() {
            if player.status == PlayerStatus::Active && player.committed_this_round < new_max {
                self.raise_option_open[idx] = true;
            }
        }
        self.raise_option_open[raiser_idx] = false;
    }

    fn post_forced_bets(&mut self) {
        if self.config.ante > ChipAmount::ZERO {
            for idx in 0..self.players.len() {
                self.post_forced_amount(idx, self.config.ante, false);
            }
        }
        self.post_forced_amount(
            self.small_blind_seat().0 as usize,
            self.config.small_blind,
            true,
        );
        self.post_forced_amount(
            self.big_blind_seat().0 as usize,
            self.config.big_blind,
            true,
        );
    }

    fn post_forced_amount(&mut self, idx: usize, amount: ChipAmount, this_round: bool) {
        let paid = std::cmp::min(amount, self.players[idx].stack);
        self.players[idx].stack -= paid;
        self.players[idx].committed_total += paid;
        if this_round {
            self.players[idx].committed_this_round += paid;
        }
        if self.players[idx].stack == ChipAmount::ZERO {
            self.players[idx].status = PlayerStatus::AllIn;
        }
    }

    fn deal_board_to(&mut self, len: usize) {
        while self.board.len() < len {
            self.board.push(self.runout_board[self.board.len()]);
        }
        self.history.board = self.board.clone();
    }

    fn return_uncalled_bets(&mut self) {
        let mut returned = vec![0u64; self.players.len()];
        for (winner, amount) in self.single_contributor_tranches() {
            returned[winner] += amount;
        }
        for (idx, amount) in returned.into_iter().enumerate() {
            if amount == 0 {
                continue;
            }
            let chips = ChipAmount::new(amount);
            self.players[idx].committed_total -= chips;
            if self.players[idx].committed_this_round >= chips {
                self.players[idx].committed_this_round -= chips;
            } else {
                self.players[idx].committed_this_round = ChipAmount::ZERO;
            }
            self.players[idx].stack += chips;
        }
    }

    fn single_contributor_tranches(&self) -> Vec<(usize, u64)> {
        let levels = self.contribution_levels();
        let mut prev = 0;
        let mut out = Vec::new();
        for level in levels {
            let contributors: Vec<usize> = self
                .players
                .iter()
                .enumerate()
                .filter(|(_, p)| p.committed_total.as_u64() >= level)
                .map(|(idx, _)| idx)
                .collect();
            if contributors.len() == 1 {
                out.push((contributors[0], level - prev));
            }
            prev = level;
        }
        out
    }

    fn compute_payouts(&self) -> Vec<(SeatId, i64)> {
        let mut awards = vec![0u64; self.players.len()];
        let levels = self.contribution_levels();

        // D-039 / D-039-rev1 配套（2026-05-08 补丁）：先按 contender 集合
        // 合并相邻 contribution level 成 main pot + 各 side pot，再做 base/rem
        // 划分。与 PokerKit `state.pots` (state.py:2378-2380) 的 pot 合并逻辑
        // 一致。直接逐 level 切 sub-pot 会让原本应整除的 main pot 因分子被切
        // 散而生成多余的 1-chip remainder，触发 100k cross-validation 桶 B-2way
        // (28 seeds) / 桶 B-3way (67 seeds) 分歧。详见
        // `docs/xvalidate_100k_diverged_seeds.md`。
        let mut pots: Vec<(u64, Vec<usize>)> = Vec::new();
        let mut prev = 0u64;
        for level in levels {
            let contributors: Vec<usize> = self
                .players
                .iter()
                .enumerate()
                .filter(|(_, p)| p.committed_total.as_u64() >= level)
                .map(|(idx, _)| idx)
                .collect();
            let amount = (level - prev) * contributors.len() as u64;
            prev = level;
            if amount == 0 {
                continue;
            }

            let contenders: Vec<usize> = contributors
                .iter()
                .copied()
                .filter(|&idx| self.players[idx].status != PlayerStatus::Folded)
                .collect();
            // Unreachable in valid B2 flow (heads-up to a side pot collapses
            // via uncalled-bet refund in `single_contributor_tranches` first).
            // Loud in debug to flush out any C1+ flow that breaks this; in
            // release fall back to first contributor to preserve I-001
            // (chip conservation) rather than silently dropping the level.
            debug_assert!(
                !contenders.is_empty(),
                "compute_payouts: level {} has only folded contributors — \
                 unreachable in valid stage-1 flow; chip non-conservation if \
                 left unhandled. Inspect single_contributor_tranches / \
                 player status transitions.",
                level,
            );
            if contenders.is_empty() {
                awards[contributors[0]] += amount;
                continue;
            }

            // Merge with the most recently appended pot if the contender set
            // is identical (same as PokerKit's `while pots and
            // pots[-1].player_indices == tuple(player_indices): amount +=
            // pots.pop().amount` collapse).
            if let Some(last) = pots.last_mut() {
                if last.1 == contenders {
                    last.0 += amount;
                    continue;
                }
            }
            pots.push((amount, contenders));
        }

        for (amount, contenders) in pots {
            let winners = self.pot_winners(&contenders);
            let base = amount / winners.len() as u64;
            let remainder = amount % winners.len() as u64;
            for &winner in &winners {
                awards[winner] += base;
            }
            if remainder > 0 {
                if let Some(winner) = self.odd_chip_order(&winners).first() {
                    // PokerKit's chips-pushing divmod gives a pot's whole
                    // remainder to the first winner in button-left order
                    // (D-039-rev1).
                    awards[*winner] += remainder;
                }
            }
        }

        self.players
            .iter()
            .enumerate()
            .map(|(idx, player)| {
                let committed = player.committed_total.as_u64();
                (
                    player.seat,
                    i64::try_from(awards[idx]).expect("stage-1 chip totals fit i64")
                        - i64::try_from(committed).expect("stage-1 chip totals fit i64"),
                )
            })
            .collect()
    }

    fn pot_winners(&self, contenders: &[usize]) -> Vec<usize> {
        if contenders.len() == 1 {
            return vec![contenders[0]];
        }
        let mut best = None;
        let mut winners = Vec::new();
        for &idx in contenders {
            let Some(hole) = self.history.hole_cards[idx] else {
                continue;
            };
            let cards = [
                hole[0],
                hole[1],
                self.board[0],
                self.board[1],
                self.board[2],
                self.board[3],
                self.board[4],
            ];
            let rank = eval::eval7(&cards);
            match best {
                None => {
                    best = Some(rank);
                    winners.clear();
                    winners.push(idx);
                }
                Some(current) if rank > current => {
                    best = Some(rank);
                    winners.clear();
                    winners.push(idx);
                }
                Some(current) if rank == current => winners.push(idx),
                Some(_) => {}
            }
        }
        winners
    }

    fn contribution_levels(&self) -> Vec<u64> {
        let mut levels = BTreeSet::new();
        for player in &self.players {
            let amount = player.committed_total.as_u64();
            if amount > 0 {
                levels.insert(amount);
            }
        }
        levels.into_iter().collect()
    }

    fn odd_chip_order(&self, winners: &[usize]) -> Vec<usize> {
        let n = self.players.len();
        let winner_set: BTreeSet<usize> = winners.iter().copied().collect();
        let mut ordered = Vec::with_capacity(winners.len());
        for offset in 1..=n {
            let seat = (self.config.button_seat.0 as usize + offset) % n;
            if winner_set.contains(&seat) {
                ordered.push(seat);
            }
        }
        ordered
    }

    fn compute_showdown_order(&self) -> Vec<SeatId> {
        let n = self.players.len();
        let start = self
            .last_aggressor
            .filter(|seat| self.players[seat.0 as usize].status != PlayerStatus::Folded)
            .unwrap_or_else(|| self.small_blind_seat());
        let mut order = Vec::new();
        for offset in 0..n {
            let seat = SeatId(((start.0 as usize + offset) % n) as u8);
            if self.players[seat.0 as usize].status != PlayerStatus::Folded {
                order.push(seat);
            }
        }
        order
    }

    fn small_blind_seat(&self) -> SeatId {
        self.next_seat(self.config.button_seat)
    }

    fn big_blind_seat(&self) -> SeatId {
        self.next_seat(self.small_blind_seat())
    }

    fn next_seat(&self, seat: SeatId) -> SeatId {
        SeatId(((seat.0 as usize + 1) % self.players.len()) as u8)
    }
}

fn empty_legal_actions() -> LegalActionSet {
    LegalActionSet {
        fold: false,
        check: false,
        call: None,
        bet_range: None,
        raise_range: None,
        all_in_amount: None,
    }
}

fn validate_config(config: &TableConfig) {
    // D-030 names 2..=9 as the spec range, but stage-1 SB/BB derivation per
    // D-022b ("SB = button+1, BB = button+2") collapses to "BB = button" when
    // n_seats == 2 — the opposite of standard heads-up NLHE where the button
    // posts SB. Heads-up support requires a D-022b-revM amendment plus an
    // explicit handler, neither of which is in B2 scope. Reject n_seats==2
    // loudly until that lands rather than silently swap blinds.
    assert!(
        (3..=9).contains(&config.n_seats),
        "TableConfig.n_seats must be in 3..=9 (stage-1 limitation: heads-up \
         requires D-022b-revM button-posts-SB handling, see CLAUDE.md and \
         workflow.md §修订历史). Spec D-030 reserves 2..=9 but n_seats==2 \
         is gated until heads-up support lands."
    );
    assert_eq!(
        config.starting_stacks.len(),
        config.n_seats as usize,
        "starting_stacks length must equal n_seats"
    );
    assert!(
        (config.button_seat.0 as usize) < config.n_seats as usize,
        "button_seat out of range"
    );
}

fn seat_order(button: SeatId, n: usize, start_offset: usize) -> Vec<SeatId> {
    (0..n)
        .map(|offset| SeatId(((button.0 as usize + start_offset + offset) % n) as u8))
        .collect()
}
