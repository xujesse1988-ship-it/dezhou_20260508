//! `SubgameNlheGame`（S6 实时搜索 6a：单层 subgame depth-limited re-solve 的 `Game` 壳）。
//!
//! 设计依据 `docs/temp/realtime_search_design_2026_06_03.md`。把"以中途 public state 为根、对各家
//! range 做期望"的 subgame 包成一个 [`Game`]，从而**原样复用** [`EsMccfrTrainer`] /
//! [`VanillaCfrTrainer`]（`trainer.rs` 一行不改）。
//!
//! # 三个复用 + 一个新 root
//!
//! - **State / 动作 / infoset / 转移 / 终局收益全部 delegate [`SimplifiedNlheGame`]**：`State` 仍是
//!   [`SimplifiedNlheState`]（自带 `tree` / `abs` / `bucket_table` Arc），故 `current` / `info_set`
//!   / `legal_actions` / `next` / `payoff` 直接转调 `SimplifiedNlheGame::*`——它们只读 state 携带的
//!   字段，state 带的是**子树**就在子树上跑（关联函数与 Game token 无关）。
//! - **只重写 [`Game::root`]**：不走开局 `with_rng_no_history`（uniform 全局发牌），而是
//!   保留中途 public state（街 / 公共牌前缀 / 下注 / 行动权）+ 重发隐藏牌（各家底牌 + 未见
//!   runout）。§5b 后底牌按 per-seat blueprint **range 加权**采样（[`resample_hidden_with_holes`]
//!   (crate::rules::state::GameState::resample_hidden_with_holes)），`use_blueprint_range=false`
//!   时退 uniform（[`GameState::resample_hidden`]）。[`EsMccfrTrainer::step`] 每 step 调一次
//!   `root` → 每 step 一个隐藏信息补全 = external chance sampling。
//! - **终局收益仍走权威 [`GameState::payouts`]**（side pot / showdown 逻辑不改）→ S1 PokerKit
//!   跨验证不受影响。
//!
//! # 边界（`realtime_search_design` §10 + §5b）
//!
//! - range = blueprint **沿历史累乘 reach** 的 per-seat marginal（§5b 默认；uniform 留作 A/B）；
//! - 当前街用**与 blueprint 同**的 action abstraction（finer 菜单留后续）；
//! - depth-limit = 解到 subgame 真实终局（小子树、无叶子近似 / 无 biased leaf；6b 再上）；
//! - 子树 node_id 是**子树本地**索引（从 0 起），与 blueprint 全局树 node_id 不同口径——解到
//!   终局、不查 blueprint 值，故子树自洽即可；6b 接 blueprint 续局值时再做 local↔global 映射。

use std::collections::BTreeSet;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use crate::abstraction::action::StreetActionAbstraction;
use crate::abstraction::bucket_table::BucketTable;
use crate::abstraction::info::{InfoSetId, StreetTag};
use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::{Card, PlayerStatus, Street};
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::nlhe::{
    SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheInfoSet, SimplifiedNlheState,
};
use crate::training::nlhe_betting_tree::{
    AbstractActionTag, BettingAbstractionRules, NodeId, PublicBettingTree,
};
use crate::training::sampling::sample_discrete;
use crate::training::trainer::{EsMccfrTrainer, Trainer};

/// 以一个中途 public state 为根的 subgame `Game`（S6 6a）。
pub struct SubgameNlheGame {
    config: TableConfig,
    /// 从 `template` 中途状态建的 betting 子树（[`PublicBettingTree::build_subtree`]）。
    subtree: Arc<PublicBettingTree>,
    /// 子树 + 运行期 `legal_actions` 同源的 action abstraction（须与建 `subtree` 用的一致）。
    abs: Arc<StreetActionAbstraction>,
    bucket_table: Arc<BucketTable>,
    /// 中途真实状态（实时搜索里 = 权威局 `auth.clone()`）。`root` 每次 clone 它再
    /// [`GameState::resample_hidden`] 重发隐藏牌。
    template: GameState,
    /// S6 §5b：per-seat hole **marginal range**（下标 = seat；每个长 1326、对齐
    /// [`hole_combos`](Self::hole_combos)）。`Some` = root 按 range 加权采样各家底牌
    /// （blueprint reach），`None` = uniform（MVP）。folded 座位的向量留空（不被读）。
    ranges: Option<Vec<Vec<f64>>>,
    /// range 采样用的 1326 具体底牌组合表（仅 `ranges == Some` 时非空）；下标对齐 `ranges`。
    hole_combos: Vec<[Card; 2]>,
}

