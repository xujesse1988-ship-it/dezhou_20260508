//! 简化 NLHE 抽象 betting tree（`docs/nlhe_infoset_history_investigation.md` 方案
//! A Phase 1）。
//!
//! 在 `SimplifiedNlheGame::new` 时一次性构建：从 `GameState::with_rng` root 出发
//! DFS 枚举所有 reachable 抽象动作序列；每个决策节点分配唯一 [`NodeId`]，由
//! `(player_acting, street, abstract_action_path)` 唯一确定。运行时
//! `SimplifiedNlheState` 携带 `current_node_id`，沿 `node.children[action_idx]`
//! 跳转。`info_set` 的下注历史维度直接用 `node_id`，因此跨街 aggressor / raise
//! 深度天然区分（Slumbot 2019 形态）。
//!
//! **不**复用 stage-2 `InfoAbstraction` —— 通用层不知道 betting tree 概念；本树
//! 是 stage-3 简化 NLHE 私有的内部数据结构（不 `pub use` 到 crate root）。

use smallvec::SmallVec;

use crate::abstraction::action::{
    AbstractAction, AbstractActionSet, ActionAbstraction, ActionAbstractionConfig, BetRatio,
    StreetActionAbstraction,
};
use crate::abstraction::info::StreetTag;
use crate::core::rng::ChaCha20Rng;
use crate::core::{ChipAmount, PlayerStatus, Street};
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::PlayerId;

/// 决策节点 id；200BB 默认 + 6-action {0.5p, 1p, 2p, allin} 实测 240,096 节点（18 bit），
/// u32 给后续扩 raise size / 加深筹码 profile 留余量。
pub type NodeId = u32;

/// 抽象动作的"身份"——同抽象边在不同 chip state 下产出的 `AbstractAction`
/// 实例 `to` 金额可能不同（连续值），但 `AbstractActionTag` 相同。树节点匹配只
/// 看 tag，不看 `to` 金额，否则连续 chip 值会让节点数爆炸。
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum AbstractActionTag {
    Fold,
    Check,
    Call,
    Bet(BetRatio),
    Raise(BetRatio),
    AllIn,
}

impl AbstractActionTag {
    pub fn of(action: &AbstractAction) -> Self {
        match action {
            AbstractAction::Fold => AbstractActionTag::Fold,
            AbstractAction::Check => AbstractActionTag::Check,
            AbstractAction::Call { .. } => AbstractActionTag::Call,
            AbstractAction::Bet { ratio_label, .. } => AbstractActionTag::Bet(*ratio_label),
            AbstractAction::Raise { ratio_label, .. } => AbstractActionTag::Raise(*ratio_label),
            AbstractAction::AllIn { .. } => AbstractActionTag::AllIn,
        }
    }
}

/// 子节点出口：`Decision(id)` 指向下一决策点（同街或下一街首个 actor）；
/// `Terminal` 表示该分支终止（fold / showdown），不分配 node_id。
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Child {
    Decision(NodeId),
    Terminal,
}

#[derive(Clone, Debug)]
pub struct TreeNode {
    pub street: StreetTag,
    pub player_acting: PlayerId,
    pub parent: Option<NodeId>,
    pub action_from_parent: Option<AbstractActionTag>,
    /// 节点合法动作集 tag 序列，**顺序与 [`StreetActionAbstraction::abstract_actions`]
    /// 输出严格对齐**（D-209）。`children[i]` 对应 `legal_actions[i]` 这条边。
    pub legal_actions: SmallVec<[AbstractActionTag; 8]>,
    pub children: SmallVec<[Child; 8]>,
}

