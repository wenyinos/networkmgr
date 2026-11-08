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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use networkmgr_core::config;
use networkmgr_core::state::TrayState;
use networkmgr_core::sys::{RealRunner, Sys};
use networkmgr_core::wg::{self, WgDictionary};
use networkmgr_core::wpa::{self, SavedNetwork, ScanResult};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

const WG_CONFIG_DIR: &str = "/usr/local/etc/wireguard";
#[cfg_attr(not(feature = "appindicator"), allow(dead_code))]
const REFRESH_SECONDS: u32 = 30;
/// 等一次无线连接完成的上限，与 Python `wait_for_connection()` 一致。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// 32×32 应用图标 RGBA，由 `src/icons/scalable/apps/networkmgr.svg` 生成。
const APP_ICON_RGBA: &[u8] = include_bytes!("../../assets/networkmgr-32.rgba");
/// 动画帧间隔，与 Python 的 `GLib.timeout_add(300, ...)` 一致。
const ANIMATION_MILLIS: u64 = 300;
/// 连接失败后的总尝试次数，与 Python 的 `MAX_ATTEMPTS` 一致。
const MAX_ATTEMPTS: u8 = 3;

/// 连接尝试期间为 true；主线程据此循环播放信号格动画。
static CONNECTING: AtomicBool = AtomicBool::new(false);
/// 动画帧计数，每 300 ms 递增一次。
static CONNECT_FRAME: AtomicUsize = AtomicUsize::new(0);

/// 连接流程的作用域守卫：无论从哪条 return 退出，都复位动画状态。
struct ConnectingGuard;

impl ConnectingGuard {
    fn start() -> Self {
        CONNECT_FRAME.store(0, Ordering::Relaxed);
        CONNECTING.store(true, Ordering::Relaxed);
        Self
    }
}

impl Drop for ConnectingGuard {
    fn drop(&mut self) {
        CONNECTING.store(false, Ordering::Relaxed);
    }
}

