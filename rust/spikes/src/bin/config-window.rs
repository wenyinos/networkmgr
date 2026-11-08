//! 阶段 2：`networkmgr_configuration` 的 eframe 配置窗口。
//!
//! 视觉参考 COSMIC / iced 设计系统：左侧导航（选中项为胶囊）、右侧卡片式表单、
//! 明暗两套主题一键切换。配色取自参考图的实际像素，不是凭感觉调的。
//!
//! 功能边界没变：默认只做校验与预览，`--apply` 才真的写 `rc.conf` 与
//! `/etc/resolv.conf`；不重启接口。`SPIKE_SECONDS` 到点自动关闭。

use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, CornerRadius, Margin, RichText, Stroke};
use networkmgr_core::sys::{RealRunner, Sys};
use networkmgr_core::{config, validate};

// ---------------------------------------------------------------- 主题

/// 一套配色。取自参考图：明色选中 `#006CA0`，暗色选中 `#69E4E3`。
struct Theme {
    is_dark: bool,
    window: Color32,
    sidebar: Color32,
    card: Color32,
    card_hover: Color32,
    text: Color32,
    muted: Color32,
    accent: Color32,
    accent_text: Color32,
    secondary: Color32,
    secondary_text: Color32,
    positive: Color32,
    destructive: Color32,
    border: Color32,
}

impl Theme {
    fn light() -> Self {
        Self {
            is_dark: false,
            window: Color32::from_rgb(235, 235, 235),
            sidebar: Color32::from_rgb(247, 247, 247),
            card: Color32::from_rgb(250, 250, 250),
            card_hover: Color32::from_rgb(240, 240, 240),
            text: Color32::from_rgb(28, 28, 28),
            muted: Color32::from_rgb(110, 110, 110),
            accent: Color32::from_rgb(0, 108, 160),
            accent_text: Color32::from_rgb(255, 255, 255),
            secondary: Color32::from_rgb(216, 216, 216),
            secondary_text: Color32::from_rgb(40, 40, 40),
            positive: Color32::from_rgb(56, 110, 60),
            destructive: Color32::from_rgb(168, 45, 52),
            border: Color32::from_rgb(220, 220, 220),
        }
    }

    fn dark() -> Self {
        Self {
            is_dark: true,
            window: Color32::from_rgb(24, 24, 24),
            sidebar: Color32::from_rgb(40, 40, 40),
            card: Color32::from_rgb(43, 43, 43),
            card_hover: Color32::from_rgb(55, 55, 55),
            text: Color32::from_rgb(232, 232, 232),
            muted: Color32::from_rgb(150, 150, 150),
            accent: Color32::from_rgb(105, 228, 227),
            accent_text: Color32::from_rgb(16, 16, 16),
            secondary: Color32::from_rgb(90, 90, 90),
            secondary_text: Color32::from_rgb(235, 235, 235),
            positive: Color32::from_rgb(172, 227, 199),
            destructive: Color32::from_rgb(244, 178, 178),
            border: Color32::from_rgb(58, 58, 58),
        }
    }

    fn for_mode(dark: bool) -> Self {
        if dark {
            Self::dark()
        } else {
            Self::light()
        }
    }
}

fn apply_visuals(ctx: &egui::Context, theme: &Theme) {
    let mut visuals = if theme.is_dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.panel_fill = theme.window;
    visuals.window_fill = theme.card;
    visuals.extreme_bg_color = theme.sidebar;
    visuals.override_text_color = Some(theme.text);
    visuals.selection.bg_fill = theme.accent;
    visuals.selection.stroke = Stroke::new(1.0, theme.accent_text);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, theme.border);
    visuals.widgets.inactive.bg_fill = theme.secondary;
    visuals.widgets.inactive.weak_bg_fill = theme.secondary;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, theme.secondary_text);
    visuals.widgets.hovered.weak_bg_fill = theme.card_hover;
    visuals.widgets.active.weak_bg_fill = theme.accent;
    ctx.set_visuals(visuals);
}

// ---------------------------------------------------------------- 组件

/// 卡片容器：圆角、卡片底色、内边距 16。
fn card(theme: &Theme) -> egui::Frame {
    egui::Frame::NONE
        .fill(theme.card)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
}