/// A3×A4 抽象层「改游戏」规则（`docs/temp/a3xa4_wiring_design_2026_06_01.md`）。
///
/// 全部作用在 `abs.abstract_actions(state)` 之后、建 node 之前——**不碰规则引擎**
/// （`GameState::legal_actions` / side pot / showdown 一行不动，S1 PokerKit 跨验证不受
/// 影响）。`Default`（全关）与历史行为逐节点 byte-equal（守 240,096 / 719,764 节点）。
#[derive(Copy, Clone, Debug)]
pub struct BettingAbstractionRules {
    /// A3 first-bet-small：0.5pot 只许作**首次进攻**（开池 `Bet`，或 preflop 越过 BB 的
    /// 首个 `Raise`），任何 **re-raise**（本街已有进攻后）一律 1pot。`raises_on_street`
    /// 感知（见 [`filter_actions`]）：postflop 行为与历史 byte-equal（postflop `Raise`
    /// 恒有前序进攻 → `raises>0` → 照旧删 0.5 reraise），但允许 preflop 菜单含 0.5 时
    /// 把它当 2.25BB 开池档（见 [`first_small_preopen_6max`]）。
    pub drop_small_reraise: bool,
    /// A4 width redirect（closing-action 优先）：preflop 第 (N+1) 个 entrant 禁被动
    /// 进场（删 `Check`/`Call`，只剩 fold 或 squeeze），把见 flop 人数收到 ≤ N。
    /// [`WIDTH_REDIRECT_OFF`](Self::WIDTH_REDIRECT_OFF) = 关。
    pub width_redirect: u8,
    /// 禁非盲注位的开池 limp：preflop 未被加注（facing 仅 BB）时，删掉**非 SB**位的
    /// `Call`（= open-limp），强制 raise-or-fold。SB 的 complete（rp==1）保留（GTO 合理）；
    /// BB 此局面是 `Check`（无 `Call` 可删）。治 S4「UTG limp 16.6%」病 + 缩树。`false` =
    /// 关（HU 默认 / `first_small_6max` 保持原行为，守 cross-check）。
    pub no_open_limp: bool,
    /// preflop 开池单档（仅小档）：preflop 开池（`raises_on_street == 0`，越过 BB 的**首个**
    /// `Raise`）只保留 0.5pot（[`BetRatio::HALF_POT`] = 2.25BB）开池档，删 1.0pot
    /// （[`BetRatio::FULL_POT`] = 3.5BB）。**仅 preflop 开池生效**：re-raise（`raises>0`）的档由
    /// `drop_small_reraise` 收到 1.0pot，postflop 一行不动。与 `drop_small_reraise` 叠加 →
    /// preflop 严格单档：开池 2.25BB、3bet+ 1.0pot。配 preflop`{0.5,1}` 菜单（见
    /// [`first_small_preopen_small_6max`]）；菜单无 1.0pot 时本旗空转。`false` = 关（默认，
    /// 保持 [`first_small_preopen_6max`] 的双开池档行为，守 cross-check）。
    pub preflop_open_small_only: bool,
    /// 禁 preflop 首个进攻 AllIn（开池 all-in）。仅删 `raises_on_street == 0` 时的
    /// `AllIn` 槽；面对加注、短码 all-in 跟注、postflop all-in 都保留。当前用于
    /// [`first_small_preopen_small_6max`]：开池动作严格收成 2.25BB 单档。
    pub drop_preflop_open_allin: bool,
}

impl BettingAbstractionRules {
    /// `width_redirect` 关闭哨兵（n_seats ≤ 9 < 255，永不触发 gate）。
    pub const WIDTH_REDIRECT_OFF: u8 = 255;
}

impl Default for BettingAbstractionRules {
    fn default() -> Self {
        BettingAbstractionRules {
            drop_small_reraise: false,
            width_redirect: Self::WIDTH_REDIRECT_OFF,
            no_open_limp: false,
            preflop_open_small_only: false,
            drop_preflop_open_allin: false,
        }
    }
}

/// A3×A4 first-bet-small 6-max profile：preflop `{1}` / postflop `{0.5,1}` 菜单 +
/// `drop_small_reraise`（0.5 仅开池）**配对**返回，杜绝菜单/标志错配（只设菜单不设
/// 标志 = 全程含 0.5 re-raise = 224 GiB 那版；反之标志空转）。`width_redirect` = N
/// （2/3，[`BettingAbstractionRules::WIDTH_REDIRECT_OFF`] = 不限宽）。配
/// `TableConfig::default_6max_100bb()` 建树即 §A3×A4 capped 博弈。
pub fn first_small_6max(width_redirect: u8) -> (StreetActionAbstraction, BettingAbstractionRules) {
    let pre = ActionAbstractionConfig::new(vec![1.0]).expect("preflop {1.0} 合法");
    let post = || ActionAbstractionConfig::new(vec![0.5, 1.0]).expect("postflop {0.5,1.0} 合法");
    let abs = StreetActionAbstraction::per_street([pre, post(), post(), post()]);
    let rules = BettingAbstractionRules {
        drop_small_reraise: true,
        width_redirect,
        no_open_limp: false,
        preflop_open_small_only: false,
        drop_preflop_open_allin: false,
    };
    (abs, rules)
}

/// S4 reshape：在 [`first_small_6max`] 基础上 **加 2.25BB 开池档 + 禁非 SB 开池 limp**。
/// preflop 菜单 `{0.5,1.0}`（drop_small_reraise raises-aware → 0.5 仅作开池 = 越过 BB 的
/// 首个 raise ≈ 2.25BB；3bet+ 一律 1.0pot），postflop `{0.5,1.0}` 不变。`no_open_limp`
/// 删 UTG/HJ/CO/BTN 的 open-limp（SB complete 保留）。
///
/// 动机（`docs/six_max_nlhe_target.md` S4 诊断 + 2026-06-01 噪声实测）：1B blueprint 的
/// 弱点是 ① 过度 limp（UTG 16.6%）② 唯一 3.5BB 开池档太大 → 边缘手宁可 limp/fold。本 profile
/// 两头夹：给便宜开池档（更多手能 raise-in），同时拿掉 limp 这条被噪声填满的劣化支路。
pub fn first_small_preopen_6max(
    width_redirect: u8,
) -> (StreetActionAbstraction, BettingAbstractionRules) {
    let pre = ActionAbstractionConfig::new(vec![0.5, 1.0]).expect("preflop {0.5,1.0} 合法");
    let post = || ActionAbstractionConfig::new(vec![0.5, 1.0]).expect("postflop {0.5,1.0} 合法");
    let abs = StreetActionAbstraction::per_street([pre, post(), post(), post()]);
    let rules = BettingAbstractionRules {
        drop_small_reraise: true,
        width_redirect,
        no_open_limp: true,
        preflop_open_small_only: false,
        drop_preflop_open_allin: false,
    };
    (abs, rules)
}

