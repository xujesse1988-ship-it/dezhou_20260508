//! `Trainer` trait + `VanillaCfrTrainer` + `EsMccfrTrainer`（API-310..API-313）。
//!
//! `Trainer<G: Game>` 统一 interface：[`Trainer::step`] 执行 1 iter（Vanilla CFR）
//! 或 1 update（ES-MCCFR）；[`Trainer::save_checkpoint`] / [`Trainer::load_checkpoint`]
//! 走 [`crate::training::Checkpoint`] 二进制 schema（D-350 / API-350）；
//! [`Trainer::current_strategy`] / [`Trainer::average_strategy`] stateless 查询
//! （D-328）。
//!
//! VanillaCfrTrainer for Kuhn / Leduc（D-300 Zinkevich 2007 详解伪代码）；
//! EsMccfrTrainer for 简化 NLHE（D-301 Lanctot 2009 详解伪代码 + D-321 多线程
//! thread-safety 模型 deferred 到 C2 \[实现\] 起步前 lock）。
//!
//! B2 \[实现\] 落地 [`VanillaCfrTrainer`] 全部 Trainer 方法（除 save/load checkpoint
//! 走 D2 \[实现\]）；[`EsMccfrTrainer`] 保持 `unimplemented!()`（C2 \[实现\] 落地）。

use std::path::Path;

use crate::core::rng::RngSource;
use crate::error::{CheckpointError, TrainerError};
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::regret::{RegretTable, StrategyAccumulator};
use crate::training::sampling::{derive_substream_seed, sample_discrete};

/// 训练器统一 trait（API-310 / D-371）。
pub trait Trainer<G: Game> {
    /// 执行 1 iter 训练（Vanilla CFR）或 1 update（ES-MCCFR D-307 alternating）。
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError>;

    /// 当前 InfoSet 上的 current strategy（regret matching；D-303 标准 RM）。
    fn current_strategy(&self, info_set: &G::InfoSet) -> Vec<f64>;

    /// 当前 InfoSet 上的 average strategy（strategy_sum 归一化；D-304 标准累积）。
    fn average_strategy(&self, info_set: &G::InfoSet) -> Vec<f64>;

    /// 已完成 iter / update 数（Vanilla CFR: iter；ES-MCCFR: per-player update）。
    fn update_count(&self) -> u64;

    /// 写出 checkpoint（D-353 write-to-temp + atomic rename + D-352 trailer BLAKE3）。
    fn save_checkpoint(&self, path: &Path) -> Result<(), CheckpointError>;

    /// 从 checkpoint 恢复（D-350 schema 校验 + D-352 eager BLAKE3 + D-356 多
    /// game 不兼容拒绝）。
    fn load_checkpoint(path: &Path, game: G) -> Result<Self, CheckpointError>
    where
        Self: Sized;
}

/// Vanilla CFR Trainer（API-311 / D-300）。
///
/// `rng_substream_seed` 是 master_seed 经 SplitMix64 finalizer × 4 派生的 32 byte
/// ChaCha20Rng seed（D-335），目前由 D2 \[实现\] checkpoint 序列化路径消费；B2
/// \[实现\] step 走 full-tree 全确定性枚举不消费 rng，因此本字段在 B2 阶段仅占位
/// 落表（`#[allow(dead_code)]` 在 D2 \[实现\] 落地后取消）。
pub struct VanillaCfrTrainer<G: Game> {
    pub(crate) game: G,
    pub(crate) regret: RegretTable<G::InfoSet>,
    pub(crate) strategy_sum: StrategyAccumulator<G::InfoSet>,
    pub(crate) iter: u64,
    #[allow(dead_code)] // D2 \[实现\] checkpoint 落地后取消
    pub(crate) rng_substream_seed: [u8; 32],
}