/// 分区小标题，对应参考图里的 `Debug` / `Buttons` / `Controls`。
fn section(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    ui.label(RichText::new(text).size(14.0).strong().color(theme.text));
}

/// 胶囊按钮：选中时填充主色，未选中时透明。
fn pill(ui: &mut egui::Ui, theme: &Theme, text: &str, selected: bool) -> egui::Response {
    let (fill, color) = if selected {
        (theme.accent, theme.accent_text)
    } else {
        (Color32::TRANSPARENT, theme.text)
    };
    ui.add(
        egui::Button::new(RichText::new(text).color(color))
            .fill(fill)
            .corner_radius(16.0)
            .min_size(egui::vec2(96.0, 32.0)),
    )
}

/// 主按钮：主色填充。
fn primary_button(ui: &mut egui::Ui, theme: &Theme, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(theme.accent_text))
            .fill(theme.accent)
            .corner_radius(16.0)
            .min_size(egui::vec2(110.0, 34.0)),
    )
}

/// 次按钮：灰色填充。
fn secondary_button(ui: &mut egui::Ui, theme: &Theme, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(theme.secondary_text))
            .fill(theme.secondary)
            .corner_radius(16.0)
            .min_size(egui::vec2(96.0, 34.0)),
    )
}

/// 左侧导航项：选中时整行胶囊。
fn nav_item(ui: &mut egui::Ui, theme: &Theme, icon: &str, text: &str, selected: bool) -> bool {
    let (fill, color) = if selected {
        (theme.accent, theme.accent_text)
    } else {
        (Color32::TRANSPARENT, theme.text)
    };
    let response = ui.add(
        egui::Button::new(RichText::new(format!("{icon}  {text}")).color(color))
            .fill(fill)
            .corner_radius(18.0)
            .min_size(egui::vec2(212.0, 36.0)),
    );
    response.clicked()
}

// ---------------------------------------------------------------- 状态

