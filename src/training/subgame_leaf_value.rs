//! S6 6b：depth-limit subgame 的**叶子续局值表**（N-player + 多 biased 续局，稀疏 per-node）。
//!
//! 设计依据 `docs/temp/realtime_search_design_2026_06_03.md` §5c / §6 #6。depth-limit
//! 搜索（[`crate::training::nlhe_betting_tree::PublicBettingTree::build_subtree_depth_limited`]）
//! 在街边界截断子树，叶子处不解到终局、改查**blueprint 续局值** = 「从该叶子公共局面起、各家
//! 按某个 biased 续局风格打到底」的 self-play 期望收益 `E[U | pos, node, bucket]`。
//!
//! # 为什么稀疏 + 只存街起点（OOM 教训）
//!
//! 初版照 [`crate::training::aivat_value`] 思路建**全节点 dense** 表（`total_rows × n_cont ×
//! n_players`）→ 6-max blueprint（`total_rows ~9e7`）× 4 续局 × 6 位 = 数十 GiB，vultr 实测
//! **OOM SIGKILL**。改正：depth-limit 叶子**只可能是街起点节点**（下注轮跨街后的首决策点），
//! 全树街起点是稀疏子集；故只在街起点累计、用 **HashMap 稀疏键** `(node, bucket, pos, cont)`，
//! 内存随覆盖度（~几百 MiB）而非全表。键即 blueprint 全局 `node_id`（精确 per-node，无值抽象、
//! 无 dense 索引器依赖）。
//!
//! # 与 [`crate::training::aivat_value`] 的关系
//!
//! 那个模块是 **HU AIVAT 方差缩减**专用（2 人硬编码 + preflop169×169 的 `vroot`），服务 Slumbot
//! 评测，**不动**。本模块是 6b 实时搜索专用的**独立** N-player 多续局稀疏表，用途不同、刻意不
//! 耦合。共享的只是「跑 blueprint self-play 缓存 `(node,bucket,seat)→E[U]`」这一思路。
//!
//! # 多续局（Modicum / Pluribus 的「续局策略」）
//!
//! 每个续局 = 对 blueprint 策略的某类动作 ×系数再归一（§6 #6：fold-bias 乘 fold、call-bias 乘
//! call、raise-bias 乘所有 raise；unbiased = 原样）。**每个续局各跑一遍 self-play**（全桌同用该
//! 续局风格）得一份叶子 EV。subgame 求解时叶子是「对手选续局」选择节点（6b-4），CFR 对 n_cont
//! 个续局值优化。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::abstraction::info::InfoSetId;
use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::{Card, PlayerStatus, SeatId};
use crate::training::game::{Game, NodeKind};
use crate::training::nlhe::{SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheState};
use crate::training::nlhe_betting_tree::{AbstractActionTag, NodeId, PublicBettingTree};
use crate::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;

/// 各街已发公共牌张数（StreetTag Preflop/Flop/Turn/River = 0..3）。
const BOARD_LEN: [usize; 4] = [0, 3, 4, 5];

/// 续局偏置类别（§6 #6）。`Unbiased` = blueprint 原样；其余 ×系数后再归一。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BiasKind {
    /// blueprint 原样（不偏）。
    Unbiased,
    /// 乘 `Fold` 概率。
    Fold,
    /// 乘 `Call` 概率（complete 也算 Call；`Check` 不算，§6 #6 字面）。
    Call,
    /// 乘**所有**进攻概率（`Bet` / `Raise` / `AllIn`）。
    Raise,
}

/// 一种续局风格 = `(类别, 系数)`。`Unbiased` 系数不被读。
#[derive(Clone, Copy, Debug)]
pub struct ContinuationSpec {
    pub kind: BiasKind,
    pub coef: f64,
}

impl ContinuationSpec {
    pub fn unbiased() -> Self {
        Self {
            kind: BiasKind::Unbiased,
            coef: 1.0,
        }
    }
}

/// 默认 4 续局（Pluribus/Modicum）：unbiased + fold/call/raise ×5。系数 ×5 是 HU/Pluribus
/// 经验值，6-max 多街多人须消融重标（§5c 风险标注）；本函数给默认，探针可覆写。
pub fn default_continuations() -> Vec<ContinuationSpec> {
    vec![
        ContinuationSpec::unbiased(),
        ContinuationSpec {
            kind: BiasKind::Fold,
            coef: 5.0,
        },
        ContinuationSpec {
            kind: BiasKind::Call,
            coef: 5.0,
        },
        ContinuationSpec {
            kind: BiasKind::Raise,
            coef: 5.0,
        },
    ]
}

