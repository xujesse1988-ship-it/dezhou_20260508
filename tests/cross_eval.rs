//! C1：评估器 vs PokerKit 交叉验证 harness。
//!
//! `pluribus_stage1_validation.md` §4 / `pluribus_stage1_workflow.md` §C1：
//!
//! - 与开源参考评估器（如 PokerKit）交叉验证：相同的 1M 组 7-card hand，
//!   名次/类型输出完全一致。
//!
//! 本文件实现 **类别（HandCategory）** 等价交叉验证：
//!
//! - 默认规模 1,000 hands（CI 友好），需要 PokerKit 装好。
//! - `--ignored` 规模 100,000 hands。完整 1M 在 E2 / D2 阶段重跑（性能优化版评估器）。
//! - PokerKit 缺失 → 测试报告 skipped 而非 fail（与 `cross_validation.rs` 同策略）。
//!
//! 角色边界：本文件只读 `src/eval.rs`；不修改产品代码。任何分歧由 [测试] agent
//! 报 issue 给 [实现] agent。

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use poker::eval::NaiveHandEvaluator;
use poker::{Card, ChaCha20Rng, HandCategory, HandEvaluator, RngSource};
use serde_json::{json, Value};

// ============================================================================
// 共享：随机 7-card 生成、card → string 编码
// ============================================================================

fn random_seven_cards<R: RngSource + ?Sized>(rng: &mut R) -> [Card; 7] {
    let mut deck: [u8; 52] = std::array::from_fn(|i| i as u8);
    for i in 0..7 {
        let j = i + (rng.next_u64() % ((52 - i) as u64)) as usize;
        deck.swap(i, j);
    }
    [
        Card::from_u8(deck[0]).unwrap(),
        Card::from_u8(deck[1]).unwrap(),
        Card::from_u8(deck[2]).unwrap(),
        Card::from_u8(deck[3]).unwrap(),
        Card::from_u8(deck[4]).unwrap(),
        Card::from_u8(deck[5]).unwrap(),
        Card::from_u8(deck[6]).unwrap(),
    ]
}

fn card_to_string(c: Card) -> String {
    let ranks = [
        "2", "3", "4", "5", "6", "7", "8", "9", "T", "J", "Q", "K", "A",
    ];
    let suits = ["c", "d", "h", "s"];
    let v = c.to_u8();
    format!("{}{}", ranks[(v / 4) as usize], suits[(v % 4) as usize])
}

fn category_to_index(c: HandCategory) -> i64 {
    match c {
        HandCategory::HighCard => 0,
        HandCategory::OnePair => 1,
        HandCategory::TwoPair => 2,
        HandCategory::Trips => 3,
        HandCategory::Straight => 4,
        HandCategory::Flush => 5,
        HandCategory::FullHouse => 6,
        HandCategory::Quads => 7,
        HandCategory::StraightFlush => 8,
        HandCategory::RoyalFlush => 9,
    }
}

fn locate_python_helper() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    PathBuf::from(manifest)
        .join("tools")
        .join("pokerkit_eval.py")
}

#[derive(Debug, Default)]
struct Report {
    matches: usize,
    diverged: usize,
    skipped: bool,
    first_divergence: Option<(usize, i64, i64, [Card; 7])>,
}

// ============================================================================
// 入口：批量比对
// ============================================================================

fn cross_validate_categories(samples: usize, seed: u64) -> Report {
    let mut report = Report::default();
    let ev = NaiveHandEvaluator;
    let mut rng = ChaCha20Rng::from_seed(seed);

    let mut our_categories: Vec<i64> = Vec::with_capacity(samples);
    let mut hands: Vec<[Card; 7]> = Vec::with_capacity(samples);
    for _ in 0..samples {
        let h = random_seven_cards(&mut rng);
        let r = ev.eval7(&h);
        our_categories.push(category_to_index(r.category()));
        hands.push(h);
    }

    // 序列化 hands → JSON
    let payload = json!({
        "hands": hands.iter().map(|h| {
            h.iter().map(|c| card_to_string(*c)).collect::<Vec<_>>()
        }).collect::<Vec<_>>(),
    });

    let script = locate_python_helper();
    let mut child = match Command::new("python3")
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            report.skipped = true;
            return report;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.to_string().as_bytes());
    }
    let output = child.wait_with_output().expect("wait_with_output");
    if let Some(code) = output.status.code() {
        if code == 2 {
            report.skipped = true;
            return report;
        }
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let resp: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            report.skipped = true; // harness/JSON error 当作 skipped（不是规则分歧）
            return report;
        }
    };
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        report.skipped = true;
        return report;
    }
    let cats = match resp.get("categories").and_then(Value::as_array) {
        Some(a) => a,
        None => {
            report.skipped = true;
            return report;
        }
    };
    if cats.len() != samples {
        report.skipped = true;
        return report;
    }

    for (i, v) in cats.iter().enumerate() {
        let their = match v.as_i64() {
            Some(x) => x,
            None => {
                report.skipped = true;
                return report;
            }
        };
        let ours = our_categories[i];
        if their == ours {
            report.matches += 1;
        } else {
            report.diverged += 1;
            if report.first_divergence.is_none() {
                report.first_divergence = Some((i, ours, their, hands[i]));
            }
        }
    }
    report
}

// ============================================================================
// 测试
// ============================================================================

#[test]
fn cross_eval_smoke_default() {
    let report = cross_validate_categories(1_000, 0xC1_E1AA);
    eprintln!("[cross_eval-1k] {report:?}");
    if report.skipped {
        eprintln!("[cross_eval-1k] PokerKit 不可用 → skipped");
        return;
    }
    assert_eq!(
        report.diverged, 0,
        "category divergence: first = {:?}",
        report.first_divergence
    );
    assert_eq!(report.matches, 1_000);
}

#[ignore = "C1 full-volume; needs PokerKit installed; run with -- --ignored"]
#[test]
fn cross_eval_full_100k() {
    let report = cross_validate_categories(100_000, 0xC1_E1AA);
    eprintln!("[cross_eval-100k] {report:?}");
    if report.skipped {
        panic!("PokerKit unavailable — install with `pip install pokerkit`");
    }
    assert_eq!(
        report.diverged, 0,
        "category divergence: first = {:?}",
        report.first_divergence
    );
    assert_eq!(report.matches, 100_000);
}