#[derive(PartialEq, Eq, Clone, Copy)]
enum Nav {
    Wifi,
    Desktop,
    Upgrade,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Ipv4,
    Ipv6,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Method4 {
    Dhcp,
    Manual,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Method6 {
    Slaac,
    Manual,
}

struct ConfigWindow {
    started: Instant,
    limit: Option<Duration>,
    dark: bool,
    nav: Nav,
    tab: Tab,
    interfaces: Vec<String>,
    selected: usize,
    method4: Method4,
    ip: String,
    netmask: String,
    gateway: String,
    dns1: String,
    dns2: String,
    search: String,
    method6: Method6,
    ip6: String,
    prefix6: String,
    gateway6: String,
    preview: String,
    errors: Vec<String>,
    apply_for_real: bool,
    commands: Vec<(String, String)>,
    resolv: Option<String>,
    status: String,
    wifi_ssid: String,
    wifi_security: WifiSecurity,
    wifi_password: String,
    eap_method: String,
    eap_identity: String,
    eap_password: String,
    eap_phase2: String,
    eap_ca: String,
    wifi_status: String,
    runner: RealRunner,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum WifiSecurity {
    Psk,
    Eap,
    Open,
}

impl Default for ConfigWindow {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            limit: None,
            dark: false,
            nav: Nav::Wifi,
            tab: Tab::Ipv4,
            interfaces: vec!["em0".to_string(), "wlan0".to_string(), "em0.10".to_string()],
            selected: 0,
            method4: Method4::Dhcp,
            ip: String::new(),
            netmask: "255.255.255.0".to_string(),
            gateway: String::new(),
            dns1: String::new(),
            dns2: String::new(),
            search: String::new(),
            method6: Method6::Slaac,
            ip6: String::new(),
            prefix6: "64".to_string(),
            gateway6: String::new(),
            preview: String::new(),
            errors: Vec::new(),
            apply_for_real: false,
            commands: Vec::new(),
            resolv: None,
            status: String::new(),
            wifi_ssid: String::new(),
            wifi_security: WifiSecurity::Psk,
            wifi_password: String::new(),
            eap_method: "PEAP".to_string(),
            eap_identity: String::new(),
            eap_password: String::new(),
            eap_phase2: "MSCHAPV2".to_string(),
            eap_ca: String::new(),
            wifi_status: String::new(),
            runner: RealRunner::new(),
        }
    }
}

impl ConfigWindow {
    fn apply(&mut self) {
        self.errors.clear();
        self.commands.clear();
        self.resolv = None;
        self.status.clear();
        let nic = self.interfaces[self.selected].clone();

        let rc_nic = match validate::interface(&nic) {
            Ok(_) => validate::rc_conf_interface(&nic),
            Err(error) => {
                self.errors.push(error);
                self.preview.clear();
                return;
            }
        };

        match self.method4 {
            Method4::Dhcp => {
                let value = config::ifconfig_value(&nic, config::AddressMethod::Dhcp);
                self.commands.push((format!("ifconfig_{rc_nic}"), value));
            }
            Method4::Manual => {
                let ip = self.check(validate::ipv4_address(&self.ip, "IP"));
                let netmask = self.check(validate::ipv4_netmask(&self.netmask));
                if let (Some(ip), Some(netmask)) = (ip, netmask) {
                    let value = config::ifconfig_value(
                        &nic,
                        config::AddressMethod::Static {
                            inet: &ip,
                            netmask: &netmask,
                        },
                    );
                    self.commands.push((format!("ifconfig_{rc_nic}"), value));
                }
                if !self.gateway.trim().is_empty() {
                    if let Some(gateway) =
                        self.check(validate::ipv4_address(&self.gateway, "Gateway"))
                    {
                        self.commands.push(("defaultrouter".to_string(), gateway));
                    }
                }
                let dns1 = if self.dns1.trim().is_empty() {
                    None
                } else {
                    self.check(validate::ipv4_address(&self.dns1, "DNS Server 1"))
                };
                let dns2 = if self.dns2.trim().is_empty() {
                    None
                } else {
                    self.check(validate::ipv4_address(&self.dns2, "DNS Server 2"))
                };
                let search = if self.search.trim().is_empty() {
                    Some(String::new())
                } else {
                    self.check(validate::search_domains(&self.search))
                };
                if let (Some(dns1), Some(search)) = (dns1, search) {
                    let nameservers: Vec<&str> = [Some(dns1.as_str()), dns2.as_deref()]
                        .into_iter()
                        .flatten()
                        .collect();
                    self.resolv = Some(config::render_resolv_conf(
                        if search.is_empty() {
                            None
                        } else {
                            Some(search.as_str())
                        },
                        &nameservers,
                    ));
                }
            }
        }

        match self.method6 {
            Method6::Slaac => {
                self.commands.push((
                    format!("ifconfig_{rc_nic}_ipv6"),
                    "inet6 accept_rtadv".to_string(),
                ));
                self.commands
                    .push(("rtsold_enable".to_string(), "YES".to_string()));
            }
            Method6::Manual => {
                let address = self.check(validate::ipv6_address(&self.ip6, "IPv6", false));
                let prefix = self.check(validate::ipv6_prefixlen(&self.prefix6));
                if let (Some(address), Some(prefix)) = (address, prefix) {
                    self.commands.push((
                        format!("ifconfig_{rc_nic}_ipv6"),
                        format!("inet6 {address} prefixlen {prefix}"),
                    ));
                }
                if !self.gateway6.trim().is_empty() {
                    if let Some(gateway) =
                        self.check(validate::ipv6_address(&self.gateway6, "Gateway", true))
                    {
                        self.commands
                            .push(("ipv6_defaultrouter".to_string(), gateway));
                    }
                }
            }
        }

        self.preview = self
            .commands
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(resolv) = &self.resolv {
            self.preview.push_str("\n--- /etc/resolv.conf ---\n");
            self.preview.push_str(resolv);
        }

        if !self.apply_for_real || !self.errors.is_empty() {
            return;
        }
        let sys = Sys::new(&self.runner);
        for (name, value) in &self.commands {
            if let Err(error) = sys.sysrc_set(name, value) {
                self.status = format!("FAILED {name}: {error}");
                return;
            }
        }
        if let Some(resolv) = &self.resolv {
            if let Err(error) = std::fs::write("/etc/resolv.conf", resolv) {
                self.status = format!("FAILED /etc/resolv.conf: {error}");
                return;
            }
        }
        // 地址写进 rc.conf 后必须重启该接口才会生效；只重启当前接口，
        // 避免把其他在线连接一起带掉。
        if let Err(error) = sys.netif_restart(Some(&nic)) {
            self.status = format!("FAILED netif restart {nic}: {error}");
            return;
        }
        self.status = networkmgr_core::tr!("Applied %s rc.conf entries")
            .replace("%s", &self.commands.len().to_string());
    }

