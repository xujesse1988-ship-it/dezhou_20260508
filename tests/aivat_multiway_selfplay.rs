//! 多人 AIVAT 无偏闸门（缺口⑥，`docs/aivat_eval.md` §7 口径推广到 N 座）。
//!
//! 6-max 自对弈（**底牌相关**的合成策略：弃牌 / 下注 / all-in 概率显式依赖 169 类——
//! card-bunching 近似若实质有偏会在此暴露，见 `aivat_multiway` 模块 doc），估计器只喂
//! **live 可见信息**（我方底牌 + 摊牌亮牌 + board + 动作序 + winnings；弃牌座底牌不给）。
//! 配对检验：`d(z) = AIVAT(z) − raw(z)`，要求 `|mean(d)| ≤ 1.96 · SD(d)/√N`。
//!
//! - 默认（smoke，~800 手，vf=None = 纯 runout 修正）：跑得快、抓结构性偏差（重放 / 锁定
//!   判定 / side-pot 结算 / 未知弃牌牌堆）。固定 seed → 确定性，非随机闸门。
//! - `#[ignore]`（全量，6000 手 + 独立自对弈建 VF-1 喂 `c_deal_us`）：HU §7 同级闸门 +
//!   SE 缩减读数（`--ignored --nocapture` 看数）。

use poker::training::aivat_multiway::{
    chips_to_mbb_per_hand, MultiwayAivatEstimator, MultiwayDealValueFn, MultiwayHandInput,
};
use poker::{Action, Card, ChaCha20Rng, GameState, PlayerStatus, RngSource, SeatId, TableConfig};

/// 牌力特征（只要求**确定性依赖底牌**，方向无所谓）：点数和 + 对子 + 同花。
fn hand_class(hole: [Card; 2]) -> u64 {
    let r0 = (hole[0].to_u8() / 4) as u64;
    let r1 = (hole[1].to_u8() / 4) as u64;
    let pair = u64::from(r0 == r1) * 30;
    let suited = u64::from(hole[0].to_u8() % 4 == hole[1].to_u8() % 4) * 7;
    r0 + r1 + pair + suited // 0..=67
}

/// 一手自对弈：底牌相关合成策略驱动到终局。返回 (终局 state, 动作序)。
fn drive_hand(cfg: &TableConfig, hand_seed: u64) -> (GameState, Vec<(SeatId, Action)>) {
    let mut deal_rng = ChaCha20Rng::from_seed(hand_seed);
    let mut st = GameState::with_rng(cfg, hand_seed, &mut deal_rng);
    let mut pol_rng = ChaCha20Rng::from_seed(hand_seed ^ 0x504F_4C49_4359_5F30); // "POLICY_0"
    let mut actions = Vec::new();
    let mut guard = 0;
    while !st.is_terminal() {
        let Some(seat) = st.current_player() else {
            break;
        };
        let la = st.legal_actions();
        let hole = st.players()[seat.0 as usize]
            .hole_cards
            .expect("行动者有底牌");
        let s = hand_class(hole); // 0..=67，越大越「强」
        let roll = pol_rng.next_u64() % 100;
        // all-in 只在 postflop（preflop 锁定的 E_runout = C(48,5)≈1.7M 精确枚举，测试承受不
        // 起；k=5 与 k≤2 走完全相同的估计器代码路径，不损覆盖——见 aivat_multiway 模块 doc）。
        let allow_allin = !st.board().is_empty() && la.all_in_amount.is_some();
        let action = if la.call.is_some() {
            // 面对下注：弱牌更常弃（**底牌相关弃牌** = card-bunching 近似的压力测试）。
            let fold_p = 55u64.saturating_sub(s); // s=0 → 55%；s≥55 → 0%
            if roll < fold_p && la.fold {
                Action::Fold
            } else if roll < fold_p + 12 && allow_allin && s > 40 {
                Action::AllIn
            } else {
                Action::Call
            }
        } else {
            // 可 check：强牌更常进攻。
            let aggr_p = 15 + s; // 15%..82%
            if roll < aggr_p {
                if roll % 7 == 0 && allow_allin {
                    Action::AllIn
                } else if let Some((min_to, _)) = la.bet_range {
                    Action::Bet { to: min_to }
                } else if let Some((min_to, _)) = la.raise_range {
                    Action::Raise { to: min_to }
                } else {
                    Action::Check
                }
            } else {
                Action::Check
            }
        };
        st.apply(action).expect("合成策略动作应合法");
        actions.push((seat, action));
        guard += 1;
        assert!(guard < 200, "一手 200 步未终局（策略环死循环？）");
    }
    assert!(st.is_terminal(), "驱动应到终局");
    (st, actions)
}

