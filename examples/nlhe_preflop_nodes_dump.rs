//! 一次性 dump 简化 NLHE betting tree 的前 N 个 preflop 决策节点。
//! 用法：`cargo run --release --example nlhe_preflop_nodes_dump -- [N]`（默认 50）。

use poker::training::nlhe_betting_tree::{AbstractActionTag, Child, PublicBettingTree, TreeNode};
use poker::{StreetTag, TableConfig};

fn tag_short(t: AbstractActionTag) -> String {
    match t {
        AbstractActionTag::Fold => "F".to_string(),
        AbstractActionTag::Check => "X".to_string(),
        AbstractActionTag::Call => "C".to_string(),
        AbstractActionTag::Bet(r) => format!("B({:?})", r),
        AbstractActionTag::Raise(r) => format!("R({:?})", r),
        AbstractActionTag::AllIn => "A".to_string(),
    }
}

fn fmt_path(tree: &PublicBettingTree, id: u32) -> String {
    let path = tree.path_to_root(id);
    if path.is_empty() {
        "(root)".to_string()
    } else {
        path.into_iter()
            .map(tag_short)
            .collect::<Vec<_>>()
            .join(" → ")
    }
}

fn fmt_legal(node: &TreeNode) -> String {
    node.legal_actions
        .iter()
        .copied()
        .map(tag_short)
        .collect::<Vec<_>>()
        .join(", ")
}

fn fmt_children(node: &TreeNode) -> String {
    node.children
        .iter()
        .map(|c| match c {
            Child::Terminal => "T".to_string(),
            Child::Decision(id) => format!("#{}", id),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);

    let tree = PublicBettingTree::build(&TableConfig::default_hu_100bb());
    let total = tree.num_nodes();

    let mut preflop_ids: Vec<u32> = (0..total as u32)
        .filter(|id| tree.node(*id).street == StreetTag::Preflop)
        .collect();
    preflop_ids.sort_unstable();

    println!(
        "total nodes = {}, preflop nodes = {}, preflop infosets = {} × 169 = {}",
        total,
        preflop_ids.len(),
        preflop_ids.len(),
        preflop_ids.len() * 169,
    );
    println!("showing first {}", n.min(preflop_ids.len()));
    println!();
    println!(
        "{:>5}  {:>6}  {:>5}  {:<45}  {:<35}  {}",
        "id", "parent", "actor", "path_from_root", "legal_actions", "children"
    );
    println!("{}", "-".repeat(130));

    for id in preflop_ids.iter().take(n) {
        let node = tree.node(*id);
        let parent = node
            .parent
            .map(|p| p.to_string())
            .unwrap_or_else(|| "—".to_string());
        println!(
            "{:>5}  {:>6}  p{}     {:<45}  {:<35}  {}",
            id,
            parent,
            node.player_acting,
            fmt_path(&tree, *id),
            fmt_legal(node),
            fmt_children(node),
        );
    }
}