    /// 把无线凭据写入 daemon 并保存；只有 `--apply` 才真的执行。
    fn save_wifi(&mut self) {
        self.wifi_status.clear();
        let nic = self.interfaces[self.selected].clone();
        if self.wifi_ssid.trim().is_empty() {
            self.wifi_status = "FAILED: SSID is empty".to_string();
            return;
        }
        if !self.apply_for_real {
            self.wifi_status = networkmgr_core::tr!("Dry run (pass --apply to write):").to_string();
            return;
        }
        let sys = Sys::new(&self.runner);
        let result = match self.wifi_security {
            WifiSecurity::Psk => {
                sys.wpa_save_network(&nic, &self.wifi_ssid, "WPA2-PSK", Some(&self.wifi_password))
            }
            WifiSecurity::Open => sys.wpa_save_network(&nic, &self.wifi_ssid, "OPEN", None),
            WifiSecurity::Eap => {
                let eap = networkmgr_core::config::EapConfig {
                    eap_method: Some(self.eap_method.clone()),
                    identity: self.eap_identity.clone(),
                    password: self.eap_password.clone(),
                    phase2: Some(self.eap_phase2.clone()),
                    ca_cert: self.eap_ca.clone(),
                    ..Default::default()
                };
                sys.wpa_save_eap_network(&nic, &self.wifi_ssid, &eap)
            }
        };
        let Some(id) = result else {
            self.wifi_status = format!("FAILED {nic}: network block rejected");
            return;
        };
        let selected = sys.wpa_select_network(&nic, &id);
        let saved = sys.wpa_save_config(&nic);
        self.wifi_status = if selected && saved {
            format!("{} {}", networkmgr_core::tr!("Saved"), self.wifi_ssid)
        } else {
            format!("FAILED {nic}: select={selected} save={saved}")
        };
    }

