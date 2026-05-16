//! Stage 4 F3 \[报告\] — dump trained blueprint preflop strategies for typical
//! hand classes × positions（一次性 instrumentation，§F3-revM 同型政策）。
//!
//! 用法：
//!
//! ```text
//! cargo run --release --bin dump_preflop_strategy -- \
//!     --checkpoint <stage4_first_usable_final.ckpt> \
//!     --bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin
//! ```
//!
//! 6-max NLHE 场景 × 12 hand class，每场景输出 14-action 概率表 + 顶 3 动作。
//!
//! **场景**（preflop only）：
//! 1. UTG first to act
//! 2. MP（UTG fold）
//! 3. CO（UTG/MP fold）
//! 4. BTN（UTG/MP/CO fold）
//! 5. SB（UTG/MP/CO/BTN fold — facing BB walk）
//! 6. BB facing BTN open（UTG/MP/CO fold + BTN raise to 250 + SB fold）

use std::path::PathBuf;
use std::sync::Arc;

use poker::abstraction::action_pluribus::PluribusAction;
use poker::core::rng::RngSource;
use poker::core::{Card, Rank, Suit};
use poker::training::game::{Game, NodeKind, PlayerId};
use poker::training::nlhe_6max::{NlheGame6, NlheGame6State};
use poker::training::trainer::Trainer;
use poker::training::EsMccfrTrainer;
use poker::BucketTable;

const HAND_LIST: &[(&str, &str)] = &[
    ("AA", "AsAh"),
    ("KK", "KsKh"),
    ("QQ", "QsQh"),
    ("JJ", "JsJh"),
    ("AKs", "AsKs"),
    ("AKo", "AsKh"),
    ("99", "9s9h"),
    ("ATo", "AsTh"),
    ("KQs", "KsQs"),
    ("JTs", "JsTs"),
    ("22", "2s2h"),
    ("72o", "7s2h"),
    ("32o", "3s2h"),
];

#[derive(Clone, Copy, Debug)]
struct Scenario {
    name: &'static str,
    target_seat: u8,
    /// 应用的 (actor_seat, action) 序列（从 game.root 起）让 current 落到 target_seat。
    actions: &'static [(u8, PluribusAction)],
}

/// Action order preflop（6-max button=0）：UTG=3 → MP=4 → CO=5 → BTN=0 → SB=1 → BB=2
const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "UTG-first-to-act",
        target_seat: 3,
        actions: &[],
    },
    Scenario {
        name: "MP-after-UTG-fold",
        target_seat: 4,
        actions: &[(3, PluribusAction::Fold)],
    },
    Scenario {
        name: "CO-after-UTG-MP-fold",
        target_seat: 5,
        actions: &[(3, PluribusAction::Fold), (4, PluribusAction::Fold)],
    },
    Scenario {
        name: "BTN-after-UTG-MP-CO-fold",
        target_seat: 0,
        actions: &[
            (3, PluribusAction::Fold),
            (4, PluribusAction::Fold),
            (5, PluribusAction::Fold),
        ],
    },
    Scenario {
        name: "SB-after-everyone-folds",
        target_seat: 1,
        actions: &[
            (3, PluribusAction::Fold),
            (4, PluribusAction::Fold),
            (5, PluribusAction::Fold),
            (0, PluribusAction::Fold),
        ],
    },
    Scenario {
        name: "BB-facing-BTN-open",
        target_seat: 2,
        actions: &[
            (3, PluribusAction::Fold),
            (4, PluribusAction::Fold),
            (5, PluribusAction::Fold),
            (0, PluribusAction::Raise2Pot), // BTN open ~ 2x pot raise (D-420 14-action)
            (1, PluribusAction::Fold),
        ],
    },
];

