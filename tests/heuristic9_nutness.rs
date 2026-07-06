//! 成手坚果度只读特征（`features::nonnut_flush` / `monster_non_nut`）正确性。
//!
//! 这两个特征为「成花/成顺湿板非坚果 made commit」造靶（`heuristic9_sim --field flushtrap`）
//! 与将来剥削层门控供数；引擎 policy 目前**不消费**它们（默认 value_raise 门未改——见
//! `docs/heuristic9_design.md` §6「Phase C 第二轮候选① 驳回」：对弱/被动场非坚果同花
//! value_raise 是 +EV，标准矩阵 11/11 退化）。本测试守住特征本身的判定正确。

use poker::core::Card;
use poker::heuristic::features::{monster_non_nut, nonnut_flush};
use poker::heuristic::view::parse_card;

fn c(s: &str) -> Card {
    parse_card(s).unwrap()
}

fn board(cards: &[&str]) -> Vec<Card> {
    cards.iter().map(|s| c(s)).collect()
}

#[test]
fn nonnut_flush_qhigh_is_nonnut() {
    // 实证 hand `1b3328bc`：Qc3c on 6h2s5c8c4c（三梅花）= Q 高非坚果同花（K/A 梅花更高）。
    let b = board(&["6h", "2s", "5c", "8c", "4c"]);
    assert!(nonnut_flush([c("Qc"), c("3c")], &b), "Q 高同花应判非坚果");
}

#[test]
fn nonnut_flush_ahigh_is_nut() {
    // Ac 高坚果同花 → 非坚果=false（不该被压制）。
    let b = board(&["6h", "2s", "5c", "8c", "4c"]);
    assert!(
        !nonnut_flush([c("Ac"), c("3c")], &b),
        "A 高坚果同花不应判非坚果"
    );
}

#[test]
fn nonnut_flush_khigh_ace_live_is_nonnut() {
    // K 高同花、A 梅花既不在 board 也不在手 → 对手可持 A 同花 → 非坚果。
    let b = board(&["6h", "2s", "5c", "8c", "4c"]);
    assert!(
        nonnut_flush([c("Kc"), c("3c")], &b),
        "A 活着的 K 高同花应判非坚果"
    );
}

#[test]
fn nonnut_flush_khigh_ace_on_board_is_nut() {
    // A 梅花在 board 上 → K 高即有效坚果同花 → 非坚果=false。
    let b = board(&["Ac", "2s", "5c", "8c", "4c"]);
    assert!(
        !nonnut_flush([c("Kc"), c("3c")], &b),
        "A 在 board 上时 K 高同花是有效坚果"
    );
}

#[test]
fn nonnut_flush_false_when_not_flush() {
    // 顶对不是同花 → nonnut_flush=false。
    let b = board(&["Ad", "7c", "2s"]);
    assert!(!nonnut_flush([c("Ah"), c("Kh")], &b));
}

#[test]
fn monster_non_nut_flush_matches() {
    // monster_non_nut 对同花的判定应与 nonnut_flush 一致。
    let b = board(&["6h", "2s", "5c", "8c", "4c"]);
    assert!(monster_non_nut([c("Qc"), c("3c")], &b));
    assert!(!monster_non_nut([c("Ac"), c("3c")], &b));
}

#[test]
fn monster_non_nut_nut_straight_dry_board_is_nut() {
    // 干板坚果顺（KQ on 9TJ → 9TJQK，无 A 高可能）→ 非坚果=false。
    let b = board(&["9h", "Ts", "Jd", "4c", "2s"]);
    assert!(
        !monster_non_nut([c("Kc"), c("Qd")], &b),
        "干板坚果顺不应判非坚果"
    );
}

#[test]
fn monster_non_nut_low_straight_dominated_is_nonnut() {
    // 干板低端顺（87 on 9TJ → 789TJ，Q/K 高更高）→ 非坚果=true。
    let b = board(&["9h", "Ts", "Jd", "4c", "2s"]);
    assert!(
        monster_non_nut([c("8c"), c("7d")], &b),
        "被压制的低端顺应判非坚果"
    );
}

#[test]
fn monster_non_nut_straight_on_monotone_board_is_nonnut() {
    // 成花板上的顺子（被同花压）→ 非坚果=true。
    let b = board(&["5c", "8c", "4c", "6h", "7d"]); // 三梅花 monotone
    assert!(
        monster_non_nut([c("9s"), c("Td")], &b),
        "monotone 板上顺子应判非坚果（可被同花压）"
    );
}

#[test]
fn monster_non_nut_fullhouse_not_downgraded() {
    // 葫芦不判非坚果（避免 PairedTrap 语境陷阱）。
    let b = board(&["7c", "7d", "2s", "2h", "9c"]);
    assert!(!monster_non_nut([c("9s"), c("9d")], &b), "葫芦不应判非坚果");
}