/// S4 reshape 变体：在 [`first_small_preopen_6max`] 基础上 **把 preflop 开池收成单档（仅
/// 2.25BB）**。preflop 菜单仍 `{0.5,1.0}`，但 `preflop_open_small_only` 删开池 1.0pot
/// （3.5BB）→ 开池只剩 0.5pot（2.25BB）；`drop_small_reraise` 把 3bet+ 收到 1.0pot。
/// 同时删 preflop 首个进攻 AllIn。即 preflop 严格单档：**开池 2.25BB、3bet+ 1.0pot、
/// 禁非 SB limp、无开池 all-in**。postflop `{0.5,1.0}` A3 first-small 不变。
///
/// 动机：`preopen`（[`first_small_preopen_6max`]）给两个开池档（2.25/3.5BB），树比单档大；
/// 而 `nolimp`（[`first_small_6max`] + no_limp）单档但开池是 3.5BB，偏紧（S4 续⑤：BTN
/// raise 37% 窄于 GTO）。本 profile 取中间：单开池档但用便宜的 2.25BB（更宽开池范围），树比
/// `preopen` 小（preflop 开池少一档）。
pub fn first_small_preopen_small_6max(
    width_redirect: u8,
) -> (StreetActionAbstraction, BettingAbstractionRules) {
    let (abs, mut rules) = first_small_preopen_6max(width_redirect);
    rules.preflop_open_small_only = true;
    rules.drop_preflop_open_allin = true;
    (abs, rules)
}

pub struct PublicBettingTree {
    nodes: Vec<TreeNode>,
    root_id: NodeId,
}

/// 建树用的固定 RNG seed。树结构只依赖 abstract action geometry，不依赖
/// hole/board cards；这个 seed 只是 `GameState::with_rng` API 强制的输入。
const TREE_BUILD_RNG_SEED: u64 = 0x4E4C_4845_5F54_5245; // "NLHE_TRE"

impl PublicBettingTree {
    /// 从 `config` 出发 DFS 建完整树（全街 `{0.5,1,2}` 默认 abstraction）。同
    /// `config` 必出同结构（节点顺序确定）。
    ///
    /// 行为与历史一致：`StreetActionAbstraction::default_6_action()` 四条街共用
    /// `{0.5,1,2}`，与旧的 `DefaultActionAbstraction::default_6_action()` byte-equal。
    pub fn build(config: &TableConfig) -> PublicBettingTree {
        Self::build_with_abstraction(config, &StreetActionAbstraction::default_6_action())
    }

    /// 用指定的按街 abstraction 建树（bet-size 扩张：flop `{0.33,0.66,1,2}` 等）。
    /// `legal_actions` 随街变——`TreeNode.legal_actions` 逐节点由
    /// `abs.abstract_actions(state)` 决定，因此 [`crate::abstraction::action::StreetActionAbstraction::per_street`]
    /// 下各街取各街的 raise 集合。同 `(config, abs)` 必出同结构。
    pub fn build_with_abstraction(
        config: &TableConfig,
        abs: &StreetActionAbstraction,
    ) -> PublicBettingTree {
        Self::build_with_rules(config, abs, BettingAbstractionRules::default())
    }

