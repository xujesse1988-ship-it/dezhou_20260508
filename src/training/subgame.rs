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
    compute_hand_bucket, SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheInfoSet,
    SimplifiedNlheState,
};
use crate::training::nlhe_betting_tree::{
    AbstractActionTag, BettingAbstractionRules, Child, NodeId, PublicBettingTree,
};
use crate::training::sampling::sample_discrete;
use crate::training::subgame_leaf_value::LeafValueTables;
use crate::training::trainer::{EsMccfrTrainer, Trainer};

/// S6 6b depth-limit：subgame 叶子续局值上下文（[`SimplifiedNlheState::leaf_ctx`] 携带，
/// depth-limit 叶子的 [`SubgameNlheGame::payoff`] 读）。
///
/// `SubgameNlheGame::new_depth_limited` 构建：把子树每个 depth-limit 叶子（本地 `NodeId`）映射
/// 回 **blueprint 全局树** 的街起点 `NodeId`（`global_by_local`，非叶项 = [`u32::MAX`] 哨兵），
/// 配 `values`（[`LeafValueTables`]）+ 续局 `cont`，使叶子 `payoff` 能查 `E[U | seat, 全局节点,
/// bucket, cont]`。弃牌座的叶子值不查表（= 固定 `−committed_total`，见 `payoff`）。
pub struct SubgameLeafCtx {
    /// blueprint 叶子续局值表（与构建 subgame 的 blueprint 同源）。
    pub values: Arc<LeafValueTables>,
    /// `global_by_local[local_node]` = 该 depth-limit 叶子对应的 blueprint 全局街起点 `NodeId`；
    /// 非叶节点 = [`u32::MAX`]（不被读）。
    pub global_by_local: Vec<NodeId>,
    /// 查哪个续局（6b-3 step-1 固定 0 = unbiased；6b-4 叶子选择节点起按对手所选 cont）。
    pub cont: usize,
}

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
    /// S6 6b depth-limit：`Some` = 子树用 [`PublicBettingTree::build_subtree_depth_limited`] 截断、
    /// 叶子查 blueprint 续局值（[`SubgameLeafCtx`]）；`None`（6a）= 子树解到真实终局、走真实
    /// showdown payoff。`root` 把它 clone 进 state（depth-limit 叶子 `payoff` 读）。
    leaf_ctx: Option<Arc<SubgameLeafCtx>>,
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
            leaf_ctx: None,
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

    /// S6 6b：从中途 `template` 建 **depth-limit** subgame（`limit_street` 之后截断、叶子查
    /// blueprint 续局值）。比 [`new_with_ranges`](Self::new_with_ranges) 多三件：①子树用
    /// [`PublicBettingTree::build_subtree_depth_limited`]；②把每个 depth-limit 叶子映射回
    /// **blueprint 全局树** 街起点 `NodeId`（沿子树 tag 路径在 `blueprint_tree` 从
    /// `root_global_node` 导航）；③装 [`SubgameLeafCtx`]（`values` + 映射 + `cont`）。
    ///
    /// `root_global_node` = `template` 对应的 blueprint 全局节点（[`subgame_search`] 现算：
    /// CurrentDecision = 当前节点 / RoundStart = 街起点）。任一叶子无法映射（同抽象下不应发生，
    /// 防御几何漂移）→ `Err`，调用方回落 blueprint。`ranges` 同 `new_with_ranges`（`None` =
    /// uniform）。`cont` = 查哪个续局（step-1 固定 0 = unbiased）。
    #[allow(clippy::too_many_arguments)]
    pub fn new_depth_limited(
        bucket_table: Arc<BucketTable>,
        config: TableConfig,
        abs: StreetActionAbstraction,
        rules: BettingAbstractionRules,
        template: GameState,
        entrants: u16,
        raises_on_street: u32,
        ranges: Option<Vec<Vec<f64>>>,
        limit_street: StreetTag,
        blueprint_tree: &PublicBettingTree,
        root_global_node: NodeId,
        values: Arc<LeafValueTables>,
        cont: usize,
    ) -> Result<Self, String> {
        debug_assert!(
            !template.is_terminal() && template.current_player().is_some(),
            "new_depth_limited: template 须是非终局 decision 节点"
        );
        let subtree = Arc::new(PublicBettingTree::build_subtree_depth_limited(
            &template,
            &abs,
            rules,
            entrants,
            raises_on_street,
            limit_street,
        ));
        // 叶子 → blueprint 全局街起点映射（沿子树 tag 路径在全局树从 root_global_node 导航）。
        let mut global_by_local = vec![u32::MAX; subtree.num_nodes()];
        for local in 0..subtree.num_nodes() as NodeId {
            if subtree.node(local).depth_limit_leaf {
                let tags = subtree.path_to_root(local);
                let g = navigate_global(blueprint_tree, root_global_node, &tags)?;
                global_by_local[local as usize] = g;
            }
        }
        let leaf_ctx = Some(Arc::new(SubgameLeafCtx {
            values,
            global_by_local,
            cont,
        }));
        let (ranges_opt, hole_combos) = match ranges {
            Some(r) => {
                debug_assert_eq!(r.len(), template.players().len(), "ranges 长度须 == 座位数");
                (Some(r), all_hole_combos())
            }
            None => (None, Vec::new()),
        };
        Ok(Self {
            config,
            subtree,
            abs: Arc::new(abs),
            bucket_table,
            template,
            ranges: ranges_opt,
            hole_combos,
            leaf_ctx,
        })
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

    /// 在**任意 subtree 节点** `node_id` 为给定 `game_state`（提供真实手牌 + board + 当前
    /// betting 几何）构造查询 `(InfoSetId, 合法动作)`。
    ///
    /// 实时搜索 solve 完后用它索引 hero 真实手在该节点的策略：`game_state` 须是**当前决策点
    /// 的权威态**（`auth`）——`info_set` 只读 board + actor 真实底牌 + `node_id` 的 betting 维度
    /// （node.street），`legal_actions` 按 `game_state` 几何算全集再过滤到 `node_id.legal_actions`。
    /// **不能用 round-start `template`** 当 game_state 查深层节点：round-start 几何与深层节点的
    /// 合法集不符，会把 `legal_actions` 过滤错（round-start re-solve 关键，见 [`subgame_search`]）。
    /// actor 由 `node_id` 的 `player_acting` 给出（= 该节点决策者）。返回的合法动作顺序与
    /// [`Trainer::average_strategy`](crate::training::trainer::Trainer::average_strategy) 向量逐位
    /// 对齐（同一 subtree 节点的 `legal_actions`，D-209 序）。
    pub fn query_at(
        &self,
        node_id: NodeId,
        game_state: &GameState,
    ) -> (SimplifiedNlheInfoSet, Vec<SimplifiedNlheAction>) {
        let actor = self.subtree.node(node_id).player_acting;
        let query = SimplifiedNlheState {
            game_state: game_state.clone(),
            action_history: Vec::new(),
            bucket_table: Arc::clone(&self.bucket_table),
            current_node_id: node_id,
            tree: Arc::clone(&self.subtree),
            abs: Arc::clone(&self.abs),
            info_set_cache: AtomicU64::new(0),
            leaf_ctx: None, // query 读非叶决策点策略，不查叶子值。
        };
        let info = SimplifiedNlheGame::info_set(&query, actor);
        let legal = SimplifiedNlheGame::legal_actions(&query);
        (info, legal)
    }

    /// 在 subtree root 为 `template` 携带的真实手牌构造查询（= [`query_at`](Self::query_at)
    /// 在 root + template 上的特化；CurrentDecision 模式下 template = `auth`，几何一致）。
    pub fn root_query(&self) -> (SimplifiedNlheInfoSet, Vec<SimplifiedNlheAction>) {
        self.query_at(self.subtree.root_id(), &self.template)
    }
}

