//! 特征提取（EHS² / OCHS / histogram）。
//!
//! 模块私有 surface（D-254 内部子模块隔离，不在 `lib.rs` 顶层 re-export）。
//! 允许使用浮点（D-273 浮点边界扩展，B2/C2 在该模块计算特征向量后量化为
//! `u8` 写入 mmap bucket table，与 `abstraction::map` 物理隔离）。
//!
//! A1 阶段为空骨架，B2/C2 \[实现\] 填充 EHS² / OCHS / 直方图特征提取实现。