fn main() -> Result<(), String> {
    let mut ckpt: Option<PathBuf> = None;
    let mut table: Option<PathBuf> = None;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--checkpoint" => ckpt = Some(PathBuf::from(it.next().ok_or("--checkpoint missing")?)),
            "--bucket-table" => {
                table = Some(PathBuf::from(it.next().ok_or("--bucket-table missing")?))
            }
            other => return Err(format!("unrecognized flag: {other}")),
        }
    }
    let ckpt = ckpt.ok_or("--checkpoint required")?;
    let table_path = table.unwrap_or_else(|| {
        PathBuf::from("artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin")
    });

    let table_arc =
        Arc::new(BucketTable::open(&table_path).map_err(|e| format!("BucketTable::open: {e:?}"))?);
    let game =
        NlheGame6::new(Arc::clone(&table_arc)).map_err(|e| format!("NlheGame6::new: {e:?}"))?;
    let blueprint = EsMccfrTrainer::<NlheGame6>::load_checkpoint(&ckpt, game)
        .map_err(|e| format!("load_checkpoint: {e:?}"))?;
    eprintln!(
        "[dump_preflop_strategy] loaded checkpoint @ update {}",
        blueprint.update_count()
    );

    println!("# stage 4 first usable blueprint preflop strategy dump");
    println!("# checkpoint: {ckpt:?}");
    println!("# update_count: {}", blueprint.update_count());
    println!();

    for scenario in SCENARIOS {
        println!(
            "## scenario: {} (seat {})",
            scenario.name, scenario.target_seat
        );
        println!();
        println!("| hand | top action | top p | 2nd action | 2nd p | 3rd action | 3rd p | full distribution |");
        println!("|---|---|---|---|---|---|---|---|");
        for (hand_name, hole_str) in HAND_LIST {
            let hole =
                parse_hole(hole_str).ok_or_else(|| format!("parse_hole({hole_str}) failed"))?;
            let state = build_state_at_target(
                blueprint.game_ref(),
                scenario.target_seat,
                hole,
                scenario.actions,
            )
            .ok_or_else(|| format!("build_state {hand_name} {}", scenario.name))?;
            // sanity: actor must be target_seat
            let actor = match NlheGame6::current(&state) {
                NodeKind::Player(a) => a,
                other => {
                    println!("| {hand_name} | (state not Player: {other:?}) | | | | | | |");
                    continue;
                }
            };
            if actor != scenario.target_seat {
                println!(
                    "| {hand_name} | (current = seat {actor} ≠ target {}) | | | | | | |",
                    scenario.target_seat
                );
                continue;
            }
            let info = NlheGame6::info_set(&state, actor);
            let avg = blueprint.average_strategy_for_traverser(actor, &info);
            let legal = NlheGame6::legal_actions(&state);
            print_strategy_row(hand_name, &legal, &avg);
        }
        println!();
    }
    Ok(())
}

/// 解析 `"AsKh"` → `[Card; 2]`。
fn parse_hole(s: &str) -> Option<[Card; 2]> {
    if s.len() != 4 {
        return None;
    }
    let c1 = parse_card(&s[..2])?;
    let c2 = parse_card(&s[2..])?;
    Some([c1, c2])
}

fn parse_card(s: &str) -> Option<Card> {
    let bytes = s.as_bytes();
    if bytes.len() != 2 {
        return None;
    }
    let rank = match bytes[0] as char {
        '2' => Rank::Two,
        '3' => Rank::Three,
        '4' => Rank::Four,
        '5' => Rank::Five,
        '6' => Rank::Six,
        '7' => Rank::Seven,
        '8' => Rank::Eight,
        '9' => Rank::Nine,
        'T' => Rank::Ten,
        'J' => Rank::Jack,
        'Q' => Rank::Queen,
        'K' => Rank::King,
        'A' => Rank::Ace,
        _ => return None,
    };
    let suit = match bytes[1] as char {
        'c' => Suit::Clubs,
        'd' => Suit::Diamonds,
        'h' => Suit::Hearts,
        's' => Suit::Spades,
        _ => return None,
    };
    Some(Card::new(rank, suit))
}

/// 6-max NlheGame6（button=0）dealing：deal_order = `[SB(1), BB(2), UTG(3), MP(4), CO(5), BTN(0)]`
/// k=0..5, seat ↔ `deck[k] + deck[6+k]`。
/// `our_seat → k` 映射：seat 0 → k=5；seat 1..5 → k = seat - 1。
fn seat_to_k(seat: u8) -> usize {
    if seat == 0 {
        5
    } else {
        (seat - 1) as usize
    }
}

/// 用 stacked deck 构造让 `our_seat` 持 `our_hole` 的 NlheGame6State，apply 给定
/// preflop 序列让 current 落到 `target_seat`（caller 责任 actions 序列正确）。
fn build_state_at_target(
    game: &NlheGame6,
    target_seat: u8,
    our_hole: [Card; 2],
    actions: &[(u8, PluribusAction)],
) -> Option<NlheGame6State> {
    let k = seat_to_k(target_seat);
    let mut target = [u8::MAX; 52];
    target[k] = our_hole[0].to_u8();
    target[k + 6] = our_hole[1].to_u8();
    let mut used = [false; 52];
    if our_hole[0].to_u8() == our_hole[1].to_u8() {
        return None;
    }
    used[our_hole[0].to_u8() as usize] = true;
    used[our_hole[1].to_u8() as usize] = true;
    let mut next_unused: u8 = 0;
    for slot in target.iter_mut() {
        if *slot == u8::MAX {
            while next_unused < 52 && used[next_unused as usize] {
                next_unused += 1;
            }
            if next_unused >= 52 {
                return None;
            }
            *slot = next_unused;
            used[next_unused as usize] = true;
            next_unused += 1;
        }
    }
    let mut rng = StackedDeckRng::from_target_u8(target);
    let mut state = game.root(&mut rng);
    let mut dummy_rng = NoopRng;
    for (expected_actor, action) in actions {
        let actor = match NlheGame6::current(&state) {
            NodeKind::Player(a) => a,
            _ => return None,
        };
        if actor != *expected_actor {
            eprintln!(
                "[build_state] expected actor {expected_actor} but got {actor} (target_seat={target_seat})"
            );
            return None;
        }
        state = NlheGame6::next(state, *action, &mut dummy_rng);
    }
    Some(state)
}

