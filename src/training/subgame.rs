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

use crate::abstraction::action::{ActionAbstraction, StreetActionAbstraction};
use crate::abstraction::bucket_table::BucketTable;
use crate::abstraction::info::{InfoSetId, StreetTag};
use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::{Card, PlayerStatus, Street};
use crate::rules::action::Action;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::nlhe::{
    compute_hand_bucket, SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheInfoSet,
    SimplifiedNlheState,
};
use crate::training::nlhe_betting_tree::{
    deep_menu_for, AbstractActionTag, BettingAbstractionRules, Child, NodeId, PublicBettingTree,
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
    /// per-seat 归一前缀和 CDF（构造期建，[`sample_holes_from_ranges`]
    /// (Self::sample_holes_from_ranges) 热路径二分用）；`None` = 该座 range 总权重 0（无信号，
    /// 直接走精确扫描兜底）。下标对齐 `ranges`，仅 `ranges == Some` 时非空。
    range_cdfs: Vec<Option<SeatRangeCdf>>,
    /// 每个 hole 组合的 52-bit 牌位掩码（两张 `1<<card.to_u8()` OR）；下标对齐 `hole_combos`，
    /// 撞牌检查 = 一次 AND（仅 `ranges == Some` 时非空）。
    hole_masks: Vec<u64>,
    /// S6 6b depth-limit：`Some` = 子树用 [`PublicBettingTree::build_subtree_depth_limited`] 截断、
    /// 叶子查 blueprint 续局值（[`SubgameLeafCtx`]）；`None`（6a）= 子树解到真实终局、走真实
    /// showdown payoff。`root` 把它 clone 进 state（depth-limit 叶子 `payoff` 读）。
    leaf_ctx: Option<Arc<SubgameLeafCtx>>,
}

/// 单座位 range 的归一前缀和（[`SubgameNlheGame::sample_holes_from_ranges`] 热路径）。
struct SeatRangeCdf {
    /// `cum[i] = Σ_{j≤i} w_j / total`（单调非降、尾项 ≈ 1）。零权 hole 的区间宽 0，
    /// 二分（`partition_point(c <= u)`）不会命中。
    cum: Vec<f64>,
    /// 最大正权下标：`u` 落在尾部浮点缝隙（≥ `cum.last()`）时的保底（与
    /// [`sample_discrete`] 的「保底走最后一个 outcome」同型语义）。
    last_positive: usize,
}

/// 为每个座位建 [`SeatRangeCdf`]（总权重 0 → `None`，采样时直接走精确扫描兜底）。
fn build_range_cdfs(ranges: &[Vec<f64>]) -> Vec<Option<SeatRangeCdf>> {
    ranges
        .iter()
        .map(|r| {
            let total: f64 = r.iter().sum();
            if !total.is_finite() || total <= 0.0 {
                return None; // 全零 / 空向量（folded 座留空）/ NaN / inf 防御。
            }
            let mut cum = Vec::with_capacity(r.len());
            let mut acc = 0.0_f64;
            let mut last_positive = 0usize;
            for (i, &w) in r.iter().enumerate() {
                debug_assert!(w >= 0.0, "range 权重须非负，seat range[{i}]={w}");
                if w > 0.0 {
                    last_positive = i;
                }
                acc += w;
                cum.push(acc / total);
            }
            Some(SeatRangeCdf { cum, last_positive })
        })
        .collect()
}

/// 拒绝采样重试上限：worst-case（river 6-way，~15/52 张已用）接受率仍 ≥ ~0.5 →
/// 连拒 32 次概率 ~2⁻³²；真打满只剩病态角（range 质量几乎全在已用牌上），走精确兜底。
const RANGE_REJECTION_RETRY_CAP: usize = 32;

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
            range_cdfs: Vec::new(),
            hole_masks: Vec::new(),
            leaf_ctx: None,
        }
    }

    /// 装上 per-seat `ranges` 并预计算采样结构（CDF + 牌位掩码）。两个带 range 的构造路径
    /// （[`new_with_ranges`](Self::new_with_ranges) / [`new_depth_limited`](Self::new_depth_limited)）
    /// 共用，保证 `ranges` 与预计算结构永远同步。
    fn attach_ranges(&mut self, ranges: Vec<Vec<f64>>) {
        debug_assert_eq!(
            ranges.len(),
            self.template.players().len(),
            "ranges 长度须 == 座位数"
        );
        self.range_cdfs = build_range_cdfs(&ranges);
        self.ranges = Some(ranges);
        self.hole_combos = all_hole_combos();
        self.hole_masks = self
            .hole_combos
            .iter()
            .map(|h| (1u64 << h[0].to_u8()) | (1u64 << h[1].to_u8()))
            .collect();
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
        g.attach_ranges(ranges);
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
        let mut g = Self {
            config,
            subtree,
            abs: Arc::new(abs),
            bucket_table,
            template,
            ranges: None,
            hole_combos: Vec::new(),
            range_cdfs: Vec::new(),
            hole_masks: Vec::new(),
            leaf_ctx,
        };
        if let Some(r) = ranges {
            g.attach_ranges(r);
        }
        Ok(g)
    }

    /// 子树（诊断 / 评测：取 root_id 构造查询 infoset）。
    pub fn subtree(&self) -> &PublicBettingTree {
        &self.subtree
    }

    /// 按 `self.ranges` 为每个**未弃牌**座位采样一手底牌（顺序 card-removal：逐座位限制到
    /// 「未被 board / 已采样底牌占用」的 hole 上按权采样）。返回 per-seat `Option<[Card;2]>`
    /// （弃牌座 None）。
    ///
    /// 热路径 = 构造期预计算的 [`SeatRangeCdf`] 上二分 + 撞牌拒绝重采：被拒后重抽采到的
    /// 正是「受限重归一分布」的精确样本（拒绝采样恒等，非近似），与旧实现（逐组合过滤 +
    /// 归一 + [`sample_discrete`]）**同分布**，把每 root 每座位 O(1326) 扫描挪出热路径
    /// （它吃掉 range 加权搜索 ~7× 迭代吞吐）。连拒 [`RANGE_REJECTION_RETRY_CAP`] 次
    /// （range 质量几乎全在已用牌上的病态角）或该座 range 总权重 0 → 走
    /// [`sample_hole_exact`](Self::sample_hole_exact) 精确扫描；兜底切换不引入分布偏差
    /// （兜底本身也是受限分布的精确采样）。rng 消费序与旧实现不同（每座 1 次 → 1+拒绝数次），
    /// 同 seed 自可复现不变。
    fn sample_holes_from_ranges(
        &self,
        ranges: &[Vec<f64>],
        rng: &mut dyn RngSource,
    ) -> Vec<Option<[Card; 2]>> {
        let mut used: u64 = self
            .template
            .board()
            .iter()
            .fold(0u64, |m, c| m | (1u64 << c.to_u8()));
        let players = self.template.players();
        let mut out: Vec<Option<[Card; 2]>> = vec![None; players.len()];
        for (seat, player) in players.iter().enumerate() {
            if player.hole_cards.is_none() {
                continue; // 弃牌座：无底牌。
            }
            let mut chosen: Option<usize> = None;
            if let Some(cdf) = self.range_cdfs[seat].as_ref() {
                for _ in 0..RANGE_REJECTION_RETRY_CAP {
                    // 与 sample_discrete 同型的 53-bit 单位均匀采样。
                    let u = (rng.next_u64() >> 11) as f64 / ((1u64 << 53) as f64);
                    let mut idx = cdf.cum.partition_point(|&c| c <= u);
                    if idx >= cdf.cum.len() {
                        idx = cdf.last_positive; // 尾部浮点缝隙（u ≥ cum.last()）保底。
                    }
                    if self.hole_masks[idx] & used == 0 {
                        chosen = Some(idx);
                        break;
                    }
                }
            }
            let chosen_idx =
                chosen.unwrap_or_else(|| self.sample_hole_exact(&ranges[seat], used, rng));
            used |= self.hole_masks[chosen_idx];
            out[seat] = Some(self.hole_combos[chosen_idx]);
        }
        out
    }

    /// 单座位精确扫描采样（旧实现路径，留作拒绝采样的兜底）：限制到「不撞 `used`」的正权
    /// hole 归一后 [`sample_discrete`]；受限全零（该座 range 全零 / 正权质量全在已用牌上）→
    /// 退均匀采可用 hole。
    fn sample_hole_exact(&self, range: &[f64], used: u64, rng: &mut dyn RngSource) -> usize {
        let mut dist: Vec<(usize, f64)> = Vec::new();
        let mut total = 0.0_f64;
        for (hi, &mask) in self.hole_masks.iter().enumerate() {
            let w = range.get(hi).copied().unwrap_or(0.0);
            if w > 0.0 && mask & used == 0 {
                dist.push((hi, w));
                total += w;
            }
        }
        if total > 0.0 {
            // 归一（sample_discrete 要求 sum≈1）。
            for e in dist.iter_mut() {
                e.1 /= total;
            }
            sample_discrete(&dist, rng)
        } else {
            // 受限 range 全零 → 退均匀采可用 hole。
            let avail: Vec<usize> = self
                .hole_masks
                .iter()
                .enumerate()
                .filter(|(_, m)| **m & used == 0)
                .map(|(hi, _)| hi)
                .collect();
            let p = 1.0 / avail.len() as f64;
            let uni: Vec<(usize, f64)> = avail.into_iter().map(|hi| (hi, p)).collect();
            sample_discrete(&uni, rng)
        }
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
    /// 缺口③（`realtime_search_openpoker_exec` §2.1 / §3.2）：`true` = 子树下注菜单按
    /// 子树根 SPR 自适应选宽（[`deep_menu_for`]，缺口③ v2 细化）——深 SPR 收到**单一
    /// {1pot}**（`deep_single_pot`，把深码 / 多人解到终局的树压到可解：到终局层数多，靠单档
    /// 收窄每节点分叉）；浅 SPR（≤ 4×pot）且 ≤3 Active（小池树小可负担；多人加宽是乘性
    /// 爆炸，实测 6-way 边界 20.6×）放宽到 `{0.5,1}` 两档（`deep_wide_half_pot`）。与 blueprint 菜单**解耦不引偏差**：桶表按
    /// (cards,board) 归桶、与菜单无关，子树自洽即可（建树与运行期 `legal_actions` 同一 `abs`，
    /// §2.1）。`false`（默认）= 子树沿用 blueprint 自身 abstraction/rules（既有行为，**保持
    /// probe / advisor / §11.5 基线逐 infoset byte-equal、不改生产行为**）。
    ///
    /// **deep_menu 下返回子树自身合法集上的分布**（{1pot} 动作对象，携带按 `auth` 真实 pot 算出
    /// 的 `to`/`ratio_label`），**不对齐调用方 `legal_abs`**——菜单不同（{1pot} ⊊ blueprint），
    /// 强行对齐必失败。调用方须用 {1pot} 抽象做 outgoing（advisor deep 路径）。与 [`depth_limit`]
    /// **互斥**（深码解到终局、无叶子续局值，§2.1 / §6 #2）：两者同开 → [`subgame_search`] `Err`。
    ///
    /// [`depth_limit`]: Self::depth_limit
    pub deep_menu: bool,
    /// 缺口①续（限时杠杆②，与 LCFR 正交）：`true` = subgame solve 的 traverser 只轮**子树根
    /// 仍 `Active`** 的座位（[`EsMccfrTrainer::with_traverser_rotation`]）。弃牌 / all-in 座在
    /// 子树里**零决策节点**（规则引擎只让 `Active` 座当 `current_player`）→ 默认 `0..n_seats`
    /// 轮转下轮到它们的迭代纯零学习（σ / regret 都只在 `actor == traverser` 节点累积，
    /// `trainer.rs` `recurse_es`）；只轮 Active 座 = 同 wall 有效迭代 ×`n_seats/n_active`
    /// （fold 剩 2-3 人的最常见局面 2-3×，h2h `SearchObserver` 的浪费遥测即量此）。
    /// `false`（默认）= 既有全座轮转——**保持全部既有基线逐 infoset byte-equal、不改生产行为**。
    /// 开启后 rng 消费序列改变 → 与 `false` 基线不 byte-equal（固定迭代 + 固定 seed 下自身仍
    /// 确定性可复现；本字段进 within-round solve 缓存 key）。
    pub live_traversers: bool,
    /// range 先验平滑 λ（2026-06-12 searchon50 实跑修复）：把**对手**座位经 [`estimate_range`]
    /// 估出的 blueprint reach 与「非撞 board 组合上的 uniform」混合，`r' = (1−λ)·r + λ·u`；
    /// **hero（当前决策 actor）的 range 不混**，保持原 reach（[`mix_lambda_for_seat`] doc：
    /// 混 hero range 会虚增弃牌率、实测方向反掉）。
    ///
    /// 动机：多街薄线上 blueprint σ 欠训练 → reach 估计塌缩成噪声窄 range（实测 river 对手
    /// range 有效组合 50、单牌类占 36%、几乎无同花 = 被钉死「封顶」），无约束重解将其放大成
    /// max-exploit——对手在解内对 jam 弃 72.6% → 空气桶 99.98% 下注 / 73% 超池 jam，且预算
    /// 越足越极端（20s → 0.91，是收敛而非噪声）；uniform 先验 A/B 同局面回 check 0.31 =
    /// 机制实锤在 range 先验。混合给对手每个合法组合保底 `λ/n_valid` 权重 → 对手 range 永不
    /// 被钉死「封顶」，剥削度随 λ 连续回拉（`λ=1` = 对手 uniform 先验、hero 仍 blueprint reach）。
    ///
    /// `0.0`（默认）= 不混合，既有 probe / §11.5 基线逐位 byte-equal；生产 advisor 另有自己的
    /// 默认（`openpoker_advisor --search-range-uniform-mix`）。仅 `use_blueprint_range=true`
    /// 时有意义。进 solve 缓存 key（cfg 字段 + 混合后 reach 向量双重覆盖）。clamp 到 [0,1]。
    pub range_uniform_mix: f64,
    /// solve update 并行线程数（2026-06-12 限时杠杆③）：`>1` = 子树 CFR 走
    /// [`EsMccfrTrainer::step_parallel`]（rayon pool + thread-local delta 确定性合并，
    /// blueprint 训练已验证的同一路径）——同 wall 预算 update 数 ≈ ×核数，给 deep_menu
    /// 加宽下注档（infoset 乘性变多 → per-bucket 样本变稀）换回采样密度。**注意杠杆边界**：
    /// 并行只助 solve 侧；深码×多人的真瓶颈是单线程建树（exec 文档 §4.1 A②），build wall
    /// 不随本字段缩放。
    ///
    /// `1`（默认）/ `0` = 既有单线程 `step`——保持全部既有基线逐 infoset byte-equal。
    /// `>1` 与单线程不 byte-equal（per-tid rng 流 + 批内 stale-σ 语义，`step_parallel` doc），
    /// 但固定迭代 + 固定 seed 下自身仍确定性可复现（批调度是 `(iterations, threads, batch)`
    /// 纯函数、delta 合并按 tid 序确定）→ m1==m2 契约不破。进 solve 缓存 key。
    pub solve_threads: usize,
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
            deep_menu: false,
            live_traversers: false,
            range_uniform_mix: 0.0,
            solve_threads: 1,
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

/// solve 并行 per-tid rng 派生盐（"THRD"，与 `train_cfr` rng_pool 同型：
/// `search_seed(master, SALT, tid)`）。与单线程档的 `master ^ 0xC0FF…` 流天然不同。
const SOLVE_THREADS_RNG_SALT: u64 = 0x5448_5244;

/// [`EsMccfrTrainer::step_parallel`] 每 worker 每批 trajectory 数（调度摊薄 knob，
/// `train_cfr` 默认同值；详 trainer.rs「为什么需要 batching」）。deadline 检查粒度 =
/// `solve_threads × 本值` 个 update（~11–33 µs/update → 数百 µs 一批）。
const SOLVE_PARALLEL_BATCH: u64 = 16;

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

/// 脱锚搜索档一（前缀 reach，`unanchored_range_design_2026_06_10` §1）的 range 先验输入：
/// 失同步时**已同步前缀**的决策三元组列表 + blueprint σ 查询面。失同步发生在某个具体动作上，
/// 之前每一步影子都走通了、有精确的 blueprint 节点（不是近似）——把这段前缀喂
/// [`estimate_range`]（`skip_all_in=true`，跳过 100BB-shove 语义的 AllIn-tag）即得**更粗但
/// 合法**的 per-seat range 先验（「给定已同步前缀的 range」），替代脱锚默认的 uniform。
///
/// `decisions` = 全路径决策三元组（[`synced_prefix_decisions`] 从失同步影子节点取）；脱锚搜索
/// 内部按 [`ResolveRoot::RoundStart`] 只用**当前街之前**的决策（当前街 betting 由 CFR 解，§2）。
/// `strategy` = actor 的 blueprint average strategy（同质 blueprint 假设，[`estimate_range`] doc）。
/// `None` = uniform 先验（既有行为，byte-equal）。
pub struct PrefixReach<'a> {
    pub strategy: &'a dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    pub decisions: &'a [(NodeId, AbstractActionTag, PlayerId)],
}

/// 档一前缀 reach：从失同步时**已同步**的影子 `synced_node`（lockstep 闭包在断点前的
/// `current_node_id`）取已同步前缀的决策三元组列表（[`decisions_on_path`] 在 blueprint 全局树上
/// 回溯）。结果喂 [`PrefixReach`]。`synced_node` 之后的（断点 + 其后）决策不在列表里 = 按无
/// 信息处理（因子 1，`unanchored_range_design` §1）。
pub fn synced_prefix_decisions(
    game: &SimplifiedNlheGame,
    synced_node: NodeId,
) -> Vec<(NodeId, AbstractActionTag, PlayerId)> {
    decisions_on_path(game.tree(), synced_node)
}

/// 估计 `seat` 的 per-hole marginal range（reach 向量，下标对齐 `holes`，归一）。沿 `decisions`
/// 里属于 `seat` 的决策，对每个候选 hole 累乘该 hole 在 blueprint σ 下走该动作的概率——**逐街
/// 用 `info_set_for_cards` 注入真实 board 前缀算当前街桶**（绝不在固定桶上累乘，§5b 陷阱①）。
/// 撞 board 的 hole reach=0；空/坏 σ 退均匀（同 `strategy_distribution`）。全零（无信号）→ 返回
/// 全零，调用方退均匀采样。
///
/// **同质 blueprint 假设**：用 `game`/`strategy`（actor 的 blueprint）为所有 seat 估 range——
/// 探针自对弈（hero/field 同 blueprint）下精确；异质 field 下是近似（§5b 陷阱②的工程折中）。
///
/// `skip_all_in`（脱锚搜索档一前缀 reach，`unanchored_range_design_2026_06_10` §1）：`true` =
/// **跳过 `AllIn`-tag 决策**（按因子 1 处理、不乘进 reach）。理由：AllIn-tag 的 σ 语义是
/// 「100BB 全栈 shove」，off-100BB 真栈下撒谎最狠（blueprint RFI 在 100BB 几乎不开池 shove，
/// 真 30BB shove 的 range 宽得多）——乘进去会把 shover 错收成「100BB shove range」，比 uniform
/// 更糟。`false`（anchored 路径，栈≈100BB、AllIn = 真 100BB shove）= 不跳，逐位 byte-equal。
fn estimate_range(
    game: &SimplifiedNlheGame,
    strategy: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    decisions: &[(NodeId, AbstractActionTag, PlayerId)],
    board: &[Card],
    seat: PlayerId,
    holes: &[[Card; 2]],
    skip_all_in: bool,
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
            if skip_all_in && *tag == AbstractActionTag::AllIn {
                continue; // 档一 v1：AllIn-tag 按因子 1 处理（σ 是 100BB shove，off-100BB 撒谎）。
            }
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

/// range 先验平滑（[`SubgameSearchConfig::range_uniform_mix`]）：`r' = (1−λ)·r + λ·u`，
/// `u` = 非撞 board 组合上的 uniform。前置：`range` 已归一（[`estimate_range`] 的 sum>0 路径），
/// 混合后和仍为 1。**全零（无信号）保持全零**——下游 [`sample_holes_from_ranges`]
/// (SubgameNlheGame::sample_holes_from_ranges) 的「受限 range 全零 → 退均匀」兜底语义不变，
/// 混入 λ·u 会把「无信号」伪装成「有 range」。撞 board 组合两边都是 0 → 混合后仍 0
/// （card-removal 不变）。
/// range 平滑只作用于**对手**座位：hero（`auth_actor`）的 range 保持原 blueprint reach。
/// 缘由：①对手 range 的「不确定性 → 向 uniform 回拉」有正当性；hero 自己的 range 是
/// 「hero 实际怎么走到这条线」的自一致输入，混 uniform = 解错游戏；②实测（2026-06-12
/// vultr 固定 150k 迭代 ×3 seeds）对称混合把 hero range 也灌入强牌组合 → 对手在解内被迫
/// 更尊重 hero 下注 → 弃牌率虚增 → λ=0.5 比 λ=0 还激进（river 空气 allin 0.92/0.74/0.60
/// vs 0.46/0.91/0.53），方向直接反掉。
fn mix_lambda_for_seat(seat: PlayerId, hero: PlayerId, lambda: f64) -> f64 {
    if seat == hero {
        0.0
    } else {
        lambda
    }
}

fn mix_range_with_uniform(range: &mut [f64], holes: &[[Card; 2]], board: &[Card], lambda: f64) {
    let lambda = lambda.clamp(0.0, 1.0);
    if lambda <= 0.0 || range.iter().sum::<f64>() <= 0.0 {
        return;
    }
    let board_set: BTreeSet<u8> = board.iter().map(|c| c.to_u8()).collect();
    let valid: Vec<bool> = holes
        .iter()
        .map(|h| !board_set.contains(&h[0].to_u8()) && !board_set.contains(&h[1].to_u8()))
        .collect();
    let n_valid = valid.iter().filter(|v| **v).count();
    if n_valid == 0 {
        return;
    }
    let u = lambda / n_valid as f64;
    for (w, ok) in range.iter_mut().zip(valid.iter()) {
        *w = if *ok { (1.0 - lambda) * *w + u } else { 0.0 };
    }
}

// ===========================================================================
// within-round solve 缓存（exec 文档 §6 #2：「每轮恰好一个 solve」落到常驻 advisor）
// ===========================================================================

/// 已求解的子博弈（[`SubgameSolveCache`] 的条目）：solve 段产物 + 子树自身抽象——deep_menu /
/// unanchored 的 mid-round 真实动作导航须用与 solve **同一份**菜单现算 tag（命中时不重算
/// `deep_menu_for`，直接读存下的）。
struct SolvedSubgame {
    trainer: EsMccfrTrainer<SubgameNlheGame>,
    sub_abs: StreetActionAbstraction,
}

/// within-round solve 缓存（容量 1，advisor 常驻进程持有、跨请求传入）。
///
/// **动机（exec 文档 §6 #2）**：RoundStart + round-stable seed 的设计本意是「同一街多次决策
/// 共享字节相同的 solve、读不同节点 = 一个均衡内自洽」。固定迭代下成立，但 advisor 逐决策
/// 无状态重解：(a) 同街第二次决策从头重建重解一遍字节相同的子博弈 = 纯浪费 wall（build 是
/// 深码×多人的真瓶颈，§4.1 A②）；(b) 开 `time_budget`（生产限时路径）后该保证悄悄破了——
/// anytime 迭代数随机器负载变，同街两次重解可停在不同迭代数 → 两次决策读的是**不同均衡**
/// （§6 #2 想避免的 mid-round 不一致部分回来）。缓存命中 → 复用 solve、只重做导航/读数：
/// 一致性恢复「每轮恰好一个 solve」，mid-round wall ≈ 0，首决策因此可放心用满 time_budget。
///
/// **key 必须覆盖 solve 的全部输入**（本机制唯一认真风险：漏一项 = 读错均衡）——故 key 不从
/// 请求层推导，而在 solve 边界由实际构造输入现算（[`solve_cache_key`]）。固定迭代路径命中
/// 输出与从头重解 byte-equal（确定性 solve 同输入同输出，缓存只省 wall）；命中不重试 solve
/// （即便上次因 time_budget 偏收敛）——这正是「每轮一个 solve」语义。容量 1 够用：生产是顺序
/// 决策流，同手同街连续命中、跨街 / 跨手 key 自然变化即替换。depth_limit 路径不缓存
/// （key 须带 blueprint 树叶子映射身份，非生产路径，见 [`subgame_search_cached`]）。
pub struct SubgameSolveCache {
    entry: Option<([u8; 32], SolvedSubgame)>,
    hits: u64,
    misses: u64,
}

impl SubgameSolveCache {
    pub fn new() -> SubgameSolveCache {
        SubgameSolveCache {
            entry: None,
            hits: 0,
            misses: 0,
        }
    }

    /// 命中次数（诊断 / 测试：钉「同街第二决策不重解」）。
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// 未命中次数（≥ 实际跑的 solve 数；solve 失败也计 miss、不 store）。
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// 当前缓存条目 solve 的更新数（[`Trainer::update_count`]，ES-MCCFR 每 `step` 一次
    /// traverser 递归 +1）。`time_budget` anytime 下这就是「预算内实际完成多少 update」的
    /// 读数口（advisor 遥测）；命中时返回被复用 solve 的原始计数（同街共享同一 solve 的
    /// 语义一致）。无条目 → `None`。
    pub fn entry_update_count(&self) -> Option<u64> {
        self.entry.as_ref().map(|(_, s)| s.trainer.update_count())
    }

    /// key 是否命中（计数）。未命中后 caller 须 [`store`](Self::store)（或失败丢弃）。
    fn lookup(&mut self, key: [u8; 32]) -> bool {
        match &self.entry {
            Some((k, _)) if *k == key => {
                self.hits += 1;
                true
            }
            _ => {
                self.misses += 1;
                false
            }
        }
    }

    fn store(&mut self, key: [u8; 32], solved: SolvedSubgame) {
        self.entry = Some((key, solved));
    }

    fn entry(&self, key: [u8; 32]) -> Option<&SolvedSubgame> {
        self.entry
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, s)| s)
    }
}

