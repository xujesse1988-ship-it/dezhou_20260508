//! Temporary: how graded are preflop blueprint average strategies, and where does an
//! exponential tilt put extra mass when widening an action?
//! (`docs/temp/preflop_range_offanchor_research_2026_10_29.md` §4.1 review.)
//! Helpers copied from `tmp_ante_preflop_view.rs`.
//!
//! usage: tmp_preflop_sigma_grade <crop.zst> [reshape=ante-flop2-3bet] [stack_bb=200]

use std::io::{BufReader, Read};
use std::process::{Command, Stdio};

use poker::training::nlhe_betting_tree::{
    six_max_reshape, AbstractActionTag, Child, NodeId, PublicBettingTree,
};
use poker::{BetRatio, Card, ChipAmount, PreflopLossless169, Rank, StreetTag, Suit, TableConfig};

const HEADER_LEN: usize = 152;

fn parse_tag(s: &str) -> AbstractActionTag {
    match s {
        "F" => AbstractActionTag::Fold,
        "C" => AbstractActionTag::Call,
        "A" => AbstractActionTag::AllIn,
        "R" => AbstractActionTag::Raise(BetRatio::FULL_POT),
        "H" => AbstractActionTag::Raise(BetRatio::HALF_POT),
        "O" => AbstractActionTag::Raise(BetRatio::from_f64(1.0 / 3.0).unwrap()),
        other => panic!("bad tag {other}"),
    }
}

/// 169 canonical hands: (label, bucket, combos).
fn hands() -> Vec<(String, usize, f64)> {
    use Rank::*;
    let ranks = [
        Ace, King, Queen, Jack, Ten, Nine, Eight, Seven, Six, Five, Four, Three, Two,
    ];
    let ch = [
        'A', 'K', 'Q', 'J', 'T', '9', '8', '7', '6', '5', '4', '3', '2',
    ];
    let p = PreflopLossless169::new();
    let mut out = Vec::new();
    for i in 0..13 {
        for j in i..13 {
            if i == j {
                let h = [
                    Card::new(ranks[i], Suit::Spades),
                    Card::new(ranks[i], Suit::Hearts),
                ];
                out.push((format!("{}{}", ch[i], ch[i]), p.hand_class(h) as usize, 6.0));
            } else {
                let s = [
                    Card::new(ranks[i], Suit::Spades),
                    Card::new(ranks[j], Suit::Spades),
                ];
                let o = [
                    Card::new(ranks[i], Suit::Spades),
                    Card::new(ranks[j], Suit::Hearts),
                ];
                out.push((
                    format!("{}{}s", ch[i], ch[j]),
                    p.hand_class(s) as usize,
                    4.0,
                ));
                out.push((
                    format!("{}{}o", ch[i], ch[j]),
                    p.hand_class(o) as usize,
                    12.0,
                ));
            }
        }
    }
    assert_eq!(out.len(), 169);
    out
}

struct Spot {
    name: String,
    /// (node, action index taken) along the path, root first; last entry = the spot node.
    path: Vec<(NodeId, Option<usize>)>,
}

fn walk_path(tree: &PublicBettingTree, toks: &[&str]) -> Vec<(NodeId, Option<usize>)> {
    let mut node = tree.root_id();
    let mut path = Vec::new();
    for t in toks {
        let tag = parse_tag(t);
        let n = tree.node(node);
        let idx = n
            .legal_actions
            .iter()
            .position(|x| *x == tag)
            .unwrap_or_else(|| panic!("no edge {t} at node {node}: {:?}", n.legal_actions));
        path.push((node, Some(idx)));
        node = match n.children[idx] {
            Child::Decision(id) => id,
            Child::Terminal => panic!("terminal at {t}"),
        };
    }
    path.push((node, None));
    path
}

/// Per-node extracted data: strategy sums (169 × ac) and touched flags.
struct NodeData {
    ac: usize,
    sums: Vec<f64>,
    touched: Vec<bool>,
}

struct Layout {
    /// node -> (row_base, slot_base) in the kept space.
    base: Vec<Option<(u64, u64)>>,
    kept_nodes: u64,
    kept_rows: u64,
    kept_slots: u64,
    src_rows: u64,
    src_slots: u64,
}

