//! 写入 `rc.conf` / `resolv.conf` 之前的值校验，逐条移植 `NetworkMgr/validate.py`。
//!
//! `rc.conf` 是开机时以 root 执行的 shell 脚本，而 `sysrc` 写值时只套一层
//! 双引号、不做转义。Rust 的 `Command` 参数列表能防住命令拼接注入，却防不住
//! 写入值本身变成 shell 代码，所以这里的放行集合必须和 Python 版一样严格。

use std::net::{Ipv4Addr, Ipv6Addr};

pub type Result<T> = std::result::Result<T, String>;

const MAX_SEARCH_DOMAINS: usize = 6;
const MAX_SEARCH_LENGTH: usize = 256;

fn is_digits(text: &str, min: usize, max: usize) -> bool {
    let len = text.len();
    (min..=max).contains(&len) && text.bytes().all(|b| b.is_ascii_digit())
}

/// 检查 FreeBSD 接口名：`[A-Za-z][A-Za-z0-9_.]{0,14}`。
///
/// 对应 IFNAMSIZ 的 15 字符上限；重命名后的接口名可以很自由，这里只放行
/// 在 rc.conf 里安全的字符。
pub fn interface(name: &str) -> Result<String> {
    let bytes = name.as_bytes();
    let valid = (1..=15).contains(&bytes.len())
        && bytes[0].is_ascii_alphabetic()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'.');
    if valid {
        Ok(name.to_string())
    } else {
        Err(format!("\"{name}\" is not a valid interface name."))
    }
}

/// rc.conf 变量名里的接口名形式：`em0.10` 要用 `ifconfig_em0_10`。
///
/// 依据 rc.subr 的 `get_if_var()`，`. - / +` 四种标点都会被换成下划线。
pub fn rc_conf_interface(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '.' | '-' | '/' | '+' => '_',
            _ => c,
        })
        .collect()
}

/// 解析 IPv4 地址，行为对齐 Python `ipaddress.IPv4Address`。
///
/// 标准库的 `Ipv4Addr::from_str` 对前导零的容忍度和 Python 不同，因此手写
/// 四段检查：每段 1–3 位 ASCII 数字、不允许前导零、值不超过 255。
fn parse_ipv4(text: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = text.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (index, part) in parts.iter().enumerate() {
        if !is_digits(part, 1, 3) || (part.len() > 1 && part.starts_with('0')) {
            return None;
        }
        octets[index] = part.parse::<u8>().ok()?;
    }
    Some(Ipv4Addr::from(octets))
}

/// 校验并规范化 IPv4 地址。
pub fn ipv4_address(text: &str, field: &str) -> Result<String> {
    parse_ipv4(text.trim())
        .map(|addr| addr.to_string())
        .ok_or_else(|| format!("{field}: \"{text}\" is not a valid IPv4 address."))
}

/// 校验 IPv4 子网掩码，接受 `24` 或 `255.255.255.0`，返回点分十进制。
///
/// 拒绝主机掩码（如 `0.0.0.255`）与全零掩码：前者不是合法掩码，后者会把
/// 整个互联网放到链路上。
pub fn ipv4_netmask(text: &str) -> Result<String> {
    let value = text.trim();
    let error = || format!("Subnet Mask: \"{text}\" is not a valid netmask.");
    if is_digits(value, 1, 3) {
        let prefix: u32 = value.parse().map_err(|_| error())?;
        if !(1..=32).contains(&prefix) {
            return Err(error());
        }
        let mask = u32::MAX << (32 - prefix);
        return Ok(Ipv4Addr::from(mask).to_string());
    }
    let addr = parse_ipv4(value).ok_or_else(error)?;
    let mask = u32::from(addr);
    let inverted = !mask;
    // inverted 是 2^k - 1 时，原掩码才是连续的 1 后跟连续的 0。
    let contiguous = inverted & inverted.wrapping_add(1) == 0;
    if !contiguous || mask == 0 {
        return Err(error());
    }
    Ok(addr.to_string())
}

/// 校验 IPv6 地址，可选允许 `%接口` 区域（链路本地网关需要）。
pub fn ipv6_address(text: &str, field: &str, allow_scope: bool) -> Result<String> {
    let value = text.trim();
    let error = || format!("{field}: \"{text}\" is not a valid IPv6 address.");
    let (address, scope) = match value.split_once('%') {
        Some((head, tail)) => (head, Some(tail)),
        None => (value, None),
    };
    if value.ends_with('%') || scope.is_some_and(|s| !allow_scope || interface(s).is_err()) {
        return Err(error());
    }
    let parsed: Ipv6Addr = address.parse().map_err(|_| error())?;
    Ok(match scope {
        Some(s) => format!("{}%{}", parsed, s),
        None => parsed.to_string(),
    })
}

/// 校验 DNS 字段，IPv4 或 IPv6 均可。
pub fn ip_address(text: &str, field: &str) -> Result<String> {
    if let Ok(addr) = ipv4_address(text, field) {
        return Ok(addr);
    }
    if let Ok(addr) = ipv6_address(text, field, false) {
        return Ok(addr);
    }
    Err(format!("{field}: \"{text}\" is not a valid IP address."))
}

