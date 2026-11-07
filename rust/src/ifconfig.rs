//! `ifconfig` / `netstat` 输出的解析。
//!
//! 这些函数只读文本、不执行任何命令，因此可以直接用 FreeBSD 实机录制的样本
//! 在 Linux 宿主上做契约测试。对应 Python 版 `net_api.py` 的 `nics_list` /
//! `default_card`，以及 `src/auto-switch.py` 里的默认路由判断。

/// 从 `ifconfig -a` 输出中提取接口名。
///
/// 接口段落的首行顶格且形如 `vtnet0: flags=...`，其余行都以空白开头。
pub fn interface_names(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| {
            let first = line.chars().next()?;
            if first.is_whitespace() {
                return None;
            }
            line.split_once(':').map(|(name, _)| name)
        })
        .collect()
}

/// 解析 `ifconfig -l ether` 的空白分隔接口名列表。
pub fn ether_interfaces(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// 判断指定接口在其 `ifconfig -a` 段落里是否为 `status: active`。
pub fn interface_is_active(text: &str, interface: &str) -> bool {
    let mut in_section = false;
    for line in text.lines() {
        let top_level = line
            .chars()
            .next()
            .is_some_and(|first| !first.is_whitespace());
        if top_level {
            in_section = line
                .split_once(':')
                .is_some_and(|(name, _)| name == interface);
            continue;
        }
        if in_section && line.trim() == "status: active" {
            return true;
        }
    }
    false
}

/// 从 `netstat -rn` 中取 IPv4 默认路由所在接口。
///
/// 形如 `default  192.168.122.1  UGS  vtnet0`，最后一列即 Netif。
pub fn default_route_interface(text: &str) -> Option<&str> {
    text.lines()
        .find(|line| line.starts_with("default "))
        .and_then(|line| line.split_whitespace().last())
}

/// 从 `ifconfig wlan0` 输出取当前关联的 SSID。
///
/// 形如 `ssid ExampleNet channel 11 (2462 MHz ...) bssid ...`；只取 ssid 后的
/// 第一个词。含空格的 SSID 在 ifconfig 里的表现形式需用 mock 另行覆盖。
pub fn wireless_ssid(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("ssid "))
        .and_then(|rest| rest.split_whitespace().next())
}

/// 从 `ifconfig wlan0` 输出取 BSSID。
pub fn wireless_bssid(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|line| line.split_once("bssid "))
        .and_then(|(_, rest)| rest.split_whitespace().next())
}

/// 判断是否已关联（`status: associated`）。
pub fn wireless_is_associated(text: &str) -> bool {
    text.lines().any(|line| line.trim() == "status: associated")
}

/// 取无线克隆接口的父物理接口，例如 `parent interface: iwn0` → `iwn0`。
pub fn wireless_parent_interface(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("parent interface: "))
        .map(str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 样本录制自 FreeBSD 15.1-RELEASE-p6（libvirt 域 freebsd15），只读采集。
    const IFCONFIG: &str = include_str!("../tests/fixtures/ifconfig-a.txt");
    const ETHER: &str = include_str!("../tests/fixtures/ifconfig-l-ether.txt");
    const NETSTAT: &str = include_str!("../tests/fixtures/netstat-rn.txt");
    const SYSRC: &str = include_str!("../tests/fixtures/sysrc-network.txt");
    // 样本来自实体机 X200-BSD（FreeBSD 15.1-RELEASE-p6），值已脱敏。
    const WLAN: &str = include_str!("../tests/fixtures/ifconfig-wlan0.txt");
    const SYSRC_WIRELESS: &str = include_str!("../tests/fixtures/sysrc-wireless.txt");

    #[test]
    fn parses_real_freebsd_15_interfaces() {
        assert_eq!(interface_names(IFCONFIG), vec!["vtnet0", "lo0"]);
        assert_eq!(ether_interfaces(ETHER), vec!["vtnet0"]);
    }

    #[test]
    fn reads_link_status_and_default_route() {
        assert!(interface_is_active(IFCONFIG, "vtnet0"));
        assert!(!interface_is_active(IFCONFIG, "lo0"));
        assert_eq!(default_route_interface(NETSTAT), Some("vtnet0"));
    }

    #[test]
    fn sysrc_sample_uses_ifconfig_default() {
        assert!(SYSRC.contains("ifconfig_DEFAULT: DHCP"));
        assert!(SYSRC.contains("hostname: freebsd"));
    }

    #[test]
    fn parses_real_associated_wireless_interface() {
        assert_eq!(wireless_ssid(WLAN), Some("ExampleNet"));
        assert_eq!(wireless_bssid(WLAN), Some("00:11:22:33:44:55"));
        assert!(wireless_is_associated(WLAN));
        assert_eq!(wireless_parent_interface(WLAN), Some("iwn0"));
    }

    #[test]
    fn real_rc_conf_uses_wpa_with_static_address() {
        assert!(SYSRC_WIRELESS.contains("ifconfig_wlan0: WPA inet 192.0.2.77"));
        assert!(SYSRC_WIRELESS.contains("wlans_iwn0: wlan0"));
        assert!(SYSRC_WIRELESS.contains("create_args_wlan0"));
    }
}