/// 从 subtree root 沿 `tags`（round-start→当前的 within-round 抽象动作序）逐边导航，返回当前
/// 决策节点的 subtree-local `NodeId`。`tags` 空（CurrentDecision / round-start 首决策）→ 返回 root。
///
/// 每步在当前节点 `legal_actions` 里按 tag 定位 edge → 取 `children[idx]`。tag 不在合法集
/// （real-geometry subtree 与 blueprint abstract-geometry 漂移 / 影子失同步）或导向终局（不应
/// 到达）→ `Err`，[`subgame_search`] 回落 blueprint。同质 on-tree 自对弈下 real==abstract 几何、
/// 必命中。
fn navigate_subtree(
    subtree: &PublicBettingTree,
    tags: &[AbstractActionTag],
) -> Result<NodeId, String> {
    let mut id = subtree.root_id();
    for tag in tags {
        let node = subtree.node(id);
        let idx = node
            .legal_actions
            .iter()
            .position(|t| t == tag)
            .ok_or_else(|| {
                format!("within-round tag {tag:?} 不在 subtree 节点 {id} 合法集（几何漂移/失同步）")
            })?;
        match node.children[idx] {
            Child::Decision(next) => id = next,
            Child::Terminal => {
                return Err(format!(
                    "within-round tag {tag:?} 导向 subtree 终局（不应到达）"
                ))
            }
        }
    }
    Ok(id)
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
            // 6b depth-limit：把叶子值上下文带进 state（叶子 payoff 读）；6a/uniform = None。
            leaf_ctx: self.leaf_ctx.clone(),
        }
    }

    // 多数 delegate SimplifiedNlheGame——只读 state 携带的 tree/abs/game_state（关联函数，与
    // Game token 无关）。**current/payoff 例外**：6b depth-limit 叶子由本 Game 改写（见各方法）。
    fn current(state: &SimplifiedNlheState) -> NodeKind {
        // 6b：depth-limit 叶子（子树街边界截断点）当 Terminal——CFR 在此不展开、走 payoff 查
        // blueprint 续局值。非 depth-limit 树该标记恒 false（base/6a 行为不变）。
        if state.tree.node(state.current_node_id).depth_limit_leaf {
            return NodeKind::Terminal;
        }
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
        // 6b：depth-limit 叶子查 blueprint 续局值（弃牌座 = −committed），否则走真实 showdown。
        let node = state.tree.node(state.current_node_id);
        if node.depth_limit_leaf {
            return leaf_payoff(state, player, node.street);
        }
        SimplifiedNlheGame::payoff(state, player)
    }
}

/// depth-limit 叶子的 `payoff`（[`SubgameNlheGame::payoff`] 调）。
///
/// **弃牌座**：净收益固定 = `−committed_total`（已投入全损、不依赖 runout，故不查值表——值表
/// 只对仍在手座位累计，见 [`SubgameLeafCtx`]）。**在手座（Active/AllIn）**：查
/// `values.value(player, 全局街起点节点, 该街 bucket, cont)`；该续局缺 → 退 unbiased(cont 0)；
/// 仍缺（街起点高频、miss 罕见）→ 0（已知近似，§见模块顶部 doc）。
fn leaf_payoff(state: &SimplifiedNlheState, player: PlayerId, leaf_street: StreetTag) -> f64 {
    let p = &state.game_state.players()[player as usize];
    if matches!(p.status, PlayerStatus::Folded) {
        return -(p.committed_total.as_u64() as f64);
    }
    let ctx = state
        .leaf_ctx
        .as_ref()
        .expect("depth-limit 叶子 state 必带 leaf_ctx");
    let global = ctx.global_by_local[state.current_node_id as usize];
    if global == u32::MAX {
        // 未映射叶子（new_depth_limited 构建期已 Err 排除，此为防御兜底）。
        return 0.0;
    }
    let bucket = compute_hand_bucket(state, player, leaf_street);
    ctx.values
        .value(player as usize, global, bucket, ctx.cont)
        .or_else(|| ctx.values.value(player as usize, global, bucket, 0))
        .unwrap_or(0.0)
}