#[derive(Clone, Debug)]
enum Action {
    Restart(Option<String>),
    Close,
    Scan(String),
    ConnectHidden(String),
    Configure(String),
    Disconnect(String),
    FixWpaCtrl,
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

/// 工作线程发回主线程的事件。
enum UiRequest {
    Action(Action),
    ConnectOutcome(ConnectOutcome),
    /// 要求主线程重建菜单与图标（扫描、断开、启用/禁用后）。
    Refresh,
}

/// 一次连接尝试的结果；失败时带上足够的信息决定重试或清理。
enum ConnectOutcome {
    Connected {
        ssid: String,
    },
    Failed {
        ssid: String,
        nic: String,
        security: String,
        saved: bool,
        id: Option<String>,
        attempt: u8,
    },
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
    /// 企业级网络，弹 EAP 凭据对话框。
    Eap,
    /// 托盘无法处理的类型。
    Unsupported,
}

fn connect_plan(security: &str, saved_id: Option<String>) -> ConnectPlan {
    if let Some(id) = saved_id {
        return ConnectPlan::Saved(id);
    }
    match security {
        "OPEN" => ConnectPlan::Open,
        "WPA2-PSK" | "WPA-PSK" | "WPA3-SAE" | "WEP" => ConnectPlan::Password,
        security if security.ends_with("-EAP") => ConnectPlan::Eap,
        _ => ConnectPlan::Unsupported,
    }
}

struct Snapshot {
    state: TrayState,
    saved: Vec<SavedNetwork>,
    scans: Vec<ScanResult>,
    tunnels: WgDictionary,
    /// 无线已启用但 `wpa_cli` 连不上 daemon（通常是缺 ctrl_interface）。
    wpa_unavailable: bool,
}

fn gather() -> Snapshot {
    let runner = RealRunner::new();
    let sys = Sys::new(&runner);
    let state = sys.tray_state();

    let mut saved = Vec::new();
    let mut scans = Vec::new();
    let mut wpa_unavailable = false;
    if state.kind == Some("wifi") {
        if let Some(card) = state.card.clone() {
            wpa_unavailable = sys.wpa_status(&card).is_empty();
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
        wpa_unavailable,
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
    let wireless = state.kind == Some("wifi");
    let label = |template: &'static str, card: &str| {
        networkmgr_core::i18n::tr(template).replace("%s", card)
    };
    let headline = match (&state.card, state.connection) {
        (None, _) => networkmgr_core::tr!("Network card is not enabled").to_string(),
        (Some(card), "Connected") => match &state.ssid {
            Some(ssid) => format!("{ssid} ({card})"),
            None if wireless => label("WiFi %s Connected", card),
            None => label("Wired %s Connected", card),
        },
        (Some(card), "Disabled") if wireless => label("WiFi %s Disabled", card),
        (Some(card), "Disabled") => label("Wired %s Disabled", card),
        (Some(card), "Unplug") => label("Wired %s Unplug", card),
        (Some(card), "Disconnected") if wireless => label("WiFi %s Disconnected", card),
        (Some(card), _) => label("Wired %s Disconnected", card),
    };
    title!(menu, &headline);
    let _ = menu.append(&PredefinedMenuItem::separator());

    if snapshot.wpa_unavailable {
        add!(
            menu,
            networkmgr_core::tr!("Fix wpa_cli control interface"),
            Action::FixWpaCtrl
        );
        let _ = menu.append(&PredefinedMenuItem::separator());
    }

    if !snapshot.tunnels.configs.is_empty() {
        title!(menu, networkmgr_core::tr!("WireGuard VPN"));
        for (device, config) in &snapshot.tunnels.configs {
            if config.state == "Connected" {
                add!(
                    menu,
                    &networkmgr_core::tr!("Disable %s").replace("%s", &config.info),
                    Action::WgDown(device.clone())
                );
            } else {
                add!(
                    menu,
                    &format!("{} {}", networkmgr_core::tr!("Enable"), config.info),
                    Action::WgUp(device.clone())
                );
            }
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
    }

    if let Some(card) = &state.card {
        if state.connection == "Disabled" {
            let enable = networkmgr_core::tr!("Enable");
            add!(menu, enable, Action::Enable(card.clone()));
        } else {
            add!(
                menu,
                &networkmgr_core::tr!("Disable %s").replace("%s", card),
                Action::Disable(card.clone())
            );
            add!(
                menu,
                &networkmgr_core::tr!("Configure %s").replace("%s", card),
                Action::Configure(card.clone())
            );
            if state.kind == Some("wifi") && state.connection == "Connected" {
                if let Some(ssid) = &state.ssid {
                    add!(
                        menu,
                        &networkmgr_core::tr!("Disconnect from %s").replace("%s", ssid),
                        Action::Disconnect(card.clone())
                    );
                }
            }
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
    }

    let nic = state.card.clone().unwrap_or_default();
    // 已保存网络也带上扫描结果里的安全类型，失败时才知道该重开哪种对话框。
    let scan_security: HashMap<&str, &str> = snapshot
        .scans
        .iter()
        .map(|scan| (scan.ssid.as_str(), scan.security))
        .collect();

    if !snapshot.saved.is_empty() {
        let submenu = Submenu::new(networkmgr_core::tr!("Saved Networks"), true);
        for network in &snapshot.saved {
            let label = format!("{} {}", network.ssid, network.flags);
            let security = scan_security
                .get(network.ssid.as_str())
                .copied()
                .unwrap_or_default()
                .to_string();
            add!(
                submenu,
                &label,
                Action::Connect {
                    ssid: network.ssid.clone(),
                    nic: nic.clone(),
                    saved_id: Some(network.id.clone()),
                    security,
                }
            );
        }
        let _ = menu.append(&submenu);

        // 已保存的网络即使不在扫描范围内也要能删除，否则一个错口令的块
        // 会永远留在 wpa_supplicant.conf 里。
        let forget = Submenu::new(networkmgr_core::tr!("Forget Network"), true);
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
        let submenu = Submenu::new(networkmgr_core::tr!("Available Connections"), true);
        add!(
            submenu,
            networkmgr_core::tr!("Scan for Networks"),
            Action::Scan(nic.clone())
        );
        add!(
            submenu,
            networkmgr_core::tr!("Connect to Hidden Network"),
            Action::ConnectHidden(nic.clone())
        );
        let saved_ids: HashMap<&str, &str> = snapshot
            .saved
            .iter()
            .map(|network| (network.ssid.as_str(), network.id.as_str()))
            .collect();
        for scan in &snapshot.scans {
            // 已经连上的 AP 不再出现在候选里，与 Python 的 passes 行为一致。
            let connected_here = state.connection == "Connected"
                && state.ssid.as_deref() == Some(scan.ssid.as_str());
            if connected_here {
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
        networkmgr_core::tr!("Restart Networking"),
        Action::Restart(state.card.clone())
    );
    let _ = menu.append(&PredefinedMenuItem::separator());
    add!(
        menu,
        networkmgr_core::tr!("Close Network Manager"),
        Action::Close
    );

    (menu, actions)
}

/// 主线程上的动作分发：需要对话框的动作在这里弹窗，阻塞命令交给工作线程。
fn dispatch(action: Action, tx: &mpsc::Sender<UiRequest>) {
    debug_log(&format!("dispatch {action:?}"));
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
                let tx = tx.clone();
                thread::spawn(move || {
                    let runner = RealRunner::new();
                    let sys = Sys::new(&runner);
                    connect_saved(&sys, &nic, &ssid, &id, &security, 0, tx);
                });
            }
            ConnectPlan::Open => {
                let tx = tx.clone();
                thread::spawn(move || {
                    let runner = RealRunner::new();
                    let sys = Sys::new(&runner);
                    connect_new(&sys, &nic, &ssid, "OPEN", None, 0, tx);
                });
            }
            ConnectPlan::Password => {
                if let Some(psk) = ask_password(&ssid) {
                    let tx = tx.clone();
                    thread::spawn(move || {
                        let runner = RealRunner::new();
                        let sys = Sys::new(&runner);
                        connect_new(&sys, &nic, &ssid, &security, Some(&psk), 0, tx);
                    });
                }
            }
            ConnectPlan::Eap => {
                if let Some(eap) = ask_eap_credentials(&ssid) {
                    let tx = tx.clone();
                    thread::spawn(move || {
                        let runner = RealRunner::new();
                        let sys = Sys::new(&runner);
                        connect_eap(&sys, &nic, &ssid, eap, 0, tx);
                    });
                }
            }
            ConnectPlan::Unsupported => {
                let hint = networkmgr_core::tr!(concat!(
                    "needs 802.1X credentials; ",
                    "use the configuration window."
                ));
                show_message(
                    networkmgr_core::tr!("Enterprise network"),
                    &format!("{ssid} {hint}"),
                );
            }
        },
        Action::FixWpaCtrl => {
            if !confirm_fix_wpa_ctrl() {
                return;
            }
            let tx = tx.clone();
            thread::spawn(move || {
                match networkmgr_core::config::fix_ctrl_interface_file("/etc/wpa_supplicant.conf") {
                    Ok(message) => println!("networkmgr: {message}"),
                    Err(error) => eprintln!("networkmgr: {error}"),
                }
                let _ = tx.send(UiRequest::Refresh);
            });
        }
        Action::ConnectHidden(nic) => {
            if let Some((ssid, psk)) = ask_hidden_network() {
                let security = if psk.is_some() { "WPA2-PSK" } else { "OPEN" };
                let tx = tx.clone();
                thread::spawn(move || {
                    let runner = RealRunner::new();
                    let sys = Sys::new(&runner);
                    connect_new(&sys, &nic, &ssid, security, psk.as_deref(), 0, tx);
                });
            }
        }
        Action::Disconnect(nic) => {
            let tx = tx.clone();
            thread::spawn(move || {
                let runner = RealRunner::new();
                let sys = Sys::new(&runner);
                if sys.wpa_disconnect(&nic) {
                    println!("networkmgr: disconnected {nic}");
                } else {
                    eprintln!("networkmgr: disconnect failed on {nic}");
                }
                let _ = tx.send(UiRequest::Refresh);
            });
        }
        Action::Forget { ssid, nic } => {
            let tx = tx.clone();
            thread::spawn(move || {
                forget_network(&nic, &ssid);
                let _ = tx.send(UiRequest::Refresh);
            });
        }
        other => {
            let tx = tx.clone();
            thread::spawn(move || {
                execute(other);
                let _ = tx.send(UiRequest::Refresh);
            });
        }
    }
}

/// 配置窗口的候选路径：安装后的契约名优先，开发时回退 cargo 产物名。
const CONFIG_WINDOW_CANDIDATES: &[&str] = &[
    "/usr/local/bin/networkmgr_configuration-rust",
    "/usr/local/bin/networkmgr_configuration",
    "config-window",
];

/// 启动配置窗口并预选网卡；找不到二进制时只记录错误，不阻塞托盘。
fn open_configuration(nic: &str) {
    for program in CONFIG_WINDOW_CANDIDATES {
        match std::process::Command::new(program)
            .arg("--interface")
            .arg(nic)
            .spawn()
        {
            Ok(child) => {
                debug_log(&format!("opened {program} for {nic} (pid {})", child.id()));
                println!("networkmgr: opened configuration for {nic}");
                return;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                debug_log(&format!("{program} not found"));
            }
            Err(error) => {
                debug_log(&format!("failed to open {program}: {error}"));
                eprintln!("networkmgr: failed to open {program}: {error}");
                return;
            }
        }
    }
    debug_log("configuration window not found");
    eprintln!("networkmgr: configuration window not found");
}

fn debug_log(message: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("networkmgr-debug.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{message}");
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
            open_configuration(&nic);
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

fn send_outcome(tx: &mpsc::Sender<UiRequest>, outcome: ConnectOutcome) {
    let _ = tx.send(UiRequest::ConnectOutcome(outcome));
}

fn failed(
    ssid: &str,
    nic: &str,
    security: &str,
    saved: bool,
    id: Option<String>,
    attempt: u8,
) -> ConnectOutcome {
    ConnectOutcome::Failed {
        ssid: ssid.to_string(),
        nic: nic.to_string(),
        security: security.to_string(),
        saved,
        id,
        attempt,
    }
}

fn connect_saved(
    sys: &Sys<'_, RealRunner>,
    nic: &str,
    ssid: &str,
    id: &str,
    security: &str,
    attempt: u8,
    tx: mpsc::Sender<UiRequest>,
) {
    let _guard = ConnectingGuard::start();
    if !sys.wpa_select_network(nic, id) {
        send_outcome(
            &tx,
            failed(ssid, nic, security, true, Some(id.into()), attempt),
        );
        return;
    }
    let connected = sys.wpa_wait_for_connection(nic, ssid, CONNECT_TIMEOUT);
    if connected {
        let _ = sys.wpa_save_config(nic);
    }
    let _ = sys.wpa_enable_all(nic);
    send_outcome(
        &tx,
        if connected {
            ConnectOutcome::Connected {
                ssid: ssid.to_string(),
            }
        } else {
            failed(ssid, nic, security, true, Some(id.into()), attempt)
        },
    );
}

fn connect_new(
    sys: &Sys<'_, RealRunner>,
    nic: &str,
    ssid: &str,
    security: &str,
    psk: Option<&str>,
    attempt: u8,
    tx: mpsc::Sender<UiRequest>,
) {
    let _guard = ConnectingGuard::start();
    let Some(id) = sys.wpa_save_network(nic, ssid, security, psk) else {
        send_outcome(&tx, failed(ssid, nic, security, false, None, attempt));
        return;
    };
    if !sys.wpa_select_network(nic, &id) {
        send_outcome(&tx, failed(ssid, nic, security, false, Some(id), attempt));
        return;
    }
    let connected = sys.wpa_wait_for_connection(nic, ssid, CONNECT_TIMEOUT);
    if connected {
        let _ = sys.wpa_save_config(nic);
    }
    let _ = sys.wpa_enable_all(nic);
    send_outcome(
        &tx,
        if connected {
            ConnectOutcome::Connected {
                ssid: ssid.to_string(),
            }
        } else {
            failed(ssid, nic, security, false, Some(id), attempt)
        },
    );
}

fn connect_eap(
    sys: &Sys<'_, RealRunner>,
    nic: &str,
    ssid: &str,
    eap: config::EapConfig,
    attempt: u8,
    tx: mpsc::Sender<UiRequest>,
) {
    let _guard = ConnectingGuard::start();
    let Some(id) = sys.wpa_save_eap_network(nic, ssid, &eap) else {
        send_outcome(&tx, failed(ssid, nic, "WPA2-EAP", false, None, attempt));
        return;
    };
    if !sys.wpa_select_network(nic, &id) {
        send_outcome(&tx, failed(ssid, nic, "WPA2-EAP", false, Some(id), attempt));
        return;
    }
    let connected = sys.wpa_wait_for_connection(nic, ssid, CONNECT_TIMEOUT);
    if connected {
        let _ = sys.wpa_save_config(nic);
    }
    let _ = sys.wpa_enable_all(nic);
    send_outcome(
        &tx,
        if connected {
            ConnectOutcome::Connected {
                ssid: ssid.to_string(),
            }
        } else {
            failed(ssid, nic, "WPA2-EAP", false, Some(id), attempt)
        },
    );
}

/// 已保存网络的密码重试：只改 psk，保留块里的其他字段。
fn connect_update_psk(
    sys: &Sys<'_, RealRunner>,
    nic: &str,
    ssid: &str,
    id: &str,
    psk: &str,
    attempt: u8,
    tx: mpsc::Sender<UiRequest>,
) {
    let _guard = ConnectingGuard::start();
    let updated = sys.wpa_update_psk(nic, id, psk)
        && sys.wpa_enable_network(nic, id)
        && sys.wpa_select_network(nic, id);
    if !updated {
        send_outcome(
            &tx,
            failed(ssid, nic, "WPA2-PSK", true, Some(id.into()), attempt),
        );
        return;
    }
    let connected = sys.wpa_wait_for_connection(nic, ssid, CONNECT_TIMEOUT);
    if connected {
        let _ = sys.wpa_save_config(nic);
    }
    let _ = sys.wpa_enable_all(nic);
    send_outcome(
        &tx,
        if connected {
            ConnectOutcome::Connected {
                ssid: ssid.to_string(),
            }
        } else {
            failed(ssid, nic, "WPA2-PSK", true, Some(id.into()), attempt)
        },
    );
}

/// 主线程：失败后按 Python 的策略重开对话框，最多 3 次。
fn handle_connect_outcome(
    outcome: ConnectOutcome,
    tx: &mpsc::Sender<UiRequest>,
    refresh: &dyn Fn(),
) {
    match outcome {
        ConnectOutcome::Connected { ssid } => {
            println!("networkmgr: connected to {ssid}");
            refresh();
        }
        ConnectOutcome::Failed {
            ssid,
            nic,
            security,
            saved,
            id,
            attempt,
        } => {
            let next = attempt + 1;
            let retryable = next < MAX_ATTEMPTS;
            if retryable && security.ends_with("-EAP") {
                if let Some(eap) = ask_eap_credentials(&ssid) {
                    let tx = tx.clone();
                    thread::spawn(move || {
                        let runner = RealRunner::new();
                        let sys = Sys::new(&runner);
                        let _ = sys.wpa_remove_by_ssid(&nic, &ssid);
                        let _ = sys.wpa_reconfigure(&nic);
                        connect_eap(&sys, &nic, &ssid, eap, next, tx);
                    });
                    return;
                }
            } else if retryable
                && matches!(
                    security.as_str(),
                    "WPA2-PSK" | "WPA-PSK" | "WPA3-SAE" | "WEP"
                )
            {
                if let Some(psk) = ask_password(&ssid) {
                    let tx = tx.clone();
                    let saved_id = id.clone();
                    thread::spawn(move || {
                        let runner = RealRunner::new();
                        let sys = Sys::new(&runner);
                        if saved {
                            if let Some(id) = saved_id {
                                connect_update_psk(&sys, &nic, &ssid, &id, &psk, next, tx);
                                return;
                            }
                        }
                        let _ = sys.wpa_remove_by_ssid(&nic, &ssid);
                        connect_new(&sys, &nic, &ssid, &security, Some(&psk), next, tx);
                    });
                    return;
                }
            }
            // 放弃：新建块删除，已保存块保留。
            if !saved {
                if let Some(id) = &id {
                    let runner = RealRunner::new();
                    let sys = Sys::new(&runner);
                    let _ = sys.wpa_remove_network(&nic, id);
                }
            }
            show_message(
                &networkmgr_core::tr!("Could not connect to %s").replace("%s", &ssid),
                &networkmgr_core::tr!("Could not connect to %s. Try again.").replace("%s", &ssid),
            );
            refresh();
        }
    }
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
        "{}\n\n{}",
        networkmgr_core::tr!("Restart networking on %s?").replace("%s", target),
        networkmgr_core::tr!("The interface goes down briefly and the connection may drop.")
    );
    let dialog = gtk::Dialog::with_buttons(
        Some(networkmgr_core::tr!("Restart Networking")),
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        &[
            (networkmgr_core::tr!("Cancel"), gtk::ResponseType::Cancel),
            (networkmgr_core::tr!("Restart"), gtk::ResponseType::Ok),
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
fn confirm_fix_wpa_ctrl() -> bool {
    use gtk::prelude::*;
    let dialog = gtk::MessageDialog::new(
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Question,
        gtk::ButtonsType::None,
        networkmgr_core::tr!(concat!(
            "Add ctrl_interface to /etc/wpa_supplicant.conf? ",
            "A backup is kept; restart wpa_supplicant afterwards."
        )),
    );
    dialog.set_title(networkmgr_core::tr!("Fix wpa_cli control interface"));
    dialog.add_button(networkmgr_core::tr!("Cancel"), gtk::ResponseType::Cancel);
    dialog.add_button(networkmgr_core::tr!("Fix"), gtk::ResponseType::Ok);
    dialog.set_default_response(gtk::ResponseType::Cancel);
    let response = dialog.run();
    dialog.close();
    response == gtk::ResponseType::Ok
}

#[cfg(not(feature = "appindicator"))]
fn confirm_fix_wpa_ctrl() -> bool {
    eprintln!("networkmgr: fixing ctrl_interface needs the appindicator (GTK) backend");
    false
}

#[cfg(feature = "appindicator")]
fn ask_password(ssid: &str) -> Option<String> {
    use gtk::prelude::*;
    let title = networkmgr_core::tr!("Connect to %s").replace("%s", ssid);
    let dialog = gtk::Dialog::with_buttons(
        Some(&title),
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        &[
            (networkmgr_core::tr!("Cancel"), gtk::ResponseType::Cancel),
            (networkmgr_core::tr!("Connect"), gtk::ResponseType::Ok),
        ],
    );
    dialog.set_default_response(gtk::ResponseType::Ok);
    let entry = gtk::Entry::new();
    entry.set_visibility(false);
    entry.set_activates_default(true);
    entry.set_placeholder_text(Some(networkmgr_core::tr!("Password:")));
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

/// 手动连接隐藏网络：SSID + 口令，或勾选「开放网络」。
#[cfg(feature = "appindicator")]
fn ask_hidden_network() -> Option<(String, Option<String>)> {
    use gtk::prelude::*;
    let dialog = gtk::Dialog::with_buttons(
        Some(networkmgr_core::tr!("Connect to Hidden Network")),
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        &[
            (networkmgr_core::tr!("Cancel"), gtk::ResponseType::Cancel),
            (networkmgr_core::tr!("Connect"), gtk::ResponseType::Ok),
        ],
    );
    dialog.set_default_response(gtk::ResponseType::Ok);
    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(12);

    let ssid_label = gtk::Label::new(Some("SSID"));
    ssid_label.set_xalign(0.0);
    let ssid = gtk::Entry::new();
    ssid.set_hexpand(true);
    grid.attach(&ssid_label, 0, 0, 1, 1);
    grid.attach(&ssid, 1, 0, 1, 1);

    let password_label = gtk::Label::new(Some(networkmgr_core::tr!("Password:")));
    password_label.set_xalign(0.0);
    let password = gtk::Entry::new();
    password.set_visibility(false);
    password.set_hexpand(true);
    grid.attach(&password_label, 0, 1, 1, 1);
    grid.attach(&password, 1, 1, 1, 1);

    let open = gtk::CheckButton::with_label(networkmgr_core::tr!("Open network"));
    grid.attach(&open, 1, 2, 1, 1);
    let password_toggle = password.clone();
    open.connect_toggled(move |check| {
        password_toggle.set_sensitive(!check.is_active());
    });

    dialog.content_area().add(&grid);
    dialog.show_all();
    let response = dialog.run();
    let ssid_text = ssid.text().to_string();
    let password_text = password.text().to_string();
    let is_open = open.is_active();
    dialog.close();

    if response != gtk::ResponseType::Ok || ssid_text.is_empty() {
        return None;
    }
    if is_open {
        Some((ssid_text, None))
    } else if password_text.is_empty() {
        None
    } else {
        Some((ssid_text, Some(password_text)))
    }
}

#[cfg(not(feature = "appindicator"))]
fn ask_hidden_network() -> Option<(String, Option<String>)> {
    eprintln!("networkmgr: hidden network prompt needs the appindicator (GTK) backend");
    None
}

/// 企业级（802.1X）凭据对话框。
///
/// TLS 用客户端证书认证，没有口令与内层认证；切换方法时把无关字段置灰，
/// 避免用户填了 password 却选 TLS 这种无效组合。
#[cfg(feature = "appindicator")]
fn ask_eap_credentials(ssid: &str) -> Option<config::EapConfig> {
    use gtk::prelude::*;

    let title = networkmgr_core::tr!("Enterprise Authentication for %s").replace("%s", ssid);
    let dialog = gtk::Dialog::with_buttons(
        Some(&title),
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        &[
            (networkmgr_core::tr!("Cancel"), gtk::ResponseType::Cancel),
            (networkmgr_core::tr!("Connect"), gtk::ResponseType::Ok),
        ],
    );
    dialog.set_default_response(gtk::ResponseType::Ok);
    dialog.set_border_width(12);

    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(12);

    let mut row = 0;
    let add_label = |text: &str, row: i32| {
        let label = gtk::Label::new(Some(text));
        label.set_xalign(0.0);
        grid.attach(&label, 0, row, 1, 1);
    };

    add_label(networkmgr_core::tr!("EAP Method:"), row);
    let method = gtk::ComboBoxText::new();
    for item in ["PEAP", "TTLS", "TLS"] {
        method.append_text(item);
    }
    method.set_active(Some(0));
    grid.attach(&method, 1, row, 1, 1);
    row += 1;

    add_label(networkmgr_core::tr!("Username:"), row);
    let identity = gtk::Entry::new();
    identity.set_hexpand(true);
    grid.attach(&identity, 1, row, 1, 1);
    row += 1;

    add_label(networkmgr_core::tr!("Password:"), row);
    let password = gtk::Entry::new();
    password.set_visibility(false);
    password.set_hexpand(true);
    grid.attach(&password, 1, row, 1, 1);
    row += 1;

    add_label(networkmgr_core::tr!("Inner Auth:"), row);
    let phase2 = gtk::ComboBoxText::new();
    for item in ["MSCHAPV2", "MSCHAP", "PAP", "CHAP", "GTC", "TLS"] {
        phase2.append_text(item);
    }
    phase2.set_active(Some(0));
    grid.attach(&phase2, 1, row, 1, 1);
    row += 1;

    add_label(networkmgr_core::tr!("Anonymous ID:"), row);
    let anonymous = gtk::Entry::new();
    anonymous.set_placeholder_text(Some(networkmgr_core::tr!("Optional - for privacy")));
    grid.attach(&anonymous, 1, row, 1, 1);
    row += 1;

    add_label(networkmgr_core::tr!("CA Certificate:"), row);
    let ca_cert = gtk::Entry::new();
    grid.attach(&ca_cert, 1, row, 1, 1);
    row += 1;

    add_label("Domain suffix:", row);
    let domain = gtk::Entry::new();
    grid.attach(&domain, 1, row, 1, 1);
    row += 1;

    add_label(networkmgr_core::tr!("Client Cert:"), row);
    let client_cert = gtk::Entry::new();
    grid.attach(&client_cert, 1, row, 1, 1);
    row += 1;

    add_label(networkmgr_core::tr!("Private Key:"), row);
    let private_key = gtk::Entry::new();
    grid.attach(&private_key, 1, row, 1, 1);
    row += 1;

    add_label(networkmgr_core::tr!("Key Password:"), row);
    let key_password = gtk::Entry::new();
    key_password.set_visibility(false);
    grid.attach(&key_password, 1, row, 1, 1);

    let password_toggle = password.clone();
    let phase2_toggle = phase2.clone();
    let tls_widgets = [
        client_cert.clone(),
        private_key.clone(),
        key_password.clone(),
    ];
    method.connect_changed(move |combo| {
        let tls = combo
            .active_text()
            .map(|text| text.as_str() == "TLS")
            .unwrap_or(false);
        password_toggle.set_sensitive(!tls);
        phase2_toggle.set_sensitive(!tls);
        for widget in &tls_widgets {
            widget.set_sensitive(tls);
        }
    });
    client_cert.set_sensitive(false);
    private_key.set_sensitive(false);
    key_password.set_sensitive(false);

    dialog.content_area().add(&grid);
    dialog.show_all();
    let response = dialog.run();
    let eap = config::EapConfig {
        eap_method: method.active_text().map(|text| text.to_string()),
        identity: identity.text().to_string(),
        password: password.text().to_string(),
        phase2: phase2.active_text().map(|text| text.to_string()),
        anonymous_identity: anonymous.text().to_string(),
        ca_cert: ca_cert.text().to_string(),
        client_cert: client_cert.text().to_string(),
        private_key: private_key.text().to_string(),
        private_key_passwd: key_password.text().to_string(),
        domain_suffix_match: domain.text().to_string(),
    };
    dialog.close();

    if response != gtk::ResponseType::Ok {
        return None;
    }
    let tls = eap.eap_method.as_deref() == Some("TLS");
    let valid = !eap.identity.is_empty()
        && if tls {
            !eap.client_cert.is_empty() && !eap.private_key.is_empty()
        } else {
            !eap.password.is_empty()
        };
    if !valid {
        show_message(
            &networkmgr_core::tr!("%s Enterprise Authentication Failed").replace("%s", ssid),
            networkmgr_core::tr!(
                "Enter the username and either the password or the TLS certificate paths."
            ),
        );
        return None;
    }
    Some(eap)
}

#[cfg(not(feature = "appindicator"))]
fn ask_eap_credentials(ssid: &str) -> Option<config::EapConfig> {
    eprintln!("networkmgr: EAP credentials for {ssid} need the appindicator (GTK) backend");
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

/// 把 DBus/X 环境对齐到当前桌面会话，让直接运行 `networkmgr-rust` 也能工作。
///
/// - 扫描 `/tmp/dbus-*`，选带 `org.kde.StatusNotifierWatcher` 的 bus；
/// - 未设置 DISPLAY/XAUTHORITY 时用 `:0` 与 `/home/<user>/.Xauthority`；
/// - root 通过 sudo 或直接登录时，降权到桌面用户再运行。
fn prepare_session_environment() {
    let bus = find_status_notifier_bus();
    if let Some(bus) = &bus {
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", bus);
    }
    if std::env::var_os("DISPLAY").is_none() {
        std::env::set_var("DISPLAY", ":0");
    }
    let user = std::env::var("SUDO_USER")
        .ok()
        .or_else(|| find_panel().and_then(|pid| process_owner(&pid)))
        .or_else(|| std::env::var("USER").ok())
        .filter(|user| user != "root");
    let Some(user) = user else {
        return;
    };
    if std::env::var_os("XAUTHORITY").is_none() {
        let path = format!("/home/{user}/.Xauthority");
        if std::path::Path::new(&path).exists() {
            std::env::set_var("XAUTHORITY", path);
        }
    }
    if !is_root() {
        return;
    }
    let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
    let xauthority = std::env::var("XAUTHORITY").ok();
    relaunch_as(&user, &display, xauthority.as_deref(), bus.as_deref());
    std::process::exit(0);
}

fn find_panel() -> Option<String> {
    let output = std::process::Command::new("pgrep")
        .args(["-x", "xfce4-panel"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(str::to_string)
}

/// 找一条能注册 StatusNotifierItem 的 session bus。
fn find_status_notifier_bus() -> Option<String> {
    let mut candidates: Vec<String> = std::fs::read_dir("/tmp")
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with("dbus-")
                .then(|| format!("unix:path=/tmp/{name}"))
        })
        .collect();
    candidates.sort();
    candidates
        .into_iter()
        .find(|address| status_notifier_reachable(address))
}

fn status_notifier_reachable(address: &str) -> bool {
    std::process::Command::new("dbus-send")
        .env("DBUS_SESSION_BUS_ADDRESS", address)
        .args([
            "--session",
            "--dest=org.kde.StatusNotifierWatcher",
            "--type=method_call",
            "--print-reply",
            "/StatusNotifierWatcher",
            "org.freedesktop.DBus.Peer.Ping",
        ])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn process_owner(pid: &str) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-o", "user=", "-p", pid])
        .output()
        .ok()?;
    let user = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if user.is_empty() {
        None
    } else {
        Some(user)
    }
}

fn is_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "0")
        .unwrap_or(false)
}

fn relaunch_as(user: &str, display: &str, xauthority: Option<&str>, bus: Option<&str>) {
    let exe = std::env::current_exe().unwrap_or_default();
    let mut command = format!("env DISPLAY={display}");
    if let Some(xauthority) = xauthority {
        command.push_str(&format!(" XAUTHORITY={xauthority}"));
    }
    if let Some(bus) = bus {
        command.push_str(&format!(" DBUS_SESSION_BUS_ADDRESS='{bus}'"));
    }
    command.push(' ');
    command.push_str(&exe.to_string_lossy());
    for arg in std::env::args().skip(1) {
        command.push(' ');
        command.push_str(&arg);
    }
    debug_log(&format!("relaunching as {user}: {command}"));
    let _ = std::process::Command::new("su")
        .args(["-m", user, "-c", &command])
        .status();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    networkmgr_core::i18n::init();
    prepare_session_environment();
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

    // 工作线程 → 主线程的事件通道；菜单事件也走这里。
    let (tx, rx) = mpsc::channel::<UiRequest>();

    // 菜单重建是主线程职责：定时刷新与动作完成后的即时刷新共用同一段逻辑。
    #[cfg(feature = "appindicator")]
    {
        let latest = Rc::new(RefCell::new(snapshot.state.clone()));
        let refresh: Rc<dyn Fn()> = {
            let tray_refresh = Rc::clone(&tray);
            let actions_refresh = Arc::clone(&actions);
            let latest_refresh = Rc::clone(&latest);
            Rc::new(move || {
                let snapshot = gather();
                let (menu, map) = build_menu(&snapshot);
                *actions_refresh.lock().expect("actions lock") = map;
                *latest_refresh.borrow_mut() = snapshot.state.clone();
                let tray = tray_refresh.borrow();
                let _ = tray.set_tooltip(Some(&snapshot.state.tooltip));
                // 连接动画期间不要用静态图标覆盖动画帧。
                if !CONNECTING.load(Ordering::Relaxed) {
                    let _ = tray.set_icon(Some(icon_for(
                        snapshot.state.signal,
                        snapshot.state.kind == Some("wifi"),
                    )));
                }
                tray.set_menu(Some(Box::new(menu)));
            })
        };

        let refresh_timer = Rc::clone(&refresh);
        gtk::glib::timeout_add_seconds_local(REFRESH_SECONDS, move || {
            refresh_timer();
            gtk::glib::ControlFlow::Continue
        });

        // 300 ms 一帧，与 Python 版一致：0→4 格循环，结束恢复真实信号。
        let tray_anim = Rc::clone(&tray);
        let latest_anim = Rc::clone(&latest);
        let mut was_animating = false;
        gtk::glib::timeout_add_local(Duration::from_millis(ANIMATION_MILLIS), move || {
            if CONNECTING.load(Ordering::Relaxed) {
                was_animating = true;
                let frame = CONNECT_FRAME.fetch_add(1, Ordering::Relaxed) as u32 % 5;
                let _ = tray_anim.borrow().set_icon(Some(bars_icon(frame, true)));
            } else if was_animating {
                was_animating = false;
                let state = latest_anim.borrow();
                let _ = tray_anim
                    .borrow()
                    .set_icon(Some(icon_for(state.signal, state.kind == Some("wifi"))));
            }
            gtk::glib::ControlFlow::Continue
        });

        let refresh_channel = Rc::clone(&refresh);
        let tx_channel = tx.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
            while let Ok(request) = rx.try_recv() {
                match request {
                    UiRequest::Action(action) => dispatch(action, &tx_channel),
                    UiRequest::ConnectOutcome(outcome) => {
                        handle_connect_outcome(outcome, &tx_channel, refresh_channel.as_ref());
                    }
                    UiRequest::Refresh => refresh_channel(),
                }
            }
            gtk::glib::ControlFlow::Continue
        });
        gtk::main();
    }

    let seconds = std::env::var("SPIKE_SECONDS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(600);
    let deadline = Instant::now() + Duration::from_secs(seconds);

    // 菜单事件线程只转发动作；对话框和命令分发都回到主线程。
    let menu_rx = MenuEvent::receiver();
    let icon_rx = TrayIconEvent::receiver();
    let actions_events = Arc::clone(&actions);
    let tx_events = tx.clone();
    let card_events = snapshot.state.card.clone();
    thread::spawn(move || loop {
        if let Ok(event) = menu_rx.try_recv() {
            debug_log(&format!("menu event id={}", event.id.0));
            let action = actions_events
                .lock()
                .expect("actions lock")
                .get(&event.id.0)
                .cloned();
            match action {
                Some(action) => {
                    let _ = tx_events.send(UiRequest::Action(action));
                }
                None => println!("networkmgr: unknown menu id {:?}", event.id),
            }
        }
        if let Ok(event) = icon_rx.try_recv() {
            debug_log(&format!("tray event {event:?}"));
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Some(card) = &card_events {
                    let _ = tx_events.send(UiRequest::Action(Action::Configure(card.clone())));
                }
            }
        }
        if Instant::now() >= deadline {
            println!("networkmgr: timeout, exiting");
            std::process::exit(0);
        }
        thread::sleep(Duration::from_millis(50));
    });

    #[cfg(not(feature = "appindicator"))]
    loop {
        while let Ok(request) = rx.try_recv() {
            if let UiRequest::Action(action) = request {
                dispatch(action, &tx);
            }
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
    let bars = match signal {
        None => 0,
        Some(value) if value >= 75 => 4,
        Some(value) if value >= 50 => 3,
        Some(value) if value >= 25 => 2,
        Some(_) => 1,
    };
    app_icon(bars, wireless)
}

/// 应用图标 + 右下角状态角标：有线显示绿/红点，无线显示 4 格信号。
fn app_icon(bars: u32, wireless: bool) -> Icon {
    const SIZE: u32 = 32;
    let mut pixels = APP_ICON_RGBA.to_vec();
    let color = if !wireless {
        if bars == 0 {
            (200, 80, 80)
        } else {
            (80, 200, 120)
        }
    } else if bars >= 3 {
        (60, 150, 90)
    } else if bars >= 1 {
        (200, 150, 60)
    } else {
        (170, 70, 70)
    };
    let mut put = |x: u32, y: u32| {
        let index = ((y * SIZE + x) * 4) as usize;
        if index + 3 < pixels.len() {
            pixels[index] = color.0;
            pixels[index + 1] = color.1;
            pixels[index + 2] = color.2;
            pixels[index + 3] = 255;
        }
    };
    if wireless {
        for slot in 0..4 {
            if slot < bars {
                let x0 = 18 + slot * 3;
                for y in 24..29 {
                    for x in x0..x0 + 2 {
                        put(x, y);
                    }
                }
            }
        }
    } else {
        for y in 24..29 {
            for x in 23..28 {
                put(x, y);
            }
        }
    }
    Icon::from_rgba(pixels, SIZE, SIZE).expect("32x32 RGBA icon is valid")
}

/// 直接按格数绘制，供连接动画逐帧调用（0→4 格循环）。
fn bars_icon(bars: u32, wireless: bool) -> Icon {
    const SIZE: u32 = 32;
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
            wpa_unavailable: false,
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
        assert_eq!(connect_plan("WPA2-EAP", None), ConnectPlan::Eap);
    }

    #[test]
    fn configuration_candidates_start_with_the_contract_path() {
        assert_eq!(
            CONFIG_WINDOW_CANDIDATES[0],
            "/usr/local/bin/networkmgr_configuration-rust"
        );
    }
}