/// 把续局偏置作用到一组**已归一**概率上：命中类别的槽 ×`coef`，再归一（§6 #6）。`Unbiased`
/// 直接返回原样。全零（不应发生）则不动。`actions` 与 `probs` 逐位对齐（同 `legal_actions` 序）。
pub fn apply_bias(probs: &mut [f64], actions: &[SimplifiedNlheAction], spec: ContinuationSpec) {
    if spec.kind == BiasKind::Unbiased {
        return;
    }
    debug_assert_eq!(probs.len(), actions.len(), "probs 与 actions 须同长");
    for (p, a) in probs.iter_mut().zip(actions) {
        let hit = match spec.kind {
            BiasKind::Fold => matches!(AbstractActionTag::of(a), AbstractActionTag::Fold),
            BiasKind::Call => matches!(AbstractActionTag::of(a), AbstractActionTag::Call),
            BiasKind::Raise => matches!(
                AbstractActionTag::of(a),
                AbstractActionTag::Bet(_) | AbstractActionTag::Raise(_) | AbstractActionTag::AllIn
            ),
            BiasKind::Unbiased => false,
        };
        if hit {
            *p *= spec.coef;
        }
    }
    let sum: f64 = probs.iter().sum();
    if sum > 0.0 {
        for p in probs.iter_mut() {
            *p /= sum;
        }
    }
}

/// 稀疏键打包 `(node_id:32, bucket:16, pos:8, cont:8)` → u64。bucket ≤ 500 < 2¹⁶、pos < n ≤ 6、
/// cont < n_cont ≤ 16，均不溢出。
#[inline]
fn pack_key(node_id: NodeId, bucket: u32, pos: u8, cont: u8) -> u64 {
    debug_assert!(bucket < (1 << 16), "bucket {bucket} 超 16 bit");
    (node_id as u64) | ((bucket as u64) << 32) | ((pos as u64) << 48) | ((cont as u64) << 56)
}

/// N-player 多续局叶子值表（稀疏 per-node）。键 = blueprint 全局 `(node_id, bucket, pos, cont)`，
/// 只含 self-play 在**街起点**访问到的项。
pub struct LeafValueTables {
    pub n_players: usize,
    pub n_cont: usize,
    /// 按钮座位（位置相对量基准）。
    pub button: u8,
    /// 构建用 blueprint 的 `update_count`（provenance）。
    pub update_count: u64,
    /// 构建用 bucket table BLAKE3（provenance）。
    pub bucket_blake3: [u8; 32],
    /// `mean[pack_key(node,bucket,pos,cont)]` = `E[U | pos, node, bucket, cont]`（已 finalize，
    /// 仅含 count>0 项）。
    mean: HashMap<u64, f64>,
    /// 叶子查值遥测（在手座 leaf payoff 计数；探针报 leaf-miss 率 = 深街叶子覆盖软肋的可见度）。
    /// 跨 rayon 线程共享原子累加（`Relaxed`，只做计数）；探针**每臂前 [`reset_eval_stats`] 复位**
    /// （表 Arc 在两臂共享时不混计）。
    leaf_evals: AtomicU64,
    leaf_misses: AtomicU64,
}

impl LeafValueTables {
    /// 续局数。
    #[inline]
    pub fn n_cont(&self) -> usize {
        self.n_cont
    }

    /// 叶子续局值 `E[U | seat, global node, bucket, 续局 cont]`。`seat` = 真实座位（内部转
    /// 相对按钮位置 `pos = (seat + n − button) % n`）；`node_id` = **blueprint 全局树** 街起点
    /// 节点（subgame 本地叶子须先映射回全局，见 6b-3）；`bucket` = seat 在该街的桶；`cont` <
    /// `n_cont`。未访问 → `None`（调用方按 6b-3 兜底）。
    #[inline]
    pub fn value(&self, seat: usize, node_id: NodeId, bucket: u32, cont: usize) -> Option<f64> {
        debug_assert!(
            cont < self.n_cont,
            "cont {cont} 越界 n_cont {}",
            self.n_cont
        );
        let pos = ((seat + self.n_players - self.button as usize) % self.n_players) as u8;
        self.mean
            .get(&pack_key(node_id, bucket, pos, cont as u8))
            .copied()
    }