impl SubgameNlheGame {
    /// 从中途 `template` 状态 + action `abs` + A3×A4 `rules` 建 subgame。`entrants` /
    /// `raises_on_street` = `template` 处的 A3×A4 上下文（调用方据权威局现算；`rules == Default`
    /// 时不被读，传 `(0, 0)`）。`abs` 必须与建子树用的一致（运行期 `legal_actions` 同源）。
    ///
    /// 前置：`template` 是非终局 decision 节点（实时搜索从决策点为根）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bucket_table: Arc<BucketTable>,
        config: TableConfig,
        abs: StreetActionAbstraction,
        rules: BettingAbstractionRules,
        template: GameState,
        entrants: u16,
        raises_on_street: u32,
    ) -> Self {
        debug_assert!(
            !template.is_terminal() && template.current_player().is_some(),
            "SubgameNlheGame::new: template 须是非终局 decision 节点"
        );
        let subtree = Arc::new(PublicBettingTree::build_subtree(
            &template,
            &abs,
            rules,
            entrants,
            raises_on_street,
        ));
        Self {
            config,
            subtree,
            abs: Arc::new(abs),
            bucket_table,
            template,
            ranges: None,
            hole_combos: Vec::new(),
        }
    }

    /// 同 [`new`](Self::new)，但 root 按 per-seat blueprint `ranges` 加权采样各家底牌（S6 §5b
    /// 去 confound）。`ranges[seat]` 长 1326、对齐 [`all_hole_combos`]（folded 座位向量留空，
    /// 不被读）。其余参数同 `new`。
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_ranges(
        bucket_table: Arc<BucketTable>,
        config: TableConfig,
        abs: StreetActionAbstraction,
        rules: BettingAbstractionRules,
        template: GameState,
        entrants: u16,
        raises_on_street: u32,
        ranges: Vec<Vec<f64>>,
    ) -> Self {
        let mut g = Self::new(
            bucket_table,
            config,
            abs,
            rules,
            template,
            entrants,
            raises_on_street,
        );
        debug_assert_eq!(
            ranges.len(),
            g.template.players().len(),
            "ranges 长度须 == 座位数"
        );
        g.ranges = Some(ranges);
        g.hole_combos = all_hole_combos();
        g
    }

    /// 子树（诊断 / 评测：取 root_id 构造查询 infoset）。
    pub fn subtree(&self) -> &PublicBettingTree {
        &self.subtree
    }

    /// 按 `self.ranges` 为每个**未弃牌**座位采样一手底牌（顺序 card-removal：逐座位从
    /// 其 range 限制到「未被 board / 已采样底牌占用」的 hole 上、归一后 [`sample_discrete`]；
    /// 受限 range 全零 → 退均匀采可用 hole）。返回 per-seat `Option<[Card;2]>`（弃牌座 None）。
    fn sample_holes_from_ranges(
        &self,
        ranges: &[Vec<f64>],
        rng: &mut dyn RngSource,
    ) -> Vec<Option<[Card; 2]>> {
        let mut used: BTreeSet<u8> = self.template.board().iter().map(|c| c.to_u8()).collect();
        let players = self.template.players();
        let mut out: Vec<Option<[Card; 2]>> = vec![None; players.len()];
        for (seat, player) in players.iter().enumerate() {
            if player.hole_cards.is_none() {
                continue; // 弃牌座：无底牌。
            }
            // 限制到可用 hole（不撞 used）的 (idx, weight)；weight>0 由 range 决定。
            let mut dist: Vec<(usize, f64)> = Vec::new();
            let mut total = 0.0_f64;
            for (hi, hole) in self.hole_combos.iter().enumerate() {
                let w = ranges[seat].get(hi).copied().unwrap_or(0.0);
                if w > 0.0 && !used.contains(&hole[0].to_u8()) && !used.contains(&hole[1].to_u8()) {
                    dist.push((hi, w));
                    total += w;
                }
            }
            let chosen_idx = if total > 0.0 {
                // 归一（sample_discrete 要求 sum≈1）。
                for e in dist.iter_mut() {
                    e.1 /= total;
                }
                sample_discrete(&dist, rng)
            } else {
                // 受限 range 全零 → 退均匀采可用 hole。
                let avail: Vec<usize> = self
                    .hole_combos
                    .iter()
                    .enumerate()
                    .filter(|(_, h)| !used.contains(&h[0].to_u8()) && !used.contains(&h[1].to_u8()))
                    .map(|(hi, _)| hi)
                    .collect();
                let p = 1.0 / avail.len() as f64;
                let uni: Vec<(usize, f64)> = avail.into_iter().map(|hi| (hi, p)).collect();
                sample_discrete(&uni, rng)
            };
            let hole = self.hole_combos[chosen_idx];
            used.insert(hole[0].to_u8());
            used.insert(hole[1].to_u8());
            out[seat] = Some(hole);
        }
        out
    }

    /// 中途模板状态（决定 root 的 betting 几何 + 真实公共牌前缀）。
    pub fn template(&self) -> &GameState {
        &self.template
    }

    /// 在 subtree root 为 **template 携带的真实手牌**构造查询 `(InfoSetId, 合法动作)`。
    ///
    /// 实时搜索 solve 完后用它索引 hero 真实手在 root 的策略：`template` = `auth.clone()`
    /// 带 actor 的真实底牌 + 真实 board，故 [`SimplifiedNlheGame::info_set`] 算出的就是
    /// hero 真实手的 bucket（"对全 range 求解、事后索引真实桶"，设计 §10.1）。返回的
    /// 合法动作顺序与 [`Trainer::average_strategy`](crate::training::trainer::Trainer::average_strategy)
    /// 向量逐位对齐（同一 subtree root 节点的 `legal_actions`，D-209 序）。
    pub fn root_query(&self) -> (SimplifiedNlheInfoSet, Vec<SimplifiedNlheAction>) {
        let actor = self
            .template
            .current_player()
            .expect("subgame template 必是 decision 节点")
            .0 as PlayerId;
        let query = SimplifiedNlheState {
            game_state: self.template.clone(),
            action_history: Vec::new(),
            bucket_table: Arc::clone(&self.bucket_table),
            current_node_id: self.subtree.root_id(),
            tree: Arc::clone(&self.subtree),
            abs: Arc::clone(&self.abs),
            info_set_cache: AtomicU64::new(0),
        };
        let info = SimplifiedNlheGame::info_set(&query, actor);
        let legal = SimplifiedNlheGame::legal_actions(&query);
        (info, legal)
    }
}

impl Game for SubgameNlheGame {
    type State = SimplifiedNlheState;
    type Action = SimplifiedNlheAction;
    type InfoSet = SimplifiedNlheInfoSet;

    // subgame 不存 checkpoint（实时一次性求解）；复用 SimplifiedNlhe 变体 tag。
    const VARIANT: crate::error::GameVariant = crate::error::GameVariant::SimplifiedNlhe;

    fn bucket_table_blake3(&self) -> [u8; 32] {
        self.bucket_table.content_hash()
    }

    fn n_players(&self) -> usize {
        self.config.n_seats as usize
    }

    fn root(&self, rng: &mut dyn RngSource) -> SimplifiedNlheState {
        // 唯一与 SimplifiedNlheGame 不同处：保留中途 public state、重发隐藏牌（per-step chance）。
        // §5b：有 range 则按 blueprint reach 加权采样各家底牌（去 confound）；否则 uniform。
        let game_state = match &self.ranges {
            Some(ranges) => {
                let holes = self.sample_holes_from_ranges(ranges, rng);
                self.template.resample_hidden_with_holes(&holes, rng)
            }
            None => self.template.resample_hidden(rng),
        };
        SimplifiedNlheState {
            game_state,
            action_history: Vec::new(),
            bucket_table: Arc::clone(&self.bucket_table),
            current_node_id: self.subtree.root_id(),
            tree: Arc::clone(&self.subtree),
            abs: Arc::clone(&self.abs),
            info_set_cache: AtomicU64::new(0),
        }
    }