fn layout(tree: &PublicBettingTree) -> Layout {
    let buckets = [169u64, 500, 500, 500];
    let mut base = Vec::with_capacity(tree.num_nodes());
    let (mut kn, mut kr, mut ks, mut sr, mut ss) = (0, 0, 0, 0, 0);
    for id in 0..tree.num_nodes() as NodeId {
        let n = tree.node(id);
        let b = buckets[n.street as usize];
        let ac = n.legal_actions.len() as u64;
        sr += b;
        ss += b * ac;
        if n.street as u8 <= StreetTag::Flop as u8 {
            base.push(Some((kr, ks)));
            kn += 1;
            kr += b;
            ks += b * ac;
        } else {
            base.push(None);
        }
    }
    Layout {
        base,
        kept_nodes: kn,
        kept_rows: kr,
        kept_slots: ks,
        src_rows: sr,
        src_slots: ss,
    }
}

struct HashedReader<R: Read> {
    r: R,
    h: blake3::Hasher,
}

impl<R: Read> HashedReader<R> {
    fn exact(&mut self, buf: &mut [u8]) {
        self.r.read_exact(buf).expect("short read");
        self.h.update(buf);
    }
}

fn extract(
    path: &str,
    tree: &PublicBettingTree,
    lay: &Layout,
    nodes: &[NodeId],
) -> (u64, Vec<NodeData>) {
    let mut child = Command::new("zstd")
        .args(["-dc", path])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn zstd");
    let mut r = HashedReader {
        r: BufReader::with_capacity(8 << 20, child.stdout.take().unwrap()),
        h: blake3::Hasher::new(),
    };
    let mut hdr = [0u8; HEADER_LEN];
    r.exact(&mut hdr);
    assert_eq!(&hdr[0..8], b"PLDNCROP", "{path}: magic");
    let rd = |o: usize| u64::from_le_bytes(hdr[o..o + 8].try_into().unwrap());
    assert_eq!(hdr[12], StreetTag::Flop as u8, "{path}: keep_through");
    let has_regret = hdr[13] == 1;
    let update_count = rd(16);
    assert_eq!(
        (rd(40), rd(48), rd(56)),
        (tree.num_nodes() as u64, lay.src_rows, lay.src_slots),
        "{path}: source layout mismatch"
    );
    assert_eq!(
        (rd(64), rd(72), rd(80)),
        (lay.kept_nodes, lay.kept_rows, lay.kept_slots),
        "{path}: kept layout mismatch"
    );

    // Two touched bitsets; a row counts as visited if either bit is set (CroppedBlueprint rule).
    let n_words = lay.kept_rows.div_ceil(64) as usize;
    let mut touched_any = vec![0u64; n_words];
    let mut buf = vec![0u8; 1 << 20];
    for _ in 0..2 {
        let mut w = 0usize;
        while w < n_words {
            let k = (n_words - w).min(buf.len() / 8);
            r.exact(&mut buf[..k * 8]);
            for (i, b) in buf[..k * 8].chunks_exact(8).enumerate() {
                touched_any[w + i] |= u64::from_le_bytes(b.try_into().unwrap());
            }
            w += k;
        }
    }

    // Wanted slot ranges, sorted by slot offset.
    let mut want: Vec<(u64, u64, usize)> = nodes
        .iter()
        .enumerate()
        .map(|(i, &id)| {
            let (_, sb) = lay.base[id as usize].expect("preflop node kept");
            let ac = tree.node(id).legal_actions.len() as u64;
            (sb, sb + 169 * ac, i)
        })
        .collect();
    want.sort();
    let mut out: Vec<NodeData> = nodes
        .iter()
        .map(|&id| {
            let ac = tree.node(id).legal_actions.len();
            let (rb, _) = lay.base[id as usize].unwrap();
            let touched = (0..169u64)
                .map(|b| {
                    let row = rb + b;
                    (touched_any[(row / 64) as usize] >> (row % 64)) & 1 == 1
                })
                .collect();
            NodeData {
                ac,
                sums: vec![0.0; 169 * ac],
                touched,
            }
        })
        .collect();
    drop(touched_any);

    let skip_section = |r: &mut HashedReader<_>, mut n: u64, buf: &mut [u8]| {
        while n > 0 {
            let k = n.min(buf.len() as u64) as usize;
            r.exact(&mut buf[..k]);
            n -= k as u64;
        }
    };
    if has_regret {
        skip_section(&mut r, lay.kept_slots * 8, &mut buf);
    }
    // Stream the strategy section slot by slot in 1 MiB chunks.
    let mut slot = 0u64;
    let mut wi = 0usize;
    while slot < lay.kept_slots {
        let k = (lay.kept_slots - slot).min((buf.len() / 8) as u64);
        r.exact(&mut buf[..(k * 8) as usize]);
        let (lo, hi) = (slot, slot + k);
        let mut j = wi;
        while j < want.len() && want[j].0 < hi {
            let (s0, s1, i) = want[j];
            let a = s0.max(lo);
            let b = s1.min(hi);
            for s in a..b {
                let off = ((s - lo) * 8) as usize;
                out[i].sums[(s - s0) as usize] =
                    f64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
            }
            j += 1;
        }
        while wi < want.len() && want[wi].1 <= hi {
            wi += 1;
        }
        slot = hi;
    }
    let digest: [u8; 32] = r.h.finalize().into();
    let mut trailer = [0u8; 32];
    r.r.read_exact(&mut trailer).expect("trailer");
    assert_eq!(digest, trailer, "{path}: BLAKE3 trailer mismatch");
    let mut extra = [0u8; 1];
    assert_eq!(r.r.read(&mut extra).unwrap(), 0, "{path}: trailing bytes");
    assert!(child.wait().unwrap().success(), "{path}: zstd failed");
    (update_count, out)
}