    fn check(&mut self, result: Result<String, String>) -> Option<String> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.errors.push(error);
                None
            }
        }
    }

    fn body(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Wi-Fi").size(22.0).strong().color(theme.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.apply_for_real {
                    if primary_button(ui, theme, networkmgr_core::tr!("Apply")).clicked() {
                        self.apply();
                    }
                } else if secondary_button(ui, theme, networkmgr_core::tr!("Preview")).clicked() {
                    self.apply();
                }
            });
        });
        ui.label(
            RichText::new(networkmgr_core::tr!(
                "Pick an interface, then choose how it gets its address."
            ))
            .size(12.0)
            .color(theme.muted),
        );
        ui.add_space(12.0);

        card(theme).show(ui, |ui| {
            section(ui, theme, networkmgr_core::tr!("Interface"));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let current = self.interfaces[self.selected].clone();
                egui::ComboBox::from_id_salt("nic")
                    .selected_text(current)
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        for (index, name) in self.interfaces.iter().enumerate() {
                            ui.selectable_value(&mut self.selected, index, name);
                        }
                    });
                ui.add_space(12.0);
                if pill(ui, theme, "IPv4", self.tab == Tab::Ipv4).clicked() {
                    self.tab = Tab::Ipv4;
                }
                if pill(ui, theme, "IPv6", self.tab == Tab::Ipv6).clicked() {
                    self.tab = Tab::Ipv6;
                }
            });
        });

        ui.add_space(12.0);
        card(theme).show(ui, |ui| match self.tab {
            Tab::Ipv4 => {
                section(ui, theme, "IPv4");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if pill(ui, theme, "DHCP", self.method4 == Method4::Dhcp).clicked() {
                        self.method4 = Method4::Dhcp;
                    }
                    let manual = networkmgr_core::tr!("Manual");
                    let picked = self.method4 == Method4::Manual;
                    if pill(ui, theme, manual, picked).clicked() {
                        self.method4 = Method4::Manual;
                    }
                });
                if self.method4 == Method4::Manual {
                    ui.add_space(10.0);
                    egui::Grid::new("ipv4")
                        .num_columns(2)
                        .spacing([18.0, 10.0])
                        .show(ui, |ui| {
                            field(ui, theme, networkmgr_core::tr!("IP address"), &mut self.ip);
                            let subnet = networkmgr_core::tr!("Subnet mask");
                            field(ui, theme, subnet, &mut self.netmask);
                            field(
                                ui,
                                theme,
                                networkmgr_core::tr!("Default gateway"),
                                &mut self.gateway,
                            );
                            let dns1_label = networkmgr_core::tr!("DNS server 1");
                            field(ui, theme, dns1_label, &mut self.dns1);
                            let dns2_label = networkmgr_core::tr!("DNS server 2");
                            field(ui, theme, dns2_label, &mut self.dns2);
                            let search_label = networkmgr_core::tr!("Search domain");
                            field(ui, theme, search_label, &mut self.search);
                        });
                }
            }
            Tab::Ipv6 => {
                section(ui, theme, "IPv6");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if pill(ui, theme, "SLAAC", self.method6 == Method6::Slaac).clicked() {
                        self.method6 = Method6::Slaac;
                    }
                    let manual = networkmgr_core::tr!("Manual");
                    let picked = self.method6 == Method6::Manual;
                    if pill(ui, theme, manual, picked).clicked() {
                        self.method6 = Method6::Manual;
                    }
                });
                if self.method6 == Method6::Manual {
                    ui.add_space(10.0);
                    egui::Grid::new("ipv6")
                        .num_columns(2)
                        .spacing([18.0, 10.0])
                        .show(ui, |ui| {
                            field(
                                ui,
                                theme,
                                networkmgr_core::tr!("IPv6 address"),
                                &mut self.ip6,
                            );
                            let prefix = networkmgr_core::tr!("Prefix length");
                            field(ui, theme, prefix, &mut self.prefix6);
                            field(
                                ui,
                                theme,
                                networkmgr_core::tr!("Default gateway"),
                                &mut self.gateway6,
                            );
                        });
                }
            }
        });

        ui.add_space(12.0);
        card(theme).show(ui, |ui| self.wifi_card(ui, theme));

        if !self.errors.is_empty() {
            ui.add_space(12.0);
            card(theme).show(ui, |ui| {
                section(ui, theme, networkmgr_core::tr!("Validation"));
                ui.add_space(4.0);
                for error in &self.errors {
                    ui.label(RichText::new(error).color(theme.destructive).size(12.0));
                }
            });
        }

        if !self.preview.is_empty() {
            ui.add_space(12.0);
            card(theme).show(ui, |ui| {
                section(ui, theme, networkmgr_core::tr!("Preview"));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(if self.apply_for_real {
                        networkmgr_core::tr!("Will write:")
                    } else {
                        networkmgr_core::tr!("Dry run (pass --apply to write):")
                    })
                    .size(11.0)
                    .color(theme.muted),
                );
                ui.add_space(4.0);
                ui.label(RichText::new(&self.preview).monospace().size(12.0));
            });
        }

        if !self.status.is_empty() {
            ui.add_space(12.0);
            let color = if self.status.starts_with("FAILED") {
                theme.destructive
            } else {
                theme.positive
            };
            ui.label(RichText::new(&self.status).color(color).size(12.0));
        }

        if self
            .limit
            .is_some_and(|limit| self.started.elapsed() >= limit)
        {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Wi-Fi 凭据：PSK / EAP / 开放网络。默认 dry-run，`--apply` 才写 daemon。
    fn wifi_card(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        section(ui, theme, networkmgr_core::tr!("Wi-Fi Network"));
        ui.add_space(6.0);
        field(ui, theme, "SSID", &mut self.wifi_ssid);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if pill(ui, theme, "PSK", self.wifi_security == WifiSecurity::Psk).clicked() {
                self.wifi_security = WifiSecurity::Psk;
            }
            if pill(ui, theme, "EAP", self.wifi_security == WifiSecurity::Eap).clicked() {
                self.wifi_security = WifiSecurity::Eap;
            }
            if pill(ui, theme, "OPEN", self.wifi_security == WifiSecurity::Open).clicked() {
                self.wifi_security = WifiSecurity::Open;
            }
        });
        ui.add_space(6.0);
        match self.wifi_security {
            WifiSecurity::Psk => {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(networkmgr_core::tr!("Password:"))
                            .size(13.0)
                            .color(theme.text),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.wifi_password)
                            .password(true)
                            .desired_width(240.0),
                    );
                });
            }
            WifiSecurity::Eap => {
                egui::Grid::new("eap")
                    .num_columns(2)
                    .spacing([18.0, 10.0])
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(networkmgr_core::tr!("EAP Method:"))
                                .size(13.0)
                                .color(theme.text),
                        );
                        egui::ComboBox::from_id_salt("eap_method")
                            .selected_text(&self.eap_method)
                            .show_ui(ui, |ui| {
                                for method in ["PEAP", "TTLS", "TLS"] {
                                    ui.selectable_value(
                                        &mut self.eap_method,
                                        method.to_string(),
                                        method,
                                    );
                                }
                            });
                        ui.end_row();
                        field(
                            ui,
                            theme,
                            networkmgr_core::tr!("Username:"),
                            &mut self.eap_identity,
                        );
                        ui.label(
                            RichText::new(networkmgr_core::tr!("Password:"))
                                .size(13.0)
                                .color(theme.text),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut self.eap_password)
                                .password(true)
                                .desired_width(240.0),
                        );
                        ui.end_row();
                        ui.label(
                            RichText::new(networkmgr_core::tr!("Inner Auth:"))
                                .size(13.0)
                                .color(theme.text),
                        );
                        egui::ComboBox::from_id_salt("eap_phase2")
                            .selected_text(&self.eap_phase2)
                            .show_ui(ui, |ui| {
                                for method in ["MSCHAPV2", "MSCHAP", "PAP", "CHAP", "GTC", "TLS"] {
                                    ui.selectable_value(
                                        &mut self.eap_phase2,
                                        method.to_string(),
                                        method,
                                    );
                                }
                            });
                        ui.end_row();
                        field(
                            ui,
                            theme,
                            networkmgr_core::tr!("CA Certificate:"),
                            &mut self.eap_ca,
                        );
                    });
            }
            WifiSecurity::Open => {}
        }
        ui.add_space(8.0);
        if primary_button(ui, theme, networkmgr_core::tr!("Apply")).clicked() {
            self.save_wifi();
        }
        if !self.wifi_status.is_empty() {
            ui.label(
                RichText::new(&self.wifi_status)
                    .size(12.0)
                    .color(theme.muted),
            );
        }
    }
}