/// 终局 state → 估计器输入（**live 可见口径**：弃牌座底牌不给）。`hero_hole` 由调用方从
/// 根态读（hero 弃牌后引擎置 `None`，终局态读不到）。
fn live_input(
    cfg: &TableConfig,
    st: &GameState,
    actions: &[(SeatId, Action)],
    hero: usize,
    hero_hole: [Card; 2],
) -> MultiwayHandInput {
    let n = cfg.n_seats as usize;
    let contenders: Vec<usize> = (0..n)
        .filter(|&i| st.players()[i].status != PlayerStatus::Folded)
        .collect();
    let showdown = contenders.len() >= 2;
    let revealed: Vec<Option<[Card; 2]>> = (0..n)
        .map(|i| {
            if showdown && contenders.contains(&i) {
                st.players()[i].hole_cards
            } else {
                None
            }
        })
        .collect();
    let payouts = st.payouts().expect("终局有 payouts");
    let winnings = payouts
        .iter()
        .find(|(s, _)| s.0 as usize == hero)
        .unwrap()
        .1;
    MultiwayHandInput {
        config: cfg.clone(),
        our_seat: SeatId(hero as u8),
        our_hole: hero_hole,
        revealed,
        board: st.board().to_vec(),
        actions: actions.to_vec(),
        winnings,
    }
}

struct Stats {
    n: usize,
    mean: f64,
    sd: f64,
}

fn stats(xs: &[f64]) -> Stats {
    let n = xs.len();
    let mean = xs.iter().sum::<f64>() / n as f64;
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n as f64 - 1.0);
    Stats {
        n,
        mean,
        sd: var.sqrt(),
    }
}

