//! Stage 3 CFR / MCCFR 训练 CLI（H3 NLHE 路径）。
//!
//! H3 只要求补齐简化 heads-up NLHE blueprint 训练入口；Kuhn / Leduc 仍通过
//! 测试和专用 report 工具覆盖，不在本 CLI 中新增行为。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_betting_tree::{first_small_6max, first_small_preopen_6max};
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::{ConvergenceMonitor, EsMccfrTrainer, Game, StrategySnapshot, Trainer};
use poker::{
    BucketTable, ChaCha20Rng, CheckpointError, InfoSetId, RngSource, TableConfig, TrainerError,
};

#[derive(Debug)]
struct Args {
    game: String,
    /// game profile：`hu`（默认，HU 200BB {0.5,1,2}，byte-equal 历史行为）或
    /// `six-max`（6-max 100BB A3×A4 first-small，配 `postflop_cap`）。S4 6-max
    /// blueprint 训练走 `six-max`。
    profile: String,
    /// 仅 `--profile six-max` 生效：A3×A4 postflop width-redirect 上限 N（见
    /// `first_small_6max`）。限 `{2, 3, 4}`：N=2 = 树小供 smoke/调试、N=3 = 8.04 GiB@200
    /// 生产甜点（S3 桶复用精确验过 ≤3-way）；N=4 = 4-way postflop（sizing 1.445B infoset /
    /// 48 GiB 两表，需 ≥56 GiB 机；桶复用是 A1 6-way 数据下的优雅退化外推、非精确验证；
    /// 注意 `docs/six_max_nlhe_target.md` S4 续③ 实测 N=4 同 1B 预算仅 ~9% 覆盖率）。
    postflop_cap: u8,
    /// 仅 `--profile six-max`：betting 抽象 reshape（S4，治过度 limp + 开池档太大）。
    /// `none`（默认）= [`first_small_6max`]（baseline，230.5M infoset）；`nolimp` = 加禁
    /// 非 SB 开池 limp（55.2M infoset，缩树 4.2×）；`preopen` = 再加 2.25BB 开池档
    /// （157.9M infoset，full fix；见 [`first_small_preopen_6max`]）。
    reshape: String,
    trainer: String,
    updates: u64,
    seed: u64,
    checkpoint_dir: PathBuf,
    resume: Option<PathBuf>,
    checkpoint_every: u64,
    report_every: Option<u64>,
    keep_last: usize,
    bucket_table: PathBuf,
    threads: usize,
    /// 每 worker 单次 `step_parallel` 跑多少条 trajectory。1 = 原"每 worker
    /// 1 trajectory 然后 sync"行为；> 1 摊薄 rayon 调度 + `sched_yield` overhead，
    /// 是 NLHE 多核 scaling 的主 knob（详 `EsMccfrTrainer::step_parallel` doc）。
    /// 仅在 `--threads > 1` 路径生效。
    batch_per_worker: usize,
    quiet: bool,
    /// LCFR-MCCFR period 大小（None = vanilla ES-MCCFR）。Brown & Sandholm 2018
    /// §Discounted Monte Carlo CFR：period n 末 rescale × n/(n+1)。HUNL 推荐
    /// period ≈ 10⁶ updates（Brown 原 paper 10⁷ nodes / ~10 nodes per update）。
    /// 只在 cold start (`--resume` 不传) 生效；resume 不能 enable LCFR
    /// （EsMccfrTrainer 校验 update_count == 0）。
    lcfr_period: Option<u64>,
    /// 用 dense full-prealloc infoset 表（[`DenseNlheEsMccfrTrainer`]）替代默认
    /// HashMap 后端。两后端在同 seed 下 byte-equal（`tests/dense_nlhe_trainer.rs`
    /// 5 个对照）；dense 把 `HashMap<InfoSetId, Vec<f64>>` 换成两张扁平 `Vec<f64>`
    /// （当前 119.7M profile ~4.6 GiB），避免 HashMap collision + 几百 M infoset
    /// 扩张时的内存爆炸。checkpoint 走 dense raw v3 格式（与 HashMap ckpt 不互通，
    /// resume 必须 dense ckpt；LCFR 元数据随 dense ckpt 恢复）。
    dense: bool,
    /// 切到 dense **lock-free atomic** 并行路径（[`DenseNlheEsMccfrTrainer::step_parallel_lockfree`]，
    /// `docs/temp/nlhe_dense_parallel_merge_alternatives_2026_05_28.md` §A）。worker 直接 CAS
    /// add 主表，省 local delta + merge 阶段，throughput 高但 CAS race 顺序不定 →
    /// **跨 run 不再 byte-equal**（deterministic merge 路径仍是 byte-equal anchor）。
    /// 仅与 `--dense` 组合生效；`--threads 1` 走 `step()`，本旗等价 no-op。
    /// checkpoint / resume 与 deterministic dense 互通（表 layout 同；只是写入路径不同）。
    lockfree: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            game: String::new(),
            profile: "hu".to_string(),
            postflop_cap: 3,
            reshape: "none".to_string(),
            trainer: "es-mccfr".to_string(),
            updates: 0,
            seed: 0,
            checkpoint_dir: PathBuf::from("artifacts"),
            resume: None,
            checkpoint_every: 0,
            report_every: None,
            keep_last: 5,
            bucket_table: PathBuf::new(),
            threads: 1,
            batch_per_worker: 16,
            quiet: false,
            lcfr_period: None,
            dense: false,
            lockfree: false,
        }
    }
}

