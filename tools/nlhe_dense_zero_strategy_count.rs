//! 统计 dense NLHE checkpoint 中 `strategy_sum` 全 0 的 infoset 数量。
//!
//! dense checkpoint 的 `strategy_touched` bitset 按 infoset row 记录
//! `strategy_sum` 是否被写过。ES-MCCFR 的 strategy_sum 是非负策略累积：
//! 一行一旦被写过，至少有一个 action slot > 0；未写过则该 infoset
//! 所有 action 的 strategy_sum 都为 0。
//!
//! 用法：
//! ```
//! cargo run --release --bin nlhe_dense_zero_strategy_count -- \
//!     --checkpoint artifacts/run_dense_lockfree/nlhe_es_mccfr_final_001000000000.ckpt
//! ```

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use poker::training::nlhe_dense::NlheDenseIndexer;
use poker::{BucketTable, SimplifiedNlheGame, StreetTag};

const DENSE_MAGIC: &[u8; 8] = b"PLDNCKPT";
const DENSE_SCHEMA_VERSION: u32 = 3;
const STORAGE_KIND_DENSE_NLHE_V1: u8 = 1;
const HEADER_LEN: usize = 160;
const TRAILER_LEN: u64 = 32;

const OFF_MAGIC: usize = 0;
const OFF_SCHEMA: usize = 8;
const OFF_STORAGE_KIND: usize = 12;
const OFF_UPDATE_COUNT: usize = 16;
const OFF_NUM_NODES: usize = 40;
const OFF_TOTAL_ROWS: usize = 48;
const OFF_TOTAL_SLOTS: usize = 56;
const OFF_BUCKET_BLAKE3: usize = 96;

struct Args {
    checkpoint: PathBuf,
    bucket_table: PathBuf,
    json: bool,
}

#[derive(Clone)]
struct StreetReport {
    name: &'static str,
    infoset_rows: u64,
    strategy_sum_nonzero_rows: u64,
    strategy_sum_all_zero_rows: u64,
    all_zero_pct: f64,
}

struct Report {
    checkpoint: String,
    bucket_table: String,
    update_count: u64,
    bucket_blake3: String,
    num_nodes: u64,
    total_infoset_rows: u64,
    total_action_slots: u64,
    strategy_sum_nonzero_rows: u64,
    strategy_sum_all_zero_rows: u64,
    all_zero_pct: f64,
    by_street: Vec<StreetReport>,
    expected_file_bytes: u64,
    actual_file_bytes: u64,
}

fn usage() -> &'static str {
    "usage: nlhe_dense_zero_strategy_count --checkpoint PATH \
     [--bucket-table artifacts/bucket_table_default_1000_1000_1000_seed_cafebabe_schemav4.bin] \
     [--json]"
}

fn parse_args() -> Result<Args, String> {
    let mut checkpoint = None;
    let mut bucket_table =
        PathBuf::from("artifacts/bucket_table_default_1000_1000_1000_seed_cafebabe_schemav4.bin");
    let mut json = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--checkpoint" => {
                checkpoint =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        "missing value for --checkpoint".to_string()
                    })?));
            }
            "--bucket-table" | "--artifact" => {
                bucket_table = PathBuf::from(
                    args.next()
                        .ok_or_else(|| "missing value for --bucket-table".to_string())?,
                );
            }
            "--json" => json = true,
            "--help" | "-h" => return Err(usage().to_string()),
            other => return Err(format!("unknown arg {other:?}\n{}", usage())),
        }
    }
    Ok(Args {
        checkpoint: checkpoint.ok_or_else(|| format!("missing --checkpoint\n{}", usage()))?,
        bucket_table,
        json,
    })
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[nlhe_dense_zero_strategy_count] failed: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let report = inspect_checkpoint(&args.checkpoint, &args.bucket_table)?;
    if args.json {
        print_json(&report);
    } else {
        print_markdown(&report);
    }
    Ok(())
}

