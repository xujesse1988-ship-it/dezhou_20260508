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
    AbstractAction, ActionAbstraction, BetRatio, StreetActionAbstraction,
};
use crate::abstraction::info::StreetTag;
use crate::core::rng::ChaCha20Rng;
use crate::core::Street;
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
        tree.root_id = tree.walk(state, None, None, abs);
        tree
    }

    fn walk(
        &mut self,
        state: GameState,
        parent: Option<NodeId>,
        action_from_parent: Option<AbstractActionTag>,
        abs: &StreetActionAbstraction,
    ) -> NodeId {
        let actor = state
            .current_player()
            .expect("walk only invoked on Player nodes")
            .0 as PlayerId;
        let my_id = self.nodes.len() as NodeId;

        let legal_set = abs.abstract_actions(&state);
        let legal_actions: SmallVec<[AbstractActionTag; 8]> =
            legal_set.iter().map(AbstractActionTag::of).collect();

        // Placeholder for stable index; children backfilled after recursion.
        self.nodes.push(TreeNode {
            street: street_to_tag(state.street()),
            player_acting: actor,
            parent,
            action_from_parent,
            legal_actions: legal_actions.clone(),
            children: SmallVec::new(),
        });

        let mut children: SmallVec<[Child; 8]> = SmallVec::new();
        for action in legal_set.iter().copied() {
            let mut next_state = state.clone();
            next_state
                .apply(action.to_concrete())
                .expect("action abstraction must emit legal Actions for current state");
            let child = if next_state.is_terminal() {
                Child::Terminal
            } else {
                let next_action_tag = AbstractActionTag::of(&action);
                Child::Decision(self.walk(next_state, Some(my_id), Some(next_action_tag), abs))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn build_default_tree() -> PublicBettingTree {
        PublicBettingTree::build(&TableConfig::default_hu_200bb())
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
