//! AIVAT 值函数表构建（NLHE 自对弈 Monte Carlo）。见 `docs/aivat_eval.md` §5。
//!
//! 一次 blueprint vs blueprint 自对弈 pass 产出两组值表（按位置 SB/BB 分开）：
//! - **VF-3 `vf[pos][row]`** = self-play `E[U | 我方 bucket, betting node, 位置]`，
//!   `row = NlheDenseIndexer::row_for(node_id, our_bucket)`。每条 rollout 走过的**每个
//!   决策节点**，把该手对**评分座位**的最终净收益累计进 `(node_id, 该座位 bucket)`
//!   —— 键用 [`SimplifiedNlheGame::info_set_for_cards`]（我方 bucket），即便节点是对方
//!   行动也按我方牌取桶（见 review keying note）。含 root → 同时给 §4.1 的 V₁。
//! - **VF-2 `vroot[pos][our_class*169+opp_class]`** = self-play `E[U | 双方 preflop 169
//!   类, 位置]`，用于 §4.2 对方发牌修正。
//!
//! 无偏性不依赖本表质量（值函数任意固定即可，见 `docs/aivat_eval.md` §3）；自对弈只是
//! "对手未知"下能拿到的最好基线，决定降方差幅度。

use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;

use crate::abstraction::info::InfoSetId;
use crate::abstraction::preflop::PreflopLossless169;
use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::{Card, SeatId};
use crate::training::game::{Game, NodeKind};
use crate::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use crate::training::nlhe_betting_tree::NodeId;
use crate::training::nlhe_dense::NlheDenseIndexer;
use crate::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;

/// 169-类维度（preflop lossless）。
pub const N_PREFLOP_CLASSES: usize = 169;

/// 各街已发公共牌张数（StreetTag Preflop/Flop/Turn/River = 0..3）。
const BOARD_LEN: [usize; 4] = [0, 3, 4, 5];

const MAGIC: &[u8; 8] = b"AIVATVF1";

/// AIVAT 值表（位置 0 = SB/button，1 = BB）。
pub struct AivatValueTables {
    pub total_rows: u64,
    pub hands: u64,
    pub seed: u64,
    /// 构建所用 blueprint 的 `update_count`（provenance）。
    pub update_count: u64,
    /// 构建所用 bucket table 的 BLAKE3（provenance）。
    pub bucket_blake3: [u8; 32],
    /// `vf_mean[pos][row]`，仅 `vf_count[pos][row] > 0` 的格有意义。
    pub vf_mean: [Vec<f64>; 2],
    pub vf_count: [Vec<u32>; 2],
    /// `vroot_mean[pos][our_class*169 + opp_class]`。
    pub vroot_mean: [Vec<f64>; 2],
    pub vroot_count: [Vec<u32>; 2],
}

impl AivatValueTables {
    /// `V_info[pos, row]`，未访问（count 0）返回 `None`。
    #[inline]
    pub fn v_info(&self, pos: usize, row: u64) -> Option<f64> {
        let r = row as usize;
        (self.vf_count[pos][r] > 0).then(|| self.vf_mean[pos][r])
    }

    /// `V_root_both[pos, our_class, opp_class]`，未访问返回 `None`。
    #[inline]
    pub fn v_root_both(&self, pos: usize, our_class: usize, opp_class: usize) -> Option<f64> {
        let i = our_class * N_PREFLOP_CLASSES + opp_class;
        (self.vroot_count[pos][i] > 0).then(|| self.vroot_mean[pos][i])
    }