impl<G: Game> VanillaCfrTrainer<G> {
    /// 新建空 Trainer。`master_seed` 用 D-335 SplitMix64 finalizer × 4 派生 32 byte
    /// sub-stream seed 占位（Vanilla CFR full-tree 全确定性枚举，sub-stream seed
    /// 仅在 D2 \[实现\] checkpoint 序列化时存档；step 本身不消费）。
    pub fn new(game: G, master_seed: u64) -> Self {
        let rng_substream_seed = derive_substream_seed(master_seed, 0, 0);
        Self {
            game,
            regret: RegretTable::new(),
            strategy_sum: StrategyAccumulator::new(),
            iter: 0,
            rng_substream_seed,
        }
    }
}

impl<G: Game> Trainer<G> for VanillaCfrTrainer<G> {
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        // D-300：alternating traverser × 完整博弈树 DFS × cfv 累积 × regret update
        // × strategy_sum 累积。每 step 内部 traverser ∈ [0, n_players) 各遍历 1 次。
        let n_players = self.game.n_players();
        let root = self.game.root(rng);
        for traverser in 0..n_players as u8 {
            recurse_vanilla::<G>(
                root.clone(),
                traverser,
                1.0,
                1.0,
                &mut self.regret,
                &mut self.strategy_sum,
                rng,
            );
        }
        self.iter += 1;
        Ok(())
    }

    fn current_strategy(&self, info_set: &G::InfoSet) -> Vec<f64> {
        let n = self
            .regret
            .inner()
            .get(info_set)
            .map(|v| v.len())
            .or_else(|| self.strategy_sum.inner().get(info_set).map(|v| v.len()))
            .unwrap_or(0);
        if n == 0 {
            return Vec::new();
        }
        self.regret.current_strategy(info_set, n)
    }

    fn average_strategy(&self, info_set: &G::InfoSet) -> Vec<f64> {
        let n = self
            .strategy_sum
            .inner()
            .get(info_set)
            .map(|v| v.len())
            .or_else(|| self.regret.inner().get(info_set).map(|v| v.len()))
            .unwrap_or(0);
        if n == 0 {
            return Vec::new();
        }
        self.strategy_sum.average_strategy(info_set, n)
    }

    fn update_count(&self) -> u64 {
        self.iter
    }

    fn save_checkpoint(&self, _path: &Path) -> Result<(), CheckpointError> {
        unimplemented!("stage 3 B2 scaffold: VanillaCfrTrainer::save_checkpoint (D2 实现)")
    }

    fn load_checkpoint(_path: &Path, _game: G) -> Result<Self, CheckpointError>
    where
        Self: Sized,
    {
        unimplemented!("stage 3 B2 scaffold: VanillaCfrTrainer::load_checkpoint (D2 实现)")
    }
}

/// Vanilla CFR DFS recurse（D-300 详解伪代码）。
///
/// 返回 traverser 视角的 cfv（counterfactual value）。
fn recurse_vanilla<G: Game>(
    state: G::State,
    traverser: PlayerId,
    pi_trav: f64,
    pi_opp: f64,
    regret: &mut RegretTable<G::InfoSet>,
    strategy_sum: &mut StrategyAccumulator<G::InfoSet>,
    rng: &mut dyn RngSource,
) -> f64 {
    match G::current(&state) {
        NodeKind::Terminal => G::payoff(&state, traverser),
        NodeKind::Chance => {
            let dist = G::chance_distribution(&state);
            let mut value = 0.0;
            for (action, prob) in dist {
                let next_state = G::next(state.clone(), action, rng);
                value += prob
                    * recurse_vanilla::<G>(
                        next_state,
                        traverser,
                        pi_trav,
                        pi_opp,
                        regret,
                        strategy_sum,
                        rng,
                    );
            }
            value
        }
        NodeKind::Player(actor) => {
            let info = G::info_set(&state, actor);
            let actions = G::legal_actions(&state);
            let n = actions.len();
            // ensure regret slot exists with correct length (D-324)
            regret.get_or_init(info.clone(), n);
            let sigma = regret.current_strategy(&info, n);

            if actor == traverser {
                // traverser node：枚举每个 action 的 cfv，累积 regret + strategy_sum
                let mut cfvs = Vec::with_capacity(n);
                for (i, action) in actions.iter().enumerate() {
                    let next_state = G::next(state.clone(), *action, rng);
                    let cfv = recurse_vanilla::<G>(
                        next_state,
                        traverser,
                        pi_trav * sigma[i],
                        pi_opp,
                        regret,
                        strategy_sum,
                        rng,
                    );
                    cfvs.push(cfv);
                }
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                // regret update R(I, a) += π_opp × (cfv_a - σ_node)
                let delta: Vec<f64> = cfvs.iter().map(|c| pi_opp * (c - sigma_value)).collect();
                regret.accumulate(info.clone(), &delta);
                // strategy_sum update S(I, a) += π_traverser × σ(I, a)
                let weighted: Vec<f64> = sigma.iter().map(|s| pi_trav * s).collect();
                strategy_sum.accumulate(info, &weighted);
                sigma_value
            } else {
                // opponent node：σ 加权累计 cfv，opp reach probability 乘 σ(a)
                let mut value = 0.0;
                for (i, action) in actions.iter().enumerate() {
                    let next_state = G::next(state.clone(), *action, rng);
                    value += sigma[i]
                        * recurse_vanilla::<G>(
                            next_state,
                            traverser,
                            pi_trav,
                            pi_opp * sigma[i],
                            regret,
                            strategy_sum,
                            rng,
                        );
                }
                value
            }
        }
    }
}