    // 以下全部 delegate SimplifiedNlheGame——它们只读 state 携带的 tree/abs/game_state，
    // state 带子树就在子树上跑（关联函数，与 Game token 无关）。
    fn current(state: &SimplifiedNlheState) -> NodeKind {
        SimplifiedNlheGame::current(state)
    }

    fn info_set(state: &SimplifiedNlheState, actor: PlayerId) -> SimplifiedNlheInfoSet {
        SimplifiedNlheGame::info_set(state, actor)
    }

    fn legal_actions(state: &SimplifiedNlheState) -> Vec<SimplifiedNlheAction> {
        SimplifiedNlheGame::legal_actions(state)
    }

    fn next(
        state: SimplifiedNlheState,
        action: SimplifiedNlheAction,
        rng: &mut dyn RngSource,
    ) -> SimplifiedNlheState {
        SimplifiedNlheGame::next(state, action, rng)
    }

    fn chance_distribution(state: &SimplifiedNlheState) -> Vec<(SimplifiedNlheAction, f64)> {
        SimplifiedNlheGame::chance_distribution(state)
    }

    fn payoff(state: &SimplifiedNlheState, player: PlayerId) -> f64 {
        SimplifiedNlheGame::payoff(state, player)
    }
}

// ===========================================================================
// 实时搜索驱动（S6 6a MVP）：触发判据 + 中途上下文现算 + subgame solve + 取分布
// ===========================================================================
//
// # 边界 + 探针解读（必读）
//
// 本驱动 = `realtime_search_design_2026_06_03.md` §10 MVP **+ §5b range 去 confound**。
// `use_blueprint_range`（默认 true）后，subgame 在 blueprint 沿历史累乘出的**真 range**（而非
// 均匀先验）上求解 → 探针**能**有意义地测 §2「搜索放大 blueprint 偏差」：若 blueprint range
// 本身偏（欠训练），solve 建在偏 range 上 → 可能更差；若 range 好 → solve 改进 blueprint 策略。
//
// 仍在的近似（解读探针时记住）：
// 1. **range = per-seat marginal + bucket 粒度**（§5b 陷阱②的工程折中）：玩家间负相关只靠
//    采样期 card-removal 近似，不建联合分布；postflop range 落桶（有损），preflop 精确。
// 2. **解到真实 showdown 终局、无 biased leaf**：MVP 子树小（6-max first_small flop ≈ 4434
//    节点），直接解到真实终局——**无叶子近似**（不像 Pluribus 截 depth-limit 查 blueprint 值），
//    故「无 blueprint 续局值」在这里**不是 confound**，反而是更精确的全解。biased leaf 是 6b。
// 3. **per-bucket 欠采样**：root 对全 range 求解、事后索引 hero 真实桶；`iterations` 摊到每桶
//    的更新数有限 → 桶策略有噪声、CI 偏宽；提高 `iterations` 收敛。`uniform`（`use_blueprint_range
//    = false`）保留作 A/B 对照。
//
// 价值：①plumbing（construct→estimate_range→加权 resample→CFR→取分布→outgoing）正确、可复现、
// 不破守恒；②**去 confound 后的 §2 探针**——search-on vs blueprint-only 的 mbb/g + CI。
// 下一步质量杠杆 = 6b（continual re-solving + biased leaf）。

/// 实时搜索触发 + 求解配置（S6 6a MVP）。`Copy` → 随 `Contestant` 按值带。
#[derive(Clone, Copy, Debug)]
pub struct SubgameSearchConfig {
    /// CFR 迭代步数（每步 [`GameState::resample_hidden`] 一次 = per-step external chance）。
    pub iterations: u64,
    /// subtree 节点数上限；超过即放弃搜索（[`subgame_search`] 返回 `Err` → 回落 blueprint）。
    /// 设计 §5a 守 100–2000（6-max first_small flop 子树）；HU 默认 `{0.5,1,2}` 抽象更大，
    /// 故默认放宽，仅作防爆炸保险，不当调参。
    pub max_subtree_nodes: usize,
    /// 搜索 RNG 基 seed。与 `(hand_seed, decision_ordinal)` 混合 → 每决策点确定派生、
    /// byte-equal 可复现，且跨手独立。
    pub seed: u64,
    /// `true`（默认）= root 按 blueprint **沿历史累乘 reach** 估的 per-seat marginal range
    /// 加权采样各家底牌（§5b 去 confound——subgame 在真 range 而非均匀先验上求解）；`false`
    /// = uniform resample（MVP 旧行为，留作 A/B 对照）。
    pub use_blueprint_range: bool,
}

impl Default for SubgameSearchConfig {
    fn default() -> Self {
        Self {
            iterations: 1000,
            max_subtree_nodes: 8000,
            seed: 0x5347_4D45_5F53_3641, // "SGME_S6A"
            use_blueprint_range: true,
        }
    }
}

/// MVP 触发判据（设计 §10 step 4「仅 flop 第一个决策点」）：flop 街、且**本街未起注**
/// （所有 `committed_this_round == 0`）。缩小验证面，其余决策点回 blueprint。
///
/// 「本街未起注」⟺ `raises_on_street == 0`（postflop 无盲注，max committed_this_round==0
/// 即无 Bet/Raise/AllIn）→ [`subtree_context`] 在此恒返回 raises=0（正确，不会把 re-raise
/// 的 0.5pot 误当开池档；§10.1 审核 A 的坑）。flop 多个 check 直到首次下注前都满足——验证面
/// 仍小，且 raises 仍恒 0，正确性不受影响。
pub fn should_search(auth: &GameState) -> bool {
    if auth.is_terminal() || auth.current_player().is_none() {
        return false;
    }
    auth.street() == Street::Flop && max_committed_this_round(auth) == 0
}

/// 本街最高 `committed_this_round`（`GameState::max_committed_this_round` 是私有，这里据
/// 公开 `players()` 现算）。
fn max_committed_this_round(auth: &GameState) -> u64 {
    auth.players()
        .iter()
        .map(|p| p.committed_this_round.as_u64())
        .max()
        .unwrap_or(0)
}