    /// 序列化到二进制 artifact。
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let f = std::fs::File::create(path)?;
        let mut w = BufWriter::with_capacity(1 << 22, f);
        w.write_all(MAGIC)?;
        w.write_all(&self.total_rows.to_le_bytes())?;
        w.write_all(&self.hands.to_le_bytes())?;
        w.write_all(&self.seed.to_le_bytes())?;
        w.write_all(&self.update_count.to_le_bytes())?;
        w.write_all(&self.bucket_blake3)?;
        for pos in 0..2 {
            write_f64s(&mut w, &self.vf_mean[pos])?;
            write_u32s(&mut w, &self.vf_count[pos])?;
        }
        for pos in 0..2 {
            write_f64s(&mut w, &self.vroot_mean[pos])?;
            write_u32s(&mut w, &self.vroot_count[pos])?;
        }
        w.flush()
    }

    /// 从 artifact 反序列化。
    pub fn load(path: &Path) -> io::Result<AivatValueTables> {
        let f = std::fs::File::open(path)?;
        let mut r = BufReader::with_capacity(1 << 22, f);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "AIVAT VF magic 不匹配",
            ));
        }
        let total_rows = read_u64(&mut r)?;
        let hands = read_u64(&mut r)?;
        let seed = read_u64(&mut r)?;
        let update_count = read_u64(&mut r)?;
        let mut bucket_blake3 = [0u8; 32];
        r.read_exact(&mut bucket_blake3)?;
        let rows = total_rows as usize;
        let mut vf_mean = [Vec::new(), Vec::new()];
        let mut vf_count = [Vec::new(), Vec::new()];
        for pos in 0..2 {
            vf_mean[pos] = read_f64s(&mut r, rows)?;
            vf_count[pos] = read_u32s(&mut r, rows)?;
        }
        let nroot = N_PREFLOP_CLASSES * N_PREFLOP_CLASSES;
        let mut vroot_mean = [Vec::new(), Vec::new()];
        let mut vroot_count = [Vec::new(), Vec::new()];
        for pos in 0..2 {
            vroot_mean[pos] = read_f64s(&mut r, nroot)?;
            vroot_count[pos] = read_u32s(&mut r, nroot)?;
        }
        Ok(AivatValueTables {
            total_rows,
            hands,
            seed,
            update_count,
            bucket_blake3,
            vf_mean,
            vf_count,
            vroot_mean,
            vroot_count,
        })
    }
}

/// 用一组累加器构建值表。`trainer` = 已加载的 blueprint；自对弈双方都打 blueprint
/// （Hybrid：strategy_sum 行非零取 average，否则 current；空/全零→uniform）。
pub fn build_value_tables(
    trainer: &DenseNlheEsMccfrTrainer,
    hands: u64,
    seed: u64,
    max_actions_per_hand: usize,
) -> AivatValueTables {
    let game = trainer.game();
    let indexer: &Arc<NlheDenseIndexer> = trainer.strategy_sum().indexer();
    let total_rows = indexer.total_rows() as usize;
    let button = game.config.button_seat.0;
    let tree = game.tree();
    let pf = PreflopLossless169::new();

    let mut vf_sum: [Vec<f64>; 2] = [vec![0.0; total_rows], vec![0.0; total_rows]];
    let mut vf_cnt: [Vec<u32>; 2] = [vec![0u32; total_rows], vec![0u32; total_rows]];
    let nroot = N_PREFLOP_CLASSES * N_PREFLOP_CLASSES;
    let mut vroot_sum: [Vec<f64>; 2] = [vec![0.0; nroot], vec![0.0; nroot]];
    let mut vroot_cnt: [Vec<u32>; 2] = [vec![0u32; nroot], vec![0u32; nroot]];

    // Hybrid blueprint 策略（与 slumbot_advisor 一致）。
    let strat = |info: &InfoSetId| -> Vec<f64> {
        if trainer.strategy_sum().row_sum_by_info(*info) <= 0.0 {
            trainer.current_strategy(*info)
        } else {
            trainer.average_strategy(*info)
        }
    };

    let mut rng = ChaCha20Rng::from_seed(seed);
    for _ in 0..hands {
        let mut visited: Vec<NodeId> = Vec::with_capacity(16);
        let Some((terminal, holes)) =
            play_selfplay_hand(game, &strat, &mut rng, max_actions_per_hand, &mut visited)
        else {
            continue; // 未在 cap 内到 terminal（实际不应发生），跳过保不偏
        };

        let board = terminal.game_state.board();
        let payouts = terminal.game_state.payouts().expect("terminal payouts");
        let u = [seat_payoff(&payouts, 0), seat_payoff(&payouts, 1)];

        // VF-3：每个访问到的决策节点，两个座位各累计一次（键 = 该座位 bucket）。
        // bucket 仅随街变 → 按 (seat, street) 缓存，省 canonical_observation_id。
        let mut bucket_cache: [[Option<u32>; 4]; 2] = [[None; 4]; 2];
        for &node_id in &visited {
            let street = tree.node(node_id).street as usize;
            let sub_board = &board[..BOARD_LEN[street]];
            for seat in 0..2 {
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
                let pos = if seat as u8 == button { 0 } else { 1 };
                vf_sum[pos][row] += u[seat];
                vf_cnt[pos][row] += 1;
            }
        }

        // VF-2：root 处按双方 169 类累计。
        let cls = [
            pf.hand_class(holes[0]) as usize,
            pf.hand_class(holes[1]) as usize,
        ];
        for seat in 0..2 {
            let pos = if seat as u8 == button { 0 } else { 1 };
            let idx = cls[seat] * N_PREFLOP_CLASSES + cls[1 - seat];
            vroot_sum[pos][idx] += u[seat];
            vroot_cnt[pos][idx] += 1;
        }
    }

    let vf_mean = [
        finalize_mean(&vf_sum[0], &vf_cnt[0]),
        finalize_mean(&vf_sum[1], &vf_cnt[1]),
    ];
    let vroot_mean = [
        finalize_mean(&vroot_sum[0], &vroot_cnt[0]),
        finalize_mean(&vroot_sum[1], &vroot_cnt[1]),
    ];

    AivatValueTables {
        total_rows: total_rows as u64,
        hands,
        seed,
        update_count: trainer.update_count(),
        bucket_blake3: game.bucket_table_blake3(),
        vf_mean,
        vf_count: [
            std::mem::take(&mut vf_cnt[0]),
            std::mem::take(&mut vf_cnt[1]),
        ],
        vroot_mean,
        vroot_count: [
            std::mem::take(&mut vroot_cnt[0]),
            std::mem::take(&mut vroot_cnt[1]),
        ],
    }
}