/// External-Sampling MCCFR Trainer（API-312 / D-301）。
///
/// **D-321-rev1 lock**（2026-05-13，C2 \[实现\] 起步前；详见
/// `pluribus_stage3_decisions.md` §10.2）：thread-safety 模型 = thread-local
/// accumulator + batch merge（候选 ③）。C2 commit ship serial-equivalent
/// `step_parallel`（在 rng_pool 上循环 single-threaded step；不引入真并发同步），
/// 真并发实现 deferred 到 E2 \[实现\]。`rng_substream_seed` 字段由 D2 \[实现\]
/// checkpoint 序列化路径消费；C2 阶段不参与 step 路径，`#[allow(dead_code)]`
/// 占位（与 `VanillaCfrTrainer::rng_substream_seed` 同型）。
pub struct EsMccfrTrainer<G: Game> {
    pub(crate) game: G,
    pub(crate) regret: RegretTable<G::InfoSet>,
    pub(crate) strategy_sum: StrategyAccumulator<G::InfoSet>,
    pub(crate) update_count: u64,
    #[allow(dead_code)] // D2 \[实现\] checkpoint 落地后取消
    pub(crate) rng_substream_seed: [u8; 32],
}

impl<G: Game> EsMccfrTrainer<G> {
    /// 新建空 Trainer（API-312）。`master_seed` 用 D-335 SplitMix64 finalizer ×
    /// 4 派生 32 byte sub-stream seed 占位（D2 checkpoint 序列化时存档；step
    /// 本身不消费——`step` 接受的 `rng: &mut dyn RngSource` 是唯一 randomness
    /// 来源）。
    pub fn new(game: G, master_seed: u64) -> Self {
        let rng_substream_seed = derive_substream_seed(master_seed, 0, 0);
        Self {
            game,
            regret: RegretTable::new(),
            strategy_sum: StrategyAccumulator::new(),
            update_count: 0,
            rng_substream_seed,
        }
    }

    /// 多线程并发 step（D-321-rev1 lock）。
    ///
    /// **C2 \[实现\] 形态（serial-equivalent fallback）**：在 `rng_pool` 上循环
    /// single-threaded `step`；忽略 `n_threads` 参数。该路径让 step_parallel
    /// 调用方在 C2 阶段获得正确 byte-equal 结果（单次 step_parallel 调用 =
    /// `rng_pool.len()` 次 step；alternating traverser 在每次 step 内自然
    /// 触发）。E2 \[实现\] 落地真并发后翻面为 thread-local accumulator + batch
    /// merge（候选 ③）真路径；外部签名不变。
    pub fn step_parallel(
        &mut self,
        rng_pool: &mut [Box<dyn RngSource>],
        _n_threads: usize,
    ) -> Result<(), TrainerError> {
        for rng in rng_pool.iter_mut() {
            self.step(rng.as_mut())?;
        }
        Ok(())
    }
}

