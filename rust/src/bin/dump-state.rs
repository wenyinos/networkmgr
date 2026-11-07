//! 阶段 1 验收用诊断命令：输出与 Python `net_api.tray_state()` 等价的单行 JSON。
//!
//! 只在目标 FreeBSD 上有意义。验收方法是在同一台机器、同一网络状态下分别运行
//! Python 版与 `dump-state`，逐字段比对（第 22 节记录了一次实机比对结果）。
//!
//! 命令执行交给 `sys::RealRunner`：超时、错误语义、`wpa_cli` 参数都在那一层，
//! 这里只负责打印。

use networkmgr_core::sys::{RealRunner, Sys};

fn main() {
    let runner = RealRunner::new();
    let state = Sys::new(&runner).tray_state();
    println!("{}", state.to_json());
}
