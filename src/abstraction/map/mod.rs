//! 运行时映射热路径子模块（D-252 / D-273）。
//!
//! 本模块持有 `InfoAbstraction::map` / `BucketTable::lookup` 等运行时映射调用的
//! 内部实现细节（`InfoSetId` 位编码、preflop / postflop bucket id 派发等）。
//! 所有计算必须为整数路径——浮点特征提取 / clustering 在 sibling 模块
//! `abstraction::feature` / `abstraction::cluster` / `abstraction::equity` 完成，
//! 输出量化为 `u8` 写入 mmap bucket table 后由本模块以整数 key lookup。
//!
//! D-252 锁死的实现手段：本模块文件顶 `#![deny(clippy::float_arithmetic)]` inner
//! attribute，让 clippy 在本模块内任何 `f32` / `f64` 算术触发硬错（即使 `cargo
//! clippy` 无 `-D warnings`）。该约束保护 stage 4+ CFR / 实时搜索 driver 在
//! `map` 路径上的 byte-equal 跨架构稳定性（继承 stage 1 D-026 整数边界精神到
//! stage 2 抽象层）。
//!
//! D-254 内部子模块隔离：本模块不在 `lib.rs` 顶层 re-export，仅通过
//! `PreflopLossless169::map` / `PostflopBucketAbstraction::map` 间接对外。
//!
//! A1 阶段为空骨架，B2 \[实现\] 填充整数 key 派发逻辑（InfoSetId bit pack /
//! unpack helpers / preflop 169 closed-form 公式 / postflop canonical observation
//! id → bucket id 整数 lookup）。

#![deny(clippy::float_arithmetic)]
