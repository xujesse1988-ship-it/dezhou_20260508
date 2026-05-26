//! 一次性迁移：把 v3 bucket table 重排成 v4。
//!
//! 背景：`canonical_enum` 在 2026-05 把 canonical observation id 编号从「整表
//! u128 sort rank」改为 shape-major direct combinatorial rank（详见
//! `src/abstraction/canonical_enum.rs` 模块头）。bucket table 的 lookup 段是按
//! canonical id 索引的 `[u32; N]`；重编号后旧 v3 表的行↔等价类对应关系错位。
//!
//! 但**重编号不改变任何一手牌的 feature，也就不改变它被分到哪个 bucket** —— 只是
//! 行的排列顺序变了。所以无需重算 feature / 重跑 k-means，只要把 lookup 段按新 id
//! 重排即可，得到逐 bucket 与 v3 完全一致的 v4 表。
//!
//! 重排映射：旧 id 的定义 = 该等价类 packed-u128 key 在全部 key 升序中的 rank
//! （旧 `canonical_observation_id` 就是 sorted `Vec<u128>` 里的 binary-search 下标）。
//! 因此对每个 `new_id` 算出其 packed key，按 key 升序排 → 排到第 `old_id` 位即该类
//! 的旧 id。`lookup_new[new_id] = lookup_old[old_id]`。
//!
//! 每个街的重排只依赖街（与表内容无关），故 3 个 seed 文件共用同一组排列，只算一次。
//!
//! 用法（在 repo root，release 跑）：
//! ```bash
//! cargo run --release --bin bucket_table_reindex_v3_to_v4 -- \
//!     IN1.bin OUT1.bin [IN2.bin OUT2.bin ...]
//! ```
//!
//! 校验：产出 v4 表后跑 `cargo test --release --test bucket_quality`（需把
//! `PRODUCTION_ARTIFACT_PATH` 指向产出文件）——质量门槛只在 bucket 分配正确时通过，
//! 故等价于「same hand → same bucket」端到端验证。

use std::env;
use std::fs;

use rayon::prelude::*;

use poker::abstraction::canonical_enum::{
    nth_canonical_form, N_CANONICAL_OBSERVATION_FLOP, N_CANONICAL_OBSERVATION_RIVER,
    N_CANONICAL_OBSERVATION_TURN,
};
use poker::{Card, StreetTag};

const MAGIC: &[u8; 8] = b"PLBKT\0\0\0";
const PREFLOP_LEN: usize = 1326;

// header 偏移（参 `src/abstraction/bucket_table.rs` header layout 文档）。
const OFF_SCHEMA_VERSION: usize = 0x08;
const OFF_N_CANONICAL_FLOP: usize = 0x1C;
const OFF_N_CANONICAL_TURN: usize = 0x20;
const OFF_N_CANONICAL_RIVER: usize = 0x24;
const OFF_LOOKUP_TABLE_OFFSET: usize = 0x48;
const TRAILER_LEN: usize = 32;

/// 旧编号方案的 sort key：与 `canonical_enum::canonical_sigs` + `pack_sigs` 逐位
/// 一致——4 个 suit 的 `(b_count, h_count, b_mask, h_mask)` 升序排好，每个打包成
/// 32 bit `[b_count(3) | h_count(2) | b_mask(13) | h_mask(13)]`，slot 0 占高位。
/// u128 数值序 == 旧 canonical id 序。
fn legacy_key(board: &[Card], hole: &[Card; 2]) -> u128 {
    let mut suits: [(u16, u16); 4] = [(0, 0); 4];
    for &c in board {
        suits[c.suit() as usize].0 |= 1u16 << (c.rank() as u8);
    }
    for &c in hole {
        suits[c.suit() as usize].1 |= 1u16 << (c.rank() as u8);
    }
    let mut sigs: [(u8, u8, u16, u16); 4] = [(0, 0, 0, 0); 4];
    for (s, suit) in suits.iter().enumerate() {
        sigs[s] = (
            suit.0.count_ones() as u8,
            suit.1.count_ones() as u8,
            suit.0,
            suit.1,
        );
    }
    sigs.sort_unstable();
    let mut key: u128 = 0;
    for (i, sig) in sigs.iter().enumerate() {
        let pack: u128 = ((sig.0 as u128) << 28)
            | ((sig.1 as u128) << 26)
            | ((sig.2 as u128) << 13)
            | (sig.3 as u128);
        key |= pack << (32 * (3 - i));
    }
    key
}

fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn rd_u64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
fn wr_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// 返回 `order`，其中 `order[old_id] = new_id`。旧 id = packed key 升序 rank。
fn build_perm(street: StreetTag, n: usize) -> Vec<u32> {
    eprintln!("[perm] {street:?} N={n}：算 packed key …");
    let keys: Vec<u128> = (0..n)
        .into_par_iter()
        .map(|new_id| {
            let (board, hole) = nth_canonical_form(street, new_id as u32);
            legacy_key(&board, &hole)
        })
        .collect();
    eprintln!("[perm] {street:?}：排序 …");
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.par_sort_unstable_by(|&a, &b| keys[a as usize].cmp(&keys[b as usize]));
    // 旧编号要求 key 全 distinct（canonical 等价类两两不同）；否则 old_id 有歧义。
    let dup = (1..n)
        .into_par_iter()
        .any(|i| keys[order[i] as usize] == keys[order[i - 1] as usize]);
    assert!(!dup, "{street:?}: legacy key 非 distinct（迁移前提被破坏）");
    eprintln!("[perm] {street:?}：完成");
    order
}

/// 把 `bytes` 中从 `region_off` 起的 `n` 个 u32 lookup 按 `order` 重排：
/// `new[new_id] = old[old_id]`，`order[old_id] = new_id`。
fn reindex_street(bytes: &mut [u8], region_off: usize, n: usize, order: &[u32]) {
    let mut new_lookup = vec![0u32; n];
    for (old_id, &new_id) in order.iter().enumerate() {
        let bucket = rd_u32(bytes, region_off + old_id * 4);
        new_lookup[new_id as usize] = bucket;
    }
    for (new_id, &bucket) in new_lookup.iter().enumerate() {
        wr_u32(bytes, region_off + new_id * 4, bucket);
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 || (args.len() - 1) % 2 != 0 {
        eprintln!("usage: {} IN1 OUT1 [IN2 OUT2 ...]", args[0]);
        std::process::exit(2);
    }

    let n_flop = N_CANONICAL_OBSERVATION_FLOP as usize;
    let n_turn = N_CANONICAL_OBSERVATION_TURN as usize;
    let n_river = N_CANONICAL_OBSERVATION_RIVER as usize;

    // 3 街排列只依赖街，算一次复用到所有文件。
    let perm_flop = build_perm(StreetTag::Flop, n_flop);
    let perm_turn = build_perm(StreetTag::Turn, n_turn);
    let perm_river = build_perm(StreetTag::River, n_river);

    let mut i = 1;
    while i < args.len() {
        let inp = &args[i];
        let outp = &args[i + 1];
        i += 2;

        let mut bytes = fs::read(inp).unwrap_or_else(|e| panic!("read {inp}: {e}"));
        assert_eq!(&bytes[0..8], MAGIC, "{inp}: bad magic（非 bucket table）");
        assert_eq!(
            rd_u32(&bytes, OFF_SCHEMA_VERSION),
            3,
            "{inp}: 不是 schema v3（本工具只迁移 v3→v4）"
        );
        assert_eq!(
            rd_u32(&bytes, OFF_N_CANONICAL_FLOP) as usize,
            n_flop,
            "{inp}: N_flop 不符"
        );
        assert_eq!(
            rd_u32(&bytes, OFF_N_CANONICAL_TURN) as usize,
            n_turn,
            "{inp}: N_turn 不符"
        );
        assert_eq!(
            rd_u32(&bytes, OFF_N_CANONICAL_RIVER) as usize,
            n_river,
            "{inp}: N_river 不符"
        );

        let lookup_off = rd_u64(&bytes, OFF_LOOKUP_TABLE_OFFSET) as usize;
        let flop_off = lookup_off + PREFLOP_LEN * 4;
        let turn_off = flop_off + n_flop * 4;
        let river_off = turn_off + n_turn * 4;
        // 末尾还有 32 byte trailer，lookup river 段不应越界。
        assert!(
            river_off + n_river * 4 + TRAILER_LEN <= bytes.len(),
            "{inp}: lookup 段越界（文件被截断？）"
        );

        // preflop 段（1326）按 canonical_hole_id 编号，未变，原样保留。
        reindex_street(&mut bytes, flop_off, n_flop, &perm_flop);
        reindex_street(&mut bytes, turn_off, n_turn, &perm_turn);
        reindex_street(&mut bytes, river_off, n_river, &perm_river);

        wr_u32(&mut bytes, OFF_SCHEMA_VERSION, 4);

        // 重算 trailer = BLAKE3(file[..len-32])。
        let body_end = bytes.len() - TRAILER_LEN;
        let h = blake3::hash(&bytes[..body_end]);
        bytes[body_end..].copy_from_slice(h.as_bytes());

        fs::write(outp, &bytes).unwrap_or_else(|e| panic!("write {outp}: {e}"));
        eprintln!("[done] {inp} → {outp}（{} bytes, schema v4）", bytes.len());
    }
}
