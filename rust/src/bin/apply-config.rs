//! 诊断命令：把一份接口配置真正写进 `rc.conf` / `resolv.conf`。
//!
//! 默认 **dry-run**，只打印将执行的命令；`--apply` 才动手。用于在 B 类环境验证
//! 写盘链路，不依赖 GUI 点击，也不要求先接好桌面。
//!
//! 用法：
//!   apply-config <nic> --dhcp [--apply]
//!   apply-config <nic> --static <ip> <netmask> \
//!       [--gateway <gw>] [--dns <dns1,dns2>] [--search <domain>] [--apply]
//!   apply-config --fix-wpa-ctrl
//!
//! `--fix-wpa-ctrl` 给 `/etc/wpa_supplicant.conf` 补上 `ctrl_interface`，
//! 修复老机器上 `wpa_cli` 全链路不可用的问题；它不重启服务，避免静默断网。
//!
//! 验证时请挑一个不影响 SSH 的接口（例如 `ifconfig epair0 create` 出来的
//! `epair0a`），不要拿承载 SSH 的那块网卡试。

use std::process::ExitCode;

use networkmgr_core::sys::{RealRunner, Sys};
use networkmgr_core::{config, validate};

struct Options {
    nic: String,
    dhcp: bool,
    interface_address: Option<(String, String)>,
    gateway: Option<String>,
    dns: Vec<String>,
    search: Option<String>,
    apply: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut nic: Option<String> = None;
    let mut dhcp = false;
    let mut interface_address = None;
    let mut gateway = None;
    let mut dns = Vec::new();
    let mut search = None;
    let mut apply = false;

    let mut index = 0;
    while index < args.len() {
        let next = |offset: usize| -> Result<&String, String> {
            args.get(index + offset)
                .ok_or_else(|| format!("{} 缺少参数", args[index]))
        };
        match args[index].as_str() {
            "--dhcp" => dhcp = true,
            "--static" => {
                interface_address = Some((next(1)?.clone(), next(2)?.clone()));
                index += 2;
            }
            "--gateway" => {
                gateway = Some(next(1)?.clone());
                index += 1;
            }
            "--dns" => {
                dns = next(1)?.split(',').map(str::to_string).collect();
                index += 1;
            }
            "--search" => {
                search = Some(next(1)?.clone());
                index += 1;
            }
            "--apply" => apply = true,
            flag if flag.starts_with("--") => return Err(format!("未知参数 {flag}")),
            value if nic.is_none() => nic = Some(value.to_string()),
            value => return Err(format!("多余的参数 {value}")),
        }
        index += 1;
    }

    Ok(Options {
        nic: nic.ok_or("缺少接口名")?,
        dhcp,
        interface_address,
        gateway,
        dns,
        search,
        apply,
    })
}

/// 给 `/etc/wpa_supplicant.conf` 补 `ctrl_interface`；不重启服务。
///
/// 重启 wpa_supplicant 会短暂断网，所以只打印提示，交给用户决定时机。
fn fix_wpa_ctrl() -> ExitCode {
    match config::fix_ctrl_interface_file("/etc/wpa_supplicant.conf") {
        Ok(message) => {
            println!("{message}");
            println!("请手动重启 wpa_supplicant 使其生效（会短暂断网）：");
            println!("  service wpa_supplicant restart");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("apply-config: {error}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--fix-wpa-ctrl") {
        return fix_wpa_ctrl();
    }
    let options = match parse(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("apply-config: {error}");
            return ExitCode::from(2);
        }
    };

    let nic = match validate::interface(&options.nic) {
        Ok(nic) => nic,
        Err(error) => {
            eprintln!("apply-config: {error}");
            return ExitCode::FAILURE;
        }
    };
    let rc_nic = validate::rc_conf_interface(&nic);

    let mut commands: Vec<(String, String)> = Vec::new();
    let mut resolv = None;

    if options.dhcp {
        commands.push((
            format!("ifconfig_{rc_nic}"),
            config::ifconfig_value(&nic, config::AddressMethod::Dhcp),
        ));
    } else if let Some((ip, netmask)) = &options.interface_address {
        let ip = match validate::ipv4_address(ip, "IP") {
            Ok(value) => value,
            Err(error) => {
                eprintln!("apply-config: {error}");
                return ExitCode::FAILURE;
            }
        };
        let netmask = match validate::ipv4_netmask(netmask) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("apply-config: {error}");
                return ExitCode::FAILURE;
            }
        };
        commands.push((
            format!("ifconfig_{rc_nic}"),
            config::ifconfig_value(
                &nic,
                config::AddressMethod::Static {
                    inet: &ip,
                    netmask: &netmask,
                },
            ),
        ));
        if let Some(gateway) = &options.gateway {
            match validate::ipv4_address(gateway, "Gateway") {
                Ok(value) => commands.push(("defaultrouter".to_string(), value)),
                Err(error) => {
                    eprintln!("apply-config: {error}");
                    return ExitCode::FAILURE;
                }
            }
        }
        if !options.dns.is_empty() {
            let mut nameservers = Vec::new();
            for dns in &options.dns {
                match validate::ipv4_address(dns, "DNS") {
                    Ok(value) => nameservers.push(value),
                    Err(error) => {
                        eprintln!("apply-config: {error}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            let search = match &options.search {
                Some(search) => match validate::search_domains(search) {
                    Ok(value) => Some(value),
                    Err(error) => {
                        eprintln!("apply-config: {error}");
                        return ExitCode::FAILURE;
                    }
                },
                None => None,
            };
            let refs: Vec<&str> = nameservers.iter().map(String::as_str).collect();
            resolv = Some(config::render_resolv_conf(search.as_deref(), &refs));
        }
    } else {
        eprintln!("apply-config: 需要 --dhcp 或 --static <ip> <netmask>");
        return ExitCode::from(2);
    }

    for (name, value) in &commands {
        println!("sysrc {name}={value}");
    }
    if let Some(content) = &resolv {
        println!("--- /etc/resolv.conf ---");
        print!("{content}");
    }

    if !options.apply {
        println!("dry run: 加 --apply 才会真正写入");
        return ExitCode::SUCCESS;
    }

    let runner = RealRunner::new();
    let sys = Sys::new(&runner);
    for (name, value) in &commands {
        if let Err(error) = sys.sysrc_set(name, value) {
            eprintln!("apply-config: 写 {name} 失败: {error}");
            return ExitCode::FAILURE;
        }
    }
    if let Some(content) = &resolv {
        if let Err(error) = std::fs::write("/etc/resolv.conf", content) {
            eprintln!("apply-config: 写 /etc/resolv.conf 失败: {error}");
            return ExitCode::FAILURE;
        }
    }
    println!("applied {} rc.conf entries", commands.len());
    ExitCode::SUCCESS
}