impl<G: Game> Trainer<G> for EsMccfrTrainer<G> {
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        // D-307 alternating traverser：iter t 上 traverser = (t mod n_players)。
        let n_players = self.game.n_players() as u64;
        let traverser = (self.update_count % n_players) as PlayerId;
        let root = self.game.root(rng);
        recurse_es::<G>(
            root,
            traverser,
            1.0,
            1.0,
            &mut self.regret,
            &mut self.strategy_sum,
            rng,
        );
        self.update_count += 1;
        Ok(())
    }

    fn current_strategy(&self, info_set: &G::InfoSet) -> Vec<f64> {
        let n = self
            .regret
            .inner()
            .get(info_set)
            .map(|v| v.len())
            .or_else(|| self.strategy_sum.inner().get(info_set).map(|v| v.len()))
            .unwrap_or(0);
        if n == 0 {
            return Vec::new();
        }
        self.regret.current_strategy(info_set, n)
    }

    fn average_strategy(&self, info_set: &G::InfoSet) -> Vec<f64> {
        let n = self
            .strategy_sum
            .inner()
            .get(info_set)
            .map(|v| v.len())
            .or_else(|| self.regret.inner().get(info_set).map(|v| v.len()))
            .unwrap_or(0);
        if n == 0 {
            return Vec::new();
        }
        self.strategy_sum.average_strategy(info_set, n)
    }

    fn update_count(&self) -> u64 {
        self.update_count
    }

    fn save_checkpoint(&self, _path: &Path) -> Result<(), CheckpointError> {
        unimplemented!("stage 3 C2 scaffold: EsMccfrTrainer::save_checkpoint (D2 实现)")
    }

    fn load_checkpoint(_path: &Path, _game: G) -> Result<Self, CheckpointError>
    where
        Self: Sized,
    {
        unimplemented!("stage 3 C2 scaffold: EsMccfrTrainer::load_checkpoint (D2 实现)")
    }
}

