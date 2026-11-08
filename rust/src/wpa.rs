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

/// `wpa_cli add_network` 的响应：最后一行的十进制网络 id。
///
/// 输出可能带 banner，Python 版取 `splitlines()[-1]`，这里保持一致。
/// 不是数字（例如 `FAIL`）时返回 `None`。
pub fn parse_network_id(text: &str) -> Option<String> {
    let last = text.lines().last()?.trim();
    last.parse::<u32>().ok().map(|id| id.to_string())
}

/// 构造一条新网络的 `set_network` 字段，顺序与 Python `save_psk_network` /
/// `save_open_network` 逐项一致。
///
/// 企业级（EAP）不在托盘里收集凭据，返回 `None` 让调用方引导用户去配置窗口。
/// WPA2/WPA3 过渡网络按 RSN 写入：同一口令就能加入，SAE 只影响 daemon 侧。
pub fn connect_fields(
    ssid: &str,
    security: &str,
    psk: Option<&str>,
) -> Option<Vec<(&'static str, String)>> {
    let ssid = quoted(ssid);
    let fields = match security {
        "WPA2-PSK" | "WPA3-SAE" => vec![
            ("ssid", ssid),
            ("key_mgmt", "WPA-PSK".to_string()),
            ("proto", "RSN".to_string()),
            ("psk", plain_quoted(psk?)),
        ],
        "WPA-PSK" => vec![
            ("ssid", ssid),
            ("key_mgmt", "WPA-PSK".to_string()),
            ("proto", "WPA".to_string()),
            ("psk", plain_quoted(psk?)),
        ],
        "WEP" => vec![
            ("ssid", ssid),
            ("key_mgmt", "NONE".to_string()),
            ("wep_tx_keyidx", "0".to_string()),
            ("wep_key0", wep_key_value(psk?)),
        ],
        "OPEN" => vec![("ssid", ssid), ("key_mgmt", "NONE".to_string())],
        _ => return None,
    };
    Some(fields)
}

/// WEP 密钥：10/26/32 位十六进制按 hex 原样写入，其余按 ASCII 加引号。
///
/// wpa_supplicant 对裸值和引号值的解释不同，长度与字符集必须同时匹配，
/// 否则一个纯数字口令会被当成非法 hex 而连接失败。
fn wep_key_value(key: &str) -> String {
    let is_hex =
        matches!(key.len(), 10 | 26 | 32) && key.bytes().all(|byte| byte.is_ascii_hexdigit());
    if is_hex {
        key.to_string()
    } else {
        quoted(key)
    }
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

/// daemon 未上报噪声底时的兜底值，对应 Python `_noise_floor()` 的 `-95`。
pub const DEFAULT_NOISE_FLOOR: i32 = -95;

/// 一条已保存网络，对应 `wpa_cli list_networks` 的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedNetwork {
    pub id: String,
    pub ssid: String,
    pub bssid: String,
    pub flags: String,
}

/// 一条扫描结果，字段与 Python `scan_networks()` 的记录一一对应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanResult {
    pub ssid: String,
    pub bssid: String,
    pub frequency: i32,
    pub level: i32,
    pub noise: i32,
    pub signal: i32,
    pub flags: String,
    pub security: &'static str,
    pub enterprise: bool,
}

/// 解析 `wpa_cli status`，并还原 SSID 的 printf 转义。
///
/// 只覆盖 Python `wpa_status()` 中拿到输出之后的部分；daemon 不可达时调用方
/// 传空文本即可得到空表，与 Python 的 `{}` 行为一致。
pub fn wpa_status(text: &str) -> BTreeMap<String, String> {
    let mut values = parse_key_values(text);
    if let Some(encoded) = values.get("ssid").cloned() {
        values.insert("ssid".to_string(), printf_decode(&encoded));
    }
    values
}

/// 解析 `wpa_cli list_networks`：跳过表头、按 tab 分列、SSID 需 printf 解码。
pub fn wpa_networks(text: &str) -> Vec<SavedNetwork> {
    let mut networks = Vec::new();
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() > 3 {
            networks.push(SavedNetwork {
                id: fields[0].trim().to_string(),
                ssid: printf_decode(fields[1]),
                bssid: fields[2].to_string(),
                flags: fields[3].to_string(),
            });
        }
    }
    networks
}