/// Average strategy for one bucket (None = unvisited row).
fn avg(d: &NodeData, b: usize) -> Option<Vec<f64>> {
    if !d.touched[b] {
        return None;
    }
    let s = &d.sums[b * d.ac..(b + 1) * d.ac];
    let t: f64 = s.iter().sum();
    Some(if t > 0.0 {
        s.iter().map(|x| x / t).collect()
    } else {
        vec![1.0 / d.ac as f64; d.ac]
    })
}

/// Membership test for an action group (e.g. "all raise sizes + all-in").
type TagPred = fn(&AbstractActionTag) -> bool;

fn chen_of(label: &str) -> f64 {
    let rank = |c: char| -> Rank {
        use Rank::*;
        match c {
            'A' => Ace,
            'K' => King,
            'Q' => Queen,
            'J' => Jack,
            'T' => Ten,
            '9' => Nine,
            '8' => Eight,
            '7' => Seven,
            '6' => Six,
            '5' => Five,
            '4' => Four,
            '3' => Three,
            _ => Two,
        }
    };
    let c: Vec<char> = label.chars().collect();
    let s2 = if label.ends_with('s') {
        Suit::Spades
    } else {
        Suit::Hearts
    };
    poker::training::subgame::chen_score([
        Card::new(rank(c[0]), Suit::Spades),
        Card::new(rank(c[1]), s2),
    ])
}

/// Spearman rank correlation with combo weights (average ranks for ties).
fn spearman(xs: &[f64], ys: &[f64], ws: &[f64]) -> f64 {
    let ranks = |v: &[f64]| -> Vec<f64> {
        let mut idx: Vec<usize> = (0..v.len()).collect();
        idx.sort_by(|&a, &b| v[a].partial_cmp(&v[b]).unwrap());
        let mut r = vec![0.0; v.len()];
        let mut i = 0;
        while i < idx.len() {
            let mut j = i;
            while j + 1 < idx.len() && v[idx[j + 1]] == v[idx[i]] {
                j += 1;
            }
            let avg = (i + j) as f64 / 2.0;
            for k in i..=j {
                r[idx[k]] = avg;
            }
            i = j + 1;
        }
        r
    };
    let (rx, ry) = (ranks(xs), ranks(ys));
    let tw: f64 = ws.iter().sum();
    let mx = rx.iter().zip(ws).map(|(a, w)| a * w).sum::<f64>() / tw;
    let my = ry.iter().zip(ws).map(|(a, w)| a * w).sum::<f64>() / tw;
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for i in 0..xs.len() {
        let (dx, dy) = (rx[i] - mx, ry[i] - my);
        sxy += ws[i] * dx * dy;
        sxx += ws[i] * dx * dx;
        syy += ws[i] * dy * dy;
    }
    if sxx == 0.0 || syy == 0.0 {
        return f64::NAN;
    }
    sxy / (sxx * syy).sqrt()
}

