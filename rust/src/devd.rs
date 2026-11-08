//! devd 事件处理，等价于原 `src/setup-nic.py` / `link-up.py` / `auto-switch.py`。
//!
//! devd 以 root 身份调用 `networkmgr-devd <attach|link-up|link-down> <iface>`：
//! - `attach`：声明网卡、安装或补全 `wpa_supplicant.conf`、拉起接口；
//! - `link-up`：恢复 DHCP 或撤销 `link-down` 留下的标记；
//! - `link-down`：默认路由网卡掉线时停掉接口，并把 DHCP 交给仍在线的网卡。
//!
//! 所有系统交互都经过 [`DevdEnv`]，因此这些分支可以在 Linux 上用 [`FakeEnv`]
//! 断言，不必等到 FreeBSD。接口名过滤沿用原脚本的正则语义：不是锚定匹配，
//! 而是「任意位置出现 `前缀+数字`」——devd 传来的名字并不总是干净的接口名。

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::Path;

use crate::sys::{Runner, Sys};

/// 全局无线配置。
pub const WPA_CONF: &str = "/etc/wpa_supplicant.conf";
/// 安装时落地的模板。
pub const WPA_TEMPLATE: &str = "/usr/local/share/networkmgr-rust/wpa_supplicant.conf";
/// `link-down` 留下的运行时标记；`/var/run` 每次开机都会清空。
pub const LINK_DOWN_MARKER: &str = "/var/run/link-down-";

/// 不当作普通网卡处理的前缀，对应原脚本的 `NOT_NICS_REGEX`。
const NOT_NIC_PREFIXES: &[&str] = &[
    "enc", "lo", "fwe", "fwip", "tap", "plip", "pfsync", "pflog", "ipfw", "tun", "sl", "faith",
    "ppp", "bridge", "wg", "wlan",
];

/// 无线驱动前缀，对应 devd.conf 的 `wifi-driver-regex`。
const WIFI_PREFIXES: &[&str] = &[
    "ath", "bwi", "bwn", "ipw", "iwlwifi", "iwi", "iwm", "iwn", "malo", "mwl", "mt79", "otus",
    "ral", "rsu", "rtw", "rtwn", "rum", "run", "uath", "upgt", "ural", "urtw", "wpi", "wtap",
    "zyd",
];

/// `attach` 之后要做的系统操作。
pub trait DevdEnv {
    fn read_file(&self, path: &str) -> Option<String>;
    fn write_file(&self, path: &str, contents: &str) -> bool;
    fn copy_file(&self, from: &str, to: &str) -> bool;
    fn file_exists(&self, path: &str) -> bool;
    fn remove_file(&self, path: &str) -> bool;
    fn chmod_0600(&self, path: &str) -> bool;
    fn chown_root_wheel(&self, path: &str) -> bool;
    fn sysrc_set(&self, name: &str, value: &str) -> bool;
    fn sysrc_get(&self, name: &str) -> String;
    fn ifconfig(&self, nic: &str) -> String;
    fn ifconfig_list_ether(&self) -> String;
    fn ifconfig_up(&self, nic: &str) -> bool;
    fn netstat_rn(&self) -> String;
    fn service(&self, name: &str, action: &str, iface: Option<&str>) -> bool;
    fn pccard_ether(&self, nic: &str, action: &str) -> bool;
}

/// 真正执行系统命令与文件操作。
pub struct RealEnv<'a, R: Runner> {
    sys: Sys<'a, R>,
}

impl<'a, R: Runner> RealEnv<'a, R> {
    pub fn new(runner: &'a R) -> Self {
        Self {
            sys: Sys::new(runner),
        }
    }
}

