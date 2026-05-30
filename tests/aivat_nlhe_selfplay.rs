//! AIVAT NLHE 估计器**无偏性主闸门**（`docs/aivat_eval.md` §7.2）。
//!
//! blueprint-vs-blueprint 自对弈（合成 σ）生成 Slumbot 同格式手局（双方底牌恒知 → 覆盖
//! "全量含对方发牌"），逐手同时算 raw 与 AIVAT，配对差 `d = AIVAT − raw`。大 N 下要求
//! `|mean(d)| ≤ k·SE(d)`（无偏）。
//!
//! **为什么合成 VF 就能抓 bug**：无偏对任意固定 V 成立，但 `E[c_t]=0` 要求 sibling 集合 +
//! 权重 == 真实条件分布。任何**非常数** V 下，sibling 集合错（board 枚举漏/多扣牌、街切换
//! V_child 偷看 realized、runout 重复扣、位置/桶错位）都会让某个 `E[c_t]≠0` → `mean(d)≠0`。
//! 故合成（varying）VF 的无偏闸门是对**结构**的强校验。变量缩减幅度另在真值表生产跑里量。
//!
//! `#[ignore]`：自对弈 + flop sibling C(48,3) 枚举较慢，走 `--release --ignored` 在 vultr 跑。

use std::sync::Arc;

use poker::abstraction::bucket_table::{BucketConfig, BucketTable};
use poker::training::aivat_nlhe::{AivatNlheEstimator, AivatValueFn, HandInput, LoggedDecision};
use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_betting_tree::{AbstractActionTag, NodeId};
use poker::training::nlhe_replay::{outgoing_incr, tag_short_name};
use poker::{ChaCha20Rng, InfoSetId, RngSource, StreetActionAbstraction};

const NODE_SHIFT: u32 = 38;

/// 合成值函数：`(pos, node, bucket)` / `(pos, 双方类)` 的确定性**非常数**映射（splitmix
/// 风格 hash → ±3000）。与 U 不必相关——无偏不依赖相关性；非常数即可激活所有 sibling 集合。
struct SyntheticVF;

fn mix(a: u64, b: u64, c: u64) -> f64 {
    let mut h = a.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ c.wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    ((h >> 11) as f64 / (1u64 << 53) as f64) * 6000.0 - 3000.0
}

impl AivatValueFn for SyntheticVF {
    fn v_info(&self, pos: usize, node: NodeId, bucket: u32) -> Option<f64> {
        Some(mix(pos as u64, node as u64, bucket as u64 + 1))
    }
    fn v_root_both(&self, pos: usize, our_class: usize, opp_class: usize) -> Option<f64> {
        Some(mix(
            pos as u64 + 100,
            our_class as u64,
            opp_class as u64 + 0x1000,
        ))
    }
}

/// 合成 σ：从 `info` 确定性派生的非均匀分布（长度 = 该 node legal 数）。self-play 抽样与
/// estimator 重算共用同一函数 → cross-check 必过。
fn synth_sigma(game: &SimplifiedNlheGame, info: InfoSetId) -> Vec<f64> {
    let node = (info.raw() >> NODE_SHIFT) as NodeId;
    let n = game.tree().node(node).legal_actions.len();
    let mut w = vec![0.0f64; n];
    let mut s = 0.0;
    for (i, wi) in w.iter_mut().enumerate() {
        let bits = (info
            .raw()
            .wrapping_mul(2_654_435_761)
            .wrapping_add(i as u64)
            % 7)
            + 1;
        *wi = bits as f64;
        s += *wi;
    }
    for wi in &mut w {
        *wi /= s;
    }
    w
}

fn sample(probs: &[f64], rng: &mut dyn RngSource) -> usize {
    let r = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
    let mut cum = 0.0;
    for (i, &p) in probs.iter().enumerate() {
        cum += p;
        if r < cum {
            return i;
        }
    }
    probs.len() - 1
}