impl Default for SubgameSolveCache {
    fn default() -> SubgameSolveCache {
        SubgameSolveCache::new()
    }
}

/// 缓存 key 的路径判别（anchored 与 unanchored 不串条目——即便其余输入碰巧相同也分开，
/// 读数契约 / rules 处理不同，保守隔离）。
const KEY_KIND_ANCHORED: u8 = 0;
const KEY_KIND_UNANCHORED: u8 = 1;

/// solve 缓存 key = blake3(solve 的**全部**实际构造输入)。在 solve 边界现算（非请求层推导）：
/// 喂进 [`SubgameNlheGame`] 构造 + [`solve_subgame`] 的每个输入要么逐字段哈希，要么经进程内
/// 身份哈希——逐项对应：
///
/// - 桶表：**子树 solve 实际用的表**（`bucket_override` 解析后；blueprint 表对 solve 的全部
///   影响经 range——而 range 已按算出的 reach 向量逐位哈希）：content hash（跨进程稳定）+
///   `Arc` 指针（区分 content hash 同为全 0 的 stub 表；缓存条目经 trainer 持有该 Arc →
///   条目存活期内指针不可能被释放复用）；
/// - `cfg` 全字段（iterations / max_subtree_nodes / seed / use_blueprint_range / trigger /
///   resolve_root / depth_limit / biased_leaf / lcfr / time_budget / deep_menu /
///   live_traversers / range_uniform_mix / solve_threads——含不影响 solve 的 trigger：
///   过覆盖只丢命中、欠覆盖读错均衡）；
/// - `(hand_seed, seed_ordinal)`：与 `cfg.seed` 共同决定 master seed（RoundStart 下
///   seed_ordinal = 街索引 = round-stable）；
/// - root 几何：`root_state` 的全部可见面（`TableConfig` 各字段 + 街 / board / pot /
///   当前行动者 + 每座 stack / committed / status / hole_cards）——`resample_hidden*` 与建树
///   只读这些 + 外部 RNG，故可见面相等 ⇒ solve 输入相等；
/// - `entrants` / `raises_on_street`（子树根上下文）；
/// - range 先验：直接哈希**算出的** per-seat reach 向量（覆盖 strategy_fn + 前街历史的全部
///   影响，不依赖上游推导）；
/// - 子树菜单 + 规则：各街 raise 档 milli（`DefaultActionAbstraction` 行为仅由 ratio 集决定）
///   + [`BettingAbstractionRules`] 5 字段。
#[allow(clippy::too_many_arguments)]
fn solve_cache_key(
    kind: u8,
    sub_table: &Arc<BucketTable>,
    root_state: &GameState,
    entrants: u16,
    raises_on_street: u32,
    ranges: Option<&[Vec<f64>]>,
    sub_abs: &StreetActionAbstraction,
    sub_rules: &BettingAbstractionRules,
    cfg: &SubgameSearchConfig,
    hand_seed: u64,
    seed_ordinal: u64,
) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(&[kind]);
    h.update(&sub_table.content_hash());
    h.update(&(Arc::as_ptr(sub_table) as usize as u64).to_le_bytes());
    // cfg 全字段。
    h.update(&cfg.iterations.to_le_bytes());
    h.update(&(cfg.max_subtree_nodes as u64).to_le_bytes());
    h.update(&cfg.seed.to_le_bytes());
    h.update(&[
        cfg.use_blueprint_range as u8,
        match cfg.trigger {
            SearchTrigger::AllPostflop => 0,
            SearchTrigger::FlopFirstUnraised => 1,
        },
        match cfg.resolve_root {
            ResolveRoot::CurrentDecision => 0,
            ResolveRoot::RoundStart => 1,
        },
        cfg.depth_limit as u8,
        cfg.biased_leaf as u8,
        cfg.lcfr as u8,
        cfg.deep_menu as u8,
        cfg.live_traversers as u8,
        cfg.time_budget.is_some() as u8,
    ]);
    h.update(
        &cfg.time_budget
            .map_or(0u128, |d| d.as_nanos())
            .to_le_bytes(),
    );
    h.update(&cfg.range_uniform_mix.to_le_bytes());
    // 生效线程数（0 与 1 都走单线程 step、solve 输出相同 → 同 key 不丢命中）。
    h.update(&(cfg.solve_threads.max(1) as u64).to_le_bytes());
    h.update(&hand_seed.to_le_bytes());
    h.update(&seed_ordinal.to_le_bytes());
    h.update(&entrants.to_le_bytes());
    h.update(&raises_on_street.to_le_bytes());
    // root 几何（config + 公共面 + 每座状态）。
    let rc = root_state.config();
    h.update(&[rc.n_seats]);
    for s in &rc.starting_stacks {
        h.update(&s.as_u64().to_le_bytes());
    }
    h.update(&rc.small_blind.as_u64().to_le_bytes());
    h.update(&rc.big_blind.as_u64().to_le_bytes());
    h.update(&rc.ante.as_u64().to_le_bytes());
    h.update(&[rc.button_seat.0]);
    h.update(&[root_state.street() as u8]);
    h.update(&[root_state.board().len() as u8]);
    for c in root_state.board() {
        h.update(&[c.to_u8()]);
    }
    h.update(&root_state.pot().as_u64().to_le_bytes());
    h.update(&[root_state.current_player().map_or(0xFF, |s| s.0)]);
    for p in root_state.players() {
        h.update(&p.stack.as_u64().to_le_bytes());
        h.update(&p.committed_this_round.as_u64().to_le_bytes());
        h.update(&p.committed_total.as_u64().to_le_bytes());
        h.update(&[match p.status {
            PlayerStatus::Active => 0,
            PlayerStatus::AllIn => 1,
            PlayerStatus::Folded => 2,
            PlayerStatus::SittingOut => 3,
        }]);
        match p.hole_cards {
            Some([a, b]) => h.update(&[1, a.to_u8(), b.to_u8()]),
            None => h.update(&[0, 0xFF, 0xFF]),
        };
    }
    // range 先验（§5b reach 向量，f64 逐位）。
    match ranges {
        None => {
            h.update(&[0]);
        }
        Some(rs) => {
            h.update(&[1]);
            h.update(&(rs.len() as u64).to_le_bytes());
            for r in rs {
                h.update(&(r.len() as u64).to_le_bytes());
                for v in r {
                    h.update(&v.to_le_bytes());
                }
            }
        }
    }
    // 子树菜单（各街 raise 档 milli）+ 规则。
    for street in [Street::Preflop, Street::Flop, Street::Turn, Street::River] {
        let ratios = &sub_abs.config_for(street).raise_pot_ratios;
        h.update(&(ratios.len() as u64).to_le_bytes());
        for r in ratios {
            h.update(&r.as_milli().to_le_bytes());
        }
    }
    h.update(&[
        sub_rules.drop_small_reraise as u8,
        sub_rules.width_redirect,
        sub_rules.no_open_limp as u8,
        sub_rules.preflop_open_small_only as u8,
        sub_rules.drop_preflop_open_allin as u8,
    ]);
    *h.finalize().as_bytes()
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
/// `cfg.deep_menu`（缺口③，§2.1）：`true` → 子树菜单按根 SPR 自适应（[`deep_menu_for`]：深
/// SPR = {1pot} 单档 / 浅 SPR 且 ≤3 Active = {0.5,1} 两档，缺口③ v2 细化）建+解，**返回子树自身合法集上的
/// 分布**（不对齐 `legal_abs`——菜单不同，{1pot} ⊊ blueprint）；调用方须用**同一** `deep_menu_for
/// (root_state)` 抽象 outgoing。`false`（默认）→ 子树用 blueprint 自身菜单、返回**对齐
/// `legal_abs`** 的分布（既有契约，byte-equal）。deep_menu 与 depth_limit 互斥（早 Err）。
///
/// `within_round_real`（缺口③「仍未做③」：deep_menu 配 `AllPostflop` 的 within-round 导航）：
/// 当前街 round-start 以来的**真实动作序** `(动作, 是否令行动者 all-in)`。deep_menu 子树菜单 ≠
/// blueprint 菜单 → blueprint within-round tags 在子树上**必失配**（{1pot} 没有 0.5pot 档），
/// mid-round（tags 非空）改用真实动作序在子树上重放导航
/// （[`navigate_subtree_by_real_actions`]，tag 以真栈几何现算——与 unanchored 同口径）；
/// 未提供（`None`）→ mid-round deep 搜索 `Err`（调用方降级，与旧行为同语义、原因更明确）。
/// 非 deep_menu 路径**不读**（blueprint tags 导航，byte-equal 不变）。
///
/// 任一失败（auth/root_state 非 decision / 子树越界 / within-round 导航失同步 / 当前桶在
/// `iterations` 内未被访问 / 维度不符 / 非 deep_menu 路径 `legal_abs` 含子树没有的 tag / 对齐后
/// 全零 / deep_menu+depth_limit 同开）→ `Err`，调用方按设计 §4.1 降级（深码搜索区 check-when-free，
/// 不回落 blueprint）。
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
    within_round_real: Option<&[(Action, bool)]>,
    hand_seed: u64,
    decision_ordinal: u64,
) -> Result<Vec<(SimplifiedNlheAction, f64)>, String> {
    subgame_search_cached(
        None,
        auth,
        root_state,
        game,
        legal_abs,
        node_id,
        strategy,
        cfg,
        None,
        leaf_values,
        within_round_real,
        hand_seed,
        decision_ordinal,
    )
}

/// [`subgame_search`] 的缓存版（within-round solve 缓存，[`SubgameSolveCache`] doc）：
/// `cache = Some` 且 key（solve 全部输入，[`solve_cache_key`]）命中 → 跳过建树 + 求解，复用
/// 已解 trainer、只重做导航 / 读数——同手同街多决策恢复「每轮恰好一个 solve」（time_budget
/// anytime 下重解会读不同均衡，§6 #2），mid-round wall ≈ 0。`cache = None` = 原行为
/// （逐 infoset byte-equal，[`subgame_search`] 即此薄壳）。**depth_limit 路径不缓存**（key 须带
/// blueprint 树叶子映射身份 / root_global，非生产路径不值得）——自动退 `None` 语义。
///
/// `bucket_override`：`Some` = 子树 solve（CFR infoset 归桶）+ 解完后 hero 读数（[`query_at`]
/// (SubgameNlheGame::query_at)）改用这张表——两处同表即自洽，子树是独立一次性求解、不触
/// blueprint checkpoint 的桶空间，故可换**更细**的表（如 500/500/500 vs blueprint 200）换
/// 搜索区分辨率。**range 估计（[`estimate_range`]）不受影响**：它查 blueprint σ、必须留在
/// `game.bucket_table`（blueprint 桶空间）；range 本身是 1326 具体组合粒度、加权采样不经过
/// 桶表。代价 = 同迭代预算下桶更细 → per-bucket 样本更稀，「当前桶未被访问 → `Err`」降级
/// 概率上升。与 `depth_limit` **不兼容**（叶子续局值表按 blueprint 桶空间键，换表 = 查错
/// 桶 → 早 `Err`）。`None`（默认）= 沿用 `game.bucket_table`（既有行为 byte-equal）。
/// 解析后的表进 solve 缓存 key（content hash + Arc 指针，[`solve_cache_key`]）。
#[allow(clippy::too_many_arguments)]
pub fn subgame_search_cached(
    cache: Option<&mut SubgameSolveCache>,
    auth: &GameState,
    root_state: &GameState,
    game: &SimplifiedNlheGame,
    legal_abs: &[SimplifiedNlheAction],
    node_id: NodeId,
    strategy: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    cfg: &SubgameSearchConfig,
    bucket_override: Option<&Arc<BucketTable>>,
    leaf_values: Option<&Arc<LeafValueTables>>,
    within_round_real: Option<&[(Action, bool)]>,
    hand_seed: u64,
    decision_ordinal: u64,
) -> Result<Vec<(SimplifiedNlheAction, f64)>, String> {
    subgame_search_cached_inner(
        cache,
        auth,
        root_state,
        game,
        legal_abs,
        node_id,
        strategy,
        cfg,
        bucket_override,
        leaf_values,
        within_round_real,
        hand_seed,
        decision_ordinal,
        None,
        false,
    )
}