impl<R: Runner> DevdEnv for RealEnv<'_, R> {
    fn read_file(&self, path: &str) -> Option<String> {
        fs::read_to_string(path).ok()
    }

    fn write_file(&self, path: &str, contents: &str) -> bool {
        fs::write(path, contents).is_ok()
    }

    fn copy_file(&self, from: &str, to: &str) -> bool {
        fs::copy(from, to).is_ok()
    }

    fn file_exists(&self, path: &str) -> bool {
        Path::new(path).exists()
    }

    fn remove_file(&self, path: &str) -> bool {
        fs::remove_file(path).is_ok()
    }

    fn chmod_0600(&self, path: &str) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).is_ok()
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            false
        }
    }

    fn chown_root_wheel(&self, path: &str) -> bool {
        // 按名字解析：FreeBSD 上 wheel 恒为 gid 0，但直接用 chown 更贴近原脚本。
        self.sys.chown(path, "root:wheel")
    }

    fn sysrc_set(&self, name: &str, value: &str) -> bool {
        self.sys.sysrc_set(name, value).is_ok()
    }

    fn sysrc_get(&self, name: &str) -> String {
        self.sys.sysrc_get(name)
    }

    fn ifconfig(&self, nic: &str) -> String {
        self.sys.ifconfig(nic)
    }

    fn ifconfig_list_ether(&self) -> String {
        self.sys.ifconfig_list_ether()
    }

    fn ifconfig_up(&self, nic: &str) -> bool {
        self.sys.ifconfig_args(&[nic, "up"])
    }

    fn netstat_rn(&self) -> String {
        self.sys.netstat_rn()
    }

    fn service(&self, name: &str, action: &str, iface: Option<&str>) -> bool {
        match iface {
            Some(iface) => self.sys.service_iface(name, action, iface).is_ok(),
            None => self.sys.service(name, action).is_ok(),
        }
    }

    fn pccard_ether(&self, nic: &str, action: &str) -> bool {
        self.sys.pccard_ether(nic, action)
    }
}

/// 判断接口名是否落在原脚本的排除集合里。
///
/// 原正则没有锚点，`re.search` 会在任意位置命中；这里保持同样的行为，
/// 避免 devd 传来的 `wlan0`、`vm-bhyve0` 这类名字被当成物理网卡。
pub fn is_not_nic(name: &str) -> bool {
    contains_prefix_digits(name, NOT_NIC_PREFIXES) || contains_vm_letters(name)
}

/// 判断设备名是否属于无线驱动。
pub fn is_wifi_driver(name: &str) -> bool {
    contains_prefix_digits(name, WIFI_PREFIXES)
}

fn contains_prefix_digits(text: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| {
        let mut start = 0;
        while let Some(found) = text[start..].find(prefix) {
            let after = start + found + prefix.len();
            if text[after..].starts_with(|c: char| c.is_ascii_digit()) {
                return true;
            }
            start = after;
        }
        false
    })
}

fn contains_vm_letters(text: &str) -> bool {
    let mut start = 0;
    while let Some(found) = text[start..].find("vm-") {
        let after = start + found + 3;
        if text[after..].starts_with(|c: char| c.is_ascii_lowercase()) {
            return true;
        }
        start = after;
    }
    false
}

/// 拆出所有 `key=value` 行的键与原始行；空行与注释跳过。
pub fn setting_lines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let stripped = line.trim();
            if stripped.is_empty() || stripped.starts_with('#') {
                return None;
            }
            let (key, _) = stripped.split_once('=')?;
            Some((key.trim().to_string(), line.to_string()))
        })
        .collect()
}

/// 把模板里存在、配置文件里缺失的全局设置补到文件开头。
///
/// 返回 `None` 表示没有缺失项，调用方不要重写文件（重写会丢掉注释顺序）。
pub fn add_missing_settings(current: &str, template: &str) -> Option<String> {
    let present: HashSet<String> = setting_lines(current)
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    let missing: Vec<String> = setting_lines(template)
        .into_iter()
        .filter(|(key, _)| !present.contains(key))
        .map(|(_, line)| line)
        .collect();
    if missing.is_empty() {
        None
    } else {
        Some(format!("{}\n{}", missing.join("\n"), current))
    }
}

/// 读取 `rc.conf` 与 `rc.conf.local` 的拼接内容，与原脚本一致。
pub fn rc_conf_content(env: &dyn DevdEnv) -> String {
    let mut content = env.read_file("/etc/rc.conf").unwrap_or_default();
    if env.file_exists("/etc/rc.conf.local") {
        content.push_str(&env.read_file("/etc/rc.conf.local").unwrap_or_default());
    }
    content
}