/// 从权威中途局现算 [`PublicBettingTree::build_subtree`] 需要的
/// `(entrants_bitmask, raises_on_street)`。
///
/// **仅 postflop 未起注决策点正确**（本 MVP 只在 flop 触发，见 [`should_search`]）：
/// - `entrants` = 所有未弃牌（`Active|AllIn`）座位的 bitmask。到 postflop，任何未弃牌玩家
///   都必在 preflop 做过 ≥1 非弃牌动作 → entrants bit 必置（preflop 中途有人尚未行动时
///   不成立，故本函数不用于 preflop）。
/// - `raises_on_street` = **0**。`GameState` 无「本街进攻数」getter；放宽触发面到「已起注」
///   决策点前，**必须**在此实现真·多档计数，否则 `drop_small_reraise` 会把 re-raise 的
///   0.5pot 误当开池档保留（§10.1 审核 A）。`debug_assert` 守住当前前提。
fn subtree_context(auth: &GameState) -> (u16, u32) {
    let mut entrants = 0u16;
    for (i, p) in auth.players().iter().enumerate() {
        if matches!(p.status, PlayerStatus::Active | PlayerStatus::AllIn) {
            entrants |= 1u16 << i;
        }
    }
    debug_assert_eq!(
        max_committed_this_round(auth),
        0,
        "subtree_context 仅支持本街未起注的决策点（raises_on_street==0）；放宽触发面须先实现多档计数"
    );
    (entrants, 0)
}

