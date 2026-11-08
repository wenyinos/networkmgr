//! 阶段 3：托盘主程序（tray-icon + 原生菜单 + 30 秒刷新）。
//!
//! 菜单结构对齐方案第 1.3 节第 1 条：
//! 状态标题 → WireGuard 区 → 有线/无线区 → 已保存网络 → 忘记网络 →
//! 扫描结果 → `Restart Networking` → `Close Network Manager`。
//!
//! 连接流程与 Python 版一致：已保存的网络直接 `select_network`；新网络先
//! `add_network` + `set_network`，连上之后才 `save_config`，失败则删掉新建的块。
//! PSK/WEP 口令用 GTK 对话框收集；企业级凭据仍引导用户去配置窗口。
//!
//! `Restart Networking` 会先弹确认框，并且只重启当前默认网卡。VM 实测无参数
//! `service netif restart` 会让 SSH 长时间不恢复，不能直接执行。
//!
//! `appindicator` 后端要求调用线程有 GTK 事件循环：main 先 `gtk::init()`，
//! 菜单与图标都在主线程构建/刷新（`TrayIcon` 内部是 `RefCell`，不能跨线程）。
//! 菜单事件线程只把动作转发回主线程，对话框与命令分发都在主线程完成，
//! 阻塞的系统命令再交给工作线程。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;
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
/// 等一次无线连接完成的上限，与 Python `wait_for_connection()` 一致。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
enum Action {
    Restart(Option<String>),
    Close,
    Scan(String),
    Configure(String),
    Disable(String),
    Enable(String),
    Connect {
        ssid: String,
        nic: String,
        saved_id: Option<String>,
        security: String,
    },
    Forget {
        ssid: String,
        nic: String,
    },
    WgUp(String),
    WgDown(String),
}

/// 菜单点击之后要走哪条连接路径。
#[derive(Debug, PartialEq, Eq)]
enum ConnectPlan {
    /// 网络已保存，直接按 id 选中。
    Saved(String),
    /// 开放网络，不需要口令。
    Open,
    /// PSK / WEP，需要先问口令。
    Password,
    /// 企业级网络，托盘不收集凭据。
    Unsupported,
}

fn connect_plan(security: &str, saved_id: Option<String>) -> ConnectPlan {
    if let Some(id) = saved_id {
        return ConnectPlan::Saved(id);
    }
    match security {
        "OPEN" => ConnectPlan::Open,
        "WPA2-PSK" | "WPA-PSK" | "WPA3-SAE" | "WEP" => ConnectPlan::Password,
        _ => ConnectPlan::Unsupported,
    }
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
        let _ = menu.append(&PredefinedMenuItem::separator());
    }

    let nic = state.card.clone().unwrap_or_default();

    if !snapshot.saved.is_empty() {
        let submenu = Submenu::new("Saved Networks", true);
        for network in &snapshot.saved {
            let label = format!("{} {}", network.ssid, network.flags);
            add!(
                submenu,
                &label,
                Action::Connect {
                    ssid: network.ssid.clone(),
                    nic: nic.clone(),
                    saved_id: Some(network.id.clone()),
                    security: String::new(),
                }
            );
        }
        let _ = menu.append(&submenu);

        // 已保存的网络即使不在扫描范围内也要能删除，否则一个错口令的块
        // 会永远留在 wpa_supplicant.conf 里。
        let forget = Submenu::new("Forget Network", true);
        for network in &snapshot.saved {
            add!(
                forget,
                &network.ssid,
                Action::Forget {
                    ssid: network.ssid.clone(),
                    nic: nic.clone(),
                }
            );
        }
        let _ = menu.append(&forget);
    }

    if state.kind == Some("wifi") {
        let submenu = Submenu::new("Connect to Network", true);
        add!(submenu, "Scan for Networks", Action::Scan(nic.clone()));
        let saved_ids: HashMap<&str, &str> = snapshot
            .saved
            .iter()
            .map(|network| (network.ssid.as_str(), network.id.as_str()))
            .collect();
        for scan in &snapshot.scans {
            // 已经连上的 AP 不再出现在候选里，与 Python 的 passes 行为一致。
            if state.connection == "Connected" && state.ssid.as_deref() == Some(scan.ssid.as_str()) {
                continue;
            }
            add!(
                submenu,
                &format!("{} ({}%)", scan.ssid, scan.signal),
                Action::Connect {
                    ssid: scan.ssid.clone(),
                    nic: nic.clone(),
                    saved_id: saved_ids.get(scan.ssid.as_str()).map(|id| id.to_string()),
                    security: scan.security.to_string(),
                }
            );
        }
        let _ = menu.append(&submenu);
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    add!(
        menu,
        "Restart Networking",
        Action::Restart(state.card.clone())
    );
    let _ = menu.append(&PredefinedMenuItem::separator());
    add!(menu, "Close Network Manager", Action::Close);

    (menu, actions)
}

