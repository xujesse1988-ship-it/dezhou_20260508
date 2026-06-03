//! `SubgameNlheGame`（S6 实时搜索 6a：单层 subgame depth-limited re-solve 的 `Game` 壳）。
//!
//! 设计依据 `docs/temp/realtime_search_design_2026_06_03.md`。把"以中途 public state 为根、对各家
//! range 做期望"的 subgame 包成一个 [`Game`]，从而**原样复用** [`EsMccfrTrainer`] /
//! [`VanillaCfrTrainer`]（`trainer.rs` 一行不改）。
//!
//! # 三个复用 + 一个新 root
//!
//! - **State / 动作 / infoset / 转移 / 终局收益全部 delegate [`SimplifiedNlheGame`]**：`State` 仍是
//!   [`SimplifiedNlheState`]（自带 `tree` / `abs` / `bucket_table` Arc），故 `current` / `info_set`
//!   / `legal_actions` / `next` / `payoff` 直接转调 `SimplifiedNlheGame::*`——它们只读 state 携带的
//!   字段，state 带的是**子树**就在子树上跑（关联函数与 Game token 无关）。
//! - **只重写 [`Game::root`]**：不走开局 `with_rng_no_history`（uniform 全局发牌），而是
//!   `template.resample_hidden(rng)`——保留中途 public state（街 / 公共牌前缀 / 下注 / 行动权）、
//!   重发隐藏牌（各家底牌 + 未见 runout）。[`EsMccfrTrainer::step`] 每 step 调一次 `root` →
//!   每 step 一个隐藏信息补全 = external chance sampling；MVP 用 uniform range。
//! - **终局收益仍走权威 [`GameState::payouts`]**（side pot / showdown 逻辑不改）→ S1 PokerKit
//!   跨验证不受影响。
//!
//! # MVP 边界（`realtime_search_design` §10）
//!
//! - range = **uniform**（resample 任意补全），非 blueprint 加权；
//! - 当前街用**与 blueprint 同**的 action abstraction（finer 菜单留后续）；
//! - depth-limit = 解到 subgame 终局（不截断、无 biased leaf；6b 再上）；
//! - 子树 node_id 是**子树本地**索引（从 0 起），与 blueprint 全局树 node_id 不同口径——MVP 解到
//!   终局、不查 blueprint，故子树自洽即可；6b 接 blueprint 续局值时再做 local↔global 映射。

use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use crate::abstraction::action::StreetActionAbstraction;
use crate::abstraction::bucket_table::BucketTable;
use crate::core::rng::RngSource;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::nlhe::{
    SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheInfoSet, SimplifiedNlheState,
};
use crate::training::nlhe_betting_tree::{BettingAbstractionRules, PublicBettingTree};

/// 以一个中途 public state 为根的 subgame `Game`（S6 6a）。
pub struct SubgameNlheGame {
    config: TableConfig,
    /// 从 `template` 中途状态建的 betting 子树（[`PublicBettingTree::build_subtree`]）。
    subtree: Arc<PublicBettingTree>,
    /// 子树 + 运行期 `legal_actions` 同源的 action abstraction（须与建 `subtree` 用的一致）。
    abs: Arc<StreetActionAbstraction>,
    bucket_table: Arc<BucketTable>,
    /// 中途真实状态（实时搜索里 = 权威局 `auth.clone()`）。`root` 每次 clone 它再
    /// [`GameState::resample_hidden`] 重发隐藏牌。
    template: GameState,
}

