//! 桌面配置（API §3）。

use crate::core::{ChipAmount, SeatId};

/// 桌面配置。
///
/// 阶段 1 假定全程无 sit-in/sit-out（D-032）：所有 `n_seats` 座位从模拟开始
/// 到结束全程在场，按钮每手向左移动一格。
#[derive(Clone, Debug)]
pub struct TableConfig {
    /// 2..=9，默认 6（D-030）。
    pub n_seats: u8,
    /// 长度 = `n_seats`。**发盲注 / ante 之前** 的座位栈（D-024 / I-001）。
    pub starting_stacks: Vec<ChipAmount>,
    /// 默认 50 chips。
    pub small_blind: ChipAmount,
    /// 默认 100 chips。
    pub big_blind: ChipAmount,
    /// 默认 0（D-024 / D-031）。
    pub ante: ChipAmount,
    /// 不交 ante 的座位（bit i = 座 i），默认 0 = 人人都交。实盘短桌把 N 座树里多出来的空座
    /// 映成「开局先弃牌」的幻影座，真实桌上没有这几份 ante，故按空座豁免（见
    /// `openpoker_advisor` 的真栈重放）。`ante == 0` 时不起作用。
    pub ante_exempt: u16,
    /// 开局额外补一个 live 大盲的座位（bit i = 座 i），默认 0 = 没人补。实盘新进场 / 回座的玩家
    /// 补盲：补的 `big_blind` 算本轮下注（跟注额照扣、没人加注时轮到他可以过牌，同大盲的选择权），
    /// 行动顺序不变（不是 straddle）。只能标非盲注座（构造 [`crate::GameState`] 时校验）。
    pub live_posts: u16,
    /// 强制 straddle 额（chips），默认 0 = 没有 straddle。非 0 时大盲下家（UTG，3 座桌即按钮）每手
    /// 发盲注后再强制交这一份，算本轮下注；翻前从 straddle 下家开始说话，straddle 座最后说话、没人
    /// 加注时可过牌（同大盲的选择权）；翻前最小加注额 = straddle（1/2/4 桌最小加注到 8），翻后最小
    /// 下注仍是大盲。须 ≥ 大盲，只能用于 3 座以上（构造 [`crate::GameState`] 时校验）。
    pub straddle: ChipAmount,
    /// 抽水比例（万分之几），默认 0 = 不抽水（终局收益零和，与历史逐位相同）。非 0 时终局结算里，
    /// 每个座位从**独赢**的底池（主池 / 边池，平分的不算）拿到的筹码合计按这个比例向下取整扣一份，
    /// 每座封顶 [`rake_cap`](Self::rake_cap)；没人跟的注先退回、不计入。收益不再零和。
    /// wopo ante 桌实测 = 400（4%）、封顶 10BB（见 `tools/wopo_rake_check.py`）。
    pub rake_bps: u32,
    /// 每座抽水封顶（chips），0 = 不封顶。`rake_bps == 0` 时不起作用。
    pub rake_cap: ChipAmount,
    /// 起始按钮位（默认 `SeatId(0)`，由 D-022b 推出 SB=1 / BB=2）。
    pub button_seat: SeatId,
}

impl TableConfig {
    /// 6-max，起始码深 = `stack_bb` 个大盲（BB=100 chips、SB=50、ante=0、按钮在座位 0）。
    /// `default_6max_100bb()` == `six_max_at_bb(100)`，逐字段一致。
    ///
    /// 深浅 grid 训练（`train_cfr --reshape preopen-1pot --stack-bb <N>`）传 50/100/200/300/400
    /// 等建对应码深的树：betting tree 只按 `AbstractActionTag` 分叉、all-in 阈值在运行期按真实
    /// per-seat `committed + stack` 现算，故更深的 `starting_stacks` 自动多出后续加注层（深码
    /// preflop 多 4bet/5bet 层、postflop 高 SPR 多街），无需改建树代码。注意：深码树更大，
    /// node_id 上界 2^26（见 `nlhe_betting_tree` v2 packing），实际节点数看 train_cfr 的
    /// `tree_nodes` 日志。
    pub fn six_max_at_bb(stack_bb: u32) -> TableConfig {
        Self::n_max_at_bb(6, stack_bb)
    }