    /// 用指定 abstraction + A3×A4 规则（[`BettingAbstractionRules`]）建树。
    /// `rules == Default`（全关）时与 [`build_with_abstraction`](Self::build_with_abstraction)
    /// 逐节点 byte-equal；设 `drop_small_reraise` / `width_redirect` 即 §A3×A4 capped
    /// 博弈（见 `docs/temp/a3xa4_wiring_design_2026_06_01.md`）。同 `(config, abs, rules)`
    /// 必出同结构（节点顺序确定）。
    pub fn build_with_rules(
        config: &TableConfig,
        abs: &StreetActionAbstraction,
        rules: BettingAbstractionRules,
    ) -> PublicBettingTree {
        let mut tree = PublicBettingTree {
            nodes: Vec::new(),
            root_id: 0,
        };
        let mut rng = ChaCha20Rng::from_seed(TREE_BUILD_RNG_SEED);
        let state = GameState::with_rng(config, 0, &mut rng);
        debug_assert!(
            !state.is_terminal() && state.current_player().is_some(),
            "root state must be a Player decision node"
        );
        // root entrants = 0：盲注是强制投入、非 voluntary（probe 同口径）。raises_on_street = 0
        // （preflop BB 是强制盲注、非 walk 动作，不计进攻 → preflop 首个 raise 见 raises==0）。
        tree.root_id = tree.walk(state, None, None, abs, &rules, 0, 0);
        tree
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        state: GameState,
        parent: Option<NodeId>,
        action_from_parent: Option<AbstractActionTag>,
        abs: &StreetActionAbstraction,
        rules: &BettingAbstractionRules,
        entrants: u16,
        raises_on_street: u32,
    ) -> NodeId {
        let actor = state
            .current_player()
            .expect("walk only invoked on Player nodes")
            .0 as PlayerId;
        let my_id = self.nodes.len() as NodeId;

        // no_open_limp：preflop 未被加注（max committed == BB）且 actor 非 SB（rp != 1）→
        // 该节点的 `Call` 是 open-limp，删之（强制 raise-or-fold）。BB（rp==2）此局面是
        // free-check 无 Call，moot；SB（rp==1）的 complete 保留（GTO 合理）。rules 关时恒 false。
        let drop_open_limp = rules.no_open_limp && state.street() == Street::Preflop && {
            let cfg = state.config();
            let max_committed = state
                .players()
                .iter()
                .map(|p| p.committed_this_round)
                .max()
                .unwrap_or(ChipAmount::ZERO);
            let n = cfg.n_seats;
            let rp = (actor + n - cfg.button_seat.0) % n; // BTN=0 SB=1 BB=2 UTG=3..
            max_committed == cfg.big_blind && rp != 1
        };

        // A4 redirect 不变量（closing-action 精确收口）：postflop 在场恒 ≤ N。>N 只可能
        // 是多人 all-in 跑马（无 postflop 下注，不进 walk）。debug 建树即验（= probe
        // redirect_postflop_over_n == 0 的生产侧断言）。
        debug_assert!(
            rules.width_redirect == BettingAbstractionRules::WIDTH_REDIRECT_OFF
                || state.street() == Street::Preflop
                || live_count(&state) <= rules.width_redirect,
            "redirect invariant 破：postflop live {} > N {}",
            live_count(&state),
            rules.width_redirect
        );

        // preflop_open_small_only：preflop 开池（raises==0 = 越过 BB 的首个 Raise）只留
        // 0.5pot 开池档，删 1.0pot（见 filter_actions）。仅 preflop 开池生效（re-raise 由
        // drop_small_reraise 收档；postflop 不碰）。rules 关时恒 false。
        let drop_large_open = rules.preflop_open_small_only
            && state.street() == Street::Preflop
            && raises_on_street == 0;
        let drop_open_allin = rules.drop_preflop_open_allin
            && state.street() == Street::Preflop
            && raises_on_street == 0;

        let legal_set = abs.abstract_actions(&state);
        // A3×A4 过滤（drop_small_reraise + redirect 禁被动进场）。filter 后的集合同时
        // 决定 node.legal_actions 与 children → child 下标恒对齐 legal_actions（D-209）。
        let allowed = filter_actions(
            &legal_set,
            actor,
            entrants,
            rules,
            raises_on_street,
            drop_open_limp,
            drop_large_open,
            drop_open_allin,
        );
        let legal_actions: SmallVec<[AbstractActionTag; 8]> =
            allowed.iter().map(AbstractActionTag::of).collect();

        // Placeholder for stable index; children backfilled after recursion.
        self.nodes.push(TreeNode {
            street: street_to_tag(state.street()),
            player_acting: actor,
            parent,
            action_from_parent,
            legal_actions,
            children: SmallVec::new(),
        });

        let mut children: SmallVec<[Child; 8]> = SmallVec::new();
        for action in allowed.iter().copied() {
            let mut next_state = state.clone();
            next_state
                .apply(action.to_concrete())
                .expect("action abstraction must emit legal Actions for current state");
            // entrants 累积（A4 redirect）：Fold 清 actor 位，其它（含 Check）置位；
            // 跨街不清零（entrant = 谁还在这手牌里）。rules 关时不被读，零影响。
            let next_entrants = if matches!(action, AbstractAction::Fold) {
                entrants & !(1u16 << actor)
            } else {
                entrants | (1u16 << actor)
            };
            // raises_on_street（A3 raises-aware）：切街清零；本街进攻（Bet/Raise/AllIn）+1；
            // 被动（Check/Call/Fold）不变。下一节点据此判 0.5 是开池(raises==0)还是 re-raise。
            // 用 `as u8` 比街（next 可能是 terminal Showdown，不能过 street_to_tag）。
            let next_raises = if next_state.street() as u8 != state.street() as u8 {
                0
            } else if is_aggression(&action) {
                raises_on_street + 1
            } else {
                raises_on_street
            };
            let child = if next_state.is_terminal() {
                Child::Terminal
            } else {
                let next_action_tag = AbstractActionTag::of(&action);
                Child::Decision(self.walk(
                    next_state,
                    Some(my_id),
                    Some(next_action_tag),
                    abs,
                    rules,
                    next_entrants,
                    next_raises,
                ))
            };
            children.push(child);
        }
        self.nodes[my_id as usize].children = children;
        my_id
    }

    pub fn root_id(&self) -> NodeId {
        self.root_id
    }

    pub fn num_nodes(&self) -> usize {
        self.nodes.len()
    }

    pub fn node(&self, id: NodeId) -> &TreeNode {
        &self.nodes[id as usize]
    }

    /// 用于诊断 / 测试：把节点路径还原成 root..node 的 `AbstractActionTag` 序列。
    pub fn path_to_root(&self, mut id: NodeId) -> Vec<AbstractActionTag> {
        let mut path = Vec::new();
        loop {
            let node = self.node(id);
            match node.action_from_parent {
                Some(tag) => path.push(tag),
                None => break,
            }
            id = node.parent.expect("non-root node must have parent");
        }
        path.reverse();
        path
    }
}