/// `attach`：新网卡/无线驱动出现时的初始化。
pub fn attach(env: &dyn DevdEnv, nic: &str) {
    if is_not_nic(nic) {
        return;
    }
    let rc_content = rc_conf_content(env);

    if is_wifi_driver(nic) {
        if !env.file_exists(WPA_CONF) {
            if env.copy_file(WPA_TEMPLATE, WPA_CONF) {
                env.chown_root_wheel(WPA_CONF);
                env.chmod_0600(WPA_CONF);
            }
        } else if let (Some(current), Some(template)) =
            (env.read_file(WPA_CONF), env.read_file(WPA_TEMPLATE))
        {
            if let Some(updated) = add_missing_settings(&current, &template) {
                env.write_file(WPA_CONF, &updated);
            }
        }

        if rc_content.contains(&format!("wlans_{nic}=")) {
            env.pccard_ether(nic, "startchildren");
            return;
        }
        // 找第一个尚未使用的 wlanN；原脚本只试 0..8。
        for number in 0..9 {
            let wlan = format!("wlan{number}");
            if !rc_content.contains(&wlan) {
                env.sysrc_set(&format!("wlans_{nic}"), &wlan);
                env.sysrc_set(&format!("ifconfig_{wlan}"), "WPA DHCP");
                break;
            }
        }
        env.pccard_ether(nic, "startchildren");
        // 首次生成 wlan 时它是 down 的，wpa_supplicant 不会自己拉起。
        env.ifconfig_up(nic);
    } else {
        if !rc_content.contains(&format!("ifconfig_{nic}=")) {
            env.sysrc_set(&format!("ifconfig_{nic}"), "DHCP");
        }
        env.pccard_ether(nic, "start");
    }
}

/// `link-up`：网线重新插上时的恢复动作。
pub fn link_up(env: &dyn DevdEnv, nic: &str) {
    if is_not_nic(nic) {
        return;
    }
    let marker = format!("{LINK_DOWN_MARKER}{nic}");
    if env.file_exists(&marker) {
        let text = env.ifconfig(nic);
        if !text.contains("inet ") {
            env.service("netif", "start", Some(nic));
        }
        env.service("routing", "restart", None);
        env.service("dhclient", "restart", Some(nic));
        env.remove_file(&marker);
    } else {
        env.service("dhclient", "quietstart", Some(nic));
    }
}

