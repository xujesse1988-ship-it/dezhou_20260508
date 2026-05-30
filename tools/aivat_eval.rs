//! AIVAT 估计器：接 Slumbot strategy 日志 + 自对弈 VF 表 + blueprint，逐手算 raw 与
//! AIVAT，输出 `mbb/g` + SE + 95% CI + 配对差 `d = AIVAT − raw` + 按修正类型拆分 +
//! 按位置拆分。见 `docs/aivat_eval.md` §6/§7。
//!
//! 用法：
//! ```text
//! aivat_eval \
//!   --checkpoint artifacts/run_dense_lockfree/nlhe_es_mccfr_final_001000000000.ckpt \
//!   --bucket-table artifacts/bucket_table_default_1000_1000_1000_seed_cafebabe_schemav4.bin \
//!   --vf artifacts/aivat_values.bin \
//!   --strategy-log slumbot_strategy_20260529_1.jsonl
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;

use poker::training::aivat_nlhe::{
    AivatNlheEstimator, HandInput, HandResult, LoggedDecision, TableValueFn,
};
use poker::training::aivat_value::AivatValueTables;
use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::nlhe_replay::parse_card;
use poker::{BucketTable, Card, ChaCha20Rng, InfoSetId, RngSource};

struct Args {
    checkpoint: PathBuf,
    bucket_table: PathBuf,
    vf: PathBuf,
    strategy_log: PathBuf,
    limit: Option<usize>,
}

fn parse_args() -> Result<Args, String> {
    let mut checkpoint = None;
    let mut bucket_table = None;
    let mut vf = None;
    let mut strategy_log = None;
    let mut limit = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut next = |a: &str| it.next().ok_or_else(|| format!("{a} 缺参数值"));
        match arg.as_str() {
            "--checkpoint" => checkpoint = Some(PathBuf::from(next(&arg)?)),
            "--bucket-table" => bucket_table = Some(PathBuf::from(next(&arg)?)),
            "--vf" => vf = Some(PathBuf::from(next(&arg)?)),
            "--strategy-log" => strategy_log = Some(PathBuf::from(next(&arg)?)),
            "--limit" => limit = Some(next(&arg)?.parse().map_err(|e| format!("--limit: {e}"))?),
            other => return Err(format!("未知参数 {other}")),
        }
    }
    Ok(Args {
        checkpoint: checkpoint.ok_or("缺 --checkpoint")?,
        bucket_table: bucket_table.ok_or("缺 --bucket-table")?,
        vf: vf.ok_or("缺 --vf")?,
        strategy_log: strategy_log.ok_or("缺 --strategy-log")?,
        limit,
    })
}

/// 在线统计累加器（mean / SE）。
#[derive(Default)]
struct Stat {
    n: u64,
    sum: f64,
    sum_sq: f64,
}

impl Stat {
    fn push(&mut self, x: f64) {
        self.n += 1;
        self.sum += x;
        self.sum_sq += x * x;
    }
    fn mean(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.sum / self.n as f64
        }
    }
    /// 样本方差（无偏，n−1）。
    fn var(&self) -> f64 {
        if self.n < 2 {
            return 0.0;
        }
        let n = self.n as f64;
        (self.sum_sq - self.sum * self.sum / n) / (n - 1.0)
    }
    fn se(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            (self.var() / self.n as f64).sqrt()
        }
    }
}

/// chips → mbb/g（`mbb = chips × 10`）。
const MBB: f64 = 10.0;

fn report(name: &str, s: &Stat) {
    let m = s.mean() * MBB;
    let se = s.se() * MBB;
    println!(
        "  {name:<14} mean={m:>10.2} mbb/g   SE={se:>8.2}   95% CI [{:>10.2}, {:>10.2}]   (n={})",
        m - 1.96 * se,
        m + 1.96 * se,
        s.n
    );
}