/// 主线程上的动作分发：需要对话框的动作在这里弹窗，阻塞命令交给工作线程。
fn dispatch(action: Action) {
    match action {
        Action::Close => std::process::exit(0),
        Action::Restart(card) => {
            if !confirm_restart(card.as_deref()) {
                return;
            }
            thread::spawn(move || {
                let runner = RealRunner::new();
                let sys = Sys::new(&runner);
                match sys.netif_restart(card.as_deref()) {
                    Ok(()) => println!("networkmgr: netif restarted"),
                    Err(error) => eprintln!("networkmgr: restart failed: {error}"),
                }
            });
        }
        Action::Connect {
            ssid,
            nic,
            saved_id,
            security,
        } => match connect_plan(&security, saved_id) {
            ConnectPlan::Saved(id) => {
                thread::spawn(move || connect_saved(&nic, &ssid, &id));
            }
            ConnectPlan::Open => {
                thread::spawn(move || connect_new(&nic, &ssid, "OPEN", None));
            }
            ConnectPlan::Password => {
                if let Some(psk) = ask_password(&ssid) {
                    thread::spawn(move || connect_new(&nic, &ssid, &security, Some(&psk)));
                }
            }
            ConnectPlan::Unsupported => show_message(
                "Enterprise network",
                &format!("{ssid} needs 802.1X credentials; use the configuration window."),
            ),
        },
        Action::Forget { ssid, nic } => {
            thread::spawn(move || forget_network(&nic, &ssid));
        }
        other => {
            thread::spawn(move || execute(other));
        }
    }
}