/// 走一手 blueprint 自对弈，记录访问到的决策节点。返回 `(terminal, 双方底牌)`；底牌在
/// root 处捕获（fold 终局会把弃牌方 hole_cards muck 成 None，故不能从 terminal 读）。
/// 未在 cap 内到 terminal 返回 `None`。
fn play_selfplay_hand(
    game: &SimplifiedNlheGame,
    strat: &dyn Fn(&InfoSetId) -> Vec<f64>,
    rng: &mut dyn RngSource,
    max_actions: usize,
    visited: &mut Vec<NodeId>,
) -> Option<(SimplifiedNlheState, [[Card; 2]; 2])> {
    let mut state = game.root(rng);
    let holes = {
        let players = state.game_state.players();
        [
            players[0].hole_cards.expect("root seat0 hole_cards"),
            players[1].hole_cards.expect("root seat1 hole_cards"),
        ]
    };
    for _ in 0..max_actions {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => return Some((state, holes)),
            NodeKind::Player(actor) => {
                visited.push(state.current_node_id);
                let actions = SimplifiedNlheGame::legal_actions(&state);
                let info = SimplifiedNlheGame::info_set(&state, actor);
                let probs = normalized_probs(&strat(&info), actions.len());
                let idx = sample_idx(&probs, rng);
                state = SimplifiedNlheGame::next(state, actions[idx], rng);
            }
            NodeKind::Chance => unreachable!("简化 NLHE 无 chance 节点"),
        }
    }
    None
}

/// 归一；空 / 长度不符 / 全非正 → uniform（复刻 advisor 的 uniform fallback 层）。
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

fn write_f64s(w: &mut impl Write, xs: &[f64]) -> io::Result<()> {
    for &x in xs {
        w.write_all(&x.to_le_bytes())?;
    }
    Ok(())
}

fn write_u32s(w: &mut impl Write, xs: &[u32]) -> io::Result<()> {
    for &x in xs {
        w.write_all(&x.to_le_bytes())?;
    }
    Ok(())
}

fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_f64s(r: &mut impl Read, n: usize) -> io::Result<Vec<f64>> {
    let mut out = Vec::with_capacity(n);
    let mut b = [0u8; 8];
    for _ in 0..n {
        r.read_exact(&mut b)?;
        out.push(f64::from_le_bytes(b));
    }
    Ok(out)
}

fn read_u32s(r: &mut impl Read, n: usize) -> io::Result<Vec<u32>> {
    let mut out = Vec::with_capacity(n);
    let mut b = [0u8; 4];
    for _ in 0..n {
        r.read_exact(&mut b)?;
        out.push(u32::from_le_bytes(b));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abstraction::bucket_table::{BucketConfig, BucketTable};

    #[test]
    fn build_value_tables_smoke_and_root_consistency() {
        // 小 bucket 数 stub 表 + 未训练 trainer（策略 ≈ uniform）。验证累积逻辑、键一致性、
        // 序列化往返；不依赖真实 1B checkpoint。
        // SimplifiedNlheGame::new 只接受受支持的 bucket config（500/1000）。
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ));
        let game = SimplifiedNlheGame::new(table).expect("stub game");
        let trainer = DenseNlheEsMccfrTrainer::new(game, 7);

        let tables = build_value_tables(&trainer, 4000, 123, 400);

        let indexer = trainer.strategy_sum().indexer();
        assert_eq!(tables.total_rows, indexer.total_rows());

        // 至少有格被访问到。
        let pop0: u64 = tables.vf_count[0].iter().map(|&c| c as u64).sum();
        assert!(pop0 > 0, "SB vf 无任何访问");

        // 均值有限。
        for pos in 0..2 {
            for (r, &c) in tables.vf_count[pos].iter().enumerate() {
                if c > 0 {
                    assert!(
                        tables.vf_mean[pos][r].is_finite(),
                        "pos{pos} row{r} 均值非有限"
                    );
                }
            }
        }

        // 强不变量：root 处 V_info[pos, row(root, class)] 必须 == vroot[pos][class][*] 的类边际
        // （同一批手、同一 U 在 root 上累积；root bucket = preflop169 = class）。
        let game = trainer.game();
        let root = game.tree().root_id();
        for class in 0..N_PREFLOP_CLASSES {
            for pos in 0..2 {
                // class 在 169 内一定 < bucket_count(preflop=169)。
                let row = indexer.row_for(root, class as u32) as usize;
                let vf_c = tables.vf_count[pos][row];
                // vroot 类边际
                let mut root_cnt: u64 = 0;
                let mut root_sum: f64 = 0.0;
                for opp in 0..N_PREFLOP_CLASSES {
                    let i = class * N_PREFLOP_CLASSES + opp;
                    let c = tables.vroot_count[pos][i];
                    root_cnt += c as u64;
                    root_sum += tables.vroot_mean[pos][i] * c as f64;
                }
                // 计数必须逐位完全一致（root 是每手 visited[0]，两处对同一座位各加一次）。
                assert_eq!(
                    vf_c as u64, root_cnt,
                    "pos{pos} class{class}: vf_count {vf_c} != vroot 类边际 count {root_cnt}"
                );
                if vf_c > 0 {
                    let vf_mean = tables.vf_mean[pos][row];
                    let root_mean = root_sum / root_cnt as f64;
                    assert!(
                        (vf_mean - root_mean).abs() < 1e-6,
                        "pos{pos} class{class}: V_info[root] {vf_mean} != vroot 边际 {root_mean}"
                    );
                }
            }
        }

        // 序列化往返。
        let path = std::env::temp_dir().join(format!("aivat_vf_test_{}.bin", std::process::id()));
        tables.save(&path).expect("save");
        let loaded = AivatValueTables::load(&path).expect("load");
        assert_eq!(loaded.total_rows, tables.total_rows);
        assert_eq!(loaded.vf_mean[0], tables.vf_mean[0]);
        assert_eq!(loaded.vf_count[1], tables.vf_count[1]);
        assert_eq!(loaded.vroot_mean[0], tables.vroot_mean[0]);
        assert_eq!(loaded.bucket_blake3, tables.bucket_blake3);
        std::fs::remove_file(&path).ok();
    }
}