/// 跑 K 手、返回 (raw, aivat, d) 向量 + 诊断计数。hero = hand_idx % 6。
#[allow(clippy::type_complexity)]
fn run_eval(
    cfg: &TableConfig,
    k: usize,
    seed0: u64,
    vf: Option<&dyn MultiwayDealValueFn>,
) -> (Vec<f64>, Vec<f64>, Vec<f64>, u64, u64) {
    let est = MultiwayAivatEstimator::new(vf);
    let mut raw = Vec::with_capacity(k);
    let mut aivat = Vec::with_capacity(k);
    let mut d = Vec::with_capacity(k);
    let mut n_runout: u64 = 0;
    let mut n_unknown_folded: u64 = 0;
    for h in 0..k {
        let hand_seed = seed0
            .wrapping_add(h as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let (st, actions) = drive_hand(cfg, hand_seed);
        let hero = h % cfg.n_seats as usize;
        // hero 弃牌后底牌在终局态读不到 → 重发一遍同 seed 的根态读（发牌只依赖 (cfg,seed,rng)）。
        let mut deal_rng = ChaCha20Rng::from_seed(hand_seed);
        let root = GameState::with_rng(cfg, hand_seed, &mut deal_rng);
        let hero_hole = root.players()[hero].hole_cards.expect("根态有底牌");
        let input = live_input(cfg, &st, &actions, hero, hero_hole);
        let r = est
            .estimate_hand(&input)
            .unwrap_or_else(|e| panic!("hand {h} 估计失败（不允许选择性跳过）: {e}"));
        raw.push(r.raw);
        aivat.push(r.aivat);
        d.push(r.aivat - r.raw);
        n_runout += u64::from(r.has_runout);
        n_unknown_folded += r.n_unknown_folded as u64;
    }
    (raw, aivat, d, n_runout, n_unknown_folded)
}

/// smoke 闸门（默认跑，~800 手，vf=None）：|mean(d)| ≤ 1.96·SE(d)，且 runout 手占比非零
/// （否则闸门没测到主修正项）。固定 seed → 确定性。
#[test]
fn multiway_aivat_unbiased_smoke() {
    let cfg = TableConfig::default_6max_100bb();
    let (raw, aivat, d, n_runout, _) = run_eval(&cfg, 800, 0x4D57_4149_5641_5431, None);
    let sd_stats = stats(&d);
    let se = sd_stats.sd / (sd_stats.n as f64).sqrt();
    assert!(
        n_runout > 40,
        "runout 手过少（{n_runout}/800），闸门没压到 c_runout 主路径"
    );
    assert!(
        sd_stats.mean.abs() <= 1.96 * se,
        "配对无偏闸门失败：mean(d)={:.3} vs 1.96·SE={:.3}（N={}，raw_mean={:.3}，aivat_mean={:.3}）",
        sd_stats.mean,
        1.96 * se,
        sd_stats.n,
        stats(&raw).mean,
        stats(&aivat).mean
    );
}

/// 全量闸门 + SE 缩减读数（`cargo test --release -- --ignored --nocapture`）：6000 手评测 +
/// **独立** 4000 手自对弈建 VF-1（fixed-V：建表手与评测手不相交 → 无偏不受影响）。
#[test]
#[ignore]
fn multiway_aivat_unbiased_full_with_vf() {
    let cfg = TableConfig::default_6max_100bb();
    let n_seats = cfg.n_seats as usize;

    // —— VF-1：独立自对弈（不同 seed 流），E[U | rel_pos, class]（visit-加权均值）——
    struct Vf1 {
        sum: Vec<f64>,
        cnt: Vec<u64>,
        classes: usize,
    }
    impl MultiwayDealValueFn for Vf1 {
        fn v_deal(&self, rel_pos: usize, class169: usize) -> Option<f64> {
            let i = rel_pos * self.classes + class169;
            (self.cnt.get(i).copied().unwrap_or(0) > 0).then(|| self.sum[i] / self.cnt[i] as f64)
        }
    }
    let classes = 68; // hand_class 值域 0..=67
    let mut vf = Vf1 {
        sum: vec![0.0; n_seats * classes],
        cnt: vec![0; n_seats * classes],
        classes,
    };
    let button = 0usize; // default_6max_100bb button=0
    for h in 0..4000u64 {
        let hand_seed = 0x5646_3153_4545_4400u64
            .wrapping_add(h)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut deal_rng = ChaCha20Rng::from_seed(hand_seed);
        let root = GameState::with_rng(&cfg, hand_seed, &mut deal_rng);
        let holes: Vec<[Card; 2]> = (0..n_seats)
            .map(|i| root.players()[i].hole_cards.expect("根态有底牌"))
            .collect();
        let (st, _) = drive_hand(&cfg, hand_seed);
        let payouts = st.payouts().expect("终局 payouts");
        for (seat, p) in payouts {
            let i = seat.0 as usize;
            let rel = (i + n_seats - button) % n_seats;
            let cls = hand_class(holes[i]) as usize;
            vf.sum[rel * classes + cls] += p as f64;
            vf.cnt[rel * classes + cls] += 1;
        }
    }

    // —— 评测（独立 seed 流）——
    let (raw, aivat, d, n_runout, n_unknown) =
        run_eval(&cfg, 6000, 0x4D57_4149_5641_5432, Some(&vf));
    let sd_stats = stats(&d);
    let se = sd_stats.sd / (sd_stats.n as f64).sqrt();
    let s_raw = stats(&raw);
    let s_aivat = stats(&aivat);
    let bb = cfg.big_blind.as_u64();
    eprintln!(
        "[multiway-aivat full] N={} runout_hands={} unknown_folded_total={}\n\
         raw:   mean={:.2} chips ({:.1} mbb/g)  SD={:.1}\n\
         aivat: mean={:.2} chips ({:.1} mbb/g)  SD={:.1}\n\
         d:     mean={:.3} ± 1.96·SE={:.3}  → SE 缩减 ×{:.3}（方差 ×{:.3}）",
        sd_stats.n,
        n_runout,
        n_unknown,
        s_raw.mean,
        chips_to_mbb_per_hand(s_raw.mean, bb),
        s_raw.sd,
        s_aivat.mean,
        chips_to_mbb_per_hand(s_aivat.mean, bb),
        s_aivat.sd,
        sd_stats.mean,
        1.96 * se,
        s_raw.sd / s_aivat.sd,
        (s_raw.sd / s_aivat.sd) * (s_raw.sd / s_aivat.sd),
    );
    assert!(n_runout > 300, "runout 手过少（{n_runout}/6000）");
    assert!(
        n_unknown > 0,
        "应有未知弃牌座样本（card-bunching 近似被压到）"
    );
    assert!(
        sd_stats.mean.abs() <= 1.96 * se,
        "配对无偏闸门失败：mean(d)={:.3} vs 1.96·SE={:.3}",
        sd_stats.mean,
        1.96 * se
    );
}