/// 表单一行：左侧标签、右侧输入框。
fn field(ui: &mut egui::Ui, theme: &Theme, label: &str, value: &mut String) {
    ui.label(RichText::new(label).size(13.0).color(theme.text));
    ui.add(egui::TextEdit::singleline(value).desired_width(240.0));
    ui.end_row();
}

fn placeholder(ui: &mut egui::Ui, theme: &Theme, title: &str, body: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(title).size(22.0).strong().color(theme.text));
    ui.add_space(12.0);
    card(theme).show(ui, |ui| {
        ui.label(RichText::new(body).size(13.0).color(theme.muted));
    });
}

impl ConfigWindow {
    fn sidebar_contents(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        ui.add_space(4.0);
        ui.label(
            RichText::new("NetworkMgr")
                .size(17.0)
                .strong()
                .color(theme.text),
        );
        ui.label(
            RichText::new(networkmgr_core::tr!("System network configuration"))
                .size(11.0)
                .color(theme.muted),
        );
        ui.add_space(14.0);

        if nav_item(ui, theme, "📶", "Wi-Fi", self.nav == Nav::Wifi) {
            self.nav = Nav::Wifi;
        }
        ui.add_space(4.0);
        let desktop = networkmgr_core::tr!("Desktop");
        if nav_item(ui, theme, "🖥", desktop, self.nav == Nav::Desktop) {
            self.nav = Nav::Desktop;
        }
        ui.add_space(4.0);
        if nav_item(
            ui,
            theme,
            "🧩",
            networkmgr_core::tr!("OS Upgrade & Recovery"),
            self.nav == Nav::Upgrade,
        ) {
            self.nav = Nav::Upgrade;
        }

        ui.add_space(18.0);
        ui.label(
            RichText::new(networkmgr_core::tr!("Theme"))
                .size(12.0)
                .color(theme.muted),
        );
        ui.horizontal(|ui| {
            if pill(ui, theme, networkmgr_core::tr!("Light"), !self.dark).clicked() {
                self.dark = false;
            }
            if pill(ui, theme, networkmgr_core::tr!("Dark"), self.dark).clicked() {
                self.dark = true;
            }
        });
    }
}