/// 信号强度百分比，沿用旧版 S:N 算术，**故意不 clamp**，以便沿用同一套图标分档。
pub fn bar_percent(level: i32, noise: i32) -> i32 {
    (level - noise) * 4
}

/// 从 `wpa_cli bss 0` 输出读取噪声底；读不到时调用方用 `DEFAULT_NOISE_FLOOR`。
pub fn noise_floor(bss_output: &str) -> Option<i32> {
    parse_key_values(bss_output)
        .get("noise")?
        .trim()
        .parse::<i32>()
        .ok()
}

/// 解析 `wpa_cli scan_results`。
///
/// 与 Python 版一致：跳过表头、忽略隐藏网络（空 SSID）、同 SSID 保留信号最强
/// 的 AP、`security` 由 flags 推导、`enterprise` 由 `-EAP` 后缀判定。
/// 唯一有意差异：数字字段解析失败时跳过该行，而非像 Python 那样抛未捕获异常。
pub fn scan_networks(text: &str, noise: i32) -> BTreeMap<String, ScanResult> {
    let mut networks = BTreeMap::new();
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 5 {
            continue;
        }
        let ssid = printf_decode(fields[4]);
        if ssid.is_empty() {
            continue;
        }
        let Ok(level) = fields[2].trim().parse::<i32>() else {
            continue;
        };
        let Ok(frequency) = fields[1].trim().parse::<i32>() else {
            continue;
        };
        let signal = bar_percent(level, noise);
        if networks
            .get(&ssid)
            .is_some_and(|existing: &ScanResult| existing.signal >= signal)
        {
            continue;
        }
        let flags = fields[3].to_string();
        let security = security_from_flags(&flags);
        networks.insert(
            ssid.clone(),
            ScanResult {
                ssid,
                bssid: fields[0].to_string(),
                frequency,
                level,
                noise,
                signal,
                flags,
                security,
                enterprise: security.ends_with("-EAP"),
            },
        );
    }
    networks
}

#[cfg(test)]
mod tests {
    use super::*;

    // 样本来自实体机 X200-BSD，值已脱敏。
    const NO_SOCKET: &str = include_str!("../tests/fixtures/wpa-cli-no-socket.txt");
    const CONF_STRUCTURE: &str =
        include_str!("../tests/fixtures/wpa-supplicant-conf-structure.txt");
    // 标准 wpa_cli 输出格式的 mock 样本（不改动实体机，按方案 2 补齐）。
    const STATUS: &str = include_str!("../tests/fixtures/wpa-status.txt");
    const LIST_NETWORKS: &str = include_str!("../tests/fixtures/wpa-list-networks.txt");
    const SCAN_RESULTS: &str = include_str!("../tests/fixtures/wpa-scan-results.txt");
    const BSS_NOISE: &str = include_str!("../tests/fixtures/wpa-bss-noise.txt");

    #[test]
    fn status_decodes_ssid_and_ignores_banner() {
        let status = wpa_status(STATUS);
        assert_eq!(
            status.get("wpa_state").map(String::as_str),
            Some("COMPLETED")
        );
        assert_eq!(status.get("ssid").map(String::as_str), Some("中文 Net"));
        assert_eq!(
            status.get("bssid").map(String::as_str),
            Some("00:11:22:33:44:55")
        );
        assert!(!status.contains_key("Selected interface 'wlan0'"));
        assert!(wpa_status("").is_empty());
    }

    #[test]
    fn list_networks_decodes_escaped_ssids() {
        let networks = wpa_networks(LIST_NETWORKS);
        assert_eq!(networks.len(), 3);
        assert_eq!(networks[0].id, "0");
        assert_eq!(networks[0].ssid, "ExampleNet");
        assert_eq!(networks[0].flags, "[CURRENT]");
        assert_eq!(networks[1].ssid, "中文 Net");
        assert_eq!(networks[2].ssid, "a\"b\\c");
    }

    #[test]
    fn scan_results_keep_strongest_ap_and_classify_security() {
        let noise = noise_floor(BSS_NOISE).unwrap_or(DEFAULT_NOISE_FLOOR);
        assert_eq!(noise, -95);
        let networks = scan_networks(SCAN_RESULTS, noise);
        assert_eq!(networks.len(), 5);
        let home = &networks["ExampleNet"];
        assert_eq!(home.bssid, "00:11:22:33:44:66");
        assert_eq!(home.level, -35);
        assert_eq!(home.signal, bar_percent(-35, -95));
        assert_eq!(home.security, "WPA2-PSK");
        assert!(!home.enterprise);
        assert_eq!(networks["中文 Net"].security, "WPA2-PSK");
        assert_eq!(networks["Corp-EAP"].security, "WPA2-EAP");
        assert!(networks["Corp-EAP"].enterprise);
        assert_eq!(networks["OldNet"].security, "WEP");
        assert_eq!(networks["Guest"].security, "OPEN");
    }