/// SplitMix64 finalizer 混合 `(base, hand_seed, ordinal)` → subgame solve 的 master seed。
/// 相邻 ordinal / hand_seed 充分去相关（避免不同决策点共用相近 RNG 流）。
fn search_seed(base: u64, hand_seed: u64, ordinal: u64) -> u64 {
    let mut x = base
        ^ hand_seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ ordinal.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

// --- §5b：blueprint range 估计（per-seat marginal，逐街 re-bucket，沿历史累乘 reach） ---

/// 全部 1326 个具体两张底牌组合（升序 card id 对）；下标即 range 向量的 hole 索引。
pub(crate) fn all_hole_combos() -> Vec<[Card; 2]> {
    let mut v = Vec::with_capacity(1326);
    for a in 0u8..52 {
        for b in (a + 1)..52 {
            v.push([
                Card::from_u8(a).expect("0..52 valid"),
                Card::from_u8(b).expect("0..52 valid"),
            ]);
        }
    }
    v
}

/// 节点所在街应取的真实 board 前缀（preflop=0 / flop=3 / turn=4 / river=5）。clamp 到
/// `board.len()`——flop 触发下只会撞 Preflop/Flop（board.len()==3），其余街不出现。
fn board_prefix_for_street(board: &[Card], street: StreetTag) -> &[Card] {
    let n = match street {
        StreetTag::Preflop => 0,
        StreetTag::Flop => 3,
        StreetTag::Turn => 4,
        StreetTag::River => 5,
    };
    &board[..n.min(board.len())]
}

/// 从 actor 当前节点沿 parent 链回溯，收集每个**已做**决策 `(decider_node_id, action_tag,
/// decider_seat)`（顺序无关，estimate_range 只做乘积）。
fn decisions_on_path(
    tree: &PublicBettingTree,
    current_node_id: NodeId,
) -> Vec<(NodeId, AbstractActionTag, PlayerId)> {
    let mut out = Vec::new();
    let mut id = current_node_id;
    loop {
        let node = tree.node(id);
        match node.action_from_parent {
            Some(tag) => {
                let parent_id = node.parent.expect("non-root 节点须有 parent");
                out.push((parent_id, tag, tree.node(parent_id).player_acting));
                id = parent_id;
            }
            None => break,
        }
    }
    out
}

/// 估计 `seat` 的 per-hole marginal range（reach 向量，下标对齐 `holes`，归一）。沿 `decisions`
/// 里属于 `seat` 的决策，对每个候选 hole 累乘该 hole 在 blueprint σ 下走该动作的概率——**逐街
/// 用 `info_set_for_cards` 注入真实 board 前缀算当前街桶**（绝不在固定桶上累乘，§5b 陷阱①）。
/// 撞 board 的 hole reach=0；空/坏 σ 退均匀（同 `strategy_distribution`）。全零（无信号）→ 返回
/// 全零，调用方退均匀采样。
///
/// **同质 blueprint 假设**：用 `game`/`strategy`（actor 的 blueprint）为所有 seat 估 range——
/// 探针自对弈（hero/field 同 blueprint）下精确；异质 field 下是近似（§5b 陷阱②的工程折中）。
fn estimate_range(
    game: &SimplifiedNlheGame,
    strategy: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    decisions: &[(NodeId, AbstractActionTag, PlayerId)],
    board: &[Card],
    seat: PlayerId,
    holes: &[[Card; 2]],
) -> Vec<f64> {
    let board_set: BTreeSet<u8> = board.iter().map(|c| c.to_u8()).collect();
    let tree = game.tree();
    let seat_decisions: Vec<(NodeId, AbstractActionTag)> = decisions
        .iter()
        .filter(|(_, _, s)| *s == seat)
        .map(|(n, t, _)| (*n, *t))
        .collect();
    let mut range = vec![0.0_f64; holes.len()];
    for (hi, hole) in holes.iter().enumerate() {
        if board_set.contains(&hole[0].to_u8()) || board_set.contains(&hole[1].to_u8()) {
            continue; // 撞 board → reach 0
        }
        let mut reach = 1.0_f64;
        for (node_id, tag) in &seat_decisions {
            let node = tree.node(*node_id);
            let bp = board_prefix_for_street(board, node.street);
            let info = game.info_set_for_cards(*node_id, *hole, bp);
            let n = node.legal_actions.len();
            let sigma = strategy(&info, n);
            let idx = node.legal_actions.iter().position(|t| t == tag);
            // 空/坏 σ 或维度不符 / 找不到 tag → 均匀兜底（同 strategy_distribution 容错）。
            let p = match idx {
                Some(i) if sigma.len() == n && sigma[i].is_finite() && sigma[i] >= 0.0 => sigma[i],
                _ => 1.0 / n as f64,
            };
            reach *= p;
            if reach == 0.0 {
                break;
            }
        }
        range[hi] = reach;
    }
    let sum: f64 = range.iter().sum();
    if sum > 0.0 {
        for r in range.iter_mut() {
            *r /= sum;
        }
    }
    range
}

/// S6 6a MVP 实时搜索：从权威中途局 `auth`（actor 待行动）建单层 subgame、跑 CFR、返回
/// actor **真实手**在 root 的策略分布——对齐调用方 `legal_abs`（影子的合法集），可直接喂
/// [`sample_discrete`](crate::training::sampling::sample_discrete) → `outgoing_action`。
///
/// `game` = 该 actor 的 blueprint game（提供 bucket 表 / 同一 action 抽象 + A3×A4 规则，
/// subgame 用**同一套**重建子树）。`node_id` = actor 在 blueprint 树的当前节点（= 影子
/// `current_node_id`；§5b range 估计沿其 public 决策路径累乘 reach）。`strategy` = blueprint
/// average strategy 查询面（range 估计用；同质 blueprint 假设见 [`estimate_range`]）。
/// `(hand_seed, decision_ordinal)` 唯一确定本次 solve 的 RNG（可复现 + 跨手独立）。
///
/// `cfg.use_blueprint_range`：`true` → root 按 per-seat blueprint range 加权采样底牌（§5b 去
/// confound）；`false` → uniform resample（MVP 旧行为，A/B 对照）。
///
/// 任一失败（auth 非 decision / 子树越界 / root 桶在 `iterations` 内未被访问 / 维度不符 /
/// `legal_abs` 含 subtree root 没有的 tag = 影子与 auth 失同步 / 对齐后全零）→ `Err`，
/// 调用方按设计 §4.1 回落 blueprint `strategy_distribution`。
#[allow(clippy::too_many_arguments)]
pub fn subgame_search(
    auth: &GameState,
    game: &SimplifiedNlheGame,
    legal_abs: &[SimplifiedNlheAction],
    node_id: NodeId,
    strategy: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    cfg: &SubgameSearchConfig,
    hand_seed: u64,
    decision_ordinal: u64,
) -> Result<Vec<(SimplifiedNlheAction, f64)>, String> {
    if auth.is_terminal() || auth.current_player().is_none() {
        return Err("subgame_search: auth 非 decision 节点".to_string());
    }
    let (entrants, raises_on_street) = subtree_context(auth);

    // 建 subgame：同 blueprint 的 bucket 表 / action 抽象 + A3×A4 规则，从 auth 中途态为根。
    // §5b：use_blueprint_range → 为每个未弃牌座位估 marginal range，root 按其加权采样底牌；
    // 否则 uniform。range 估计用 actor 在 blueprint 树的 `node_id` 回溯 public 决策路径。
    let sub = if cfg.use_blueprint_range {
        let holes = all_hole_combos();
        let board: Vec<Card> = auth.board().to_vec();
        let decisions = decisions_on_path(game.tree(), node_id);
        let players = auth.players();
        let ranges: Vec<Vec<f64>> = (0..players.len())
            .map(|seat| {
                if players[seat].hole_cards.is_some() {
                    estimate_range(game, strategy, &decisions, &board, seat as PlayerId, &holes)
                } else {
                    Vec::new() // 弃牌座：range 不被读。
                }
            })
            .collect();
        SubgameNlheGame::new_with_ranges(
            Arc::clone(&game.bucket_table),
            auth.config().clone(),
            game.abstraction().clone(),
            game.rules(),
            auth.clone(),
            entrants,
            raises_on_street,
            ranges,
        )
    } else {
        SubgameNlheGame::new(
            Arc::clone(&game.bucket_table),
            auth.config().clone(),
            game.abstraction().clone(),
            game.rules(),
            auth.clone(),
            entrants,
            raises_on_street,
        )
    };
    let n_nodes = sub.subtree().num_nodes();
    if n_nodes == 0 || n_nodes > cfg.max_subtree_nodes {
        return Err(format!(
            "subtree 节点数 {n_nodes} 越界（cap {}）",
            cfg.max_subtree_nodes
        ));
    }

    // 跑 CFR：master seed + step rng 都由 (cfg.seed, hand_seed, decision_ordinal) 确定派生。
    let master = search_seed(cfg.seed, hand_seed, decision_ordinal);
    let mut trainer = EsMccfrTrainer::new(sub, master);
    let mut srng = ChaCha20Rng::from_seed(master ^ 0xC0FF_EE00_C0FF_EE00);
    for _ in 0..cfg.iterations {
        trainer
            .step(&mut srng)
            .map_err(|e| format!("subgame CFR step 失败: {e:?}"))?;
    }

    // 取 actor 真实手在 subtree root 的策略（average strategy，对齐 subtree root 合法动作序）。
    let (info, sub_legal) = trainer.game().root_query();
    let avg = trainer.average_strategy(&info);
    if avg.is_empty() {
        return Err(
            "subgame root infoset 未被 CFR 访问（该 bucket 在 iterations 内未采样到）".to_string(),
        );
    }
    if avg.len() != sub_legal.len() {
        return Err(format!(
            "subgame root 策略维度 {} ≠ 合法动作数 {}",
            avg.len(),
            sub_legal.len()
        ));
    }

    // 按 tag 把 subtree 策略对齐到调用方 legal_abs：返回的动作对象必须是**影子的**
    // （供 outgoing_action / 推进影子复用其 ratio_label/to）。tag 唯一（Bet/Raise 带 ratio），
    // 故一一映射。legal_abs 出现 subtree root 没有的 tag = 影子与 auth 失同步 → Err 回落。
    let prob_by_tag: Vec<(AbstractActionTag, f64)> = sub_legal
        .iter()
        .map(AbstractActionTag::of)
        .zip(avg.iter().copied())
        .collect();
    let mut out: Vec<(SimplifiedNlheAction, f64)> = Vec::with_capacity(legal_abs.len());
    let mut sum = 0.0_f64;
    for a in legal_abs {
        let tag = AbstractActionTag::of(a);
        let p = prob_by_tag
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, p)| *p)
            .ok_or_else(|| {
                format!("legal_abs tag {tag:?} 不在 subtree root 动作集（影子与 auth 失同步）")
            })?;
        if p.is_finite() && p > 0.0 {
            sum += p;
            out.push((*a, p));
        }
    }
    if !(sum.is_finite() && sum > 0.0) {
        return Err("subgame root 策略对齐 legal_abs 后全零".to_string());
    }
    for (_, p) in out.iter_mut() {
        *p /= sum;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    // `ChaCha20Rng` / `AbstractActionTag` / `EsMccfrTrainer` / `Trainer` 已由 `use super::*`
    // 从父模块带入；这里只补父模块未引入的项。
    use crate::abstraction::action::AbstractAction;
    use crate::abstraction::bucket_table::BucketConfig;
    use crate::training::nlhe_betting_tree::first_small_6max;

    fn stub_table() -> Arc<BucketTable> {
        Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ))
    }

    /// 把 HU 默认 game 推到一个 flop 中途状态（SB complete → BB check → flop），返回该
    /// `SimplifiedNlheState`（其 `game_state` 即可作 subgame template）。
    fn hu_flop_state(game: &SimplifiedNlheGame, seed: u64) -> SimplifiedNlheState {
        let mut rng = ChaCha20Rng::from_seed(seed);
        let rng: &mut dyn RngSource = &mut rng;
        let mut state = game.root(rng);
        // SB(button) complete。
        let call = SimplifiedNlheGame::legal_actions(&state)
            .into_iter()
            .find(|a| AbstractActionTag::of(a) == AbstractActionTag::Call)
            .expect("SB 根应有 Call(complete)");
        state = SimplifiedNlheGame::next(state, call, rng);
        // BB option check → flop。
        let check = SimplifiedNlheGame::legal_actions(&state)
            .into_iter()
            .find(|a| matches!(a, AbstractAction::Check))
            .expect("BB option 应有 Check");
        state = SimplifiedNlheGame::next(state, check, rng);
        assert_eq!(state.game_state.board().len(), 3, "应进 flop（板 3 张）");
        assert!(!state.game_state.is_terminal() && state.game_state.current_player().is_some());
        state
    }

    /// 端到端 MVP plumbing：`build_subtree` + `resample_hidden` + `EsMccfrTrainer<SubgameNlheGame>`
    /// 跑通——CFR 在 flop subgame 上每 step 重发隐藏牌、走子树到权威终局、累积策略；①不 panic、
    /// ②update_count 准、③累积到 ≥1 个 infoset、④同 seed 两 trainer 逐 infoset byte-equal（可复现）。
    #[test]
    fn subgame_cfr_runs_and_is_deterministic() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x5542_4732_4D45_5F30); // "SUBG2ME_0"
        let template = flop.game_state.clone();

        let make = || {
            SubgameNlheGame::new(
                stub_table(),
                TableConfig::default_hu_200bb(),
                StreetActionAbstraction::default_6_action(),
                BettingAbstractionRules::default(),
                template.clone(),
                0,
                0,
            )
        };
        let sub = make();
        assert!(sub.subtree().num_nodes() > 0, "子树非空");
        assert_eq!(sub.n_players(), 2);

        let steps = 600u64;
        let run = |seed: u64| {
            let mut tr = EsMccfrTrainer::new(make(), seed);
            let mut rng = ChaCha20Rng::from_seed(seed ^ 0xC0FF_EE00);
            for _ in 0..steps {
                tr.step(&mut rng).expect("subgame step");
            }
            tr
        };
        let a = run(0xA1);
        let b = run(0xA1);

        assert_eq!(a.update_count(), steps, "update_count 应 == steps");
        assert!(
            !a.strategy_sum().inner().is_empty(),
            "subgame CFR 应累积到 ≥1 个 infoset"
        );

        // 同 seed → 两 trainer 逐 infoset average_strategy byte-equal（byte-equal 可复现）。
        assert_eq!(
            a.strategy_sum().inner().len(),
            b.strategy_sum().inner().len(),
            "同 seed 两 trainer 表大小须一致"
        );
        for (info, _) in a.strategy_sum().inner().iter() {
            assert_eq!(
                a.average_strategy(info),
                b.average_strategy(info),
                "同 seed 两 trainer 在 infoset {info:?} 策略须 byte-equal"
            );
        }

        // 对 hero 真实手的 root infoset：若被访问到，则是合法分布（len 对齐 + 和≈1）。
        let actor = template.current_player().expect("flop 有行动者").0 as PlayerId;
        let query = SimplifiedNlheState {
            game_state: template.clone(),
            action_history: Vec::new(),
            bucket_table: stub_table(),
            current_node_id: sub.subtree().root_id(),
            tree: Arc::clone(&sub.subtree),
            abs: Arc::clone(&sub.abs),
            info_set_cache: AtomicU64::new(0),
        };
        let info = SimplifiedNlheGame::info_set(&query, actor);
        let avg = a.average_strategy(&info);
        if !avg.is_empty() {
            let n_legal = SimplifiedNlheGame::legal_actions(&query).len();
            assert_eq!(avg.len(), n_legal, "root 策略维度应 == 合法动作数");
            let sum: f64 = avg.iter().sum();
            assert!((sum - 1.0).abs() < 1e-9, "root 策略应归一，和={sum}");
        }
    }

    /// [`should_search`] 触发面：preflop / flop 已起注 = false；flop 未起注 = true。
    /// 顺带钉 [`subtree_context`]：HU flop 两家都 live → entrants 两 bit、raises==0。
    #[test]
    fn should_search_triggers_only_flop_unraised() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let mut rng = ChaCha20Rng::from_seed(0x5333_4541_5243_4831); // "S3EARCH1"
        let drng: &mut dyn RngSource = &mut rng;

        // preflop root：非 flop → false。
        let pre = game.root(drng);
        assert!(!should_search(&pre.game_state), "preflop 不应触发搜索");

        // 推到 flop 第一个决策点（未起注）：true。
        let flop = hu_flop_state(&game, 0x5333_4541_5243_4832);
        assert!(
            should_search(&flop.game_state),
            "flop 未起注首决策点应触发搜索"
        );
        // subtree_context：HU flop 两家 live → entrants == 0b11、raises == 0。
        let (entrants, raises) = subtree_context(&flop.game_state);
        assert_eq!(entrants, 0b11, "HU flop 两家 live → entrants 两 bit");
        assert_eq!(raises, 0, "flop 未起注 → raises_on_street == 0");

        // flop 上打一个 Bet → 本街已起注 → false。
        let bet = SimplifiedNlheGame::legal_actions(&flop)
            .into_iter()
            .find(|a| matches!(AbstractActionTag::of(a), AbstractActionTag::Bet(_)))
            .expect("flop 首决策点应有 Bet 档");
        let after_bet = SimplifiedNlheGame::next(flop.clone(), bet, drng);
        assert_eq!(after_bet.game_state.street(), crate::core::Street::Flop);
        assert!(
            !should_search(&after_bet.game_state),
            "flop 已起注（有人 Bet）不应再触发（MVP 只搜未起注首决策点）"
        );
    }

    /// [`subgame_search`] 包装契约：①不 panic；②cap 够大时返回 `Ok`，分布归一、动作全在
    /// `legal_abs` 内（按 tag）、维度 ≤ legal_abs；③同 `(hand_seed, ordinal)` 两次调用逐项
    /// byte-equal（可复现）；④节点上限被触发时优雅回落 `Err`（不 panic）。
    ///
    /// 注：stub 桶表 postflop 把**所有**手归桶 0（[`BucketTable::lookup`] is_stub 分支），故
    /// root infoset `(bucket=0, root_id, flop)` 在 traverser==root_actor 的 step 必被累积 →
    /// cap 够大时 `Ok` 是确定的（不靠 per-bucket 命中运气；真桶表下的欠采样 confound 见模块
    /// 顶部 doc，由探针的 search-vs-fallback 计数实测）。
    #[test]
    fn subgame_search_contract_valid_and_reproducible() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x5347_5F43_4F4E_5452); // "SG_CONTR"
        let auth = flop.game_state.clone();
        assert!(
            should_search(&auth),
            "测试前置：auth 须是 flop 未起注决策点"
        );
        let legal_abs = SimplifiedNlheGame::legal_actions(&flop);
        assert!(!legal_abs.is_empty(), "flop 决策点合法集非空");

        // accepting cap：HU 默认 {0.5,1,2} flop 子树较大（见 _measure_flop_subtree_sizes），
        // 用大上限确保不被 cap 拒；stub 全归桶 0 → root infoset 必累积 → Ok 确定。
        // use_blueprint_range=true 走 §5b 路径（estimate_range + 加权采样）；uniform 策略 →
        // range 近均匀（验 range 路径不破契约 + 可复现）。
        let cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            seed: 0xA11C_E55E_5EED_u64,
            use_blueprint_range: true,
        };
        let node_id = flop.current_node_id;
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let mut ok_count = 0usize;
        for hand_seed in 0u64..3 {
            let ordinal = 3u64;
            let r1 = subgame_search(
                &auth, &game, &legal_abs, node_id, &strat, &cfg, hand_seed, ordinal,
            );
            let r2 = subgame_search(
                &auth, &game, &legal_abs, node_id, &strat, &cfg, hand_seed, ordinal,
            );
            assert_eq!(
                r1.is_ok(),
                r2.is_ok(),
                "同 (hand_seed={hand_seed}, ordinal) 两次结果种类须一致（可复现）"
            );
            if let (Ok(d1), Ok(d2)) = (&r1, &r2) {
                ok_count += 1;
                assert_eq!(d1.len(), d2.len(), "可复现：两次维度一致");
                for ((a1, p1), (a2, p2)) in d1.iter().zip(d2) {
                    assert_eq!(a1, a2, "可复现：动作逐项一致");
                    assert_eq!(p1.to_bits(), p2.to_bits(), "可复现：概率 byte-equal");
                }
                let sum: f64 = d1.iter().map(|(_, p)| *p).sum();
                assert!((sum - 1.0).abs() < 1e-9, "返回分布须归一，和={sum}");
                assert!(d1.len() <= legal_abs.len(), "返回维度 ≤ legal_abs");
                for (a, p) in d1 {
                    assert!(*p > 0.0, "只返回正概率动作");
                    let tag = AbstractActionTag::of(a);
                    assert!(
                        legal_abs.iter().any(|l| AbstractActionTag::of(l) == tag),
                        "返回动作 {tag:?} 须在 legal_abs 内"
                    );
                }
            }
        }
        assert_eq!(
            ok_count, 3,
            "accepting cap + stub 桶 0 下每次都应 Ok（root infoset 必累积）"
        );

        // 节点上限触发 → 优雅回落 Err（不 panic）。HU flop 子树 ≫ 5 节点。
        let tiny = SubgameSearchConfig {
            iterations: 50,
            max_subtree_nodes: 5,
            seed: 0xA11C_E55E_5EED_u64,
            use_blueprint_range: true,
        };
        let r = subgame_search(&auth, &game, &legal_abs, node_id, &strat, &tiny, 0, 0);
        assert!(r.is_err(), "节点上限被触发应回落 Err，实得 {r:?}");
    }

    /// §5b [`estimate_range`]：reach 向量归一（Σ≈1）、撞 board 的 hole 恒 0、uniform 策略下
    /// 非冲突 hole 等权（reach 不依赖 hole）。
    #[test]
    fn estimate_range_normalized_and_zeros_board_conflicts() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x5241_4E47_4553_5430); // "RANGEST0"
        let board: Vec<Card> = flop.game_state.board().to_vec();
        let board_set: BTreeSet<u8> = board.iter().map(|c| c.to_u8()).collect();
        let holes = all_hole_combos();
        assert_eq!(holes.len(), 1326);
        let decisions = decisions_on_path(game.tree(), flop.current_node_id);
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let actor = flop.game_state.current_player().unwrap().0 as PlayerId;

        let range = estimate_range(&game, &strat, &decisions, &board, actor, &holes);
        assert_eq!(range.len(), 1326);
        let sum: f64 = range.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "range 须归一，和={sum}");
        let mut positive = 0usize;
        for (hi, hole) in holes.iter().enumerate() {
            let conflicts =
                board_set.contains(&hole[0].to_u8()) || board_set.contains(&hole[1].to_u8());
            if conflicts {
                assert_eq!(range[hi], 0.0, "撞 board 的 hole reach 须 0");
            } else if range[hi] > 0.0 {
                positive += 1;
            }
        }
        // uniform 策略 → 非冲突 hole 等权且全正：恰 C(49,2)=1176 个（52-3 张非 board 牌）。
        assert_eq!(positive, 1176, "非冲突 hole 应全正且 = C(49,2)");
        let w = range.iter().copied().find(|p| *p > 0.0).unwrap();
        for (hi, hole) in holes.iter().enumerate() {
            let conflicts =
                board_set.contains(&hole[0].to_u8()) || board_set.contains(&hole[1].to_u8());
            if !conflicts {
                assert!(
                    (range[hi] - w).abs() < 1e-12,
                    "uniform 策略下非冲突 hole 须等权"
                );
            }
        }
    }

    /// §5b range-weighted `root`：集中 range（seat0→hole k、seat1→hole m，cards disjoint 且不撞
    /// board）→ root 采样的底牌恒 = 该集中 hole（card-removal 不互斥）。验 range→采样链路。
    #[test]
    fn range_weighted_root_respects_concentrated_range() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x5247_524F_4F54_5F30); // "RGROOT_0"
        let template = flop.game_state.clone();
        let board: BTreeSet<u8> = template.board().iter().map(|c| c.to_u8()).collect();
        let holes = all_hole_combos();
        let avail: Vec<u8> = (0u8..52).filter(|v| !board.contains(v)).collect();
        let find = |a: u8, b: u8| {
            holes
                .iter()
                .position(|h| h[0].to_u8() == a && h[1].to_u8() == b)
                .unwrap()
        };
        let k = find(avail[0], avail[1]); // seat0 集中 hole
        let m = find(avail[2], avail[3]); // seat1 集中 hole（cards disjoint）
        let mut r0 = vec![0.0_f64; 1326];
        r0[k] = 1.0;
        let mut r1 = vec![0.0_f64; 1326];
        r1[m] = 1.0;
        let sub = SubgameNlheGame::new_with_ranges(
            stub_table(),
            TableConfig::default_hu_200bb(),
            StreetActionAbstraction::default_6_action(),
            BettingAbstractionRules::default(),
            template,
            0b11,
            0,
            vec![r0, r1],
        );
        let mut rng = ChaCha20Rng::from_seed(0x5247_524F_4F54_0001);
        let drng: &mut dyn RngSource = &mut rng;
        for _ in 0..16 {
            let s = sub.root(drng);
            assert_eq!(
                s.game_state.players()[0].hole_cards,
                Some(holes[k]),
                "seat0 集中 range → 恒采样 hole k"
            );
            assert_eq!(
                s.game_state.players()[1].hole_cards,
                Some(holes[m]),
                "seat1 集中 range → 恒采样 hole m"
            );
        }
    }

    /// 诊断（非门槛）：打印 HU 默认 / 6-max first_small(3) 的 flop 子树节点数，用于校准
    /// [`SubgameSearchConfig::max_subtree_nodes`] 默认值（防爆炸兜底，须 ≥ 实际 MVP 子树）。
    /// `cargo test -p poker --lib -- --ignored --nocapture _measure_flop_subtree_sizes`。
    #[test]
    #[ignore = "诊断打印 subtree 节点数；--ignored --nocapture 跑"]
    fn _measure_flop_subtree_sizes() {
        // HU 默认 {0.5,1,2}。
        let hu = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let hu_flop = hu_flop_state(&hu, 0xD1A6_5152_E5F0_0D00);
        let hu_sub = SubgameNlheGame::new(
            stub_table(),
            TableConfig::default_hu_200bb(),
            StreetActionAbstraction::default_6_action(),
            BettingAbstractionRules::default(),
            hu_flop.game_state.clone(),
            0,
            0,
        );
        eprintln!(
            "[measure] HU default flop subtree nodes = {}",
            hu_sub.subtree().num_nodes()
        );

        // 6-max first_small(3)：驱动到 flop（limp 到底 + 超员被 redirect fold → 3-way flop）。
        let (abs6, rules6) = first_small_6max(3);
        let g6 = SimplifiedNlheGame::new_with_abstraction(
            stub_table(),
            TableConfig::default_6max_100bb(),
            abs6.clone(),
            rules6,
        )
        .expect("6max game");
        let mut rng = ChaCha20Rng::from_seed(0x6D41_5800_0000_0001);
        let drng: &mut dyn RngSource = &mut rng;
        let mut s = g6.root(drng);
        let mut guard = 0;
        while s.game_state.street() == Street::Preflop && !s.game_state.is_terminal() && guard < 60
        {
            let la = SimplifiedNlheGame::legal_actions(&s);
            let pick = la
                .iter()
                .copied()
                .find(|a| AbstractActionTag::of(a) == AbstractActionTag::Call)
                .or_else(|| {
                    la.iter()
                        .copied()
                        .find(|a| matches!(a, AbstractAction::Check))
                })
                .unwrap_or(la[0]);
            s = SimplifiedNlheGame::next(s, pick, drng);
            guard += 1;
        }
        eprintln!(
            "[measure] 6max drive: street={:?} terminal={} live={}",
            s.game_state.street(),
            s.game_state.is_terminal(),
            s.game_state
                .players()
                .iter()
                .filter(|p| matches!(
                    p.status,
                    crate::core::PlayerStatus::Active | crate::core::PlayerStatus::AllIn
                ))
                .count()
        );
        if s.game_state.street() == Street::Flop && !s.game_state.is_terminal() {
            let (ent, rs) = subtree_context(&s.game_state);
            let sub6 = SubgameNlheGame::new(
                stub_table(),
                TableConfig::default_6max_100bb(),
                abs6,
                rules6,
                s.game_state.clone(),
                ent,
                rs,
            );
            eprintln!(
                "[measure] 6max first_small(3) flop subtree nodes = {} (entrants=0b{:b})",
                sub6.subtree().num_nodes(),
                ent
            );
        }
    }
}
