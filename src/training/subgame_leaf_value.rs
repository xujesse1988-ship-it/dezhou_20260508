//! S6 6b：depth-limit subgame 的**叶子续局值表**（N-player + 多 biased 续局）。
//!
//! 设计依据 `docs/temp/realtime_search_design_2026_06_03.md` §5c / §6 #6。depth-limit
//! 搜索（[`crate::training::nlhe_betting_tree::PublicBettingTree::build_subtree_depth_limited`]）
//! 在街边界截断子树，叶子处不解到终局、改查**blueprint 续局值** = 「从该叶子公共局面起、各家
//! 按某个 biased 续局风格打到底」的 self-play 期望收益 `E[U | pos, node, bucket]`。
//!
//! 与 [`crate::training::aivat_value`] 的关系：那个模块是 **HU AIVAT 方差缩减**专用（2 人
//! 硬编码 + preflop169×169 的 `vroot` 修正），服务的是 Slumbot 对战评测，**不动**。本模块是
//! 6b 实时搜索专用的**独立** N-player 多续局表，二者用途不同、刻意不耦合（改 aivat 会波及在产的
//! AIVAT 评测）。共享的只是「跑 blueprint self-play 缓存 `(node,bucket,seat)→E[U]`」这一思路。
//!
//! # 多续局（Modicum / Pluribus 的「续局策略」）
//!
//! 每个续局 = 对 blueprint 策略的某类动作 ×系数再归一（§6 #6：fold-bias 乘 fold、call-bias 乘
//! call、raise-bias 乘所有 raise；unbiased = 原样）。**每个续局各跑一遍 self-play**（全桌同用该
//! 续局风格）得一份叶子 EV → `vf[pos][row*n_cont + cont]`。subgame 求解时叶子是「对手选续局」
//! 选择节点（6b-4），CFR 对 n_cont 个续局值 minimize/优化。
//!
//! # 键
//!
//! 表按 **blueprint 全局树** 的 `(node_id, bucket)`（经 dense [`NlheDenseIndexer::row_for`]）+
//! 相对按钮位置 `pos` 索引。subgame depth-limit 叶子查值时须先把**子树本地**叶子映射回全局
//! `node_id`（见 6b-3 接线），再调 [`LeafValueTables::value`]。

use std::sync::Arc;

use crate::abstraction::info::InfoSetId;
use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::{Card, PlayerStatus, SeatId};
use crate::training::game::{Game, NodeKind};
use crate::training::nlhe::{SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheState};
use crate::training::nlhe_betting_tree::{AbstractActionTag, NodeId};
use crate::training::nlhe_dense::NlheDenseIndexer;
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

/// N-player 多续局叶子值表。`vf_mean[pos]` 长 `total_rows * n_cont`，`idx = row*n_cont + cont`。
pub struct LeafValueTables {
    pub n_players: usize,
    pub n_cont: usize,
    /// 按钮座位（位置相对量基准）。
    pub button: u8,
    pub total_rows: u64,
    /// 构建用 blueprint 的 `update_count`（provenance）。
    pub update_count: u64,
    /// 构建用 bucket table BLAKE3（provenance）。
    pub bucket_blake3: [u8; 32],
    /// 与 blueprint 同源的 dense 索引器（叶子查值用 `row_for(global_node, bucket)`）。
    indexer: Arc<NlheDenseIndexer>,
    /// `vf_mean[pos][row*n_cont + cont]`，仅 `vf_count > 0` 的格有意义。
    vf_mean: Vec<Vec<f64>>,
    vf_count: Vec<Vec<u32>>,
}

impl LeafValueTables {
    /// 续局数。
    #[inline]
    pub fn n_cont(&self) -> usize {
        self.n_cont
    }

