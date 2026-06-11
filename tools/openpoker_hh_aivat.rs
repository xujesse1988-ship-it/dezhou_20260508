//! §4.2 live 半段：OpenPoker HH JSONL（driver `--hh-log`）→ 多人 AIVAT 报表（缺口⑥接真数据）。
//!
//! 每行经 [`hh_to_multiway_input`] 转换（单位 ×scale、动作转换重放）+
//! [`MultiwayAivatEstimator`] 分解；输出 raw / AIVAT 的 mean ± SE（**mbb/g**，经
//! [`chips_to_mbb_per_hand`]）+ 失败计数。失败 **loud**（逐行打 stderr）——转换/估计失败的手
//! 被排除时必须可见，静默丢弃 = selection bias。
//!
//! ```bash
//! cargo run --release --bin openpoker_hh_aivat -- \
//!   --hh-log openpoker_hh.jsonl [--vf1 artifacts/vf1_deal_table.json]
//! ```

use std::io::BufRead;
use std::process::ExitCode;

use poker::training::aivat_multiway::{
    chips_to_mbb_per_hand, MultiwayAivatEstimator, MultiwayDealValueFn, Vf1DealTable,
};
use poker::training::openpoker_hh::{hh_to_multiway_input, HhRecord};

const SOLVER_BB: u64 = 100;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[openpoker_hh_aivat] failed: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let mut hh_log = String::new();
    let mut vf1_path: Option<String> = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--hh-log" => hh_log = next(&mut it, "--hh-log")?,
            "--vf1" => vf1_path = Some(next(&mut it, "--vf1")?),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if hh_log.is_empty() {
        return Err("--hh-log is required".to_string());
    }

    let vf1: Option<Vf1DealTable> = match &vf1_path {
        Some(p) => {
            let t = Vf1DealTable::load(std::path::Path::new(p))?;
            eprintln!(
                "[openpoker_hh_aivat] VF-1 表: {} (blueprint={} hands={} 覆盖格 {}/{})",
                p,
                t.blueprint,
                t.hands_played,
                t.counts.iter().flatten().filter(|&&c| c > 0).count(),
                t.n_seats * 169
            );
            Some(t)
        }
        None => None,
    };
    let estimator =
        MultiwayAivatEstimator::new(vf1.as_ref().map(|t| t as &dyn MultiwayDealValueFn));

    let f = std::fs::File::open(&hh_log).map_err(|e| format!("打开 {hh_log} 失败: {e}"))?;
    let reader = std::io::BufReader::new(f);

    let mut raw_mbb: Vec<f64> = Vec::new();
    let mut aivat_mbb: Vec<f64> = Vec::new();
    let mut n_parse_err = 0u64;
    let mut n_convert_err = 0u64;
    let mut n_estimate_err = 0u64;
    let mut n_runout = 0u64;
    let mut runout_completions = 0u64;
    let mut unknown_folded = 0u64;
    let mut c_deal_used = 0u64;
    let mut n_short = 0u64;

    for (lineno, line) in reader.lines().enumerate() {
        let lineno = lineno + 1;
        let line = line.map_err(|e| format!("读第 {lineno} 行失败: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: HhRecord = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  [line {lineno}] JSON 解析失败: {e}");
                n_parse_err += 1;
                continue;
            }
        };
        let conv = match hh_to_multiway_input(&rec) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "  [line {lineno}{}] 转换失败: {e}",
                    rec.hand_id
                        .as_deref()
                        .map(|h| format!(" {h}"))
                        .unwrap_or_default()
                );
                n_convert_err += 1;
                continue;
            }
        };
        if conv.n_dealt < rec.num_seats as usize {
            n_short += 1;
        }
        match estimator.estimate_hand(&conv.input) {
            Ok(r) => {
                raw_mbb.push(chips_to_mbb_per_hand(r.raw, SOLVER_BB));
                aivat_mbb.push(chips_to_mbb_per_hand(r.aivat, SOLVER_BB));
                if r.has_runout {
                    n_runout += 1;
                    runout_completions += r.n_runout_completions;
                }
                unknown_folded += r.n_unknown_folded as u64;
                if r.c_deal_us != 0.0 {
                    c_deal_used += 1;
                }
            }
            Err(e) => {
                eprintln!(
                    "  [line {lineno}{}] AIVAT 估计失败: {e}",
                    conv.hand_id
                        .as_deref()
                        .map(|h| format!(" {h}"))
                        .unwrap_or_default()
                );
                n_estimate_err += 1;
            }
        }
    }

    let n_ok = raw_mbb.len();
    println!(
        "hands_ok={n_ok} failed: parse={n_parse_err} convert={n_convert_err} estimate={n_estimate_err}"
    );
    if n_ok == 0 {
        return Err("没有可估计的手（全部失败？检查上面的失败原因）".to_string());
    }
    println!(
        "short_handed={n_short} runout_hands={n_runout} (avg completions {:.0}) unknown_folded_total={unknown_folded} c_deal_active={c_deal_used}",
        if n_runout > 0 {
            runout_completions as f64 / n_runout as f64
        } else {
            0.0
        }
    );
    let (rm, rse) = mean_se(&raw_mbb);
    let (am, ase) = mean_se(&aivat_mbb);
    println!(
        "raw   mbb/g = {rm:+.1} ± {rse:.1}  CI95 [{:+.1}, {:+.1}]",
        rm - 1.96 * rse,
        rm + 1.96 * rse
    );
    println!(
        "aivat mbb/g = {am:+.1} ± {ase:.1}  CI95 [{:+.1}, {:+.1}]  (SE 缩减 ×{:.2})",
        am - 1.96 * ase,
        am + 1.96 * ase,
        if ase > 0.0 { rse / ase } else { f64::NAN }
    );
    Ok(())
}

fn mean_se(xs: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    if xs.len() < 2 {
        return (mean, 0.0);
    }
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1.0);
    (mean, (var / n).sqrt())
}

fn next(it: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{name} requires a value"))
}