fn street_to_tag(s: Street) -> StreetTag {
    match s {
        Street::Preflop => StreetTag::Preflop,
        Street::Flop => StreetTag::Flop,
        Street::Turn => StreetTag::Turn,
        Street::River => StreetTag::River,
        Street::Showdown => {
            unreachable!("Showdown is terminal — should not enter walk as a decision node")
        }
    }
}

/// 在场人数（`Active∪AllIn`）= A4 width 口径（all-in 玩家已见后续街、仍在底池）。
fn live_count(state: &GameState) -> u8 {
    state
        .players()
        .iter()
        .filter(|p| matches!(p.status, PlayerStatus::Active | PlayerStatus::AllIn))
        .count() as u8
}

/// 进攻动作（推进 `raises_on_street`）：`Bet` / `Raise` / `AllIn`。把 AllIn 也计进攻，
/// 保证任何 postflop `Raise` 必见 `raises_on_street > 0`（postflop Raise 恒有前序进攻：
/// Bet 或越-bet 的 AllIn 都已 +1）→ raises-aware drop 与历史「删全部 Raise{0.5}」byte-equal。
fn is_aggression(a: &AbstractAction) -> bool {
    matches!(
        a,
        AbstractAction::Bet { .. } | AbstractAction::Raise { .. } | AbstractAction::AllIn { .. }
    )
}

