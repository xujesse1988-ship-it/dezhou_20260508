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
//!   `template.resample_hidden(rng)`——保留中途 public state（街 / 公共牌前缀 / 下注 / 行动权）、
//!   重发隐藏牌（各家底牌 + 未见 runout）。[`EsMccfrTrainer::step`] 每 step 调一次 `root` →
//!   每 step 一个隐藏信息补全 = external chance sampling；MVP 用 uniform range。
//! - **终局收益仍走权威 [`GameState::payouts`]**（side pot / showdown 逻辑不改）→ S1 PokerKit
//!   跨验证不受影响。
//!
//! # MVP 边界（`realtime_search_design` §10）
//!
//! - range = **uniform**（resample 任意补全），非 blueprint 加权；
//! - 当前街用**与 blueprint 同**的 action abstraction（finer 菜单留后续）；
//! - depth-limit = 解到 subgame 终局（不截断、无 biased leaf；6b 再上）；
//! - 子树 node_id 是**子树本地**索引（从 0 起），与 blueprint 全局树 node_id 不同口径——MVP 解到
//!   终局、不查 blueprint，故子树自洽即可；6b 接 blueprint 续局值时再做 local↔global 映射。

use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use crate::abstraction::action::StreetActionAbstraction;
use crate::abstraction::bucket_table::BucketTable;
use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::{PlayerStatus, Street};
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::nlhe::{
    SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheInfoSet, SimplifiedNlheState,
};
use crate::training::nlhe_betting_tree::{
    AbstractActionTag, BettingAbstractionRules, PublicBettingTree,
};
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
        }
    }

    /// 子树（诊断 / 评测：取 root_id 构造查询 infoset）。
    pub fn subtree(&self) -> &PublicBettingTree {
        &self.subtree
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
        let game_state = self.template.resample_hidden(rng);
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
// # MVP 边界（必读 —— 决定如何解读「不退化探针」结果）
//
// 本驱动是 `realtime_search_design_2026_06_03.md` §10 的最小可行第一步，刻意**不**含
// 后续刀（§3/§5b/§5c/6b）。三个有意为之的近似，都会影响探针信号的可解读性：
//
// 1. **uniform range**：[`GameState::resample_hidden`] 把**所有**未弃牌座位（含 hero）的
//    底牌 uniform 重发——subgame 在「各家 flop range = 均匀」的**错设游戏**上求解，而非
//    blueprint 沿历史累乘出的真实 range（§5b 是下一刀）。故搜索可能因 range 错设而非
//    blueprint 质量退化。
// 2. **无 blueprint 续局值 / biased leaf**：解到 subgame **真实 showdown 终局**（§10 step 3），
//    叶子不查 blueprint EV、不做 biased continuation。→ 本 MVP **不**触及 §2 的「搜索放大
//    blueprint 偏差」风险（搜索里根本没有 blueprint），它测的是「均匀-range 全解 vs blueprint
//    在该决策点谁更强」，**不是** §2 宣称的 blueprint 质量判别器。
// 3. **per-bucket 欠采样**：root 对全 range 求解、事后索引 hero 真实桶；postflop 有 200 桶，
//    `iterations` 步 resample 摊到每桶仅 `iterations/(n_players·200)` 次更新 → 桶策略噪声大、
//    极端时该桶从未被访问 → [`subgame_search`] 返回 `Err` → 调用方回落 blueprint。要让桶策略
//    稳定需**远多于** smoke 的迭代数（探针 CI 会很宽）。
//
// 结论：本 MVP 的价值 = ①把搜索接进 live 决策环并证 plumbing（construct→resample→CFR→取分布→
// outgoing 翻译）正确、可复现、不破对局守恒；②给一个**有上述 confound 的弱**首信号。要把它
// 升级成真正的 §2 判别器，须接 §5b range + §5c blueprint 叶子值。

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
}

impl Default for SubgameSearchConfig {
    fn default() -> Self {
        Self {
            iterations: 1000,
            max_subtree_nodes: 8000,
            seed: 0x5347_4D45_5F53_3641, // "SGME_S6A"
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

/// S6 6a MVP 实时搜索：从权威中途局 `auth`（actor 待行动）建单层 subgame、跑 CFR、返回
/// actor **真实手**在 root 的策略分布——对齐调用方 `legal_abs`（影子的合法集），可直接喂
/// [`sample_discrete`](crate::training::sampling::sample_discrete) → `outgoing_action`。
///
/// `game` = 该 actor 的 blueprint game（提供 bucket 表 / 同一 action 抽象 + A3×A4 规则，
/// subgame 用**同一套**重建子树）。`(hand_seed, decision_ordinal)` 唯一确定本次 solve 的
/// RNG（可复现 + 跨手独立）。
///
/// 任一失败（auth 非 decision / 子树越界 / root 桶在 `iterations` 内未被访问 / 维度不符 /
/// `legal_abs` 含 subtree root 没有的 tag = 影子与 auth 失同步 / 对齐后全零）→ `Err`，
/// 调用方按设计 §4.1 回落 blueprint `strategy_distribution`。
///
/// MVP 近似见本模块顶部 doc（uniform range / 解到终局无 blueprint 叶子 / per-bucket 欠采样）。
pub fn subgame_search(
    auth: &GameState,
    game: &SimplifiedNlheGame,
    legal_abs: &[SimplifiedNlheAction],
    cfg: &SubgameSearchConfig,
    hand_seed: u64,
    decision_ordinal: u64,
) -> Result<Vec<(SimplifiedNlheAction, f64)>, String> {
    if auth.is_terminal() || auth.current_player().is_none() {
        return Err("subgame_search: auth 非 decision 节点".to_string());
    }
    let (entrants, raises_on_street) = subtree_context(auth);

    // 建 subgame：同 blueprint 的 bucket 表 / action 抽象 / A3×A4 规则，从 auth 中途态为根。
    let sub = SubgameNlheGame::new(
        Arc::clone(&game.bucket_table),
        auth.config().clone(),
        game.abstraction().clone(),
        game.rules(),
        auth.clone(),
        entrants,
        raises_on_street,
    );
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
        let cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            seed: 0xA11C_E55E_5EED_u64,
        };
        let mut ok_count = 0usize;
        for hand_seed in 0u64..3 {
            let ordinal = 3u64;
            let r1 = subgame_search(&auth, &game, &legal_abs, &cfg, hand_seed, ordinal);
            let r2 = subgame_search(&auth, &game, &legal_abs, &cfg, hand_seed, ordinal);
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
        };
        let r = subgame_search(&auth, &game, &legal_abs, &tiny, 0, 0);
        assert!(r.is_err(), "节点上限被触发应回落 Err，实得 {r:?}");
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