    /// 叶子续局值 `E[U | seat, global node, bucket, 续局 cont]`。`seat` = 真实座位（内部转
    /// 相对按钮位置 `pos = (seat + n − button) % n`）；`node_id` = **blueprint 全局树** 节点
    /// （subgame 本地叶子须先映射回全局，见 6b-3）；`bucket` = seat 在该街的桶；`cont` < `n_cont`。
    /// 未访问（count 0）→ `None`（调用方按 6b-3 兜底）。
    #[inline]
    pub fn value(&self, seat: usize, node_id: NodeId, bucket: u32, cont: usize) -> Option<f64> {
        debug_assert!(
            cont < self.n_cont,
            "cont {cont} 越界 n_cont {}",
            self.n_cont
        );
        let pos = (seat + self.n_players - self.button as usize) % self.n_players;
        let row = self.indexer.row_for(node_id, bucket) as usize;
        let idx = row * self.n_cont + cont;
        (self.vf_count[pos][idx] > 0).then(|| self.vf_mean[pos][idx])
    }

    /// 诊断：某 (pos, cont) 被访问到的格数（测试 / 覆盖率报告）。
    pub fn populated(&self, pos: usize, cont: usize) -> u64 {
        self.vf_count[pos]
            .iter()
            .skip(cont)
            .step_by(self.n_cont)
            .filter(|&&c| c > 0)
            .count() as u64
    }
}