/// 训练后端抽象：HashMap 与 dense 两个 trainer 的 step/checkpoint 入口同型但不共享
/// trait（dense 是 NLHE 专属、不实现泛型 `Trainer<G>`）。本地 trait 让 CLI 的训练
/// 主循环只写一份，两后端走同一 [`drive`]，保证 throughput / checkpoint 节奏一致。
trait CfrBackend: StrategySnapshot {
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError>;
    fn step_parallel(
        &mut self,
        rng_pool: &mut [Box<dyn RngSource>],
        n_threads: usize,
        batch_per_worker: usize,
    ) -> Result<(), TrainerError>;
    fn update_count(&self) -> u64;
    fn save_checkpoint(&self, path: &Path) -> Result<(), CheckpointError>;
}

impl CfrBackend for EsMccfrTrainer<SimplifiedNlheGame> {
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        <Self as Trainer<SimplifiedNlheGame>>::step(self, rng)
    }
    fn step_parallel(
        &mut self,
        rng_pool: &mut [Box<dyn RngSource>],
        n_threads: usize,
        batch_per_worker: usize,
    ) -> Result<(), TrainerError> {
        EsMccfrTrainer::step_parallel(self, rng_pool, n_threads, batch_per_worker)
    }
    fn update_count(&self) -> u64 {
        <Self as Trainer<SimplifiedNlheGame>>::update_count(self)
    }
    fn save_checkpoint(&self, path: &Path) -> Result<(), CheckpointError> {
        <Self as Trainer<SimplifiedNlheGame>>::save_checkpoint(self, path)
    }
}

/// dense backend 包装：`lockfree` 旗子决定 `step_parallel` 分派到 deterministic
/// local-delta + merge ([`DenseNlheEsMccfrTrainer::step_parallel`]) 还是
/// lock-free atomic CAS ([`DenseNlheEsMccfrTrainer::step_parallel_lockfree`])。
/// 单线程路径不分流（`drive` 在 `args.threads == 1` 时走 `step()`）。
struct DenseBackend {
    inner: DenseNlheEsMccfrTrainer,
    lockfree: bool,
}

impl CfrBackend for DenseBackend {
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        self.inner.step(rng)
    }
    fn step_parallel(
        &mut self,
        rng_pool: &mut [Box<dyn RngSource>],
        n_threads: usize,
        batch_per_worker: usize,
    ) -> Result<(), TrainerError> {
        if self.lockfree {
            self.inner
                .step_parallel_lockfree(rng_pool, n_threads, batch_per_worker)
        } else {
            self.inner
                .step_parallel(rng_pool, n_threads, batch_per_worker)
        }
    }
    fn update_count(&self) -> u64 {
        self.inner.update_count()
    }
    fn save_checkpoint(&self, path: &Path) -> Result<(), CheckpointError> {
        self.inner.save_checkpoint(path)
    }
}