/// 从 blueprint 全局树 `from` 节点沿 `tags` 导航，返回落点全局 `NodeId`（[`navigate_subtree`]
/// 的任意起点版，用于 [`SubgameNlheGame::new_depth_limited`] 把子树叶子映射回全局街起点）。
/// tag 不在合法集 / 导向终局 → `Err`（几何漂移，调用方回落 blueprint）。
fn navigate_global(
    tree: &PublicBettingTree,
    from: NodeId,
    tags: &[AbstractActionTag],
) -> Result<NodeId, String> {
    let mut id = from;
    for tag in tags {
        let node = tree.node(id);
        let idx = node
            .legal_actions
            .iter()
            .position(|t| t == tag)
            .ok_or_else(|| format!("global 导航 tag {tag:?} 不在节点 {id} 合法集（几何漂移）"))?;
        match node.children[idx] {
            Child::Decision(next) => id = next,
            Child::Terminal => return Err(format!("global 导航 tag {tag:?} 导向终局（不应到达）")),
        }
    }
    Ok(id)
}

/// 从 `node_id` 沿 parent 链上爬到**当前街起点**（最浅的同街祖先）= RoundStart depth-limit 的
/// subgame 根全局节点（root_state = 轮起点快照对应的全局节点）。preflop 会爬到全树 root；
/// subgame 只在 postflop 触发，故落在 flop/turn/river 街起点。
fn climb_to_street_start(tree: &PublicBettingTree, node_id: NodeId) -> NodeId {
    let street = tree.node(node_id).street;
    let mut id = node_id;
    while let Some(p) = tree.node(id).parent {
        if tree.node(p).street != street {
            break; // parent 在更浅街 → id 即本街起点。
        }
        id = p;
    }
    id
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

/// 实时搜索触发面（[`should_search`]）。
///
/// **实测（2026-06-04 vultr，§10.4）**：`AllPostflop` 朴素放宽**结构性显著退化**（24k 手
/// −192 / 12k 手 −426，CI 上界 < 0；4× 迭代仅 −426→−310 仍负 → **非迭代噪声**；退化集中在
/// **盲位**）。根因 = MVP 从**当前决策点**独立重解，mid-round 决策撞设计 §6 #1/#2 landmine
/// （非 betting-round 起点重解 + 无 within-round 冻结，论文明说更可剥削），在盲位（每手 postflop
/// 决策最多）累积最重；`FlopFirstUnraised` 因 flop 首点 = round-start 而"恰好正确"（−72，CI 跨 0
/// 不退化）。**故默认 = `FlopFirstUnraised`**；`AllPostflop` 为研究用 opt-in，正确放宽须先做
/// §6 round-start re-solve（6b）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchTrigger {
    /// 仅 flop **未起注**首决策点（= betting-round 起点，§6 #1 下"恰好正确"，默认）。
    FlopFirstUnraised,
    /// 任意 postflop 决策点（flop 含已起注 / turn / river）。配 [`ResolveRoot::RoundStart`]
    /// 才正确（§6 #1）；配 [`ResolveRoot::CurrentDecision`]（旧 MVP）撞 landmine 实测退化（§10.4）。
    AllPostflop,
}

/// subgame 重解的**根**取在哪（设计 §6 #1：landmine #1）。
///
/// `AllPostflop` 触发面下，从**当前决策点**独立重解（[`CurrentDecision`](Self::CurrentDecision)）
/// 撞 §6 #1/#2 landmine——mid-round 决策用 blueprint-at-current-point 的噪声 range、且多决策
/// 各自重解（不同均衡）互斥 → 实测结构性退化（§10.4：all-postflop −192~−426）。
/// [`RoundStart`](Self::RoundStart) 从 **betting-round 起点**重解、当前街 betting 落入 subgame 由
/// CFR 解（range 用更可靠的轮起点 reach），并对同一轮的多次行动用 **round-stable seed**（街索引而
/// 非 decision ordinal）→ within-round 多决策共享**字节相同**的 solve、读不同节点 = 一个均衡内自洽
/// （= §6 #2「冻结」的一致性意图，无需显式 reach-pin）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolveRoot {
    /// 从当前决策点为根（MVP 旧行为；byte-equal 保留作 A/B / 回归）。`FlopFirstUnraised` 下
    /// 当前点 = round-start，与 `RoundStart` 等价；`AllPostflop` 下 mid-round 重解撞 landmine。
    CurrentDecision,
    /// 从当前 betting-round 起点为根（§6 #1，默认）。当前街 betting 进 subgame；round-stable
    /// seed 给 within-round 一致性（§6 #2 意图）。
    RoundStart,
}

/// 实时搜索触发 + 求解配置（S6 6a）。`Copy` → 随 `Contestant` 按值带。
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
    /// 触发面。默认 [`SearchTrigger::FlopFirstUnraised`]（验证不退化的安全窄触发面；
    /// `AllPostflop` 须配 [`ResolveRoot::RoundStart`] 才正确，见 [`SearchTrigger`] doc）。
    pub trigger: SearchTrigger,
    /// 重解根（§6 #1）。默认 [`ResolveRoot::RoundStart`]（正确放宽触发面的前置）。
    /// `FlopFirstUnraised` 下两模式等价（当前点 = round-start）；`CurrentDecision` 保留作 A/B。
    pub resolve_root: ResolveRoot,
    /// S6 6b：`true` = **depth-limit** 搜索——子树在当前街边界截断、叶子查 blueprint 续局值表
    /// （绕开深层欠训练节点，§10.5 退化主因）；`false`（默认）= 6a 解到真实终局。`true` 需
    /// [`subgame_search`] 收到 `leaf_values`（否则该次搜索 `Err` 回落 blueprint）。
    pub depth_limit: bool,
}

impl Default for SubgameSearchConfig {
    fn default() -> Self {
        Self {
            iterations: 1000,
            max_subtree_nodes: 8000,
            seed: 0x5347_4D45_5F53_3641, // "SGME_S6A"
            use_blueprint_range: true,
            trigger: SearchTrigger::FlopFirstUnraised,
            resolve_root: ResolveRoot::RoundStart,
            depth_limit: false,
        }
    }
}

