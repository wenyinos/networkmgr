//! NetworkMgr 重写后的纯逻辑层。
//!
//! 本 crate 不依赖任何 GUI，也不执行系统命令：所有系统交互后续由 `sys.rs`
//! 通过 trait 注入。因此它可以直接在 Linux 宿主上用 mock 样本跑契约测试，
//! 不需要 FreeBSD 交叉编译环境。

pub mod config;
pub mod i18n;
pub mod ifconfig;
pub mod state;
pub mod validate;
pub mod wg;
pub mod wpa;