fn inspect_checkpoint(path: &Path, bucket_table_path: &Path) -> Result<Report, String> {
    let mut file = File::open(path).map_err(|e| format!("open {} failed: {e}", path.display()))?;
    let actual_file_bytes = file
        .metadata()
        .map_err(|e| format!("metadata {} failed: {e}", path.display()))?
        .len();

    let mut header = [0u8; HEADER_LEN];
    file.read_exact(&mut header)
        .map_err(|e| format!("read dense header failed: {e}"))?;
    validate_header(&header)?;

    let update_count = read_u64(&header, OFF_UPDATE_COUNT);
    let num_nodes = read_u64(&header, OFF_NUM_NODES);
    let total_infoset_rows = read_u64(&header, OFF_TOTAL_ROWS);
    let total_action_slots = read_u64(&header, OFF_TOTAL_SLOTS);
    let bucket_blake3 = hex_bytes(&header[OFF_BUCKET_BLAKE3..OFF_BUCKET_BLAKE3 + 32]);
    let bucket_blake3_raw: [u8; 32] = header[OFF_BUCKET_BLAKE3..OFF_BUCKET_BLAKE3 + 32]
        .try_into()
        .unwrap();

    let touched_word_count = total_infoset_rows.div_ceil(64);
    let touched_bytes = touched_word_count
        .checked_mul(8)
        .ok_or_else(|| "touched bitset byte length overflow".to_string())?;
    let strategy_touched_offset = HEADER_LEN as u64 + touched_bytes;
    let expected_file_bytes = HEADER_LEN as u64
        + touched_bytes
            .checked_mul(2)
            .ok_or_else(|| "touched bitset total byte length overflow".to_string())?
        + total_action_slots
            .checked_mul(8)
            .and_then(|n| n.checked_mul(2))
            .ok_or_else(|| "dense values byte length overflow".to_string())?
        + TRAILER_LEN;
    if actual_file_bytes < strategy_touched_offset + touched_bytes {
        return Err(format!(
            "file too short for strategy_touched bitset: len={actual_file_bytes}, need={}",
            strategy_touched_offset + touched_bytes
        ));
    }

    file.seek(SeekFrom::Start(strategy_touched_offset))
        .map_err(|e| format!("seek strategy_touched failed: {e}"))?;
    let touched_words = read_touched_words(&mut file, total_infoset_rows)?;
    let strategy_sum_nonzero_rows = count_touched_bits(&touched_words, total_infoset_rows);
    let strategy_sum_all_zero_rows = total_infoset_rows.saturating_sub(strategy_sum_nonzero_rows);
    let all_zero_pct = if total_infoset_rows == 0 {
        0.0
    } else {
        strategy_sum_all_zero_rows as f64 * 100.0 / total_infoset_rows as f64
    };
    let (bucket_table, indexer) = build_indexer(bucket_table_path, bucket_blake3_raw)?;
    validate_indexer(&indexer, num_nodes, total_infoset_rows, total_action_slots)?;
    let by_street = summarize_by_street(&indexer, &touched_words);

    Ok(Report {
        checkpoint: path.display().to_string(),
        bucket_table: bucket_table.display().to_string(),
        update_count,
        bucket_blake3,
        num_nodes,
        total_infoset_rows,
        total_action_slots,
        strategy_sum_nonzero_rows,
        strategy_sum_all_zero_rows,
        all_zero_pct,
        by_street,
        expected_file_bytes,
        actual_file_bytes,
    })
}

fn build_indexer(
    bucket_table_path: &Path,
    expected_bucket_blake3: [u8; 32],
) -> Result<(PathBuf, NlheDenseIndexer), String> {
    let table = Arc::new(BucketTable::open(bucket_table_path).map_err(|e| {
        format!(
            "BucketTable::open({}) failed: {e:?}",
            bucket_table_path.display()
        )
    })?);
    let actual = table.content_hash();
    if actual != expected_bucket_blake3 {
        return Err(format!(
            "bucket table hash mismatch: checkpoint={} table={}",
            hex_bytes(&expected_bucket_blake3),
            hex_bytes(&actual)
        ));
    }
    let game = SimplifiedNlheGame::new(Arc::clone(&table))
        .map_err(|e| format!("SimplifiedNlheGame::new failed: {e:?}"))?;
    let counts = [
        table.bucket_count(StreetTag::Preflop),
        table.bucket_count(StreetTag::Flop),
        table.bucket_count(StreetTag::Turn),
        table.bucket_count(StreetTag::River),
    ];
    Ok((
        bucket_table_path.to_path_buf(),
        NlheDenseIndexer::from_tree(game.tree(), counts),
    ))
}