    /// `n_seats` 座、起始码深 = `stack_bb` 个大盲（BB=100 chips、SB=50、ante=0、按钮在座位 0）。
    /// `six_max_at_bb(bb)` == `n_max_at_bb(6, bb)`，逐字段一致。`train_cfr --seats <N>` 用它
    /// 建 N 人桌（如 9-max）。`n_seats` 合法范围 2..=9 由 [`crate::GameState`] 构造时校验，本函数
    /// 不重复检查；注意 `n_seats == 2` 走 HU 语义（button = SB）。
    pub fn n_max_at_bb(n_seats: u8, stack_bb: u32) -> TableConfig {
        TableConfig {
            n_seats,
            // 100 chips = 1BB（与 default 一致：100BB = 10_000 chips）。
            starting_stacks: vec![ChipAmount::new(u64::from(stack_bb) * 100); usize::from(n_seats)],
            small_blind: ChipAmount::new(50),
            big_blind: ChipAmount::new(100),
            ante: ChipAmount::ZERO,
            ante_exempt: 0,
            live_posts: 0,
            straddle: ChipAmount::ZERO,
            rake_bps: 0,
            rake_cap: ChipAmount::ZERO,
            button_seat: SeatId(0),
        }
    }

    /// 把 `straddle`（UTG 强制 straddle 额，chips）换成给定值，其余字段不变。`train_cfr --straddle
    /// <chips>` 用它在 `n_max_at_bb(..).with_ante(..)` 之上叠 straddle。
    pub fn with_straddle(mut self, straddle: ChipAmount) -> TableConfig {
        self.straddle = straddle;
        self
    }

    /// 把抽水换成给定值（`rake_bps` 万分比、`rake_cap` 每座封顶 chips，0 = 不封顶），其余字段不变。
    /// `train_cfr --rake-bps <n> --rake-cap <chips>` 用它叠在 `n_max_at_bb(..).with_ante(..)` 之上。
    pub fn with_rake(mut self, rake_bps: u32, rake_cap: ChipAmount) -> TableConfig {
        self.rake_bps = rake_bps;
        self.rake_cap = rake_cap;
        self
    }

    /// 独赢 `amount` chips 要交的抽水：`amount × rake_bps / 10000` 向下取整，封顶 `rake_cap`
    /// （0 = 不封顶）。`rake_bps == 0` 恒为 0。
    pub fn rake_on(&self, amount: u64) -> u64 {
        if self.rake_bps == 0 {
            return 0;
        }
        let raw = amount * u64::from(self.rake_bps) / 10_000;
        if self.rake_cap > ChipAmount::ZERO {
            raw.min(self.rake_cap.as_u64())
        } else {
            raw
        }
    }

    /// `rake_bps` 是否合法：不超过 10000（100%）。
    pub fn rake_valid(&self) -> bool {
        self.rake_bps <= 10_000
    }

    /// straddle 座（大盲下家）；没有 straddle（`straddle == 0`）或不足 3 座时返回 `None`。
    pub fn straddle_seat(&self) -> Option<SeatId> {
        if self.straddle == ChipAmount::ZERO || self.n_seats < 3 {
            return None;
        }
        let (_, bb) = self.blind_seats();
        Some(SeatId((bb.0 + 1) % self.n_seats))
    }

    /// 翻前「一份大盲」的额：有 straddle 时是 straddle，否则是大盲。翻前开局要跟的额、最小加注
    /// 额，以及「没人加注」（最高注 == 这个额）的判定都按它。
    pub fn preflop_blind(&self) -> ChipAmount {
        if self.straddle > ChipAmount::ZERO {
            self.straddle
        } else {
            self.big_blind
        }
    }

