//! 对一组人读 (hole, board) 组合算 canonical_observation_id → 查 bucket。
//!
//! 用法:
//!   cargo run --release --example bucket_lookup_hands -- \
//!       artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav3.bin
//!
//! 第一次访问 river 会构建 lazy table (~3 min on 4-core)。

use std::path::PathBuf;

use poker::{
    canonical_observation_id, BucketTable, Card, Rank, StreetTag, Suit,
};
use poker::abstraction::preflop::canonical_hole_id;

fn parse_rank(c: char) -> Rank {
    match c {
        '2' => Rank::Two, '3' => Rank::Three, '4' => Rank::Four,
        '5' => Rank::Five, '6' => Rank::Six, '7' => Rank::Seven,
        '8' => Rank::Eight, '9' => Rank::Nine, 'T' | 't' => Rank::Ten,
        'J' | 'j' => Rank::Jack, 'Q' | 'q' => Rank::Queen,
        'K' | 'k' => Rank::King, 'A' | 'a' => Rank::Ace,
        _ => panic!("bad rank: {c}"),
    }
}

fn parse_suit(c: char) -> Suit {
    match c {
        'c' | 'C' => Suit::Clubs, 'd' | 'D' => Suit::Diamonds,
        'h' | 'H' => Suit::Hearts, 's' | 'S' => Suit::Spades,
        _ => panic!("bad suit: {c}"),
    }
}

fn parse_card(s: &str) -> Card {
    let mut it = s.chars();
    let r = parse_rank(it.next().unwrap());
    let s = parse_suit(it.next().unwrap());
    Card::new(r, s)
}

fn parse_cards(s: &str) -> Vec<Card> {
    s.split_whitespace().map(parse_card).collect()
}

struct Case {
    label: &'static str,
    street: StreetTag,
    hole: &'static str,
    board: &'static str,
}