/// [`subgame_search_cached`] 本体 + 预热钩子（RoundStart 预热方案，2026-06-12）。
///
/// `actor_override`：`Some(seat)` = range 平滑的「hero 不混」座位（[`mix_lambda_for_seat`]）
/// 以及读数 actor 用它而非 `auth.current_player()`。预热在该街 hero 行动**之前**发起，
/// `auth = round_start`、current_player = 该街首行动者 ≠ hero——若按 current_player 混合，
/// ranges 向量与决策时（actor = hero）不同 → 缓存 key 必 miss、预热静默失效。`None` =
/// 既有行为（决策路径，逐位 byte-equal）。
///
/// `solve_only`：`true` = 建树 + 求解 + 入缓存后**直接返回空 vec**，跳过导航/读数（预热
/// 没有 hero 决策点可读；读首行动者的「重放占位牌」策略是垃圾读数）。`false` = 既有行为。
#[allow(clippy::too_many_arguments)]
fn subgame_search_cached_inner(
    cache: Option<&mut SubgameSolveCache>,
    auth: &GameState,
    root_state: &GameState,
    game: &SimplifiedNlheGame,
    legal_abs: &[SimplifiedNlheAction],
    node_id: NodeId,
    strategy: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    cfg: &SubgameSearchConfig,
    bucket_override: Option<&Arc<BucketTable>>,
    leaf_values: Option<&Arc<LeafValueTables>>,
    within_round_real: Option<&[(Action, bool)]>,
    hand_seed: u64,
    decision_ordinal: u64,
    actor_override: Option<PlayerId>,
    solve_only: bool,
) -> Result<Vec<(SimplifiedNlheAction, f64)>, String> {
    if auth.is_terminal() || auth.current_player().is_none() {
        return Err("subgame_search: auth 非 decision 节点".to_string());
    }
    if root_state.is_terminal() || root_state.current_player().is_none() {
        return Err("subgame_search: root_state 非 decision 节点".to_string());
    }
    let auth_actor = actor_override
        .unwrap_or_else(|| auth.current_player().expect("checked above").0 as PlayerId);
    // 缺口③：deep_menu（{1pot} 解到终局）与 depth_limit（街边界截断 + 叶子续局值）互斥
    // （§2.1 / §6 #2：深码不重建叶子值、改解到终局）。两者同开是配置错误 → 早 Err（不静默择一）。
    if cfg.deep_menu && cfg.depth_limit {
        return Err(
            "subgame_search: deep_menu 与 depth_limit 互斥（深码解到终局、无叶子值，§2.1）"
                .to_string(),
        );
    }
    if bucket_override.is_some() && cfg.depth_limit {
        return Err(
            "subgame_search: bucket_override 与 depth_limit 不兼容（叶子续局值表按 blueprint \
             桶空间键，换表查错桶）"
                .to_string(),
        );
    }
    // 子树 solve 实际用表：override 优先（搜索区独立换粒度），否则 blueprint 表（byte-equal）。
    let sub_table: Arc<BucketTable> =
        bucket_override.map_or_else(|| Arc::clone(&game.bucket_table), Arc::clone);

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
    // 否则 None = uniform。depth-limit / 解到终局都复用这一份 ranges。对手座位做 uniform 平滑
    // （range_uniform_mix 字段 doc），hero（auth_actor）保持原 reach（mix_lambda_for_seat doc）。
    let ranges_opt: Option<Vec<Vec<f64>>> = if cfg.use_blueprint_range {
        let holes = all_hole_combos();
        let board: Vec<Card> = root_state.board().to_vec();
        let players = root_state.players();
        Some(
            (0..players.len())
                .map(|seat| {
                    if players[seat].hole_cards.is_some() {
                        let mut r = estimate_range(
                            game,
                            strategy,
                            &range_decisions,
                            &board,
                            seat as PlayerId,
                            &holes,
                            false, // anchored：栈≈100BB，AllIn = 真 100BB shove，不跳过（byte-equal）。
                        );
                        mix_range_with_uniform(
                            &mut r,
                            &holes,
                            &board,
                            mix_lambda_for_seat(
                                seat as PlayerId,
                                auth_actor,
                                cfg.range_uniform_mix,
                            ),
                        );
                        r
                    } else {
                        Vec::new() // 弃牌座：range 不被读。
                    }
                })
                .collect(),
        )
    } else {
        None
    };

    // 缺口③（§2.1 / §3.2）：deep_menu → 子树下注菜单按根 SPR 自适应（deep_menu_for：深 SPR =
    // {1pot} 单档控树 / 浅 SPR 且 ≤3 Active = {0.5,1} 两档，v2 细化），把深码 / 多人解到终局的树压到可解；
    // 与 blueprint 菜单解耦不引偏差（桶表按 cards/board 归桶、与菜单无关，建树与运行期
    // legal_actions 同一 abs，§2.1）。false（默认）= 用 blueprint 自身 abstraction/rules
    // （既有行为，byte-equal）。deep_menu 与 depth_limit 互斥已在函数顶 guard：故 depth_limit 分支
    // 必 deep_menu=false，sub_abs/sub_rules == blueprint 自身（与 game.tree() 抽象一致，叶子映射不破）。
    let (sub_abs, sub_rules) = if cfg.deep_menu {
        deep_menu_for(root_state)
    } else {
        (game.abstraction().clone(), game.rules())
    };

    // 跑 CFR 的 master seed：(cfg.seed, hand_seed, seed_ordinal) 确定派生。RoundStart 下
    // seed_ordinal = 街索引 → 同一轮多决策的 solve 字节相同（§6 #2 一致性）。
    let master = search_seed(cfg.seed, hand_seed, seed_ordinal);
    // within-round solve 缓存：key 在 solve 边界按实际构造输入现算（solve_cache_key doc）。
    // depth_limit 路径不缓存（key 须带 blueprint 树叶子映射身份 + root_global，非生产路径）。
    let cache = if cfg.depth_limit { None } else { cache };
    let key = cache.as_ref().map(|_| {
        solve_cache_key(
            KEY_KIND_ANCHORED,
            &sub_table,
            root_state,
            entrants,
            raises_on_street,
            ranges_opt.as_deref(),
            &sub_abs,
            &sub_rules,
            cfg,
            hand_seed,
            seed_ordinal,
        )
    });
    // 建 subgame + 求解（仅 miss / 无缓存时跑）：bucket 表 + 上面选定的 action 抽象
    // （sub_abs/sub_rules），从 root_state 为根。6b depth-limit → 子树街边界截断 + 叶子查
    // blueprint 续局值（new_depth_limited）；否则 6a 解到终局。
    let build_and_solve = move || -> Result<SolvedSubgame, String> {
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
                Arc::clone(&sub_table),
                root_state.config().clone(),
                sub_abs.clone(),
                sub_rules,
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
                Arc::clone(&sub_table),
                root_state.config().clone(),
                sub_abs.clone(),
                sub_rules,
                root_state.clone(),
                entrants,
                raises_on_street,
                ranges,
            )
        } else {
            SubgameNlheGame::new(
                Arc::clone(&sub_table),
                root_state.config().clone(),
                sub_abs.clone(),
                sub_rules,
                root_state.clone(),
                entrants,
                raises_on_street,
            )
        };
        let trainer = solve_subgame(sub, cfg, master)?;
        Ok(SolvedSubgame { trainer, sub_abs })
    };
    let solved_fresh: SolvedSubgame;
    let solved: &SolvedSubgame = match (cache, key) {
        (Some(c), Some(key)) => {
            if !c.lookup(key) {
                c.store(key, build_and_solve()?);
            }
            c.entry(key).expect("lookup 命中或刚 store")
        }
        _ => {
            solved_fresh = build_and_solve()?;
            &solved_fresh
        }
    };
    // 预热（solve_only）：solve 已入缓存即达成目的，跳过导航/读数（hero 决策点尚不存在）。
    if solve_only {
        return Ok(Vec::new());
    }
    let trainer = &solved.trainer;

    // 导航到当前决策点（CurrentDecision / round-start 首决策点 tags 空 → root），用 auth 几何读
    // 真实手在落点的平均策略（actor 校验 + 维度检查见 read_current_strategy）。
    // 缺口③「仍未做③」：deep_menu mid-round（tags 非空）时子树菜单 ≠ blueprint 菜单，blueprint
    // tags 必失配（{1pot} 子树没有 0.5pot 档）→ 改用当前街真实动作序在子树上重放
    // （navigate_subtree_by_real_actions，tag 以真栈几何在子树自身抽象下现算，与 unanchored
    // 同口径；缓存命中时菜单取 solve 时存下的同一份 sub_abs）；调用方未提供动作序 → Err 降级
    // （与旧「tag 失配 Err」同语义、原因更明确）。非 deep_menu 路径仍走 blueprint tags 导航
    // （byte-equal 不变）。
    let cur_node = if cfg.deep_menu && !within_round_tags.is_empty() {
        let wr = within_round_real.ok_or_else(|| {
            "deep_menu mid-round 导航需要当前街真实动作序（调用方未提供 within_round_real）"
                .to_string()
        })?;
        navigate_subtree_by_real_actions(trainer.game().subtree(), &solved.sub_abs, root_state, wr)?
    } else {
        navigate_subtree(trainer.game().subtree(), &within_round_tags)?
    };
    let (avg, sub_legal) = read_current_strategy(trainer, cur_node, auth, auth_actor)?;

    // 缺口③ deep_menu（§2.1）：子树菜单（{1pot}）与调用方 blueprint `legal_abs` 不同
    // （{1pot} ⊊ blueprint），**不能对齐 legal_abs**（强行对齐时 blueprint 多出的档如 0.5pot
    // 找不到对应 = 必 Err）。直接返回**子树自身合法集** `sub_legal` 上的分布：动作对象携带按
    // `auth` 真实 pot 算出的 `to`/`ratio_label`（query_at 用 auth 几何，§query_at doc），调用方
    // 用 {1pot} 抽象 outgoing（advisor deep 路径）即自洽（self_distribution）。
    if cfg.deep_menu {
        return self_distribution(&sub_legal, &avg);
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

/// LCFR period（[`solve_subgame`] 用）：≈ iterations/50，落进 with_lcfr_period 要求的
/// 总更新/period∈[20,100]（trainer.rs:364 doc）；iterations<50 → clamp 1。
///
/// time_budget anytime 下 iterations 只是安全上界（advisor 默认抬到 `u64::MAX`，求解由墙钟
/// 截断）、实际 update 数由 wall 决定 → period 不能挂在 iterations 上（u64::MAX/50 = 永不
/// rescale = LCFR 静默退化 vanilla）。cap 取 10_000：5s 档实测 ~11–33µs/iter ≈ 150–450k
/// updates → 15–45 个 period；iterations ≤ 500k 时公式与 None 路径相同 → 「预算不绑定 ==
/// 固定迭代」契约（time_budget_anytime_stops_and_is_valid）在其测试档位不破。
fn lcfr_period(cfg: &SubgameSearchConfig) -> u64 {
    let by_iters = (cfg.iterations / 50).max(1);
    if cfg.time_budget.is_some() {
        by_iters.min(10_000)
    } else {
        by_iters
    }
}

/// 共享求解段（[`subgame_search`] blueprint 锚 / [`subgame_search_unanchored`] 真栈锚共用）：
/// 子树节点数 cap → 建 trainer（LCFR 可选）→ 求解（固定迭代或墙钟 anytime）。逐字保持原
/// `subgame_search` 求解段的操作顺序 / RNG 流（既有 probe / advisor / §11.5 基线 byte-equal）。
fn solve_subgame(
    sub: SubgameNlheGame,
    cfg: &SubgameSearchConfig,
    master: u64,
) -> Result<EsMccfrTrainer<SubgameNlheGame>, String> {
    let n_nodes = sub.subtree().num_nodes();
    if n_nodes == 0 || n_nodes > cfg.max_subtree_nodes {
        return Err(format!(
            "subtree 节点数 {n_nodes} 越界（cap {}）",
            cfg.max_subtree_nodes
        ));
    }
    // 缺口①续（限时杠杆②）：live_traversers → traverser 只轮子树根仍 Active 的座
    // （弃牌 / all-in 座零决策节点，轮到 = 零学习迭代）。root 是 decision 节点 ⇒ 至少
    // current_player 一座 Active ⇒ 轮转表非空（with_traverser_rotation 的非空断言满足）。
    // 默认 false = 既有全座轮转，逐位不变。
    let rotation: Option<Vec<PlayerId>> = cfg.live_traversers.then(|| {
        sub.template()
            .players()
            .iter()
            .enumerate()
            .filter(|(_, p)| p.status == PlayerStatus::Active)
            .map(|(i, _)| i as PlayerId)
            .collect()
    });
    // 缺口①：LCFR 加权（限时第一杠杆，A②）。period 见 [`lcfr_period`]。fresh trainer
    // （update_count==0）→ with_lcfr_period 前置满足。LCFR rescale 确定性（固定迭代 + seed）→
    // 仍 byte-equal 可复现；vanilla（默认）保持既有行为不变。
    let base = EsMccfrTrainer::new(sub, master);
    let mut trainer = if cfg.lcfr {
        base.with_lcfr_period(lcfr_period(cfg))
    } else {
        base
    };
    if let Some(rot) = rotation {
        trainer = trainer.with_traverser_rotation(rot);
    }
    // 缺口①本体（§2.3）：time_budget=Some → 墙钟 anytime（跑到 iterations 上限或 wall 达预算就停，
    // 此时 iterations 退为安全上界）；None → 跑满固定 iterations（既有行为，byte-equal）。budgeted
    // 下迭代数随机器速度/负载变 → 不可 byte-equal（§2.3），可复现靠固定迭代档 + seeded RNG。
    let deadline = cfg.time_budget.map(|_| Instant::now());
    let mut done: u64 = 0;
    if cfg.solve_threads > 1 {
        // solve update 并行（限时杠杆③，[`SubgameSearchConfig::solve_threads`] doc）：复用
        // blueprint 训练的 step_parallel（rayon + thread-local delta 确定性合并）。per-tid rng
        // 从 master 加盐派生（train_cfr rng_pool 同型）；批调度是 (iterations, threads, batch)
        // 纯函数 + 合并按 tid 序 → 固定迭代档同输入同输出（m1==m2）。deadline 检查在批间，
        // 粒度 = threads × batch 个 update（~数百 µs wall，对 ms 级预算过冲可忽略）。
        let n = cfg.solve_threads;
        let mut pool: Vec<Box<dyn RngSource>> = (0..n as u64)
            .map(|tid| {
                Box::new(ChaCha20Rng::from_seed(search_seed(
                    master,
                    SOLVE_THREADS_RNG_SALT,
                    tid,
                ))) as Box<dyn RngSource>
            })
            .collect();
        while done < cfg.iterations {
            let remaining = cfg.iterations - done;
            let batch = (remaining / n as u64).min(SOLVE_PARALLEL_BATCH) as usize;
            if batch == 0 {
                // remaining < threads：整批放不下，退单线程 step 收尾（不超 iterations 上限）。
                trainer
                    .step(pool[0].as_mut())
                    .map_err(|e| format!("subgame CFR step 失败: {e:?}"))?;
                done += 1;
            } else {
                trainer
                    .step_parallel(&mut pool, n, batch)
                    .map_err(|e| format!("subgame CFR step_parallel 失败: {e:?}"))?;
                done += n as u64 * batch as u64;
            }
            if let (Some(start), Some(budget)) = (deadline, cfg.time_budget) {
                if start.elapsed() >= budget {
                    break;
                }
            }
        }
    } else {
        let mut srng = ChaCha20Rng::from_seed(master ^ 0xC0FF_EE00_C0FF_EE00);
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
    }
    if cfg.time_budget.is_some() && done == 0 {
        // 连一轮 CFR 迭代都没完成（iterations==0 的退化配置）→ 直接 fold（§2.3 降级，不回落
        // blueprint）。够不够*有用*迭代由「当前桶未被访问 → Err」（read_current_strategy）继续兜。
        return Err("time_budget 内连一轮 CFR 迭代都未完成（→ fold）".to_string());
    }
    Ok(trainer)
}

/// 读当前决策点策略（共享）：导航落点的决策者须 == 权威当前 actor（否则读到错座的真实手 →
/// 失同步）；取 actor 真实手在 cur_node 的 average strategy。**用 auth 几何 query**——cur_node
/// 是当前街 betting 后的深层节点，其合法集须用当前几何算（RoundStart 的 round-start template
/// 几何不符，见 [`SubgameNlheGame::query_at`] doc）。
fn read_current_strategy(
    trainer: &EsMccfrTrainer<SubgameNlheGame>,
    cur_node: NodeId,
    auth: &GameState,
    auth_actor: PlayerId,
) -> Result<(Vec<f64>, Vec<SimplifiedNlheAction>), String> {
    let node = trainer.game().subtree().node(cur_node);
    let cur_actor = node.player_acting;
    if cur_actor != auth_actor {
        return Err(format!(
            "within-round 导航落点 actor {cur_actor} ≠ 权威当前 actor {auth_actor}（失同步）"
        ));
    }
    let (info, sub_legal) = trainer.game().query_at(cur_node, auth);
    let avg = trainer.average_strategy(&info);
    if avg.is_empty() {
        return Err(
            "subgame 当前决策 infoset 未被 CFR 访问（该 bucket 在 iterations 内未采样到）"
                .to_string(),
        );
    }
    // `avg` 按子树节点**建树几何** tag 序索引（CFR 解的维度恒 == node.legal_actions 长度——CFR
    // 首访该 infoset 时按 `legal_actions(state)` 注册动作槽，子树 solve 几何 == build 几何 →
    // 槽数 == 建树 tag 数）。不等 = CFR 索引契约破，硬 Err。
    if avg.len() != node.legal_actions.len() {
        return Err(format!(
            "subgame 解维度 {} ≠ 子树节点 tag 数 {}（CFR 索引契约破）",
            avg.len(),
            node.legal_actions.len()
        ));
    }
    // 真栈（auth）几何下加注档可能塌进 all-in（hero 响应**真实大注**、SPR 比建树期映小的对手注
    // 更浅 → `candidate_to ≥ cap` 被 AllIn 槽吸收，见 abstract_actions AA-004-rev1），故 query_at
    // 的 `sub_legal` 会比建树 tag 少（deep-wide `{0.5,1}` 菜单最易触发：4 维解 vs 3 合法 → 旧版
    // 在此硬 Err → giveup → 白丢牌）。按 tag 把策略投影到 `sub_legal`：命中直接对位，建树期独立
    // 的加注档塌缩则 mass 并入 AllIn（保总概率）。返回向量与 `sub_legal` 同长、按位对齐 →
    // self_distribution / legal_abs 对齐两路调用方零改动复用。无塌缩时为恒等映射（byte-equal）。
    let projected = project_strategy_onto_auth_legal(&node.legal_actions, &avg, &sub_legal)?;
    Ok((projected, sub_legal))
}

/// 把建树几何 tag 序的策略 `avg`（与子树节点 `node_tags` 一一对应、同长）投影到 **auth 真栈
/// 几何**的合法集 `sub_legal` 上，返回与 `sub_legal` 同长、**按位对齐**的概率向量。
///
/// `query_at` 的 `sub_legal` 由 `abstract_actions(auth).filter(tag ∈ node_tags)` 得 → 其 tag 恒
/// **⊆ `node_tags`**（只会因真栈更浅而收缩、不会凭空多档）。收缩有**两个**合法成因，都把
/// mass 并入 `sub_legal` 的 AllIn 槽（与真栈语义一致）：
///  1. **加注档塌缩**：`candidate_to ≥ cap` 被 AllIn 槽吸收（abstract_actions 的
///     `candidate_to >= cap` 跳过分支与 AA-004-rev1 折叠）；abstract 几何里 0.5pot/1.0pot 两档
///     不会互相塌成同 `to`（1.0pot 候选恒 `> min_to`，postflop `pot_after_call ≥ to_call` ⇒ 不
///     floor 到 min_to），故缺失的建树加注 tag 必然塌进 all-in。
///  2. **跟注塌缩（all-in-for-less）**：对手真实注 ≥ hero 剩余栈时，真栈下 hero 的跟注**即
///     all-in**，`query_at` 把它折进 AllIn 槽、无独立 `Call`（与
///     [`navigate_subtree_by_real_actions`] 的被动 Call→AllIn 同义）。而建树期对手注被 off-tree
///     映小（`{0.5,1}` 宽档最易：真实 0.8pot 注映成 0.5pot < hero 栈）→ 建树 Call 非 all-in、
///     真栈 Call = all-in → 建树 `Call` tag 缺失于 `sub_legal`、须并入 AllIn。
///
/// `Fold`/`Check` 缺失（不可能由塌缩产生——二者无金额、不随真栈变）/ 任一档塌缩但 `sub_legal`
/// 无 AllIn 槽 = 结构性失配 → `Err`，调用方降级（§2.3 check-when-free）。投影保总概率（塌缩只
/// 搬运 mass、不丢弃）。
fn project_strategy_onto_auth_legal(
    node_tags: &[AbstractActionTag],
    avg: &[f64],
    sub_legal: &[SimplifiedNlheAction],
) -> Result<Vec<f64>, String> {
    debug_assert_eq!(
        node_tags.len(),
        avg.len(),
        "project: node_tags 与 avg 必同长（CFR 索引契约，调用方已校验）"
    );
    let mut out = vec![0.0_f64; sub_legal.len()];
    // sub_legal 的 AllIn 落点（塌缩并入用）；hero 无 all-in 槽时为 None（则任何塌缩 = 结构失配）。
    let all_in_idx = sub_legal
        .iter()
        .position(|a| matches!(AbstractActionTag::of(a), AbstractActionTag::AllIn));
    for (tag, &p) in node_tags.iter().zip(avg.iter()) {
        if let Some(j) = sub_legal
            .iter()
            .position(|a| AbstractActionTag::of(a) == *tag)
        {
            out[j] += p; // tag 命中真栈合法集 → 直接对位。
        } else {
            match tag {
                AbstractActionTag::Bet(_)
                | AbstractActionTag::Raise(_)
                | AbstractActionTag::Call => {
                    // 真栈几何下该档塌进 all-in → mass 并入 AllIn 槽（保总概率）：
                    //  · Bet/Raise：candidate_to ≥ cap 被 AllIn 吸收；
                    //  · Call：对手真实注 ≥ hero 剩余栈 → 跟注即 all-in-for-less，真栈合法集只
                    //    剩 AllIn 槽（query_at 把跟注折进 AllIn，与 navigate_subtree_by_real_actions
                    //    被动 Call→AllIn 同义）。建树期对手注被 off-tree 映小（{0.5,1} 宽档最易）
                    //    → 建树 Call 非 all-in、真栈 Call = all-in。
                    let j = all_in_idx.ok_or_else(|| {
                        format!(
                            "subgame 投影：建树 tag {tag:?} 真栈塌缩但合法集无 AllIn 槽（结构失配）"
                        )
                    })?;
                    out[j] += p;
                }
                _ => {
                    return Err(format!(
                        "subgame 投影：建树 tag {tag:?} 在真栈合法集无落点（Fold/Check 不可塌缩 → 失同步）"
                    ))
                }
            }
        }
    }
    debug_assert!(
        (out.iter().sum::<f64>() - avg.iter().sum::<f64>()).abs() < 1e-9,
        "project: 投影须保总概率（塌缩只搬运 mass）"
    );
    Ok(out)
}

/// 子树**自身合法集**上的归一分布（deep_menu / unanchored 共用的返回契约）：只保留正概率
/// 动作 + 归一。动作对象携带按 `auth` 真实 pot 算出的 `to`/`ratio_label`（query_at 用 auth
/// 几何），调用方须用与子树同一抽象做 outgoing。
fn self_distribution(
    sub_legal: &[SimplifiedNlheAction],
    avg: &[f64],
) -> Result<Vec<(SimplifiedNlheAction, f64)>, String> {
    let mut out: Vec<(SimplifiedNlheAction, f64)> = Vec::with_capacity(sub_legal.len());
    let mut sum = 0.0_f64;
    for (a, &p) in sub_legal.iter().zip(avg.iter()) {
        if p.is_finite() && p > 0.0 {
            sum += p;
            out.push((*a, p));
        }
    }
    if !(sum.is_finite() && sum > 0.0) {
        return Err("subgame 当前决策策略（子树自身合法集）全零".to_string());
    }
    for (_, p) in out.iter_mut() {
        *p /= sum;
    }
    Ok(out)
}

/// 真栈版 within-round 导航（缺口②续：node_id 来源脱离 100BB 影子）：从 subtree root 起，把
/// 当前街**真实动作序**逐个译成抽象 tag 并沿边推进——tag 以**真栈几何**现算（aggressive 经
/// [`ActionAbstraction::map_off_tree`]，状态从 `root_real` 沿真实动作 apply 推进），不读 blueprint
/// 全局树。每步校验子树节点决策者 == 真实状态当前行动者（错位 = 失同步 → `Err`，拒绝静默走错
/// 路径）。子树与 tag 建在**同一**真栈几何上 → on-menu 动作必命中；真 all-in / 最近档在该几何
/// 塌进 AllIn 槽 → AllIn（与 [`advance_shadow_by_applied`] incoming 同义，参考系换成真栈）。
///
/// [`advance_shadow_by_applied`]: crate::training::blueprint_advisor::advance_shadow_by_applied
fn navigate_subtree_by_real_actions(
    subtree: &PublicBettingTree,
    abs: &StreetActionAbstraction,
    root_real: &GameState,
    actions: &[(Action, bool)],
) -> Result<NodeId, String> {
    let mut state = root_real.clone();
    let mut id = subtree.root_id();
    for (applied, applied_is_all_in) in actions {
        let node = subtree.node(id);
        let cur = state
            .current_player()
            .ok_or("真栈导航：中途状态无行动者（与动作序不符）")?;
        if node.player_acting != cur.0 as PlayerId {
            return Err(format!(
                "真栈导航：子树节点决策者 {} ≠ 真实行动者 {}（失同步）",
                node.player_acting, cur.0
            ));
        }
        let has = |t: AbstractActionTag| node.legal_actions.contains(&t);
        let tag = match applied {
            Action::Fold => AbstractActionTag::Fold,
            Action::Check => AbstractActionTag::Check,
            Action::Call => {
                if has(AbstractActionTag::Call) {
                    AbstractActionTag::Call
                } else if *applied_is_all_in && has(AbstractActionTag::AllIn) {
                    // all-in 跟注：该 Call 在子树折进 AllIn 槽（AA-004-rev1，advance_shadow 同义）。
                    AbstractActionTag::AllIn
                } else {
                    return Err("真栈导航：被动 Call 在子树无对应（结构性 gap）".to_string());
                }
            }
            Action::AllIn => AbstractActionTag::AllIn,
            Action::Bet { to } | Action::Raise { to } => {
                let raw = AbstractActionTag::of(&abs.map_off_tree(&state, *to));
                if *applied_is_all_in || !has(raw) {
                    // 真 all-in / 最近档不在子树合法集（该档在此几何塌进 AllIn 槽）→ AllIn。
                    AbstractActionTag::AllIn
                } else {
                    raw
                }
            }
        };
        let idx = node
            .legal_actions
            .iter()
            .position(|t| *t == tag)
            .ok_or_else(|| format!("真栈导航：tag {tag:?} 不在子树节点 {id} 合法集（失同步）"))?;
        match node.children[idx] {
            Child::Decision(next) => id = next,
            Child::Terminal => {
                return Err(format!("真栈导航：tag {tag:?} 导向子树终局（不应到达）"))
            }
        }
        state
            .apply(*applied)
            .map_err(|e| format!("真栈导航：apply({applied:?}) 非法: {e:?}"))?;
    }
    Ok(id)
}

/// 缺口②续（exec 文档 v1 边界①）：**真栈锚**子博弈搜索——node_id 不再来自 100BB 影子 /
/// blueprint 全局树。off-stack all-in 线上 blueprint 树**结构性缺节点**（树按 100BB 对称栈建：
/// 短码 30BB shove 在 100BB 树里是全栈 all-in，「raise-over / call 完还活着」的后续节点根本
/// 不存在），影子导航再鲁棒也修不了 → 本入口把搜索需要的全部上下文改从**真栈**取：
///
/// - 子树根 = `root_state`（当前街轮起点快照，[`ResolveRoot::RoundStart`]）；entrants = 轮起点
///   live bitmask；raises_on_street = 0（轮起点）；
/// - within-round 导航 = 当前街真实动作序在子树上重放（[`navigate_subtree_by_real_actions`]，
///   tag 以真栈几何现算）；`within_round` 元素 = `(动作, 该动作是否令行动者 all-in)`；
/// - **range 先验**：默认退 uniform（`cfg.use_blueprint_range` 被忽略）——blueprint reach 要沿
///   全局树路径累乘，而该路径在 off-stack 线上不存在；uniform 即 §5b 留作 A/B 的那条路，且
///   off-100BB 下 blueprint range 本就是「假设 100BB 的 range」（exec 文档 §0.3）——诚实退化、
///   不假装有先验。**档一前缀 reach**（[`subgame_search_unanchored_cached`] 的 `prefix_reach =
///   Some`，`unanchored_range_design` §1）= 用**已同步前缀**（断点之前的精确 blueprint 决策，
///   跳过 AllIn-tag）估 per-seat range（[`estimate_range`] + [`PrefixReach`]）替代 uniform——更粗
///   但合法的条件化，非错条件化。本无缓存入口恒 uniform（`prefix_reach = None`）；
/// - 返回**子树自身合法集**上的分布（同 `deep_menu` 契约，[`self_distribution`]）：调用方用与
///   子树同一抽象（`deep_menu` → [`deep_menu_for`]`(root_state)`，否则 blueprint 菜单）按
///   `auth` 真实几何做 outgoing。
///
/// 仅支持 postflop + [`ResolveRoot::RoundStart`]（preflop 走 blueprint，§1 gating；
/// `CurrentDecision` 是影子锚的 A/B 旧模式、不在生产路径）；`depth_limit` 需要 blueprint 树锚做
/// 叶子映射 → 不支持。失败语义同 [`subgame_search`]：`Err` → 调用方降级 check-when-free、不回落
/// blueprint（§2.3）。
pub fn subgame_search_unanchored(
    auth: &GameState,
    root_state: &GameState,
    game: &SimplifiedNlheGame,
    within_round: &[(Action, bool)],
    cfg: &SubgameSearchConfig,
    hand_seed: u64,
) -> Result<Vec<(SimplifiedNlheAction, f64)>, String> {
    subgame_search_unanchored_cached(
        None,
        auth,
        root_state,
        game,
        within_round,
        cfg,
        None,
        None,
        hand_seed,
    )
}

/// [`subgame_search_unanchored`] 的缓存版（与 [`subgame_search_cached`] 同语义）：`cache = Some`
/// 且 key 命中（同手同街，[`solve_cache_key`]）→ 复用已解 trainer、只重做 within-round 真实
/// 动作导航 + 读数（导航用 solve 时存下的同一份 sub_abs）。`cache = None` = 原行为。
/// `bucket_override` 语义同 [`subgame_search_cached`]（换表只动子树归桶 + hero 读数；depth_limit
/// 本路径恒不支持，无兼容性问题）。
///
/// `prefix_reach`（档一，`unanchored_range_design` §1）：`Some` = 用**已同步前缀**估 per-seat
/// range 先验替代 uniform（[`PrefixReach`] doc）。算出的 reach 向量进 solve 缓存 key
/// （[`solve_cache_key`] 的 ranges 项）→ 开/关前缀 reach 自动 cache miss，不会读错均衡。
/// `None`（默认）= uniform（既有行为，**逐 infoset byte-equal、不改既有调用点**）。**前缀里没有
/// 当前街之前的决策**（如 limp 池在首动作即失同步 → 前缀为空）→ 退 uniform 路径（byte-equal）。
#[allow(clippy::too_many_arguments)]
pub fn subgame_search_unanchored_cached(
    cache: Option<&mut SubgameSolveCache>,
    auth: &GameState,
    root_state: &GameState,
    game: &SimplifiedNlheGame,
    within_round: &[(Action, bool)],
    cfg: &SubgameSearchConfig,
    bucket_override: Option<&Arc<BucketTable>>,
    prefix_reach: Option<PrefixReach<'_>>,
    hand_seed: u64,
) -> Result<Vec<(SimplifiedNlheAction, f64)>, String> {
    subgame_search_unanchored_cached_inner(
        cache,
        auth,
        root_state,
        game,
        within_round,
        cfg,
        bucket_override,
        prefix_reach,
        None,
        hand_seed,
        false,
    )
}

/// [`subgame_search_unanchored_cached`] 本体 + 预热钩子。`solve_only` 语义同
/// [`subgame_search_cached_inner`]。`actor_override`：`Some(hero)` = range 平滑的「hero 不混」
/// 座（[`mix_lambda_for_seat`]）按它算而非 `auth.current_player()`——前缀 reach 预热在该街 hero
/// 行动**前**（`auth = round_start`、current_player = 首行动者 ≠ hero），不传 hero 则混错座、
/// ranges 与决策时不同 → key 必 miss、预热静默失效（同 anchored [`subgame_search_cached_inner`]
/// 的 actor_override）。`None`（决策路径）= `auth.current_player()`（= hero，require_my_turn）。
/// `prefix_reach = None` 时本参数不被读（uniform 无混合）。
#[allow(clippy::too_many_arguments)]
fn subgame_search_unanchored_cached_inner(
    cache: Option<&mut SubgameSolveCache>,
    auth: &GameState,
    root_state: &GameState,
    game: &SimplifiedNlheGame,
    within_round: &[(Action, bool)],
    cfg: &SubgameSearchConfig,
    bucket_override: Option<&Arc<BucketTable>>,
    prefix_reach: Option<PrefixReach<'_>>,
    actor_override: Option<PlayerId>,
    hand_seed: u64,
    solve_only: bool,
) -> Result<Vec<(SimplifiedNlheAction, f64)>, String> {
    if auth.is_terminal() || auth.current_player().is_none() {
        return Err("subgame_search_unanchored: auth 非 decision 节点".to_string());
    }
    if root_state.is_terminal() || root_state.current_player().is_none() {
        return Err("subgame_search_unanchored: root_state 非 decision 节点".to_string());
    }
    if cfg.depth_limit {
        return Err(
            "subgame_search_unanchored: depth_limit 需要 blueprint 树锚（叶子映射），不支持"
                .to_string(),
        );
    }
    if cfg.resolve_root != ResolveRoot::RoundStart {
        return Err(
            "subgame_search_unanchored: 仅支持 RoundStart（CurrentDecision 是影子锚的 A/B 旧模式）"
                .to_string(),
        );
    }
    let auth_actor = actor_override
        .unwrap_or_else(|| auth.current_player().expect("checked above").0 as PlayerId);
    if root_state.street() != auth.street() {
        return Err("subgame_search_unanchored: root_state 与 auth 不同街".to_string());
    }
    // postflop 限定（preflop 走 blueprint，§1 gating；StreetTag 无 Showdown）。street_tag 兼作
    // round-stable seed ordinal——与 anchored RoundStart 同口径（同手同街 → 同 solve）。
    let street_tag = match auth.street() {
        Street::Flop => StreetTag::Flop,
        Street::Turn => StreetTag::Turn,
        Street::River => StreetTag::River,
        _ => {
            return Err(
                "subgame_search_unanchored: 仅 postflop（preflop 走 blueprint）".to_string(),
            )
        }
    };

    // 子树菜单：deep_menu → 按根 SPR 自适应（deep_menu_for：深 {1pot} / 浅 {0.5,1}，缺口③ v2
    // 细化）；否则 blueprint 自身抽象/规则（与 anchored 一致）。
    let (sub_abs, mut sub_rules) = if cfg.deep_menu {
        deep_menu_for(root_state)
    } else {
        (game.abstraction().clone(), game.rules())
    };
    // 真栈子树解**真游戏宽度**：A4 width_redirect 是 blueprint 训练期的 preflop 收口装置
    // （>N-way 历史在 100BB 影子上必 desync、根本到不了锚定路径），真实牌局 4+way 见 flop 合法
    // 且常见（主目标分布，§2.2）。脱影子子树从 postflop 根建——redirect 只影响 preflop 菜单 +
    // 触发 build_subtree 的 ≤N 断言（panic，live 不可崩）——关掉它让真 N-way 子树可建，树宽由
    // max_subtree_nodes cap 兜底（越界 → Err → check-when-free）。deep_menu_for 两档的 rules
    // 本即 redirect 关（deep_single_pot / deep_wide_half_pot 的 rules 都是 Default），
    // 两路一致。
    sub_rules.width_redirect = BettingAbstractionRules::WIDTH_REDIRECT_OFF;
    // 真栈锚上下文：entrants = 轮起点 live bitmask；raises_on_street = 0（轮起点）。
    let entrants = live_entrants(root_state);

    // 档一前缀 reach（PrefixReach=Some 且有当前街之前的前缀决策）→ 为每个未弃牌座位估 marginal
    // range（new_with_ranges 加权采样底牌），替代 uniform（SubgameNlheGame::new 的 root uniform
    // resample）。只用**当前街之前**的决策（当前街 betting 在子博弈内由 CFR 解，§2）；AllIn-tag
    // 跳过（estimate_range skip_all_in=true，档一 v1）；对手座位做 uniform 平滑（range_uniform_mix），
    // hero（auth_actor）不混（mix_lambda_for_seat）。**前缀里无当前街之前的决策（如 limp 池首动作
    // 即失同步）→ ranges=None 退 uniform 路径**（与既有 byte-equal，不走 new_with_ranges 的均匀
    // 向量——那是不同采样路径、不 byte-equal）。
    let ranges_opt: Option<Vec<Vec<f64>>> = prefix_reach.and_then(|pr| {
        let tree = game.tree();
        let cur = root_state.street() as u8;
        let prior: Vec<(NodeId, AbstractActionTag, PlayerId)> = pr
            .decisions
            .iter()
            .filter(|(nid, _, _)| (tree.node(*nid).street as u8) < cur)
            .copied()
            .collect();
        if prior.is_empty() {
            return None; // 前缀无当前街之前的决策 → 退 uniform（byte-equal）。
        }
        let holes = all_hole_combos();
        let board: Vec<Card> = root_state.board().to_vec();
        let players = root_state.players();
        Some(
            (0..players.len())
                .map(|seat| {
                    if players[seat].hole_cards.is_some() {
                        let mut r = estimate_range(
                            game,
                            pr.strategy,
                            &prior,
                            &board,
                            seat as PlayerId,
                            &holes,
                            true, // 档一：跳过 AllIn-tag（100BB shove 语义，off-100BB 撒谎）。
                        );
                        mix_range_with_uniform(
                            &mut r,
                            &holes,
                            &board,
                            mix_lambda_for_seat(
                                seat as PlayerId,
                                auth_actor,
                                cfg.range_uniform_mix,
                            ),
                        );
                        r
                    } else {
                        Vec::new() // 弃牌座：range 不被读。
                    }
                })
                .collect(),
        )
    });

    // 子树 solve 实际用表（同 anchored：override 优先，否则 blueprint 表 byte-equal）。
    let sub_table: Arc<BucketTable> =
        bucket_override.map_or_else(|| Arc::clone(&game.bucket_table), Arc::clone);
    let master = search_seed(cfg.seed, hand_seed, street_tag as u64);
    let key = cache.as_ref().map(|_| {
        solve_cache_key(
            KEY_KIND_UNANCHORED,
            &sub_table,
            root_state,
            entrants,
            0,
            ranges_opt.as_deref(),
            &sub_abs,
            &sub_rules,
            cfg,
            hand_seed,
            street_tag as u64,
        )
    });
    let build_and_solve = move || -> Result<SolvedSubgame, String> {
        let sub = if let Some(ranges) = ranges_opt {
            SubgameNlheGame::new_with_ranges(
                Arc::clone(&sub_table),
                root_state.config().clone(),
                sub_abs.clone(), // 真栈导航还要同一抽象现算 tag
                sub_rules,
                root_state.clone(),
                entrants,
                0,
                ranges,
            )
        } else {
            SubgameNlheGame::new(
                Arc::clone(&sub_table),
                root_state.config().clone(),
                sub_abs.clone(),
                sub_rules,
                root_state.clone(),
                entrants,
                0,
            )
        };
        let trainer = solve_subgame(sub, cfg, master)?;
        Ok(SolvedSubgame { trainer, sub_abs })
    };
    let solved_fresh: SolvedSubgame;
    let solved: &SolvedSubgame = match (cache, key) {
        (Some(c), Some(key)) => {
            if !c.lookup(key) {
                c.store(key, build_and_solve()?);
            }
            c.entry(key).expect("lookup 命中或刚 store")
        }
        _ => {
            solved_fresh = build_and_solve()?;
            &solved_fresh
        }
    };
    // 预热（solve_only）：solve 已入缓存即达成目的，跳过导航/读数。
    if solve_only {
        return Ok(Vec::new());
    }
    let trainer = &solved.trainer;
    let cur_node = navigate_subtree_by_real_actions(
        trainer.game().subtree(),
        &solved.sub_abs,
        root_state,
        within_round,
    )?;
    let (avg, sub_legal) = read_current_strategy(trainer, cur_node, auth, auth_actor)?;
    self_distribution(&sub_legal, &avg)
}

// ===========================================================================
// RoundStart 预热（2026-06-12）：该街 hero 行动前把 solve 提前算进缓存
// ===========================================================================

/// RoundStart 预热（**锚定**路径）：街起点一确定（板发出、对手还在行动），就用街起点快照把
/// build+solve 提前算进 [`SubgameSolveCache`]——hero 首决策时 key 命中、只做导航/读数，
/// build+solve wall 藏进对手思考时间（既有「mid-round wall ≈ 0」机制经此扩到首决策）。
///
/// 成立的根基 = [`ResolveRoot::RoundStart`] 设计：solve 的**全部**输入（root_state / entrants /
/// raises=0 / ranges（只依赖之前街）/ seed_ordinal=街索引 / hand_seed（不含 actions/board））在
/// 街开始那一刻已知、与街内后续动作无关。`hero` = 之后读数的座位——range 平滑的「hero 不混」
/// 座位必须按它算而非街首行动者（[`subgame_search_cached_inner`] `actor_override` doc，否则
/// ranges 不同 → key 必 miss、预热静默失效）。`node_id` = **街起点**影子节点（与决策时 hero
/// 节点同路径前缀 → RoundStart 推导逐项相等 → 同 key）。
///
/// **失败无害**：预热没成（Err / key 对不上）只丢 wall 收益，hero 决策时现解——key 覆盖 solve
/// 全部输入，错配 = miss = 现解，不可能读错均衡。仅支持 RoundStart + 非 depth_limit
/// （depth_limit 路径不入缓存，预热无处可存）。
#[allow(clippy::too_many_arguments)]
pub fn subgame_search_prewarm(
    cache: &mut SubgameSolveCache,
    hero: PlayerId,
    round_start: &GameState,
    game: &SimplifiedNlheGame,
    node_id: NodeId,
    strategy: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    cfg: &SubgameSearchConfig,
    bucket_override: Option<&Arc<BucketTable>>,
    hand_seed: u64,
) -> Result<(), String> {
    if cfg.resolve_root != ResolveRoot::RoundStart {
        return Err(
            "subgame_search_prewarm: 仅 RoundStart（CurrentDecision 逐决策独立重解，无预热意义）"
                .to_string(),
        );
    }
    if cfg.depth_limit {
        return Err(
            "subgame_search_prewarm: depth_limit 路径不入缓存（subgame_search_cached doc）"
                .to_string(),
        );
    }
    subgame_search_cached_inner(
        Some(cache),
        round_start,
        round_start,
        game,
        &[],
        node_id,
        strategy,
        cfg,
        bucket_override,
        None,
        None,
        hand_seed,
        0, // RoundStart 的 seed_ordinal = 街索引，本参数不被读。
        Some(hero),
        true,
    )
    .map(|_| ())
}

/// RoundStart 预热（**脱影子**路径）：语义同 [`subgame_search_prewarm`]。`round_start` 兼作 auth
/// （街起点是 decision 节点）。`prefix_reach`（档一）= `Some` 时用已同步前缀估 range——预热须与
/// 决策时**算出同一份 ranges** 才能命中 key，故须传 `hero`（range 平滑「不混」座按它算而非街首
/// 行动者，否则 ranges 不同 → key miss → 预热静默失效；同 anchored [`subgame_search_prewarm`]）。
/// `prefix_reach = None` = uniform（`hero` 不被读、传任意值即可）。失败无害（同上）。
#[allow(clippy::too_many_arguments)]
pub fn subgame_search_unanchored_prewarm(
    cache: &mut SubgameSolveCache,
    hero: PlayerId,
    round_start: &GameState,
    game: &SimplifiedNlheGame,
    prefix_reach: Option<PrefixReach<'_>>,
    cfg: &SubgameSearchConfig,
    bucket_override: Option<&Arc<BucketTable>>,
    hand_seed: u64,
) -> Result<(), String> {
    subgame_search_unanchored_cached_inner(
        Some(cache),
        round_start,
        round_start,
        game,
        &[],
        cfg,
        bucket_override,
        prefix_reach,
        Some(hero),
        hand_seed,
        true,
    )
    .map(|_| ())
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
    use crate::training::nlhe_betting_tree::{
        deep_single_pot, deep_wide_half_pot, first_small_6max,
    };
    use crate::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
    use crate::training::subgame_leaf_value::{build_leaf_value_tables, default_continuations};

    fn stub_table() -> Arc<BucketTable> {
        Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ))
    }

    // ---- project_strategy_onto_auth_legal（deep-wide 菜单维度漂移修复）----
    // 复现 OpenPoker AK turn 弃牌的根因：deep-wide `{0.5,1}` 子树 4 维解，真栈下 0.5pot 加注塌
    // 进 all-in → query_at 只剩 3 合法动作。旧版在 read_current_strategy 硬 Err → giveup → 弃牌；
    // 新版按 tag 投影、塌缩 mass 并入 AllIn。直接喂合成输入（不必跑 CFR），错算法（旧的「维度不
    // 等就 Err」/ 把塌缩 mass 丢掉）都会 fail。
    use crate::abstraction::action::BetRatio;

    fn t_raise(r: BetRatio) -> AbstractActionTag {
        AbstractActionTag::Raise(r)
    }
    fn a_call(to: u64) -> SimplifiedNlheAction {
        AbstractAction::Call {
            to: ChipAmount::new(to),
        }
    }
    fn a_raise(to: u64, r: BetRatio) -> SimplifiedNlheAction {
        AbstractAction::Raise {
            to: ChipAmount::new(to),
            ratio_label: r,
        }
    }
    fn a_allin(to: u64) -> SimplifiedNlheAction {
        AbstractAction::AllIn {
            to: ChipAmount::new(to),
        }
    }

    #[test]
    fn project_identity_when_no_collapse() {
        // 真栈合法集 tag 与建树 tag 完全一致 → 投影 = 恒等（byte-equal 既有路径）。
        let node_tags = vec![
            AbstractActionTag::Fold,
            AbstractActionTag::Call,
            t_raise(BetRatio::HALF_POT),
            AbstractActionTag::AllIn,
        ];
        let avg = vec![0.1, 0.2, 0.3, 0.4];
        let sub_legal = vec![
            AbstractAction::Fold,
            a_call(746),
            a_raise(1554, BetRatio::HALF_POT),
            a_allin(1598),
        ];
        let out = project_strategy_onto_auth_legal(&node_tags, &avg, &sub_legal).expect("不应 Err");
        assert_eq!(out, avg, "无塌缩须恒等");
    }

    #[test]
    fn project_merges_collapsed_raise_into_allin() {
        // AK turn 复现：建树 [Fold,Call,Raise0.5,AllIn]（4 维），真栈 0.5pot 塌进 all-in →
        // sub_legal [Fold,Call,AllIn]（3 档）。Raise0.5 的 mass 0.3 须并入 AllIn 的 0.4 = 0.7。
        let node_tags = vec![
            AbstractActionTag::Fold,
            AbstractActionTag::Call,
            t_raise(BetRatio::HALF_POT),
            AbstractActionTag::AllIn,
        ];
        let avg = vec![0.1, 0.2, 0.3, 0.4];
        let sub_legal = vec![AbstractAction::Fold, a_call(746), a_allin(1598)];
        let out = project_strategy_onto_auth_legal(&node_tags, &avg, &sub_legal).expect("不应 Err");
        assert_eq!(out.len(), 3);
        assert!((out[0] - 0.1).abs() < 1e-12, "Fold 不动");
        assert!((out[1] - 0.2).abs() < 1e-12, "Call 不动");
        assert!((out[2] - 0.7).abs() < 1e-12, "Raise0.5 ⊕ AllIn = 0.7");
        assert!(
            (out.iter().sum::<f64>() - 1.0).abs() < 1e-12,
            "投影保总概率"
        );
    }

    #[test]
    fn project_merges_two_collapsed_raises() {
        // 建树双加注档 {0.5,1.0} 真栈双塌 → 两份 mass 都进 AllIn。
        let node_tags = vec![
            AbstractActionTag::Fold,
            AbstractActionTag::Call,
            t_raise(BetRatio::HALF_POT),
            t_raise(BetRatio::FULL_POT),
            AbstractActionTag::AllIn,
        ];
        let avg = vec![0.1, 0.2, 0.25, 0.15, 0.3];
        let sub_legal = vec![AbstractAction::Fold, a_call(900), a_allin(1598)];
        let out = project_strategy_onto_auth_legal(&node_tags, &avg, &sub_legal).expect("不应 Err");
        assert!(
            (out[2] - 0.7).abs() < 1e-12,
            "0.25+0.15+0.3 = 0.7 全进 AllIn"
        );
        assert!((out.iter().sum::<f64>() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn project_errs_when_raise_collapses_but_no_allin_slot() {
        // 加注档缺失却无 AllIn 落点 = 结构失配（塌缩不可能产生此形）→ Err 降级，不静默丢 mass。
        let node_tags = vec![
            AbstractActionTag::Fold,
            AbstractActionTag::Call,
            t_raise(BetRatio::HALF_POT),
        ];
        let avg = vec![0.3, 0.3, 0.4];
        let sub_legal = vec![AbstractAction::Fold, a_call(746)];
        assert!(project_strategy_onto_auth_legal(&node_tags, &avg, &sub_legal).is_err());
    }

    #[test]
    fn project_merges_collapsed_call_into_allin() {
        // KK river all-in-for-less 复现（live searchon100 弃 KsKc 的根因）：对手真实注 ≥ hero
        // 剩余栈 → 真栈下 hero 跟注即 all-in，query_at 把跟注折进 AllIn 槽（无独立 Call）。而
        // 建树期对手注被 {0.5,1} 宽档 off-tree 映小 → 建树 hero 节点 [Fold,Call,AllIn]（3 维解，
        // Call 非 all-in）。真栈 sub_legal [Fold,AllIn]（2 档）→ Call 的 mass 0.3 须并入 AllIn 的
        // 0.4 = 0.7（旧版在此把 Call 当不可塌缩硬 Err → giveup → 弃 KK）。
        let node_tags = vec![
            AbstractActionTag::Fold,
            AbstractActionTag::Call,
            AbstractActionTag::AllIn,
        ];
        let avg = vec![0.3, 0.3, 0.4];
        let sub_legal = vec![AbstractAction::Fold, a_allin(1598)];
        let out = project_strategy_onto_auth_legal(&node_tags, &avg, &sub_legal).expect("不应 Err");
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.3).abs() < 1e-12, "Fold 不动");
        assert!(
            (out[1] - 0.7).abs() < 1e-12,
            "Call ⊕ AllIn = 0.7（all-in-for-less）"
        );
        assert!(
            (out.iter().sum::<f64>() - 1.0).abs() < 1e-12,
            "投影保总概率"
        );
    }

    #[test]
    fn project_errs_when_uncollapsible_tag_missing() {
        // Fold/Check 无金额、不随真栈变 → 缺失只能是真失同步（非 all-in 塌缩）→ Err 降级。
        // 这里 Check 在真栈合法集缺失（建树以为可 check、真栈面对注）→ 不可塌缩 → Err。
        let node_tags = vec![
            AbstractActionTag::Check,
            t_raise(BetRatio::HALF_POT),
            AbstractActionTag::AllIn,
        ];
        let avg = vec![0.5, 0.2, 0.3];
        let sub_legal = vec![a_call(746), a_allin(1598)];
        assert!(
            project_strategy_onto_auth_legal(&node_tags, &avg, &sub_legal).is_err(),
            "Check 缺失（不可塌缩）须 Err"
        );
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
            deep_menu: false,
            live_traversers: false,
            range_uniform_mix: 0.0,
            solve_threads: 1,
        };
        let node_id = flop.current_node_id;
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let mut ok_count = 0usize;
        for hand_seed in 0u64..3 {
            let ordinal = 3u64;
            let r1 = subgame_search(
                &auth, &auth, &game, &legal_abs, node_id, &strat, &cfg, None, None, hand_seed,
                ordinal,
            );
            let r2 = subgame_search(
                &auth, &auth, &game, &legal_abs, node_id, &strat, &cfg, None, None, hand_seed,
                ordinal,
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
            deep_menu: false,
            live_traversers: false,
            range_uniform_mix: 0.0,
            solve_threads: 1,
        };
        let r = subgame_search(
            &auth, &auth, &game, &legal_abs, node_id, &strat, &tiny, None, None, 0, 0,
        );
        assert!(r.is_err(), "节点上限被触发应回落 Err，实得 {r:?}");
    }

    /// 缺口③：`deep_menu` 子树用 {1pot} 单档菜单（[`deep_single_pot`]）——验
    /// ①**不因菜单不匹配而 Err**（{1pot} ⊊ blueprint `{0.5,1,2}`，旧契约对齐 `legal_abs` 必失败）；
    /// ②返回分布的 Bet/Raise **只含 1.0pot 档**（证 {1pot} 菜单确在子树生效）；③归一 / 正概率 / 可复现。
    #[test]
    fn subgame_search_deep_menu_single_pot() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x4445_4550_315F_5054); // "DEEP1_PT"
        let auth = flop.game_state.clone();
        let legal_abs = SimplifiedNlheGame::legal_actions(&flop);
        // 前置：blueprint legal_abs 含非 1.0pot 档（0.5/2.0）→ 证明菜单不同（旧对齐会在这些档上 Err）。
        let has_non_full_pot = legal_abs.iter().any(|a| {
            matches!(a,
                AbstractAction::Bet { ratio_label, .. } | AbstractAction::Raise { ratio_label, .. }
                if ratio_label.as_milli() != 1000)
        });
        assert!(
            has_non_full_pot,
            "前置：default 菜单 legal_abs 应含非 1.0pot 档，得 {legal_abs:?}"
        );

        // AllPostflop + RoundStart：flop 首点 round-start == 当前点（within tags 空 → 导航回 root）。
        let cfg = SubgameSearchConfig {
            iterations: 400,
            max_subtree_nodes: 1_000_000,
            trigger: SearchTrigger::AllPostflop,
            resolve_root: ResolveRoot::RoundStart,
            deep_menu: true,
            ..SubgameSearchConfig::default()
        };
        let node_id = flop.current_node_id;
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let d1 = subgame_search(
            &auth, &auth, &game, &legal_abs, node_id, &strat, &cfg, None, None, 7, 3,
        )
        .expect("deep_menu 子树解应 Ok（不因菜单不匹配 Err）");
        let sum: f64 = d1.iter().map(|(_, p)| *p).sum();
        assert!((sum - 1.0).abs() < 1e-9, "返回分布须归一，和={sum}");
        for (a, p) in &d1 {
            assert!(*p > 0.0, "只返回正概率动作");
            if let AbstractAction::Bet { ratio_label, .. }
            | AbstractAction::Raise { ratio_label, .. } = a
            {
                assert_eq!(
                    ratio_label.as_milli(),
                    1000,
                    "deep_menu 子树只许 1.0pot 档，得 {a:?}"
                );
            }
        }
        // 可复现：同 (hand_seed, ordinal) 两次逐项 byte-equal。
        let d2 = subgame_search(
            &auth, &auth, &game, &legal_abs, node_id, &strat, &cfg, None, None, 7, 3,
        )
        .expect("可复现：第二次也应 Ok");
        assert_eq!(d1.len(), d2.len(), "可复现：维度一致");
        for ((a1, p1), (a2, p2)) in d1.iter().zip(&d2) {
            assert_eq!(a1, a2, "可复现：动作一致");
            assert_eq!(p1.to_bits(), p2.to_bits(), "可复现：概率 byte-equal");
        }
    }

    /// 缺口③ guard：`deep_menu` 与 `depth_limit` 同开 = 配置错误 → `Err`（不静默择一）。
    #[test]
    fn deep_menu_and_depth_limit_mutually_exclusive() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x4D55_5445_5845_5843); // "MUTEXEXC"
        let auth = flop.game_state.clone();
        let legal_abs = SimplifiedNlheGame::legal_actions(&flop);
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let cfg = SubgameSearchConfig {
            iterations: 50,
            deep_menu: true,
            depth_limit: true,
            ..SubgameSearchConfig::default()
        };
        let r = subgame_search(
            &auth,
            &auth,
            &game,
            &legal_abs,
            flop.current_node_id,
            &strat,
            &cfg,
            None,
            None,
            0,
            0,
        );
        assert!(r.is_err(), "deep_menu+depth_limit 同开应 Err，得 {r:?}");
    }

    /// 缺口③ v2 细化（SPR + 人数自适应菜单宽度，exec §2.1「深码单档、短码可放宽」）。
    /// **2026-06-13 阈值：≤3-way = 40×pot / 4-way = 20×pot / 5+way = 一律 {1pot}**（按
    /// `_measure_deep_wide_menu_tree_sizes` 实测分档，控建树 wall + 不撑爆 4M cap）：
    /// ① 极深 SPR（HU ≈ 99×pot > 40）→ {1pot} 单档；
    /// ② 3-way 40×pot 边界 → {0.5,1} 两档（边界含等号取宽）；
    /// ③ 3-way 刚过 40×pot → 回 {1pot}；
    /// ④ 4-way 20×pot 边界 → {0.5,1} 两档；
    /// ⑤ 4-way 刚过 20×pot → 回 {1pot}（4-way 更深 = SPR40 树 3.98M 贴满 cap，故阈值更低）；
    /// ⑥ 5+way（6-way 浅 4×pot）→ 人数闸一律 {1pot}（6-way 宽档实测 558k，乘性爆炸）；
    /// ⑦ 生产边界宽档树大小护栏：两档最大宽树（3-way@40× + 4-way@20×）实测 < 4M cap
    ///   （越界即 live giveup → 测试失败逼回滚），真实数值 eprintln。
    #[test]
    fn deep_menu_spr_adaptive_selection_and_boundary_tree_bounded() {
        let bet_ratios = |abs: &StreetActionAbstraction, st: &GameState| -> Vec<u32> {
            abs.abstract_actions(st)
                .iter()
                .filter_map(|a| match a {
                    AbstractAction::Bet { ratio_label, .. }
                    | AbstractAction::Raise { ratio_label, .. } => Some(ratio_label.as_milli()),
                    _ => None,
                })
                .collect()
        };
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        // ① 极深 SPR（HU limped ≈ 99×pot > 40）→ {1pot} 单档。
        let deep_state = hu_flop_state(&game, 0x5350_525F_4D4E_5530).game_state; // "SPR_MNU0"
        let (deep_abs, _) = deep_menu_for(&deep_state);
        assert_eq!(
            bet_ratios(&deep_abs, &deep_state),
            vec![1000],
            "极深 SPR（>40×pot）应选 {{1pot}} 单档"
        );

        // ② 3-way 40×pot 边界：limped pot=300、各家剩 12000 = 40×300（stack=12100）。
        let b3_wide = nway_limped_flop_state(3, 12_100, 0x5350_525F_4D4E_5531);
        let (w3_abs, w3_rules) = deep_menu_for(&b3_wide);
        assert_eq!(
            bet_ratios(&w3_abs, &b3_wide),
            vec![500, 1000],
            "3-way 40×pot 边界应选 {{0.5,1}} 两档"
        );
        assert!(
            !w3_rules.drop_small_reraise,
            "宽档 0.5pot 须全层级可用（含 re-raise）"
        );

        // ③ 3-way 刚过 40×pot（剩 12100 > 12000）→ 回 {1pot}。
        let b3_narrow = nway_limped_flop_state(3, 12_200, 0x5350_525F_4D4E_5532);
        assert_eq!(
            bet_ratios(&deep_menu_for(&b3_narrow).0, &b3_narrow),
            vec![1000],
            "3-way 刚过 40×pot 应回 {{1pot}}"
        );

        // ④ 4-way 20×pot 边界：limped pot=400、各家剩 8000 = 20×400（stack=8100）。
        let b4_wide = nway_limped_flop_state(4, 8_100, 0x5350_525F_4D4E_5534);
        assert_eq!(
            bet_ratios(&deep_menu_for(&b4_wide).0, &b4_wide),
            vec![500, 1000],
            "4-way 20×pot 边界应选 {{0.5,1}} 两档"
        );

        // ⑤ 4-way 刚过 20×pot（剩 8100 > 8000）→ 回 {1pot}（4-way 阈值更低，见 SPR sweep）。
        let b4_narrow = nway_limped_flop_state(4, 8_200, 0x5350_525F_4D4E_5535);
        assert_eq!(
            bet_ratios(&deep_menu_for(&b4_narrow).0, &b4_narrow),
            vec![1000],
            "4-way 刚过 20×pot 应回 {{1pot}}（4-way SPR40 树 3.98M 贴满 cap）"
        );

        // ⑥ 5+way：6-way 浅 4×pot → 人数闸一律 {1pot}。
        let mw6 = nway_limped_flop_state(6, 2_500, 0x5350_525F_4D4E_5533);
        assert_eq!(
            bet_ratios(&deep_menu_for(&mw6).0, &mw6),
            vec![1000],
            ">4 Active 应被人数闸拦回 {{1pot}}（6-way 宽档实测 558k 节点）"
        );

        // ⑦ 生产边界宽档树大小护栏：两档最大宽树 < 4M cap（越界即 live giveup）。
        let wide_nodes = |st: &GameState| {
            SubgameNlheGame::new(
                stub_table(),
                st.config().clone(),
                deep_wide_half_pot().0,
                deep_wide_half_pot().1,
                st.clone(),
                live_entrants(st),
                0,
            )
            .subtree()
            .num_nodes()
        };
        let n3 = wide_nodes(&b3_wide);
        let n4 = wide_nodes(&b4_wide);
        eprintln!(
            "[deep_menu SPR 边界] 生产最大宽树：3-way@40×pot={n3} / 4-way@20×pot={n4}（cap 4M）"
        );
        const PROD_CAP: usize = 4_000_000; // openpoker_advisor --search-max-nodes 生产值
        assert!(
            n3 < PROD_CAP && n4 < PROD_CAP,
            "边界宽档撑爆生产 cap：3-way@40×={n3} / 4-way@20×={n4} ≥ {PROD_CAP}（阈值不可行 → 调低）"
        );
    }

    /// 缺口③「仍未做③」（deep_menu 配 `AllPostflop` 的 within-round 导航）：mid-round 决策的
    /// blueprint within-round tags（0.5pot 档）在 {1pot} 子树上**必失配**；deep_menu 路径改用
    /// **当前街真实动作序**在子树上重放导航（与 unanchored 同口径）。验：
    /// ① 提供 `within_round_real` → `Ok`，分布归一、Bet/Raise 只含 1.0pot 档（{1pot} 子树）；
    /// ② 未提供（`None`）→ 优雅 `Err`（安全降级语义，不 panic）；
    /// ③ 同输入两次 byte-equal（可复现）。
    #[test]
    fn deep_menu_allpostflop_midround_navigates_by_real_actions() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x4450_4D49_4452_4E44); // "DPMIDRND"
        let round_start = flop.game_state.clone();

        // 首行动者打 0.5pot（blueprint 档、{1pot} 子树没有）→ 对手 mid-round 决策点。
        let half = SimplifiedNlheGame::legal_actions(&flop)
            .into_iter()
            .find(|a| {
                matches!(AbstractActionTag::of(a),
                    AbstractActionTag::Bet(r) if r.as_milli() == 500)
            })
            .expect("default {0.5,1,2} 菜单应有 0.5pot 档");
        let real_bet = match &half {
            AbstractAction::Bet { to, .. } => Action::Bet { to: *to },
            other => panic!("0.5pot 档应是 Bet，得 {other:?}"),
        };
        let mut rng = ChaCha20Rng::from_seed(0x4450_4D49_4452_0001);
        let drng: &mut dyn RngSource = &mut rng;
        let sb = SimplifiedNlheGame::next(flop.clone(), half, drng);
        assert_eq!(sb.game_state.street(), Street::Flop, "0.5pot 后仍 flop");
        let auth = sb.game_state.clone();
        let node_id = sb.current_node_id;
        let legal_abs = SimplifiedNlheGame::legal_actions(&sb);
        let within: Vec<(Action, bool)> = vec![(real_bet, false)];

        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let cfg = SubgameSearchConfig {
            iterations: 400,
            max_subtree_nodes: 1_000_000,
            trigger: SearchTrigger::AllPostflop,
            resolve_root: ResolveRoot::RoundStart,
            deep_menu: true,
            ..SubgameSearchConfig::default()
        };
        // ① 真实动作序导航 → Ok。
        let d1 = subgame_search(
            &auth,
            &round_start,
            &game,
            &legal_abs,
            node_id,
            &strat,
            &cfg,
            None,
            Some(&within),
            7,
            3,
        )
        .expect("deep_menu mid-round 提供真实动作序应 Ok（真栈几何重放导航）");
        let sum: f64 = d1.iter().map(|(_, p)| *p).sum();
        assert!((sum - 1.0).abs() < 1e-9, "分布须归一，和={sum}");
        for (a, p) in &d1 {
            assert!(*p > 0.0, "只返回正概率动作");
            if let AbstractAction::Bet { ratio_label, .. }
            | AbstractAction::Raise { ratio_label, .. } = a
            {
                assert_eq!(
                    ratio_label.as_milli(),
                    1000,
                    "深 SPR deep_menu 子树只许 1.0pot 档，得 {a:?}"
                );
            }
        }
        // ② 未提供真实动作序 → Err（旧降级语义保留，原因明确）。
        let r = subgame_search(
            &auth,
            &round_start,
            &game,
            &legal_abs,
            node_id,
            &strat,
            &cfg,
            None,
            None,
            7,
            3,
        );
        assert!(
            r.is_err(),
            "deep_menu mid-round 未提供真实动作序应 Err（安全降级），得 {r:?}"
        );
        // ③ 可复现。
        let d2 = subgame_search(
            &auth,
            &round_start,
            &game,
            &legal_abs,
            node_id,
            &strat,
            &cfg,
            None,
            Some(&within),
            7,
            3,
        )
        .expect("可复现：第二次也应 Ok");
        assert_eq!(d1.len(), d2.len(), "可复现：维度一致");
        for ((a1, p1), (a2, p2)) in d1.iter().zip(&d2) {
            assert_eq!(a1, a2, "可复现：动作一致");
            assert_eq!(p1.to_bits(), p2.to_bits(), "可复现：概率 byte-equal");
        }
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
            deep_menu: false,
            live_traversers: false,
            range_uniform_mix: 0.0,
            solve_threads: 1,
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

        let range = estimate_range(&game, &strat, &decisions, &board, actor, &holes, false);
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

    /// [`mix_range_with_uniform`]：集中 range 混合后每个合法组合有保底 `λ/n_valid`、撞 board
    /// 恒 0、和保持 1；λ=0 不动；全零（无信号）保持全零；λ=1（及 >1 clamp）= 精确 uniform。
    /// 错的混合（漏保底 / 撞 board 渗权 / 破归一）任一条都 fail。
    #[test]
    fn mix_range_with_uniform_floor_and_normalization() {
        let holes = all_hole_combos();
        // board = 2c 3d 4h（u8: 0 / 5 / 10）。
        let board: Vec<Card> = [0u8, 5, 10]
            .iter()
            .map(|v| Card::from_u8(*v).expect("<52"))
            .collect();
        let board_set: BTreeSet<u8> = board.iter().map(|c| c.to_u8()).collect();
        let valid: Vec<bool> = holes
            .iter()
            .map(|h| !board_set.contains(&h[0].to_u8()) && !board_set.contains(&h[1].to_u8()))
            .collect();
        let n_valid = valid.iter().filter(|v| **v).count();
        assert_eq!(n_valid, 1176, "C(49,2)");
        let k = valid.iter().position(|v| *v).expect("有合法 hole");

        // 集中 range：全部权重在 hole k。
        let mut r = vec![0.0_f64; holes.len()];
        r[k] = 1.0;
        mix_range_with_uniform(&mut r, &holes, &board, 0.25);
        let sum: f64 = r.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "混合后须归一，和={sum}");
        let floor = 0.25 / n_valid as f64;
        assert!(
            (r[k] - (0.75 + floor)).abs() < 1e-12,
            "集中 hole = 0.75+保底"
        );
        for (hi, ok) in valid.iter().enumerate() {
            if *ok {
                assert!(r[hi] >= floor - 1e-15, "合法 hole 须有保底权重");
            } else {
                assert_eq!(r[hi], 0.0, "撞 board 的 hole 混合后须仍 0");
            }
        }

        // λ=0 不动（byte-equal 路径）。
        let mut r0 = vec![0.0_f64; holes.len()];
        r0[k] = 1.0;
        mix_range_with_uniform(&mut r0, &holes, &board, 0.0);
        assert_eq!(r0[k], 1.0);
        assert_eq!(r0.iter().filter(|w| **w > 0.0).count(), 1);

        // 全零（无信号）保持全零——sample_holes_from_ranges 的退均匀兜底语义不变。
        let mut rz = vec![0.0_f64; holes.len()];
        mix_range_with_uniform(&mut rz, &holes, &board, 0.5);
        assert!(rz.iter().all(|w| *w == 0.0), "无信号须保持全零");

        // λ=1（及 >1 clamp）= 精确 uniform。
        for lambda in [1.0, 1.5] {
            let mut r1 = vec![0.0_f64; holes.len()];
            r1[k] = 1.0;
            mix_range_with_uniform(&mut r1, &holes, &board, lambda);
            for (hi, ok) in valid.iter().enumerate() {
                let expect = if *ok { 1.0 / n_valid as f64 } else { 0.0 };
                assert!((r1[hi] - expect).abs() < 1e-15, "λ={lambda} 须精确 uniform");
            }
        }

        // 平滑只作用于对手：hero 座 λ 恒 0（混 hero range = 虚增弃牌率，mix_lambda_for_seat doc）。
        assert_eq!(mix_lambda_for_seat(2, 2, 0.25), 0.0, "hero 座不混");
        assert_eq!(mix_lambda_for_seat(1, 2, 0.25), 0.25, "对手座混 λ");
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

    /// 旧 range 采样实现（逐组合过滤 + 归一 + [`sample_discrete`]，commit `d18c2da` 时点）的
    /// 忠实拷贝——新拒绝采样路径的**分布 oracle** 兼 wall 基线
    /// （[`_measure_range_sampling_wall`]）。
    fn sample_holes_reference(
        template: &GameState,
        ranges: &[Vec<f64>],
        hole_combos: &[[Card; 2]],
        rng: &mut dyn RngSource,
    ) -> Vec<Option<[Card; 2]>> {
        let mut used: BTreeSet<u8> = template.board().iter().map(|c| c.to_u8()).collect();
        let players = template.players();
        let mut out: Vec<Option<[Card; 2]>> = vec![None; players.len()];
        for (seat, player) in players.iter().enumerate() {
            if player.hole_cards.is_none() {
                continue;
            }
            let mut dist: Vec<(usize, f64)> = Vec::new();
            let mut total = 0.0_f64;
            for (hi, hole) in hole_combos.iter().enumerate() {
                let w = ranges[seat].get(hi).copied().unwrap_or(0.0);
                if w > 0.0 && !used.contains(&hole[0].to_u8()) && !used.contains(&hole[1].to_u8()) {
                    dist.push((hi, w));
                    total += w;
                }
            }
            let chosen_idx = if total > 0.0 {
                for e in dist.iter_mut() {
                    e.1 /= total;
                }
                sample_discrete(&dist, rng)
            } else {
                let avail: Vec<usize> = hole_combos
                    .iter()
                    .enumerate()
                    .filter(|(_, h)| !used.contains(&h[0].to_u8()) && !used.contains(&h[1].to_u8()))
                    .map(|(hi, _)| hi)
                    .collect();
                let p = 1.0 / avail.len() as f64;
                let uni: Vec<(usize, f64)> = avail.into_iter().map(|hi| (hi, p)).collect();
                sample_discrete(&uni, rng)
            };
            let hole = hole_combos[chosen_idx];
            used.insert(hole[0].to_u8());
            used.insert(hole[1].to_u8());
            out[seat] = Some(hole);
        }
        out
    }

    /// 新拒绝采样热路径与旧逐组合扫描**同分布**（边际频率 TV 距离 oracle 对照）+ 支撑正确 +
    /// 同 seed 可复现。3-way flop：座 0/1 用**同一份**稀疏加权 range（~58 个正权组合、权重
    /// 1..=16，且**不剔除**撞 board 的正权条目——旧路径靠过滤、新路径靠拒绝，正是要对照的
    /// 两种实现；座间共享组合 = card-removal 强耦合），座 2 全零（退均匀兜底分派）。错的
    /// 二分（off-by-one → 命中零权组合，支撑断言抓）/ 错的掩码 / 漏拒绝 / 权重读错（如退化
    /// 均匀，TV ≈ 0.24 ≫ 阈 0.06）任一条都 fail；TV 在固定 seed 下是确定数（阈含 ~2.4×
    /// 采样噪声余量，不 flaky）。
    #[test]
    fn range_sampling_rejection_matches_exact_scan_distribution() {
        let template = nway_limped_flop_state(3, 10_000, 0x5253_414D_504C_4531); // "RSAMPLE1"
        let holes = all_hole_combos();
        // hole [a<b] → 下标的平面查找表（统计计数用）。
        let mut idx_of = vec![usize::MAX; 52 * 52];
        for (hi, h) in holes.iter().enumerate() {
            idx_of[h[0].to_u8() as usize * 52 + h[1].to_u8() as usize] = hi;
        }

        // 稀疏加权 range：每 23 个组合取 1 个、权重 1..=16 循环。
        let mut r = vec![0.0_f64; 1326];
        for hi in (0..1326).step_by(23) {
            r[hi] = ((hi / 23) % 16 + 1) as f64;
        }
        let ranges_vec = vec![r.clone(), r, vec![0.0_f64; 1326]];

        let (abs, rules) = deep_single_pot();
        let sub = SubgameNlheGame::new_with_ranges(
            stub_table(),
            template.config().clone(),
            abs,
            rules,
            template.clone(),
            0,
            0,
            ranges_vec.clone(),
        );

        let n = 30_000usize;
        let n_seats = template.players().len();
        let mut cnt_new = vec![vec![0u32; 1326]; n_seats];
        let mut cnt_ref = vec![vec![0u32; 1326]; n_seats];
        let board_mask: u64 = template
            .board()
            .iter()
            .fold(0u64, |m, c| m | (1u64 << c.to_u8()));
        // 支撑断言（两套实现同测）：不撞 board / 座间不撞 / 正权座只出正权组合。
        let tally = |out: &[Option<[Card; 2]>], cnt: &mut [Vec<u32>]| {
            let mut used = board_mask;
            for (seat, hole) in out.iter().enumerate() {
                let h = hole.expect("3-way limped flop 全员未弃牌，应都有底牌");
                let mask = (1u64 << h[0].to_u8()) | (1u64 << h[1].to_u8());
                assert_eq!(mask & used, 0, "seat{seat} 采样撞牌（board / 前序座位）");
                used |= mask;
                let hi = idx_of[h[0].to_u8() as usize * 52 + h[1].to_u8() as usize];
                assert_ne!(hi, usize::MAX, "采样组合须在 hole_combos 表内");
                if seat < 2 {
                    assert!(
                        ranges_vec[seat][hi] > 0.0,
                        "seat{seat} 有正权 range，采样组合权重须 > 0（hi={hi}）"
                    );
                }
                cnt[seat][hi] += 1;
            }
        };
        let mut rng_new = ChaCha20Rng::from_seed(0x5253_4E45_5700_0001);
        let mut rng_ref = ChaCha20Rng::from_seed(0x5253_5245_4600_0002);
        for _ in 0..n {
            let o_new = sub.sample_holes_from_ranges(sub.ranges.as_ref().unwrap(), &mut rng_new);
            tally(&o_new, &mut cnt_new);
            let o_ref = sample_holes_reference(&template, &ranges_vec, &holes, &mut rng_ref);
            tally(&o_ref, &mut cnt_ref);
        }
        // 座 0/1（加权热路径）边际 TV；座 2 走与旧代码同一条均匀兜底（sample_discrete），
        // 支撑断言已覆盖、不重复量 TV（1326 桶下 TV 噪声 ~0.12，量了也判不动）。
        for seat in 0..2 {
            let tv: f64 = (0..1326)
                .map(|hi| (cnt_new[seat][hi] as f64 - cnt_ref[seat][hi] as f64).abs())
                .sum::<f64>()
                / (2.0 * n as f64);
            assert!(
                tv < 0.06,
                "seat{seat} 新旧采样边际 TV={tv:.4} 超阈 0.06（分布应一致）"
            );
        }
        // 同 seed 可复现：两条独立 rng 流逐 draw 相等。
        let mut ra = ChaCha20Rng::from_seed(0x5253_4445_5400_0003);
        let mut rb = ChaCha20Rng::from_seed(0x5253_4445_5400_0003);
        for i in 0..50 {
            let a = sub.sample_holes_from_ranges(sub.ranges.as_ref().unwrap(), &mut ra);
            let b = sub.sample_holes_from_ranges(sub.ranges.as_ref().unwrap(), &mut rb);
            assert_eq!(a, b, "同 seed 第 {i} 次采样须逐位一致");
        }
    }

    /// 拒绝重试打满的病态角：座 0 正权质量**全部**在撞 board[0] 的组合上（受限全零）→ 连拒
    /// [`RANGE_REJECTION_RETRY_CAP`] 次后走精确扫描兜底 → 退均匀采可用 hole（旧语义不变，
    /// 采到的组合 range 权重必为 0 = 硬证走的是均匀兜底而非加权热路径）；不 panic、不撞牌、
    /// 同 seed 可复现。
    #[test]
    fn range_sampling_blocked_mass_falls_back_uniform() {
        let template = nway_limped_flop_state(3, 10_000, 0x5253_424C_4F43_4B31); // "RSBLOCK1"
        let holes = all_hole_combos();
        let board0 = template.board()[0].to_u8();
        let board_mask: u64 = template
            .board()
            .iter()
            .fold(0u64, |m, c| m | (1u64 << c.to_u8()));
        // 座 0：全部质量在含 board[0] 的 51 个组合上（受限后全零）。
        let mut r0 = vec![0.0_f64; 1326];
        for (hi, h) in holes.iter().enumerate() {
            if h[0].to_u8() == board0 || h[1].to_u8() == board0 {
                r0[hi] = 1.0;
            }
        }
        // 座 1：非撞 board 均权（验后续座位不被座 0 的兜底打乱）；座 2：全零。
        let r1: Vec<f64> = holes
            .iter()
            .map(|h| {
                let m = (1u64 << h[0].to_u8()) | (1u64 << h[1].to_u8());
                if m & board_mask == 0 {
                    1.0
                } else {
                    0.0
                }
            })
            .collect();
        let ranges_vec = vec![r0.clone(), r1, vec![0.0_f64; 1326]];
        let (abs, rules) = deep_single_pot();
        let sub = SubgameNlheGame::new_with_ranges(
            stub_table(),
            template.config().clone(),
            abs,
            rules,
            template.clone(),
            0,
            0,
            ranges_vec,
        );
        let mut idx_of = vec![usize::MAX; 52 * 52];
        for (hi, h) in holes.iter().enumerate() {
            idx_of[h[0].to_u8() as usize * 52 + h[1].to_u8() as usize] = hi;
        }
        let mut rng = ChaCha20Rng::from_seed(0x5253_424C_4B00_0001);
        for _ in 0..200 {
            let out = sub.sample_holes_from_ranges(sub.ranges.as_ref().unwrap(), &mut rng);
            let mut used = board_mask;
            for (seat, hole) in out.iter().enumerate() {
                let h = hole.expect("全员未弃牌");
                let mask = (1u64 << h[0].to_u8()) | (1u64 << h[1].to_u8());
                assert_eq!(mask & used, 0, "seat{seat} 兜底采样撞牌");
                used |= mask;
            }
            // 座 0 采到的组合在其 range 里权重必为 0（质量全被 board 封死 → 均匀兜底）。
            let h0 = out[0].unwrap();
            let hi0 = idx_of[h0[0].to_u8() as usize * 52 + h0[1].to_u8() as usize];
            assert_eq!(
                r0[hi0], 0.0,
                "座 0 受限全零，采样必来自均匀兜底（采到正权组合 = 拒绝逻辑漏撞牌）"
            );
        }
        let mut ra = ChaCha20Rng::from_seed(0x5253_424C_4B00_0002);
        let mut rb = ChaCha20Rng::from_seed(0x5253_424C_4B00_0002);
        for i in 0..20 {
            let a = sub.sample_holes_from_ranges(sub.ranges.as_ref().unwrap(), &mut ra);
            let b = sub.sample_holes_from_ranges(sub.ranges.as_ref().unwrap(), &mut rb);
            assert_eq!(a, b, "同 seed 第 {i} 次兜底采样须逐位一致");
        }
    }

    /// 诊断（exec §3.2 缺口② range 平滑连带发现「range 加权采样 ~7× 掉速」的修复读数）：
    /// ① `sample_holes_from_ranges` 本体 ns/call——新拒绝采样 vs 旧逐组合扫描
    /// （[`sample_holes_reference`]）；② 端到端 µs/iter——同一 6-way limped flop {1pot} 子树，
    /// uniform root（无 ranges）vs range 加权 root（λ 混合生产形态：非撞 board 组合全正）。
    /// 修复后 ② 两行应基本持平（range 税收掉）。
    #[test]
    #[ignore = "诊断：range 加权采样 wall（新拒绝采样 vs 旧扫描 + 端到端 µs/iter）；--release --ignored --nocapture 跑"]
    fn _measure_range_sampling_wall() {
        let template = nway_limped_flop_state(6, 10_000, 0x5253_5741_4C4C_5F30); // "RSWALL_0"
        let holes = all_hole_combos();
        let board_mask: u64 = template
            .board()
            .iter()
            .fold(0u64, |m, c| m | (1u64 << c.to_u8()));
        // λ 混合后的生产形态：非撞 board 组合全正（近均匀 + 锯齿权重，座间 salt 不同）。
        let mk = |salt: u64| -> Vec<f64> {
            holes
                .iter()
                .enumerate()
                .map(|(hi, h)| {
                    let m = (1u64 << h[0].to_u8()) | (1u64 << h[1].to_u8());
                    if m & board_mask != 0 {
                        0.0
                    } else {
                        1.0 + ((hi as u64 ^ salt) % 7) as f64 / 7.0
                    }
                })
                .collect()
        };
        let ranges_vec: Vec<Vec<f64>> = (0..6u64).map(mk).collect();
        let (abs, rules) = deep_single_pot();
        let sub = SubgameNlheGame::new_with_ranges(
            stub_table(),
            template.config().clone(),
            abs.clone(),
            rules,
            template.clone(),
            0,
            0,
            ranges_vec.clone(),
        );

        // ① 采样本体 ns/call。
        let m_new = 200_000u32;
        let mut rng = ChaCha20Rng::from_seed(0x5253_574E_4557_0001);
        let t0 = Instant::now();
        for _ in 0..m_new {
            std::hint::black_box(
                sub.sample_holes_from_ranges(sub.ranges.as_ref().unwrap(), &mut rng),
            );
        }
        let ns_new = t0.elapsed().as_nanos() as f64 / m_new as f64;
        let m_ref = 20_000u32;
        let mut rng = ChaCha20Rng::from_seed(0x5253_5752_4546_0002);
        let t1 = Instant::now();
        for _ in 0..m_ref {
            std::hint::black_box(sample_holes_reference(
                &template,
                &ranges_vec,
                &holes,
                &mut rng,
            ));
        }
        let ns_ref = t1.elapsed().as_nanos() as f64 / m_ref as f64;
        eprintln!(
            "[range-wall] sample ns/call: new={ns_new:.0} ref={ns_ref:.0} speedup={:.1}x",
            ns_ref / ns_new
        );

        // ② 端到端 µs/iter：uniform root vs range 加权 root（同子树、同迭代数）。
        let iters = 20_000u64;
        let run = |game: SubgameNlheGame, label: &str| {
            let mut tr = EsMccfrTrainer::new(game, 0x5253_5741_4C4C ^ 0xA5A5);
            let mut rng = ChaCha20Rng::from_seed(0x5253_5741_4C4C_0003);
            let t = Instant::now();
            for _ in 0..iters {
                tr.step(&mut rng).expect("probe step");
            }
            let us = t.elapsed().as_micros() as f64 / iters as f64;
            eprintln!("[range-wall] e2e {label}: {us:.2} µs/iter");
            us
        };
        let uniform_game = SubgameNlheGame::new(
            stub_table(),
            template.config().clone(),
            abs,
            rules,
            template.clone(),
            0,
            0,
        );
        let us_uniform = run(uniform_game, "uniform-root");
        let us_ranges = run(sub, "ranges-root ");
        eprintln!(
            "[range-wall] range 税 = {:.2}x（修复前 sweep 实测 ~7x）",
            us_ranges / us_uniform
        );
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
            deep_menu: false,
            live_traversers: false,
            range_uniform_mix: 0.0,
            solve_threads: 1,
        };
        let run = |cfg: &SubgameSearchConfig| {
            subgame_search(
                &auth, &auth, &game, &legal_abs, node_id, &strat, cfg, None, None, 0x9999, 7,
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
            deep_menu: false,
            live_traversers: false,
            range_uniform_mix: 0.0,
            solve_threads: 1,
        };
        let base = subgame_search(
            &auth, &auth, &game, &legal_abs, node_id, &strat, &none_cfg, None, None, 0x55, 4,
        )
        .expect("None 路径应 Ok（stub 桶 0 → root infoset 必累积）");

        // ① 预算 60s 远不绑定（300 迭代 HU flop ~数十 ms）→ 跑满 300 迭代、deadline 从不触发 →
        // 与 None 路径逐项 byte-equal（Instant 只读不入 RNG/trainer，不引入非确定性）。
        let loose_cfg = SubgameSearchConfig {
            time_budget: Some(Duration::from_secs(60)),
            ..none_cfg
        };
        let loose = subgame_search(
            &auth, &auth, &game, &legal_abs, node_id, &strat, &loose_cfg, None, None, 0x55, 4,
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
            &auth, &auth, &game, &legal_abs, node_id, &strat, &tight_cfg, None, None, 0x55, 4,
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

    /// LCFR period 在 time_budget anytime 下不能挂在 iterations 上（advisor 把 iterations
    /// 默认抬到 `u64::MAX` 当安全上界后，iterations/50 = 永不 rescale = LCFR 静默退化
    /// vanilla）。钉 [`lcfr_period`]：budgeted 大迭代档 cap 到 10_000；小迭代档与 None
    /// 公式一致（保 time_budget_anytime_stops_and_is_valid 的 == 契约）。
    #[test]
    fn lcfr_period_capped_under_time_budget() {
        let mut cfg = SubgameSearchConfig {
            iterations: u64::MAX,
            time_budget: Some(Duration::from_secs(5)),
            ..SubgameSearchConfig::default()
        };
        assert_eq!(lcfr_period(&cfg), 10_000, "budgeted + 巨大上界 → cap 10k");
        cfg.iterations = 300;
        assert_eq!(lcfr_period(&cfg), 6, "budgeted 小迭代档 == iterations/50");
        cfg.time_budget = None;
        assert_eq!(lcfr_period(&cfg), 6, "None 路径公式不变");
        cfg.iterations = u64::MAX;
        assert_eq!(
            lcfr_period(&cfg),
            u64::MAX / 50,
            "None 路径不 cap（固定迭代档行为逐位保留）"
        );
    }

    /// advisor budget 模式实配（iterations=`u64::MAX` 安全上界 + lcfr）端到端：求解须在
    /// 预算量级内终止（不挂死在 u64::MAX 循环上）且出合法归一分布。
    #[test]
    fn time_budget_with_unbounded_iterations_terminates() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x5447_4254_5F41_3242);
        let auth = flop.game_state.clone();
        let legal_abs = SimplifiedNlheGame::legal_actions(&flop);
        let node_id = flop.current_node_id;
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let cfg = SubgameSearchConfig {
            iterations: u64::MAX,
            max_subtree_nodes: 1_000_000,
            seed: 0x7B0D_6E7A_5EED_u64,
            use_blueprint_range: false,
            trigger: SearchTrigger::AllPostflop,
            resolve_root: ResolveRoot::RoundStart,
            lcfr: true,
            time_budget: Some(Duration::from_millis(200)),
            ..SubgameSearchConfig::default()
        };
        let t0 = Instant::now();
        let d = subgame_search(
            &auth, &auth, &game, &legal_abs, node_id, &strat, &cfg, None, None, 0x55, 4,
        )
        .expect("200ms 预算足够采样 root infoset → Ok");
        // 终止性：每迭代查一次 deadline，HU flop 单迭代 µs 级 → 总 wall ≈ 预算。10× 余量防慢机。
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "u64::MAX 上界下须由墙钟截断（实测 {:?}）",
            t0.elapsed()
        );
        let sum: f64 = d.iter().map(|(_, p)| *p).sum();
        assert!((sum - 1.0).abs() < 1e-9, "归一，和={sum}");
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

    /// 可行性实测（`--ignored --nocapture`）：deep_menu 宽档 `{0.5,1}` 子树节点数随 **SPR**
    /// 增长——SPR 闸从 4 放宽前先量「深 SPR 宽档会不会撑爆生产 cap 4M（→ live giveup）」。
    /// 各 SPR 用对称 limped flop 反推起始栈（SPR=(stack-100)/(n×100)）。打印 HU/3-way/4-way 在
    /// SPR∈{4,10,20,40,99} 的宽档节点数 + 是否越 4M。
    #[test]
    #[ignore]
    fn _measure_deep_wide_menu_tree_sizes() {
        const CAP: usize = 4_000_000;
        eprintln!("[deep_wide SPR sweep] n_active,SPR,stack,wide{{0.5,1}}_nodes,narrow{{1pot}}_nodes,over_4M");
        for n in [2u8, 3, 4] {
            for spr in [4u64, 10, 20, 40, 99] {
                let stack = spr * (n as u64) * 100 + 100; // SPR=(stack-100)/(n*100)
                let st = nway_limped_flop_state(n, stack, 0xD1_5E_A5_E0 ^ (n as u64) ^ (spr << 8));
                let entrants = live_entrants(&st);
                let nodes = |menu: (StreetActionAbstraction, BettingAbstractionRules)| {
                    SubgameNlheGame::new(
                        stub_table(),
                        st.config().clone(),
                        menu.0,
                        menu.1,
                        st.clone(),
                        entrants,
                        0,
                    )
                    .subtree()
                    .num_nodes()
                };
                let wide = nodes(deep_wide_half_pot());
                let narrow = nodes(deep_single_pot());
                eprintln!(
                    "[deep_wide SPR sweep] {n},{spr},{stack},{wide},{narrow},{}",
                    if wide >= CAP { "OVER" } else { "ok" }
                );
            }
        }
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

    /// 诊断（非门槛，exec §4.1 A 收尾② / §2.2 深码×多人叠加 / §8「下一步②」）：补
    /// [`_measure_subgame_wall_and_convergence`] **刻意避开**的角落——它多人只取中等码深
    /// （4way 100BB / 5way 60BB，注释明写「深码×多人叠加是单独设计，首测不堆满」）。本测把
    /// **深码 × 多人两个难点同时拉满**：N-way（3..6）limped flop × 码深 100..500BB、
    /// `deep_single_pot` {1pot} 解到终局。直接回答②「唯一可能超时 / 建不出来的格子」——每格报
    /// **节点数 + 建树 wall + µs/iter（300-iter vanilla 探针）+ 5s 单线程可达迭代数**，画出
    /// 「深码×多人单机可解前沿」。LCFR wall ≈ vanilla（杠杆是每迭代收敛、非每迭代 wall，姊妹
    /// 函数已定论），故探针只跑 vanilla。
    ///
    /// **收敛深度不在本测范围**：多街到终局大树的 per-infoset L1 收敛是步 B 质量项——姊妹函数已
    /// 定「{1pot} turn @300k 仍 L1≈0.15、default-menu turn 1M 参考都欠收敛」；深码×多人 flop 树
    /// infoset 空间更大、参考更不可能干净收敛，量不出干净 ε。故这里只答「**建不建得出来 + 5s
    /// 塞得进多少迭代**」（可解性 / wall），收敛深度留步 B（exec §4.1）。
    ///
    /// **OOM 防护（不改核心建树器，避 byte-equal 风险）**：`walk` 是 eager DFS 全推进
    /// `Vec<TreeNode>`（~120 B/node），深码×多人树可能几千万节点。每行（固定 n）按栈**升序**扫，
    /// 单格节点数超 `SAFE_NODE_CEIL` 即 `break` 该行更深栈（**不建下一个更大的树**）→ 峰值内存 =
    /// 首个越阈格 ≤ `SAFE_NODE_CEIL` × 单步比，远低于 vultr 物理。越阈点**明确 log**（不静默
    /// 截断；节点数本身即「单机能否建 / 解」的结论——超不出来的角落 = §2.2 单独设计 / 换更强机器，
    /// §6 #4 决策点）。wall 用 `Instant`（仅测量，不入确定性求解路径）。
    /// `cargo test -p poker --lib --release -- --ignored --nocapture _measure_deep_multiway_wall`
    /// （须 --release，wall 才有意义；大树跑较久）。
    #[test]
    #[ignore = "诊断：深码×多人 {1pot} 解到终局 wall（节点/建树/µs-iter/5s-iters）；--release --ignored --nocapture 跑"]
    fn _measure_deep_multiway_wall() {
        // ~120 B/node，越阈即 break（不建更大树）→ 峰值 ≈ 首个越阈格 ≤ CEIL×单步比。CEIL=15M
        // → 越阈格至多 ~37M（~4.5GB @120B/node），远低于 vultr 11GB；{1pot} 栈 1.25× 步进下
        // 节点单步比 ~1.5–2.5×，不会一步跳到 OOM。CEIL 也是「单机可解前沿」读数本身。
        const SAFE_NODE_CEIL: usize = 15_000_000;
        eprintln!("[A2-deepmw] n_seats,stack_bb,nodes,build_ms,us_per_iter,iters_in_5s,over_ceil");
        for n in [3u8, 4, 5, 6] {
            for &stack_bb in &[100u64, 150, 200, 250, 300, 400, 500] {
                let stack = stack_bb * 100; // BB = 100 chips
                let seed = 0x6D77_0000_0000_0000_u64 ^ ((n as u64) << 24) ^ stack_bb;
                let flop = nway_limped_flop_state(n, stack, seed);
                let (abs, rules) = deep_single_pot();
                let tb = Instant::now();
                let game = SubgameNlheGame::new(
                    stub_table(),
                    flop.config().clone(),
                    abs,
                    rules,
                    flop.clone(),
                    0,
                    0,
                );
                let build_ms = tb.elapsed().as_secs_f64() * 1e3;
                let nodes = game.subtree().num_nodes();
                // µs/iter：300-iter vanilla 探针。external sampling 每迭代一条轨迹、µs/iter 主要
                // 随树**深**（路径上决策数）而非总节点数 → 即便大树也可测、且 5s 可达迭代数有意义。
                let mut tr = EsMccfrTrainer::new(game, seed ^ 0xA5A5_0000);
                let mut rng = ChaCha20Rng::from_seed(seed ^ 0xC0FF_EE00);
                let t0 = Instant::now();
                for _ in 0..300u64 {
                    tr.step(&mut rng).expect("probe step");
                }
                let us_per_iter = t0.elapsed().as_micros() as f64 / 300.0;
                let iters_5s = if us_per_iter > 0.0 {
                    (5_000_000.0 / us_per_iter) as u64
                } else {
                    u64::MAX
                };
                let over = nodes > SAFE_NODE_CEIL;
                eprintln!(
                    "[A2-deepmw] {n},{stack_bb},{nodes},{build_ms:.1},{us_per_iter:.3},{iters_5s},{over}"
                );
                if over {
                    eprintln!(
                        "[A2-deepmw] n={n}: {stack_bb}BB nodes={nodes} > SAFE_NODE_CEIL={SAFE_NODE_CEIL} → break 该行更深栈（OOM 防护，不建更大树）"
                    );
                    break;
                }
            }
        }
    }

    /// 诊断（非门槛，exec §4.1 A 收尾① / §8 下一步①）：ε/δ_conv 收敛距离**真阈值**——在
    /// **river/turn 子树**上量 ① per-infoset 平均策略 L1（vs M 参考）+ ② **root EV 差 δ_conv**
    /// （avg-vs-avg MC）。**实测（commit `453c1ba`，真桶）**：river（单街、~1690 infoset、912 节点）
    /// 1M 参考干净收敛 → L1→0.03 / EV 差→0.01 chip @300k = **真 ε≈0.05 / δ_conv≈1 chip**；turn
    /// （多街 + river 跑出 → 真桶 infoset 爆炸到 29.5 万+ 且 300k 迭代仍在涨）**1M 参考欠收敛**、不是
    /// 干净锚（exec §4.1）。故 river 是 ε 锚；大 flop 子树同理欠采样（§4.1，故移出
    /// `_measure_subgame_wall_and_convergence` 的粗 sanity）。
    ///
    /// **两套菜单**：default {0.5,1,2}（对照）+ {1pot}（`deep_single_pot`，生产深码/多人解到终局
    /// 所用，缺口③）——量「A② wall 证『330k 迭代塞进 5s』之外的真问题：多街到终局树在该迭代数内
    /// 收不收敛」。
    ///
    /// **桶表**：默认读 `SUBGAME_CAL_BUCKET_TABLE`（缺省
    /// `artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin`，vultr 已有）的**真
    /// schema-v4 桶表** → postflop per-hand 真桶 → 非退化 ε。打不开（如本机无 artifact）则**退回
    /// stub**（postflop 全归桶 0 = 退化 ε，仅机制）——保证本测试在无 artifact 机器上
    /// （`cargo test --release -- --ignored`）不 panic；`bucket_mode` 列自报 real/stub。
    /// 注意 L1 沿**同一 seed 单轨**（同 seed → ladder@K == 参考前 K 步）量「实时预算 K 迭代离长解
    /// M 还差多少」、即 strategy@K↔@M 位移；ε 读在预算可达的 K 上。真桶 per-hand infoset 数远超
    /// stub（最多 ~500×/街）→ 需远多迭代才收敛，故 ladder 拉到 300..300k、参考 M=1_000_000（不够
    /// 看到 L1 收敛尾则下次调大）。`cargo test -p poker --lib --release -- --ignored --nocapture
    /// _measure_convergence_calibration`（须 --release）。
    #[test]
    #[ignore = "诊断：river/turn 收敛 L1 + root-EV 差（真桶表 ε/δ_conv；无 artifact 退 stub）；--release --ignored 跑"]
    fn _measure_convergence_calibration() {
        // exec §8 下一步①：真桶表 → 非退化 ε；打不开退 stub（无 artifact 机器不 panic）。
        let bucket_path = std::env::var("SUBGAME_CAL_BUCKET_TABLE").unwrap_or_else(|_| {
            "artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin".to_string()
        });
        let opened = BucketTable::open(std::path::Path::new(&bucket_path));
        let (table, bucket_mode): (Arc<BucketTable>, &str) = match opened {
            Ok(t) => (Arc::new(t), "real"),
            Err(e) => {
                eprintln!(
                    "[A1-cal] WARN 打不开真桶表 {bucket_path}: {e:?} → 退 stub（退化 ε，仅机制）"
                );
                (stub_table(), "stub")
            }
        };
        eprintln!("[A1-cal] bucket_mode={bucket_mode} path={bucket_path}");

        const REF_ITERS: u64 = 1_000_000;
        const LADDER: [u64; 7] = [300, 1_000, 3_000, 10_000, 30_000, 100_000, 300_000];
        const EV_ROLLOUTS: usize = 100_000; // MC 噪声 ~ pot/√rollouts；真桶下增 rollouts 压噪

        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        // state 生成 bucket-无关（passive 推进只看 legal_actions / 转移）；真桶只在建 subgame 时作用。
        let turn = hu_state_at(&game, 0x5455_524E_0000_0001, Street::Turn) // "TURN"
            .game_state
            .clone();
        let river = hu_state_at(&game, 0x5249_5645_0000_0001, Street::River) // "RIVE"
            .game_state
            .clone();
        let targets: [(&str, &GameState); 2] =
            [("hu_turn_subtree", &turn), ("hu_river_subtree", &river)];
        // 两套下注菜单：default {0.5,1,2}（对照）+ {1pot}（生产深码/多人解到终局所用，缺口③）。
        // {1pot} 单档收窄分叉 → 直接量「多街到终局树在 5s 预算迭代数内收不收敛」= A② wall
        // 「塞得进」之外的真问题（river=单街已干净收敛，turn=多街+river 跑出 → infoset 爆炸）。
        let menus: [(&str, bool); 2] = [("default", false), ("1pot", true)];

        eprintln!(
            "[A1-cal] bucket_mode,menu,target,nodes,iters,ref,mean_l1,max_l1,ev_short,ev_ref,ev_abs_diff,infosets"
        );
        for (menu_name, one_pot) in menus {
            // 每次现造菜单（StreetActionAbstraction/Rules 非 Copy）→ 无需 Clone、确定性不变。
            let build = |tmpl: &GameState| {
                let (abs, rules) = if one_pot {
                    deep_single_pot()
                } else {
                    (
                        StreetActionAbstraction::default_6_action(),
                        BettingAbstractionRules::default(),
                    )
                };
                SubgameNlheGame::new(
                    table.clone(),
                    tmpl.config().clone(),
                    abs,
                    rules,
                    tmpl.clone(),
                    0,
                    0,
                )
            };
            for (name, tmpl) in targets {
                let nodes = build(tmpl).subtree().num_nodes();
                // root EV 的 traverser = root 决策者（postflop HU = BB 先动），便于解读。
                let root_actor = tmpl.current_player().expect("非终局").0 as PlayerId;
                let reference = {
                    let mut tr = EsMccfrTrainer::new(build(tmpl), 0xCA1B_0000);
                    let mut rng = ChaCha20Rng::from_seed(0xCA1B_0000 ^ 0xC0FF_EE00);
                    for _ in 0..REF_ITERS {
                        tr.step(&mut rng).expect("ref step");
                    }
                    tr
                };
                let ev_ref = mc_root_ev(&reference, root_actor, EV_ROLLOUTS, 0x00E7_0000);
                for &iters in &LADDER {
                    let mut tr = EsMccfrTrainer::new(build(tmpl), 0xCA1B_0000);
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
                    let ev_short = mc_root_ev(&tr, root_actor, EV_ROLLOUTS, 0x00E7_0000);
                    eprintln!(
                        "[A1-cal] {bucket_mode},{menu_name},{name},{nodes},{iters},{REF_ITERS},{:.4},{:.4},{:.2},{:.2},{:.4},{n}",
                        if n > 0 { sum_l1 / n as f64 } else { 0.0 },
                        max_l1,
                        ev_short,
                        ev_ref,
                        (ev_short - ev_ref).abs(),
                    );
                }
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

    // —— 缺口②续：subgame_search_unanchored（node_id 脱离 100BB 影子）——

    /// 6-max nolimp（N=2 redirect、stub 桶）game：unanchored 只用它的桶表 + 抽象/规则，
    /// 不读它的 100BB 全局树（这正是被测性质）。
    fn nolimp_6max_game() -> SimplifiedNlheGame {
        let (a, mut r) = first_small_6max(2);
        r.no_open_limp = true;
        SimplifiedNlheGame::new_with_abstraction(
            stub_table(),
            TableConfig::default_6max_100bb(),
            a,
            r,
        )
        .expect("6max nolimp game")
    }

    /// 构造一个 **off-stack all-in 线**的真栈中途局（blueprint 100BB 树上结构性缺节点的目标
    /// 场景）：UTG 短码 30BB open-shove → HJ/CO/BTN fold → SB raise-over 到 60BB → BB call →
    /// flop（UTG capped all-in，SB/BB live，SB 首个行动、未起注）。100BB 对称树上「raise-over
    /// 全栈 all-in 后还有人活着行动」的节点不存在 → 影子 / 全局树导航必失同步。
    fn offstack_allin_flop_state() -> GameState {
        let mut cfg = TableConfig::default_6max_100bb();
        cfg.starting_stacks[3] = ChipAmount::new(3_000); // UTG 短码 30BB（bb=100）。
        let mut st = GameState::new(&cfg, 0x0FF5_7ACC);
        assert_eq!(
            st.current_player(),
            Some(SeatId(3)),
            "6-max preflop 首行动 UTG"
        );
        st.apply(Action::AllIn).expect("UTG shove 30BB");
        st.apply(Action::Fold).expect("HJ fold");
        st.apply(Action::Fold).expect("CO fold");
        st.apply(Action::Fold).expect("BTN fold");
        st.apply(Action::Raise {
            to: ChipAmount::new(6_000),
        })
        .expect("SB raise-over 短码 all-in 到 60BB（真栈下合法）");
        st.apply(Action::Call).expect("BB call 60BB");
        assert_eq!(
            st.street(),
            Street::Flop,
            "UTG capped、SB/BB matched → flop"
        );
        assert_eq!(st.current_player(), Some(SeatId(1)), "flop 首行动 = SB");
        st
    }

    /// unanchored 在 off-stack all-in 线上可解 + 分布归一 + 同 seed 可复现（核心契约：这条线
    /// 在 100BB 影子上拿不到 node_id，真栈锚是唯一入口）。
    #[test]
    fn unanchored_offstack_allin_line_solves_and_reproducible() {
        let game = nolimp_6max_game();
        let auth = offstack_allin_flop_state();
        let cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        // flop 首决策点：round-start == 当前点，within-round 动作序空。
        let run = || subgame_search_unanchored(&auth, &auth, &game, &[], &cfg, 0xD15C);
        let d1 = run().expect("off-stack all-in 线 unanchored 应可解");
        let sum: f64 = d1.iter().map(|(_, p)| p).sum();
        assert!((sum - 1.0).abs() < 1e-9, "分布应归一，和={sum}");
        assert!(d1.iter().all(|(_, p)| *p > 0.0 && p.is_finite()));
        let d2 = run().expect("再跑一次");
        assert_eq!(
            format!("{d1:?}"),
            format!("{d2:?}"),
            "同 seed unanchored 须可复现"
        );
    }

    /// within-round 真实动作导航：SB check 后 BB 决策——round-start 重解 + [(Check, false)]
    /// 导航到 check 后节点、读 BB 策略（钉 navigate_subtree_by_real_actions 的非空路径）。
    #[test]
    fn unanchored_within_round_real_actions_navigate() {
        let game = nolimp_6max_game();
        let round_start = offstack_allin_flop_state();
        let mut auth = round_start.clone();
        auth.apply(Action::Check).expect("SB check");
        assert_eq!(auth.current_player(), Some(SeatId(2)), "check 后 BB 行动");
        let cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let within = [(Action::Check, false)];
        let d = subgame_search_unanchored(&auth, &round_start, &game, &within, &cfg, 0xD15D)
            .expect("within-round check 导航应成功");
        let sum: f64 = d.iter().map(|(_, p)| p).sum();
        assert!((sum - 1.0).abs() < 1e-9, "分布应归一，和={sum}");
    }

    // —— 脱锚搜索档一：同步前缀 reach（unanchored_range_design §1）——

    /// off-stack all-in 线（[`offstack_allin_flop_state`]）在 100BB 影子上**已同步前缀**：UTG
    /// all-in + HJ/CO/BTN fold（断点 = SB raise-over，影子拿不到）→ synced_node = SB 决策节点。
    /// 返回前缀决策三元组（全 preflop）。供前缀 reach 测试用（与脱锚 root = offstack flop 配对）。
    fn offstack_synced_prefix(
        game: &SimplifiedNlheGame,
    ) -> Vec<(NodeId, AbstractActionTag, PlayerId)> {
        let mut rng = ChaCha20Rng::from_seed(0x4F46_4653_5953_4E43); // "OFFSYSNC"
        let drng: &mut dyn RngSource = &mut rng;
        let mut abs = game.root(drng);
        assert_eq!(
            abs.game_state.current_player(),
            Some(SeatId(3)),
            "UTG 首行动"
        );
        let allin = SimplifiedNlheGame::legal_actions(&abs)
            .into_iter()
            .find(|a| AbstractActionTag::of(a) == AbstractActionTag::AllIn)
            .expect("UTG 根应有 AllIn 档");
        abs = SimplifiedNlheGame::next(abs, allin, drng);
        for _ in 0..3 {
            let fold = SimplifiedNlheGame::legal_actions(&abs)
                .into_iter()
                .find(|a| matches!(a, AbstractAction::Fold))
                .expect("HJ/CO/BTN 应可 fold");
            abs = SimplifiedNlheGame::next(abs, fold, drng);
        }
        assert_eq!(
            abs.game_state.current_player(),
            Some(SeatId(1)),
            "前缀末 = SB 决策点（断点前）"
        );
        let prefix = synced_prefix_decisions(game, abs.current_node_id);
        assert!(
            !prefix.is_empty()
                && prefix
                    .iter()
                    .all(|(nid, _, _)| game.tree().node(*nid).street == StreetTag::Preflop),
            "off-stack 前缀须非空且全 preflop"
        );
        prefix
    }

    /// 档一：前缀**为空**（如 limp 池首动作即失同步 → synced_node = root → 无当前街之前的
    /// 决策）→ 脱锚搜索退 uniform 路径，与 `prefix_reach = None` **byte-equal**（不走
    /// `new_with_ranges` 的均匀向量——那是不同采样路径、不 byte-equal）。
    #[test]
    fn unanchored_prefix_reach_empty_is_uniform_byte_equal() {
        let game = nolimp_6max_game();
        let auth = offstack_allin_flop_state();
        let cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let empty: Vec<(NodeId, AbstractActionTag, PlayerId)> = Vec::new();
        let prefix = PrefixReach {
            strategy: &strat,
            decisions: &empty,
        };
        let with_empty = subgame_search_unanchored_cached(
            None,
            &auth,
            &auth,
            &game,
            &[],
            &cfg,
            None,
            Some(prefix),
            0xE111,
        )
        .expect("空前缀应 Ok");
        let uniform = subgame_search_unanchored(&auth, &auth, &game, &[], &cfg, 0xE111)
            .expect("uniform 应 Ok");
        assert_eq!(
            format!("{with_empty:?}"),
            format!("{uniform:?}"),
            "空前缀须与 uniform 路径 byte-equal"
        );
    }

    /// 档一：前缀**非空**（off-stack 已同步前缀 = UTG all-in + 3 fold，全 preflop）→ 算出的
    /// reach 进 solve 缓存 key → 与 uniform（`prefix_reach = None`）不同 key → 同一 cache 必 miss
    /// （开/关前缀 reach 不串均衡，验收 ③）。stub 桶下 ranges 本身 ≈ uniform，但 Some(ranges)≠None
    /// 的 key 差异即证前缀真进 key；解归一 + 同 seed 可复现。
    #[test]
    fn unanchored_prefix_reach_ranges_enter_cache_key() {
        let game = nolimp_6max_game();
        let auth = offstack_allin_flop_state();
        let cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let decisions = offstack_synced_prefix(&game);
        let mut cache = SubgameSolveCache::new();
        // uniform（prefix=None）→ miss + store。
        subgame_search_unanchored_cached(
            Some(&mut cache),
            &auth,
            &auth,
            &game,
            &[],
            &cfg,
            None,
            None,
            0xE333,
        )
        .expect("uniform 应 Ok");
        // prefix reach（同 cache）→ ranges 进 key → 必 miss（不复用 uniform 解）。
        let d_pre = subgame_search_unanchored_cached(
            Some(&mut cache),
            &auth,
            &auth,
            &game,
            &[],
            &cfg,
            None,
            Some(PrefixReach {
                strategy: &strat,
                decisions: &decisions,
            }),
            0xE333,
        )
        .expect("prefix reach 应 Ok");
        assert_eq!(
            (cache.misses(), cache.hits()),
            (2, 0),
            "开/关前缀 reach → ranges 进 key → 必 miss（不串均衡）"
        );
        let sum: f64 = d_pre.iter().map(|(_, p)| p).sum();
        assert!((sum - 1.0).abs() < 1e-9, "前缀 reach 分布须归一，和={sum}");
        // 同 seed 可复现（无缓存现解）。
        let d_pre2 = subgame_search_unanchored_cached(
            None,
            &auth,
            &auth,
            &game,
            &[],
            &cfg,
            None,
            Some(PrefixReach {
                strategy: &strat,
                decisions: &decisions,
            }),
            0xE333,
        )
        .expect("再跑一次");
        assert_eq!(
            format!("{d_pre:?}"),
            format!("{d_pre2:?}"),
            "前缀 reach 同 seed 须可复现"
        );
    }

    /// 档一核心机制（[`estimate_range`] `skip_all_in`）：①AllIn-tag 决策按因子 1 跳过
    /// （skip=true ≡ 删该决策）；②无 AllIn 的座 skip 不影响；③非 AllIn 决策（Raise）→ range
    /// 非 uniform、AllIn 被处理（skip=false）时 range 非 uniform。①②是任意桶都成立的结构等式；
    /// ③的「非 uniform」需**真桶表**（stub 全归桶 0 → 任何 σ 都让 range uniform，机制不可观测）：
    /// 默认读 `SUBGAME_CAL_BUCKET_TABLE`（缺省 vultr 已有的 schema-v4 表），打不开退 stub
    /// （仅验①②，③跳过）。`bucket_mode` 自报。
    #[test]
    fn unanchored_prefix_reach_skips_all_in_tag() {
        let bucket_path = std::env::var("SUBGAME_CAL_BUCKET_TABLE").unwrap_or_else(|_| {
            "artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin".to_string()
        });
        let (table, is_real): (Arc<BucketTable>, bool) = match BucketTable::open(
            std::path::Path::new(&bucket_path),
        ) {
            Ok(t) => (Arc::new(t), true),
            Err(e) => {
                eprintln!("[prefix-reach] WARN 打不开真桶表 {bucket_path}: {e:?} → 退 stub（仅验结构等式）");
                (stub_table(), false)
            }
        };
        eprintln!(
            "[prefix-reach] bucket_mode={}",
            if is_real { "real" } else { "stub" }
        );
        let (a, mut r) = first_small_6max(2);
        r.no_open_limp = true;
        let game = SimplifiedNlheGame::new_with_abstraction(
            table,
            TableConfig::default_6max_100bb(),
            a,
            r,
        )
        .expect("6max game");
        // 影子前缀：UTG(3) raise（非 all-in）→ HJ(4) all-in（断点无关，这里只取前缀决策）。
        let mut rng = ChaCha20Rng::from_seed(0x534B_4950_414C_4C49); // "SKIPALLI"
        let drng: &mut dyn RngSource = &mut rng;
        let mut abs = game.root(drng);
        let utg = abs.game_state.current_player().expect("decision").0 as PlayerId;
        let raise = SimplifiedNlheGame::legal_actions(&abs)
            .into_iter()
            .find(|x| matches!(AbstractActionTag::of(x), AbstractActionTag::Raise(_)))
            .expect("UTG 根应有 Raise 档");
        abs = SimplifiedNlheGame::next(abs, raise, drng);
        let hj = abs.game_state.current_player().expect("decision").0 as PlayerId;
        let allin = SimplifiedNlheGame::legal_actions(&abs)
            .into_iter()
            .find(|x| AbstractActionTag::of(x) == AbstractActionTag::AllIn)
            .expect("HJ 应有 AllIn 档");
        abs = SimplifiedNlheGame::next(abs, allin, drng);
        let decisions = synced_prefix_decisions(&game, abs.current_node_id);
        // 非均匀 σ（按 info.raw 偏斜，确定性）→ 真桶下 reach 非 uniform。
        let strat = |i: &InfoSetId, n: usize| {
            let mut v: Vec<f64> = (0..n)
                .map(|k| 1.0 + k as f64 + (i.raw() % 7) as f64 * 0.1)
                .collect();
            let s: f64 = v.iter().sum();
            v.iter_mut().for_each(|x| *x /= s);
            v
        };
        let holes = all_hole_combos();
        let board: Vec<Card> = Vec::new();
        let empty: Vec<(NodeId, AbstractActionTag, PlayerId)> = Vec::new();

        // ① UTG（只有 Raise，无 AllIn）→ skip 不影响（任意桶）。
        let utg_skip = estimate_range(&game, &strat, &decisions, &board, utg, &holes, true);
        let utg_noskip = estimate_range(&game, &strat, &decisions, &board, utg, &holes, false);
        assert_eq!(utg_skip, utg_noskip, "UTG 无 AllIn-tag → skip 不影响");
        // ② HJ（只有 AllIn）→ skip=true ≡ 删该决策（= 空决策，任意桶）。
        let hj_skip = estimate_range(&game, &strat, &decisions, &board, hj, &holes, true);
        let hj_empty = estimate_range(&game, &strat, &empty, &board, hj, &holes, false);
        assert_eq!(
            hj_skip, hj_empty,
            "skip=true 跳 AllIn-tag ≡ 删该决策（因子 1）"
        );

        if is_real {
            // ③ 真桶：AllIn 被处理（skip=false）→ reach 非 uniform → 与 skip=true(uniform) 不同。
            let hj_noskip = estimate_range(&game, &strat, &decisions, &board, hj, &holes, false);
            assert_ne!(
                hj_noskip, hj_skip,
                "真桶：skip=false 处理 AllIn → 非 uniform，与 skip=true 不同"
            );
            // 非 AllIn 决策（Raise）→ UTG range 非 uniform。
            let w0 = utg_skip.iter().copied().find(|w| *w > 0.0).expect("有正权");
            assert!(
                utg_skip.iter().any(|w| *w > 0.0 && (*w - w0).abs() > 1e-12),
                "真桶：Raise 条件化 → UTG range 非 uniform"
            );
        }
    }

    /// 不支持的配置显式 `Err`（不静默择一）：depth_limit（要 blueprint 树锚）/ CurrentDecision
    /// （影子锚 A/B 旧模式）/ preflop（走 blueprint，§1 gating）。
    #[test]
    fn unanchored_rejects_unsupported_configs() {
        let game = nolimp_6max_game();
        let auth = offstack_allin_flop_state();
        let dl = SubgameSearchConfig {
            depth_limit: true,
            ..SubgameSearchConfig::default()
        };
        assert!(subgame_search_unanchored(&auth, &auth, &game, &[], &dl, 1).is_err());
        let cd = SubgameSearchConfig {
            resolve_root: ResolveRoot::CurrentDecision,
            ..SubgameSearchConfig::default()
        };
        assert!(subgame_search_unanchored(&auth, &auth, &game, &[], &cd, 1).is_err());
        let pre = GameState::new(&TableConfig::default_6max_100bb(), 7);
        assert!(subgame_search_unanchored(
            &pre,
            &pre,
            &game,
            &[],
            &SubgameSearchConfig::default(),
            1
        )
        .is_err());
    }

    // —— within-round solve 缓存（§6 #2「每轮恰好一个 solve」）——

    /// 同手同街（RoundStart + round-stable seed）两个决策点共享缓存：第二决策命中（不重解，
    /// hits/misses 计数硬证）、命中输出与无缓存从头重解 byte-equal（固定迭代确定性 → 缓存只省
    /// wall 不改结果）；换 hand_seed / 换 iterations / 换 root 几何 → key 变 → miss
    /// （漏 key 输入 = 读错均衡，这里钉关键输入都进了 key）。
    #[test]
    fn solve_cache_within_round_hit_and_key_sensitivity() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x5343_4143_4845_3031); // "SCACHE01"
        let round_start = flop.game_state.clone();
        // 决策 1 = 轮起点首决策（within 空）；决策 2 = 首行动者 Bet 后的 mid-round 决策点。
        let mut rng = ChaCha20Rng::from_seed(0x5343_4143_4845_0002);
        let drng: &mut dyn RngSource = &mut rng;
        let bet = SimplifiedNlheGame::legal_actions(&flop)
            .into_iter()
            .find(|a| matches!(AbstractActionTag::of(a), AbstractActionTag::Bet(_)))
            .expect("flop 首决策点应有 Bet 档");
        let mid = SimplifiedNlheGame::next(flop.clone(), bet, drng);
        let cfg = SubgameSearchConfig {
            iterations: 400,
            max_subtree_nodes: 1_000_000,
            trigger: SearchTrigger::AllPostflop,
            ..SubgameSearchConfig::default()
        };
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];

        let mut cache = SubgameSolveCache::new();
        let d1 = subgame_search_cached(
            Some(&mut cache),
            &flop.game_state,
            &round_start,
            &game,
            &SimplifiedNlheGame::legal_actions(&flop),
            flop.current_node_id,
            &strat,
            &cfg,
            None,
            None,
            None,
            0xABCD,
            0,
        )
        .expect("决策 1 应 Ok");
        assert!(!d1.is_empty());
        assert_eq!(
            (cache.misses(), cache.hits()),
            (1, 0),
            "决策 1 = 首 solve（miss）"
        );

        let d2 = subgame_search_cached(
            Some(&mut cache),
            &mid.game_state,
            &round_start,
            &game,
            &SimplifiedNlheGame::legal_actions(&mid),
            mid.current_node_id,
            &strat,
            &cfg,
            None,
            None,
            None,
            0xABCD,
            7,
        )
        .expect("决策 2（mid-round）应 Ok");
        assert_eq!(
            (cache.misses(), cache.hits()),
            (1, 1),
            "同手同街第二决策须命中、不重解"
        );
        // 命中输出 byte-equal 无缓存从头重解（同 solve 输入确定性）。
        let d2_fresh = subgame_search(
            &mid.game_state,
            &round_start,
            &game,
            &SimplifiedNlheGame::legal_actions(&mid),
            mid.current_node_id,
            &strat,
            &cfg,
            None,
            None,
            0xABCD,
            7,
        )
        .expect("无缓存重解应 Ok");
        assert_eq!(
            format!("{d2:?}"),
            format!("{d2_fresh:?}"),
            "命中输出须 byte-equal 从头重解"
        );

        // key 敏感性（每次换一个 solve 输入 → 必 miss）：
        // (a) hand_seed（换手）。
        subgame_search_cached(
            Some(&mut cache),
            &mid.game_state,
            &round_start,
            &game,
            &SimplifiedNlheGame::legal_actions(&mid),
            mid.current_node_id,
            &strat,
            &cfg,
            None,
            None,
            None,
            0xABCE,
            7,
        )
        .expect("换 hand_seed 仍 Ok");
        assert_eq!(cache.misses(), 2, "hand_seed 变 → miss");
        // (b) cfg.iterations。
        let cfg_more = SubgameSearchConfig {
            iterations: 500,
            ..cfg
        };
        subgame_search_cached(
            Some(&mut cache),
            &mid.game_state,
            &round_start,
            &game,
            &SimplifiedNlheGame::legal_actions(&mid),
            mid.current_node_id,
            &strat,
            &cfg_more,
            None,
            None,
            None,
            0xABCE,
            7,
        )
        .expect("换 iterations 仍 Ok");
        assert_eq!(cache.misses(), 3, "iterations 变 → miss");
        // (c) root 几何（另一手牌局 → board / 底牌 / 状态全变）。
        let flop2 = hu_flop_state(&game, 0x5343_4143_4845_3032); // "SCACHE02"
        let rs2 = flop2.game_state.clone();
        subgame_search_cached(
            Some(&mut cache),
            &flop2.game_state,
            &rs2,
            &game,
            &SimplifiedNlheGame::legal_actions(&flop2),
            flop2.current_node_id,
            &strat,
            &cfg_more,
            None,
            None,
            None,
            0xABCE,
            0,
        )
        .expect("换 root 几何仍 Ok");
        assert_eq!(cache.misses(), 4, "root 几何变 → miss");
        // (d) cfg.range_uniform_mix（range 先验平滑 λ；stub 桶 + uniform σ 下混合后 reach 向量
        // 可能数值不变，key 须经 cfg 字段哈希仍区分——串读 = 读错均衡）。
        let cfg_mix = SubgameSearchConfig {
            range_uniform_mix: 0.25,
            ..cfg_more
        };
        subgame_search_cached(
            Some(&mut cache),
            &flop2.game_state,
            &rs2,
            &game,
            &SimplifiedNlheGame::legal_actions(&flop2),
            flop2.current_node_id,
            &strat,
            &cfg_mix,
            None,
            None,
            None,
            0xABCE,
            0,
        )
        .expect("换 range_uniform_mix 仍 Ok");
        assert_eq!(cache.misses(), 5, "range_uniform_mix 变 → miss");
        // (e) cfg.solve_threads（并行档 rng 流 + stale-σ 语义不同 → 不同均衡，串读 = 读错均衡）。
        let cfg_par = SubgameSearchConfig {
            solve_threads: 2,
            ..cfg_mix
        };
        subgame_search_cached(
            Some(&mut cache),
            &flop2.game_state,
            &rs2,
            &game,
            &SimplifiedNlheGame::legal_actions(&flop2),
            flop2.current_node_id,
            &strat,
            &cfg_par,
            None,
            None,
            None,
            0xABCE,
            0,
        )
        .expect("换 solve_threads 仍 Ok");
        assert_eq!(cache.misses(), 6, "solve_threads 变 → miss");
    }

    /// 限时杠杆③（[`SubgameSearchConfig::solve_threads`]）：`>1` 走 `step_parallel` 并行
    /// solve。钉三件事：①固定迭代 + 固定 seed 下并行档自身**确定性可复现**（批调度纯函数 +
    /// delta 按 tid 序合并 → m1==m2 契约对并行档继续成立）；②输出分布契约不破（归一 / 正
    /// 概率 / 动作在 legal_abs 内）；③与单线程档**确实分流**（rng 流不同 → 分布不同位；若
    /// 两档 byte-equal，说明并行分支是死代码——trip-wire）。
    #[test]
    fn solve_threads_parallel_reproducible_and_valid() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x5354_4852_4431_5054); // "STHRD1PT"
        let auth = flop.game_state.clone();
        let legal_abs = SimplifiedNlheGame::legal_actions(&flop);
        let base = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            trigger: SearchTrigger::AllPostflop,
            ..SubgameSearchConfig::default()
        };
        // iterations=300 不是 threads×batch=2×16 的整数倍（300 = 9×32 + 12）→ 同时覆盖
        // 整批路径 + 「remaining < 整批退单线程 step 收尾」的尾段路径。
        let par = SubgameSearchConfig {
            solve_threads: 2,
            ..base
        };
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let run = |cfg: &SubgameSearchConfig| {
            subgame_search(
                &auth,
                &auth,
                &game,
                &legal_abs,
                flop.current_node_id,
                &strat,
                cfg,
                None,
                None,
                0x7777,
                3,
            )
            .expect("stub 桶 0 → root infoset 必累积 → Ok")
        };
        // ① 并行档 m1==m2。
        let p1 = run(&par);
        let p2 = run(&par);
        assert_eq!(p1.len(), p2.len(), "并行档可复现：维度一致");
        for ((a1, q1), (a2, q2)) in p1.iter().zip(&p2) {
            assert_eq!(a1, a2, "并行档可复现：动作逐项一致");
            assert_eq!(q1.to_bits(), q2.to_bits(), "并行档可复现：概率 byte-equal");
        }
        // ② 分布契约。
        let sum: f64 = p1.iter().map(|(_, p)| *p).sum();
        assert!((sum - 1.0).abs() < 1e-9, "并行档分布须归一，和={sum}");
        for (a, p) in &p1 {
            assert!(*p > 0.0, "只返回正概率动作");
            let tag = AbstractActionTag::of(a);
            assert!(
                legal_abs.iter().any(|l| AbstractActionTag::of(l) == tag),
                "返回动作 {tag:?} 须在 legal_abs 内"
            );
        }
        // ③ 与单线程档分流（死代码 trip-wire；两边都确定性 → 本断言不引入 flake）。
        let s1 = run(&base);
        let bitwise_equal = s1.len() == p1.len()
            && s1
                .iter()
                .zip(&p1)
                .all(|((a1, q1), (a2, q2))| a1 == a2 && q1.to_bits() == q2.to_bits());
        assert!(
            !bitwise_equal,
            "solve_threads=2 与 =1 输出逐位相同：并行分支疑似未生效"
        );
    }

    /// 限时杠杆③吞吐实测（vultr 4-core 跑：`cargo test --release -- --ignored \
    /// _measure_solve_threads_throughput --nocapture`）：同 time_budget 下 solve_threads=1/2/4
    /// 的 update 数（经 [`SubgameSolveCache::entry_update_count`] 读数）。
    ///
    /// **桶表两档**：stub（postflop 全归桶 0 → infoset 极少 = delta 合并密度被人为放大，
    /// 并行 scaling 的**悲观下界**）+ 真 schema-v4 表（`SUBGAME_CAL_BUCKET_TABLE`，缺省
    /// `_measure_convergence_calibration` 同款 500/500/500，vultr 已有；per-hand 真桶 →
    /// 每 update 计算更重 / 合并占比更小 = 接近生产）。两档都打（真表打不开只打 stub 档），
    /// 只打印不断言比值（吞吐受机器负载影响，结论留给读数）。
    ///
    /// **实测（2026-06-12 vultr，4 vCPU = 2 物理核 ×SMT2）**：真表 1s 预算 update
    /// 3.8k→4.8k→7.3k/s @1/2/4t = **×1.92@4t ≈ 该机 SMT 上限**（2 物理核 ×~1.2 SMT 增益
    /// ≈ ×2.4 理论顶）；stub 档 ×1.2@4t（2t 倒挂 ≈1.0，合并密度放大实锤悲观下界）。物理核
    /// 多的部署机按 `step_parallel` 在 blueprint 训练已验证的曲线涨（c6a.8xlarge 32t 8–10×）。
    #[test]
    #[ignore]
    fn _measure_solve_threads_throughput() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x5354_4852_4D45_4153); // "STHRMEAS"
        let auth = flop.game_state.clone();
        let legal_abs = SimplifiedNlheGame::legal_actions(&flop);
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let bucket_path = std::env::var("SUBGAME_CAL_BUCKET_TABLE").unwrap_or_else(|_| {
            "artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin".to_string()
        });
        let real: Option<Arc<BucketTable>> =
            match BucketTable::open(std::path::Path::new(&bucket_path)) {
                Ok(t) => Some(Arc::new(t)),
                Err(e) => {
                    eprintln!("[measure] WARN 打不开真桶表 {bucket_path}: {e:?} → 只打 stub 档");
                    None
                }
            };
        let modes: Vec<(&str, Option<&Arc<BucketTable>>)> = match &real {
            Some(t) => vec![("stub", None), ("real500", Some(t))],
            None => vec![("stub", None)],
        };
        for (mode, table_override) in modes {
            for threads in [1usize, 2, 4] {
                let cfg = SubgameSearchConfig {
                    iterations: u64::MAX,
                    max_subtree_nodes: 1_000_000,
                    trigger: SearchTrigger::AllPostflop,
                    time_budget: Some(Duration::from_millis(1000)),
                    solve_threads: threads,
                    ..SubgameSearchConfig::default()
                };
                let mut cache = SubgameSolveCache::new();
                let t0 = Instant::now();
                subgame_search_cached(
                    Some(&mut cache),
                    &auth,
                    &auth,
                    &game,
                    &legal_abs,
                    flop.current_node_id,
                    &strat,
                    &cfg,
                    table_override,
                    None,
                    None,
                    0x1234,
                    3,
                )
                .expect("预算 1s 充裕应 Ok");
                let wall = t0.elapsed();
                let updates = cache.entry_update_count().expect("solve 已入缓存");
                println!(
                    "[measure] bucket={mode} solve_threads={threads}: updates={updates} \
                     wall={wall:?} ({:.1}k updates/s)",
                    updates as f64 / wall.as_secs_f64() / 1e3
                );
            }
        }
    }

    /// time_budget（墙钟 anytime）下的 within-round 一致性：anytime 迭代数随机器负载变、原理上
    /// 不可 byte-equal（§2.3）——同街第二次**重解**可能停在不同迭代数 = 两次决策读不同均衡。
    /// 缓存命中 = 直接复用第一次的 solve（hits 计数硬证没有第二次 solve）→「每轮恰好一个
    /// solve」恢复；同节点再读逐位相同。
    #[test]
    fn solve_cache_time_budget_one_solve_per_round() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x5343_4143_4845_3033); // "SCACHE03"
        let round_start = flop.game_state.clone();
        let cfg = SubgameSearchConfig {
            iterations: 2000,
            max_subtree_nodes: 1_000_000,
            trigger: SearchTrigger::AllPostflop,
            time_budget: Some(Duration::from_millis(50)),
            ..SubgameSearchConfig::default()
        };
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let legal = SimplifiedNlheGame::legal_actions(&flop);
        let mut cache = SubgameSolveCache::new();
        let run = |cache: &mut SubgameSolveCache| {
            subgame_search_cached(
                Some(cache),
                &flop.game_state,
                &round_start,
                &game,
                &legal,
                flop.current_node_id,
                &strat,
                &cfg,
                None,
                None,
                None,
                0xB07,
                0,
            )
            .expect("time_budget 决策应 Ok")
        };
        let d1 = run(&mut cache);
        let d1b = run(&mut cache);
        assert_eq!(
            (cache.misses(), cache.hits()),
            (1, 1),
            "time_budget 下同街第二决策须命中（每轮恰好一个 solve）"
        );
        for ((a1, p1), (a1b, p1b)) in d1.iter().zip(&d1b) {
            assert_eq!(a1, a1b);
            assert_eq!(p1.to_bits(), p1b.to_bits(), "命中 = 读同一均衡（逐位相同）");
        }
    }

    /// unanchored 路径同样吃缓存：同手同街 mid-round 命中（导航用 solve 时存的同一份 sub_abs）、
    /// 命中输出 byte-equal 无缓存版；换 hand_seed → miss。
    #[test]
    fn solve_cache_unanchored_within_round_hit() {
        let game = nolimp_6max_game();
        let round_start = offstack_allin_flop_state();
        let mut auth = round_start.clone();
        auth.apply(Action::Check).expect("SB check");
        let cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let mut cache = SubgameSolveCache::new();
        let d1 = subgame_search_unanchored_cached(
            Some(&mut cache),
            &round_start,
            &round_start,
            &game,
            &[],
            &cfg,
            None,
            None,
            0xD15E,
        )
        .expect("首决策应 Ok");
        assert!(!d1.is_empty());
        let within = [(Action::Check, false)];
        let d2 = subgame_search_unanchored_cached(
            Some(&mut cache),
            &auth,
            &round_start,
            &game,
            &within,
            &cfg,
            None,
            None,
            0xD15E,
        )
        .expect("mid-round 应 Ok");
        assert_eq!(
            (cache.misses(), cache.hits()),
            (1, 1),
            "unanchored 同手同街第二决策须命中"
        );
        let d2_fresh = subgame_search_unanchored(&auth, &round_start, &game, &within, &cfg, 0xD15E)
            .expect("无缓存重解应 Ok");
        assert_eq!(
            format!("{d2:?}"),
            format!("{d2_fresh:?}"),
            "命中输出须 byte-equal 从头重解"
        );
        // 换 hand_seed → miss（key 覆盖 master seed 输入）。
        subgame_search_unanchored_cached(
            Some(&mut cache),
            &round_start,
            &round_start,
            &game,
            &[],
            &cfg,
            None,
            None,
            0xD15F,
        )
        .expect("换 hand_seed 仍 Ok");
        assert_eq!(cache.misses(), 2, "hand_seed 变 → miss");
    }

    // —— RoundStart 预热：hero 行动前把 solve 提前算进缓存（build+solve wall 藏进对手行动时间）——

    /// 锚定预热：街起点（hero 行动**前**、首行动者 ≠ hero）预热 → hero 首决策 key 命中、不重解；
    /// 命中输出 byte-equal 无缓存现解。`range_uniform_mix=0.25`（生产默认档）+ **非均匀** σ 钉
    /// actor_override 修复：预热时 current_player = 街首行动者，若 range 平滑的「不混」座位按它
    /// 算（而非 hero），ranges 向量不同 → key 必 miss——回归即命中断言 fail。（均匀 σ 下 reach
    /// 本就均匀、混不混向量相同，钉不住，故 σ 必须非均匀。）
    #[test]
    fn prewarm_anchored_first_decision_hits() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x5052_4557_4D31_5F41); // "PREWM1_A"
        let round_start = flop.game_state.clone();
        let first_actor = round_start.current_player().expect("decision").0 as PlayerId;
        // 首行动者 Check → hero（第二行动座）的首决策点。
        let mut rng = ChaCha20Rng::from_seed(0x5052_4557_4D31_5F42);
        let drng: &mut dyn RngSource = &mut rng;
        let check = SimplifiedNlheGame::legal_actions(&flop)
            .into_iter()
            .find(|a| matches!(a, AbstractAction::Check))
            .expect("flop 首决策点应可 Check");
        let mid = SimplifiedNlheGame::next(flop.clone(), check, drng);
        let hero = mid.game_state.current_player().expect("decision").0 as PlayerId;
        assert_ne!(hero, first_actor, "测试前置：hero 须是第二行动座");
        let cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            trigger: SearchTrigger::AllPostflop,
            range_uniform_mix: 0.25,
            ..SubgameSearchConfig::default()
        };
        // 非均匀 σ（按 info 偏斜，确定性）→ reach 非均匀 → mix 的「不混」座位可分辨。
        let strat = |i: &InfoSetId, n: usize| {
            let mut v: Vec<f64> = (0..n)
                .map(|k| 1.0 + k as f64 + (i.raw() % 7) as f64 * 0.1)
                .collect();
            let s: f64 = v.iter().sum();
            v.iter_mut().for_each(|x| *x /= s);
            v
        };
        let mut cache = SubgameSolveCache::new();
        subgame_search_prewarm(
            &mut cache,
            hero,
            &round_start,
            &game,
            flop.current_node_id,
            &strat,
            &cfg,
            None,
            0xFEED,
        )
        .expect("预热应 Ok（stub 桶 + accepting cap）");
        assert_eq!(
            (cache.misses(), cache.hits()),
            (1, 0),
            "预热 = 首 solve（miss + store）"
        );

        let legal_mid = SimplifiedNlheGame::legal_actions(&mid);
        let d = subgame_search_cached(
            Some(&mut cache),
            &mid.game_state,
            &round_start,
            &game,
            &legal_mid,
            mid.current_node_id,
            &strat,
            &cfg,
            None,
            None,
            None,
            0xFEED,
            5,
        )
        .expect("hero 首决策应 Ok");
        assert_eq!(
            (cache.misses(), cache.hits()),
            (1, 1),
            "hero 首决策须命中预热 solve——actor_override 回归时此处 fail"
        );
        // 命中输出 byte-equal 无缓存现解（预热只省 wall、不改读数）。
        let d_fresh = subgame_search(
            &mid.game_state,
            &round_start,
            &game,
            &legal_mid,
            mid.current_node_id,
            &strat,
            &cfg,
            None,
            None,
            0xFEED,
            5,
        )
        .expect("无缓存现解应 Ok");
        assert_eq!(
            format!("{d:?}"),
            format!("{d_fresh:?}"),
            "命中输出须 byte-equal 现解"
        );

        // 配置守卫：CurrentDecision / depth_limit → 早 Err（无处预热）。
        let cd = SubgameSearchConfig {
            resolve_root: ResolveRoot::CurrentDecision,
            ..cfg
        };
        assert!(subgame_search_prewarm(
            &mut cache,
            hero,
            &round_start,
            &game,
            flop.current_node_id,
            &strat,
            &cd,
            None,
            0xFEED,
        )
        .is_err());
        let dl = SubgameSearchConfig {
            depth_limit: true,
            ..cfg
        };
        assert!(subgame_search_prewarm(
            &mut cache,
            hero,
            &round_start,
            &game,
            flop.current_node_id,
            &strat,
            &dl,
            None,
            0xFEED,
        )
        .is_err());
    }

    /// 脱影子预热（uniform 先验，prefix_reach=None → hero 不被读）：预热 → mid-round 首决策命中、
    /// 输出 byte-equal 无缓存现解。
    #[test]
    fn prewarm_unanchored_first_decision_hits() {
        let game = nolimp_6max_game();
        let round_start = offstack_allin_flop_state();
        let mut auth = round_start.clone();
        auth.apply(Action::Check).expect("SB check");
        let cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let hero = round_start.current_player().expect("decision").0 as PlayerId;
        let mut cache = SubgameSolveCache::new();
        subgame_search_unanchored_prewarm(
            &mut cache,
            hero,
            &round_start,
            &game,
            None,
            &cfg,
            None,
            0xD15E,
        )
        .expect("预热应 Ok");
        assert_eq!((cache.misses(), cache.hits()), (1, 0));
        let within = [(Action::Check, false)];
        let d = subgame_search_unanchored_cached(
            Some(&mut cache),
            &auth,
            &round_start,
            &game,
            &within,
            &cfg,
            None,
            None,
            0xD15E,
        )
        .expect("首决策应 Ok");
        assert_eq!(
            (cache.misses(), cache.hits()),
            (1, 1),
            "首决策须命中预热 solve"
        );
        let d_fresh = subgame_search_unanchored(&auth, &round_start, &game, &within, &cfg, 0xD15E)
            .expect("无缓存现解应 Ok");
        assert_eq!(
            format!("{d:?}"),
            format!("{d_fresh:?}"),
            "命中输出须 byte-equal 现解"
        );
    }

    // —— bucket_override：子树 solve 独立换桶表（搜索区分辨率与 blueprint 表解耦）——

    /// anchored 路径 bucket_override 三件套：① `Some(blueprint 自身表)` ≡ `None`（解析到同一
    /// Arc → 同 key 命中缓存 + 输出 byte-equal，钉「override 的是解析结果而非 Option 形状」）；
    /// ② 换成**另一张表**（stub content hash 同为全 0，仅 Arc 指针不同）→ 必 miss（key 经
    /// 指针区分，串读 = 在错误桶空间读均衡）；③ 白盒钉**真接线**：miss 后缓存里 trainer 持有
    /// 的子树桶表 ptr == override 表（漏接线时 solve 仍用 blueprint 表，仅靠 key 测不出来）。
    #[test]
    fn bucket_override_anchored_keys_and_wires_subtree_table() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x4F56_5252_4944_4531); // "OVRRIDE1"
        let round_start = flop.game_state.clone();
        let legal = SimplifiedNlheGame::legal_actions(&flop);
        let cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            trigger: SearchTrigger::AllPostflop,
            ..SubgameSearchConfig::default()
        };
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let mut cache = SubgameSolveCache::new();
        let d1 = subgame_search_cached(
            Some(&mut cache),
            &flop.game_state,
            &round_start,
            &game,
            &legal,
            flop.current_node_id,
            &strat,
            &cfg,
            None,
            None,
            None,
            0xB0CA,
            0,
        )
        .expect("override=None 应 Ok");
        // ① Some(blueprint 自身表)：解析到同一 Arc → 同 key 命中 + byte-equal。
        let d2 = subgame_search_cached(
            Some(&mut cache),
            &flop.game_state,
            &round_start,
            &game,
            &legal,
            flop.current_node_id,
            &strat,
            &cfg,
            Some(&game.bucket_table),
            None,
            None,
            0xB0CA,
            0,
        )
        .expect("override=Some(同表) 应 Ok");
        assert_eq!(
            (cache.misses(), cache.hits()),
            (1, 1),
            "Some(blueprint 自身表) 须与 None 同 key（命中）"
        );
        assert_eq!(format!("{d1:?}"), format!("{d2:?}"), "同表 override ≡ None");
        // ②③ 换另一张表：miss + 缓存 trainer 真持有 override 表。
        let other = stub_table();
        let d3 = subgame_search_cached(
            Some(&mut cache),
            &flop.game_state,
            &round_start,
            &game,
            &legal,
            flop.current_node_id,
            &strat,
            &cfg,
            Some(&other),
            None,
            None,
            0xB0CA,
            0,
        )
        .expect("override=Some(另一表) 应 Ok");
        assert!(!d3.is_empty());
        assert_eq!(cache.misses(), 2, "换表（仅指针不同）→ 必 miss");
        let (_, solved) = cache.entry.as_ref().expect("miss 后须 store");
        assert!(
            Arc::ptr_eq(&solved.trainer.game().bucket_table, &other),
            "子树 solve 须真用 override 表（接线，非仅 key）"
        );
    }

    /// unanchored（生产脱影子路径）同三件套：同表 override 命中 + byte-equal；换表 miss +
    /// trainer 真持有 override 表 + 分布仍归一。
    #[test]
    fn bucket_override_unanchored_keys_and_wires() {
        let game = nolimp_6max_game();
        let auth = offstack_allin_flop_state();
        let cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            ..SubgameSearchConfig::default()
        };
        let mut cache = SubgameSolveCache::new();
        let d1 = subgame_search_unanchored_cached(
            Some(&mut cache),
            &auth,
            &auth,
            &game,
            &[],
            &cfg,
            None,
            None,
            0xB0CB,
        )
        .expect("override=None 应 Ok");
        let d2 = subgame_search_unanchored_cached(
            Some(&mut cache),
            &auth,
            &auth,
            &game,
            &[],
            &cfg,
            Some(&game.bucket_table),
            None,
            0xB0CB,
        )
        .expect("override=Some(同表) 应 Ok");
        assert_eq!((cache.misses(), cache.hits()), (1, 1), "同表 → 命中");
        assert_eq!(format!("{d1:?}"), format!("{d2:?}"), "同表 override ≡ None");
        let other = stub_table();
        let d3 = subgame_search_unanchored_cached(
            Some(&mut cache),
            &auth,
            &auth,
            &game,
            &[],
            &cfg,
            Some(&other),
            None,
            0xB0CB,
        )
        .expect("override=Some(另一表) 应 Ok");
        assert_eq!(cache.misses(), 2, "换表 → 必 miss");
        let sum: f64 = d3.iter().map(|(_, p)| p).sum();
        assert!((sum - 1.0).abs() < 1e-9, "override 解分布应归一，和={sum}");
        let (_, solved) = cache.entry.as_ref().expect("miss 后须 store");
        assert!(
            Arc::ptr_eq(&solved.trainer.game().bucket_table, &other),
            "unanchored 子树 solve 须真用 override 表"
        );
    }

    /// bucket_override 与 depth_limit 互斥（叶子续局值表按 blueprint 桶空间键，换表 = 查错桶）
    /// → 早 `Err`，不静默择一。
    #[test]
    fn bucket_override_depth_limit_rejected() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x4F56_5252_4944_4532); // "OVRRIDE2"
        let round_start = flop.game_state.clone();
        let legal = SimplifiedNlheGame::legal_actions(&flop);
        let cfg_dl = SubgameSearchConfig {
            depth_limit: true,
            ..SubgameSearchConfig::default()
        };
        let strat = |_: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let other = stub_table();
        let err = subgame_search_cached(
            None,
            &flop.game_state,
            &round_start,
            &game,
            &legal,
            flop.current_node_id,
            &strat,
            &cfg_dl,
            Some(&other),
            None,
            None,
            1,
            0,
        )
        .expect_err("depth_limit + bucket_override 须 Err");
        assert!(
            err.contains("bucket_override 与 depth_limit 不兼容"),
            "错误须指明互斥原因，得：{err}"
        );
    }

    // —— 缺口①续：live_traversers（traverser 只轮子树根仍 Active 的座）——

    /// live_traversers 端到端：off-stack 线（6 座中 3 弃牌 + UTG all-in → 仅 SB/BB Active，
    /// 默认轮转下 4/6 迭代零学习）开旗求解 Ok + 分布归一 + 同 seed 可复现（rng 消费序列与
    /// 默认轮转不同，但固定迭代下自身确定性）；且旗进 within-round solve 缓存 key
    /// （翻转必 miss——两种轮转解出的是不同 rng 流的均衡，串读 = 读错均衡）。
    #[test]
    fn live_traversers_solves_reproducible_and_keyed() {
        let game = nolimp_6max_game();
        let auth = offstack_allin_flop_state();
        let cfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 1_000_000,
            live_traversers: true,
            ..SubgameSearchConfig::default()
        };
        let run = || subgame_search_unanchored(&auth, &auth, &game, &[], &cfg, 0x7261);
        let d1 = run().expect("live_traversers 求解应 Ok");
        let sum: f64 = d1.iter().map(|(_, p)| p).sum();
        assert!((sum - 1.0).abs() < 1e-9, "分布应归一，和={sum}");
        assert!(d1.iter().all(|(_, p)| *p > 0.0 && p.is_finite()));
        let d2 = run().expect("再跑一次");
        assert_eq!(format!("{d1:?}"), format!("{d2:?}"), "同 seed 须可复现");

        // 旗进缓存 key：off 先 solve，翻 on 必 miss（key 覆盖 live_traversers）。
        let mut cache = SubgameSolveCache::new();
        let off = SubgameSearchConfig {
            live_traversers: false,
            ..cfg
        };
        subgame_search_unanchored_cached(
            Some(&mut cache),
            &auth,
            &auth,
            &game,
            &[],
            &off,
            None,
            None,
            0x7261,
        )
        .expect("off 应 Ok");
        subgame_search_unanchored_cached(
            Some(&mut cache),
            &auth,
            &auth,
            &game,
            &[],
            &cfg,
            None,
            None,
            0x7261,
        )
        .expect("on 应 Ok");
        assert_eq!(
            (cache.misses(), cache.hits()),
            (2, 0),
            "live_traversers 翻转须 miss（key 覆盖该旗）"
        );
    }
}