fn print_strategy_row(hand_name: &str, legal: &[PluribusAction], avg: &[f64]) {
    if avg.is_empty() || avg.len() != legal.len() {
        println!(
            "| {hand_name} | (no policy: avg.len={} legal.len={}) | | | | | | |",
            avg.len(),
            legal.len()
        );
        return;
    }
    let mut indexed: Vec<(usize, f64)> = avg.iter().enumerate().map(|(i, &p)| (i, p)).collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top = indexed.iter().take(3).collect::<Vec<_>>();
    let action_label = |idx: usize| action_short(legal[idx]);
    let full: Vec<String> = legal
        .iter()
        .zip(avg.iter())
        .map(|(a, p)| format!("{}={:.3}", action_short(*a), p))
        .collect();
    println!(
        "| {} | {} | {:.3} | {} | {:.3} | {} | {:.3} | {} |",
        hand_name,
        action_label(top[0].0),
        top[0].1,
        top.get(1)
            .map(|(i, _)| action_label(*i))
            .unwrap_or("-".to_string()),
        top.get(1).map(|(_, p)| *p).unwrap_or(0.0),
        top.get(2)
            .map(|(i, _)| action_label(*i))
            .unwrap_or("-".to_string()),
        top.get(2).map(|(_, p)| *p).unwrap_or(0.0),
        full.join(" "),
    );
}

fn action_short(a: PluribusAction) -> String {
    match a {
        PluribusAction::Fold => "F".to_string(),
        PluribusAction::Check => "K".to_string(),
        PluribusAction::Call => "C".to_string(),
        PluribusAction::AllIn => "A".to_string(),
        PluribusAction::Raise05Pot => "R0.5".to_string(),
        PluribusAction::Raise075Pot => "R0.75".to_string(),
        PluribusAction::Raise1Pot => "R1".to_string(),
        PluribusAction::Raise15Pot => "R1.5".to_string(),
        PluribusAction::Raise2Pot => "R2".to_string(),
        PluribusAction::Raise3Pot => "R3".to_string(),
        PluribusAction::Raise5Pot => "R5".to_string(),
        PluribusAction::Raise10Pot => "R10".to_string(),
        PluribusAction::Raise25Pot => "R25".to_string(),
        PluribusAction::Raise50Pot => "R50".to_string(),
    }
}

// === StackedDeckRng + NoopRng 私有 helpers（同型 slumbot_eval.rs 副本）===

struct StackedDeckRng {
    sequence: Vec<u64>,
    cursor: usize,
}

impl StackedDeckRng {
    fn from_target_u8(target: [u8; 52]) -> StackedDeckRng {
        let mut seen = [false; 52];
        for &v in &target {
            assert!(v < 52);
            assert!(!seen[v as usize]);
            seen[v as usize] = true;
        }
        let mut deck: Vec<u8> = (0..52).collect();
        let mut sequence = Vec::with_capacity(51);
        for i in 0..51 {
            let want = target[i];
            let pos = deck[i..]
                .iter()
                .position(|&c| c == want)
                .map(|p| p + i)
                .expect("StackedDeckRng: target card already locked");
            sequence.push((pos - i) as u64);
            deck.swap(i, pos);
        }
        StackedDeckRng {
            sequence,
            cursor: 0,
        }
    }
}

impl RngSource for StackedDeckRng {
    fn next_u64(&mut self) -> u64 {
        let v = self.sequence[self.cursor];
        self.cursor += 1;
        v
    }
}

/// Dummy RNG — NlheGame6::next 后 dealing 已完成，no chance node 路径上 next
/// 不消费 RNG（D-028 字面）。但 trait 需要 &mut dyn RngSource 占位。
struct NoopRng;

impl RngSource for NoopRng {
    fn next_u64(&mut self) -> u64 {
        0
    }
}

// silence unused warnings
#[allow(dead_code)]
fn _unused(_: PlayerId) {}