fn main() -> Result<(), String> {
    let args = parse_args()?;

    eprintln!(
        "[aivat-eval] 加载 bucket table {}",
        args.bucket_table.display()
    );
    let table = Arc::new(
        BucketTable::open(&args.bucket_table).map_err(|e| format!("BucketTable::open: {e:?}"))?,
    );
    let game = SimplifiedNlheGame::new(Arc::clone(&table)).map_err(|e| format!("game: {e:?}"))?;

    eprintln!("[aivat-eval] 加载 checkpoint {}", args.checkpoint.display());
    let trainer = DenseNlheEsMccfrTrainer::load_checkpoint(&args.checkpoint, game)
        .map_err(|e| format!("load_checkpoint: {e:?}"))?;
    let game: &SimplifiedNlheGame = trainer.game();

    eprintln!("[aivat-eval] 加载 VF 表 {}", args.vf.display());
    let vf = AivatValueTables::load(&args.vf).map_err(|e| format!("VF load: {e}"))?;

    // ---- provenance（§6）：VF 与 blueprint / bucket 表必须同源，否则 row/桶错位。----
    if vf.update_count != trainer.update_count() {
        return Err(format!(
            "VF.update_count {} != blueprint.update_count {}（VF 不是这个 blueprint 自对弈建的）",
            vf.update_count,
            trainer.update_count()
        ));
    }
    let bucket_hash = game.bucket_table_blake3();
    if vf.bucket_blake3 != bucket_hash {
        return Err(format!(
            "VF.bucket_blake3 {} != 当前 bucket 表 {}（VF 与 bucket 表不同源）",
            hex32(&vf.bucket_blake3),
            hex32(&bucket_hash)
        ));
    }
    let indexer = Arc::clone(trainer.strategy_sum().indexer());
    if vf.total_rows != indexer.total_rows() {
        return Err(format!(
            "VF.total_rows {} != indexer.total_rows {}",
            vf.total_rows,
            indexer.total_rows()
        ));
    }
    eprintln!(
        "[aivat-eval] provenance OK：update_count={} bucket_b3={} strategy_b3={} VF(hands={},seed={})",
        trainer.update_count(),
        hex32(&bucket_hash),
        compute_strategy_blake3(&trainer, game),
        vf.hands,
        vf.seed
    );

    // ---- Hybrid σ 闭包（与 advisor strategy_fn Hybrid 分支字面一致）----
    let sigma_fn: Box<dyn Fn(InfoSetId) -> Vec<f64>> = Box::new(|info: InfoSetId| {
        if trainer.strategy_sum().row_sum_by_info(info) <= 0.0 {
            trainer.current_strategy(info)
        } else {
            trainer.average_strategy(info)
        }
    });

    let vffn = TableValueFn {
        tables: vf,
        indexer: Arc::clone(&indexer),
    };
    let estimator = AivatNlheEstimator::new(game, &vffn, sigma_fn);

    // ---- 流式跑日志 ----
    let text = std::fs::read_to_string(&args.strategy_log)
        .map_err(|e| format!("读 {} 失败: {e}", args.strategy_log.display()))?;

    let mut raw = Stat::default();
    let mut aivat = Stat::default();
    let mut diff = Stat::default();
    // 按位置（0=SB/button，1=BB）。
    let mut raw_pos = [Stat::default(), Stat::default()];
    let mut aivat_pos = [Stat::default(), Stat::default()];
    // 各修正类型。
    let mut c_deal_us = Stat::default();
    let mut c_deal_opp = Stat::default();
    let mut c_board = Stat::default();
    let mut c_runout = Stat::default();
    let mut c_act = Stat::default();

    let mut failures: u64 = 0;
    let mut printed_fail = 0u32;
    let mut processed = 0usize;

    let t0 = Instant::now();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Err(format!("第 {} 行 JSON 解析失败: {e}", lineno + 1)),
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("hand") {
            continue;
        }
        if let Some(lim) = args.limit {
            if processed >= lim {
                break;
            }
        }
        processed += 1;

        let input = match build_input(&v) {
            Ok(inp) => inp,
            Err(e) => {
                failures += 1;
                if printed_fail < 20 {
                    eprintln!("[fail] 第 {} 行 build_input: {e}", lineno + 1);
                    printed_fail += 1;
                }
                continue;
            }
        };
        match estimator.estimate_hand(&input) {
            Ok(r) => {
                let our_pos = r.our_pos;
                accumulate(
                    &r,
                    our_pos,
                    &mut raw,
                    &mut aivat,
                    &mut diff,
                    &mut raw_pos,
                    &mut aivat_pos,
                    &mut c_deal_us,
                    &mut c_deal_opp,
                    &mut c_board,
                    &mut c_runout,
                    &mut c_act,
                );
            }
            Err(e) => {
                failures += 1;
                if printed_fail < 20 {
                    eprintln!("[fail] 第 {} 行 estimate: {e}", lineno + 1);
                    printed_fail += 1;
                }
            }
        }
    }
    let wall = t0.elapsed();

    println!("\n========== AIVAT 评测报告 ==========");
    println!(
        "处理 {processed} 手 / 成功 {} / 失败 {failures} / wall {:.1}s",
        raw.n,
        wall.as_secs_f64()
    );
    if failures > 0 {
        println!("⚠️  有 {failures} 手失败——存在系统性 bug 或脏数据，下面的数字不可信，先修失败。");
    }
    println!("\n-- 总体 --");
    report("raw", &raw);
    report("AIVAT", &aivat);
    let se_ratio = if aivat.se() > 0.0 {
        raw.se() / aivat.se()
    } else {
        f64::NAN
    };
    println!(
        "  SE 缩减比 raw/AIVAT = {se_ratio:.3}x   方差缩减 = {:.2}x",
        se_ratio * se_ratio
    );

    println!("\n-- 配对差 d = AIVAT − raw（无偏闸门：|mean(d)| ≤ 1.96·SE(d)）--");
    let dm = diff.mean() * MBB;
    let dse = diff.se() * MBB;
    let pass = diff.mean().abs() <= 1.96 * diff.se();
    println!(
        "  mean(d)={dm:>10.2} mbb/g   SE(d)={dse:>8.2}   95% CI [{:>10.2}, {:>10.2}]   {}",
        dm - 1.96 * dse,
        dm + 1.96 * dse,
        if pass {
            "✅ 落在 CI 内"
        } else {
            "❌ 偏出 CI"
        }
    );

    println!("\n-- 按位置 --");
    for (pos, name) in [(0usize, "SB/button"), (1usize, "BB")] {
        println!("  [{name}]");
        report("  raw", &raw_pos[pos]);
        report("  AIVAT", &aivat_pos[pos]);
    }

    println!("\n-- 各修正类型均值（定位泄漏；每项应近 0 或解释得通）--");
    for (name, s) in [
        ("c_deal_us", &c_deal_us),
        ("c_deal_opp", &c_deal_opp),
        ("c_board", &c_board),
        ("c_runout", &c_runout),
        ("c_act", &c_act),
    ] {
        let m = s.mean() * MBB;
        let se = s.se() * MBB;
        println!(
            "  {name:<12} mean={m:>10.2} mbb/g   SE={se:>8.2}   95% CI [{:>10.2}, {:>10.2}]",
            m - 1.96 * se,
            m + 1.96 * se
        );
    }
    println!("====================================");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn accumulate(
    r: &HandResult,
    our_pos: usize,
    raw: &mut Stat,
    aivat: &mut Stat,
    diff: &mut Stat,
    raw_pos: &mut [Stat; 2],
    aivat_pos: &mut [Stat; 2],
    c_deal_us: &mut Stat,
    c_deal_opp: &mut Stat,
    c_board: &mut Stat,
    c_runout: &mut Stat,
    c_act: &mut Stat,
) {
    raw.push(r.raw);
    aivat.push(r.aivat);
    diff.push(r.aivat - r.raw);
    raw_pos[our_pos].push(r.raw);
    aivat_pos[our_pos].push(r.aivat);
    c_deal_us.push(r.c_deal_us);
    c_deal_opp.push(r.c_deal_opp);
    c_board.push(r.c_board);
    c_runout.push(r.c_runout);
    c_act.push(r.c_act);
}

