//! 阶段 2 骨架：`networkmgr_configuration` 的 eframe 配置窗口。
//!
//! 只做校验与预览，**不写盘、不重启接口**——写盘那一步在阶段 2 完成时再接
//! `sysrc` / `resolv.conf`，本轮先把「界面 + 校验 + 生成内容」跑通，确保
//! IPv4/IPv6 双标签、DHCP/静态切换与校验提示的形状是对的。
//!
//! `SPIKE_SECONDS` 到点自动关闭，便于无人值守截图。

use std::time::{Duration, Instant};

use eframe::egui;
use networkmgr_core::{config, validate};

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
}

impl Default for ConfigWindow {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            limit: None,
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
        }
    }
}

impl ConfigWindow {
    /// 校验当前输入并生成将要写入的 `rc.conf` 行与 `resolv.conf` 内容。
    fn apply(&mut self) {
        self.errors.clear();
        let mut lines: Vec<String> = Vec::new();
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
                lines.push(format!("ifconfig_{rc_nic}={value}"));
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
                    lines.push(format!("ifconfig_{rc_nic}={value}"));
                }
                if !self.gateway.trim().is_empty() {
                    if let Some(gateway) =
                        self.check(validate::ipv4_address(&self.gateway, "Gateway"))
                    {
                        lines.push(format!("defaultrouter={gateway}"));
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
                    lines.push(config::render_resolv_conf(
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
                lines.push(format!("ifconfig_{rc_nic}_ipv6=inet6 accept_rtadv"));
                lines.push("rtsold_enable=YES".to_string());
            }
            Method6::Manual => {
                let address = self.check(validate::ipv6_address(&self.ip6, "IPv6", false));
                let prefix = self.check(validate::ipv6_prefixlen(&self.prefix6));
                if let (Some(address), Some(prefix)) = (address, prefix) {
                    lines.push(format!(
                        "ifconfig_{rc_nic}_ipv6=inet6 {address} prefixlen {prefix}"
                    ));
                }
                if !self.gateway6.trim().is_empty() {
                    if let Some(gateway) =
                        self.check(validate::ipv6_address(&self.gateway6, "Gateway", true))
                    {
                        lines.push(format!("ipv6_defaultrouter={gateway}"));
                    }
                }
            }
        }

        self.preview = lines.join("\n");
    }

    /// 校验一个字段：成功取值，失败记录错误并返回 None。
    fn check(&mut self, result: Result<String, String>) -> Option<String> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.errors.push(error);
                None
            }
        }
    }
}

impl eframe::App for ConfigWindow {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("NetworkMgr configuration (stage 2 skeleton)");
            ui.horizontal(|ui| {
                ui.label("Interface:");
                let current = self.interfaces[self.selected].clone();
                egui::ComboBox::from_id_salt("nic")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (index, name) in self.interfaces.iter().enumerate() {
                            ui.selectable_value(&mut self.selected, index, name);
                        }
                    });
                if ui.button("Validate & preview").clicked() {
                    self.apply();
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Ipv4, "IPv4");
                ui.selectable_value(&mut self.tab, Tab::Ipv6, "IPv6");
            });

            match self.tab {
                Tab::Ipv4 => {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.method4, Method4::Dhcp, "DHCP");
                        ui.radio_value(&mut self.method4, Method4::Manual, "Manual");
                    });
                    if self.method4 == Method4::Manual {
                        egui::Grid::new("ipv4").num_columns(2).show(ui, |ui| {
                            ui.label("IP address");
                            ui.text_edit_singleline(&mut self.ip);
                            ui.end_row();
                            ui.label("Subnet mask");
                            ui.text_edit_singleline(&mut self.netmask);
                            ui.end_row();
                            ui.label("Default gateway");
                            ui.text_edit_singleline(&mut self.gateway);
                            ui.end_row();
                            ui.label("DNS server 1");
                            ui.text_edit_singleline(&mut self.dns1);
                            ui.end_row();
                            ui.label("DNS server 2");
                            ui.text_edit_singleline(&mut self.dns2);
                            ui.end_row();
                            ui.label("Search domain");
                            ui.text_edit_singleline(&mut self.search);
                            ui.end_row();
                        });
                    }
                }
                Tab::Ipv6 => {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.method6, Method6::Slaac, "SLAAC");
                        ui.radio_value(&mut self.method6, Method6::Manual, "Manual");
                    });
                    if self.method6 == Method6::Manual {
                        egui::Grid::new("ipv6").num_columns(2).show(ui, |ui| {
                            ui.label("IPv6 address");
                            ui.text_edit_singleline(&mut self.ip6);
                            ui.end_row();
                            ui.label("Prefix length");
                            ui.text_edit_singleline(&mut self.prefix6);
                            ui.end_row();
                            ui.label("Default gateway");
                            ui.text_edit_singleline(&mut self.gateway6);
                            ui.end_row();
                        });
                    }
                }
            }

            ui.separator();
            for error in &self.errors {
                ui.colored_label(egui::Color32::from_rgb(200, 60, 60), error);
            }
            if !self.preview.is_empty() {
                ui.label("Would write:");
                ui.monospace(&self.preview);
            }

            if self
                .limit
                .is_some_and(|limit| self.started.elapsed() >= limit)
            {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> ConfigWindow {
        ConfigWindow::default()
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
}

fn main() -> eframe::Result<()> {
    let limit = std::env::var("SPIKE_SECONDS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 460.0])
            .with_title("NetworkMgr configuration"),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "networkmgr-config-window",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(ConfigWindow {
                limit,
                ..Default::default()
            }))
        }),
    )
}
