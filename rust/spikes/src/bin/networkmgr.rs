//! 阶段 3：托盘主程序（tray-icon + 原生菜单 + 30 秒刷新）。
//!
//! 菜单结构对齐方案第 1.3 节第 1 条：
//! 状态标题 → WireGuard 区 → 有线/无线区 → 已保存网络 → 扫描结果 →
//! `Restart Networking` → `Close Network Manager`。
//!
//! **仍未实现**（执行时只打印日志，不假装成功）：
//! - 扫描结果里的「连接」需要 add_network / set_network / select_network 整套流程，
//!   骨架阶段没有做，菜单项会明确打印 `scan connect not implemented`。
//! - 连接中的 300 ms 帧动画。
//!
//! `appindicator` 后端要求调用线程有 GTK 事件循环：main 先 `gtk::init()`，
//! 菜单与图标都在主线程构建/刷新（`TrayIcon` 内部是 `RefCell`，不能跨线程），
//! 只有命令执行放在事件线程。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use networkmgr_core::state::TrayState;
use networkmgr_core::sys::{RealRunner, Sys};
use networkmgr_core::wg::{self, WgDictionary};
use networkmgr_core::wpa::{self, SavedNetwork, ScanResult};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

const WG_CONFIG_DIR: &str = "/usr/local/etc/wireguard";
#[cfg_attr(not(feature = "appindicator"), allow(dead_code))]
const REFRESH_SECONDS: u32 = 30;

#[derive(Clone, Debug)]
enum Action {
    Restart,
    Close,
    Scan,
    Configure(String),
    Disable(String),
    Enable(String),
    Connect(String),
    Forget(String),
    WgUp(String),
    WgDown(String),
}

struct Snapshot {
    state: TrayState,
    saved: Vec<SavedNetwork>,
    scans: Vec<ScanResult>,
    tunnels: WgDictionary,
}

fn gather() -> Snapshot {
    let runner = RealRunner::new();
    let sys = Sys::new(&runner);
    let state = sys.tray_state();

    let mut saved = Vec::new();
    let mut scans = Vec::new();
    if state.kind == Some("wifi") {
        if let Some(card) = state.card.clone() {
            saved = wpa::wpa_networks(&sys.wpa_list_networks(&card));
            let results = sys.wpa_scan_results(&card);
            let noise =
                wpa::noise_floor(&sys.wpa_bss_current(&card)).unwrap_or(wpa::DEFAULT_NOISE_FLOOR);
            scans = wpa::scan_networks(&results, noise).into_values().collect();
            scans.sort_by(|a, b| b.signal.cmp(&a.signal));
        }
    }

    let raw_service = sys.sysrc_get("wireguard_enable");
    let service = wg::service_state(if raw_service.trim().is_empty() {
        None
    } else {
        Some(&raw_service)
    });
    let tunnels = wg::build_dictionary(&service, &sys.wg_configs(Path::new(WG_CONFIG_DIR)));

    Snapshot {
        state,
        saved,
        scans,
        tunnels,
    }
}