/// 监控只读快照委托给内层 dense trainer（`DenseNlheEsMccfrTrainer` 已在 lib 内实现
/// `StrategySnapshot`）；`lockfree` 旗子只影响写路径，查询路径不分流。
impl StrategySnapshot for DenseBackend {
    fn average_strategy_for(&self, info: InfoSetId) -> Vec<f64> {
        self.inner.average_strategy_for(info)
    }
    fn regret_for(&self, info: InfoSetId) -> Vec<f64> {
        self.inner.regret_for(info)
    }
    fn visited_infosets(&self) -> u64 {
        self.inner.visited_infosets()
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[train_cfr] failed: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if args.game != "nlhe" {
        return Err("H3 train_cfr currently supports only --game nlhe".to_string());
    }
    if args.trainer != "es-mccfr" {
        return Err("H3 NLHE path supports only --trainer es-mccfr".to_string());
    }
    if args.updates == 0 {
        return Err("--updates must be > 0".to_string());
    }
    if args.threads == 0 {
        return Err("--threads must be > 0".to_string());
    }
    if args.batch_per_worker == 0 {
        return Err("--batch-per-worker must be > 0".to_string());
    }
    if args.report_every == Some(0) {
        return Err("--report-every must be > 0 when provided".to_string());
    }
    if args.bucket_table.as_os_str().is_empty() {
        return Err("--bucket-table is required for --game nlhe".to_string());
    }
    if args.lockfree && !args.dense {
        return Err(
            "--lockfree requires --dense: lock-free atomic 路径仅 DenseNlheEsMccfrTrainer 实现 \
             (HashMap 后端无 step_parallel_lockfree)。"
                .to_string(),
        );
    }
    fs::create_dir_all(&args.checkpoint_dir)
        .map_err(|e| format!("create checkpoint dir failed: {e}"))?;

    let table = Arc::new(BucketTable::open(&args.bucket_table).map_err(|e| {
        format!(
            "BucketTable::open({}) failed: {e:?}",
            args.bucket_table.display()
        )
    })?);
    // profile 分派：hu = 历史默认（byte-equal）；six-max = A3×A4 first-small N-cap 游戏。
    // 两条路都产 `SimplifiedNlheGame`，下游 dense/HashMap 后端 + drive 主循环不分流。
    let game = match args.profile.as_str() {
        "hu" => SimplifiedNlheGame::new(Arc::clone(&table))
            .map_err(|e| format!("SimplifiedNlheGame::new failed: {e:?}"))?,
        "six-max" => {
            if !matches!(args.postflop_cap, 2..=4) {
                return Err(format!(
                    "--postflop-cap must be 2, 3, or 4 for --profile six-max (A3×A4 cap)，got {}",
                    args.postflop_cap
                ));
            }
            let (abs, rules) = match args.reshape.as_str() {
                "none" => first_small_6max(args.postflop_cap),
                "nolimp" => {
                    let (a, mut r) = first_small_6max(args.postflop_cap);
                    r.no_open_limp = true;
                    (a, r)
                }
                "preopen" => first_small_preopen_6max(args.postflop_cap),
                other => {
                    return Err(format!(
                        "unknown --reshape {other} (expected none | nolimp | preopen)"
                    ))
                }
            };
            SimplifiedNlheGame::new_with_abstraction(
                Arc::clone(&table),
                TableConfig::default_6max_100bb(),
                abs,
                rules,
            )
            .map_err(|e| {
                format!("SimplifiedNlheGame::new_with_abstraction (six-max) failed: {e:?}")
            })?
        }
        other => return Err(format!("unknown --profile {other} (expected hu | six-max)")),
    };
    let table_hash = hex32(&table.content_hash());

    if !args.quiet {
        eprintln!("[train_cfr] profile          = {}", args.profile);
        if args.profile == "six-max" {
            eprintln!(
                "[train_cfr] postflop_cap     = {} (A3×A4 width-redirect N)",
                args.postflop_cap
            );
            eprintln!("[train_cfr] reshape          = {}", args.reshape);
        }
        eprintln!("[train_cfr] n_players        = {}", game.n_players());
        eprintln!("[train_cfr] tree_nodes       = {}", game.tree().num_nodes());
    }

    // 收敛监控器（S4）：训练前从 game 取 preflop 根节点 × 169 手型类样本，drive 主循环
    // 在 report 间隔观测 average-regret / entropy / 动作概率震荡。HU 与 6-max 都建
    // （HU 根 = SB 开局；监控只读不触训练状态 → byte-equal 不破）。
    let monitor = ConvergenceMonitor::for_game(&game);

    // 后端选择：dense full-prealloc 表 or 默认 HashMap。两者 byte-equal（见 Args.dense
    // doc），训练主循环共用 `drive`——只构造入口不同。dense 路径下 --lockfree 进一步
    // 切到 lock-free atomic CAS（详 Args.lockfree doc / DenseBackend）。
    if args.dense {
        let inner = build_dense(&args, game)?;
        let mut trainer = DenseBackend {
            inner,
            lockfree: args.lockfree,
        };
        let label = if args.lockfree {
            "dense-lockfree-es-mccfr"
        } else {
            "dense-es-mccfr"
        };
        drive(&mut trainer, &args, &table_hash, label, monitor)
    } else {
        let mut trainer = build_hashmap(&args, game)?;
        drive(&mut trainer, &args, &table_hash, "es-mccfr", monitor)
    }
}

/// 构造 HashMap 后端（默认路径，行为与改动前一致）。
fn build_hashmap(
    args: &Args,
    game: SimplifiedNlheGame,
) -> Result<EsMccfrTrainer<SimplifiedNlheGame>, String> {
    if let Some(ref resume) = args.resume {
        if args.lcfr_period.is_some() {
            return Err(
                "--lcfr-period cannot be combined with --resume: LCFR period state 不存 \
                 checkpoint，resume 路径默认回退 vanilla（详 EsMccfrTrainer::load_checkpoint doc）。\
                 production 路径走 cold start。"
                    .to_string(),
            );
        }
        <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::load_checkpoint(
            resume, game,
        )
        .map_err(|e| format!("load checkpoint {} failed: {e:?}", resume.display()))
    } else {
        let base = EsMccfrTrainer::new(game, args.seed);
        Ok(match args.lcfr_period {
            Some(period) => base.with_lcfr_period(period),
            None => base,
        })
    }
}

/// 构造 dense 后端。resume 走 dense raw v3 ckpt（`DenseNlheEsMccfrTrainer::load_checkpoint`
/// 随 ckpt 恢复 LCFR 元数据，故 dense resume 不需要、也不接受 `--lcfr-period`）。
/// 从旧 HashMap ckpt 迁移用 `from_hashmap_checkpoint`，不在本 CLI 路径暴露。
fn build_dense(args: &Args, game: SimplifiedNlheGame) -> Result<DenseNlheEsMccfrTrainer, String> {
    if let Some(ref resume) = args.resume {
        if args.lcfr_period.is_some() {
            return Err(
                "--lcfr-period cannot be combined with --resume (--dense): dense ckpt 已存 \
                 LCFR period state，resume 自动续；显式传 --lcfr-period 会与恢复的状态冲突。"
                    .to_string(),
            );
        }
        DenseNlheEsMccfrTrainer::load_checkpoint(resume, game)
            .map_err(|e| format!("load dense checkpoint {} failed: {e:?}", resume.display()))
    } else {
        let base = DenseNlheEsMccfrTrainer::new(game, args.seed);
        Ok(match args.lcfr_period {
            Some(period) => base.with_lcfr_period(period),
            None => base,
        })
    }
}

/// 训练主循环（后端无关）：banner → step/step_parallel 直到 `args.updates` →
/// 周期 checkpoint + 最终 checkpoint。两后端共用以保证 throughput / checkpoint 节奏
/// 一致；唯一差异是 `trainer` 的具体类型。
fn drive<T: CfrBackend>(
    trainer: &mut T,
    args: &Args,
    table_hash: &str,
    backend_label: &str,
    mut monitor: ConvergenceMonitor,
) -> Result<(), String> {
    let start_update = trainer.update_count();
    if start_update >= args.updates {
        if !args.quiet {
            eprintln!(
                "[train_cfr] checkpoint already at update_count={} >= target {}; no-op",
                start_update, args.updates
            );
        }
        save_checkpoint(trainer, &args.checkpoint_dir, "final")?;
        return Ok(());
    }

    if !args.quiet {
        eprintln!("[train_cfr] game             = nlhe");
        eprintln!("[train_cfr] trainer          = {backend_label}");
        eprintln!("[train_cfr] target_updates   = {}", args.updates);
        eprintln!("[train_cfr] start_update     = {start_update}");
        eprintln!("[train_cfr] seed             = 0x{:016x}", args.seed);
        eprintln!("[train_cfr] threads          = {}", args.threads);
        eprintln!(
            "[train_cfr] batch_per_worker = {} (effective per step_parallel = {})",
            args.batch_per_worker,
            args.threads * args.batch_per_worker,
        );
        eprintln!(
            "[train_cfr] bucket_table     = {}",
            args.bucket_table.display()
        );
        eprintln!("[train_cfr] bucket_blake3    = {table_hash}");
        eprintln!(
            "[train_cfr] checkpoint_dir   = {}",
            args.checkpoint_dir.display()
        );
        if let Some(report_every) = args.report_every {
            eprintln!("[train_cfr] report_every     = {report_every}");
        }
        match args.lcfr_period {
            Some(p) => eprintln!("[train_cfr] lcfr_period      = {p} (LCFR-MCCFR)"),
            None => eprintln!("[train_cfr] lcfr_period      = none (vanilla ES-MCCFR)"),
        }
        if args.dense {
            let mode = if args.lockfree {
                "lockfree (atomic CAS; not byte-equal)"
            } else {
                "deterministic merge (byte-equal anchor)"
            };
            eprintln!("[train_cfr] dense_parallel   = {mode}");
        }
    }

    let mut single_rng = ChaCha20Rng::from_seed(args.seed);
    let mut rng_pool: Vec<Box<dyn RngSource>> = (0..args.threads as u64)
        .map(|tid| {
            let seed = mix3(args.seed, 0x5448_5244, tid);
            Box::new(ChaCha20Rng::from_seed(seed)) as Box<dyn RngSource>
        })
        .collect();

    let t0 = Instant::now();
    let default_report_every = args
        .checkpoint_every
        .max((args.updates - start_update).saturating_div(10).max(1));
    let report_every = args.report_every.unwrap_or(default_report_every);
    let mut next_report = start_update.saturating_add(report_every);
    let mut next_checkpoint = match start_update.checked_div(args.checkpoint_every) {
        Some(n) => (n + 1) * args.checkpoint_every,
        None => u64::MAX,
    };
    let mut saved_this_run: Vec<PathBuf> = Vec::new();

    while trainer.update_count() < args.updates {
        let remaining = args.updates - trainer.update_count();
        if args.threads == 1 {
            trainer
                .step(&mut single_rng)
                .map_err(|e| format!("step failed at update {}: {e:?}", trainer.update_count()))?;
        } else {
            // 单次 step_parallel 产 n × batch 个 update。floor batch 保证
            // n × batch ≤ remaining，绝不越过 args.updates；不足一整批的尾数
            // （remaining % n）留到下一轮——下一轮 n 缩到该尾数后 batch = 1 收尾，
            // 精确命中 args.updates（div_ceil 会 round-up 越界，见 P1 修复）。
            let n = args.threads.min(remaining as usize).max(1);
            let batch = ((remaining / n as u64).min(args.batch_per_worker as u64) as usize).max(1);
            trainer
                .step_parallel(&mut rng_pool, n, batch)
                .map_err(|e| {
                    format!(
                        "step_parallel failed at update {}: {e:?}",
                        trainer.update_count()
                    )
                })?;
        }

        let cur = trainer.update_count();
        if !args.quiet && cur >= next_report {
            let elapsed = t0.elapsed().as_secs_f64();
            let throughput = (cur - start_update) as f64 / elapsed.max(1e-9);
            eprintln!(
                "[train_cfr] update {cur} / {} elapsed={elapsed:.1}s throughput={throughput:.0}/s",
                args.updates
            );
            // 收敛监控（S4）：preflop 根 × 169 手型类样本上的 entropy / 平均正 regret /
            // average-strategy L1 漂移。只读快照，不触训练状态。
            let report = monitor.observe(cur, &*trainer);
            eprintln!("{report}");
            while next_report <= cur {
                next_report = next_report.saturating_add(report_every);
            }
        }

        if args.checkpoint_every > 0 && cur >= next_checkpoint {
            let path = save_checkpoint(trainer, &args.checkpoint_dir, "auto")?;
            saved_this_run.push(path);
            prune_saved(&mut saved_this_run, args.keep_last);
            while next_checkpoint <= cur {
                next_checkpoint = next_checkpoint.saturating_add(args.checkpoint_every);
            }
        }
    }

    let final_path = save_checkpoint(trainer, &args.checkpoint_dir, "final")?;
    if !args.quiet {
        let elapsed = t0.elapsed().as_secs_f64();
        let throughput = (trainer.update_count() - start_update) as f64 / elapsed.max(1e-9);
        eprintln!(
            "[train_cfr] done update_count={} wall={elapsed:.1}s throughput={throughput:.0}/s final={}",
            trainer.update_count(),
            final_path.display()
        );
    }
    Ok(())
}

fn parse_args() -> Result<Args, String> {
    let mut out = Args::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--game" => out.game = next_value(&mut args, "--game")?,
            "--profile" => out.profile = next_value(&mut args, "--profile")?,
            "--postflop-cap" => {
                out.postflop_cap = parse_u64(&next_value(&mut args, "--postflop-cap")?)? as u8
            }
            "--reshape" => out.reshape = next_value(&mut args, "--reshape")?,
            "--trainer" => out.trainer = next_value(&mut args, "--trainer")?,
            "--updates" => out.updates = parse_u64(&next_value(&mut args, "--updates")?)?,
            "--iter" => {
                let _ = next_value(&mut args, "--iter")?;
            }
            "--seed" => out.seed = parse_u64(&next_value(&mut args, "--seed")?)?,
            "--checkpoint-dir" => {
                out.checkpoint_dir = PathBuf::from(next_value(&mut args, "--checkpoint-dir")?)
            }
            "--resume" => out.resume = Some(PathBuf::from(next_value(&mut args, "--resume")?)),
            "--checkpoint-every" => {
                out.checkpoint_every = parse_u64(&next_value(&mut args, "--checkpoint-every")?)?
            }
            "--report-every" => {
                out.report_every = Some(parse_u64(&next_value(&mut args, "--report-every")?)?)
            }
            "--keep-last" => {
                out.keep_last = parse_u64(&next_value(&mut args, "--keep-last")?)? as usize
            }
            "--bucket-table" => {
                out.bucket_table = PathBuf::from(next_value(&mut args, "--bucket-table")?)
            }
            "--threads" => out.threads = parse_u64(&next_value(&mut args, "--threads")?)? as usize,
            "--batch-per-worker" => {
                out.batch_per_worker =
                    parse_u64(&next_value(&mut args, "--batch-per-worker")?)? as usize
            }
            "--quiet" => out.quiet = true,
            "--lcfr-period" => {
                out.lcfr_period = Some(parse_u64(&next_value(&mut args, "--lcfr-period")?)?)
            }
            "--dense" => out.dense = true,
            "--lockfree" => out.lockfree = true,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if out.game.is_empty() {
        return Err("--game is required".to_string());
    }
    Ok(out)
}

fn next_value(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{name} requires a value"))
}

fn parse_u64(raw: &str) -> Result<u64, String> {
    if let Some(hex) = raw.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).map_err(|e| format!("invalid hex integer {raw}: {e}"))
    } else {
        raw.parse::<u64>()
            .map_err(|e| format!("invalid integer {raw}: {e}"))
    }
}