/// 从一条 strategy 日志 JSON 记录抽 [`HandInput`]。
fn build_input(v: &Value) -> Result<HandInput, String> {
    let client_pos = v
        .get("client_pos")
        .and_then(|x| x.as_u64())
        .ok_or("缺 client_pos")? as u8;
    let our_hole = parse_pair(v.get("hole_cards").ok_or("缺 hole_cards")?)?;
    let opp_hole = parse_pair(v.get("bot_hole_cards").ok_or("缺 bot_hole_cards")?)?;
    let board = parse_card_list(v.get("board").ok_or("缺 board")?)?;
    let action = v
        .get("action")
        .and_then(|x| x.as_str())
        .ok_or("缺 action")?
        .to_string();
    let winnings = v
        .get("winnings")
        .and_then(|x| x.as_i64())
        .ok_or("缺 winnings")?;

    let decisions_json = v
        .get("decisions")
        .and_then(|x| x.as_array())
        .ok_or("缺 decisions")?;
    let mut log_decisions = Vec::with_capacity(decisions_json.len());
    for d in decisions_json {
        let info_set = d
            .get("info_set")
            .and_then(|x| x.as_u64())
            .ok_or("decision 缺 info_set")?;
        let chosen = d
            .get("chosen")
            .and_then(|x| x.as_str())
            .ok_or("decision 缺 chosen")?
            .to_string();
        let fallback_uniform = d
            .get("fallback_uniform")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let probs_obj = d
            .get("action_probs")
            .and_then(|x| x.as_object())
            .ok_or("decision 缺 action_probs")?;
        let mut probs_by_name = HashMap::with_capacity(probs_obj.len());
        for (k, val) in probs_obj {
            let p = val.as_f64().ok_or("action_probs 值非数字")?;
            probs_by_name.insert(k.clone(), p);
        }
        log_decisions.push(LoggedDecision {
            info_set,
            probs_by_name,
            chosen,
            fallback_uniform,
        });
    }

    Ok(HandInput {
        client_pos,
        our_hole,
        opp_hole,
        board,
        action,
        winnings,
        log_decisions,
    })
}