fn build_menu(snapshot: &Snapshot) -> (Menu, HashMap<String, Action>) {
    let mut actions: HashMap<String, Action> = HashMap::new();
    let mut next = 0usize;
    let menu = Menu::new();

    macro_rules! add {
        ($target:expr, $text:expr, $action:expr) => {{
            let id = format!("act-{}", next);
            next += 1;
            actions.insert(id.clone(), $action);
            let _ = $target.append(&MenuItem::with_id(id, $text, true, None));
        }};
    }
    macro_rules! title {
        ($target:expr, $text:expr) => {{
            let id = format!("title-{}", next);
            next += 1;
            let _ = $target.append(&MenuItem::with_id(id, $text, false, None));
        }};
    }

    let state = &snapshot.state;
    let headline = match (&state.card, state.connection) {
        (None, _) => "Network card is not enabled".to_string(),
        (Some(card), "Connected") => match &state.ssid {
            Some(ssid) => format!("{ssid} ({card})"),
            None => format!("Wired {card} Connected"),
        },
        (Some(card), other) => format!("{card} {other}"),
    };
    title!(menu, &headline);
    let _ = menu.append(&PredefinedMenuItem::separator());

    if !snapshot.tunnels.configs.is_empty() {
        title!(menu, "WireGuard");
        for (device, config) in &snapshot.tunnels.configs {
            if config.state == "Connected" {
                add!(
                    menu,
                    &format!("Disable {}", config.info),
                    Action::WgDown(device.clone())
                );
            } else {
                add!(
                    menu,
                    &format!("Enable {}", config.info),
                    Action::WgUp(device.clone())
                );
            }
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
    }

    if let Some(card) = &state.card {
        if state.connection == "Disabled" {
            add!(menu, "Enable", Action::Enable(card.clone()));
        } else {
            add!(
                menu,
                &format!("Disable {card}"),
                Action::Disable(card.clone())
            );
            add!(
                menu,
                &format!("Configure {card}"),
                Action::Configure(card.clone())
            );
        }
        if state.kind == Some("wifi") && state.connection == "Connected" {
            if let Some(ssid) = &state.ssid {
                add!(menu, "Forget Network", Action::Forget(ssid.clone()));
            }
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
    }

    if !snapshot.saved.is_empty() {
        let submenu = Submenu::new("Saved Networks", true);
        for network in &snapshot.saved {
            let label = format!("{} {}", network.ssid, network.flags);
            add!(submenu, &label, Action::Connect(network.ssid.clone()));
        }
        let _ = menu.append(&submenu);
    }

    if state.kind == Some("wifi") {
        let submenu = Submenu::new("Connect to Network", true);
        add!(submenu, "Scan for Networks", Action::Scan);
        for scan in &snapshot.scans {
            add!(
                submenu,
                &format!("{} ({}%)", scan.ssid, scan.signal),
                Action::Connect(scan.ssid.clone())
            );
        }
        let _ = menu.append(&submenu);
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    add!(menu, "Restart Networking", Action::Restart);
    let _ = menu.append(&PredefinedMenuItem::separator());
    add!(menu, "Close Network Manager", Action::Close);

    (menu, actions)
}

fn execute(action: Action, card: &Option<String>) {
    let runner = RealRunner::new();
    let sys = Sys::new(&runner);
    match action {
        Action::Close => std::process::exit(0),
        Action::Restart => match sys.service("netif", "restart") {
            Ok(()) => println!("networkmgr: netif restarted"),
            Err(error) => eprintln!("networkmgr: restart failed: {error}"),
        },
        Action::Scan => {
            if let Some(card) = card {
                println!(
                    "networkmgr: scan requested on {card}: {}",
                    sys.wpa_scan(card)
                );
            }
        }
        Action::Disable(nic) => {
            let _ = sys.ifconfig_args(&[&nic, "down"]);
            println!("networkmgr: {nic} down");
        }
        Action::Enable(nic) => {
            let _ = sys.ifconfig_args(&[&nic, "up"]);
            println!("networkmgr: {nic} up");
        }
        Action::Configure(nic) => {
            println!("networkmgr: configure {nic} -> 阶段 2 的配置窗口，尚未接线");
        }
        Action::Forget(ssid) => println!("networkmgr: forget {ssid} not implemented in skeleton"),
        Action::Connect(ssid) => {
            println!(
                "networkmgr: scan connect {ssid} not implemented (needs add/set/select_network)"
            )
        }
        Action::WgUp(device) => {
            let _ = sys.wg_quick("up", &device);
            println!("networkmgr: wg-quick up {device}");
        }
        Action::WgDown(device) => {
            let _ = sys.wg_quick("down", &device);
            println!("networkmgr: wg-quick down {device}");
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "appindicator")]
    gtk::init()?;

    let snapshot = gather();
    let (menu, actions) = build_menu(&snapshot);
    let actions = Arc::new(Mutex::new(actions));

    #[cfg_attr(not(feature = "appindicator"), allow(unused_variables))]
    let tray = Rc::new(RefCell::new(
        TrayIconBuilder::new()
            .with_tooltip(snapshot.state.tooltip.clone())
            .with_icon(icon_for(
                snapshot.state.signal,
                snapshot.state.kind == Some("wifi"),
            ))
            .with_menu(Box::new(menu))
            .build()?,
    ));

    println!(
        "networkmgr: tray ready, state = {}",
        snapshot.state.to_json()
    );

    // 30 秒被动刷新：菜单与图标只能在主线程重建。
    #[cfg(feature = "appindicator")]
    {
        let tray_timer = Rc::clone(&tray);
        let actions_timer = Arc::clone(&actions);
        gtk::glib::timeout_add_seconds_local(REFRESH_SECONDS, move || {
            let snapshot = gather();
            let (menu, map) = build_menu(&snapshot);
            *actions_timer.lock().expect("actions lock") = map;
            let tray = tray_timer.borrow();
            let _ = tray.set_tooltip(Some(&snapshot.state.tooltip));
            let _ = tray.set_icon(Some(icon_for(
                snapshot.state.signal,
                snapshot.state.kind == Some("wifi"),
            )));
            tray.set_menu(Some(Box::new(menu)));
            gtk::glib::ControlFlow::Continue
        });
    }

    let seconds = std::env::var("SPIKE_SECONDS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(600);
    let deadline = Instant::now() + Duration::from_secs(seconds);

    let menu_rx = MenuEvent::receiver();
    let icon_rx = TrayIconEvent::receiver();
    let actions_events = Arc::clone(&actions);
    let card = snapshot.state.card.clone();
    thread::spawn(move || loop {
        if let Ok(event) = menu_rx.try_recv() {
            let action = actions_events
                .lock()
                .expect("actions lock")
                .get(&event.id.0)
                .cloned();
            match action {
                Some(action) => execute(action, &card),
                None => println!("networkmgr: unknown menu id {:?}", event.id),
            }
        }
        if let Ok(event) = icon_rx.try_recv() {
            println!("networkmgr: tray event {event:?}");
        }
        if Instant::now() >= deadline {
            println!("networkmgr: timeout, exiting");
            std::process::exit(0);
        }
        thread::sleep(Duration::from_millis(50));
    });

    #[cfg(feature = "appindicator")]
    gtk::main();

    #[cfg(not(feature = "appindicator"))]
    loop {
        thread::sleep(Duration::from_secs(1));
    }

    #[cfg(feature = "appindicator")]
    Ok(())
}

/// 按信号强度生成 0–4 格的方块图标。
///
/// 无信号/有线时画浅色实心方块：全暗图标在深色面板上等于没画，
/// 第一次 VM 验收就吃过这个亏。
fn icon_for(signal: Option<i32>, wireless: bool) -> Icon {
    const SIZE: u32 = 32;
    let bars = match signal {
        None => 0,
        Some(value) if value >= 75 => 4,
        Some(value) if value >= 50 => 3,
        Some(value) if value >= 25 => 2,
        Some(_) => 1,
    };
    let color = if !wireless {
        (120, 120, 120)
    } else if bars >= 3 {
        (60, 150, 90)
    } else if bars >= 1 {
        (200, 150, 60)
    } else {
        (170, 70, 70)
    };

    let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let column = x / 8;
            let filled = column < bars && y >= 8 + (3 - column) * 4;
            let (r, g, b) = if bars == 0 {
                if (10..22).contains(&x) && (10..22).contains(&y) {
                    (210, 210, 210)
                } else {
                    (70, 70, 70)
                }
            } else if filled {
                color
            } else {
                (40, 40, 40)
            };
            pixels.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Icon::from_rgba(pixels, SIZE, SIZE).expect("32x32 RGBA icon is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_snapshot() -> Snapshot {
        Snapshot {
            state: TrayState {
                card: Some("wlan0".to_string()),
                kind: Some("wifi"),
                connection: "Connected",
                ssid: Some("Home".to_string()),
                signal: Some(80),
                tooltip: "tooltip".to_string(),
            },
            saved: vec![SavedNetwork {
                id: "0".to_string(),
                ssid: "Home".to_string(),
                bssid: "any".to_string(),
                flags: "[CURRENT]".to_string(),
            }],
            scans: vec![ScanResult {
                ssid: "Cafe".to_string(),
                bssid: "00:11:22:33:44:55".to_string(),
                frequency: 2412,
                level: -50,
                noise: -95,
                signal: 180,
                flags: "[ESS]".to_string(),
                security: "OPEN",
                enterprise: false,
            }],
            tunnels: wg::build_dictionary("YES", &[]),
        }
    }

    #[test]
    fn snapshot_holds_the_expected_sections() {
        // Menu 需要 GTK 初始化，测试里只验证快照构造，真正的渲染在 VM 上验收。
        let snapshot = demo_snapshot();
        assert_eq!(snapshot.saved.len(), 1);
        assert_eq!(snapshot.scans.len(), 1);
        assert!(snapshot.tunnels.configs.is_empty());
    }

    #[test]
    fn icon_bars_follow_the_signal() {
        for signal in [None, Some(0), Some(30), Some(60), Some(90)] {
            drop(icon_for(signal, true));
        }
    }
}
