//! NLHE 规则与状态机（API §2 / §3 / §4）。
//!
//! - [`action`]：动作枚举与合法动作集合
//! - [`config`]：桌面配置（座位数、盲注、起始栈、按钮位）
//! - [`state`]：游戏状态机入口

pub mod action;
pub mod config;
pub mod state;
