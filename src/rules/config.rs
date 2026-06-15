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
        TableConfig {
            n_seats: 6,
            // 100 chips = 1BB（与 default 一致：100BB = 10_000 chips）。
            starting_stacks: vec![ChipAmount::new(u64::from(stack_bb) * 100); 6],
            small_blind: ChipAmount::new(50),
            big_blind: ChipAmount::new(100),
            ante: ChipAmount::ZERO,
            button_seat: SeatId(0),
        }
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
            button_seat: SeatId(0),
        }
    }
}
