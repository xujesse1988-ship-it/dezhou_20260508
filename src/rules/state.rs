//! 游戏状态机（API §4）。

use std::collections::BTreeSet;

use smallvec::{smallvec, SmallVec};

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
    /// SmallVec 内联存 ≤ 9 个 `Player`（D-030 n_seats 上界），HU NLHE (n=2) 完全
    /// 走 inline 路径，`GameState::clone` 不再为 players 字段 malloc/free（实测
    /// CFR 热路径 clone+drop ≈ 24% → 显著下降）。`pub fn players() -> &[Player]`
    /// 由 SmallVec Deref 透明转 slice，公开 API 不变。
    players: SmallVec<[Player; 9]>,
    street: Street,
    /// SmallVec inline 5 张公共牌；preflop 状态 `len() == 0` 不 alloc，flop/turn/river
    /// 推进时 len 增至 3/4/5 仍 inline。
    board: SmallVec<[Card; 5]>,
    runout_board: [Card; 5],
    /// 每个 seat 的摊牌手牌强度，在 `with_rng_opts`（root）一次性用 `runout_board`
    /// 的全 5 张 + 该 seat hole_cards 预算。一次 update 内牌全程固定（NLHE 无
    /// in-game chance，runout 在 root 发完），摊牌胜负只由牌决定，与下注路径无关；
    /// `pot_winners` 读此缓存即可，避免 ES-MCCFR traverser 枚举出的多个 showdown
    /// terminal 各自重跑 `eval7`。
    ///
    /// 与历史逐 terminal `eval7(board[0..5])` **逐 bit 一致**：`deal_board_to` 保证
    /// `board[i] == runout_board[i]`，showdown 时 `board` 必为全 5 张（见
    /// `pot_winners` 仅在 `contenders.len() > 1` 被调用），故 root 用 `runout_board`
    /// 与终局用 `board` 是同一组牌。SmallVec inline ≤ 9 个 `Option<HandRank>`，
    /// clone 走内联拷贝无 malloc；无 hole_cards 的 seat 存 `None`，保持原
    /// `let Some(hole) = .. else continue` 跳过语义。
    showdown_ranks: SmallVec<[Option<eval::HandRank>; 9]>,
    current_player: Option<SeatId>,
    terminal: bool,
    final_payouts: Option<Vec<(SeatId, i64)>>,
    history: HandHistory,
    /// SmallVec inline ≤ 9 个 bool，clone 走 inline 拷贝免 malloc。
    raise_option_open: SmallVec<[bool; 9]>,
    last_full_raise_size: ChipAmount,
    last_aggressor: Option<SeatId>,
    /// CFR 训练 fast path（D-378）：`false` 时 `apply` / `finalize_terminal`
    /// 跳过 `history.actions` / `history.board` / `history.final_payouts` /
    /// `history.showdown_order` 写入，`history.actions` 起始无 buffer 预分配。
    /// `clone()` 由此省 `with_capacity(32)` Vec buffer 复制 + per-apply push
    /// realloc。`hand_history()` 仍可访问（仅返回 config / seed / hole_cards）。
    /// 默认 `true` 兼容所有现存使用方；只有 [`GameState::with_rng_no_history`]
    /// 构造路径置 `false`。
    track_history: bool,
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
        Self::with_rng_opts(config, seed, rng, true)
    }

    /// CFR 训练专用 fast path（D-378）：与 [`Self::with_rng`] 行为完全一致，
    /// 但 `apply` / `finalize_terminal` 内所有 `history.actions` / `history.board`
    /// / `history.final_payouts` / `history.showdown_order` 写入跳过；
    /// `history.actions` 不预分配 32-entry buffer。`clone()` 因此省一次
    /// 32 × sizeof(RecordedAction) 的 buffer 复制 + 每次 apply 的 Vec push 增长。
    ///
    /// `payouts()` / 终态合法性不受影响（走 `self.final_payouts` 字段，独立于
    /// `history.final_payouts`）。仅 `hand_history().actions/board/final_payouts/
    /// showdown_order` 在 no-history 模式下为空 —— 仍可读 `config / seed / hole_cards`。
    pub fn with_rng_no_history(
        config: &TableConfig,
        seed: u64,
        rng: &mut dyn RngSource,
    ) -> GameState {
        Self::with_rng_opts(config, seed, rng, false)
    }

    fn with_rng_opts(
        config: &TableConfig,
        seed: u64,
        rng: &mut dyn RngSource,
        track_history: bool,
    ) -> GameState {
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

        let mut players: SmallVec<[Player; 9]> = SmallVec::with_capacity(n);
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
        // D-378 CFR fast path：!track_history 时不分配 history.hole_cards Vec
        // （每条 clone 省一次 malloc + free）；pot_winners 已改读
        // players[].hole_cards。track_history 路径仍正常累积，保 replay 语义。
        let mut history_holes: Vec<Option<[Card; 2]>> = if track_history {
            vec![None; n]
        } else {
            Vec::new()
        };
        for (k, seat) in deal_order.iter().enumerate() {
            let idx = seat.0 as usize;
            let hole = [deck[k], deck[n + k]];
            players[idx].hole_cards = Some(hole);
            if track_history {
                history_holes[idx] = Some(hole);
            }
        }

        let runout_board = [
            deck[2 * n],
            deck[2 * n + 1],
            deck[2 * n + 2],
            deck[2 * n + 3],
            deck[2 * n + 4],
        ];

        // 预算每个 seat 的摊牌强度（root 一次性）。runout_board 此刻已是全 5 张，
        // hole_cards 也已发完，rank 由 hole + 全板唯一确定，整手不再变。
        let showdown_ranks: SmallVec<[Option<eval::HandRank>; 9]> = players
            .iter()
            .map(|p| {
                p.hole_cards.map(|hole| {
                    eval::eval7(&[
                        hole[0],
                        hole[1],
                        runout_board[0],
                        runout_board[1],
                        runout_board[2],
                        runout_board[3],
                        runout_board[4],
                    ])
                })
            })
            .collect();

        let mut state = GameState {
            config: config.clone(),
            players,
            street: Street::Preflop,
            board: SmallVec::new(),
            runout_board,
            showdown_ranks,
            current_player: None,
            terminal: false,
            final_payouts: None,
            history: HandHistory {
                schema_version: 1,
                config: config.clone(),
                seed,
                // 6-max NLHE 单手实测分布的 99 分位 < 32 actions（E1 / D1 1M
                // fuzz 数据），预分配避免 simulate 热路径上的多次 Vec realloc。
                // no-history (track_history=false) 路径绝不 push，buffer 留空
                // 让每次 `GameState::clone` 省 32 × sizeof(RecordedAction) 字节复制。
                actions: if track_history {
                    Vec::with_capacity(32)
                } else {
                    Vec::new()
                },
                // !track_history 时 board 留空（finalize_terminal / deal_board_to
                // 均 gated by track_history，不会写）；省一次 cap=5 Vec alloc。
                board: if track_history {
                    Vec::with_capacity(5)
                } else {
                    Vec::new()
                },
                hole_cards: history_holes,
                final_payouts: Vec::new(),
                showdown_order: Vec::new(),
            },
            raise_option_open: smallvec![true; n],
            last_full_raise_size: config.big_blind,
            last_aggressor: None,
            track_history,
        };

        state.post_forced_bets();
        for idx in 0..state.players.len() {
            state.raise_option_open[idx] = state.players[idx].status == PlayerStatus::Active;
        }
        let first = state.next_seat(state.big_blind_seat());
        state.current_player = state.next_player_needing_action_from(first);
        state
    }

    /// 重采样隐藏信息（S6 实时搜索 subgame 发牌）。克隆本**中途**状态，**保留**公共牌前缀
    /// （`board` = 已亮出的 flop/turn/river）+ 全部下注 / 筹码 / 街 / 行动权状态，**重发**所有
    /// 未弃牌座位的底牌 + 未见的 runout 公共牌后缀（从去掉可见牌的牌堆里 Fisher-Yates 抽），
    /// 据新发的牌重算 `showdown_ranks`。弃牌座位 `hole_cards` 保持 `None`（保留弃牌结构）。
    ///
    /// 用途：subgame CFR 在固定 public state 下对各家 range 做期望——每次 CFR `step` 由
    /// `Game::root` 调本函数得一个隐藏信息的随机补全（external chance sampling），betting 子树
    /// 照常 `apply` 推进，终局收益仍走**权威** [`payouts`](Self::payouts)（side pot / showdown
    /// 逻辑一行不动 → S1 PokerKit 跨验证不受影响）。MVP 用 uniform range（任意补全）；后续可按
    /// blueprint range 加权 / 拒绝采样。
    ///
    /// **加性**：现有构造函数 / `apply` / `payouts` / 发牌协议全不改动；本方法只在 clone 上覆写
    /// 卡牌字段（`hole_cards` / `runout_board` / `showdown_ranks`），其余下注 / 公共 / 行动权字段
    /// （`board` / 街 / 筹码 / `current_player` / `raise_option_open` / `last_*` …）逐字保留。
    ///
    /// 唯一例外：返回状态强制 `track_history = false`（对齐 base `SimplifiedNlheGame::root` 的
    /// [`with_rng_no_history`](Self::with_rng_no_history) CFR fast path）。否则从 `track_history =
    /// true` 的权威局 clone 来时，每步 re-solve 都会在 `apply` 的 action 记录 / `finalize_terminal`
    /// 历史写入 / 逐 apply 增长的 `history.actions`（使 per-node clone 退化成 O(depth²)）上白付开销
    /// （CFR / `payouts` 都不读 history；`hand_history()` 在 subgame 状态上无意义）。
    ///
    /// 前置：`self` 须为非终局（实时搜索从 decision 节点为根）。同 `(self, rng 状态)` 必出同补全
    /// （byte-equal 可复现）。
    pub(crate) fn resample_hidden(&self, rng: &mut dyn RngSource) -> GameState {
        debug_assert!(
            !self.terminal && self.current_player.is_some(),
            "resample_hidden 只用于非终局中途 decision 状态"
        );
        let mut state = self.clone();
        // Subgame 状态仅供 CFR 求解：强制走 no-history fast path（见上方 doc）——避免从
        // track_history=true 的权威局 clone 来时每步 re-solve 白付 history 开销。
        state.track_history = false;
        let visible_len = state.board.len();

        // 去掉可见公共牌（board 前缀）的剩余牌堆。
        let visible: BTreeSet<u8> = state.board.iter().map(|c| c.to_u8()).collect();
        let mut deck: Vec<Card> = (0u8..52)
            .filter(|v| !visible.contains(v))
            .map(|v| Card::from_u8(v).expect("0..52 are valid cards"))
            .collect();
        // Fisher-Yates（同 `with_rng_opts` 抽法：尾部缩小，`next_u64 % range`）。
        let len = deck.len();
        for i in 0..len.saturating_sub(1) {
            let j = i + (rng.next_u64() % ((len - i) as u64)) as usize;
            deck.swap(i, j);
        }

        // 重发未弃牌座位底牌（seat index 序，确定性），再发 runout 后缀。
        let mut cursor = 0usize;
        for idx in 0..state.players.len() {
            if state.players[idx].hole_cards.is_some() {
                state.players[idx].hole_cards = Some([deck[cursor], deck[cursor + 1]]);
                cursor += 2;
            }
        }
        // runout：前缀 = 已亮公共牌（强制与 board 一致），后缀从牌堆补。
        let mut runout = state.runout_board;
        for (i, slot) in runout.iter_mut().enumerate() {
            if i < visible_len {
                *slot = state.board[i];
            } else {
                *slot = deck[cursor];
                cursor += 1;
            }
        }
        debug_assert!(
            cursor <= deck.len(),
            "resample_hidden 抽牌越界：cursor={cursor} > deck {}",
            deck.len()
        );
        state.runout_board = runout;

        // 重算 showdown_ranks（同 `with_rng_opts`：hole + 全 5 runout）。
        state.showdown_ranks = state
            .players
            .iter()
            .map(|p| {
                p.hole_cards.map(|hole| {
                    eval::eval7(&[
                        hole[0], hole[1], runout[0], runout[1], runout[2], runout[3], runout[4],
                    ])
                })
            })
            .collect();

        // history.hole_cards 不再同步：track_history 已置 false，subgame 不维护 history。
        state
    }

    /// 同 [`resample_hidden`](Self::resample_hidden)，但未弃牌座位的底牌由调用方**给定**
    /// （S6 §5b：subgame root 按 blueprint range 加权采样的结果——range 估计 + 采样是训练层
    /// 逻辑，本方法只做规则层的「装牌 + 补 runout + 重算 showdown」，不含 blueprint 知识）。
    ///
    /// `holes[seat]` 须与 `self` 的 live 模式一致：未弃牌座位 `Some([c0,c1])`、已弃 `None`。
    /// runout 后缀从「52 − 公共牌 − 全部给定底牌」的剩余牌堆补；`showdown_ranks` 重算；与
    /// [`resample_hidden`](Self::resample_hidden) 一样强制 `track_history = false`。终局收益仍走
    /// 权威 [`payouts`](Self::payouts)（side pot / showdown 一行不动 → S1 跨验证不受影响）。
    ///
    /// 调用方须保证 `holes` 两两不冲突且不撞 board（range 采样的 card-removal 负责）；debug 下
    /// 断言无冲突，release 下 `used` 仍正确排除（runout 不会重复发牌）。
    pub(crate) fn resample_hidden_with_holes(
        &self,
        holes: &[Option<[Card; 2]>],
        rng: &mut dyn RngSource,
    ) -> GameState {
        debug_assert!(
            !self.terminal && self.current_player.is_some(),
            "resample_hidden_with_holes 只用于非终局中途 decision 状态"
        );
        debug_assert_eq!(holes.len(), self.players.len(), "holes 长度须 == 座位数");
        let mut state = self.clone();
        state.track_history = false;
        let visible_len = state.board.len();

        // 装入给定底牌（live 模式须与 template 一致）；`used` 累积 board + 全部底牌。
        let mut used: BTreeSet<u8> = state.board.iter().map(|c| c.to_u8()).collect();
        for (idx, hole) in holes.iter().enumerate() {
            debug_assert_eq!(
                hole.is_some(),
                state.players[idx].hole_cards.is_some(),
                "holes[{idx}] live 模式须与 template 一致"
            );
            if let Some(h) = hole {
                state.players[idx].hole_cards = Some(*h);
                // insert 必须在 debug_assert 外（release 也要排除，否则 runout 重复发牌）。
                let a = used.insert(h[0].to_u8());
                let b = used.insert(h[1].to_u8());
                debug_assert!(a && b, "底牌与 board/其它底牌冲突 @ seat {idx}");
            }
        }

        // 剩余牌堆 = 52 − used；Fisher-Yates（同 resample_hidden 抽法）；补 runout 后缀。
        let mut deck: Vec<Card> = (0u8..52)
            .filter(|v| !used.contains(v))
            .map(|v| Card::from_u8(v).expect("0..52 are valid cards"))
            .collect();
        let len = deck.len();
        for i in 0..len.saturating_sub(1) {
            let j = i + (rng.next_u64() % ((len - i) as u64)) as usize;
            deck.swap(i, j);
        }
        let mut runout = state.runout_board;
        let mut cursor = 0usize;
        for (i, slot) in runout.iter_mut().enumerate() {
            if i < visible_len {
                *slot = state.board[i];
            } else {
                *slot = deck[cursor];
                cursor += 1;
            }
        }
        debug_assert!(
            cursor <= deck.len(),
            "resample_hidden_with_holes 抽牌越界：cursor={cursor} > deck {}",
            deck.len()
        );
        state.runout_board = runout;

        state.showdown_ranks = state
            .players
            .iter()
            .map(|p| {
                p.hole_cards.map(|hole| {
                    eval::eval7(&[
                        hole[0], hole[1], runout[0], runout[1], runout[2], runout[3], runout[4],
                    ])
                })
            })
            .collect();
        state
    }

    /// 实时搜索生产入口（`tools/openpoker_advisor` 缺口②）：把**外部牌局**（OpenPoker 服务端）
    /// 的真实公共牌 + hero 真实底牌注入一个**重放出**的中途 decision 状态。betting 几何来自重放
    /// （`apply` 历史动作还原下注 / 筹码 / 行动权），但牌来自外部——**不是本方 seed 发的**。
    ///
    /// 与 [`resample_hidden_with_holes`](Self::resample_hidden_with_holes) 的区别：后者**保留**
    /// `self.board`、只重发未见 runout 后缀；本方法**覆写**可见 board 为外部真实 board（外部牌局
    /// 的板不由我方 seed 决定），并把 hero 座位底牌设成外部真实底牌——这是 `subgame_search` 的
    /// `query_at` 索引 hero 真实桶所必需（否则读到 seed 发的随机牌 = 错桶 → 搜索解错牌力）。
    ///
    /// 其余**未弃牌**座位（对手）发占位底牌（剩余牌堆按 id 升序取）——subgame solve 每 step 会按
    /// blueprint range 重采样覆盖，故占位值不影响求解；只需合法（不撞 board / hero / 彼此）以保
    /// [`I-003`](GameState) 无重复牌 + `showdown_ranks` 可算。已弃座 `hole_cards` 保持 `None`。
    /// 强制 `track_history = false`（同 `resample_hidden*`：subgame 状态只供求解）。
    ///
    /// 错误（外部数据可能脏 → 返回 `Err`、调用方 fold，**绝不 panic**）：
    /// - `board.len()` ≠ `self.board.len()`（外部 board 与重放街公共牌数不一致）；
    /// - `hero_seat` 越界或已弃牌（无底牌）；
    /// - board + hero 底牌互相冲突。
    pub fn inject_external_cards(
        &self,
        hero_seat: SeatId,
        hero_hole: [Card; 2],
        board: &[Card],
    ) -> Result<GameState, String> {
        if board.len() != self.board.len() {
            return Err(format!(
                "inject_external_cards: 外部 board 长 {} ≠ 重放街公共牌数 {}",
                board.len(),
                self.board.len()
            ));
        }
        let hero_idx = hero_seat.0 as usize;
        if hero_idx >= self.players.len() || self.players[hero_idx].hole_cards.is_none() {
            return Err(format!(
                "inject_external_cards: hero 座 {hero_idx} 越界或已弃牌（无底牌）"
            ));
        }

        // used = board + hero 底牌；任何重复 = 外部数据脏 → Err（不 panic）。
        let mut used: BTreeSet<u8> = BTreeSet::new();
        for c in board.iter().chain(hero_hole.iter()) {
            if !used.insert(c.to_u8()) {
                return Err(format!("inject_external_cards: 外部牌重复 {c:?}"));
            }
        }

        let mut state = self.clone();
        state.track_history = false;
        // 覆写可见 board（外部真实板）+ hero 真实底牌。
        state.board = board.iter().copied().collect();
        state.players[hero_idx].hole_cards = Some(hero_hole);

        // 剩余牌堆（52 − board − hero）：占位底牌 + runout 后缀均从此按序取（cursor 保两两不撞）。
        let deck: Vec<Card> = (0u8..52)
            .filter(|v| !used.contains(v))
            .map(|v| Card::from_u8(v).expect("0..52 are valid cards"))
            .collect();
        let mut cursor = 0usize;
        for idx in 0..state.players.len() {
            if idx == hero_idx || state.players[idx].hole_cards.is_none() {
                continue; // hero 已设；弃牌座保持 None。
            }
            state.players[idx].hole_cards = Some([deck[cursor], deck[cursor + 1]]);
            cursor += 2;
        }
        // runout：前缀 = 真实 board，后缀占位（subgame 每 step 重发 → 不影响求解）。
        let mut runout = state.runout_board;
        for (i, slot) in runout.iter_mut().enumerate() {
            if i < board.len() {
                *slot = board[i];
            } else {
                *slot = deck[cursor];
                cursor += 1;
            }
        }
        debug_assert!(cursor <= deck.len(), "inject_external_cards 抽牌越界");
        state.runout_board = runout;
        // 重算 showdown_ranks（hole + 全 5 runout；同 resample_hidden）。
        state.showdown_ranks = state
            .players
            .iter()
            .map(|p| {
                p.hole_cards.map(|hole| {
                    eval::eval7(&[
                        hole[0], hole[1], runout[0], runout[1], runout[2], runout[3], runout[4],
                    ])
                })
            })
            .collect();
        Ok(state)
    }

    /// 多人 AIVAT 评测入口（缺口⑥，[`crate::training::aivat_multiway`]）：把**外部已知**底牌
    /// （可多座）+ 真实可见 board + **指定** runout 后缀装进一个重放出的中途 decision 状态——
    /// E_runout 逐补全精确枚举用（评测层，非随机抽样）。
    ///
    /// 与 [`inject_external_cards`](Self::inject_external_cards) 的区别：①底牌可装**多座**
    /// （`holes[seat] = Some`）；未给（`None`）的未弃牌座发占位牌（剩余牌堆按 id 升序取，
    /// **保证不与 board / 后缀 / 已知底牌冲突**）——评测方须自行保证这些座位在摊牌前弃牌，
    /// 否则 `payouts` 会读到占位牌 = 错值；②runout 后缀由调用方**指定**（`board ++ suffix`
    /// 共 5 张），不从牌堆随机补。
    ///
    /// 错误（外部数据可能脏 → `Err`，**绝不 panic**）：`holes` 长度 ≠ 座位数 /
    /// `board.len()` ≠ 重放街公共牌数 / `board+suffix` ≠ 5 张 / 已弃座给了底牌 / 牌重复。
    /// 强制 `track_history = false`（同 `resample_hidden*`：仅供评测 `apply` + `payouts`）。
    pub fn with_external_cards_and_runout(
        &self,
        holes: &[Option<[Card; 2]>],
        board: &[Card],
        runout_suffix: &[Card],
    ) -> Result<GameState, String> {
        if holes.len() != self.players.len() {
            return Err(format!(
                "with_external_cards_and_runout: holes 长度 {} ≠ 座位数 {}",
                holes.len(),
                self.players.len()
            ));
        }
        if board.len() != self.board.len() {
            return Err(format!(
                "with_external_cards_and_runout: 外部 board 长 {} ≠ 重放街公共牌数 {}",
                board.len(),
                self.board.len()
            ));
        }
        if board.len() + runout_suffix.len() != 5 {
            return Err(format!(
                "with_external_cards_and_runout: board {} + 后缀 {} ≠ 5 张",
                board.len(),
                runout_suffix.len()
            ));
        }

        // used = board + 后缀 + 全部给定底牌；任何重复 = 外部数据脏 → Err。
        let mut used: BTreeSet<u8> = BTreeSet::new();
        for c in board.iter().chain(runout_suffix.iter()) {
            if !used.insert(c.to_u8()) {
                return Err(format!("with_external_cards_and_runout: 公共牌重复 {c:?}"));
            }
        }
        for (idx, hole) in holes.iter().enumerate() {
            if let Some(h) = hole {
                if self.players[idx].hole_cards.is_none() {
                    return Err(format!(
                        "with_external_cards_and_runout: 已弃座 {idx} 不应给底牌"
                    ));
                }
                for c in h {
                    if !used.insert(c.to_u8()) {
                        return Err(format!(
                            "with_external_cards_and_runout: 底牌重复 {c:?} @ seat {idx}"
                        ));
                    }
                }
            }
        }

        let mut state = self.clone();
        state.track_history = false;
        state.board = board.iter().copied().collect();

        // 装入给定底牌；未给的未弃牌座发占位（剩余牌堆按 id 升序，used 已含后缀 → 不冲突）。
        let deck: Vec<Card> = (0u8..52)
            .filter(|v| !used.contains(v))
            .map(|v| Card::from_u8(v).expect("0..52 are valid cards"))
            .collect();
        let mut cursor = 0usize;
        for (player, hole) in state.players.iter_mut().zip(holes.iter()) {
            if player.hole_cards.is_none() {
                continue; // 弃牌座保持 None。
            }
            if let Some(h) = hole {
                player.hole_cards = Some(*h);
            } else {
                player.hole_cards = Some([deck[cursor], deck[cursor + 1]]);
                cursor += 2;
            }
        }

        // runout = 真实 board 前缀 + 指定后缀；重算 showdown_ranks（同 resample_hidden）。
        let mut runout = state.runout_board;
        for (i, slot) in runout.iter_mut().enumerate() {
            if i < board.len() {
                *slot = board[i];
            } else {
                *slot = runout_suffix[i - board.len()];
            }
        }
        state.runout_board = runout;
        state.showdown_ranks = state
            .players
            .iter()
            .map(|p| {
                p.hole_cards.map(|hole| {
                    eval::eval7(&[
                        hole[0], hole[1], runout[0], runout[1], runout[2], runout[3], runout[4],
                    ])
                })
            })
            .collect();
        Ok(state)
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

        // all-in 合法性（LA-007，与 bet_range/raise_range 同口径）：all-in 投入超过 call 额
        // （`cap > max_committed`）即构成 raise；若此时面对 bet（`max_committed > 0`）且 raise
        // 未重开（`!raise_option_open[idx]`，前序 all-in-for-less 没重开下注）→ all-in 会被规则
        // 当**非法 raise** 拒（`RaiseOptionNotReopened`），故不列入合法集（actor 只能 Call/Fold）。
        // 其余 all-in 都合法、照列：开池 all-in（`max_committed==0`）/ 合法 all-in raise（含
        // raise-for-less，`raise_option_open` 仍 true 而 `cap<min_to`）/ all-in call-for-less
        // （`cap<=max_committed`）。symmetric 等栈下面对 all-in 时 `cap<=max_committed`，本判恒
        // false → 既有行为 byte-equal、S1/PokerKit 不受影响；仅修不对称 / 短码线（步 A①）。
        let all_in_is_illegal_raise =
            max_committed > ChipAmount::ZERO && !self.raise_option_open[idx] && cap > max_committed;
        let all_in_amount =
            (player.stack > ChipAmount::ZERO && !all_in_is_illegal_raise).then_some(cap);

        LegalActionSet {
            fold: true,
            check,
            call,
            bet_range,
            raise_range,
            all_in_amount,
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

    /// `true` 表示本 state 路径仍累积 `HandHistory.actions / board /
    /// final_payouts / showdown_order`；`false` 是 [`Self::with_rng_no_history`]
    /// 构造的 CFR fast path，上述字段不被写入（D-378）。
    pub fn track_history(&self) -> bool {
        self.track_history
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
        if !self.track_history {
            return;
        }
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
                self.current_player = self.first_active_from(self.first_postflop_actor());
            }
            Street::Flop => {
                self.reset_round_for_next_street();
                self.deal_board_to(4);
                self.street = Street::Turn;
                self.current_player = self.first_active_from(self.first_postflop_actor());
            }
            Street::Turn => {
                self.reset_round_for_next_street();
                self.deal_board_to(5);
                self.street = Street::River;
                self.current_player = self.first_active_from(self.first_postflop_actor());
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
        let payouts = self.compute_payouts();
        if self.track_history {
            self.history.board = self.board.to_vec();
            self.history.showdown_order = if showdown {
                self.compute_showdown_order()
            } else {
                Vec::new()
            };
            self.history.final_payouts = payouts.clone();
        }
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
        if self.track_history {
            self.history.board = self.board.to_vec();
        }
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
            // 读 root 预算的 showdown_ranks（见字段 doc）：rank = eval7(hole +
            // runout_board)，与历史 eval7(hole + board[0..5]) 同牌同值。contenders
            // 已过滤 Folded（compute_payouts 见 line ~860），hole_cards 必为 Some，
            // 故缓存槽必为 Some；保留 None→continue 与原 `let Some(hole)` 跳过语义
            // 完全一致（无 hole_cards 的 seat 不参与比牌）。
            let Some(rank) = self.showdown_ranks[idx] else {
                continue;
            };
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
            .unwrap_or_else(|| self.first_postflop_actor());
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
        // D-022b-rev1：n_seats==2 走 button=SB 标准 HU 语义；n_seats>=3 仍按
        // D-022b 机械推导 SB=button+1。
        if self.players.len() == 2 {
            self.config.button_seat
        } else {
            self.next_seat(self.config.button_seat)
        }
    }

    fn big_blind_seat(&self) -> SeatId {
        // D-022b-rev1：n_seats==2 走 BB=non-button=button+1；n_seats>=3 仍按
        // D-022b 机械推导 BB=button+2。
        if self.players.len() == 2 {
            self.next_seat(self.config.button_seat)
        } else {
            self.next_seat(self.small_blind_seat())
        }
    }

    /// postflop 第一个行动者（universal NLHE rule：next_seat(button)）。
    ///
    /// - n_seats>=3：next_seat(button) = button+1 = SB
    /// - n_seats==2 (D-022b-rev1)：next_seat(button) = non-button = BB（OOP 先行）
    ///
    /// 取代了 finish_betting_round / compute_showdown_order 内的
    /// `first_active_from(small_blind_seat())` 路径（D-022b-rev1 之前 SB/BB
    /// 等价于 button+1/+2，二者在 n_seats>=3 上 byte-equal；HU 启用后必须显式
    /// 走 next_seat(button) 才能让 OOP=BB 先行）。
    fn first_postflop_actor(&self) -> SeatId {
        self.next_seat(self.config.button_seat)
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
    // D-022b-rev1 (2026-05-13, stage 3 C2 [实现] 起步前授权)：把 n_seats==2 heads-up
    // 路径打开，按标准 HU NLHE 语义 (button=SB, non-button=BB) 推导盲注 +
    // postflop OOP 先行。具体规则映射见 small_blind_seat / big_blind_seat /
    // finish_betting_round / compute_showdown_order 的 n_seats==2 分支。详见
    // docs/pluribus_stage1_decisions.md §修订历史 D-022b-rev1。
    assert!(
        (2..=9).contains(&config.n_seats),
        "TableConfig.n_seats must be in 2..=9 (D-030 范围；n_seats==2 由 D-022b-rev1 \
         按标准 HU NLHE 语义 button=SB / non-button=BB 推导，详见 \
         docs/pluribus_stage1_decisions.md §修订历史 D-022b-rev1)"
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

#[cfg(test)]
mod resample_tests {
    //! `resample_hidden`（S6 subgame 发牌）的规则层不变量：保留下注/公共牌状态、重发隐藏牌、
    //! 无重复牌、showdown_ranks 与新牌自洽、可推进到权威终局且筹码守恒、byte-equal 可复现。
    use super::*;
    use std::collections::BTreeSet;

    /// 构造一个 flop 中途状态（HU 200BB：SB complete → BB check → flop，双方 Active、board 3 张）。
    fn flop_mid_state(seed: u64) -> GameState {
        let cfg = TableConfig::default_hu_200bb();
        let mut s = GameState::new(&cfg, seed);
        s.apply(Action::Call).expect("SB complete 合法"); // SB(button) 先动
        s.apply(Action::Check).expect("BB check 合法"); // BB option check → flop
        assert_eq!(s.street(), Street::Flop, "应进 flop");
        assert_eq!(s.board().len(), 3, "flop 板 3 张");
        assert!(!s.is_terminal() && s.current_player().is_some());
        s
    }

    /// 被动推进到终局（check 优先 → call → fold → all-in），用于走到 showdown 验证守恒。
    fn play_passive_to_terminal(state: &mut GameState) {
        let mut guard = 0;
        while !state.is_terminal() {
            let la = state.legal_actions();
            let action = if la.check {
                Action::Check
            } else if la.call.is_some() {
                Action::Call
            } else if la.fold {
                Action::Fold
            } else {
                Action::AllIn
            };
            state.apply(action).expect("passive 动作应合法");
            guard += 1;
            assert!(guard < 64, "被动推进未终止（死循环）");
        }
    }

    /// 主不变量：保留态 + 公共牌前缀 + 无重复牌 + showdown_ranks 自洽。
    #[test]
    fn resample_invariants() {
        let base = flop_mid_state(0xA11CE);
        let mut rng = ChaCha20Rng::from_seed(0x5245_5341_4D50_4C45); // "RESAMPLE"
        let r = base.resample_hidden(&mut rng);

        // 保留：街 / 公共牌 / 当前行动者 / 每座筹码与状态 / pot。
        assert_eq!(r.street(), base.street());
        assert_eq!(r.board(), base.board(), "公共牌前缀必须保留");
        assert_eq!(r.current_player(), base.current_player());
        assert_eq!(r.pot(), base.pot());
        assert_eq!(r.players().len(), base.players().len());
        for (a, b) in r.players().iter().zip(base.players().iter()) {
            assert_eq!(a.seat, b.seat);
            assert_eq!(a.stack, b.stack, "stack 不应被 resample 改动");
            assert_eq!(a.committed_this_round, b.committed_this_round);
            assert_eq!(a.committed_total, b.committed_total);
            assert_eq!(a.status, b.status, "弃牌/在场结构必须保留");
        }

        // runout 前缀 == 可见公共牌。
        let vis = r.board().len();
        for i in 0..vis {
            assert_eq!(r.runout_board[i], r.board()[i], "runout 前缀须等于可见板");
        }

        // 无重复牌（I-003）：所有非弃牌底牌 + 全 5 runout 互不相同。
        let mut seen: BTreeSet<u8> = BTreeSet::new();
        let mut count = 0usize;
        for p in r.players() {
            if let Some(h) = p.hole_cards {
                for c in h {
                    assert!(seen.insert(c.to_u8()), "底牌与他牌重复：{c:?}");
                    count += 1;
                }
            }
        }
        for c in r.runout_board {
            assert!(seen.insert(c.to_u8()), "runout 与他牌重复：{c:?}");
            count += 1;
        }
        assert_eq!(seen.len(), count, "存在重复牌");

        // showdown_ranks 与新发牌自洽（= eval7(hole + 全 5 runout)）。
        for (idx, p) in r.players().iter().enumerate() {
            match p.hole_cards {
                Some(h) => {
                    let expect = eval::eval7(&[
                        h[0],
                        h[1],
                        r.runout_board[0],
                        r.runout_board[1],
                        r.runout_board[2],
                        r.runout_board[3],
                        r.runout_board[4],
                    ]);
                    assert_eq!(
                        r.showdown_ranks[idx],
                        Some(expect),
                        "座 {idx} showdown_rank 与重发牌不一致"
                    );
                }
                None => assert_eq!(r.showdown_ranks[idx], None, "弃牌座 rank 应为 None"),
            }
        }
    }

    /// resample 后的状态可经**权威** apply 推进到 showdown 终局，且 per-seat 净 PnL 守恒（Σ==0）。
    /// 终局收益由重发的 showdown_ranks 决定（走 `payouts()`，side pot/showdown 逻辑未改）。
    #[test]
    fn resample_plays_to_terminal_conserves() {
        let base = flop_mid_state(0xBEEF);
        let mut rng = ChaCha20Rng::from_seed(0x0DDC_0FFE_E0DD_F00D);
        let mut r = base.resample_hidden(&mut rng);
        play_passive_to_terminal(&mut r);
        assert!(r.is_terminal());
        let payouts = r.payouts().expect("终局应有 payouts");
        let sum: i64 = payouts.iter().map(|(_, pnl)| *pnl).sum();
        assert_eq!(sum, 0, "per-seat 净 PnL 必须守恒 Σ==0");
        assert_eq!(payouts.len(), base.players().len());
    }

    /// 同 (状态, rng seed) → 同补全（byte-equal 可复现：CFR 可复现的前提）。
    #[test]
    fn resample_is_deterministic() {
        let base = flop_mid_state(0xF00D);
        let mut rng_a = ChaCha20Rng::from_seed(0x1234_5678_9ABC_DEF0);
        let mut rng_b = ChaCha20Rng::from_seed(0x1234_5678_9ABC_DEF0);
        let a = base.resample_hidden(&mut rng_a);
        let b = base.resample_hidden(&mut rng_b);
        for (pa, pb) in a.players().iter().zip(b.players().iter()) {
            assert_eq!(pa.hole_cards, pb.hole_cards, "同 seed resample 底牌须一致");
        }
        assert_eq!(
            a.runout_board, b.runout_board,
            "同 seed resample runout 须一致"
        );
        assert_eq!(a.showdown_ranks, b.showdown_ranks);
    }

    /// `resample_hidden_with_holes`（S6 §5b 装牌路径）：给定底牌被精确装入、保留态不变、
    /// runout 不撞给定底牌、showdown 自洽、可推进到终局且守恒。
    #[test]
    fn resample_with_holes_installs_runout_disjoint_and_conserves() {
        let base = flop_mid_state(0xC0DE);
        assert!(
            base.players().iter().all(|p| p.hole_cards.is_some()),
            "HU flop 两座都 live"
        );
        // 选 4 张不撞 board 的牌作两手底牌。
        let board: BTreeSet<u8> = base.board().iter().map(|c| c.to_u8()).collect();
        let avail: Vec<u8> = (0u8..52).filter(|v| !board.contains(v)).collect();
        let h0 = [
            Card::from_u8(avail[0]).unwrap(),
            Card::from_u8(avail[1]).unwrap(),
        ];
        let h1 = [
            Card::from_u8(avail[2]).unwrap(),
            Card::from_u8(avail[3]).unwrap(),
        ];
        let holes = vec![Some(h0), Some(h1)];

        let mut rng = ChaCha20Rng::from_seed(0x5749_5448_4F4C_4553); // "WITHOLES"
        let mut r = base.resample_hidden_with_holes(&holes, &mut rng);

        // 给定底牌精确装入。
        assert_eq!(r.players()[0].hole_cards, Some(h0), "seat0 底牌须 == 给定");
        assert_eq!(r.players()[1].hole_cards, Some(h1), "seat1 底牌须 == 给定");
        // 保留态。
        assert_eq!(r.street(), base.street());
        assert_eq!(r.board(), base.board(), "公共牌前缀保留");
        assert_eq!(r.current_player(), base.current_player());
        assert_eq!(r.pot(), base.pot());
        // runout 前缀 == board；无重复牌（底牌 + runout disjoint）。
        for i in 0..r.board().len() {
            assert_eq!(r.runout_board[i], r.board()[i]);
        }
        let mut seen: BTreeSet<u8> = BTreeSet::new();
        for p in r.players() {
            if let Some(h) = p.hole_cards {
                for c in h {
                    assert!(seen.insert(c.to_u8()), "底牌重复：{c:?}");
                }
            }
        }
        for c in r.runout_board {
            assert!(seen.insert(c.to_u8()), "runout 撞底牌/board：{c:?}");
        }
        // showdown 自洽。
        for (idx, p) in r.players().iter().enumerate() {
            if let Some(h) = p.hole_cards {
                let expect = eval::eval7(&[
                    h[0],
                    h[1],
                    r.runout_board[0],
                    r.runout_board[1],
                    r.runout_board[2],
                    r.runout_board[3],
                    r.runout_board[4],
                ]);
                assert_eq!(
                    r.showdown_ranks[idx],
                    Some(expect),
                    "座 {idx} showdown 不自洽"
                );
            }
        }
        // 可推进到权威终局 + 守恒。
        play_passive_to_terminal(&mut r);
        let payouts = r.payouts().expect("终局应有 payouts");
        assert_eq!(
            payouts.iter().map(|(_, pnl)| *pnl).sum::<i64>(),
            0,
            "per-seat 净 PnL 须守恒 Σ==0"
        );
    }

    /// `inject_external_cards`（缺口② 生产入口）：覆写可见 board + hero 真实底牌、保留 betting
    /// 几何、其余 live 座占位且无重复牌、showdown 自洽；脏外部数据（board 长不符 / 撞牌 / 弃牌
    /// hero）返回 `Err` 不 panic。
    #[test]
    fn inject_external_cards_overwrites_board_and_hero_hole() {
        let base = flop_mid_state(0xBEEF); // HU 200BB flop，双方 live、board 3 张。
        let hero = base.current_player().expect("flop 有行动者");
        // 选 5 张互不相同、确定的牌作「外部」board(3) + hero 底牌(2)；故意 != base 发的牌。
        let ext_board = [
            Card::from_u8(0).unwrap(),
            Card::from_u8(1).unwrap(),
            Card::from_u8(2).unwrap(),
        ];
        let hero_hole = [Card::from_u8(3).unwrap(), Card::from_u8(4).unwrap()];
        let r = base
            .inject_external_cards(hero, hero_hole, &ext_board)
            .expect("干净外部牌应注入成功");

        // 可见 board 覆写为外部板；hero 底牌 = 外部底牌。
        assert_eq!(r.board(), &ext_board, "可见 board 须覆写为外部真实板");
        assert_eq!(
            r.players()[hero.0 as usize].hole_cards,
            Some(hero_hole),
            "hero 底牌须 == 外部真实底牌"
        );
        // betting 几何保留（街 / 行动权 / pot / 各座筹码）。
        assert_eq!(r.street(), base.street());
        assert_eq!(r.current_player(), base.current_player());
        assert_eq!(r.pot(), base.pot());
        for (a, b) in r.players().iter().zip(base.players()) {
            assert_eq!(a.stack, b.stack, "各座栈保留");
            assert_eq!(a.committed_this_round, b.committed_this_round);
            assert_eq!(a.status, b.status, "弃牌结构保留");
        }
        // runout 前缀 == 外部 board；全牌（底牌 + runout）无重复（I-003）。
        for (i, &c) in ext_board.iter().enumerate() {
            assert_eq!(r.runout_board[i], c);
        }
        let mut seen: BTreeSet<u8> = BTreeSet::new();
        for p in r.players() {
            if let Some(h) = p.hole_cards {
                for c in h {
                    assert!(seen.insert(c.to_u8()), "底牌重复：{c:?}");
                }
            }
        }
        for c in r.runout_board {
            assert!(seen.insert(c.to_u8()), "runout 撞底牌：{c:?}");
        }
        // showdown_ranks 与新牌自洽。
        for (idx, p) in r.players().iter().enumerate() {
            if let Some(h) = p.hole_cards {
                let expect = eval::eval7(&[
                    h[0],
                    h[1],
                    r.runout_board[0],
                    r.runout_board[1],
                    r.runout_board[2],
                    r.runout_board[3],
                    r.runout_board[4],
                ]);
                assert_eq!(
                    r.showdown_ranks[idx],
                    Some(expect),
                    "座 {idx} showdown 不自洽"
                );
            }
        }
    }

    /// 脏外部数据各分支返回 `Err`、不 panic（live 不能崩；调用方 fold）。
    #[test]
    fn inject_external_cards_rejects_dirty_input() {
        let base = flop_mid_state(0xF00D);
        let hero = base.current_player().expect("flop 有行动者");
        let hole = [Card::from_u8(10).unwrap(), Card::from_u8(11).unwrap()];
        // board 长 != 当前街公共牌数（flop=3，这里给 4）。
        let bad_len = [
            Card::from_u8(0).unwrap(),
            Card::from_u8(1).unwrap(),
            Card::from_u8(2).unwrap(),
            Card::from_u8(3).unwrap(),
        ];
        assert!(base.inject_external_cards(hero, hole, &bad_len).is_err());
        // hero 底牌撞 board。
        let board = [
            Card::from_u8(0).unwrap(),
            Card::from_u8(1).unwrap(),
            Card::from_u8(2).unwrap(),
        ];
        let collide = [Card::from_u8(0).unwrap(), Card::from_u8(5).unwrap()];
        assert!(base.inject_external_cards(hero, collide, &board).is_err());
        // 越界 hero 座。
        assert!(base.inject_external_cards(SeatId(9), hole, &board).is_err());
    }
}

#[cfg(test)]
mod legal_action_tests {
    //! LA-007（all-in 合法性）：all-in 构成**非法 raise**（面对 bet、raise 未重开、cap > call 额）
    //! 时 `all_in_amount == None`；而**合法 all-in-for-less raise**（raise 未被堵、cap < 最小 full
    //! raise）仍 `Some`。步 A① 不对称 / 短码线的根因修复（`build_subtree` 不再 panic
    //! `RaiseOptionNotReopened`）。symmetric 等栈行为不变（回归测试 + betting-tree byte-equal 守门）。
    use super::*;

    fn cfg_3way(stacks: [u64; 3]) -> TableConfig {
        TableConfig {
            n_seats: 3,
            starting_stacks: stacks.iter().map(|&c| ChipAmount::new(c)).collect(),
            small_blind: ChipAmount::new(50),
            big_blind: ChipAmount::new(100),
            ante: ChipAmount::ZERO,
            button_seat: SeatId(0),
        }
    }

    /// 短码 all-in-for-less raise 不重开下注 → 深码开池者只能 Call/Fold，all-in（= 非法 raise）
    /// 不列入；而短码 shover 自己的 all-in-for-less **是合法 raise**、照列。一致性：深码侧
    /// `apply(AllIn)` 确实被规则拒（legal set 与 apply 同口径）。
    #[test]
    fn allin_that_would_be_illegal_raise_is_not_offered() {
        // seat0=BTN 深码 200BB, seat1=SB, seat2=BB 短码 10BB。
        let cfg = cfg_3way([20_000, 20_000, 1_000]);
        let mut s = GameState::new(&cfg, 0xA11C_0DE5_u64);
        // preflop（3-handed UTG=BTN=seat0 先动）：seat0 open 到 600（6BB，min_full_raise=500）。
        assert_eq!(s.current_player(), Some(SeatId(0)));
        s.apply(Action::Raise {
            to: ChipAmount::new(600),
        })
        .expect("btn open 600");
        // seat1(SB) fold。
        assert_eq!(s.current_player(), Some(SeatId(1)));
        s.apply(Action::Fold).expect("sb fold");
        // seat2(BB, 短码 1000) 面对 600：min full-raise=1100 > cap 1000 → 不能 full-raise，
        // 但 all-in-for-less(=1000) 是**合法 raise** → all_in_amount=Some(1000)、raise_range=None。
        assert_eq!(s.current_player(), Some(SeatId(2)));
        let bb_la = s.legal_actions();
        assert!(
            bb_la.raise_range.is_none(),
            "短码无法 full-raise（cap 1000 < min_to 1100）"
        );
        assert_eq!(
            bb_la.all_in_amount,
            Some(ChipAmount::new(1_000)),
            "合法 all-in-for-less raise 须照列（不可过度抑制）"
        );
        // seat2 all-in 到 1000（raise-for-less，差额 400 < 500 → 不重开）。
        s.apply(Action::AllIn).expect("bb all-in-for-less raise");
        // 回 seat0：面对 1000、raise 未重开 → 只能 Call(1000)/Fold；all-in(20000=非法 raise) 不列入。
        assert_eq!(s.current_player(), Some(SeatId(0)));
        let btn_la = s.legal_actions();
        assert_eq!(
            btn_la.call,
            Some(ChipAmount::new(1_000)),
            "深码 call 到 1000"
        );
        assert!(btn_la.raise_range.is_none(), "raise 未重开 → 无 raise");
        assert_eq!(
            btn_la.all_in_amount, None,
            "LA-007：all-in 会是非法 raise（cap>call 且 raise 未重开）→ 不列入合法集"
        );
        // 一致性：apply(AllIn) 在此确实被规则拒。
        assert!(
            s.apply(Action::AllIn).is_err(),
            "深码 all-in 应被拒（RaiseOptionNotReopened）——legal set 已正确不列入"
        );
    }

    /// 回归：symmetric 等栈面对 all-in 时 cap == max_committed（all-in = call、非 raise）→
    /// `all_in_amount` 仍 `Some`（LA-007 附加条件恒 false）；保证修复不动既有对称行为。
    #[test]
    fn symmetric_facing_allin_still_offers_allin() {
        let cfg = TableConfig {
            n_seats: 2,
            starting_stacks: vec![ChipAmount::new(1_000); 2], // 等栈 10BB
            small_blind: ChipAmount::new(50),
            big_blind: ChipAmount::new(100),
            ante: ChipAmount::ZERO,
            button_seat: SeatId(0),
        };
        let mut s = GameState::new(&cfg, 0x5117_u64);
        // HU preflop：button=SB=seat0 先动 → all-in 到 1000（full raise，重开）。
        assert_eq!(s.current_player(), Some(SeatId(0)));
        s.apply(Action::AllIn).expect("sb all-in 1000");
        // seat1(BB) 面对 1000，cap==1000==max_committed → all-in = call（非 raise）→ 仍 Some。
        assert_eq!(s.current_player(), Some(SeatId(1)));
        let la = s.legal_actions();
        assert_eq!(
            la.all_in_amount,
            Some(ChipAmount::new(1_000)),
            "等栈面对 all-in：all-in=call → all_in_amount 仍 Some（附加条件 false）"
        );
        assert!(
            la.raise_range.is_none(),
            "等栈无法 re-raise（cap==max_committed）"
        );
    }
}