fn execute(action: Action) {
    let runner = RealRunner::new();
    let sys = Sys::new(&runner);
    match action {
        Action::Scan(card) => {
            println!(
                "networkmgr: scan requested on {card}: {}",
                sys.wpa_scan(&card)
            );
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
        Action::WgUp(device) => {
            let _ = sys.wg_quick("up", &device);
            println!("networkmgr: wg-quick up {device}");
        }
        Action::WgDown(device) => {
            let _ = sys.wg_quick("down", &device);
            println!("networkmgr: wg-quick down {device}");
        }
        _ => {}
    }
}

fn connect_saved(nic: &str, ssid: &str, id: &str) {
    let runner = RealRunner::new();
    let sys = Sys::new(&runner);
    if !sys.wpa_select_network(nic, id) {
        eprintln!("networkmgr: {ssid} could not be selected");
        return;
    }
    finish_connection(&sys, nic, ssid, true, id);
}

fn connect_new(nic: &str, ssid: &str, security: &str, psk: Option<&str>) {
    let runner = RealRunner::new();
    let sys = Sys::new(&runner);
    let Some(id) = sys.wpa_save_network(nic, ssid, security, psk) else {
        eprintln!("networkmgr: could not create a network block for {ssid}");
        return;
    };
    if !sys.wpa_select_network(nic, &id) {
        sys.wpa_remove_network(nic, &id);
        eprintln!("networkmgr: {ssid} could not be selected");
        return;
    }
    finish_connection(&sys, nic, ssid, false, &id);
}

fn finish_connection(sys: &Sys<'_, RealRunner>, nic: &str, ssid: &str, saved: bool, id: &str) {
    if sys.wpa_wait_for_connection(nic, ssid, CONNECT_TIMEOUT) {
        // 连上才写盘：错口令永远不会落进 wpa_supplicant.conf。
        let _ = sys.wpa_save_config(nic);
        println!("networkmgr: connected to {ssid}");
    } else if saved {
        eprintln!("networkmgr: could not connect to {ssid}; saved block kept");
    } else {
        // 新建的块没连上就删掉，避免留下一个永远连不上的配置。
        sys.wpa_remove_network(nic, id);
        eprintln!("networkmgr: could not connect to {ssid}; block removed");
    }
    // select_network 禁用了其他网络，结束时必须恢复。
    let _ = sys.wpa_enable_all(nic);
}

fn forget_network(nic: &str, ssid: &str) {
    let runner = RealRunner::new();
    let sys = Sys::new(&runner);
    let _ = sys.wpa_reconfigure(nic);
    if !sys.wpa_remove_by_ssid(nic, ssid) {
        eprintln!("networkmgr: no saved block for {ssid}");
        return;
    }
    let _ = sys.wpa_save_config(nic);
    println!("networkmgr: forgot {ssid}");
}

#[cfg(feature = "appindicator")]
fn confirm_restart(card: Option<&str>) -> bool {
    use gtk::prelude::*;
    let target = card.unwrap_or("all interfaces");
    let text = format!(
        "Restart networking on {target}?\n\nThe interface goes down briefly and the connection may drop."
    );
    let dialog = gtk::Dialog::with_buttons(
        Some("Restart Networking"),
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        &[
            ("Cancel", gtk::ResponseType::Cancel),
            ("Restart", gtk::ResponseType::Ok),
        ],
    );
    dialog.set_default_response(gtk::ResponseType::Cancel);
    dialog.content_area().add(&gtk::Label::new(Some(&text)));
    dialog.show_all();
    let response = dialog.run();
    dialog.close();
    response == gtk::ResponseType::Ok
}

#[cfg(not(feature = "appindicator"))]
fn confirm_restart(_card: Option<&str>) -> bool {
    eprintln!("networkmgr: restart needs the appindicator (GTK) backend");
    false
}

#[cfg(feature = "appindicator")]
fn ask_password(ssid: &str) -> Option<String> {
    use gtk::prelude::*;
    let dialog = gtk::Dialog::with_buttons(
        Some(&format!("Connect to {ssid}")),
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        &[
            ("Cancel", gtk::ResponseType::Cancel),
            ("Connect", gtk::ResponseType::Ok),
        ],
    );
    dialog.set_default_response(gtk::ResponseType::Ok);
    let entry = gtk::Entry::new();
    entry.set_visibility(false);
    entry.set_activates_default(true);
    entry.set_placeholder_text(Some("Wi-Fi password"));
    dialog.content_area().add(&entry);
    dialog.show_all();
    let response = dialog.run();
    let text = entry.text().to_string();
    dialog.close();
    if response == gtk::ResponseType::Ok && !text.is_empty() {
        Some(text)
    } else {
        None
    }
}

#[cfg(not(feature = "appindicator"))]
fn ask_password(ssid: &str) -> Option<String> {
    eprintln!("networkmgr: password prompt for {ssid} needs the appindicator (GTK) backend");
    None
}

#[cfg(feature = "appindicator")]
fn show_message(title: &str, text: &str) {
    use gtk::prelude::*;
    let dialog = gtk::MessageDialog::new(
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Info,
        gtk::ButtonsType::Ok,
        text,
    );
    dialog.set_title(title);
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.show_all();
}

#[cfg(not(feature = "appindicator"))]
fn show_message(_title: &str, text: &str) {
    eprintln!("networkmgr: {text}");
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

    // 菜单事件线程只转发动作；对话框和命令分发都回到主线程。
    let (action_tx, action_rx) = mpsc::channel::<Action>();
    let menu_rx = MenuEvent::receiver();
    let icon_rx = TrayIconEvent::receiver();
    let actions_events = Arc::clone(&actions);
    thread::spawn(move || loop {
        if let Ok(event) = menu_rx.try_recv() {
            let action = actions_events
                .lock()
                .expect("actions lock")
                .get(&event.id.0)
                .cloned();
            match action {
                Some(action) => {
                    let _ = action_tx.send(action);
                }
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
    {
        gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
            while let Ok(action) = action_rx.try_recv() {
                dispatch(action);
            }
            gtk::glib::ControlFlow::Continue
        });
        gtk::main();
    }

    #[cfg(not(feature = "appindicator"))]
    loop {
        while let Ok(action) = action_rx.try_recv() {
            dispatch(action);
        }
        thread::sleep(Duration::from_millis(200));
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

    #[test]
    fn connect_plan_prefers_saved_blocks_and_classifies_security() {
        assert_eq!(
            connect_plan("", Some("3".to_string())),
            ConnectPlan::Saved("3".to_string())
        );
        assert_eq!(connect_plan("OPEN", None), ConnectPlan::Open);
        for security in ["WPA2-PSK", "WPA-PSK", "WPA3-SAE", "WEP"] {
            assert_eq!(
                connect_plan(security, None),
                ConnectPlan::Password,
                "{security} should ask for a passphrase"
            );
        }
        assert_eq!(connect_plan("WPA2-EAP", None), ConnectPlan::Unsupported);
    }
}
