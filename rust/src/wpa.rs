//! wpa_supplicant 控制协议的字符串处理。
//!
//! 逐字节复刻 `NetworkMgr/net_api.py` 的 `_printf_encode` / `_printf_decode` /
//! `_quoted` / `_plain_quoted` / `security_from_flags` / `_parse_key_values`。
//! SSID 里的引号、反斜杠或非 ASCII 字符一旦处理有偏差，表现是「扫得到却
//! 连不上」这种静默失败，所以这些函数都带往返测试。

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// 把 wpa_supplicant 的 printf_encode 输出还原成真实字符串。
///
/// 与 Python 版唯一有意的差异：`\x` 后不是两位十六进制数时，Python 会抛
/// `ValueError`（未捕获），这里改为按字面保留，避免一个畸形 beacon 让后台
/// 线程 panic。非法 UTF-8 字节用 U+FFFD 替换，等价于 `decode('utf-8','replace')`。
pub fn printf_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        let simple = match marker {
            b'"' => Some(b'"'),
            b'\\' => Some(b'\\'),
            b'e' => Some(0x1b),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            _ => None,
        };
        if let Some(byte) = simple {
            out.push(byte);
            i += 2;
        } else if marker == b'x' && i + 3 < bytes.len() {
            match (hex_digit(bytes[i + 2]), hex_digit(bytes[i + 3])) {
                (Some(hi), Some(lo)) => {
                    out.push(hi * 16 + lo);
                    i += 4;
                }
                _ => {
                    out.push(b'\\');
                    i += 1;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 按 wpa_supplicant 的 printf_encode() 转义，结果恒为可打印 ASCII。
pub fn printf_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x1b => out.push_str("\\e"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            32..=126 => out.push(byte as char),
            _ => {
                let _ = write!(out, "\\x{byte:02x}");
            }
        }
    }
    out
}

/// `set_network` 用的 `P"..."` 形式，daemon 会先做一次 printf_decode。
pub fn quoted(value: &str) -> String {
    format!("P\"{}\"", printf_encode(value))
}

/// `psk` 等只接受普通引号的字段，wpa_config_parse_psk 不解码转义。
pub fn plain_quoted(value: &str) -> String {
    format!("\"{value}\"")
}

/// 从扫描结果的 flags 列判断安全类型。
///
/// PSK 必须排在 SAE 之前：WPA2/WPA3 过渡 AP 会同时广播
/// `[WPA2-PSK+SAE-CCMP]`，用口令即可加入，而 SAE 要求 daemon 带相应支持。
pub fn security_from_flags(flags: &str) -> &'static str {
    let modern = flags.contains("WPA2") || flags.contains("RSN");
    if flags.contains("EAP") {
        return if modern { "WPA2-EAP" } else { "WPA-EAP" };
    }
    if flags.contains("PSK") {
        return if modern { "WPA2-PSK" } else { "WPA-PSK" };
    }
    if flags.contains("SAE") {
        return "WPA3-SAE";
    }
    if flags.contains("WEP") {
        return "WEP";
    }
    "OPEN"
}