/// `link-down`：默认路由网卡掉线时把默认路由交给仍在线的网卡。
pub fn link_down(env: &dyn DevdEnv, nic: &str) {
    if is_not_nic(nic) {
        return;
    }
    let defaults: String = env
        .netstat_rn()
        .lines()
        .filter(|line| line.contains("default"))
        .collect::<Vec<_>>()
        .join("\n");
    if !defaults.contains(nic) {
        return;
    }

    let leftover = env.ifconfig_list_ether().replace(nic, "");
    let nic_list: BTreeSet<String> = leftover
        .split_whitespace()
        .filter(|name| !is_not_nic(name))
        .map(str::to_string)
        .collect();
    if nic_list.is_empty() {
        return;
    }

    let info = env.ifconfig(nic);
    let active = info.contains("status: active") || info.contains("status: associated");
    if active {
        return;
    }

    env.service("netif", "stop", Some(nic));
    // 标记「这次是运行时掉线」，link-up 靠它区分开机时的 quietstart。
    env.write_file(&format!("{LINK_DOWN_MARKER}{nic}"), "down");

    let dhcp = env.sysrc_get(&format!("ifconfig_{nic}"));
    if dhcp.trim() == "DHCP" {
        for current in &nic_list {
            let text = env.ifconfig(current);
            let has_status = text.contains("status: active") || text.contains("status: associated");
            let has_inet = text.contains("inet ") || text.contains("inet6");
            if has_status && has_inet {
                env.service("dhclient", "restart", Some(current));
                break;
            }
        }
    } else {
        env.service("routing", "restart", None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeEnv {
        files: RefCell<std::collections::BTreeMap<String, String>>,
        calls: RefCell<Vec<String>>,
        outputs: RefCell<std::collections::BTreeMap<String, String>>,
    }

    impl FakeEnv {
        fn with_file(self, path: &str, contents: &str) -> Self {
            self.files
                .borrow_mut()
                .insert(path.to_string(), contents.to_string());
            self
        }

        fn with_output(self, command: &str, output: &str) -> Self {
            self.outputs
                .borrow_mut()
                .insert(command.to_string(), output.to_string());
            self
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn output(&self, command: &str) -> String {
            self.outputs
                .borrow()
                .get(command)
                .cloned()
                .unwrap_or_default()
        }

        fn record(&self, call: impl Into<String>) {
            self.calls.borrow_mut().push(call.into());
        }
    }

    impl DevdEnv for FakeEnv {
        fn read_file(&self, path: &str) -> Option<String> {
            self.files.borrow().get(path).cloned()
        }

        fn write_file(&self, path: &str, contents: &str) -> bool {
            self.record(format!("write {path}"));
            self.files
                .borrow_mut()
                .insert(path.to_string(), contents.to_string());
            true
        }

        fn copy_file(&self, from: &str, to: &str) -> bool {
            self.record(format!("copy {from} {to}"));
            !from.is_empty()
        }

        fn file_exists(&self, path: &str) -> bool {
            self.files.borrow().contains_key(path)
        }

        fn remove_file(&self, path: &str) -> bool {
            self.record(format!("remove {path}"));
            self.files.borrow_mut().remove(path).is_some()
        }

        fn chmod_0600(&self, path: &str) -> bool {
            self.record(format!("chmod 0600 {path}"));
            true
        }

        fn chown_root_wheel(&self, path: &str) -> bool {
            self.record(format!("chown root:wheel {path}"));
            true
        }

        fn sysrc_set(&self, name: &str, value: &str) -> bool {
            self.record(format!("sysrc {name}={value}"));
            true
        }

        fn sysrc_get(&self, name: &str) -> String {
            self.record(format!("sysrc -n {name}"));
            self.files
                .borrow()
                .get(&format!("sysrc:{name}"))
                .cloned()
                .unwrap_or_default()
        }

        fn ifconfig(&self, nic: &str) -> String {
            self.record(format!("ifconfig {nic}"));
            self.output(&format!("ifconfig {nic}"))
        }

        fn ifconfig_list_ether(&self) -> String {
            self.record("ifconfig -l ether");
            self.output("ifconfig -l ether")
        }

        fn ifconfig_up(&self, nic: &str) -> bool {
            self.record(format!("ifconfig {nic} up"));
            true
        }

        fn netstat_rn(&self) -> String {
            self.record("netstat -rn");
            self.output("netstat -rn")
        }

        fn service(&self, name: &str, action: &str, iface: Option<&str>) -> bool {
            match iface {
                Some(iface) => self.record(format!("service {name} {action} {iface}")),
                None => self.record(format!("service {name} {action}")),
            }
            true
        }

        fn pccard_ether(&self, nic: &str, action: &str) -> bool {
            self.record(format!("/etc/pccard_ether {nic} {action}"));
            true
        }
    }

    #[test]
    fn nic_filters_match_the_original_regexes() {
        for name in ["wlan0", "lo0", "bridge0", "tun1", "vm-bhyve0"] {
            assert!(is_not_nic(name), "{name} should be excluded");
        }
        for name in ["em0", "vtnet0", "igb0", "re0"] {
            assert!(!is_not_nic(name), "{name} should be a real nic");
        }
        assert!(is_wifi_driver("ath0"));
        assert!(is_wifi_driver("iwm0"));
        assert!(is_wifi_driver("ath10k"));
        assert!(!is_wifi_driver("em0"));
    }

    #[test]
    fn missing_settings_are_prepended_once() {
        let template = "ctrl_interface=/var/run/wpa_supplicant\neapol_version=2\n";
        let current = "eapol_version=1\nnetwork={\n}\n";
        let updated = add_missing_settings(current, template).unwrap();
        assert_eq!(
            updated,
            "ctrl_interface=/var/run/wpa_supplicant\neapol_version=1\nnetwork={\n}\n"
        );
        assert!(add_missing_settings(&updated, template).is_none());
    }

    #[test]
    fn wired_attach_declares_dhcp_once() {
        let env = FakeEnv::default().with_file("/etc/rc.conf", "hostname=\"x\"\n");
        attach(&env, "em0");
        assert_eq!(
            env.calls(),
            vec![
                "sysrc ifconfig_em0=DHCP".to_string(),
                "/etc/pccard_ether em0 start".to_string(),
            ]
        );
    }

    #[test]
    fn attach_skips_excluded_names() {
        let env = FakeEnv::default();
        attach(&env, "wlan0");
        attach(&env, "vm-bhyve0");
        assert!(env.calls().is_empty());
    }

    #[test]
    fn wifi_attach_installs_template_and_allocates_a_wlan() {
        let env = FakeEnv::default()
            .with_file("/etc/rc.conf", "hostname=\"x\"\n")
            .with_file(WPA_TEMPLATE, "ctrl_interface=/var/run/wpa_supplicant\n");
        attach(&env, "ath0");
        let calls = env.calls();
        assert!(calls.contains(&format!("copy {WPA_TEMPLATE} {WPA_CONF}")));
        assert!(calls.contains(&format!("chown root:wheel {WPA_CONF}")));
        assert!(calls.contains(&format!("chmod 0600 {WPA_CONF}")));
        assert!(calls.contains(&"sysrc wlans_ath0=wlan0".to_string()));
        assert!(calls.contains(&"sysrc ifconfig_wlan0=WPA DHCP".to_string()));
        assert!(calls.contains(&"/etc/pccard_ether ath0 startchildren".to_string()));
        assert!(calls.contains(&"ifconfig ath0 up".to_string()));
    }

    #[test]
    fn wifi_attach_does_not_reallocate_an_existing_wlan() {
        let env = FakeEnv::default()
            .with_file("/etc/rc.conf", "wlans_ath0=wlan0\n")
            .with_file(WPA_CONF, "ctrl_interface=x\n")
            .with_file(WPA_TEMPLATE, "ctrl_interface=x\n");
        attach(&env, "ath0");
        let calls = env.calls();
        assert!(!calls.iter().any(|call| call.starts_with("sysrc wlans_")));
        assert!(!calls.iter().any(|call| call == "ifconfig ath0 up"));
        assert!(calls.contains(&"/etc/pccard_ether ath0 startchildren".to_string()));
    }

    #[test]
    fn link_up_restores_dhcp_after_a_runtime_drop() {
        let marker = format!("{LINK_DOWN_MARKER}em0");
        let env = FakeEnv::default()
            .with_file(&marker, "down")
            .with_output("ifconfig em0", "em0: flags=8863\n");
        link_up(&env, "em0");
        assert_eq!(
            env.calls(),
            vec![
                "ifconfig em0".to_string(),
                "service netif start em0".to_string(),
                "service routing restart".to_string(),
                "service dhclient restart em0".to_string(),
                format!("remove {marker}"),
            ]
        );
    }

    #[test]
    fn link_up_without_a_marker_only_starts_dhclient() {
        let env = FakeEnv::default();
        link_up(&env, "em0");
        assert_eq!(
            env.calls(),
            vec!["service dhclient quietstart em0".to_string()]
        );
    }

    #[test]
    fn link_down_moves_dhcp_to_the_next_active_nic() {
        let env = FakeEnv::default()
            .with_output("netstat -rn", "default 192.0.2.1 UGS em0\n")
            .with_output("ifconfig -l ether", "em0 em1\n")
            .with_output("ifconfig em0", "status: no carrier\n")
            .with_output("ifconfig em1", "status: active\n\tinet 192.0.2.2\n")
            .with_file("sysrc:ifconfig_em0", "DHCP\n");
        link_down(&env, "em0");
        let calls = env.calls();
        assert!(calls.contains(&"service netif stop em0".to_string()));
        assert!(calls.contains(&format!("write {LINK_DOWN_MARKER}em0")));
        assert!(calls.contains(&"service dhclient restart em1".to_string()));
    }

    #[test]
    fn link_down_ignores_a_nic_that_is_not_the_default_route() {
        let env = FakeEnv::default().with_output("netstat -rn", "default 192.0.2.1 UGS em1\n");
        link_down(&env, "em0");
        assert_eq!(env.calls(), vec!["netstat -rn".to_string()]);
    }
}
