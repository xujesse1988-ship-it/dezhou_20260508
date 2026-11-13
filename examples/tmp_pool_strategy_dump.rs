//! Temporary: print what the pool's preflop strategy looks like at a few spots of the
//! 8-seat 200BB `ante-flop2-3bet-o75-limp` tree (cap 2): target frequencies and some hands.
//!
//! usage: tmp_pool_strategy_dump <bucket table> <pool_model.json> [noise]

use std::sync::Arc;

use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_betting_tree::{six_max_reshape, AbstractActionTag as T, Child, NodeId};
use poker::training::pool_model::PoolModel;
use poker::training::pool_strategy::{PoolPreflopStrategy, PoolStrategyParams};
use poker::{BucketTable, ChipAmount, TableConfig};

const HANDS: [(&str, u32); 9] = [
    ("AA", 12),
    ("AKo", 168),
    ("99", 7),
    ("KQo", 156),
    ("A8o", 163),
    ("JTs", 57),
    ("76s", 27),
    ("22", 0),
    ("72o", 101),
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let table = Arc::new(BucketTable::open(std::path::Path::new(&args[1])).expect("bucket table"));
    let model = PoolModel::load(std::path::Path::new(&args[2])).expect("model");
    let noise: f64 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(1.0);
    let (abs, rules) = six_max_reshape("ante-flop2-3bet-o75-limp", 2).unwrap();
    let cfg = TableConfig::n_max_at_bb(8, 200).with_ante(ChipAmount::new(100));
    let game = SimplifiedNlheGame::new_with_abstraction(table, cfg, abs, rules).expect("game");
    let params = PoolStrategyParams {
        noise,
        ..PoolStrategyParams::default()
    };
    let pool = PoolPreflopStrategy::build(&game, &model, params).expect("pool");
    let tree = game.tree();
    let step = |id: NodeId, tag: T| -> NodeId {
        let n = tree.node(id);
        let i = n
            .legal_actions
            .iter()
            .position(|a| *a == tag)
            .unwrap_or_else(|| panic!("node {id}: no {tag:?} in {:?}", n.legal_actions));
        match n.children[i] {
            Child::Decision(c) => c,
            Child::Terminal => panic!("terminal"),
        }
    };
    let raise_of = |id: NodeId| -> T {
        *tree
            .node(id)
            .legal_actions
            .iter()
            .find(|a| matches!(a, T::Raise(_)))
            .expect("a raise")
    };
    let folds = |mut id: NodeId, n: usize| -> NodeId {
        for _ in 0..n {
            id = step(id, T::Fold);
        }
        id
    };
    let show = |label: &str, id: NodeId| {
        let info = pool.nodes().iter().find(|i| i.node == id).expect("info");
        let node = tree.node(id);
        println!(
            "\n## {label}\nseat {} {}  n={} confidence {:.2}  own reach {:.3}\nactions {:?}\ntarget  {:?}",
            info.seat,
            info.family,
            info.n,
            info.confidence,
            info.own_reach,
            node.legal_actions,
            info.target.iter().map(|x| format!("{x:.3}")).collect::<Vec<_>>()
        );
        // Share of each strength group in every action's range (weights = combos; only exact for
        // nodes the seat reaches with its whole range, own reach 1).
        let group = |c: usize| -> usize {
            if c < 13 {
                return if c >= 8 { 0 } else { 1 };
            }
            let idx = if c < 91 { c - 13 } else { c - 91 };
            let mut high = 1;
            while (high + 1) * high / 2 <= idx {
                high += 1;
            }
            let low = idx - high * (high - 1) / 2;
            if high == 12 && low >= 10 {
                0
            } else if low >= 8 {
                2
            } else {
                3
            }
        };
        let mut comp = vec![[0.0f64; 4]; node.legal_actions.len()];
        for c in 0..169usize {
            let w = if c < 13 {
                6.0
            } else if c < 91 {
                4.0
            } else {
                12.0
            } / 1326.0;
            let (_, row) = pool.lookup(id, c as u32).expect("row");
            for (a, p) in row.iter().enumerate() {
                comp[a][group(c)] += w * f64::from(*p);
            }
        }
        for (a, g) in comp.iter().enumerate() {
            let t: f64 = g.iter().sum();
            println!(
                "  range of {:?}: strong {:.0}% pairs {:.0}% broadway {:.0}% other {:.0}%",
                node.legal_actions[a],
                100.0 * g[0] / t,
                100.0 * g[1] / t,
                100.0 * g[2] / t,
                100.0 * g[3] / t
            );
        }
        for (name, class) in HANDS {
            let (_, row) = pool.lookup(id, class).expect("row");
            println!(
                "  {name:4} {}",
                row.iter()
                    .map(|p| format!("{p:.2}"))
                    .collect::<Vec<_>>()
                    .join("  ")
            );
        }
    };
    let root = tree.root_id();
    show("UTG, nobody in", root);
    show("BTN, folded to", folds(root, 5));
    let limp = step(root, T::Call);
    show("UTG+1 facing one limper", limp);
    let iso = step(limp, raise_of(limp));
    show("UTG+2 facing limp + raise", iso);
    let back = folds(iso, 6);
    show("limper facing the raise", back);
    let open = step(root, raise_of(root));
    show("UTG+1 facing an open", open);
    let bb = folds(open, 6);
    show("BB facing an open", bb);
    let three = step(open, raise_of(open));
    let opener = folds(three, 6);
    show("opener facing a 3bet", opener);
}
