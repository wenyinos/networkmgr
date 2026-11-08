//! devd 事件的 Rust 入口：`networkmgr-devd <attach|link-up|link-down> <iface>`。
//!
//! 由 `/usr/local/etc/devd/networkmgr.conf` 以 root 身份调用。行为与原来的
//! `setup-nic.py` / `link-up.py` / `auto-switch.py` 一一对应，逻辑都在
//! `networkmgr_core::devd` 里，本文件只负责参数解析和退出码。

use std::process::ExitCode;

use networkmgr_core::devd::{self, RealEnv};
use networkmgr_core::sys::RealRunner;

fn usage() {
    eprintln!("usage: networkmgr-devd <attach|link-up|link-down> <interface>");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        usage();
        return ExitCode::FAILURE;
    }

    let runner = RealRunner::new();
    let env = RealEnv::new(&runner);
    match args[1].as_str() {
        "attach" => devd::attach(&env, &args[2]),
        "link-up" => devd::link_up(&env, &args[2]),
        "link-down" => devd::link_down(&env, &args[2]),
        other => {
            eprintln!("networkmgr-devd: unknown event {other}");
            usage();
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
