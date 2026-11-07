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

#[cfg(test)]
mod tests {
    use super::*;

    // 样本录制自 FreeBSD 15.1-RELEASE-p6（libvirt 域 freebsd15），只读采集。
    const IFCONFIG: &str = include_str!("../tests/fixtures/ifconfig-a.txt");
    const ETHER: &str = include_str!("../tests/fixtures/ifconfig-l-ether.txt");
    const NETSTAT: &str = include_str!("../tests/fixtures/netstat-rn.txt");
    const SYSRC: &str = include_str!("../tests/fixtures/sysrc-network.txt");

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
}