fn parse_pair(v: &Value) -> Result<[Card; 2], String> {
    let list = parse_card_list(v)?;
    if list.len() != 2 {
        return Err(format!("hole 必须 2 张，收到 {}", list.len()));
    }
    Ok([list[0], list[1]])
}

fn parse_card_list(v: &Value) -> Result<Vec<Card>, String> {
    let arr = v.as_array().ok_or("期望牌数组")?;
    arr.iter()
        .map(|c| {
            let s = c.as_str().ok_or("牌非字符串")?;
            parse_card(s)
        })
        .collect()
}

// ---- provenance helpers（与 advisor / nlhe_h3_report 逐行一致）----

fn compute_strategy_blake3(trainer: &DenseNlheEsMccfrTrainer, game: &SimplifiedNlheGame) -> String {
    let probes = collect_strategy_probes(game);
    let mut hasher = blake3::Hasher::new();
    hasher.update(&trainer.update_count().to_le_bytes());
    hasher.update(&(probes.len() as u64).to_le_bytes());
    for info in probes {
        let strat = trainer.average_strategy(info);
        hasher.update(&info.raw().to_le_bytes());
        hasher.update(&(strat.len() as u32).to_le_bytes());
        for p in strat {
            hasher.update(&p.to_le_bytes());
        }
    }
    hex32(hasher.finalize().as_bytes())
}

fn collect_strategy_probes(game: &SimplifiedNlheGame) -> Vec<InfoSetId> {
    let mut rng = ChaCha20Rng::from_seed(0x4833_5052_4f42_4553);
    let mut state = game.root(&mut rng);
    let mut probes = Vec::with_capacity(4096);
    for _ in 0..4096 {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => break,
            NodeKind::Chance => break,
            NodeKind::Player(actor) => {
                probes.push(SimplifiedNlheGame::info_set(&state, actor));
                let actions = SimplifiedNlheGame::legal_actions(&state);
                if actions.is_empty() {
                    break;
                }
                let idx = (rng.next_u64() as usize) % actions.len();
                state = SimplifiedNlheGame::next(state, actions[idx], &mut rng);
            }
        }
    }
    probes
}

fn hex32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