/// 把 `wpa_cli status` 之类的 key=value 输出转成 map。
///
/// 没有等号的行（例如开头的 `Selected interface 'wlan0'` 横幅）直接忽略，
/// 键值两端去空白。用 BTreeMap 是为了输出顺序稳定，便于契约 diff。
pub fn parse_key_values(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    // 样本来自实体机 X200-BSD，值已脱敏。
    const NO_SOCKET: &str = include_str!("../tests/fixtures/wpa-cli-no-socket.txt");
    const CONF_STRUCTURE: &str =
        include_str!("../tests/fixtures/wpa-supplicant-conf-structure.txt");

    #[test]
    fn real_host_without_ctrl_interface_makes_wpa_cli_unusable() {
        // 生产路径靠 wpa_cli() 的错误返回判定，这里记录诊断文本与根因，
        // 避免以后把「配置里没有 ctrl_interface」误判成 daemon 未运行。
        assert!(NO_SOCKET.contains("Failed to connect to non-global ctrl_ifname"));
        assert!(!CONF_STRUCTURE.contains("ctrl_interface"));
        assert!(CONF_STRUCTURE.contains("network="));
    }

    #[test]
    fn plain_ascii_is_unchanged() {
        assert_eq!(printf_encode("Guest"), "Guest");
        assert_eq!(printf_decode("Guest"), "Guest");
    }

    #[test]
    fn round_trip_special_chars_and_unicode() {
        for raw in [
            "a\"b",
            "a\\b",
            "line\nbreak",
            "tab\there",
            "esc\u{1b}end",
            "中文 SSID",
            "café",
            "引号\"和\\反斜杠",
            "a\rb\tc\nd",
        ] {
            let encoded = printf_encode(raw);
            assert!(encoded.is_ascii(), "encoded must be printable ASCII: {encoded:?}");
            assert_eq!(printf_decode(&encoded), raw, "round trip failed for {raw:?}");
        }
    }

    #[test]
    fn decode_matches_wpa_supplicant_escapes() {
        assert_eq!(printf_decode(r"a\x22b"), "a\"b");
        assert_eq!(printf_decode(r"a\eb"), "a\u{1b}b");
        assert_eq!(printf_decode(r"a\nb"), "a\nb");
        assert_eq!(printf_decode(r"\xe4\xb8\xad"), "中");
        assert_eq!(printf_decode(r"ABC\x41"), "ABCA");
        // 尾部孤立的反斜杠按字面保留
        assert_eq!(printf_decode("abc\\"), "abc\\");
    }

    #[test]
    fn encode_matches_python_byte_escapes() {
        assert_eq!(printf_encode("中"), r"\xe4\xb8\xad");
        assert_eq!(printf_encode("a\"b\\c"), r#"a\"b\\c"#);
        assert_eq!(printf_encode("\n\r\t"), r"\n\r\t");
        assert_eq!(printf_encode("\u{1b}"), r"\e");
    }

    #[test]
    fn invalid_utf8_becomes_replacement_character() {
        assert_eq!(printf_decode(r"\xff"), "\u{fffd}");
    }

    #[test]
    fn malformed_hex_is_kept_literally_instead_of_panicking() {
        assert_eq!(printf_decode(r"a\xZZ"), r"a\xZZ");
    }

    #[test]
    fn quoted_forms() {
        assert_eq!(quoted("中 文"), r#"P"\xe4\xb8\xad \xe6\x96\x87""#);
        assert_eq!(plain_quoted("p\"ss"), "\"p\"ss\"");
    }

    #[test]
    fn security_flags_priority() {
        assert_eq!(security_from_flags("[WPA2-PSK+SAE-CCMP][ESS]"), "WPA2-PSK");
        assert_eq!(security_from_flags("[WPA2-EAP-CCMP][ESS]"), "WPA2-EAP");
        assert_eq!(security_from_flags("[WPA-EAP-TKIP]"), "WPA-EAP");
        assert_eq!(security_from_flags("[WPA-PSK-TKIP]"), "WPA-PSK");
        assert_eq!(security_from_flags("[WPA3-SAE-CCMP][ESS]"), "WPA3-SAE");
        assert_eq!(security_from_flags("[WEP][ESS]"), "WEP");
        assert_eq!(security_from_flags("[ESS]"), "OPEN");
    }

    #[test]
    fn parse_key_values_ignores_banner() {
        let out = "Selected interface 'wlan0'\nwpa_state=COMPLETED\nssid=My Net\nip_address=192.168.1.5\n";
        let values = parse_key_values(out);
        assert_eq!(values.get("wpa_state").map(String::as_str), Some("COMPLETED"));
        assert_eq!(values.get("ssid").map(String::as_str), Some("My Net"));
        assert_eq!(values.get("ip_address").map(String::as_str), Some("192.168.1.5"));
        assert_eq!(values.len(), 3);
    }
}
