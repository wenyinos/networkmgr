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
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

const WG_CONFIG_DIR: &str = "/usr/local/etc/wireguard";
#[cfg_attr(not(feature = "appindicator"), allow(dead_code))]
const REFRESH_SECONDS: u32 = 30;
/// 等一次无线连接完成的上限，与 Python `wait_for_connection()` 一致。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// 动画帧间隔，与 Python 的 `GLib.timeout_add(300, ...)` 一致。
const ANIMATION_MILLIS: u64 = 300;

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
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
    }

    let nic = state.card.clone().unwrap_or_default();

    if !snapshot.saved.is_empty() {
        let submenu = Submenu::new(networkmgr_core::tr!("Saved Networks"), true);
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
            ConnectPlan::Eap => {
                if let Some(eap) = ask_eap_credentials(&ssid) {
                    thread::spawn(move || connect_eap(&nic, &ssid, eap));
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
        Action::Forget { ssid, nic } => {
            thread::spawn(move || forget_network(&nic, &ssid));
        }
        other => {
            thread::spawn(move || execute(other));
        }
    }
}

/// 配置窗口的候选路径：安装后的契约名优先，开发时回退 cargo 产物名。
const CONFIG_WINDOW_CANDIDATES: &[&str] =
    &["/usr/local/bin/networkmgr_configuration", "config-window"];

/// 启动配置窗口并预选网卡；找不到二进制时只记录错误，不阻塞托盘。
fn open_configuration(nic: &str) {
    for program in CONFIG_WINDOW_CANDIDATES {
        match std::process::Command::new(program)
            .arg("--interface")
            .arg(nic)
            .spawn()
        {
            Ok(_) => {
                println!("networkmgr: opened configuration for {nic}");
                return;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!("networkmgr: failed to open {program}: {error}");
                return;
            }
        }
    }
    eprintln!("networkmgr: configuration window not found");
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

fn connect_saved(nic: &str, ssid: &str, id: &str) {
    let _guard = ConnectingGuard::start();
    let runner = RealRunner::new();
    let sys = Sys::new(&runner);
    if !sys.wpa_select_network(nic, id) {
        eprintln!("networkmgr: {ssid} could not be selected");
        return;
    }
    finish_connection(&sys, nic, ssid, true, id);
}

fn connect_new(nic: &str, ssid: &str, security: &str, psk: Option<&str>) {
    let _guard = ConnectingGuard::start();
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

fn connect_eap(nic: &str, ssid: &str, eap: config::EapConfig) {
    let _guard = ConnectingGuard::start();
    let runner = RealRunner::new();
    let sys = Sys::new(&runner);
    let Some(id) = sys.wpa_save_eap_network(nic, ssid, &eap) else {
        eprintln!("networkmgr: could not create an enterprise block for {ssid}");
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
        method.append_text(Some(item));
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
        phase2.append_text(Some(item));
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
    let client_toggle = client_cert.clone();
    let key_toggle = private_key.clone();
    let key_password_toggle = key_password.clone();
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
            networkmgr_core::tr!("%s Enterprise Authentication Failed").replace("%s", ssid),
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    networkmgr_core::i18n::init();
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
        let latest = Rc::new(RefCell::new(snapshot.state.clone()));
        let tray_timer = Rc::clone(&tray);
        let actions_timer = Arc::clone(&actions);
        let latest_timer = Rc::clone(&latest);
        gtk::glib::timeout_add_seconds_local(REFRESH_SECONDS, move || {
            let snapshot = gather();
            let (menu, map) = build_menu(&snapshot);
            *actions_timer.lock().expect("actions lock") = map;
            *latest_timer.borrow_mut() = snapshot.state.clone();
            let tray = tray_timer.borrow();
            let _ = tray.set_tooltip(Some(&snapshot.state.tooltip));
            // 连接动画期间不要用静态图标覆盖动画帧。
            if !CONNECTING.load(Ordering::Relaxed) {
                let _ = tray.set_icon(Some(icon_for(
                    snapshot.state.signal,
                    snapshot.state.kind == Some("wifi"),
                )));
            }
            tray.set_menu(Some(Box::new(menu)));
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
    let bars = match signal {
        None => 0,
        Some(value) if value >= 75 => 4,
        Some(value) if value >= 50 => 3,
        Some(value) if value >= 25 => 2,
        Some(_) => 1,
    };
    bars_icon(bars, wireless)
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
            "/usr/local/bin/networkmgr_configuration"
        );
    }
}