fn save_checkpoint<T: CfrBackend>(trainer: &T, dir: &Path, label: &str) -> Result<PathBuf, String> {
    let path = dir.join(format!(
        "nlhe_es_mccfr_{label}_{:012}.ckpt",
        trainer.update_count()
    ));
    trainer
        .save_checkpoint(&path)
        .map_err(|e| format!("save checkpoint {} failed: {e:?}", path.display()))?;
    Ok(path)
}

fn prune_saved(saved: &mut Vec<PathBuf>, keep_last: usize) {
    if keep_last == 0 {
        return;
    }
    while saved.len() > keep_last {
        let old = saved.remove(0);
        let _ = fs::remove_file(old);
    }
}

fn hex32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn mix3(seed: u64, a: u64, b: u64) -> u64 {
    let mut x =
        seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn print_usage() {
    eprintln!(
        "usage: cargo run --release --bin train_cfr -- \\\n\
         \t--game nlhe --trainer es-mccfr --bucket-table <path> --updates <N> [options]\n\n\
         options:\n\
         \t--profile <hu|six-max>  (default hu; six-max = 6-max 100BB A3×A4 first-small)\n\
         \t--postflop-cap <2|3|4>  (six-max only; A3×A4 width-redirect N, default 3; N=4 = 48 GiB tables)\n\
         \t--seed <N|0xHEX>\n\
         \t--threads <N>\n\
         \t--batch-per-worker <N>  (default 16; trajectories per worker per step_parallel dispatch)\n\
         \t--resume <checkpoint>\n\
         \t--checkpoint-dir <dir>\n\
         \t--checkpoint-every <N>\n\
         \t--report-every <N>\n\
         \t--keep-last <N>\n\
         \t--lcfr-period <N>  (cold start only; LCFR-MCCFR period rescale)\n\
         \t--dense  (dense full-prealloc infoset table backend; byte-equal to default HashMap)\n\
         \t--lockfree  (with --dense: lock-free atomic CAS parallel path; not byte-equal across runs)\n\
         \t--quiet"
    );
}