impl SubgameNlheGame {
    /// 从中途 `template` 状态 + action `abs` + A3×A4 `rules` 建 subgame。`entrants` /
    /// `raises_on_street` = `template` 处的 A3×A4 上下文（调用方据权威局现算；`rules == Default`
    /// 时不被读，传 `(0, 0)`）。`abs` 必须与建子树用的一致（运行期 `legal_actions` 同源）。
    ///
    /// 前置：`template` 是非终局 decision 节点（实时搜索从决策点为根）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bucket_table: Arc<BucketTable>,
        config: TableConfig,
        abs: StreetActionAbstraction,
        rules: BettingAbstractionRules,
        template: GameState,
        entrants: u16,
        raises_on_street: u32,
    ) -> Self {
        debug_assert!(
            !template.is_terminal() && template.current_player().is_some(),
            "SubgameNlheGame::new: template 须是非终局 decision 节点"
        );
        let subtree = Arc::new(PublicBettingTree::build_subtree(
            &template,
            &abs,
            rules,
            entrants,
            raises_on_street,
        ));
        Self {
            config,
            subtree,
            abs: Arc::new(abs),
            bucket_table,
            template,
        }
    }

    /// 子树（诊断 / 评测：取 root_id 构造查询 infoset）。
    pub fn subtree(&self) -> &PublicBettingTree {
        &self.subtree
    }

    /// 中途模板状态（决定 root 的 betting 几何 + 真实公共牌前缀）。
    pub fn template(&self) -> &GameState {
        &self.template
    }
}

impl Game for SubgameNlheGame {
    type State = SimplifiedNlheState;
    type Action = SimplifiedNlheAction;
    type InfoSet = SimplifiedNlheInfoSet;

    // subgame 不存 checkpoint（实时一次性求解）；复用 SimplifiedNlhe 变体 tag。
    const VARIANT: crate::error::GameVariant = crate::error::GameVariant::SimplifiedNlhe;

    fn bucket_table_blake3(&self) -> [u8; 32] {
        self.bucket_table.content_hash()
    }

    fn n_players(&self) -> usize {
        self.config.n_seats as usize
    }

    fn root(&self, rng: &mut dyn RngSource) -> SimplifiedNlheState {
        // 唯一与 SimplifiedNlheGame 不同处：保留中途 public state、重发隐藏牌（per-step chance）。
        let game_state = self.template.resample_hidden(rng);
        SimplifiedNlheState {
            game_state,
            action_history: Vec::new(),
            bucket_table: Arc::clone(&self.bucket_table),
            current_node_id: self.subtree.root_id(),
            tree: Arc::clone(&self.subtree),
            abs: Arc::clone(&self.abs),
            info_set_cache: AtomicU64::new(0),
        }
    }

    // 以下全部 delegate SimplifiedNlheGame——它们只读 state 携带的 tree/abs/game_state，
    // state 带子树就在子树上跑（关联函数，与 Game token 无关）。
    fn current(state: &SimplifiedNlheState) -> NodeKind {
        SimplifiedNlheGame::current(state)
    }

    fn info_set(state: &SimplifiedNlheState, actor: PlayerId) -> SimplifiedNlheInfoSet {
        SimplifiedNlheGame::info_set(state, actor)
    }

    fn legal_actions(state: &SimplifiedNlheState) -> Vec<SimplifiedNlheAction> {
        SimplifiedNlheGame::legal_actions(state)
    }

    fn next(
        state: SimplifiedNlheState,
        action: SimplifiedNlheAction,
        rng: &mut dyn RngSource,
    ) -> SimplifiedNlheState {
        SimplifiedNlheGame::next(state, action, rng)
    }

    fn chance_distribution(state: &SimplifiedNlheState) -> Vec<(SimplifiedNlheAction, f64)> {
        SimplifiedNlheGame::chance_distribution(state)
    }