/// 自对弈一手（双方打合成 σ），产 Slumbot 同格式 [`HandInput`]。`our_seat` 指定我方座位。
fn play_hand(
    game: &SimplifiedNlheGame,
    abstraction: &StreetActionAbstraction,
    our_seat: u8,
    rng: &mut dyn RngSource,
) -> Option<HandInput> {
    let mut state = game.root(rng);
    let holes = {
        let p = state.game_state.players();
        [p[0].hole_cards?, p[1].hole_cards?]
    };
    let mut action = String::new();
    let mut prev_street: Option<u8> = None;
    let mut log_decisions: Vec<LoggedDecision> = Vec::new();

    for _ in 0..512 {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => {
                let board = state.game_state.board().to_vec();
                let payouts = state.game_state.payouts()?;
                let winnings = payouts.iter().find(|(s, _)| s.0 == our_seat)?.1;
                return Some(HandInput {
                    client_pos: 1 - our_seat,
                    our_hole: holes[our_seat as usize],
                    opp_hole: holes[1 - our_seat as usize],
                    board,
                    action,
                    winnings,
                    log_decisions,
                });
            }
            NodeKind::Player(actor) => {
                let node = state.current_node_id;
                let street = game.tree().node(node).street as u8;
                let legal = SimplifiedNlheGame::legal_actions(&state);
                let info = SimplifiedNlheGame::info_set(&state, actor);
                let sigma = synth_sigma(game, info);
                let idx = sample(&sigma, rng);
                let chosen = legal[idx];

                if let Some(ps) = prev_street {
                    if street != ps {
                        action.push('/');
                    }
                }
                let incr = outgoing_incr(&state.game_state, abstraction, chosen).ok()?;
                action.push_str(&incr);
                prev_street = Some(street);

                if actor == our_seat {
                    let probs_by_name = legal
                        .iter()
                        .zip(&sigma)
                        .map(|(a, p)| {
                            (
                                tag_short_name(AbstractActionTag::of(a)),
                                (p * 1e4).round() / 1e4,
                            )
                        })
                        .collect();
                    log_decisions.push(LoggedDecision {
                        info_set: info.raw(),
                        probs_by_name,
                        chosen: tag_short_name(AbstractActionTag::of(&chosen)),
                        fallback_uniform: false,
                    });
                }
                state = SimplifiedNlheGame::next(state, chosen, rng);
            }
            NodeKind::Chance => return None,
        }
    }
    None
}

#[test]
#[ignore = "自对弈 + flop C(48,3) sibling 枚举较慢；--release --ignored 在 vultr 跑"]
fn aivat_nlhe_unbiased_selfplay() {
    let table = Arc::new(BucketTable::stub_for_postflop(
        BucketConfig::default_500_500_500(),
    ));
    let game = SimplifiedNlheGame::new(table).expect("stub game");
    let abstraction = StreetActionAbstraction::default_6_action();
    let vf = SyntheticVF;
    let sigma_fn: Box<dyn Fn(InfoSetId) -> Vec<f64>> = Box::new(|info| synth_sigma(&game, info));
    let estimator = AivatNlheEstimator::new(&game, &vf, sigma_fn);

    let n_hands = 6000;
    let mut rng = ChaCha20Rng::from_seed(0xA1_7A_7E_57_5E_1F_91_05);

    // 配对差 d 与 raw / aivat 的在线统计。
    let (mut sum_d, mut sum_d2) = (0.0f64, 0.0f64);
    let (mut sum_raw, mut sum_raw2) = (0.0f64, 0.0f64);
    let (mut sum_av, mut sum_av2) = (0.0f64, 0.0f64);
    let mut n = 0u64;
    let mut failed = 0u64;

    for h in 0..n_hands {
        let our_seat = (h % 2) as u8; // 交替位置，覆盖 SB/BB
        let Some(input) = play_hand(&game, &abstraction, our_seat, &mut rng) else {
            failed += 1;
            continue;
        };
        let r = estimator.estimate_hand(&input).unwrap_or_else(|e| {
            panic!(
                "estimate_hand 失败（hand {h}）: {e}\n  action={}",
                input.action
            )
        });
        let d = r.aivat - r.raw;
        sum_d += d;
        sum_d2 += d * d;
        sum_raw += r.raw;
        sum_raw2 += r.raw * r.raw;
        sum_av += r.aivat;
        sum_av2 += r.aivat * r.aivat;
        n += 1;
    }

    assert!(failed == 0, "{failed} 手 play_hand 失败（应 0）");
    assert!(n >= 100, "有效手数太少 {n}");
    let nf = n as f64;
    let mean_d = sum_d / nf;
    let var_d = (sum_d2 - sum_d * sum_d / nf) / (nf - 1.0);
    let se_d = (var_d / nf).sqrt();
    let mean_raw = sum_raw / nf;
    let se_raw = (((sum_raw2 - sum_raw * sum_raw / nf) / (nf - 1.0)) / nf).sqrt();
    let mean_av = sum_av / nf;
    let se_av = (((sum_av2 - sum_av * sum_av / nf) / (nf - 1.0)) / nf).sqrt();

    eprintln!("[selfplay gate] n={n}");
    eprintln!("  raw   mean={mean_raw:.2}  SE={se_raw:.2}");
    eprintln!("  AIVAT mean={mean_av:.2}  SE={se_av:.2}");
    eprintln!(
        "  d=AIVAT−raw mean={mean_d:.3}  SE(d)={se_d:.3}  |mean|/SE={:.2}",
        mean_d.abs() / se_d
    );

    // 无偏主闸门：配对差均值落在 ±4·SE(d) 内（真偏差会是很多个 SE；4σ 极少误报）。
    assert!(
        mean_d.abs() <= 4.0 * se_d,
        "无偏闸门失败：|mean(d)|={:.3} > 4·SE(d)={:.3}（mean_d={mean_d:.3} se_d={se_d:.3}）——\
         某个 c_t 的 sibling 集合/权重或 telescoping 错了",
        mean_d.abs(),
        4.0 * se_d
    );
}