/// 对 `abs.abstract_actions(state)` 套 A3×A4(+S4 reshape) 过滤（A3×A4 部分逐字对齐
/// `tools/nlhe_betting_tree_sizing.rs` 的 `WIDTH_REDIRECT` 探针 `6e6acac`）：
/// - `drop_small_reraise`（raises-aware）：仅当 `raises_on_street > 0` 删 `Raise{0.5pot}`
///   （0.5 仅作首次进攻）。postflop `Raise` 恒有前序进攻 → 与历史「删全部 Raise{0.5}」
///   byte-equal；preflop 菜单含 0.5 时其开池 Raise（raises==0）得以保留 = 2.25BB 开池档。
/// - `drop_open_limp`（S4 no_open_limp，已在 walk 算好「preflop 未加注 + actor 非 SB」）：删 `Call`。
/// - `drop_large_open`（S4 preflop_open_small_only，已在 walk 算好「preflop 开池 raises==0」）：删
///   `Raise{1.0pot}`（[`BetRatio::FULL_POT`]）→ 开池只剩 0.5pot。与 drop_small_reraise 镜像
///   （后者删 re-raise 的 0.5pot；本者删 open 的 1.0pot）。
/// - `drop_open_allin`（S4 preopen-small，已在 walk 算好同一「preflop 开池 raises==0」）：删
///   开池 `AllIn`，但保留 3bet+ / all-in call / postflop `AllIn`。
/// - width redirect：第 (N+1) 个 entrant 留下 → 删 `Check`/`Call`（fold 或 squeeze）。
///   `e` = 当前 entrant 数；actor 已是 entrant → 留下 E 不变，否则 E+1；留下后 > N 即 gate。
///
/// `entrants` = 本手已做过 ≥1 非弃牌动作的座位 bitmask（含 Check；Fold 清位）。
/// 结果**永不为空**：过滤后 preflop 开池位至少留 `Fold + Raise{0.5}`；free-check
/// 局面抽象层按 D-204 不发 Fold，但仍留 `Check` 或 sized raise。width redirect 超员
/// limped 池被 gate 的 (N+1) 进场者落在 squeeze/fold（或非开池 all-in），见设计 §7。
#[allow(clippy::too_many_arguments)]
fn filter_actions(
    legal_set: &AbstractActionSet,
    actor: PlayerId,
    entrants: u16,
    rules: &BettingAbstractionRules,
    raises_on_street: u32,
    drop_open_limp: bool,
    drop_large_open: bool,
    drop_open_allin: bool,
) -> SmallVec<[AbstractAction; 8]> {
    let block_passive = if rules.width_redirect != BettingAbstractionRules::WIDTH_REDIRECT_OFF {
        let e = entrants.count_ones();
        let actor_in = (entrants >> actor) & 1 == 1;
        let stay_e = if actor_in { e } else { e + 1 };
        stay_e > u32::from(rules.width_redirect)
    } else {
        false
    };
    legal_set
        .iter()
        .copied()
        .filter(|a| {
            if rules.drop_small_reraise && raises_on_street > 0 {
                if let AbstractAction::Raise { ratio_label, .. } = a {
                    if *ratio_label == BetRatio::HALF_POT {
                        return false;
                    }
                }
            }
            if drop_open_limp && matches!(a, AbstractAction::Call { .. }) {
                return false;
            }
            if drop_large_open {
                if let AbstractAction::Raise { ratio_label, .. } = a {
                    if *ratio_label == BetRatio::FULL_POT {
                        return false;
                    }
                }
            }
            if drop_open_allin && matches!(a, AbstractAction::AllIn { .. }) {
                return false;
            }
            if block_passive && matches!(a, AbstractAction::Check | AbstractAction::Call { .. }) {
                return false;
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn build_default_tree() -> PublicBettingTree {
        PublicBettingTree::build(&TableConfig::default_hu_200bb())
    }

    /// S4 reshape sizing：打印 baseline / +no_limp / +preopen 三 profile 的节点·infoset·
    /// 两表内存，对照当前 N=3 (1,154,822 / 230.5M / 8.04 GiB)。节点计数确定、机器无关 →
    /// 本机跑可信。`cargo test --release -- --ignored --nocapture reshape_sizes`。
    #[test]
    #[ignore = "sizing 诊断打印；release + --ignored --nocapture 跑"]
    fn reshape_sizes() {
        let cfg = TableConfig::default_6max_100bb();
        let summarize =
            |label: &str, abs: &StreetActionAbstraction, rules: BettingAbstractionRules| {
                let tree = PublicBettingTree::build_with_rules(&cfg, abs, rules);
                let (mut pre_nodes, mut post_nodes, mut rows, mut slots) = (0u64, 0u64, 0u64, 0u64);
                for id in 0..tree.num_nodes() as NodeId {
                    let node = tree.node(id);
                    let buckets = if node.street == StreetTag::Preflop {
                        169
                    } else {
                        200
                    };
                    if node.street == StreetTag::Preflop {
                        pre_nodes += 1;
                    } else {
                        post_nodes += 1;
                    }
                    rows += buckets;
                    slots += buckets * node.legal_actions.len() as u64;
                }
                let two_tables_gib = (slots * 8 * 2) as f64 / (1u64 << 30) as f64;
                println!(
                    "{label:<26} nodes={:>9} (pre {pre_nodes}/post {post_nodes}) infosets={:>13} \
                 two_tables={two_tables_gib:6.2} GiB",
                    tree.num_nodes(),
                    rows,
                );
            };

        let (b_abs, b_rules) = first_small_6max(3);
        summarize("baseline first_small N=3", &b_abs, b_rules);

        let (nl_abs, mut nl_rules) = first_small_6max(3);
        nl_rules.no_open_limp = true;
        summarize("+no_open_limp N=3", &nl_abs, nl_rules);

        let (pp_abs, pp_rules) = first_small_preopen_6max(3);
        summarize("+preopen(0.5,1)+no_limp N=3", &pp_abs, pp_rules);

        let (ps_abs, ps_rules) = first_small_preopen_small_6max(3);
        summarize("+preopen-small(open0.5)+no_limp N=3", &ps_abs, ps_rules);
    }

    /// 结构守门：reshape 确实在 UTG 根删掉 open-limp Call 且给两个开池档；baseline 仍有 limp。
    /// 证明 `no_open_limp` + preflop`{0.5,1}` 接对了（非只是树变小）。
    #[test]
    fn reshape_root_drops_open_limp() {
        let cfg = TableConfig::default_6max_100bb();
        // baseline：UTG 根有 open-limp Call。
        let (b_abs, b_rules) = first_small_6max(3);
        let bt = PublicBettingTree::build_with_rules(&cfg, &b_abs, b_rules);
        let broot = bt.node(bt.root_id());
        assert_eq!(broot.street, StreetTag::Preflop);
        assert!(
            broot.legal_actions.contains(&AbstractActionTag::Call),
            "baseline UTG 根应允许 open-limp Call"
        );
        // preopen：UTG 根无 Call、有 2 个 Raise 档（0.5=2.25BB / 1.0=3.5BB）+ Fold + AllIn。
        let (abs, rules) = first_small_preopen_6max(3);
        let t = PublicBettingTree::build_with_rules(&cfg, &abs, rules);
        let root = t.node(t.root_id());
        assert_eq!(root.street, StreetTag::Preflop);
        assert!(
            !root.legal_actions.contains(&AbstractActionTag::Call),
            "no_open_limp：UTG 根必须删 open-limp Call"
        );
        let n_raises = root
            .legal_actions
            .iter()
            .filter(|t| matches!(t, AbstractActionTag::Raise(_)))
            .count();
        assert_eq!(n_raises, 2, "preopen：UTG 根应有 2 个开池档（0.5/1.0）");
        assert!(root.legal_actions.contains(&AbstractActionTag::Fold));
        assert!(root.legal_actions.contains(&AbstractActionTag::AllIn));
    }

    /// 结构守门：`preopen-small`（[`first_small_preopen_small_6max`]）把 preflop 开池收成
    /// **单档（仅 0.5pot/2.25BB）**，且没有开池 AllIn；re-raise 仍是单档
    /// 1.0pot。证明 `preflop_open_small_only` / `drop_preflop_open_allin` 接对了——
    /// 开池删 1.0pot + AllIn，且不渗入 re-raise（re-raise 的档由 drop_small_reraise 收）。
    /// 用 N=2（树小，debug 快）；只读 root + 其开池子节点。
    #[test]
    fn reshape_preopen_small_single_open_size() {
        let cfg = TableConfig::default_6max_100bb();
        let (abs, rules) = first_small_preopen_small_6max(2);
        let t = PublicBettingTree::build_with_rules(&cfg, &abs, rules);
        let root = t.node(t.root_id());
        assert_eq!(root.street, StreetTag::Preflop);
        // UTG 开池：无 limp Call、无 AllIn、Fold 在、Raise 恰 1 档 = 0.5pot(HALF_POT)。
        assert!(
            !root.legal_actions.contains(&AbstractActionTag::Call),
            "preopen-small：UTG 根删 open-limp Call"
        );
        assert!(root.legal_actions.contains(&AbstractActionTag::Fold));
        assert!(
            !root.legal_actions.contains(&AbstractActionTag::AllIn),
            "preopen-small：UTG 根应删除开池 AllIn"
        );
        let open_raises: Vec<BetRatio> = root
            .legal_actions
            .iter()
            .filter_map(|t| match t {
                AbstractActionTag::Raise(r) => Some(*r),
                _ => None,
            })
            .collect();
        assert_eq!(
            open_raises,
            vec![BetRatio::HALF_POT],
            "preopen-small：preflop 开池只剩 0.5pot 单档（删 1.0pot）"
        );
        // 沿全 fold-chain 到 SB RFI：SB complete 保留，但开池 AllIn 同样删除。
        let mut sb_node_id = t.root_id();
        for pos in ["UTG", "HJ", "CO", "BTN"] {
            let node = t.node(sb_node_id);
            let fold_idx = node
                .legal_actions
                .iter()
                .position(|t| matches!(t, AbstractActionTag::Fold))
                .unwrap_or_else(|| panic!("{pos} RFI 应有 Fold"));
            sb_node_id = match node.children[fold_idx] {
                Child::Decision(id) => id,
                Child::Terminal => panic!("{pos} Fold 后不应直接终局，直到 SB Fold 才终局"),
            };
        }
        let sb_node = t.node(sb_node_id);
        assert_eq!(sb_node.player_acting, 1, "fold 到 SB 后应由 SB 行动");
        assert!(
            sb_node.legal_actions.contains(&AbstractActionTag::Call),
            "preopen-small：SB complete/limp 应保留"
        );
        assert!(
            !sb_node.legal_actions.contains(&AbstractActionTag::AllIn),
            "preopen-small：SB RFI 也应删除开池 AllIn"
        );
        // 沿 0.5pot 开池边走到下家 → 面对加注（raises==1）→ re-raise 恰 1 档 = 1.0pot(FULL_POT)。
        let open_idx = root
            .legal_actions
            .iter()
            .position(|t| matches!(t, AbstractActionTag::Raise(BetRatio::HALF_POT)))
            .expect("UTG 根有 0.5pot 开池");
        let child_id = match root.children[open_idx] {
            Child::Decision(id) => id,
            Child::Terminal => panic!("开池后不应是终局"),
        };
        let reraise_node = t.node(child_id);
        assert_eq!(reraise_node.street, StreetTag::Preflop);
        assert!(
            reraise_node
                .legal_actions
                .contains(&AbstractActionTag::AllIn),
            "preopen-small：面对开池的 AllIn 槽应保留，只删开池 AllIn"
        );
        let reraise_raises: Vec<BetRatio> = reraise_node
            .legal_actions
            .iter()
            .filter_map(|t| match t {
                AbstractActionTag::Raise(r) => Some(*r),
                _ => None,
            })
            .collect();
        assert_eq!(
            reraise_raises,
            vec![BetRatio::FULL_POT],
            "preopen-small：re-raise 只剩 1.0pot 单档（drop_small_reraise；小开池旗不渗入 re-raise）"
        );
    }

    /// 默认全街 `{0.5,1,2}` 树节点数 = 240,096（与 `nlhe_betting_tree_sizing` 工具
    /// 独立 walk 测得一致）。守住 `build` 路由经 `StreetActionAbstraction::default_6_action`
    /// 后行为不变（前置 P byte-equal 默认路径）。
    #[test]
    fn default_tree_node_count_unchanged() {
        let tree = build_default_tree();
        assert_eq!(
            tree.num_nodes(),
            240_096,
            "默认 6-action 树节点数偏离实测 240,096——build 路由或 abstraction 改了行为"
        );
    }

    /// per-street 目标 profile（flop `{0.33,0.66,1,2}`、其余 `{0.5,1,2}`）树节点数
    /// = 719,764，与 `nlhe_betting_tree_sizing` 工具实测一致。这是按街分派建树的
    /// 外部对照：tree builder 与 sizing 工具走两条独立代码路径，节点计数必须吻合。
    ///
    /// `#[ignore]`：719,764 节点 debug 建树较慢，走 `cargo test --release -- --ignored`。
    #[test]
    #[ignore = "构建 719,764 节点目标树较慢；release + --ignored 跑"]
    fn per_street_target_tree_node_count_matches_sizing_tool() {
        use crate::abstraction::action::ActionAbstractionConfig;
        let r3 = || ActionAbstractionConfig::new(vec![0.5, 1.0, 2.0]).unwrap();
        let flop = ActionAbstractionConfig::new(vec![0.33, 0.66, 1.0, 2.0]).unwrap();
        let abs = StreetActionAbstraction::per_street([r3(), flop, r3(), r3()]);
        let tree =
            PublicBettingTree::build_with_abstraction(&TableConfig::default_hu_200bb(), &abs);
        assert_eq!(
            tree.num_nodes(),
            719_764,
            "目标 profile 树节点数偏离 sizing 工具实测 719,764"
        );
    }

    /// A3×A4 capped 博弈（first-bet-small × width redirect）生产树节点数对
    /// `nlhe_betting_tree_sizing` 探针（`6e6acac`，`FIRST_SMALL=1 WIDTH_REDIRECT=N`）
    /// 实测的 cross-check：tree builder 与 sizing 工具两条独立路径对得上 = A3×A4 过滤
    /// 接对了。N=2 在 debug 跑（78,852 < 默认 240,096，快），顺带在 debug 触发 walk 里
    /// 的 redirect 不变量 `debug_assert`（postflop live ≤ 2）。
    #[test]
    fn redirect_capped_tree_n2_node_count_matches_sizing_tool() {
        let (abs, rules) = first_small_6max(2);
        let tree =
            PublicBettingTree::build_with_rules(&TableConfig::default_6max_100bb(), &abs, rules);
        assert_eq!(
            tree.num_nodes(),
            78_852,
            "first_small × WIDTH_REDIRECT=2 节点数偏离 probe 实测 78,852"
        );
    }

    /// N=3（保 3-way 的甜点）：1,154,822 节点，与 probe `FIRST_SMALL=1 WIDTH_REDIRECT=3`
    /// 逐字对上（infoset@200 = 230.5M、max depth 25，见 §A3×A4 2026-06-01）。
    /// `#[ignore]`：1.15M 节点 debug 建树慢，走 `cargo test --release -- --ignored`。
    #[test]
    #[ignore = "构建 1,154,822 节点 A3×A4 树较慢；release + --ignored 跑"]
    fn redirect_capped_tree_n3_node_count_matches_sizing_tool() {
        let (abs, rules) = first_small_6max(3);
        let tree =
            PublicBettingTree::build_with_rules(&TableConfig::default_6max_100bb(), &abs, rules);
        assert_eq!(
            tree.num_nodes(),
            1_154_822,
            "first_small × WIDTH_REDIRECT=3 节点数偏离 probe 实测 1,154,822"
        );
    }

    #[test]
    fn root_is_sb_preflop_with_fold_call_raise_allin() {
        let tree = build_default_tree();
        let root = tree.node(tree.root_id());
        assert_eq!(root.street, StreetTag::Preflop);
        assert_eq!(root.player_acting, 0, "SB acts first preflop in HU");
        assert!(root.parent.is_none());
        assert!(root.action_from_parent.is_none());
        let tags: HashSet<_> = root.legal_actions.iter().copied().collect();
        assert!(tags.contains(&AbstractActionTag::Fold));
        assert!(tags.contains(&AbstractActionTag::Call));
        // Raise + AllIn must be present preflop facing BB blind.
        assert!(tags
            .iter()
            .any(|t| matches!(t, AbstractActionTag::Raise(_))));
        assert!(tags.contains(&AbstractActionTag::AllIn));
    }

    #[test]
    fn parent_chain_walks_back_to_root() {
        let tree = build_default_tree();
        for id in 0..tree.num_nodes() as NodeId {
            let mut cur = id;
            let mut steps = 0;
            while let Some(parent) = tree.node(cur).parent {
                cur = parent;
                steps += 1;
                assert!(steps < 32, "parent chain length explosion at node {id}");
            }
            assert_eq!(
                cur,
                tree.root_id(),
                "node {id} parent chain must end at root"
            );
        }
    }

    #[test]
    fn children_indices_align_with_legal_actions() {
        let tree = build_default_tree();
        for id in 0..tree.num_nodes() as NodeId {
            let node = tree.node(id);
            assert_eq!(
                node.children.len(),
                node.legal_actions.len(),
                "node {id}: children len must equal legal_actions len"
            );
        }
    }

    #[test]
    fn distinct_paths_map_to_distinct_node_ids() {
        // Path-to-root 应当对 NodeId 是单射：不同节点 → 不同路径（Phase 1 核心
        // 不变量；Phase 3 才把这个推广到 InfoSetId 全枚举）。
        let tree = build_default_tree();
        let mut seen: HashMap<Vec<AbstractActionTag>, NodeId> = HashMap::new();
        for id in 0..tree.num_nodes() as NodeId {
            let path = tree.path_to_root(id);
            if let Some(prev) = seen.insert(path.clone(), id) {
                panic!("path collision: node {id} and node {prev} share path {path:?}");
            }
        }
        assert_eq!(seen.len(), tree.num_nodes());
    }

    #[test]
    fn root_has_no_terminal_children_when_only_fold_check_call_raise() {
        // Sanity: root 的 Fold 应当立即 Terminal（SB fold = BB 拿盲注）；
        // Call / Raise / AllIn 不立即 Terminal（除非 AllIn 直接走到 showdown）。
        let tree = build_default_tree();
        let root = tree.node(tree.root_id());
        for (action, child) in root.legal_actions.iter().zip(root.children.iter()) {
            match action {
                AbstractActionTag::Fold => {
                    assert_eq!(*child, Child::Terminal, "SB fold should end the hand");
                }
                AbstractActionTag::Call | AbstractActionTag::Raise(_) => {
                    assert!(
                        matches!(child, Child::Decision(_)),
                        "SB call/raise should pass to BB decision, got {child:?}"
                    );
                }
                _ => {}
            }
        }
    }
}