fn validate_indexer(
    indexer: &NlheDenseIndexer,
    num_nodes: u64,
    total_rows: u64,
    total_slots: u64,
) -> Result<(), String> {
    let got_nodes = indexer.num_nodes() as u64;
    let got_rows = indexer.total_rows();
    let got_slots = indexer.total_slots();
    if (got_nodes, got_rows, got_slots) != (num_nodes, total_rows, total_slots) {
        return Err(format!(
            "indexer/checkpoint layout mismatch: indexer(nodes={got_nodes}, rows={got_rows}, slots={got_slots}) \
             checkpoint(nodes={num_nodes}, rows={total_rows}, slots={total_slots})"
        ));
    }
    Ok(())
}

fn summarize_by_street(indexer: &NlheDenseIndexer, touched_words: &[u64]) -> Vec<StreetReport> {
    let mut rows = [0u64; 4];
    let mut nonzero = [0u64; 4];
    for node_id in 0..indexer.num_nodes() as u32 {
        let meta = indexer.node_meta(node_id);
        let street_idx = meta.street as usize;
        rows[street_idx] += u64::from(meta.bucket_count);
        nonzero[street_idx] +=
            count_touched_range(touched_words, meta.row_base, u64::from(meta.bucket_count));
    }
    [
        StreetTag::Preflop,
        StreetTag::Flop,
        StreetTag::Turn,
        StreetTag::River,
    ]
    .iter()
    .map(|street| {
        let idx = *street as usize;
        let zero = rows[idx].saturating_sub(nonzero[idx]);
        let pct = if rows[idx] == 0 {
            0.0
        } else {
            zero as f64 * 100.0 / rows[idx] as f64
        };
        StreetReport {
            name: street_name(*street),
            infoset_rows: rows[idx],
            strategy_sum_nonzero_rows: nonzero[idx],
            strategy_sum_all_zero_rows: zero,
            all_zero_pct: pct,
        }
    })
    .collect()
}

fn validate_header(header: &[u8; HEADER_LEN]) -> Result<(), String> {
    if &header[OFF_MAGIC..OFF_MAGIC + 8] != DENSE_MAGIC {
        return Err("not a dense NLHE checkpoint: magic mismatch".to_string());
    }
    let schema = read_u32(header, OFF_SCHEMA);
    if schema != DENSE_SCHEMA_VERSION {
        return Err(format!(
            "schema mismatch: expected {DENSE_SCHEMA_VERSION}, got {schema}"
        ));
    }
    let storage_kind = header[OFF_STORAGE_KIND];
    if storage_kind != STORAGE_KIND_DENSE_NLHE_V1 {
        return Err(format!(
            "storage_kind mismatch: expected {STORAGE_KIND_DENSE_NLHE_V1}, got {storage_kind}"
        ));
    }
    Ok(())
}

fn read_touched_words(file: &mut File, total_rows: u64) -> Result<Vec<u64>, String> {
    let word_count = total_rows.div_ceil(64);
    let mut words = Vec::with_capacity(word_count as usize);
    let mut buf = [0u8; 8 * 4096];
    let mut words_read = 0u64;
    while words_read < word_count {
        let words_this_chunk = ((word_count - words_read) as usize).min(4096);
        let nbytes = words_this_chunk * 8;
        file.read_exact(&mut buf[..nbytes])
            .map_err(|e| format!("read strategy_touched failed: {e}"))?;
        for i in 0..words_this_chunk {
            let mut word = u64::from_le_bytes(buf[i * 8..i * 8 + 8].try_into().unwrap());
            let global_word = words_read + i as u64;
            if global_word == word_count - 1 {
                let valid_bits = total_rows % 64;
                if valid_bits != 0 {
                    word &= (1u64 << valid_bits) - 1;
                }
            }
            words.push(word);
        }
        words_read += words_this_chunk as u64;
    }
    Ok(words)
}

fn count_touched_bits(words: &[u64], total_rows: u64) -> u64 {
    let mut total = 0u64;
    for (idx, &word) in words.iter().enumerate() {
        let mut word = word;
        if idx == words.len().saturating_sub(1) {
            let valid_bits = total_rows % 64;
            if valid_bits != 0 {
                word &= (1u64 << valid_bits) - 1;
            }
        }
        total += u64::from(word.count_ones());
    }
    total
}