    fn payoff(state: &SimplifiedNlheState, player: PlayerId) -> f64 {
        SimplifiedNlheGame::payoff(state, player)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abstraction::action::AbstractAction;
    use crate::abstraction::bucket_table::BucketConfig;
    use crate::core::rng::ChaCha20Rng;
    use crate::training::nlhe_betting_tree::AbstractActionTag;
    use crate::training::trainer::{EsMccfrTrainer, Trainer};

    fn stub_table() -> Arc<BucketTable> {
        Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ))
    }

    /// 把 HU 默认 game 推到一个 flop 中途状态（SB complete → BB check → flop），返回该
    /// `SimplifiedNlheState`（其 `game_state` 即可作 subgame template）。
    fn hu_flop_state(game: &SimplifiedNlheGame, seed: u64) -> SimplifiedNlheState {
        let mut rng = ChaCha20Rng::from_seed(seed);
        let rng: &mut dyn RngSource = &mut rng;
        let mut state = game.root(rng);
        // SB(button) complete。
        let call = SimplifiedNlheGame::legal_actions(&state)
            .into_iter()
            .find(|a| AbstractActionTag::of(a) == AbstractActionTag::Call)
            .expect("SB 根应有 Call(complete)");
        state = SimplifiedNlheGame::next(state, call, rng);
        // BB option check → flop。
        let check = SimplifiedNlheGame::legal_actions(&state)
            .into_iter()
            .find(|a| matches!(a, AbstractAction::Check))
            .expect("BB option 应有 Check");
        state = SimplifiedNlheGame::next(state, check, rng);
        assert_eq!(state.game_state.board().len(), 3, "应进 flop（板 3 张）");
        assert!(!state.game_state.is_terminal() && state.game_state.current_player().is_some());
        state
    }

    /// 端到端 MVP plumbing：`build_subtree` + `resample_hidden` + `EsMccfrTrainer<SubgameNlheGame>`
    /// 跑通——CFR 在 flop subgame 上每 step 重发隐藏牌、走子树到权威终局、累积策略；①不 panic、
    /// ②update_count 准、③累积到 ≥1 个 infoset、④同 seed 两 trainer 逐 infoset byte-equal（可复现）。
    #[test]
    fn subgame_cfr_runs_and_is_deterministic() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game");
        let flop = hu_flop_state(&game, 0x5542_4732_4D45_5F30); // "SUBG2ME_0"
        let template = flop.game_state.clone();

        let make = || {
            SubgameNlheGame::new(
                stub_table(),
                TableConfig::default_hu_200bb(),
                StreetActionAbstraction::default_6_action(),
                BettingAbstractionRules::default(),
                template.clone(),
                0,
                0,
            )
        };
        let sub = make();
        assert!(sub.subtree().num_nodes() > 0, "子树非空");
        assert_eq!(sub.n_players(), 2);

        let steps = 600u64;
        let run = |seed: u64| {
            let mut tr = EsMccfrTrainer::new(make(), seed);
            let mut rng = ChaCha20Rng::from_seed(seed ^ 0xC0FF_EE00);
            for _ in 0..steps {
                tr.step(&mut rng).expect("subgame step");
            }
            tr
        };
        let a = run(0xA1);
        let b = run(0xA1);

        assert_eq!(a.update_count(), steps, "update_count 应 == steps");
        assert!(
            !a.strategy_sum().inner().is_empty(),
            "subgame CFR 应累积到 ≥1 个 infoset"
        );

        // 同 seed → 两 trainer 逐 infoset average_strategy byte-equal（byte-equal 可复现）。
        assert_eq!(
            a.strategy_sum().inner().len(),
            b.strategy_sum().inner().len(),
            "同 seed 两 trainer 表大小须一致"
        );
        for (info, _) in a.strategy_sum().inner().iter() {
            assert_eq!(
                a.average_strategy(info),
                b.average_strategy(info),
                "同 seed 两 trainer 在 infoset {info:?} 策略须 byte-equal"
            );
        }

        // 对 hero 真实手的 root infoset：若被访问到，则是合法分布（len 对齐 + 和≈1）。
        let actor = template.current_player().expect("flop 有行动者").0 as PlayerId;
        let query = SimplifiedNlheState {
            game_state: template.clone(),
            action_history: Vec::new(),
            bucket_table: stub_table(),
            current_node_id: sub.subtree().root_id(),
            tree: Arc::clone(&sub.subtree),
            abs: Arc::clone(&sub.abs),
            info_set_cache: AtomicU64::new(0),
        };
        let info = SimplifiedNlheGame::info_set(&query, actor);
        let avg = a.average_strategy(&info);
        if !avg.is_empty() {
            let n_legal = SimplifiedNlheGame::legal_actions(&query).len();
            assert_eq!(avg.len(), n_legal, "root 策略维度应 == 合法动作数");
            let sum: f64 = avg.iter().sum();
            assert!((sum - 1.0).abs() < 1e-9, "root 策略应归一，和={sum}");
        }
    }
}