/// 触发判据。`AllPostflop`（默认）= 任意 postflop 决策点（flop 含已起注 / turn / river）；
/// `FlopFirstUnraised` = 仅 flop 未起注首决策点（窄触发面，A/B 基线）。preflop 一律不搜
/// （preflop 走 blueprint；且 preflop 中途 entrants ≠ live bitmask，[`live_entrants`] 不适用）。
pub fn should_search(auth: &GameState, trigger: SearchTrigger) -> bool {
    if auth.is_terminal() || auth.current_player().is_none() {
        return false;
    }
    let postflop = matches!(auth.street(), Street::Flop | Street::Turn | Street::River);
    match trigger {
        SearchTrigger::AllPostflop => postflop,
        SearchTrigger::FlopFirstUnraised => {
            auth.street() == Street::Flop && max_committed_this_round(auth) == 0
        }
    }
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

/// postflop entrants bitmask = 所有未弃牌（`Active|AllIn`）座位。到 postflop，任何未弃牌
/// 玩家都必在 preflop 做过 ≥1 非弃牌动作 → entrants bit 必置（preflop 中途不成立，故
/// **只用于 postflop**，与 [`should_search`] 的 preflop-不搜一致）。
fn live_entrants(auth: &GameState) -> u16 {
    let mut e = 0u16;
    for (i, p) in auth.players().iter().enumerate() {
        if matches!(p.status, PlayerStatus::Active | PlayerStatus::AllIn) {
            e |= 1u16 << i;
        }
    }
    e
}

/// tag 是否进攻动作（推进 `raises_on_street`）：`Bet` / `Raise` / `AllIn`（同
/// `nlhe_betting_tree::is_aggression` 的 tag 版）。
fn tag_is_aggression(tag: &AbstractActionTag) -> bool {
    matches!(
        tag,
        AbstractActionTag::Bet(_) | AbstractActionTag::Raise(_) | AbstractActionTag::AllIn
    )
}

/// 现算当前节点的 `raises_on_street`（[`PublicBettingTree::build_subtree`] 需要）：沿 public
/// 决策路径 `decisions`（[`decisions_on_path`]）数**当前街**上的进攻动作数。与 `walk` 的语义
/// 逐字对齐（raises 切街清零、本街每进攻 +1）→ 与 blueprint 全树该节点的 raises 相等，故
/// `drop_small_reraise` 在子树 root 正确判 0.5pot 是开池(0)还是 re-raise(>0)（§10.1 审核 A 的坑，
/// 放宽触发面后由本函数精确解决）。
fn raises_on_current_street(
    decisions: &[(NodeId, AbstractActionTag, PlayerId)],
    tree: &PublicBettingTree,
    current_street: StreetTag,
) -> u32 {
    decisions
        .iter()
        .filter(|(node_id, tag, _)| {
            tree.node(*node_id).street == current_street && tag_is_aggression(tag)
        })
        .count() as u32
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

/// S6 实时搜索：建 subgame、跑 CFR、返回 actor **真实手**在**当前决策点**的策略分布——对齐
/// 调用方 `legal_abs`（影子的合法集），可直接喂
/// [`sample_discrete`](crate::training::sampling::sample_discrete) → `outgoing_action`。
///
/// `auth` = 当前决策点的**权威**中途局（提供读 off 用的真实手 + board + betting 几何）。
/// `root_state` = subgame **根**的状态：[`ResolveRoot::CurrentDecision`] 下调用方传 `auth`；
/// [`ResolveRoot::RoundStart`]（默认，§6 #1）下传**当前 betting-round 起点快照**（决策环每街首
/// 决策点的 `auth.clone()`）。`game` = 该 actor 的 blueprint game（bucket 表 / 同一 action 抽象 +
/// A3×A4 规则，subgame 用**同一套**重建子树）。`node_id` = actor 在 blueprint 树的当前节点（= 影子
/// `current_node_id`；range 估计 + within-round 导航沿其 public 决策路径）。`strategy` = blueprint
/// average strategy 查询面（range 估计用；同质 blueprint 假设见 [`estimate_range`]）。
///
/// **RoundStart（§6 #1/#2）**：从 `root_state`（轮起点）建子树（`raises_on_street = 0`、entrants =
/// 轮起点 live）；range 只用**当前街之前**的 reach（当前街 betting 已落入 subgame、由 CFR 解，不
/// 重复计入 history）；seed 用**街索引**（round-stable）而非 `decision_ordinal` → 同一轮多次行动得
/// **字节相同**的 solve；解完沿 within-round 抽象动作序导航到当前决策点、读真实手该节点策略（用
/// `auth` 几何 query）。round-stable solve + 读不同节点 = 一个均衡内自洽（§6 #2 一致性意图）。
/// **CurrentDecision**：从 `auth` 为根、range 用全 reach、seed 用 `decision_ordinal`、读 root（MVP
/// 旧行为，byte-equal A/B）。
///
/// `cfg.use_blueprint_range`：`true` → root 按 per-seat blueprint range 加权采样底牌（§5b 去
/// confound）；`false` → uniform resample（A/B 对照）。
///
/// 任一失败（auth/root_state 非 decision / 子树越界 / within-round 导航失同步 / 当前桶在
/// `iterations` 内未被访问 / 维度不符 / `legal_abs` 含子树没有的 tag / 对齐后全零）→ `Err`，
/// 调用方按设计 §4.1 回落 blueprint `strategy_distribution`。
#[allow(clippy::too_many_arguments)]
pub fn subgame_search(
    auth: &GameState,
    root_state: &GameState,
    game: &SimplifiedNlheGame,
    legal_abs: &[SimplifiedNlheAction],
    node_id: NodeId,
    strategy: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    cfg: &SubgameSearchConfig,
    leaf_values: Option<&Arc<LeafValueTables>>,
    hand_seed: u64,
    decision_ordinal: u64,
) -> Result<Vec<(SimplifiedNlheAction, f64)>, String> {
    if auth.is_terminal() || auth.current_player().is_none() {
        return Err("subgame_search: auth 非 decision 节点".to_string());
    }
    if root_state.is_terminal() || root_state.current_player().is_none() {
        return Err("subgame_search: root_state 非 decision 节点".to_string());
    }
    let auth_actor = auth.current_player().expect("checked above").0 as PlayerId;

    // 沿 actor 在 blueprint 树的 public 决策路径分流（按 resolve_root）：
    //   - entrants：subgame 根的 live bitmask（CurrentDecision = 当前点 / RoundStart = 轮起点）。
    //   - raises_on_street：subgame 根的当前街进攻数（CurrentDecision 现算 / RoundStart 必 0）。
    //   - range_decisions：估 range 的决策子集（CurrentDecision 全路径 / RoundStart 只当前街之前）。
    //   - within_round_tags：subtree root→当前点的导航序（CurrentDecision 空 = 读 root /
    //     RoundStart = 当前街的 within-round 抽象动作，时序）。
    //   - seed_ordinal：solve seed 的 ordinal（CurrentDecision = decision_ordinal /
    //     RoundStart = 街索引，round-stable → within-round 多决策共享同一 solve）。
    let tree = game.tree();
    let decisions = decisions_on_path(tree, node_id);
    let current_street = tree.node(node_id).street;
    let (entrants, raises_on_street, range_decisions, within_round_tags, seed_ordinal) =
        match cfg.resolve_root {
            ResolveRoot::CurrentDecision => {
                let raises = raises_on_current_street(&decisions, tree, current_street);
                (
                    live_entrants(root_state),
                    raises,
                    decisions,
                    Vec::new(),
                    decision_ordinal,
                )
            }
            ResolveRoot::RoundStart => {
                // 拆 decisions 为「当前街之前」（range）与「当前街内」（within-round 导航）。
                // decisions 是 current→root 逆序，within 反转成 round-start→current 时序。
                let mut prior = Vec::new();
                let mut within = Vec::new();
                for (parent_id, tag, player) in decisions {
                    if (tree.node(parent_id).street as u8) < (current_street as u8) {
                        prior.push((parent_id, tag, player));
                    } else {
                        within.push(tag);
                    }
                }
                within.reverse();
                (
                    live_entrants(root_state),
                    0,
                    prior,
                    within,
                    current_street as u64,
                )
            }
        };

    // §5b range：use_blueprint_range → 为每个未弃牌座位估 marginal range（root 加权采样底牌）；
    // 否则 None = uniform。depth-limit / 解到终局都复用这一份 ranges。
    let ranges_opt: Option<Vec<Vec<f64>>> = if cfg.use_blueprint_range {
        let holes = all_hole_combos();
        let board: Vec<Card> = root_state.board().to_vec();
        let players = root_state.players();
        Some(
            (0..players.len())
                .map(|seat| {
                    if players[seat].hole_cards.is_some() {
                        estimate_range(
                            game,
                            strategy,
                            &range_decisions,
                            &board,
                            seat as PlayerId,
                            &holes,
                        )
                    } else {
                        Vec::new() // 弃牌座：range 不被读。
                    }
                })
                .collect(),
        )
    } else {
        None
    };

    // 建 subgame：同 blueprint 的 bucket 表 / action 抽象 + A3×A4 规则，从 root_state 为根。
    // 6b depth-limit → 子树街边界截断 + 叶子查 blueprint 续局值（new_depth_limited）；否则 6a 解到终局。
    let sub = if cfg.depth_limit {
        let values =
            leaf_values.ok_or_else(|| "depth_limit=true 但未提供 leaf_values".to_string())?;
        // root_state 的 blueprint 全局节点：CurrentDecision = 当前点（= root）；RoundStart =
        // 当前街起点（从 node_id 沿 parent 爬到街起点）。limit_street = root 节点所在街（只解当前街）。
        let root_global = match cfg.resolve_root {
            ResolveRoot::CurrentDecision => node_id,
            ResolveRoot::RoundStart => climb_to_street_start(tree, node_id),
        };
        let limit_street = tree.node(root_global).street;
        SubgameNlheGame::new_depth_limited(
            Arc::clone(&game.bucket_table),
            root_state.config().clone(),
            game.abstraction().clone(),
            game.rules(),
            root_state.clone(),
            entrants,
            raises_on_street,
            ranges_opt,
            limit_street,
            tree,
            root_global,
            Arc::clone(values),
            0, // step-1：固定 unbiased 续局；biased 选择节点 = 6b-4。
        )?
    } else if let Some(ranges) = ranges_opt {
        SubgameNlheGame::new_with_ranges(
            Arc::clone(&game.bucket_table),
            root_state.config().clone(),
            game.abstraction().clone(),
            game.rules(),
            root_state.clone(),
            entrants,
            raises_on_street,
            ranges,
        )
    } else {
        SubgameNlheGame::new(
            Arc::clone(&game.bucket_table),
            root_state.config().clone(),
            game.abstraction().clone(),
            game.rules(),
            root_state.clone(),
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

    // 跑 CFR：master seed + step rng 都由 (cfg.seed, hand_seed, seed_ordinal) 确定派生。
    // RoundStart 下 seed_ordinal = 街索引 → 同一轮多决策的 solve 字节相同（§6 #2 一致性）。
    let master = search_seed(cfg.seed, hand_seed, seed_ordinal);
    let mut trainer = EsMccfrTrainer::new(sub, master);
    let mut srng = ChaCha20Rng::from_seed(master ^ 0xC0FF_EE00_C0FF_EE00);
    for _ in 0..cfg.iterations {
        trainer
            .step(&mut srng)
            .map_err(|e| format!("subgame CFR step 失败: {e:?}"))?;
    }

    // 导航到当前决策点（within-round tags 在 subtree 上重放；CurrentDecision 时 tags 空 → root）。
    let cur_node = navigate_subtree(trainer.game().subtree(), &within_round_tags)?;
    // 导航落点的决策者须 == 权威当前 actor（否则读到错座的真实手 → 失同步）。
    let cur_actor = trainer.game().subtree().node(cur_node).player_acting;
    if cur_actor != auth_actor {
        return Err(format!(
            "within-round 导航落点 actor {cur_actor} ≠ 权威当前 actor {auth_actor}（失同步）"
        ));
    }
    // 取 actor 真实手在 cur_node 的策略（average strategy）。**用 auth 几何 query**——cur_node 是
    // 当前街 betting 后的深层节点，其合法集须用当前几何算（RoundStart 的 round-start template
    // 几何不符，见 query_at doc）。读 off 的 board/真实手在 auth 与 root_state 同（同街不变）。
    let (info, sub_legal) = trainer.game().query_at(cur_node, auth);
    let avg = trainer.average_strategy(&info);
    if avg.is_empty() {
        return Err(
            "subgame 当前决策 infoset 未被 CFR 访问（该 bucket 在 iterations 内未采样到）"
                .to_string(),
        );
    }
    if avg.len() != sub_legal.len() {
        return Err(format!(
            "subgame 当前决策策略维度 {} ≠ 合法动作数 {}",
            avg.len(),
            sub_legal.len()
        ));
    }

    // 按 tag 把 subtree 策略对齐到调用方 legal_abs：返回的动作对象必须是**影子的**
    // （供 outgoing_action / 推进影子复用其 ratio_label/to）。tag 唯一（Bet/Raise 带 ratio），
    // 故一一映射。legal_abs 出现 cur_node 没有的 tag = 影子与 auth 失同步 → Err 回落。
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
                format!("legal_abs tag {tag:?} 不在 cur_node 动作集（影子与 auth 失同步）")
            })?;
        if p.is_finite() && p > 0.0 {
            sum += p;
            out.push((*a, p));
        }
    }
    if !(sum.is_finite() && sum > 0.0) {
        return Err("subgame 当前决策策略对齐 legal_abs 后全零".to_string());
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
    use crate::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
    use crate::training::subgame_leaf_value::{build_leaf_value_tables, default_continuations};

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
            leaf_ctx: None,
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

    /// 两触发面（[`SearchTrigger`]）+ [`raises_on_current_street`] 多档计数。
    /// `AllPostflop` = 任意 postflop 决策点都搜；`FlopFirstUnraised` = 仅 flop 未起注首点。
    /// raises 计数：flop Bet → 1、Bet+Raise → 2（放宽触发面后子树 root 正确判 0.5pot 档）。
    #[test]
    fn should_search_triggers_and_raises_count() {
        use SearchTrigger::{AllPostflop, FlopFirstUnraised};
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let mut rng = ChaCha20Rng::from_seed(0x5333_4541_5243_4831); // "S3EARCH1"
        let drng: &mut dyn RngSource = &mut rng;
        let raises_at = |st: &SimplifiedNlheState| {
            let d = decisions_on_path(game.tree(), st.current_node_id);
            raises_on_current_street(&d, game.tree(), game.tree().node(st.current_node_id).street)
        };

        // preflop：两触发面都不搜。
        let pre = game.root(drng);
        assert!(!should_search(&pre.game_state, AllPostflop), "preflop 不搜");
        assert!(
            !should_search(&pre.game_state, FlopFirstUnraised),
            "preflop 不搜"
        );

        // flop 未起注首决策点：两触发面都搜；entrants=0b11、raises=0。
        let flop = hu_flop_state(&game, 0x5333_4541_5243_4832);
        assert!(should_search(&flop.game_state, AllPostflop));
        assert!(should_search(&flop.game_state, FlopFirstUnraised));
        assert_eq!(live_entrants(&flop.game_state), 0b11, "HU flop 两家 live");
        assert_eq!(raises_at(&flop), 0, "flop 未起注 → raises==0");

        // flop 打一个 Bet → 已起注：AllPostflop 仍搜、FlopFirstUnraised 不搜；raises=1。
        let bet = SimplifiedNlheGame::legal_actions(&flop)
            .into_iter()
            .find(|a| matches!(AbstractActionTag::of(a), AbstractActionTag::Bet(_)))
            .expect("flop 首决策点应有 Bet 档");
        let after_bet = SimplifiedNlheGame::next(flop.clone(), bet, drng);
        assert_eq!(after_bet.game_state.street(), Street::Flop);
        assert!(
            should_search(&after_bet.game_state, AllPostflop),
            "已起注 flop：AllPostflop 仍搜（放宽触发面的关键）"
        );
        assert!(
            !should_search(&after_bet.game_state, FlopFirstUnraised),
            "已起注 flop：窄触发面不搜"
        );
        assert_eq!(raises_at(&after_bet), 1, "1 个 flop Bet → raises==1");

        // 再 Raise → raises=2（多档计数，§10.1 审核 A 的坑由 raises_on_current_street 解决）。
        let raise = SimplifiedNlheGame::legal_actions(&after_bet)
            .into_iter()
            .find(|a| matches!(AbstractActionTag::of(a), AbstractActionTag::Raise(_)))
            .expect("面对 Bet 应有 Raise 档");
        let after_raise = SimplifiedNlheGame::next(after_bet.clone(), raise, drng);
        if after_raise.game_state.street() == Street::Flop && !after_raise.game_state.is_terminal()
        {
            assert_eq!(raises_at(&after_raise), 2, "flop Bet+Raise → raises==2");
        }
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
            should_search(&auth, SearchTrigger::AllPostflop),
            "测试前置：auth 须是 postflop 决策点"
        );
        let legal_abs = SimplifiedNlheGame::legal_actions(&flop);
        assert!(!legal_abs.is_empty(), "flop 决策点合法集非空");

        // accepting cap：HU 默认 {0.5,1,2} flop 子树较大（见 _measure_flop_subtree_sizes），
        // 用大上限确保不被 cap 拒；stub 全归桶 0 → root infoset 必累积 → Ok 确定。
        // use_blueprint_range=true 走 §5b 路径（estimate_range + 加权采样）；uniform 策略 →
        // range 近均匀（验 range 路径不破契约 + 可复现）。
        // RoundStart（默认）+ flop 首决策点：round-start == 当前点（无本街 betting），within
        // tags 空 → 导航回 root；root_state 传 auth（= 该轮起点）。
        let cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            seed: 0xA11C_E55E_5EED_u64,
            use_blueprint_range: true,
            trigger: SearchTrigger::AllPostflop,
            resolve_root: ResolveRoot::RoundStart,
            depth_limit: false,
        };
        let node_id = flop.current_node_id;
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let mut ok_count = 0usize;
        for hand_seed in 0u64..3 {
            let ordinal = 3u64;
            let r1 = subgame_search(
                &auth, &auth, &game, &legal_abs, node_id, &strat, &cfg, None, hand_seed, ordinal,
            );
            let r2 = subgame_search(
                &auth, &auth, &game, &legal_abs, node_id, &strat, &cfg, None, hand_seed, ordinal,
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
            trigger: SearchTrigger::AllPostflop,
            resolve_root: ResolveRoot::RoundStart,
            depth_limit: false,
        };
        let r = subgame_search(
            &auth, &auth, &game, &legal_abs, node_id, &strat, &tiny, None, 0, 0,
        );
        assert!(r.is_err(), "节点上限被触发应回落 Err，实得 {r:?}");
    }

    /// [`navigate_subtree`]：空 tags → root；非法 tag（flop 未起注 root 必无 Call）→ 优雅 Err。
    #[test]
    fn navigate_subtree_empty_and_bad_tag() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x4E41_5654_4553_5430); // "NAVTEST0"
        let sub = SubgameNlheGame::new(
            stub_table(),
            TableConfig::default_hu_200bb(),
            StreetActionAbstraction::default_6_action(),
            BettingAbstractionRules::default(),
            flop.game_state.clone(),
            0b11,
            0,
        );
        let subtree = sub.subtree();
        assert_eq!(
            navigate_subtree(subtree, &[]).expect("空 tags 不应 Err"),
            subtree.root_id(),
            "空 tags → root"
        );
        let root = subtree.node(subtree.root_id());
        assert!(
            !root.legal_actions.contains(&AbstractActionTag::Call),
            "前置：flop 未起注 root 无 Call"
        );
        assert!(
            navigate_subtree(subtree, &[AbstractActionTag::Call]).is_err(),
            "非法 tag → Err（不 panic）"
        );
    }

    /// §6 #1 round-start 重解（within-round 决策点）：在**已起注** flop 决策点，RoundStart 从
    /// 轮起点快照为根、沿 within-round tags 导航到当前决策点、读真实手该节点策略。验：
    /// ①[`navigate_subtree`] 落点 player_acting == 当前 actor（导航正确）；②返回合法归一分布；
    /// ③**round-stable seed**——RoundStart 用街索引而非 decision_ordinal，故不同 ordinal 字节
    /// 相同（= §6 #2 一致性机制：同一轮多决策共享同一 solve、读不同节点自洽）。
    #[test]
    fn round_start_resolve_navigates_within_round_and_reproducible() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x5253_5F4E_4156_3031); // "RS_NAV01"
        let round_start = flop.game_state.clone(); // 轮起点（flop 首决策点，无本街 betting）。
        let rs_actor = round_start.current_player().expect("flop 有行动者");

        // 推进：首行动者 Bet → 进入对手的 within-round flop 决策点。
        let mut rng = ChaCha20Rng::from_seed(0x5253_5F4E_4156_0002);
        let drng: &mut dyn RngSource = &mut rng;
        let bet = SimplifiedNlheGame::legal_actions(&flop)
            .into_iter()
            .find(|a| matches!(AbstractActionTag::of(a), AbstractActionTag::Bet(_)))
            .expect("flop 首决策点应有 Bet 档");
        let sb = SimplifiedNlheGame::next(flop.clone(), bet, drng);
        assert_eq!(sb.game_state.street(), Street::Flop, "Bet 后仍 flop");
        assert!(!sb.game_state.is_terminal() && sb.game_state.current_player().is_some());
        let cur_actor = sb.game_state.current_player().unwrap();
        assert_ne!(cur_actor, rs_actor, "Bet 后换人（HU 对手面对 Bet）");

        let auth = sb.game_state.clone();
        let node_id = sb.current_node_id;
        let legal_abs = SimplifiedNlheGame::legal_actions(&sb);

        // within-round 决策点：AllPostflop 搜、FlopFirstUnraised 不搜（已起注）。
        assert!(should_search(&auth, SearchTrigger::AllPostflop));
        assert!(!should_search(&auth, SearchTrigger::FlopFirstUnraised));

        // ① 直接验导航：从轮起点子树根沿 [Bet] 落到当前 actor 的节点。
        let rs_sub = SubgameNlheGame::new(
            stub_table(),
            TableConfig::default_hu_200bb(),
            StreetActionAbstraction::default_6_action(),
            BettingAbstractionRules::default(),
            round_start.clone(),
            live_entrants(&round_start),
            0,
        );
        let within_tags = [AbstractActionTag::of(&bet)];
        let cur_node = navigate_subtree(rs_sub.subtree(), &within_tags).expect("Bet 导航");
        assert_eq!(
            rs_sub.subtree().node(cur_node).player_acting,
            cur_actor.0 as PlayerId,
            "within-round 导航落点 player_acting == 当前 actor"
        );

        // ②③ subgame_search RoundStart：root_state = 轮起点快照；round-stable seed 验。
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let cfg = SubgameSearchConfig {
            iterations: 400,
            max_subtree_nodes: 1_000_000,
            seed: 0xB0B0_5EED_u64,
            use_blueprint_range: true,
            trigger: SearchTrigger::AllPostflop,
            resolve_root: ResolveRoot::RoundStart,
            depth_limit: false,
        };
        let d9 = subgame_search(
            &auth,
            &round_start,
            &game,
            &legal_abs,
            node_id,
            &strat,
            &cfg,
            None,
            0xABCD,
            9,
        )
        .expect("RoundStart within-round 应 Ok（stub 桶 0 必累积）");
        let d99 = subgame_search(
            &auth,
            &round_start,
            &game,
            &legal_abs,
            node_id,
            &strat,
            &cfg,
            None,
            0xABCD,
            99,
        )
        .expect("RoundStart within-round 应 Ok");
        assert_eq!(d9.len(), d99.len(), "round-stable：维度一致");
        for ((a9, p9), (a99, p99)) in d9.iter().zip(&d99) {
            assert_eq!(a9, a99, "round-stable：动作逐项一致");
            assert_eq!(
                p9.to_bits(),
                p99.to_bits(),
                "round-stable seed：不同 decision_ordinal 字节相同（§6 #2 一致性）"
            );
        }
        let sum: f64 = d9.iter().map(|(_, p)| *p).sum();
        assert!((sum - 1.0).abs() < 1e-9, "返回分布归一，和={sum}");
        for (a, p) in &d9 {
            assert!(*p > 0.0, "只返回正概率动作");
            let tag = AbstractActionTag::of(a);
            assert!(
                legal_abs.iter().any(|l| AbstractActionTag::of(l) == tag),
                "返回动作 {tag:?} 在 legal_abs 内"
            );
        }
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

    /// S6 6b：depth-limit subgame 端到端——[`SubgameNlheGame::new_depth_limited`] 建截断子树 +
    /// 叶子查 blueprint 续局值表，`EsMccfrTrainer` 跑通。验：①截断子树 ≪ 全子树（leaves 截断）；
    /// ②每个 depth-limit 叶子都映射到了 blueprint 全局节点（非 [`u32::MAX`]）、≥1 个；③CFR 跑通
    /// （不 panic、累积策略——证叶子被当 Terminal 走值表 payoff，否则空 legal_actions 会破）；
    /// ④同 seed 两 trainer 逐 infoset byte-equal（可复现）。
    #[test]
    fn depth_limited_subgame_cfr_runs_through_leaf_values() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x4454_4C5F_4347_4D45); // "DTL_CGME"
        let template = flop.game_state.clone();
        let root_global = flop.current_node_id; // flop 决策点的 blueprint 全局节点
        let limit = game.tree().node(root_global).street; // = Flop（只解 flop、叶子在 turn 起点）

        // 叶子值表：同 game 配置的 stub trainer self-play（未训练 ≈ uniform）。
        let trainer = DenseNlheEsMccfrTrainer::new(
            SimplifiedNlheGame::new(stub_table()).expect("HU game"),
            7,
        );
        let values = Arc::new(build_leaf_value_tables(
            &trainer,
            &default_continuations(),
            800,
            0xBEEF,
            400,
        ));

        // 全子树（不截断）作节点数对照。
        let full = PublicBettingTree::build_subtree(
            &template,
            &StreetActionAbstraction::default_6_action(),
            BettingAbstractionRules::default(),
            0,
            0,
        );

        let make = || {
            SubgameNlheGame::new_depth_limited(
                stub_table(),
                TableConfig::default_hu_200bb(),
                StreetActionAbstraction::default_6_action(),
                BettingAbstractionRules::default(),
                template.clone(),
                0,
                0,
                None, // uniform（聚焦 depth-limit + 叶子值链路）
                limit,
                game.tree(),
                root_global,
                Arc::clone(&values),
                0,
            )
            .expect("build depth-limited subgame")
        };

        let sub = make();
        assert!(
            sub.subtree().num_nodes() < full.num_nodes(),
            "depth-limit 子树 {} 应 < 全子树 {}（leaves 截断）",
            sub.subtree().num_nodes(),
            full.num_nodes()
        );
        let ctx = sub
            .leaf_ctx
            .as_ref()
            .expect("depth-limit subgame 应有 leaf_ctx");
        let mut mapped_leaves = 0usize;
        for local in 0..sub.subtree().num_nodes() as NodeId {
            if sub.subtree().node(local).depth_limit_leaf {
                assert_ne!(
                    ctx.global_by_local[local as usize],
                    u32::MAX,
                    "叶子 {local} 应已映射 blueprint 全局节点"
                );
                mapped_leaves += 1;
            }
        }
        assert!(mapped_leaves > 0, "应有 ≥1 个 depth-limit 叶子");

        // CFR 跑通 + 可复现（叶子当 Terminal 走值表 → 不会因空 legal_actions panic）。
        let steps = 400u64;
        let run = |seed: u64| {
            let mut tr = EsMccfrTrainer::new(make(), seed);
            let mut rng = ChaCha20Rng::from_seed(seed ^ 0xC0FF_EE00);
            for _ in 0..steps {
                tr.step(&mut rng).expect("depth-limit subgame step");
            }
            tr
        };
        let a = run(0xD1);
        let b = run(0xD1);
        assert_eq!(a.update_count(), steps);
        assert!(
            !a.strategy_sum().inner().is_empty(),
            "depth-limit subgame CFR 应累积到 ≥1 infoset"
        );
        assert_eq!(
            a.strategy_sum().inner().len(),
            b.strategy_sum().inner().len(),
            "同 seed 两 trainer 表大小一致"
        );
        for (info, _) in a.strategy_sum().inner().iter() {
            assert_eq!(
                a.average_strategy(info),
                b.average_strategy(info),
                "同 seed depth-limit subgame 策略 byte-equal @ {info:?}"
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
            let ent = live_entrants(&s.game_state);
            let decisions = decisions_on_path(g6.tree(), s.current_node_id);
            let rs = raises_on_current_street(
                &decisions,
                g6.tree(),
                g6.tree().node(s.current_node_id).street,
            );
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