    /// 座 `seat` 开局是否交了算本轮下注的强制注（SB / BB / straddle / 补盲）。
    pub fn posts_blind(&self, seat: SeatId) -> bool {
        let (sb, bb) = self.blind_seats();
        seat == sb
            || seat == bb
            || self.straddle_seat() == Some(seat)
            || self.posts_live_blind(seat)
    }

    /// `straddle` 是否合法：0（没有），或 ≥ 大盲且座位数 ≥ 3。
    pub fn straddle_valid(&self) -> bool {
        self.straddle == ChipAmount::ZERO || (self.straddle >= self.big_blind && self.n_seats >= 3)
    }

    /// 把 `ante`（每座 chips，发盲注前人人都交、计入底池但不算本轮下注）换成给定值，其余字段
    /// 不变。`train_cfr --ante <chips>` 用它在 `n_max_at_bb` / `default_hu_200bb` 之上叠 ante。
    pub fn with_ante(mut self, ante: ChipAmount) -> TableConfig {
        self.ante = ante;
        self
    }

    /// 座 `seat` 是否交 ante（`ante == 0` 时也返回 true，此时交的是 0）。
    pub fn posts_ante(&self, seat: SeatId) -> bool {
        self.ante_exempt & (1u16 << seat.0) == 0
    }

    /// 座 `seat` 开局是否补 live 大盲（见 [`TableConfig::live_posts`]）。
    pub fn posts_live_blind(&self, seat: SeatId) -> bool {
        self.live_posts & (1u16 << seat.0) != 0
    }

    /// (SB 座, BB 座)，与 [`crate::GameState`] 同一推导：2 座 button = SB（D-022b-rev1），
    /// 3 座以上 SB = button+1、BB = button+2（D-022b）。
    pub fn blind_seats(&self) -> (SeatId, SeatId) {
        let n = self.n_seats.max(1);
        let next = |s: SeatId| SeatId((s.0 + 1) % n);
        let sb = if n == 2 {
            self.button_seat
        } else {
            next(self.button_seat)
        };
        (sb, next(sb))
    }

    /// `live_posts` 是否合法：不越过 `n_seats`、不标盲注座（含 straddle 座）。
    pub fn live_posts_valid(&self) -> bool {
        let (sb, bb) = self.blind_seats();
        self.live_posts >> self.n_seats == 0
            && !self.posts_live_blind(sb)
            && !self.posts_live_blind(bb)
            && self
                .straddle_seat()
                .map_or(true, |s| !self.posts_live_blind(s))
    }

    /// 6-max 100BB 的默认配置：6 座、起始 100BB、SB=50、BB=100、ante=0、按钮在座位 0。
    pub fn default_6max_100bb() -> TableConfig {
        Self::six_max_at_bb(100)
    }

    /// Heads-up 200BB 的默认配置：2 座、起始 200BB、SB=50、BB=100、ante=0、按钮在座位 0。
    ///
    /// 200BB 与 Slumbot 等 heads-up 参考 bot 对齐，便于外部 head-to-head 评测。
    ///
    /// `GameState` 按标准 HU NLHE 语义推导：button seat 同时是 SB，非 button
    /// seat 是 BB，postflop 由 BB 先行动。
    pub fn default_hu_200bb() -> TableConfig {
        TableConfig {
            n_seats: 2,
            starting_stacks: vec![ChipAmount::new(20_000); 2],
            small_blind: ChipAmount::new(50),
            big_blind: ChipAmount::new(100),
            ante: ChipAmount::ZERO,
            ante_exempt: 0,
            live_posts: 0,
            straddle: ChipAmount::ZERO,
            rake_bps: 0,
            rake_cap: ChipAmount::ZERO,
            button_seat: SeatId(0),
        }
    }
}