/// Binary tilt p' = p·e^θ / (p·e^θ + 1 − p); θ by bisection so that Σ w·p' / Σ w = target.
fn tilt(ps: &[f64], ws: &[f64], target: f64) -> Vec<f64> {
    let tw: f64 = ws.iter().sum();
    let apply = |th: f64| -> Vec<f64> {
        ps.iter()
            .map(|&p| {
                let a = p * th.exp();
                if a + (1.0 - p) > 0.0 {
                    a / (a + 1.0 - p)
                } else {
                    0.0
                }
            })
            .collect()
    };
    let (mut lo, mut hi) = (-60.0f64, 60.0f64);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        let f = apply(mid).iter().zip(ws).map(|(p, w)| p * w).sum::<f64>() / tw;
        if f < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    apply(0.5 * (lo + hi))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .expect("usage: tmp_preflop_sigma_grade <crop.zst> [reshape] [stack_bb]")
        .clone();
    let reshape = args.get(2).map_or("ante-flop2-3bet", |s| s.as_str());
    let stack_bb: u32 = args.get(3).map_or(200, |s| s.parse().expect("stack_bb"));
    let (abs, rules) = six_max_reshape(reshape, 4).unwrap();
    let cfg = TableConfig::n_max_at_bb(8, stack_bb).with_ante(ChipAmount::new(100));
    let tree = PublicBettingTree::build_with_rules(&cfg, &abs, rules);
    let lay = layout(&tree);
    eprintln!("tree nodes {} kept {}", tree.num_nodes(), lay.kept_nodes);

    let f = |n: usize| vec!["F"; n];
    let cat = |parts: &[Vec<&'static str>]| parts.concat();
    let mut defs: Vec<(String, Vec<&'static str>)> = Vec::new();
    for k in 0..7 {
        let seat = ["UTG", "UTG1", "LJ", "HJ", "CO", "BTN", "SB"][k];
        defs.push((format!("{seat} 首入"), f(k)));
    }
    defs.push(("BB vs SB limp".into(), cat(&[f(6), vec!["C"]])));
    defs.push(("BB vs UTG 开池".into(), cat(&[vec!["O"], f(6)])));
    defs.push(("BB vs BTN 开池".into(), cat(&[f(5), vec!["O", "F"]])));
    defs.push(("BB vs SB 开池".into(), cat(&[f(6), vec!["O"]])));
    defs.push(("BTN vs CO 开池".into(), cat(&[f(4), vec!["O"]])));
    defs.push(("SB vs BTN 开池".into(), cat(&[f(5), vec!["O"]])));
    defs.push((
        "UTG 开 vs BB 3bet".into(),
        cat(&[vec!["O"], f(6), vec!["R"]]),
    ));
    defs.push((
        "BTN 开 vs BB 3bet".into(),
        cat(&[f(5), vec!["O", "F", "R"]]),
    ));
    defs.push((
        "BTN 开 vs SB 3bet".into(),
        cat(&[f(5), vec!["O", "R", "F"]]),
    ));
    if reshape == "ante-flop2-3bet" {
        defs.push((
            "BTN 开 vs BB 半池 3bet".into(),
            cat(&[f(5), vec!["O", "F", "H"]]),
        ));
        defs.push((
            "BB 半池 3bet vs BTN 4bet".into(),
            cat(&[f(5), vec!["O", "F", "H", "R"]]),
        ));
    }
    let spots: Vec<Spot> = defs
        .iter()
        .map(|(n, t)| Spot {
            name: n.clone(),
            path: walk_path(&tree, t),
        })
        .collect();
    let mut nodes: Vec<NodeId> = spots
        .iter()
        .flat_map(|s| s.path.iter().map(|(n, _)| *n))
        .collect();
    nodes.sort();
    nodes.dedup();
    let idx_of = |id: NodeId| nodes.binary_search(&id).unwrap();
    let (u, data) = extract(&path, &tree, &lay, &nodes);
    eprintln!("ok: update_count {u}");
    let hs = hands();

    println!("# 翻前 σ 渐变度（8 座 ante {stack_bb}BB {reshape}，{u} 次）\n");
    println!("口径：按组合数 × 本座到达概率加权；p = 该动作组（所有加注尺寸 + 全下合并为 raise）的概率。");
    println!("「中间带」= p∈[2%,98%]；「低区」= p<2%。ρ = 低区内 p 与 Chen 分的 Spearman 秩相关。");
    println!("放宽测试：把该动作频率 f 加到 f+min(15pt, (1−f)/2)，看新增质量里落到「低区中 Chen 前 1/3」的比例");
    println!("（均匀撒 ≈ 33%）；ε=0 用原始 σ，ε=0.02 先做 (1−ε)σ+ε/|A|。\n");
    println!("| 局面 | 动作 | f | 中间带 | 低区 | 低区 p<1e-4 | 1e-4–1e-3 | 1e-3–2e-2 | ρ(低区) | 新增→Chen前1/3 ε=0 | ε=0.02 |");
    println!("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for s in &spots {
        let (spot, _) = *s.path.last().unwrap();
        let tn = tree.node(spot);
        let actor = tn.player_acting;
        let reach = |b: usize| -> f64 {
            let mut w = 1.0;
            for (n, a) in &s.path[..s.path.len() - 1] {
                if tree.node(*n).player_acting == actor {
                    w *= avg(&data[idx_of(*n)], b).map_or(0.0, |p| p[a.unwrap()]);
                }
            }
            w
        };
        let n_act = tn.legal_actions.len() as f64;
        let groups: [(&str, TagPred); 2] = [
            ("raise", |t| {
                matches!(t, AbstractActionTag::Raise(_) | AbstractActionTag::AllIn)
            }),
            ("call", |t| matches!(t, AbstractActionTag::Call)),
        ];
        for (gname, pred) in groups {
            if !tn.legal_actions.iter().any(pred) {
                continue;
            }
            let (mut ps, mut ws, mut chens) = (Vec::new(), Vec::new(), Vec::new());
            for (label, b, combos) in &hs {
                let r = reach(*b);
                let Some(p) = avg(&data[idx_of(spot)], *b) else {
                    continue;
                };
                if r <= 0.0 {
                    continue;
                }
                let pg: f64 = tn
                    .legal_actions
                    .iter()
                    .zip(&p)
                    .filter(|(t, _)| pred(t))
                    .map(|(_, x)| x)
                    .sum();
                ps.push(pg);
                ws.push(combos * r);
                chens.push(chen_of(label));
            }
            let tw: f64 = ws.iter().sum();
            let share = |pr: &dyn Fn(f64) -> bool| -> f64 {
                ps.iter()
                    .zip(&ws)
                    .filter(|(p, _)| pr(**p))
                    .map(|(_, w)| w)
                    .sum::<f64>()
                    / tw
            };
            let fq = ps.iter().zip(&ws).map(|(p, w)| p * w).sum::<f64>() / tw;
            let mid = share(&|p| (0.02..=0.98).contains(&p));
            let low = share(&|p| p < 0.02);
            let l1 = share(&|p| p < 1e-4);
            let l2 = share(&|p| (1e-4..1e-3).contains(&p));
            let l3 = share(&|p| (1e-3..0.02).contains(&p));
            let low_ix: Vec<usize> = (0..ps.len()).filter(|&i| ps[i] < 0.02).collect();
            let rho = spearman(
                &low_ix.iter().map(|&i| ps[i]).collect::<Vec<_>>(),
                &low_ix.iter().map(|&i| chens[i]).collect::<Vec<_>>(),
                &low_ix.iter().map(|&i| ws[i]).collect::<Vec<_>>(),
            );
            // Top third (by combo weight) of the low region by Chen.
            let mut ord = low_ix.clone();
            ord.sort_by(|&a, &b| chens[b].partial_cmp(&chens[a]).unwrap());
            let low_w: f64 = low_ix.iter().map(|&i| ws[i]).sum();
            let mut top = vec![false; ps.len()];
            let mut acc = 0.0;
            for &i in &ord {
                if acc >= low_w / 3.0 {
                    break;
                }
                top[i] = true;
                acc += ws[i];
            }
            let target = fq + (0.15f64).min((1.0 - fq) / 2.0);
            let added_top = |eps: f64| -> f64 {
                let base: Vec<f64> = ps.iter().map(|p| (1.0 - eps) * p + eps / n_act).collect();
                let t = tilt(&base, &ws, target);
                let (mut add_all, mut add_top) = (0.0, 0.0);
                for &i in &low_ix {
                    let d = ws[i] * (t[i] - base[i]).max(0.0);
                    add_all += d;
                    if top[i] {
                        add_top += d;
                    }
                }
                if add_all > 0.0 {
                    add_top / add_all
                } else {
                    f64::NAN
                }
            };
            let pct = |x: f64| format!("{:.1}%", 100.0 * x);
            println!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {:.2} | {} | {} |",
                s.name,
                gname,
                pct(fq),
                pct(mid),
                pct(low),
                pct(l1),
                pct(l2),
                pct(l3),
                rho,
                pct(added_top(0.0)),
                pct(added_top(0.02)),
            );
        }
    }
    println!(
        "\n（新增→Chen前1/3 的均匀基线 ≈ 33%：低区里 Chen 前 1/3 的组合占比按构造约为 1/3。）"
    );
}