    #[test]
    fn scan_skips_hidden_networks_and_malformed_rows() {
        let mock = concat!(
            "bssid / frequency / signal level / flags / ssid\n",
            "00:11:22:33:44:55\t2412\t-50\t[ESS]\t\n",
            "00:11:22:33:44:56\t2412\tbad\t[ESS]\tBroken\n",
            "00:11:22:33:44:57\t2412\t-50\t[ESS]\tOkay\n",
        );
        let networks = scan_networks(mock, -95);
        assert_eq!(networks.len(), 1);
        assert!(networks.contains_key("Okay"));
    }

    #[test]
    fn bar_percent_is_deliberately_not_clamped() {
        assert_eq!(bar_percent(-35, -95), 240);
        assert_eq!(bar_percent(-95, -95), 0);
        assert!(bar_percent(-10, -95) > 100);
    }

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
            assert!(
                encoded.is_ascii(),
                "encoded must be printable ASCII: {encoded:?}"
            );
            assert_eq!(
                printf_decode(&encoded),
                raw,
                "round trip failed for {raw:?}"
            );
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
        assert_eq!(
            values.get("wpa_state").map(String::as_str),
            Some("COMPLETED")
        );
        assert_eq!(values.get("ssid").map(String::as_str), Some("My Net"));
        assert_eq!(
            values.get("ip_address").map(String::as_str),
            Some("192.168.1.5")
        );
        assert_eq!(values.len(), 3);
    }

    #[test]
    fn add_network_id_comes_from_the_last_line() {
        assert_eq!(parse_network_id("0\n"), Some("0".to_string()));
        assert_eq!(parse_network_id("12"), Some("12".to_string()));
        assert_eq!(parse_network_id("FAIL\n"), None);
        assert_eq!(parse_network_id(""), None);
    }

    #[test]
    fn psk_fields_match_the_python_writer() {
        let fields = connect_fields("中文 Net", "WPA2-PSK", Some("p\"ss")).unwrap();
        assert_eq!(
            fields,
            vec![
                ("ssid", r#"P"\xe4\xb8\xad\xe6\x96\x87 Net""#.to_string()),
                ("key_mgmt", "WPA-PSK".to_string()),
                ("proto", "RSN".to_string()),
                ("psk", "\"p\"ss\"".to_string()),
            ]
        );

        let legacy = connect_fields("Old", "WPA-PSK", Some("secret")).unwrap();
        assert_eq!(legacy[2], ("proto", "WPA".to_string()));

        // 过渡网络按 RSN 写入，口令即可加入。
        let transition = connect_fields("Edge", "WPA3-SAE", Some("secret")).unwrap();
        assert_eq!(transition[1], ("key_mgmt", "WPA-PSK".to_string()));
        assert_eq!(transition[2], ("proto", "RSN".to_string()));
    }

    #[test]
    fn open_and_wep_fields_match_the_python_writer() {
        let open = connect_fields("Guest", "OPEN", None).unwrap();
        assert_eq!(open.len(), 2);
        assert_eq!(open[1], ("key_mgmt", "NONE".to_string()));

        let hex = connect_fields("Old", "WEP", Some("0011223344")).unwrap();
        assert_eq!(hex[3], ("wep_key0", "0011223344".to_string()));
        let ascii = connect_fields("Old", "WEP", Some("abcde")).unwrap();
        assert_eq!(ascii[3], ("wep_key0", "P\"abcde\"".to_string()));
        // 10 位但不是 hex，必须按 ASCII 加引号。
        let not_hex = connect_fields("Old", "WEP", Some("00112233zz")).unwrap();
        assert_eq!(not_hex[3], ("wep_key0", "P\"00112233zz\"".to_string()));
    }

    #[test]
    fn enterprise_and_missing_passphrases_are_rejected() {
        assert!(connect_fields("Corp", "WPA2-EAP", None).is_none());
        assert!(connect_fields("Home", "WPA2-PSK", None).is_none());
    }
}