/// External-Sampling MCCFR DFS recurse（D-301 详解伪代码）。
///
/// 返回值语义（D-301 详解）：
/// - terminal：`utility(state, traverser) / π_traverser`（importance weighting）
/// - traverser decision：`Σ_a σ(I, a) × v_a`（σ-加权 cfv 之和）
/// - non-traverser decision：sampled action 路径上的 recursed value
///
/// 参数：
/// - `state`：当前 owned 状态（D-319 owned clone state representation）
/// - `traverser`：本 step 的 traverser（D-307 alternating）
/// - `pi_trav` / `pi_opp`：当前节点 reach probability 分解（不含 chance）
/// - `regret` / `strategy_sum`：可变借用累积容器
/// - `rng`：chance + opp action sampling 共享 rng（D-315 显式注入）
fn recurse_es<G: Game>(
    state: G::State,
    traverser: PlayerId,
    pi_trav: f64,
    pi_opp: f64,
    regret: &mut RegretTable<G::InfoSet>,
    strategy_sum: &mut StrategyAccumulator<G::InfoSet>,
    rng: &mut dyn RngSource,
) -> f64 {
    match G::current(&state) {
        NodeKind::Terminal => {
            // D-301 详解：terminal 返回 `utility / π_traverser`（importance
            // weighting：traverser sampled reach 倒数）。`pi_trav > 0` 在
            // 任意 traverser-reachable terminal 上恒成立——traverser branch
            // 内每个 action 沿 σ(a) 走，σ(a) 在 D-331 退化局面回退均匀分布
            // (1/n_actions) > 0，避免 zero division。
            let u = G::payoff(&state, traverser);
            if pi_trav > 0.0 {
                u / pi_trav
            } else {
                // 防御：π_traverser == 0 实际不可达（recurse_es 入口 π_trav =
                // 1.0，每次乘 σ(a) > 0）；触发即视作算法 bug，但 stage 3
                // 早期 carve-out 允许 fail-safe 返回 raw utility 让训练继续。
                u
            }
        }
        NodeKind::Chance => {
            // D-308 chance sample-1：在 chance_distribution 上采样 1 outcome，
            // 递归继续。chance node 不影响 π_trav / π_opp（chance 概率仅在
            // sampling 阶段隐含通过 1 / dist[i] importance correction 处理，但
            // ES-MCCFR D-308 中 chance 是单 1-sample 不做 importance correction，
            // 因此 π 不更新）。
            let dist = G::chance_distribution(&state);
            let action = sample_discrete(&dist, rng);
            let next_state = G::next(state, action, rng);
            recurse_es::<G>(
                next_state,
                traverser,
                pi_trav,
                pi_opp,
                regret,
                strategy_sum,
                rng,
            )
        }
        NodeKind::Player(actor) => {
            let info = G::info_set(&state, actor);
            let actions = G::legal_actions(&state);
            let n = actions.len();
            // ensure regret slot exists with correct length (D-324)
            regret.get_or_init(info.clone(), n);
            let sigma = regret.current_strategy(&info, n);

            if actor == traverser {
                // traverser node：枚举每个 action 的 cfv，累积 regret。
                // strategy_sum 在 D-301 详解 ES-MCCFR mode 仅在 non-traverser
                // 决策点累积（Lanctot 2009 §4.1）；traverser 决策点不累积。
                let mut cfvs = Vec::with_capacity(n);
                for (i, action) in actions.iter().enumerate() {
                    let next_state = G::next(state.clone(), *action, rng);
                    let cfv = recurse_es::<G>(
                        next_state,
                        traverser,
                        pi_trav * sigma[i],
                        pi_opp,
                        regret,
                        strategy_sum,
                        rng,
                    );
                    cfvs.push(cfv);
                }
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                // regret update R(I, a) += π_opp × (cfv_a - σ_node)
                let delta: Vec<f64> = cfvs.iter().map(|c| pi_opp * (c - sigma_value)).collect();
                regret.accumulate(info, &delta);
                sigma_value
            } else {
                // opponent node（D-309 / D-337）：按 σ 采样 1 action；非
                // traverser 决策点 strategy_sum 累积 `S(I, b) += σ(b)` for all
                // b（D-301 详解 / D-322）。
                //
                // 过滤零概率 outcome（API-331 [`sample_discrete`] 不变量：所有
                // p > 0；零概率 action 由 caller 剔除）。当 regret matching 后
                // 某些 action 的 σ 严格为 0 时（normalized R⁺ 分布常见情形），
                // 这些 action 在采样阶段不可达，从分布中剔除即可——剩余 σ 仍
                // sum 到 1（D-330 容差）。
                //
                // strategy_sum 仍按全 σ 累积（zero σ 累加零等价于不更新；保留
                // statement 让 D-304 标准累积形式不变形）。
                strategy_sum.accumulate(info, &sigma);

                let nonzero_dist: Vec<(G::Action, f64)> = actions
                    .iter()
                    .copied()
                    .zip(sigma.iter().copied())
                    .filter(|(_, p)| *p > 0.0)
                    .collect();
                debug_assert!(
                    !nonzero_dist.is_empty(),
                    "non-traverser σ all-zero impossible: RegretTable::current_strategy 退化局面 \
                     回退均匀分布 (D-331)，sum = n_actions × (1/n_actions) = 1.0 strictly > 0"
                );
                let sampled = sample_discrete(&nonzero_dist, rng);
                let sampled_idx = actions
                    .iter()
                    .position(|a| *a == sampled)
                    .expect("sampled action must be in legal_actions");
                let sampled_sigma = sigma[sampled_idx];

                let next_state = G::next(state, sampled, rng);
                recurse_es::<G>(
                    next_state,
                    traverser,
                    pi_trav,
                    pi_opp * sampled_sigma,
                    regret,
                    strategy_sum,
                    rng,
                )
            }
        }
    }
}