fn count_touched_range(words: &[u64], start: u64, len: u64) -> u64 {
    if len == 0 {
        return 0;
    }
    let end = start + len;
    let first_word = start / 64;
    let last_word = (end - 1) / 64;
    let mut total = 0u64;
    for word_idx in first_word..=last_word {
        let mut word = words[word_idx as usize];
        let word_start = word_idx * 64;
        let range_start = start.saturating_sub(word_start);
        let range_end = (end - word_start).min(64);
        let width = range_end - range_start;
        let mask = if width == 64 {
            u64::MAX
        } else {
            ((1u64 << width) - 1) << range_start
        };
        word &= mask;
        total += u64::from(word.count_ones());
    }
    total
}

fn print_markdown(r: &Report) {
    println!("# Dense NLHE strategy_sum 全 0 infoset 统计\n");
    println!("- checkpoint: `{}`", r.checkpoint);
    println!("- bucket_table: `{}`", r.bucket_table);
    println!("- update_count: `{}`", r.update_count);
    println!("- bucket_blake3: `{}`", r.bucket_blake3);
    println!(
        "- file_size_check: actual={} bytes / expected={} bytes{}",
        r.actual_file_bytes,
        r.expected_file_bytes,
        if r.actual_file_bytes == r.expected_file_bytes {
            " (ok)"
        } else {
            " (mismatch)"
        }
    );
    println!();
    println!("| metric | value |");
    println!("|---|---:|");
    println!("| betting tree nodes | {} |", r.num_nodes);
    println!("| total infoset rows | {} |", r.total_infoset_rows);
    println!("| total action slots | {} |", r.total_action_slots);
    println!(
        "| strategy_sum nonzero rows | {} |",
        r.strategy_sum_nonzero_rows
    );
    println!(
        "| strategy_sum all-zero rows | **{}** |",
        r.strategy_sum_all_zero_rows
    );
    println!("| all-zero pct | {:.6}% |", r.all_zero_pct);
    println!();
    println!("## By Street");
    println!();
    println!("| street | infoset rows | nonzero rows | all-zero rows | all-zero pct |");
    println!("|---|---:|---:|---:|---:|");
    for row in &r.by_street {
        println!(
            "| {} | {} | {} | {} | {:.6}% |",
            row.name,
            row.infoset_rows,
            row.strategy_sum_nonzero_rows,
            row.strategy_sum_all_zero_rows,
            row.all_zero_pct
        );
    }
}

fn print_json(r: &Report) {
    println!("{{");
    println!("  \"checkpoint\": {},", json_string(&r.checkpoint));
    println!("  \"bucket_table\": {},", json_string(&r.bucket_table));
    println!("  \"update_count\": {},", r.update_count);
    println!("  \"bucket_blake3\": {},", json_string(&r.bucket_blake3));
    println!("  \"num_nodes\": {},", r.num_nodes);
    println!("  \"total_infoset_rows\": {},", r.total_infoset_rows);
    println!("  \"total_action_slots\": {},", r.total_action_slots);
    println!(
        "  \"strategy_sum_nonzero_rows\": {},",
        r.strategy_sum_nonzero_rows
    );
    println!(
        "  \"strategy_sum_all_zero_rows\": {},",
        r.strategy_sum_all_zero_rows
    );
    println!("  \"all_zero_pct\": {:.12},", r.all_zero_pct);
    println!("  \"by_street\": [");
    for (idx, row) in r.by_street.iter().enumerate() {
        let comma = if idx + 1 == r.by_street.len() {
            ""
        } else {
            ","
        };
        println!(
            "    {{\"street\": {}, \"infoset_rows\": {}, \"strategy_sum_nonzero_rows\": {}, \
             \"strategy_sum_all_zero_rows\": {}, \"all_zero_pct\": {:.12}}}{comma}",
            json_string(row.name),
            row.infoset_rows,
            row.strategy_sum_nonzero_rows,
            row.strategy_sum_all_zero_rows,
            row.all_zero_pct
        );
    }
    println!("  ],");
    println!("  \"expected_file_bytes\": {},", r.expected_file_bytes);
    println!("  \"actual_file_bytes\": {}", r.actual_file_bytes);
    println!("}}");
}

fn street_name(street: StreetTag) -> &'static str {
    match street {
        StreetTag::Preflop => "preflop",
        StreetTag::Flop => "flop",
        StreetTag::Turn => "turn",
        StreetTag::River => "river",
    }
}

fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

fn read_u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
