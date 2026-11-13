//! Temporary: how much the dense tables grow when non-blind seats may open-limp
//! (`no_open_limp` off), without materializing the tree (`count_tree_layout`).
//!
//! usage: tmp_limp_layout <seats> <stack_bb> <ante_chips> <postflop_cap> <reshape>...
//! Prints, per reshape, the tree as trained today (limp forbidden) and with limps allowed.

use poker::training::nlhe_betting_tree::{count_tree_layout, six_max_reshape, TreeLayoutCounts};
use poker::{ChipAmount, TableConfig};

const BUCKETS: [u64; 4] = [169, 500, 500, 500];
const GIB: f64 = (1u64 << 30) as f64;

fn gib(c: &TreeLayoutCounts) -> f64 {
    let slots: u64 = (0..4).map(|i| c.actions_by_street[i] * BUCKETS[i]).sum();
    (slots * 16) as f64 / GIB
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let usage =
        "usage: tmp_limp_layout <seats> <stack_bb> <ante_chips> <postflop_cap> <reshape>...";
    let seats: u8 = args.get(1).expect(usage).parse().unwrap();
    let stack_bb: u32 = args.get(2).expect(usage).parse().unwrap();
    let ante: u64 = args.get(3).expect(usage).parse().unwrap();
    let cap: u8 = args.get(4).expect(usage).parse().unwrap();
    let cfg = TableConfig::n_max_at_bb(seats, stack_bb).with_ante(ChipAmount::new(ante));
    for name in &args[5..] {
        let (abs, rules) = six_max_reshape(name, cap).expect("unknown reshape");
        for limp in [false, true] {
            if limp && !rules.no_open_limp {
                continue; // profile already allows limps: the first pass measured it
            }
            let mut r = rules;
            if rules.no_open_limp {
                r.no_open_limp = !limp;
            }
            let t0 = std::time::Instant::now();
            let c = count_tree_layout(&cfg, &abs, r);
            println!(
                "{seats} seats {stack_bb}BB cap {cap} {name} limp={}: nodes {:?} total {} two tables {:.2} GiB ({:.0}s)",
                !r.no_open_limp,
                c.nodes_by_street,
                c.total_nodes(),
                gib(&c),
                t0.elapsed().as_secs_f64()
            );
        }
    }
}