impl eframe::App for ConfigWindow {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let theme = Theme::for_mode(self.dark);
        apply_visuals(ui.ctx(), &theme);

        // egui 0.37 把 SidePanel 并进了 Panel 且不再提供固定宽度方法，
        // 所以两栏背景自己画，尺寸完全可控，也更贴近参考图。
        let full = ui.max_rect();
        let sidebar_width = 252.0;
        let sidebar_rect =
            egui::Rect::from_min_size(full.min, egui::vec2(sidebar_width, full.height()));
        let body_rect =
            egui::Rect::from_min_max(egui::pos2(full.min.x + sidebar_width, full.min.y), full.max);

        ui.painter().rect_filled(full, 0.0, theme.window);
        ui.painter().rect_filled(sidebar_rect, 0.0, theme.sidebar);

        ui.scope_builder(
            egui::UiBuilder::new().max_rect(sidebar_rect.shrink2(egui::vec2(14.0, 12.0))),
            |ui| self.sidebar_contents(ui, &theme),
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(body_rect.shrink2(egui::vec2(22.0, 16.0))),
            |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| match self.nav {
                    Nav::Wifi => self.body(ui, &theme),
                    Nav::Desktop => placeholder(
                        ui,
                        &theme,
                        networkmgr_core::tr!("Desktop"),
                        networkmgr_core::tr!("Desktop settings arrive with stage 3."),
                    ),
                    Nav::Upgrade => placeholder(
                        ui,
                        &theme,
                        networkmgr_core::tr!("OS Upgrade & Recovery"),
                        networkmgr_core::tr!(
                            "Not part of NetworkMgr; kept to match the reference layout."
                        ),
                    ),
                });
            },
        );
    }
}

/// 窗口图标：64×64 RGBA，由 `src/icons/scalable/apps/networkmgr.svg` 生成。
///
/// 用原始 RGBA 而不是 PNG，是为了不引入图像解码依赖；`IconData` 本来就吃
/// 未预乘的 RGBA 字节。
const WINDOW_ICON: &[u8] = include_bytes!("../../assets/networkmgr-64.rgba");

fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: WINDOW_ICON.to_vec(),
        width: 64,
        height: 64,
    }
}

/// 读取 `--interface <nic>`，用于从托盘打开时预选网卡。
fn parse_interface_arg(args: &[String]) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == "--interface")
        .map(|pair| pair[1].clone())
}

