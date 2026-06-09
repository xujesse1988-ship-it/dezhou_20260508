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
use std::time::{Duration, Instant};

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

/// depth-limit 叶子查哪个续局值（[`SubgameLeafCtx::cont_policy`]）。
///
/// # `BiasedNextActor` 为何是「叶子续局选择节点」的闭式等价（§6 #3）
///
/// 设计 §6 #3 要求续局的「选」是 subgame 里**由 CFR 优化的真 action、infoset-level**（非固定
/// per-node min/max）。**关键观察**：depth-limit 叶子**无下游**（截断点之后不再展开）→ 在该叶子
/// 插一个「下一 actor `a` 选续局」的 CFR 决策节点时，`a` 对续局 `c` 的反事实效用 = `value[a][c]`
/// （选 c 后 a 的叶子值），**与任何下游策略无关** → `a` 的 best response 是**纯 argmax**
/// `c* = argmax_c value[a][c]`（regret-matching 对无下游的叶子选择必收敛到纯最优）。故显式 CFR
/// 选择节点收敛后 = 闭式 `c*`，对 traverser 的叶子值 = `value[traverser][c*]`。**二者逐点相等**
/// （ES-MCCFR 下闭式还**去掉了 a 学习的瞬态噪声 + 采样方差** → 更快更稳）。
///
/// `c*` 只依赖 `a` 的 infoset（`a` 的 bucket + 叶子全局节点），**不依赖 traverser 的私牌**
/// （非 clairvoyant）→ 满足 §6 #3「infoset-level，对手按自身信息选」。`a` = 叶子 `player_acting`
/// （下一街首 actor，必在手）→ self-interested opponent（§1.2 endorse，非「全桌串通超级对手」
/// min 那个过保守版）。
///
/// **近似（§11.5）**：值表是「全桌同用 style c」的对称 self-play 值（6b-2 的 (b) 口径），故
/// `c*` = 「若全桌都采 style c 则 a 最优的 c」≈ a 的单边最优 style（非 Modicum 精确的「a 变、
/// 其余 blueprint 固定」）；单 chooser（下一 actor）近似 §1.2「每个对手各自独立选」。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeafContPolicy {
    /// 固定续局 `cont`（6b-3 step-1：0 = unbiased = depth-limit 单值叶子）。
    Fixed(usize),
    /// 6b-4：叶子下一 actor `a` 按自身续局值 **argmax** 选 `c*`，traverser 取 `value[traverser][c*]`
    /// （= 续局选择节点的闭式等价，见类型 doc）。
    BiasedNextActor,
}

/// S6 6b depth-limit：subgame 叶子续局值上下文（[`SimplifiedNlheState::leaf_ctx`] 携带，
/// depth-limit 叶子的 [`SubgameNlheGame::payoff`] 读）。
///
/// `SubgameNlheGame::new_depth_limited` 构建：把子树每个 depth-limit 叶子（本地 `NodeId`）映射
/// 回 **blueprint 全局树** 的街起点 `NodeId`（`global_by_local`，非叶项 = [`u32::MAX`] 哨兵），
/// 配 `values`（[`LeafValueTables`]）+ 续局策略 `cont_policy`，使叶子 `payoff` 能查
/// `E[U | seat, 全局节点, bucket, cont]`。弃牌座的叶子值不查表（= 固定 `−committed_total`）。
pub struct SubgameLeafCtx {
    /// blueprint 叶子续局值表（与构建 subgame 的 blueprint 同源）。
    pub values: Arc<LeafValueTables>,
    /// `global_by_local[local_node]` = 该 depth-limit 叶子对应的 blueprint 全局街起点 `NodeId`；
    /// 非叶节点 = [`u32::MAX`]（不被读）。
    pub global_by_local: Vec<NodeId>,
    /// 续局选择策略（[`LeafContPolicy::Fixed`] = step-1 固定；[`LeafContPolicy::BiasedNextActor`]
    /// = 6b-4 下一 actor argmax）。
    pub cont_policy: LeafContPolicy,
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
    /// uniform）。`cont_policy` = 叶子续局选择策略（[`LeafContPolicy`]）。
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
        cont_policy: LeafContPolicy,
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
            cont_policy,
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
/// **弃牌座**：净收益固定 = `−committed_total`（已投入全损、不依赖 runout / 续局，故不查值表——
/// 值表只对仍在手座位累计，见 [`SubgameLeafCtx`]）。**在手座（Active/AllIn）**：按
/// [`LeafContPolicy`] 选续局 `c`，查 `values.value(player, 全局街起点节点, 该街 bucket, c)`：
/// - `Fixed(c)`：固定续局（step-1，0 = unbiased）。
/// - `BiasedNextActor`（6b-4）：先令叶子下一 actor `a` 按**自身**续局值 argmax 选 `c*`
///   （= 续局选择节点的闭式等价，见 [`LeafContPolicy`] doc），再取 `value[player][c*]`。
///
/// 任一查询缺 → 退 unbiased(cont 0)；仍缺（街起点高频、miss 罕见）→ 0（已知近似，§模块顶部 doc）。
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
    // 选续局 c：Fixed 直接用；BiasedNextActor 令下一 actor a 按自身续局值 argmax（无下游 →
    // 纯 best response = CFR 选择节点收敛值，§见 LeafContPolicy doc）。a = 叶子 player_acting。
    let cont = match ctx.cont_policy {
        LeafContPolicy::Fixed(c) => c,
        LeafContPolicy::BiasedNextActor => {
            let a = state.tree.node(state.current_node_id).player_acting as usize;
            let bucket_a = compute_hand_bucket(state, a as PlayerId, leaf_street);
            argmax_cont(&ctx.values, a, global, bucket_a)
        }
    };
    let bucket = compute_hand_bucket(state, player, leaf_street);
    let v = ctx
        .values
        .value(player as usize, global, bucket, cont)
        .or_else(|| ctx.values.value(player as usize, global, bucket, 0));
    // leaf-miss 遥测：both cont + unbiased 查不到 → 退 0（深街/river 叶子覆盖软肋的可见度）。
    ctx.values.record_leaf_eval(v.is_none());
    v.unwrap_or(0.0)
}