/// 校验 IPv6 前缀长度，范围 1–128，去掉前导零。
pub fn ipv6_prefixlen(text: &str) -> Result<String> {
    let value = text.trim();
    let error = || format!("Prefix Length: \"{text}\" must be a number from 1 to 128.");
    if !is_digits(value, 1, 3) {
        return Err(error());
    }
    let prefix: u32 = value.parse().map_err(|_| error())?;
    if !(1..=128).contains(&prefix) {
        return Err(error());
    }
    Ok(prefix.to_string())
}

fn is_dns_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

/// 校验空格分隔的搜索域列表，返回单空格连接的规范形式。
///
/// resolv.conf(5) 最多接受 6 个搜索域、合计 256 字符；每个标签为字母数字
/// 与内部连字符，最长 63 字符。仅去掉一个尾部点，`a.com..` 会留下空标签而被拒。
pub fn search_domains(text: &str) -> Result<String> {
    let domains: Vec<&str> = text.split_whitespace().collect();
    for domain in &domains {
        let stripped = domain.strip_suffix('.').unwrap_or(domain);
        if domain.len() > 253 || !stripped.split('.').all(is_dns_label) {
            return Err(format!(
                "Search domains: \"{domain}\" is not a valid domain name."
            ));
        }
    }
    let joined = domains.join(" ");
    if domains.len() > MAX_SEARCH_DOMAINS || joined.len() > MAX_SEARCH_LENGTH {
        return Err(format!(
            "Search domains: at most {MAX_SEARCH_DOMAINS} domains and {MAX_SEARCH_LENGTH} characters are allowed."
        ));
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_names() {
        for good in ["em0", "wlan0", "em0.10", "epair0a", "a"] {
            assert!(interface(good).is_ok(), "{good} should be valid");
        }
        for bad in [
            "",
            "0em",
            "em0-1",
            "-em0",
            "a".repeat(16).as_str(),
            "em 0",
            "em0/1",
        ] {
            assert!(interface(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn rc_conf_punctuation() {
        assert_eq!(rc_conf_interface("em0.10"), "em0_10");
        assert_eq!(rc_conf_interface("a-b/c+d.e"), "a_b_c_d_e");
        assert_eq!(rc_conf_interface("em0"), "em0");
    }

    #[test]
    fn ipv4_addresses() {
        assert_eq!(ipv4_address(" 192.168.1.1 ", "IP").unwrap(), "192.168.1.1");
        for bad in ["", "01.2.3.4", "1.2.3", "1.2.3.256", "1.2.3.4.5", "1.2.3.a"] {
            assert!(
                ipv4_address(bad, "IP").is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn netmasks() {
        assert_eq!(ipv4_netmask("24").unwrap(), "255.255.255.0");
        assert_eq!(ipv4_netmask(" 255.255.255.0 ").unwrap(), "255.255.255.0");
        assert_eq!(ipv4_netmask("32").unwrap(), "255.255.255.255");
        assert_eq!(ipv4_netmask("1").unwrap(), "128.0.0.0");
        for bad in [
            "0",
            "33",
            "0.0.0.0",
            "0.0.0.255",
            "255.0.255.0",
            "abc",
            "24.1",
        ] {
            assert!(ipv4_netmask(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn ipv6_addresses_and_scopes() {
        assert_eq!(
            ipv6_address("2001:db8::1", "GW", false).unwrap(),
            "2001:db8::1"
        );
        assert_eq!(
            ipv6_address("fe80::1%em0", "GW", true).unwrap(),
            "fe80::1%em0"
        );
        assert!(ipv6_address("fe80::1%em0", "GW", false).is_err());
        assert!(ipv6_address("fe80::1%", "GW", true).is_err());
        assert!(ipv6_address("fe80::1%em 0", "GW", true).is_err());
        assert!(ipv6_address("not-an-ip", "GW", false).is_err());
        assert!(ip_address("192.168.1.1", "DNS").is_ok());
        assert!(ip_address("2001:db8::1", "DNS").is_ok());
        assert!(ip_address("nope", "DNS").is_err());
    }

    #[test]
    fn ipv6_prefix_lengths() {
        assert_eq!(ipv6_prefixlen("064").unwrap(), "64");
        assert_eq!(ipv6_prefixlen("128").unwrap(), "128");
        for bad in ["0", "129", "999", "abc", "12a", ""] {
            assert!(ipv6_prefixlen(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn search_domain_lists() {
        assert_eq!(
            search_domains("  example.org   lan. ").unwrap(),
            "example.org lan."
        );
        assert_eq!(search_domains("").unwrap(), "");
        assert!(search_domains("a..com").is_err());
        assert!(search_domains("-bad.com").is_err());
        assert!(search_domains("bad-.com").is_err());
        assert!(search_domains("a b c d e f g").is_err());
        let long = "a".repeat(254);
        assert!(search_domains(&long).is_err());
    }
}
