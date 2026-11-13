//! Temporary: all-in equity of each of the 169 preflop classes against one random hand
//! (Monte Carlo), printed as a Rust array indexed by `PreflopLossless169::hand_class`.
//!
//! usage: tmp_preflop_equity <samples per class>

use poker::eval::NaiveHandEvaluator;
use poker::{Card, ChaCha20Rng, HandEvaluator, PreflopLossless169, Rank, RngSource, Suit};

fn main() {
    let samples: u64 = std::env::args().nth(1).expect("samples").parse().unwrap();
    let abs = PreflopLossless169::new();
    // One representative hand per class.
    let mut rep: Vec<Option<[Card; 2]>> = vec![None; 169];
    for a in 0..52u8 {
        for b in (a + 1)..52u8 {
            let hole = [Card::from_u8(a).unwrap(), Card::from_u8(b).unwrap()];
            let c = abs.hand_class(hole) as usize;
            let canonical = hole[0].suit() == Suit::from_u8(0).unwrap()
                && (hole[1].suit() == Suit::from_u8(0).unwrap()
                    || hole[1].suit() == Suit::from_u8(1).unwrap());
            if rep[c].is_none() && canonical {
                rep[c] = Some(hole);
            }
        }
    }
    let ev = NaiveHandEvaluator;
    let mut rng = ChaCha20Rng::from_seed(0x5052_4546_4c4f_5045);
    let mut out = Vec::new();
    for (c, hole) in rep.iter().enumerate() {
        let hole = hole.expect("every class has a representative");
        let mut deck: Vec<u8> = (0..52u8)
            .filter(|x| *x != hole[0].to_u8() && *x != hole[1].to_u8())
            .collect();
        let mut score = 0u64; // 2 per win, 1 per tie
        for _ in 0..samples {
            for i in 0..7 {
                let j = i + (rng.next_u64() % (deck.len() - i) as u64) as usize;
                deck.swap(i, j);
            }
            let card = |i: usize| Card::from_u8(deck[i]).unwrap();
            let board = [card(2), card(3), card(4), card(5), card(6)];
            let mine = ev.eval7(&[
                hole[0], hole[1], board[0], board[1], board[2], board[3], board[4],
            ]);
            let theirs = ev.eval7(&[
                card(0),
                card(1),
                board[0],
                board[1],
                board[2],
                board[3],
                board[4],
            ]);
            score += match mine.cmp(&theirs) {
                std::cmp::Ordering::Greater => 2,
                std::cmp::Ordering::Equal => 1,
                std::cmp::Ordering::Less => 0,
            };
        }
        let eq = score as f64 / (2 * samples) as f64;
        let _ = Rank::from_u8(0);
        out.push((c, hole, eq));
    }
    println!("pub const PREFLOP_EQUITY_VS_RANDOM: [f32; 169] = [");
    for chunk in out.chunks(8) {
        let line: Vec<String> = chunk.iter().map(|(_, _, e)| format!("{e:.4}")).collect();
        println!("    {},", line.join(", "));
    }
    println!("];");
    let name = |h: [Card; 2]| format!("{:?}{:?}", h[0], h[1]);
    let mut sorted = out.clone();
    sorted.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
    eprintln!(
        "top: {:?}",
        sorted
            .iter()
            .take(6)
            .map(|(c, h, e)| (c, name(*h), e))
            .collect::<Vec<_>>()
    );
    eprintln!(
        "bottom: {:?}",
        sorted
            .iter()
            .rev()
            .take(4)
            .map(|(c, h, e)| (c, name(*h), e))
            .collect::<Vec<_>>()
    );
}