/// 构建叶子值表：对每个续局跑 `hands_per_cont` 手 blueprint self-play（全桌同用该续局风格），
/// 把每条 rollout 上**每个访问到的决策节点**对**仍在手的每个座位**累计该座位最终净收益到
/// `(pos, node, 该座位 bucket, cont)`。blueprint 策略 = Hybrid（strategy_sum 行非零取 average，
/// 否则 current；与 aivat / slumbot_advisor 一致）。
///
/// 单线程（ChaCha20Rng 确定 → 同 `seed` byte-equal 可复现）。`max_actions_per_hand` = 每手动作
/// 上限（防御，正常必在内到 terminal）。
pub fn build_leaf_value_tables(
    trainer: &DenseNlheEsMccfrTrainer,
    continuations: &[ContinuationSpec],
    hands_per_cont: u64,
    seed: u64,
    max_actions_per_hand: usize,
) -> LeafValueTables {
    let game = trainer.game();
    let n = game.n_players();
    let indexer = Arc::clone(trainer.strategy_sum().indexer());
    let total_rows = indexer.total_rows() as usize;
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

    let mut vf_sum: Vec<Vec<f64>> = vec![vec![0.0; total_rows * n_cont]; n];
    let mut vf_cnt: Vec<Vec<u32>> = vec![vec![0u32; total_rows * n_cont]; n];

    for (ci, cont) in continuations.iter().enumerate() {
        // per-cont 解耦 seed（SplitMix 常数混 ci → 各续局独立流）。
        let mut rng =
            ChaCha20Rng::from_seed(seed ^ (ci as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        for _ in 0..hands_per_cont {
            let mut visited: Vec<(NodeId, u16)> = Vec::with_capacity(16);
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
                let street = tree.node(node_id).street as usize;
                let sub_board = &board[..BOARD_LEN[street]];
                for seat in 0..n {
                    // 只累计该节点仍在手（Active|AllIn）的座位——弃牌座不在此节点的 range 里，
                    // 其 (pos,node) 槽永不被叶子查（leaf 只查 active 座），跳过避免无意义污染。
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
                    let row = indexer.row_for(node_id, bucket) as usize;
                    let pos = (seat + n - button as usize) % n;
                    let idx = row * n_cont + ci;
                    vf_sum[pos][idx] += u[seat];
                    vf_cnt[pos][idx] += 1;
                }
            }
        }
    }

    let vf_mean: Vec<Vec<f64>> = vf_sum
        .iter()
        .zip(&vf_cnt)
        .map(|(s, c)| finalize_mean(s, c))
        .collect();

    LeafValueTables {
        n_players: n,
        n_cont,
        button,
        total_rows: total_rows as u64,
        update_count: trainer.update_count(),
        bucket_blake3: game.bucket_table_blake3(),
        indexer,
        vf_mean,
        vf_count: vf_cnt,
    }
}

/// 走一手 blueprint self-play（动作概率经 `cont` 偏置）。记录访问到的 `(node_id, active_mask)`
/// （active = Active|AllIn 座位 bitmask，决定该节点累计哪些座位）。返回 `(terminal, 各座底牌)`；
/// 底牌在 **root** 捕获（fold 终局会 muck 弃牌方 → 不能从 terminal 读）。未在 cap 内到 terminal
/// 返回 `None`。
fn play_selfplay_hand(
    game: &SimplifiedNlheGame,
    base_strat: &dyn Fn(&InfoSetId) -> Vec<f64>,
    cont: ContinuationSpec,
    rng: &mut dyn RngSource,
    max_actions: usize,
    visited: &mut Vec<(NodeId, u16)>,
) -> Option<(SimplifiedNlheState, Vec<[Card; 2]>)> {
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
                visited.push((state.current_node_id, active_mask(&state)));
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

fn finalize_mean(sum: &[f64], cnt: &[u32]) -> Vec<f64> {
    sum.iter()
        .zip(cnt)
        .map(|(&s, &c)| if c > 0 { s / c as f64 } else { 0.0 })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abstraction::action::{AbstractAction, BetRatio};
    use crate::abstraction::bucket_table::{BucketConfig, BucketTable};

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

    fn stub_trainer() -> DenseNlheEsMccfrTrainer {
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ));
        let game = SimplifiedNlheGame::new(table).expect("stub HU game");
        DenseNlheEsMccfrTrainer::new(game, 7)
    }

    /// 构建 smoke（未训练 stub blueprint ≈ uniform）：①维度对（n_players/n_cont/total_rows）；
    /// ②有格被访问、均值有限；③value() 对已访问格返回 Some、未训练桶 0 之外返回 None；
    /// ④同 seed 两次构建 byte-equal（可复现）；⑤不同续局各自有覆盖（biased pass 真跑）。
    #[test]
    fn build_leaf_value_tables_smoke_and_reproducible() {
        let trainer = stub_trainer();
        let conts = default_continuations();
        let build = || build_leaf_value_tables(&trainer, &conts, 1500, 0xC0FF_EE42, 400);
        let t = build();

        assert_eq!(t.n_players, 2, "HU");
        assert_eq!(t.n_cont, 4);
        assert_eq!(
            t.total_rows,
            trainer.strategy_sum().indexer().total_rows(),
            "total_rows 对齐 indexer"
        );

        // 每个续局都有被访问到的格（biased self-play 真跑了）。
        for cont in 0..t.n_cont {
            let pop: u64 = (0..t.n_players).map(|pos| t.populated(pos, cont)).sum();
            assert!(pop > 0, "续局 {cont} 无任何访问 → self-play 没跑");
        }

        // 均值有限。
        for pos in 0..t.n_players {
            for (i, &c) in t.vf_count[pos].iter().enumerate() {
                if c > 0 {
                    assert!(t.vf_mean[pos][i].is_finite(), "pos{pos} idx{i} 均值非有限");
                }
            }
        }

        // value() 链路：root 节点 + preflop class 0（stub 下 root 必被访问；2 人零和 → root 处
        // unbiased 双方 marginal 期望相加 ≈ 0，但单边非空即证查值通）。
        let root = trainer.game().tree().root_id();
        let mut any_some = false;
        for seat in 0..t.n_players {
            for bucket in 0..3u32 {
                if t.value(seat, root, bucket, 0).is_some() {
                    any_some = true;
                }
            }
        }
        assert!(any_some, "root 处至少一个 (seat,bucket) 应有 unbiased 值");

        // 可复现：同 seed 两次构建逐 pos 逐格 byte-equal。
        let t2 = build();
        for pos in 0..t.n_players {
            assert_eq!(
                t.vf_count[pos], t2.vf_count[pos],
                "pos{pos} count 须 byte-equal（可复现）"
            );
            for (a, b) in t.vf_mean[pos].iter().zip(&t2.vf_mean[pos]) {
                assert_eq!(a.to_bits(), b.to_bits(), "pos{pos} mean 须 byte-equal");
            }
        }
    }
}