    /// 诊断：表项总数（测试 / 覆盖率报告）。
    pub fn len(&self) -> usize {
        self.mean.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mean.is_empty()
    }

    /// 诊断：某续局 `cont` 被访问到的键数。
    pub fn populated_for_cont(&self, cont: usize) -> u64 {
        let c = cont as u8;
        self.mean
            .keys()
            .filter(|k| ((*k >> 56) & 0xff) as u8 == c)
            .count() as u64
    }

    /// 记一次在手座 leaf payoff（`missed` = 该 (seat,node,bucket) 在 cont + unbiased 都查不到 →
    /// 退 0）。`leaf_payoff` 调（深街/river 叶子覆盖软肋 → miss 率高 = 解读探针的关键，否则静默退 0）。
    #[inline]
    pub fn record_leaf_eval(&self, missed: bool) {
        self.leaf_evals.fetch_add(1, Ordering::Relaxed);
        if missed {
            self.leaf_misses.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// leaf 查值次数 / miss 次数（探针读，每臂前 [`reset_eval_stats`] 复位）。
    pub fn leaf_eval_counts(&self) -> (u64, u64) {
        (
            self.leaf_evals.load(Ordering::Relaxed),
            self.leaf_misses.load(Ordering::Relaxed),
        )
    }

    /// miss 率（miss/evals；evals==0 → 0）。
    pub fn leaf_miss_rate(&self) -> f64 {
        let (e, m) = self.leaf_eval_counts();
        if e == 0 {
            0.0
        } else {
            m as f64 / e as f64
        }
    }

    /// 复位查值遥测（探针在每臂 h2h 前调；表 Arc 两臂共享时避免混计）。
    pub fn reset_eval_stats(&self) {
        self.leaf_evals.store(0, Ordering::Relaxed);
        self.leaf_misses.store(0, Ordering::Relaxed);
    }

    /// 测试用：从显式 `(seat, node, bucket, cont, value)` 条目直接造表（绕过 self-play），
    /// 让 6b-4 `argmax_cont` / 叶子选择逻辑可在受控值下确定性测试。pos 用与 [`value`](Self::value)
    /// 同一 `(seat+n−button)%n` 映射 + [`pack_key`]，故 `value(seat,..)` 能命中这些条目。
    #[cfg(test)]
    pub(crate) fn from_entries_for_test(
        n_players: usize,
        n_cont: usize,
        button: u8,
        entries: &[(usize, NodeId, u32, usize, f64)],
    ) -> LeafValueTables {
        let mut mean = HashMap::new();
        for &(seat, node, bucket, cont, v) in entries {
            let pos = ((seat + n_players - button as usize) % n_players) as u8;
            mean.insert(pack_key(node, bucket, pos, cont as u8), v);
        }
        LeafValueTables {
            n_players,
            n_cont,
            button,
            update_count: 0,
            bucket_blake3: [0u8; 32],
            mean,
            leaf_evals: AtomicU64::new(0),
            leaf_misses: AtomicU64::new(0),
        }
    }
}

/// 一个节点是否**街起点**（其 parent 在更浅的街 → 本节点是跨街后的首决策点 = 唯一可能成为
/// depth-limit 叶子的节点）。full-tree root（preflop，无 parent）→ false。
#[inline]
fn is_street_start(tree: &PublicBettingTree, node_id: NodeId) -> bool {
    let node = tree.node(node_id);
    node.parent
        .is_some_and(|p| (tree.node(p).street as u8) < (node.street as u8))
}

/// 构建叶子值表：对每个续局跑 `hands_per_cont` 手 blueprint self-play（全桌同用该续局风格），
/// 把每条 rollout 上**每个访问到的街起点节点**对**仍在手的每个座位**累计该座位最终净收益到
/// `(node, 该座位 bucket, pos, cont)`。blueprint 策略 = Hybrid（strategy_sum 行非零取 average，
/// 否则 current；与 aivat / slumbot_advisor 一致）。
///
/// 单线程（ChaCha20Rng 确定 → 同 `seed` byte-equal 可复现）。`max_actions_per_hand` = 每手动作
/// 上限（防御，正常必在内到 terminal）。内存随覆盖到的街起点 (node,bucket,pos,cont) 项数。
pub fn build_leaf_value_tables(
    trainer: &DenseNlheEsMccfrTrainer,
    continuations: &[ContinuationSpec],
    hands_per_cont: u64,
    seed: u64,
    max_actions_per_hand: usize,
) -> LeafValueTables {
    let game = trainer.game();
    let n = game.n_players();
    let n_cont = continuations.len();
    let button = game.config.button_seat.0;
    let tree = game.tree();

    // Hybrid blueprint 策略（与 aivat / slumbot_advisor 一致）。
    let base_strat = |info: &InfoSetId| -> Vec<f64> {
        if trainer.strategy_sum().row_sum_by_info(*info) <= 0.0 {
            trainer.current_strategy(*info)
        } else {
            trainer.average_strategy(*info)
        }
    };

    // 累加器：key → (sum, count)。
    let mut acc: HashMap<u64, (f64, u32)> = HashMap::new();

    for (ci, cont) in continuations.iter().enumerate() {
        // per-cont 解耦 seed（SplitMix 常数混 ci → 各续局独立流）。
        let mut rng =
            ChaCha20Rng::from_seed(seed ^ (ci as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        for _ in 0..hands_per_cont {
            let mut visited: Vec<(NodeId, u16)> = Vec::with_capacity(8);
            let Some((terminal, holes)) = play_selfplay_hand(
                game,
                &base_strat,
                *cont,
                &mut rng,
                max_actions_per_hand,
                &mut visited,
            ) else {
                continue; // 未在 cap 内到 terminal（不应发生），跳过保不偏。
            };
            let payouts = terminal.game_state.payouts().expect("terminal payouts");
            let u: Vec<f64> = (0..n).map(|s| seat_payoff(&payouts, s as u8)).collect();
            let board = terminal.game_state.board();

            // bucket 仅随街变 → 按 (seat, street) 缓存。
            let mut bucket_cache: Vec<[Option<u32>; 4]> = vec![[None; 4]; n];
            for &(node_id, active_mask) in &visited {
                // visited 只含街起点（play_selfplay_hand 已过滤）。
                let street = tree.node(node_id).street as usize;
                let sub_board = &board[..BOARD_LEN[street]];
                for seat in 0..n {
                    // 只累计该节点仍在手（Active|AllIn）的座位——弃牌座的 (pos,node) 槽永不被叶子
                    // 查（leaf 只查 active 座），跳过避免无意义项。
                    if (active_mask >> seat) & 1 == 0 {
                        continue;
                    }
                    let bucket = match bucket_cache[seat][street] {
                        Some(b) => b,
                        None => {
                            let b = game
                                .info_set_for_cards(node_id, holes[seat], sub_board)
                                .bucket_id();
                            bucket_cache[seat][street] = Some(b);
                            b
                        }
                    };
                    let pos = ((seat + n - button as usize) % n) as u8;
                    let key = pack_key(node_id, bucket, pos, ci as u8);
                    let e = acc.entry(key).or_insert((0.0, 0));
                    e.0 += u[seat];
                    e.1 += 1;
                }
            }
        }
    }

    let mean: HashMap<u64, f64> = acc
        .into_iter()
        .map(|(k, (s, c))| (k, s / c as f64))
        .collect();

    LeafValueTables {
        n_players: n,
        n_cont,
        button,
        update_count: trainer.update_count(),
        bucket_blake3: game.bucket_table_blake3(),
        mean,
        leaf_evals: AtomicU64::new(0),
        leaf_misses: AtomicU64::new(0),
    }
}

/// 走一手 blueprint self-play（动作概率经 `cont` 偏置）。只记录**街起点**决策节点
/// `(node_id, active_mask)`（active = Active|AllIn bitmask，决定该节点累计哪些座位）。返回
/// `(terminal, 各座底牌)`；底牌在 **root** 捕获（fold 终局会 muck 弃牌方 → 不能从 terminal 读）。
/// 未在 cap 内到 terminal 返回 `None`。
fn play_selfplay_hand(
    game: &SimplifiedNlheGame,
    base_strat: &dyn Fn(&InfoSetId) -> Vec<f64>,
    cont: ContinuationSpec,
    rng: &mut dyn RngSource,
    max_actions: usize,
    visited: &mut Vec<(NodeId, u16)>,
) -> Option<(SimplifiedNlheState, Vec<[Card; 2]>)> {
    let tree = game.tree();
    let mut state = game.root(rng);
    let n = game.n_players();
    let holes: Vec<[Card; 2]> = (0..n)
        .map(|s| {
            state.game_state.players()[s]
                .hole_cards
                .expect("root 各座必有底牌")
        })
        .collect();
    for _ in 0..max_actions {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => return Some((state, holes)),
            NodeKind::Player(actor) => {
                if is_street_start(tree, state.current_node_id) {
                    visited.push((state.current_node_id, active_mask(&state)));
                }
                let actions = SimplifiedNlheGame::legal_actions(&state);
                let info = SimplifiedNlheGame::info_set(&state, actor);
                let mut probs = normalized_probs(&base_strat(&info), actions.len());
                apply_bias(&mut probs, &actions, cont);
                let idx = sample_idx(&probs, rng);
                state = SimplifiedNlheGame::next(state, actions[idx], rng);
            }
            NodeKind::Chance => unreachable!("简化 NLHE 无 chance 节点"),
        }
    }
    None
}

/// 仍在手（Active|AllIn）座位 bitmask。
fn active_mask(state: &SimplifiedNlheState) -> u16 {
    let mut m = 0u16;
    for (i, p) in state.game_state.players().iter().enumerate() {
        if matches!(p.status, PlayerStatus::Active | PlayerStatus::AllIn) {
            m |= 1u16 << i;
        }
    }
    m
}

/// 归一；空 / 长度不符 / 全非正 → uniform（同 advisor 容错层）。
fn normalized_probs(raw: &[f64], n: usize) -> Vec<f64> {
    let uniform = || vec![1.0 / n as f64; n];
    if raw.len() != n {
        return uniform();
    }
    let sum: f64 = raw.iter().map(|p| p.max(0.0)).sum();
    if !sum.is_finite() || sum <= 0.0 {
        return uniform();
    }
    raw.iter().map(|p| p.max(0.0) / sum).collect()
}

/// 累积分布采样（probs 已归一）。
fn sample_idx(probs: &[f64], rng: &mut dyn RngSource) -> usize {
    let r = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64; // [0,1)
    let mut cum = 0.0;
    for (i, &p) in probs.iter().enumerate() {
        cum += p;
        if r < cum {
            return i;
        }
    }
    probs.len() - 1
}

fn seat_payoff(payouts: &[(SeatId, i64)], seat: u8) -> f64 {
    payouts
        .iter()
        .find(|(s, _)| s.0 == seat)
        .map(|(_, c)| *c as f64)
        .expect("payouts 必含该座位（stage 1 不变量）")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abstraction::action::{AbstractAction, BetRatio};
    use crate::abstraction::bucket_table::{BucketConfig, BucketTable};
    use std::sync::Arc;

    /// [`apply_bias`] 的偏置数学（确定、强）：fold-bias ×5 把 fold 槽放大再归一；call/raise
    /// 同理；unbiased 不动；Check 不被 call-bias 碰（§6 #6 字面）。
    #[test]
    fn apply_bias_math() {
        let acts = vec![
            AbstractAction::Fold,
            AbstractAction::Check,
            AbstractAction::Raise {
                ratio_label: BetRatio::HALF_POT,
                to: crate::core::ChipAmount(10),
            },
        ];
        // fold ×5：[0.2,0.3,0.5] → [1.0,0.3,0.5]/1.8。
        let mut p = vec![0.2, 0.3, 0.5];
        apply_bias(
            &mut p,
            &acts,
            ContinuationSpec {
                kind: BiasKind::Fold,
                coef: 5.0,
            },
        );
        let s = 1.0 + 0.3 + 0.5;
        assert!((p[0] - 1.0 / s).abs() < 1e-12);
        assert!((p[1] - 0.3 / s).abs() < 1e-12);
        assert!((p[2] - 0.5 / s).abs() < 1e-12);
        assert!((p.iter().sum::<f64>() - 1.0).abs() < 1e-12);

        // raise ×5：只放大 Raise 槽。
        let mut p = vec![0.2, 0.3, 0.5];
        apply_bias(
            &mut p,
            &acts,
            ContinuationSpec {
                kind: BiasKind::Raise,
                coef: 5.0,
            },
        );
        let s = 0.2 + 0.3 + 2.5;
        assert!((p[2] - 2.5 / s).abs() < 1e-12);

        // call ×5：acts 无 Call → 不变（仍归一）。Check 不算 Call。
        let mut p = vec![0.2, 0.3, 0.5];
        apply_bias(
            &mut p,
            &acts,
            ContinuationSpec {
                kind: BiasKind::Call,
                coef: 5.0,
            },
        );
        assert!((p[0] - 0.2).abs() < 1e-12 && (p[1] - 0.3).abs() < 1e-12);

        // unbiased：原样。
        let mut p = vec![0.2, 0.3, 0.5];
        apply_bias(&mut p, &acts, ContinuationSpec::unbiased());
        assert_eq!(p, vec![0.2, 0.3, 0.5]);
    }

    /// leaf-miss 遥测：record_leaf_eval 累加 evals/misses、miss_rate 正确、reset 清零。
    #[test]
    fn leaf_eval_stats_counts_and_resets() {
        let t = LeafValueTables::from_entries_for_test(2, 4, 0, &[(0, 5, 1, 0, 2.0)]);
        assert_eq!(t.leaf_eval_counts(), (0, 0));
        t.record_leaf_eval(false);
        t.record_leaf_eval(true);
        t.record_leaf_eval(true);
        assert_eq!(t.leaf_eval_counts(), (3, 2));
        assert!((t.leaf_miss_rate() - 2.0 / 3.0).abs() < 1e-12);
        t.reset_eval_stats();
        assert_eq!(t.leaf_eval_counts(), (0, 0));
        assert_eq!(t.leaf_miss_rate(), 0.0);
    }

    fn stub_trainer() -> DenseNlheEsMccfrTrainer {
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ));
        let game = SimplifiedNlheGame::new(table).expect("stub HU game");
        DenseNlheEsMccfrTrainer::new(game, 7)
    }

    /// 找全树第一个 flop 街起点节点（parent 在 preflop、本节点 flop）。
    fn first_flop_start(tree: &PublicBettingTree) -> NodeId {
        (0..tree.num_nodes() as NodeId)
            .find(|&i| {
                is_street_start(tree, i)
                    && tree.node(i).street == crate::abstraction::info::StreetTag::Flop
            })
            .expect("应有 flop 街起点节点")
    }

    /// 构建 smoke（未训练 stub blueprint ≈ uniform）：①表非空、稀疏（≪ 全表）；②每续局都有
    /// 覆盖（biased pass 真跑）；③**root 不入表**（preflop 非街起点）、flop 街起点入表；
    /// ④value() 对 flop 街起点（stub 桶 0）返回 Some、均值有限；⑤同 seed 两次构建 byte-equal。
    #[test]
    fn build_leaf_value_tables_smoke_and_reproducible() {
        let trainer = stub_trainer();
        let conts = default_continuations();
        let build = || build_leaf_value_tables(&trainer, &conts, 2000, 0xC0FF_EE42, 400);
        let t = build();

        assert_eq!(t.n_players, 2, "HU");
        assert_eq!(t.n_cont, 4);
        assert!(!t.is_empty(), "表不应为空");
        // 稀疏：项数 ≪ total_rows×n_cont×n_players（全 dense 会 OOM）。stub 桶 0 → 项数 ≈
        // 街起点节点数 × pos × cont，远小于 240k 节点的全表。
        assert!(
            t.len() < 1_000_000,
            "稀疏表项数 {} 应远小于全 dense（防回退到 OOM 版）",
            t.len()
        );

        for cont in 0..t.n_cont {
            assert!(
                t.populated_for_cont(cont) > 0,
                "续局 {cont} 无任何访问 → self-play 没跑"
            );
        }

        let tree = trainer.game().tree();
        let root = tree.root_id();
        // root（preflop）不是街起点 → 不入表。
        let mut root_some = false;
        for seat in 0..t.n_players {
            for bucket in 0..3u32 {
                if t.value(seat, root, bucket, 0).is_some() {
                    root_some = true;
                }
            }
        }
        assert!(!root_some, "root（preflop 非街起点）不应入表");

        // flop 街起点：stub postflop 全归桶 0 → value(seat, flopstart, 0, 0) 必 Some。
        let flop_start = first_flop_start(tree);
        let mut any_some = false;
        for seat in 0..t.n_players {
            if let Some(v) = t.value(seat, flop_start, 0, 0) {
                assert!(v.is_finite(), "叶子值须有限");
                any_some = true;
            }
        }
        assert!(
            any_some,
            "flop 街起点 + 桶 0 至少一个 seat 应有 unbiased 值"
        );

        // 可复现：同 seed 两次构建逐键 byte-equal。
        let t2 = build();
        assert_eq!(t.mean.len(), t2.mean.len(), "项数须一致（可复现）");
        for (k, v) in &t.mean {
            let v2 = t2.mean.get(k).expect("同 seed 应有同键");
            assert_eq!(v.to_bits(), v2.to_bits(), "键 {k} 值须 byte-equal");
        }
    }
}
