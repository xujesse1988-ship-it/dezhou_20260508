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
    /// 6-max 100BB 的默认配置：6 座、起始 100BB、SB=50、BB=100、ante=0、按钮在座位 0。
    pub fn default_6max_100bb() -> TableConfig {
        TableConfig {
            n_seats: 6,
            starting_stacks: vec![ChipAmount::new(10_000); 6],
            small_blind: ChipAmount::new(50),
            big_blind: ChipAmount::new(100),
            ante: ChipAmount::ZERO,
            button_seat: SeatId(0),
        }
    }
}