/// 叶子下一 actor `a` 在 `(global 节点, a 的 bucket)` 上**自身**续局值最大的续局 idx（6b-4
/// `BiasedNextActor`）。全续局都 miss（无信号）→ 0（unbiased）。**只看 a 自己的 infoset**
/// （a 的 bucket，非 traverser 私牌）→ infoset-level、非 clairvoyant（§6 #3）。
fn argmax_cont(values: &LeafValueTables, a: usize, global: NodeId, bucket_a: u32) -> usize {
    let mut best_c = 0usize;
    let mut best_v = f64::NEG_INFINITY;
    let mut found = false;
    for c in 0..values.n_cont() {
        if let Some(v) = values.value(a, global, bucket_a, c) {
            if v > best_v {
                best_v = v;
                best_c = c;
                found = true;
            }
        }
    }
    if found {
        best_c
    } else {
        0
    }
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
    /// S6 6b-4：`true` = 叶子续局用 [`LeafContPolicy::BiasedNextActor`]（下一 actor argmax 自身续局
    /// 值 = 续局选择节点闭式等价，Modicum/Pluribus 鲁棒机制）；`false`（默认）= [`LeafContPolicy::Fixed`]
    /// `(0)` unbiased。仅 `depth_limit=true` 时有意义。
    pub biased_leaf: bool,
    /// 缺口①（`realtime_search_openpoker_exec` §2.3 / §4.1 A②）：`true` = 子树解用 **LCFR**
    /// 加权（Brown & Sandholm 2018 linear discounting）——同迭代 / 同 wall 离收敛更近，是限时
    /// 的第一杠杆。period 按 [`iterations`](Self::iterations) 现算小值（≈ `iterations/50`，使
    /// `总更新/period` 落进 [`EsMccfrTrainer::with_lcfr_period`] 要求的 20–100，见 `trainer.rs:332`；
    /// `iterations<50` 时 clamp 到 1）。`false`（默认）= vanilla ES-MCCFR——**保持既有 probe /
    /// advisor / §11.5 A/B 基线 byte-equal、不改生产行为**。机制已在共享 `EsMccfrTrainer`、零新核；
    /// 两条路都确定性（固定迭代 + seed）→ 接 LCFR 后静态选粒度路径仍 byte-equal（设计 §5）。
    /// wall / 收敛曲线 vanilla 与 LCFR 各量一条（A②），故是 config 旗而非写死默认。
    pub lcfr: bool,
    /// 缺口①本体（`realtime_search_openpoker_exec` §2.3 / §4.1 A 收尾③）：`Some(d)` = **墙钟
    /// anytime**——求解循环跑到 [`iterations`](Self::iterations) 上限**或** wall 达 `d` 就停、
    /// 返回当前平均策略（限时打法）。`None`（默认）= 跑满固定 `iterations`（今天的行为，
    /// **保持既有 probe / advisor / §11.5 基线逐 infoset byte-equal、不改生产行为**）。
    ///
    /// budgeted 路径的迭代数取决于机器速度 / 负载（同 `(state,seed)` 可产出不同迭代数）→
    /// **原理上做不到 byte-equal**（§2.3）；可复现性靠 seeded RNG（仅固定迭代档保 byte-equal）+
    /// replay / AIVAT 一致。连一轮迭代都跑不完（`iterations==0`）→ `Err`、调用方直接 fold
    /// （§2.3 降级，不回落 blueprint）；够不够*有用*迭代仍由既有「当前桶未被访问 → `Err`」兜。
    /// `Duration` 是 `Copy`，不破本结构 `derive(Copy)`。
    pub time_budget: Option<Duration>,
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
            biased_leaf: false,
            lcfr: false,
            time_budget: None,
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
        // 6b-4：biased_leaf → 下一 actor argmax 选续局（鲁棒机制）；否则固定 unbiased(0)。
        let cont_policy = if cfg.biased_leaf {
            LeafContPolicy::BiasedNextActor
        } else {
            LeafContPolicy::Fixed(0)
        };
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
            cont_policy,
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
    // 缺口①：LCFR 加权（限时第一杠杆，A②）。period 按 cfg.iterations 现算小值（≈ iterations/50，
    // 落进 with_lcfr_period 要求的 总更新/period∈[20,100]，见 trainer.rs:332）；iterations<50 →
    // clamp 1。fresh trainer（update_count==0）→ with_lcfr_period 前置满足。LCFR rescale 确定性
    // （固定迭代 + seed）→ 仍 byte-equal 可复现；vanilla（默认）保持既有行为不变。
    let base = EsMccfrTrainer::new(sub, master);
    let mut trainer = if cfg.lcfr {
        base.with_lcfr_period((cfg.iterations / 50).max(1))
    } else {
        base
    };
    let mut srng = ChaCha20Rng::from_seed(master ^ 0xC0FF_EE00_C0FF_EE00);
    // 缺口①本体（§2.3）：time_budget=Some → 墙钟 anytime（跑到 iterations 上限或 wall 达预算就停，
    // 此时 iterations 退为安全上界）；None → 跑满固定 iterations（既有行为，byte-equal）。budgeted
    // 下迭代数随机器速度/负载变 → 不可 byte-equal（§2.3），可复现靠固定迭代档 + seeded RNG。
    let deadline = cfg.time_budget.map(|_| Instant::now());
    let mut done: u64 = 0;
    for _ in 0..cfg.iterations {
        trainer
            .step(&mut srng)
            .map_err(|e| format!("subgame CFR step 失败: {e:?}"))?;
        done += 1;
        if let (Some(start), Some(budget)) = (deadline, cfg.time_budget) {
            if start.elapsed() >= budget {
                break;
            }
        }
    }
    if cfg.time_budget.is_some() && done == 0 {
        // 连一轮 CFR 迭代都没完成（iterations==0 的退化配置）→ 直接 fold（§2.3 降级，不回落
        // blueprint）。够不够*有用*迭代由下方「当前桶未被访问 → Err」继续兜。
        return Err("time_budget 内连一轮 CFR 迭代都未完成（→ fold）".to_string());
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
    use crate::core::{ChipAmount, SeatId};
    use crate::rules::action::Action;
    use crate::training::nlhe_betting_tree::{deep_single_pot, first_small_6max};
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
            biased_leaf: false,
            lcfr: false,
            time_budget: None,
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
            biased_leaf: false,
            lcfr: false,
            time_budget: None,
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
            biased_leaf: false,
            lcfr: false,
            time_budget: None,
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
                LeafContPolicy::Fixed(0),
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

    /// S6 6b-4：[`argmax_cont`] 选下一 actor **自身**续局值最大的续局（确定性、受控值表）。
    /// 用 [`LeafValueTables::from_entries_for_test`] 造 seat 0 在 (node 7, bucket 3) 续局值
    /// [1,5,2,3] → argmax=cont 1；无信号 (seat,node,bucket) → 退 cont 0。
    #[test]
    fn argmax_cont_selects_next_actor_best() {
        let values = LeafValueTables::from_entries_for_test(
            2,
            4,
            0,
            &[
                (0, 7, 3, 0, 1.0),
                (0, 7, 3, 1, 5.0),
                (0, 7, 3, 2, 2.0),
                (0, 7, 3, 3, 3.0),
            ],
        );
        assert_eq!(
            argmax_cont(&values, 0, 7, 3),
            1,
            "下一 actor 选自身续局值最大的 cont 1（5.0）"
        );
        assert_eq!(
            argmax_cont(&values, 0, 7, 99),
            0,
            "该 (seat,node,bucket) 无条目（全 miss）→ 退 unbiased 0"
        );
        assert_eq!(argmax_cont(&values, 1, 7, 3), 0, "seat 1 无条目 → 退 0");
    }

    /// S6 6b-4：[`LeafContPolicy::BiasedNextActor`] 端到端——depth-limit subgame 叶子按下一 actor
    /// argmax 选续局，`EsMccfrTrainer` 跑通（叶子 payoff 走 argmax_cont → value 路径不 panic）+
    /// 同 seed byte-equal 可复现。argmax 选择正确性由 [`argmax_cont_selects_next_actor_best`] 钉死。
    #[test]
    fn biased_leaf_subgame_cfr_runs_and_reproducible() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x4254_4C5F_4249_4153); // "BTL_BIAS"
        let template = flop.game_state.clone();
        let root_global = flop.current_node_id;
        let limit = game.tree().node(root_global).street;

        let trainer = DenseNlheEsMccfrTrainer::new(
            SimplifiedNlheGame::new(stub_table()).expect("HU game"),
            11,
        );
        let values = Arc::new(build_leaf_value_tables(
            &trainer,
            &default_continuations(),
            800,
            0xB1A5,
            400,
        ));

        let make = || {
            SubgameNlheGame::new_depth_limited(
                stub_table(),
                TableConfig::default_hu_200bb(),
                StreetActionAbstraction::default_6_action(),
                BettingAbstractionRules::default(),
                template.clone(),
                0,
                0,
                None,
                limit,
                game.tree(),
                root_global,
                Arc::clone(&values),
                LeafContPolicy::BiasedNextActor, // 6b-4：叶子 argmax 续局
            )
            .expect("build biased depth-limited subgame")
        };

        let steps = 400u64;
        let run = |seed: u64| {
            let mut tr = EsMccfrTrainer::new(make(), seed);
            let mut rng = ChaCha20Rng::from_seed(seed ^ 0xB1A5_C0DE);
            for _ in 0..steps {
                tr.step(&mut rng).expect("biased depth-limit subgame step");
            }
            tr
        };
        let a = run(0xB4);
        let b = run(0xB4);
        assert_eq!(a.update_count(), steps);
        assert!(
            !a.strategy_sum().inner().is_empty(),
            "biased depth-limit subgame CFR 应累积到 ≥1 infoset"
        );
        for (info, _) in a.strategy_sum().inner().iter() {
            assert_eq!(
                a.average_strategy(info),
                b.average_strategy(info),
                "同 seed biased 叶子 subgame 策略 byte-equal @ {info:?}"
            );
        }
    }

    /// 缺口①（exec §4.1 A②）：LCFR 接进子树解（[`SubgameSearchConfig::lcfr`]）。验 ① 开 LCFR 仍
    /// **byte-equal 可复现**（同 seed 两次逐项相同 → LCFR period rescale 确定性、静态选粒度路径不破
    /// byte-equal，设计 §5）；② 开/关 LCFR 出**不同**分布（证旗真接进求解、非 no-op：period rescale
    /// 重加权 average 的迭代贡献）；③ 两路都归一、动作都在 `legal_abs`。LCFR 的*均衡正确性*另由现有
    /// Kuhn/Leduc 锚保证（`leduc_es_mccfr_report` / `cfr_leduc`），此处只钉「子树接线 + 可复现 + 生效」。
    #[test]
    fn lcfr_subgame_reproducible_and_active() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x4C43_4652_5347_4D45); // "LCFRSGME"
        let auth = flop.game_state.clone();
        let legal_abs = SimplifiedNlheGame::legal_actions(&flop);
        let node_id = flop.current_node_id;
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // uniform range（聚焦 LCFR 旗本身，排除 §5b range 估计噪声）；600 迭代 → period 12、
        // 50 个 boundary，linear 权重充分（with_lcfr_period doc 的 20–100 区间）。
        let lcfr_cfg = SubgameSearchConfig {
            iterations: 600,
            max_subtree_nodes: 1_000_000,
            seed: 0x1CFA_5EED_u64,
            use_blueprint_range: false,
            trigger: SearchTrigger::AllPostflop,
            resolve_root: ResolveRoot::RoundStart,
            depth_limit: false,
            biased_leaf: false,
            lcfr: true,
            time_budget: None,
        };
        let run = |cfg: &SubgameSearchConfig| {
            subgame_search(
                &auth, &auth, &game, &legal_abs, node_id, &strat, cfg, None, 0x9999, 7,
            )
            .expect("subgame_search 应 Ok（stub 桶 0 → root infoset 必累积）")
        };
        // ① 开 LCFR：同 (seed, hand_seed, ordinal) 两次 byte-equal。
        let a = run(&lcfr_cfg);
        let b = run(&lcfr_cfg);
        assert_eq!(a.len(), b.len(), "LCFR 可复现：维度一致");
        for ((a1, p1), (a2, p2)) in a.iter().zip(&b) {
            assert_eq!(a1, a2, "LCFR 可复现：动作逐项一致");
            assert_eq!(p1.to_bits(), p2.to_bits(), "LCFR 可复现：概率 byte-equal");
        }
        // ③ 两路归一、动作合法。
        let vanilla_cfg = SubgameSearchConfig {
            lcfr: false,
            ..lcfr_cfg
        };
        let v = run(&vanilla_cfg);
        for d in [&a, &v] {
            let sum: f64 = d.iter().map(|(_, p)| *p).sum();
            assert!((sum - 1.0).abs() < 1e-9, "分布须归一，和={sum}");
            for (act, p) in d.iter() {
                assert!(*p > 0.0, "只返回正概率动作");
                let tag = AbstractActionTag::of(act);
                assert!(
                    legal_abs.iter().any(|l| AbstractActionTag::of(l) == tag),
                    "返回动作 {tag:?} 在 legal_abs 内"
                );
            }
        }
        // ② 开/关 LCFR 出不同分布（period rescale 改 average 的迭代加权 → 非 no-op）。
        let differ = a.len() != v.len()
            || a.iter()
                .zip(&v)
                .any(|((_, pa), (_, pv))| (pa - pv).abs() > 1e-9);
        assert!(
            differ,
            "开/关 LCFR 应出不同分布（证 lcfr 旗真接进子树解、非 no-op）"
        );
    }

    /// 缺口①本体（exec §2.3 / §4.1 A 收尾③）：`time_budget` 墙钟 anytime 求解路径。验
    /// ① **预算不绑定**（远大于实际 wall）时退化为固定迭代路径、与 `time_budget=None` 逐项
    /// byte-equal——这就是「默认 None 不改既有 probe/advisor/§11.5 基线」承诺的硬证；
    /// ② **预算极小绑定**时优雅停（no-panic）、若 `Ok` 则归一 + 动作合法。限时打法迭代数随机器
    /// 变、原理上做不到 byte-equal（§2.3），故 budgeted 路径只验合法性 + 不 panic，不验可复现。
    #[test]
    fn time_budget_anytime_stops_and_is_valid() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x5447_4254_5F41_3242); // "TGBT_A2B"
        let auth = flop.game_state.clone();
        let legal_abs = SimplifiedNlheGame::legal_actions(&flop);
        let node_id = flop.current_node_id;
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];

        // 基准：固定 300 迭代、time_budget=None（uniform range 聚焦 time_budget 机制本身）。
        let none_cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            seed: 0x7B0D_6E7A_5EED_u64,
            use_blueprint_range: false,
            trigger: SearchTrigger::AllPostflop,
            resolve_root: ResolveRoot::RoundStart,
            depth_limit: false,
            biased_leaf: false,
            lcfr: false,
            time_budget: None,
        };
        let base = subgame_search(
            &auth, &auth, &game, &legal_abs, node_id, &strat, &none_cfg, None, 0x55, 4,
        )
        .expect("None 路径应 Ok（stub 桶 0 → root infoset 必累积）");

        // ① 预算 60s 远不绑定（300 迭代 HU flop ~数十 ms）→ 跑满 300 迭代、deadline 从不触发 →
        // 与 None 路径逐项 byte-equal（Instant 只读不入 RNG/trainer，不引入非确定性）。
        let loose_cfg = SubgameSearchConfig {
            time_budget: Some(Duration::from_secs(60)),
            ..none_cfg
        };
        let loose = subgame_search(
            &auth, &auth, &game, &legal_abs, node_id, &strat, &loose_cfg, None, 0x55, 4,
        )
        .expect("预算不绑定路径应 Ok");
        assert_eq!(base.len(), loose.len(), "预算不绑定 → 与 None 路径同维度");
        for ((a1, p1), (a2, p2)) in base.iter().zip(&loose) {
            assert_eq!(a1, a2, "预算不绑定 → 动作逐项 == None 路径");
            assert_eq!(
                p1.to_bits(),
                p2.to_bits(),
                "预算不绑定 → 概率 byte-equal == None 路径（默认 None 不改既有行为的硬证）"
            );
        }

        // ② 预算 1ns 极小绑定：每 step 后检查 elapsed → 跑 1 迭代即停。no-panic；若 Ok 则归一 + 合法。
        let tight_cfg = SubgameSearchConfig {
            iterations: 1_000_000,
            time_budget: Some(Duration::from_nanos(1)),
            ..none_cfg
        };
        let tight = subgame_search(
            &auth, &auth, &game, &legal_abs, node_id, &strat, &tight_cfg, None, 0x55, 4,
        );
        // Err 合法（限时太紧 → 当前桶未访问 → 调用方 fold，§2.3）；不 panic 已由执行到此处证。
        if let Ok(d) = tight {
            let sum: f64 = d.iter().map(|(_, p)| *p).sum();
            assert!((sum - 1.0).abs() < 1e-9, "限时停后若 Ok 须归一，和={sum}");
            for (act, p) in &d {
                assert!(*p > 0.0, "只返回正概率动作");
                let tag = AbstractActionTag::of(act);
                assert!(
                    legal_abs.iter().any(|l| AbstractActionTag::of(l) == tag),
                    "返回动作 {tag:?} 在 legal_abs 内"
                );
            }
        }
    }

    // ======================================================================
    // 步 A①（exec §4.1 / §5 把关）：引擎在各种码深下都正确——build_subtree + subgame
    // 跑到真实终局，payouts() per-seat Σ==0（守恒）、SPR/all-in 阈值和真实 per-seat 栈一致、
    // resample 保留下注几何、byte-equal 可复现。样例含**不对称栈**（hero 200BB vs 60BB）+
    // **多人 side-pot 中途根**（3 座、短码 BB preflop all-in）+ 深码对称。现有 subgame 测试只
    // 覆盖对称 200BB；这里补 §0.3「现场求解天生处理不对称栈」的关键前提（必须验过才能当前提用）。
    // payout *数值* 的 oracle 复用 `tests/side_pots.rs`（直接 apply）；这里钉的是**经 build_subtree
    // 那条路**守恒不破 + 阈值一致。
    // ======================================================================

    /// 自定义 per-seat 起始码的 HU game（不对称 / 深码）。`stacks` = [seat0, seat1] chips。
    fn hu_game_with_stacks(stacks: [u64; 2]) -> SimplifiedNlheGame {
        let cfg = TableConfig {
            n_seats: 2,
            starting_stacks: vec![ChipAmount::new(stacks[0]), ChipAmount::new(stacks[1])],
            small_blind: ChipAmount::new(50),
            big_blind: ChipAmount::new(100),
            ante: ChipAmount::ZERO,
            button_seat: SeatId(0),
        };
        SimplifiedNlheGame::new_with_abstraction(
            stub_table(),
            cfg,
            StreetActionAbstraction::default_6_action(),
            BettingAbstractionRules::default(),
        )
        .expect("custom-stack HU game")
    }

    /// 3 座**多人 side-pot 中途根**：BTN/SB 深码、BB 短码 preflop all-in（call-for-less），
    /// 深码跟到更高额 → flop 决策点带真实 side pot（main = 3×短码、side = 2×(深注−短码)）。
    /// 返回 flop 首决策态（SB 先动、BB 已 AllIn）。栈取小值让 flop 子树小（短码全压 + 深码浅）。
    fn multiway_side_pot_flop_state(seed: u64) -> GameState {
        let cfg = TableConfig {
            n_seats: 3,
            // seat0=BTN 60BB, seat1=SB 60BB, seat2=BB 25BB。
            starting_stacks: vec![
                ChipAmount::new(6_000),
                ChipAmount::new(6_000),
                ChipAmount::new(2_500),
            ],
            small_blind: ChipAmount::new(50),
            big_blind: ChipAmount::new(100),
            ante: ChipAmount::ZERO,
            button_seat: SeatId(0),
        };
        let mut rng = ChaCha20Rng::from_seed(seed);
        let mut gs = GameState::with_rng(&cfg, seed, &mut rng);
        // 3-handed preflop 行动序：UTG=BTN(seat0) 先动 → SB(seat1) → BB(seat2)。
        // BTN(seat0) open 30BB；SB(seat1) 跟到 3000。
        gs.apply(Action::Raise {
            to: ChipAmount::new(3_000),
        })
        .expect("btn open raise");
        gs.apply(Action::Call).expect("sb call 30BB");
        // BB(seat2, 2500) 面对 3000：Call 自动 cap 到 2500 = all-in-for-less（< 3000 不重开）。
        gs.apply(Action::Call).expect("bb call-for-less = all-in");
        assert_eq!(gs.street(), Street::Flop, "preflop 轮闭 → flop");
        assert!(
            !gs.is_terminal() && gs.current_player().is_some(),
            "flop 多人 side-pot 决策点"
        );
        // 前置：真有 side pot（深码 committed > 短码 committed）+ 短码已 AllIn。
        let p = gs.players();
        assert!(
            p[0].committed_total > p[2].committed_total
                && p[1].committed_total > p[2].committed_total,
            "深码 committed 应 > 短码（side pot 存在）"
        );
        assert!(
            matches!(p[2].status, PlayerStatus::AllIn),
            "短码 BB 应 AllIn"
        );
        gs
    }

    /// N 座**等栈 limped flop**（A 收尾② 多人目标树）：preflop 全员 limp/complete、BB check →
    /// 见 flop 时 N 人全 live（无 side pot、干净多人 postflop 决策点）。驱动直接 apply 到**真
    /// `GameState`**（不过抽象层 width_redirect——否则第 (N+1) entrant 会被 redirect fold，见
    /// `nlhe_betting_tree::filter_actions`）。给 wall/conv 量「实时解 N-way 子树」的多人维度。
    fn nway_limped_flop_state(n_seats: u8, stack: u64, seed: u64) -> GameState {
        let cfg = TableConfig {
            n_seats,
            starting_stacks: vec![ChipAmount::new(stack); n_seats as usize],
            small_blind: ChipAmount::new(50),
            big_blind: ChipAmount::new(100),
            ante: ChipAmount::ZERO,
            button_seat: SeatId(0),
        };
        let mut rng = ChaCha20Rng::from_seed(seed);
        let mut gs = GameState::with_rng(&cfg, seed, &mut rng);
        let mut guard = 0;
        while gs.street() == Street::Preflop && !gs.is_terminal() {
            // 能 check 就 check（BB option），否则 call（limp/complete）；limp-only 线两者其一必有。
            let la = gs.legal_actions();
            let action = if la.check {
                Action::Check
            } else if la.call.is_some() {
                Action::Call
            } else {
                break; // 退化（不该在 limp 线发生）→ 跳出，下方断言暴露。
            };
            gs.apply(action).expect("limp/complete/check 应合法");
            guard += 1;
            assert!(guard < 64, "preflop 驱动未在 64 步内进 flop");
        }
        assert_eq!(gs.street(), Street::Flop, "全员 limp → 见 flop");
        assert!(
            !gs.is_terminal() && gs.current_player().is_some(),
            "flop 多人决策点"
        );
        let live = gs
            .players()
            .iter()
            .filter(|p| matches!(p.status, PlayerStatus::Active | PlayerStatus::AllIn))
            .count();
        assert_eq!(live, n_seats as usize, "limped flop 应 N 人全 live");
        gs
    }

    /// 用 default {0.5,1,2} 抽象 + 默认 rules 从 `template` 建解到终局的 subgame（A① 守恒/wall 共用）。
    fn build_base_subgame(template: &GameState) -> SubgameNlheGame {
        SubgameNlheGame::new(
            stub_table(),
            template.config().clone(),
            StreetActionAbstraction::default_6_action(),
            BettingAbstractionRules::default(),
            template.clone(),
            0,
            0,
        )
    }

    /// 把 HU 默认 game 用 **passive 推进**（check 优先、否则 call/complete）驱动到 `target` 街的
    /// 首决策点（ε 标定用 river/turn 小子树）。`target == Flop` 等价 [`hu_flop_state`]，更深街多走
    /// 几轮 check-check。返回该 `SimplifiedNlheState`（其 `game_state` 作 subgame template）。
    fn hu_state_at(game: &SimplifiedNlheGame, seed: u64, target: Street) -> SimplifiedNlheState {
        let mut rng = ChaCha20Rng::from_seed(seed);
        let rng: &mut dyn RngSource = &mut rng;
        let mut state = game.root(rng);
        let mut guard = 0;
        while state.game_state.street() != target && !state.game_state.is_terminal() {
            let la = SimplifiedNlheGame::legal_actions(&state);
            let pick = la
                .iter()
                .copied()
                .find(|a| matches!(a, AbstractAction::Check))
                .or_else(|| {
                    la.iter()
                        .copied()
                        .find(|a| AbstractActionTag::of(a) == AbstractActionTag::Call)
                })
                .expect("passive check/call 应可用（推进到下一街）");
            state = SimplifiedNlheGame::next(state, pick, rng);
            guard += 1;
            assert!(guard < 32, "未在 32 步内到 {target:?}");
        }
        assert_eq!(state.game_state.street(), target, "应到目标街");
        assert!(!state.game_state.is_terminal() && state.game_state.current_player().is_some());
        state
    }

    /// `traverser` 在 subgame root 的 **MC root EV**（δ_conv 度量）：两家都按 `trainer` 的**平均
    /// 策略**行动（infoset 未访问 → uniform）、root 每次重发隐藏牌，平均 `rollouts` 次终局 payoff。
    /// CFR 收敛时 avg-vs-avg root EV → 子博弈值，故 `|EV(短解) − EV(M 参考)|` 量收敛距离。
    /// 注：独立 RNG、未做 common-random-numbers → EV 差带 MC 噪声（~pot/√rollouts），真阈值标定
    /// 在真桶表 + 更大 rollouts 上做（本诊断为机制）。
    fn mc_root_ev(
        trainer: &EsMccfrTrainer<SubgameNlheGame>,
        traverser: PlayerId,
        rollouts: usize,
        seed: u64,
    ) -> f64 {
        let mut rng = ChaCha20Rng::from_seed(seed);
        let mut total = 0.0_f64;
        for _ in 0..rollouts {
            let mut st = trainer.game().root(&mut rng);
            let mut guard = 0;
            loop {
                match SubgameNlheGame::current(&st) {
                    NodeKind::Terminal => break,
                    NodeKind::Chance => {
                        // subgame root 已重发全部隐藏牌 → 正常无 mid-hand chance；防御性按分布采。
                        let dist = SubgameNlheGame::chance_distribution(&st);
                        let pairs: Vec<(usize, f64)> =
                            dist.iter().enumerate().map(|(i, (_, p))| (i, *p)).collect();
                        let idx = sample_discrete(&pairs, &mut rng);
                        st = SubgameNlheGame::next(st, dist[idx].0, &mut rng);
                    }
                    NodeKind::Player(actor) => {
                        let info = SubgameNlheGame::info_set(&st, actor);
                        let actions = SubgameNlheGame::legal_actions(&st);
                        let n = actions.len();
                        let avg = trainer.average_strategy(&info);
                        // average strategy 常含**零**分量（纯策略不打的动作）；sample_discrete 要求
                        // 全 > 0 → 必须剔零并**重归一**（同 advisor decide，防 FP 和漂移超 1e-12 容差）。
                        // 未访问 / 维度不符 / 剔零后空 → uniform 兜底。
                        let dist: Vec<(usize, f64)> = if avg.len() == n {
                            let s: f64 = avg.iter().filter(|p| p.is_finite() && **p > 0.0).sum();
                            if s > 0.0 {
                                avg.iter()
                                    .enumerate()
                                    .filter(|(_, p)| p.is_finite() && **p > 0.0)
                                    .map(|(i, p)| (i, *p / s))
                                    .collect()
                            } else {
                                let u = 1.0 / n as f64;
                                (0..n).map(|i| (i, u)).collect()
                            }
                        } else {
                            let u = 1.0 / n as f64;
                            (0..n).map(|i| (i, u)).collect()
                        };
                        let idx = sample_discrete(&dist, &mut rng);
                        st = SubgameNlheGame::next(st, actions[idx], &mut rng);
                    }
                }
                guard += 1;
                assert!(guard < 256, "MC rollout 未在 256 步终止");
            }
            total += SubgameNlheGame::payoff(&st, traverser);
        }
        total / rollouts as f64
    }

    /// 从 subgame root uniform-random 走子树到真实终局，返回终局 GameState（读 payouts 守恒）。
    fn rollout_to_terminal(sub: &SubgameNlheGame, seed: u64) -> GameState {
        let mut rng = ChaCha20Rng::from_seed(seed);
        let mut st = sub.root(&mut rng);
        let mut guard = 0;
        while !st.game_state.is_terminal() {
            let la = SimplifiedNlheGame::legal_actions(&st);
            assert!(!la.is_empty(), "非终局决策点应有合法动作");
            let pick = la[(rng.next_u64() % la.len() as u64) as usize];
            st = SimplifiedNlheGame::next(st, pick, &mut rng);
            guard += 1;
            assert!(
                guard < 256,
                "rollout 未在 256 步内终止（疑似建树/转移 bug）"
            );
        }
        st.game_state
    }

    /// resample 后下注几何（board / committed / stack / status）与 template 逐座一致（仅隐藏牌变），
    /// 且满足 I-001（Σ stack + pot == Σ starting_stacks）。subgame 守恒的根基。
    fn assert_resample_preserves_geometry(template: &GameState, sub: &SubgameNlheGame, seed: u64) {
        let mut rng = ChaCha20Rng::from_seed(seed);
        let r = sub.root(&mut rng).game_state;
        assert_eq!(r.board(), template.board(), "board 保留");
        for (i, (rp, tp)) in r.players().iter().zip(template.players()).enumerate() {
            assert_eq!(
                rp.committed_total, tp.committed_total,
                "seat {i} committed_total"
            );
            assert_eq!(
                rp.committed_this_round, tp.committed_this_round,
                "seat {i} cmt_round"
            );
            assert_eq!(rp.stack, tp.stack, "seat {i} stack");
            assert!(rp.status == tp.status, "seat {i} status 保留");
        }
        let sum_stack: u64 = r.players().iter().map(|p| p.stack.as_u64()).sum();
        let starting: u64 = template
            .config()
            .starting_stacks
            .iter()
            .map(|c| c.as_u64())
            .sum();
        assert_eq!(
            sum_stack + r.pot().as_u64(),
            starting,
            "I-001：Σ stack + pot == Σ starting"
        );
    }

    /// 多 seed rollout 到终局，每个终局 payouts per-seat Σ==0（守恒）；同 seed byte-equal。
    fn assert_conserves_over_rollouts(sub: &SubgameNlheGame, n_seeds: u64) {
        for s in 0..n_seeds {
            let term = rollout_to_terminal(sub, 0xC04F_0000 ^ s);
            let payouts = term.payouts().expect("终局应有 payouts");
            let sum: i64 = payouts.iter().map(|(_, n)| *n).sum();
            assert_eq!(sum, 0, "seed {s}：payouts Σ 须 == 0（守恒）");
            // byte-equal：同 seed 再走一遍，终局 payouts 逐项相同。
            let term2 = rollout_to_terminal(sub, 0xC04F_0000 ^ s);
            assert_eq!(
                term.payouts().unwrap(),
                term2.payouts().unwrap(),
                "seed {s}：同 seed rollout payouts byte-equal"
            );
        }
    }

    /// A①·深码对称：HU 200BB（相对 100BB blueprint = 深码）flop 子博弈守恒 + 几何保留。
    #[test]
    fn deep_stack_subgame_conserves() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU 200BB game");
        let flop = hu_flop_state(&game, 0x4445_4550_3230_3042)
            .game_state
            .clone(); // "DEEP200B"
        let sub = build_base_subgame(&flop);
        assert_resample_preserves_geometry(&flop, &sub, 0xD0);
        assert_conserves_over_rollouts(&sub, 24);
    }

    /// A①·不对称栈：hero(seat0)=200BB vs 对手(seat1)=60BB。验①守恒 + 几何保留；②**SPR/all-in
    /// 阈值取真实 per-seat 栈**——flop 行动者（短码 BB）的 all-in 额 = 其 `committed_this_round +
    /// stack`（≈60BB），且**严格小于**深码对手的栈（证引擎按真码深算、非「都当 100BB / 都当深码」）。
    /// 这是 §0.3「现场求解天生处理不对称栈」当前提前必须验的点。
    #[test]
    fn asymmetric_stack_subgame_conserves_and_caps_at_real_stack() {
        let game = hu_game_with_stacks([20_000, 6_000]); // seat0 200BB, seat1 60BB
        let flop = hu_flop_state(&game, 0x4153_594D_4D45_5452)
            .game_state
            .clone(); // "ASYMMETR"
                      // 短码 BB(seat1) 先动；all-in 阈值 = 真实 per-seat cap，且 < 深码栈。
        let actor = flop.current_player().expect("flop 行动者").0 as usize;
        assert_eq!(actor, 1, "HU flop 由 BB(seat1) 先动");
        let short = &flop.players()[1];
        let deep = &flop.players()[0];
        let cap = short.committed_this_round + short.stack;
        assert_eq!(
            flop.legal_actions().all_in_amount,
            Some(cap),
            "短码 all-in 阈值 = 其真实 committed_this_round + stack"
        );
        assert!(
            short.stack < deep.stack,
            "栈确实不对称（短码 {} < 深码 {}）",
            short.stack.as_u64(),
            deep.stack.as_u64()
        );
        let sub = build_base_subgame(&flop);
        assert_resample_preserves_geometry(&flop, &sub, 0xA5);
        assert_conserves_over_rollouts(&sub, 24);
    }

    /// A①·多人 side-pot 中途根：3 座、短码 BB preflop all-in，flop 决策点带真实 side pot。
    /// 验 build_subtree 那条路解到终局 per-seat payouts Σ==0（side pot 记账不破）+ 几何保留。
    /// payout *数值* oracle = `tests/side_pots.rs`（直接 apply 的 2/3/4-way side pot）。
    #[test]
    fn multiway_side_pot_subgame_conserves() {
        let flop = multiway_side_pot_flop_state(0x4D57_5349_4445_5F31); // "MWSIDE_1"
        let sub = build_base_subgame(&flop);
        assert_eq!(sub.n_players(), 3, "3 座 multiway");
        assert_resample_preserves_geometry(&flop, &sub, 0x3A);
        assert_conserves_over_rollouts(&sub, 32);
    }

    /// 诊断（非门槛，exec §4.1 A 收尾②）：子树解 **单决策 wall 回归曲线**（vanilla vs LCFR）+
    /// **收敛距离**（实时解 ≈ 同 game 离线高迭代解的 per-infoset 平均策略 L1）。目标树 = 中等
    /// （HU 200BB / 200v60 / 3way side-pot，default {0.5,1,2}）+ **真目标树**（HU 500BB / 4way /
    /// 5way，`deep_single_pot` {1pot} 解到终局，缺口③④）——直接回答「5/10/20s 在真深码/多人树上
    /// 能解到多少迭代」。节点数 > `HEAVY_NODE_CAP` 的树只报节点数 + 短探针 µs/iter、跳满 ladder
    /// （明确 log、不静默截断；节点数本身即「单线程是否可解」的结论）。wall 用 `Instant`（仅测量，
    /// 不入确定性求解路径）。`cargo test -p poker --lib --release -- --ignored --nocapture
    /// _measure_subgame_wall_and_convergence`（须 --release，wall 才有意义；多人树较大、跑较久）。
    #[test]
    #[ignore = "诊断：子树解 wall（vanilla/LCFR）+ 收敛 L1；--release --ignored --nocapture 跑"]
    fn _measure_subgame_wall_and_convergence() {
        let deep200 = hu_game_with_stacks([20_000, 20_000]); // HU 200BB
        let deep200_flop = hu_flop_state(&deep200, 0xDEE9_5747_0000_0001)
            .game_state
            .clone();
        let asym = hu_game_with_stacks([20_000, 6_000]); // 200 vs 60
        let asym_flop = hu_flop_state(&asym, 0xA59E_0000_0000_0001)
            .game_state
            .clone();
        let mw3_flop = multiway_side_pot_flop_state(0x6D77_0000_0000_0001);
        // A 收尾②（缺口③④）真目标树：HU 500BB {1pot} 解到终局 + 4/5-way limped {1pot}。多人
        // 取中等码深（4way 100BB / 5way 60BB）——深码×多人叠加是单独设计（§2.2），首测不堆满。
        let deep500 = hu_game_with_stacks([50_000, 50_000]); // HU 500BB
        let deep500_flop = hu_flop_state(&deep500, 0xD509_0000_0000_0001)
            .game_state
            .clone();
        let mw4_flop = nway_limped_flop_state(4, 10_000, 0x6D34_0000_0000_0001);
        let mw5_flop = nway_limped_flop_state(5, 6_000, 0x6D35_0000_0000_0001);
        // deep=true → deep_single_pot {1pot} 菜单（缺口③）；false → 既有 default {0.5,1,2}。
        let build = |tmpl: &GameState, deep: bool| -> SubgameNlheGame {
            let (abs, rules) = if deep {
                deep_single_pot()
            } else {
                (
                    StreetActionAbstraction::default_6_action(),
                    BettingAbstractionRules::default(),
                )
            };
            SubgameNlheGame::new(
                stub_table(),
                tmpl.config().clone(),
                abs,
                rules,
                tmpl.clone(),
                0,
                0,
            )
        };
        let targets: [(&str, &GameState, bool); 6] = [
            ("hu_200bb_flop", &deep200_flop, false),
            ("asym_200v60_flop", &asym_flop, false),
            ("mw_3way_sidepot_flop", &mw3_flop, false),
            ("hu_500bb_flop_1pot", &deep500_flop, true),
            ("mw_4way_100bb_flop_1pot", &mw4_flop, true),
            ("mw_5way_60bb_flop_1pot", &mw5_flop, true),
        ];
        // 树太大（>cap）→ 单线程跑满 ladder 不现实：只报节点数 + 短探针 µs/iter、明确 log 跳过
        // （不静默截断）。external-sampling 每迭代是一条轨迹、µs/iter 仍可测；node 计数本身即结论。
        const HEAVY_NODE_CAP: usize = 2_000_000;

        // --- wall：(nodes, iters) → 单决策 wall，vanilla 与 LCFR 各一条 ---
        eprintln!("[A2-wall] target,nodes,iters,variant,wall_ms,us_per_iter");
        for (name, tmpl, deep) in targets {
            let nodes = build(tmpl, deep).subtree().num_nodes();
            if nodes > HEAVY_NODE_CAP {
                let mut tr = EsMccfrTrainer::new(build(tmpl, deep), 0xA5A5_0000);
                let mut rng = ChaCha20Rng::from_seed(0xA5A5_0000 ^ 0xC0FF_EE00);
                let t0 = Instant::now();
                for _ in 0..300u64 {
                    tr.step(&mut rng).expect("probe step");
                }
                let wall = t0.elapsed();
                eprintln!(
                    "[A2-wall] {name},{nodes},300,probe_only,{:.3},{:.3} (nodes>{HEAVY_NODE_CAP} → 跳满 ladder + conv)",
                    wall.as_secs_f64() * 1e3,
                    wall.as_micros() as f64 / 300.0,
                );
                continue;
            }
            for &iters in &[300u64, 1_000, 3_000, 10_000, 30_000] {
                for lcfr in [false, true] {
                    let base = EsMccfrTrainer::new(build(tmpl, deep), 0xA5A5_0000);
                    let mut tr = if lcfr {
                        base.with_lcfr_period((iters / 50).max(1))
                    } else {
                        base
                    };
                    let mut rng = ChaCha20Rng::from_seed(0xA5A5_0000 ^ 0xC0FF_EE00);
                    let t0 = Instant::now();
                    for _ in 0..iters {
                        tr.step(&mut rng).expect("step");
                    }
                    let wall = t0.elapsed();
                    eprintln!(
                        "[A2-wall] {name},{nodes},{iters},{},{:.3},{:.3}",
                        if lcfr { "lcfr" } else { "vanilla" },
                        wall.as_secs_f64() * 1e3,
                        wall.as_micros() as f64 / iters as f64,
                    );
                }
            }
        }

        // --- 收敛距离（粗 sanity）：实时解(N) vs 离线参考(M=50000) per-infoset 平均策略 L1。
        //     ε/δ_conv 精确阈值改用 river/turn 小可枚举子树（见 _measure_convergence_calibration）
        //     ——大 flop 子树欠采样不适合定阈（§4.1）。这里只看 L1 是否随迭代单调降。---
        eprintln!("[A1-conv] target,iters,ref,mean_l1,max_l1,infosets");
        for (name, tmpl, deep) in targets {
            if build(tmpl, deep).subtree().num_nodes() > HEAVY_NODE_CAP {
                eprintln!("[A1-conv] {name} 跳过（nodes > {HEAVY_NODE_CAP}）");
                continue;
            }
            let reference = {
                let mut tr = EsMccfrTrainer::new(build(tmpl, deep), 0xC0DE_0000);
                let mut rng = ChaCha20Rng::from_seed(0xC0DE_0000 ^ 0xC0FF_EE00);
                for _ in 0..50_000 {
                    tr.step(&mut rng).expect("ref step");
                }
                tr
            };
            for &iters in &[300u64, 1_000, 3_000, 10_000] {
                let mut tr = EsMccfrTrainer::new(build(tmpl, deep), 0xC0DE_0000);
                let mut rng = ChaCha20Rng::from_seed(0xC0DE_0000 ^ 0xC0FF_EE00);
                for _ in 0..iters {
                    tr.step(&mut rng).expect("step");
                }
                let (mut sum_l1, mut max_l1, mut n) = (0.0f64, 0.0f64, 0usize);
                for (info, _) in tr.strategy_sum().inner().iter() {
                    let a = tr.average_strategy(info);
                    let b = reference.average_strategy(info);
                    if !b.is_empty() && a.len() == b.len() {
                        let l1: f64 = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).sum();
                        sum_l1 += l1;
                        max_l1 = max_l1.max(l1);
                        n += 1;
                    }
                }
                eprintln!(
                    "[A1-conv] {name},{iters},50000,{:.4},{:.4},{n}",
                    if n > 0 { sum_l1 / n as f64 } else { 0.0 },
                    max_l1,
                );
            }
        }
    }

    /// 诊断（非门槛，exec §4.1 A 收尾①）：ε/δ_conv 收敛距离**机制**——在 **river/turn 小可枚举
    /// 子树**上量 ① per-infoset 平均策略 L1（vs M=50000 参考）+ ② **root EV 差 δ_conv**（avg-vs-avg
    /// MC）。river/turn 子树小、CFR 收敛快 → L1/EV 有意义（大 flop 子树欠采样不适合定阈，§4.1，故
    /// 移出 `_measure_subgame_wall_and_convergence` 的粗 sanity）。
    ///
    /// ⚠ **stub 桶表 = 退化 ε**：postflop 全归桶 0 → 同街只有少数 infoset、无 per-hand 变化 →
    /// L1/EV 偏乐观。**最终 ε/δ_conv 阈值须在真桶表上跑**（`stub_table()` 换 `BucketTable::open` +
    /// river/turn 真 board，一行 + 一个 artifact 路径）。本诊断只钉「机制正确 + L1 随迭代降 + EV 差
    /// 随迭代收」。`cargo test -p poker --lib --release -- --ignored --nocapture
    /// _measure_convergence_calibration`（须 --release）。
    #[test]
    #[ignore = "诊断：river/turn 收敛 L1 + root-EV 差（机制；真阈值需真桶表）；--release --ignored 跑"]
    fn _measure_convergence_calibration() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let turn = hu_state_at(&game, 0x5455_524E_0000_0001, Street::Turn) // "TURN"
            .game_state
            .clone();
        let river = hu_state_at(&game, 0x5249_5645_0000_0001, Street::River) // "RIVE"
            .game_state
            .clone();
        let targets: [(&str, &GameState); 2] =
            [("hu_turn_subtree", &turn), ("hu_river_subtree", &river)];

        eprintln!(
            "[A1-cal] target,nodes,iters,ref,mean_l1,max_l1,ev_short,ev_ref,ev_abs_diff,infosets"
        );
        for (name, tmpl) in targets {
            let nodes = build_base_subgame(tmpl).subtree().num_nodes();
            // root EV 的 traverser = root 决策者（postflop HU = BB 先动），便于解读。
            let root_actor = tmpl.current_player().expect("非终局").0 as PlayerId;
            let reference = {
                let mut tr = EsMccfrTrainer::new(build_base_subgame(tmpl), 0xCA1B_0000);
                let mut rng = ChaCha20Rng::from_seed(0xCA1B_0000 ^ 0xC0FF_EE00);
                for _ in 0..50_000 {
                    tr.step(&mut rng).expect("ref step");
                }
                tr
            };
            let ev_ref = mc_root_ev(&reference, root_actor, 50_000, 0x00E7_0000);
            for &iters in &[300u64, 1_000, 3_000, 10_000] {
                let mut tr = EsMccfrTrainer::new(build_base_subgame(tmpl), 0xCA1B_0000);
                let mut rng = ChaCha20Rng::from_seed(0xCA1B_0000 ^ 0xC0FF_EE00);
                for _ in 0..iters {
                    tr.step(&mut rng).expect("step");
                }
                let (mut sum_l1, mut max_l1, mut n) = (0.0f64, 0.0f64, 0usize);
                for (info, _) in tr.strategy_sum().inner().iter() {
                    let a = tr.average_strategy(info);
                    let b = reference.average_strategy(info);
                    if !b.is_empty() && a.len() == b.len() {
                        let l1: f64 = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).sum();
                        sum_l1 += l1;
                        max_l1 = max_l1.max(l1);
                        n += 1;
                    }
                }
                let ev_short = mc_root_ev(&tr, root_actor, 50_000, 0x00E7_0000);
                eprintln!(
                    "[A1-cal] {name},{nodes},{iters},50000,{:.4},{:.4},{:.2},{:.2},{:.4},{n}",
                    if n > 0 { sum_l1 / n as f64 } else { 0.0 },
                    max_l1,
                    ev_short,
                    ev_ref,
                    (ev_short - ev_ref).abs(),
                );
            }
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