fn main() -> eframe::Result<()> {
    networkmgr_core::i18n::init();
    let args: Vec<String> = std::env::args().collect();
    let requested = parse_interface_arg(&args);
    let limit = std::env::var("SPIKE_SECONDS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs);
    let apply_for_real = args.iter().any(|arg| arg == "--apply");
    // 便于分别截图验收两套主题：NETWORKMGR_THEME=dark
    let dark = std::env::var("NETWORKMGR_THEME")
        .map(|value| value.eq_ignore_ascii_case("dark"))
        .unwrap_or(false);
    // 真实机器上列出实际接口；`ifconfig` 不可用时保留示例列表。
    let runner = RealRunner::new();
    let discovered: Vec<String> = networkmgr_core::sys::Sys::new(&runner)
        .ifconfig_list()
        .split_whitespace()
        .map(str::to_string)
        .filter(|name| networkmgr_core::ifconfig::is_configurable_interface(name))
        .collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 640.0])
            .with_icon(window_icon())
            .with_title(networkmgr_core::tr!("NetworkMgr configuration")),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "networkmgr-config-window",
        options,
        Box::new(move |_cc| {
            let mut window = ConfigWindow {
                limit,
                apply_for_real,
                dark,
                ..Default::default()
            };
            if !discovered.is_empty() {
                window.interfaces = discovered;
                window.selected = 0;
            }
            if let Some(nic) = requested {
                if !window.interfaces.iter().any(|item| item == &nic) {
                    window.interfaces.push(nic.clone());
                }
                if let Some(index) = window.interfaces.iter().position(|item| item == &nic) {
                    window.selected = index;
                }
            }
            Ok(Box::new(window))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> ConfigWindow {
        ConfigWindow::default()
    }

    #[test]
    fn interface_argument_is_parsed() {
        let args: Vec<String> = ["config-window", "--interface", "vtnet0"]
            .iter()
            .map(|item| item.to_string())
            .collect();
        assert_eq!(parse_interface_arg(&args), Some("vtnet0".to_string()));
        assert_eq!(parse_interface_arg(&args[..1]), None);
    }

    #[test]
    fn dhcp_generates_ifconfig_and_slaac() {
        let mut window = window();
        window.apply();
        assert!(window.errors.is_empty(), "{:?}", window.errors);
        assert!(window.preview.contains("ifconfig_em0=DHCP"));
        assert!(window
            .preview
            .contains("ifconfig_em0_ipv6=inet6 accept_rtadv"));
        assert!(window.preview.contains("rtsold_enable=YES"));
    }

    #[test]
    fn manual_ipv4_generates_static_and_resolv() {
        let mut window = window();
        window.method4 = Method4::Manual;
        window.ip = "192.0.2.77".to_string();
        window.gateway = "192.0.2.1".to_string();
        window.dns1 = "192.0.2.53".to_string();
        window.search = "example.org".to_string();
        window.apply();
        assert!(window.errors.is_empty(), "{:?}", window.errors);
        assert!(window
            .preview
            .contains("ifconfig_em0=inet 192.0.2.77 netmask 255.255.255.0"));
        assert!(window.preview.contains("defaultrouter=192.0.2.1"));
        assert!(window.preview.contains("search example.org"));
        assert!(window.preview.contains("nameserver 192.0.2.53"));
    }

    #[test]
    fn invalid_input_is_reported_and_not_previewed() {
        let mut window = window();
        window.method4 = Method4::Manual;
        window.ip = "192.0.2.999".to_string();
        window.apply();
        assert!(!window.errors.is_empty());
        assert!(!window.preview.contains("ifconfig_em0="));
    }

    #[test]
    fn wireless_interface_gets_the_wpa_prefix() {
        let mut window = window();
        window.selected = 1;
        window.apply();
        assert!(window.preview.contains("ifconfig_wlan0=WPA DHCP"));
    }

    #[test]
    fn manual_ipv6_writes_prefixlen_and_gateway() {
        let mut window = window();
        window.method6 = Method6::Manual;
        window.ip6 = "2001:db8::2".to_string();
        window.prefix6 = "64".to_string();
        window.gateway6 = "fe80::1%em0".to_string();
        window.apply();
        assert!(window.errors.is_empty(), "{:?}", window.errors);
        assert!(window
            .preview
            .contains("ifconfig_em0_ipv6=inet6 2001:db8::2 prefixlen 64"));
        assert!(window.preview.contains("ipv6_defaultrouter=fe80::1%em0"));
    }

    #[test]
    fn themes_differ_in_accent_and_background() {
        let light = Theme::light();
        let dark = Theme::dark();
        assert_ne!(light.accent, dark.accent);
        assert_ne!(light.window, dark.window);
        assert!(!light.is_dark);
        assert!(dark.is_dark);
        assert_eq!(Theme::for_mode(false).accent, light.accent);
        assert_eq!(Theme::for_mode(true).accent, dark.accent);
    }
}
