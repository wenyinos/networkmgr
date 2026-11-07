//! 阶段 1 验收用诊断命令：输出与 Python `net_api.tray_state()` 等价的单行 JSON。
//!
//! 只在目标 FreeBSD 上有意义。验收方法是在同一台机器、同一网络状态下分别运行
//! `python3 -c 'from NetworkMgr.net_api import tray_state; ...'` 与
//! `networkmgr-dump-state`，逐字段比对。
//!
//! 命令一律用参数列表、不经过 shell；任何一步失败都退化成空输入，让输出仍是
//! 合法 JSON，而不是让诊断命令自己崩掉。

use std::process::Command;

use networkmgr_core::ifconfig;
use networkmgr_core::state::{self, Snapshot};
use networkmgr_core::wpa;

/// 跑一条命令并取 stdout；不存在或失败时返回空串。
fn run(program: &str, args: &[&str]) -> String {
    match Command::new(program).args(args).output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(error) => {
            eprintln!("dump-state: {program} failed: {error}");
            String::new()
        }
    }
}

fn main() {
    let netstat = run("netstat", &["-rn"]);
    let Some(card) = ifconfig::default_route_interface(&netstat).map(str::to_string) else {
        println!("{}", state::TrayState::no_card().to_json());
        return;
    };

    let ifconfig_text = run("ifconfig", &[&card]);
    let wireless = card.contains("wlan");
    let status_text = if wireless {
        run("wpa_cli", &["-i", &card, "status"])
    } else {
        String::new()
    };
    let bss_text = if wireless {
        run("wpa_cli", &["-i", &card, "bss", "current"])
    } else {
        String::new()
    };

    let status = wpa::wpa_status(&status_text);
    let (level, noise) = state::bss_signal(&bss_text);
    let snapshot = Snapshot {
        card: Some(&card),
        ifconfig_text: &ifconfig_text,
        wpa_state: status.get("wpa_state").map(String::as_str),
        ssid: status.get("ssid").map(String::as_str),
        bssid: status.get("bssid").map(String::as_str).unwrap_or(""),
        level,
        noise,
    };
    println!("{}", state::build(&snapshot).to_json());
}