fn cases() -> Vec<Case> {
    vec![
        // ---- preflop ----
        Case { label: "AA",              street: StreetTag::Preflop, hole: "As Ah", board: "" },
        Case { label: "KK",              street: StreetTag::Preflop, hole: "Ks Kh", board: "" },
        Case { label: "QQ",              street: StreetTag::Preflop, hole: "Qs Qh", board: "" },
        Case { label: "AKs",             street: StreetTag::Preflop, hole: "As Ks", board: "" },
        Case { label: "AKo",             street: StreetTag::Preflop, hole: "As Kh", board: "" },
        Case { label: "76s",             street: StreetTag::Preflop, hole: "7s 6s", board: "" },
        Case { label: "22",              street: StreetTag::Preflop, hole: "2s 2h", board: "" },
        Case { label: "72o",             street: StreetTag::Preflop, hole: "7s 2h", board: "" },

        // ---- flop ----
        Case { label: "AA on dry 7c4d2h",     street: StreetTag::Flop, hole: "As Ah", board: "7c 4d 2h" },
        Case { label: "AA on KsQsJs (mono)",  street: StreetTag::Flop, hole: "Ac Ad", board: "Ks Qs Js" },
        Case { label: "AKs nut flush AsKs on Qs7s2c", street: StreetTag::Flop, hole: "As Ks", board: "Qs 7s 2c" },
        Case { label: "AKo top pair on AhKc7d",       street: StreetTag::Flop, hole: "As Kh", board: "Ad 7d 2c" }, // top pair top kicker
        Case { label: "55 set on 5h7s2d",     street: StreetTag::Flop, hole: "5d 5c", board: "5h 7s 2d" },
        Case { label: "QJ str8+OE on Tc9h8s", street: StreetTag::Flop, hole: "Qd Jc", board: "Tc 9h 8s" },
        Case { label: "88 underpair on AhKd7d", street: StreetTag::Flop, hole: "8d 8c", board: "Ah Kd 7d" },
        Case { label: "72o trash on AsKhQd",  street: StreetTag::Flop, hole: "7d 2c", board: "As Kh Qd" },
        Case { label: "9♥8♥ open-ender + bd flush on Th7s2h", street: StreetTag::Flop, hole: "9h 8h", board: "Th 7s 2h" },

        // ---- turn ----
        Case { label: "AA overpair on Ks Qs Js 2h",   street: StreetTag::Turn, hole: "Ac Ad", board: "Ks Qs Js 2h" },
        Case { label: "AKs nut flush on QsJs7s 2c",   street: StreetTag::Turn, hole: "As Ks", board: "Qs Js 7s 2c" },
        Case { label: "55 set on 5h7s2d Th",          street: StreetTag::Turn, hole: "5d 5c", board: "5h 7s 2d Th" },
        Case { label: "AsKh trash on 2c3d4h5s",       street: StreetTag::Turn, hole: "As Kh", board: "2c 3d 4h 5s" }, // wheel on board, hero plays board
        Case { label: "7d6d FD+OESD on Td9d2c 3h",    street: StreetTag::Turn, hole: "7d 6d", board: "Td 9d 2c 3h" },
        Case { label: "72o trash on AsKhQdJc",        street: StreetTag::Turn, hole: "7d 2c", board: "As Kh Qd Jc" },

        // ---- river ----
        Case { label: "AA full on KsQsJs 2h Kh",       street: StreetTag::River, hole: "Ac Ad", board: "Ks Qs Js 2h Kh" },
        Case { label: "AsKs str-flush on QsJsTs 9s 8s", street: StreetTag::River, hole: "As Ks", board: "Qs Js Ts 9s 8s" },
        Case { label: "55 quads on 5h5s2d 8c 7h",      street: StreetTag::River, hole: "5d 5c", board: "5h 5s 2d 8c 7h" },
        Case { label: "AsKh top two on AdKc 7c 3d 2s", street: StreetTag::River, hole: "As Kh", board: "Ad Kc 7c 3d 2s" },
        Case { label: "72o trash on AsKhQdJcTc",       street: StreetTag::River, hole: "7d 2c", board: "As Kh Qd Jc Tc" },
        Case { label: "QdJc bdb_str on Tc9h8s 3d 2c",  street: StreetTag::River, hole: "Qd Jc", board: "Tc 9h 8s 3d 2c" },
    ]
}

fn main() {
    let path = std::env::args().nth(1).expect(
        "usage: bucket_lookup_hands <bucket_table.bin>",
    );
    let table = BucketTable::open(&PathBuf::from(&path)).expect("open BucketTable");

    eprintln!(
        "[bucket_lookup_hands] artifact={path:?}  K=(flop {} / turn {} / river {})  seed={:#018x}",
        table.bucket_count(StreetTag::Flop),
        table.bucket_count(StreetTag::Turn),
        table.bucket_count(StreetTag::River),
        table.training_seed(),
    );

    let mut current_street: Option<StreetTag> = None;
    for c in cases() {
        if current_street != Some(c.street) {
            if matches!(c.street, StreetTag::River) {
                eprintln!("[bucket_lookup_hands] first River lookup will build lazy table (~3 min)...");
            }
            current_street = Some(c.street);
            println!();
            println!("=== {:?} ===", c.street);
        }
        let hole_cards = parse_cards(c.hole);
        assert_eq!(hole_cards.len(), 2, "{}: hole must be 2 cards", c.label);
        let hole: [Card; 2] = [hole_cards[0], hole_cards[1]];

        let cid = match c.street {
            StreetTag::Preflop => canonical_hole_id(hole),
            _ => {
                let board = parse_cards(c.board);
                canonical_observation_id(c.street, &board, hole)
            }
        };
        let bid = table.lookup(c.street, cid).expect("lookup");
        let k = table.bucket_count(c.street);
        let pct = (bid as f64 / (k - 1).max(1) as f64) * 100.0;
        println!(
            "  {:55}  hole={:5} board={:14}  canonical_id={:10}  bucket={:4} / {} ({:5.1}% strength)",
            c.label, c.hole, c.board, cid, bid, k, pct
        );
    }
}
