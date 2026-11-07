//! 托盘状态聚合，移植 `net_api.tray_state()` 及其判定函数。
//!
//! 纯逻辑：输入已经取到的 `ifconfig` 块、`wpa_cli status`、`bss current` 与
//! 路由表，输出与 Python 完全一致的结构；命令执行在 `bin/dump-state.rs`。
//! 阶段 1 的验收标准就是两边 JSON 逐字段相同。

use std::fmt::Write as _;

use crate::ifconfig::default_route_interface;
use crate::wpa::{bar_percent, parse_key_values};

/// 无默认路由时的固定文案，等价 Python 的 `"Network card is not enabled"`。
pub const NO_CARD_TOOLTIP: &str = "Network card is not enabled";

/// 接口是否被 `ifconfig down` 关掉了。
///
/// 只看 UP 标志：扫描列表为空不代表网卡关了（net80211 在 INIT 时会清缓存），
/// 而有线的 `status:` 描述的是链路而非接口。没有 flags 行时返回 false，
/// 因为接口根本不存在，谈不上被禁用。
pub fn is_disabled(ifconfig_text: &str) -> bool {
    let Some(flags) = extract_flags(ifconfig_text) else {
        return false;
    };
    !flags.split(',').any(|flag| flag == "UP")
}

/// 从 `flags=1008843<UP,BROADCAST,...>` 里取出尖括号内的部分。
fn extract_flags(ifconfig_text: &str) -> Option<&str> {
    let start = ifconfig_text.find("flags=")?;
    let rest = &ifconfig_text[start..];
    let open = rest.find('<')?;
    let close = rest[open..].find('>')?;
    Some(&rest[open + 1..open + close])
}

/// 是否有 IPv4 地址，等价 `card_has_address`（`'inet ' in text`）。
pub fn has_address(ifconfig_text: &str) -> bool {
    ifconfig_text.contains("inet ")
}

/// 有线口是否插着网线，等价 `card_has_carrier`（`'status: active' in text`）。
pub fn has_carrier(ifconfig_text: &str) -> bool {
    ifconfig_text.contains("status: active")
}

/// 第一条 `inet ` 行（已去两端空白），没有时为空串。
pub fn inet_line(ifconfig_text: &str) -> &str {
    for line in ifconfig_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("inet ") {
            return trimmed;
        }
    }
    ""
}

/// 把 `netmask 0xffffff00` 换成 `255.255.255.0`。
///
/// 与 Python 的唯一差异：原版对 `0x` 后 8 个字符直接 `int(li, 16)`，遇到非
/// 十六进制会抛异常；这里原样返回，避免一个畸形行让托盘刷新失败。
pub fn subnet_hex_to_dec(line: &str) -> String {
    let Some(start) = line.find("0x") else {
        return line.to_string();
    };
    let Some(candidate) = line.get(start..start + 10) else {
        return line.to_string();
    };
    let hex = &candidate[2..];
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return line.to_string();
    }
    let octets: Vec<String> = (0..4)
        .map(|index| {
            u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
                .map(|value| value.to_string())
                .unwrap_or_default()
        })
        .collect();
    line.replace(candidate, &octets.join("."))
}

/// 无线接口的连接状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WirelessState {
    /// `Disabled` / `Connected` / `Disconnected`。
    pub connection: &'static str,
    /// 只在 `Connected` 时有值。
    pub ssid: Option<String>,
}

/// 判定无线接口状态。
///
/// 只有 `wpa_state=COMPLETED` 才算连上：ifconfig 的 `associated` 比握手完成
/// 早约三秒，那时还不能过流量。
pub fn wireless_state(
    ifconfig_text: &str,
    wpa_state: Option<&str>,
    ssid: Option<&str>,
) -> WirelessState {
    if is_disabled(ifconfig_text) {
        return WirelessState {
            connection: "Disabled",
            ssid: None,
        };
    }
    if wpa_state == Some("COMPLETED") {
        return WirelessState {
            connection: "Connected",
            ssid: ssid.map(str::to_string),
        };
    }
    WirelessState {
        connection: "Disconnected",
        ssid: None,
    }
}

/// 判定有线接口状态：Disabled → Unplug（无载波）→ Connected（有地址）→ Disconnected。
pub fn wired_state(ifconfig_text: &str) -> &'static str {
    if is_disabled(ifconfig_text) {
        return "Disabled";
    }
    if !has_carrier(ifconfig_text) {
        return "Unplug";
    }
    if has_address(ifconfig_text) {
        return "Connected";
    }
    "Disconnected"
}

/// `tray_state()` 的返回结构，字段名与顺序都按 Python 的 key 排列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayState {
    pub card: Option<String>,
    /// `wifi` / `wire` / `None`。
    pub kind: Option<&'static str>,
    pub connection: &'static str,
    pub ssid: Option<String>,
    pub signal: Option<i32>,
    pub tooltip: String,
}

impl TrayState {
    /// 没有默认路由时的固定状态。
    pub fn no_card() -> Self {
        Self {
            card: None,
            kind: None,
            connection: "Disconnected",
            ssid: None,
            signal: None,
            tooltip: NO_CARD_TOOLTIP.to_string(),
        }
    }

    /// 输出单行 JSON，字段顺序与 Python dict 一致，便于直接 `diff`。
    pub fn to_json(&self) -> String {
        format!(
            "{{\"card\":{},\"kind\":{},\"connection\":{},\"ssid\":{},\"signal\":{},\"tooltip\":{}}}",
            json_option(self.card.as_deref()),
            json_option(self.kind),
            json_string(self.connection),
            json_option(self.ssid.as_deref()),
            match self.signal {
                Some(value) => value.to_string(),
                None => "null".to_string(),
            },
            json_string(&self.tooltip),
        )
    }
}

/// 把一次刷新需要的输入打包，避免 `build` 出现七个位置参数。
pub struct Snapshot<'a> {
    pub card: Option<&'a str>,
    pub ifconfig_text: &'a str,
    pub wpa_state: Option<&'a str>,
    pub ssid: Option<&'a str>,
    pub bssid: &'a str,
    pub level: Option<i32>,
    pub noise: Option<i32>,
}

impl Default for Snapshot<'_> {
    fn default() -> Self {
        Self {
            card: None,
            ifconfig_text: "",
            wpa_state: None,
            ssid: None,
            bssid: "",
            level: None,
            noise: None,
        }
    }
}

/// 从一份快照组装 `tray_state()`。
pub fn build(snapshot: &Snapshot<'_>) -> TrayState {
    let Some(card) = snapshot.card else {
        return TrayState::no_card();
    };
    let wireless = card.contains("wlan");
    let inet = subnet_hex_to_dec(inet_line(snapshot.ifconfig_text));

    if wireless {
        let state = wireless_state(snapshot.ifconfig_text, snapshot.wpa_state, snapshot.ssid);
        let signal = match (snapshot.level, snapshot.noise) {
            (Some(level), Some(noise)) => Some(bar_percent(level, noise)),
            _ => None,
        };
        let tooltip = if state.connection != "Connected" {
            format!("WiFi {card} not connected")
        } else {
            format!(
                "Signal Strength: {}% \nssid {} bssid {} \n{inet}",
                signal.unwrap_or(0),
                state.ssid.as_deref().unwrap_or(""),
                snapshot.bssid
            )
        };
        return TrayState {
            card: Some(card.to_string()),
            kind: Some("wifi"),
            connection: state.connection,
            ssid: state.ssid,
            signal,
            tooltip,
        };
    }

    TrayState {
        card: Some(card.to_string()),
        kind: Some("wire"),
        connection: wired_state(snapshot.ifconfig_text),
        ssid: None,
        signal: None,
        tooltip: inet,
    }
}

/// 从路由表取默认接口，再按 `Snapshot` 组装。
pub fn from_netstat<'a>(netstat_text: &'a str, snapshot: &Snapshot<'a>) -> TrayState {
    let card = default_route_interface(netstat_text);
    let owned = Snapshot {
        card,
        ifconfig_text: snapshot.ifconfig_text,
        wpa_state: snapshot.wpa_state,
        ssid: snapshot.ssid,
        bssid: snapshot.bssid,
        level: snapshot.level,
        noise: snapshot.noise,
    };
    if owned.card.is_none() {
        return TrayState::no_card();
    }
    build(&owned)
}

/// `bss current` 输出里的 `level` / `noise`，缺字段时返回 None。
pub fn bss_signal(bss_output: &str) -> (Option<i32>, Option<i32>) {
    let values = parse_key_values(bss_output);
    let read = |key: &str| {
        values
            .get(key)
            .and_then(|value| value.trim().parse::<i32>().ok())
    };
    (read("level"), read("noise"))
}

fn json_option(value: Option<&str>) -> String {
    match value {
        Some(value) => json_string(value),
        None => "null".to_string(),
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIRED_UP: &str = concat!(
        "em0: flags=1008843<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST,LOWER_UP> metric 0 mtu 1500\n",
        "\tstatus: active\n",
        "\tinet 192.0.2.77 netmask 0xffffff00 broadcast 192.0.2.255\n",
    );
    const WIRED_DOWN: &str = concat!(
        "em0: flags=1008842<BROADCAST,RUNNING,SIMPLEX,MULTICAST,LOWER_UP> metric 0 mtu 1500\n",
        "\tstatus: active\n",
    );
    const WLAN_UP: &str = concat!(
        "wlan0: flags=1008843<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST> metric 0 mtu 1500\n",
        "\tstatus: associated\n",
        "\tinet 192.0.2.77 netmask 0xffffff00 broadcast 192.0.2.255\n",
    );

    #[test]
    fn disabled_only_looks_at_the_up_flag() {
        assert!(!is_disabled(WIRED_UP));
        assert!(is_disabled(WIRED_DOWN));
        // 没有 flags 行说明接口不存在，不算被禁用
        assert!(!is_disabled(""));
    }

    #[test]
    fn address_and_carrier_are_separate_questions() {
        assert!(has_address(WIRED_UP));
        assert!(!has_address(WIRED_DOWN));
        assert!(has_carrier(WIRED_UP));
        assert!(!has_carrier(""));
    }

    #[test]
    fn inet_line_and_netmask_conversion() {
        assert_eq!(
            inet_line(WIRED_UP),
            "inet 192.0.2.77 netmask 0xffffff00 broadcast 192.0.2.255"
        );
        assert_eq!(inet_line(WIRED_DOWN), "");
        assert_eq!(
            subnet_hex_to_dec("inet 192.0.2.77 netmask 0xffffff00 broadcast 192.0.2.255"),
            "inet 192.0.2.77 netmask 255.255.255.0 broadcast 192.0.2.255"
        );
        assert_eq!(subnet_hex_to_dec("no mask here"), "no mask here");
        assert_eq!(
            subnet_hex_to_dec("netmask 0xzzzzzzzz"),
            "netmask 0xzzzzzzzz"
        );
    }

    #[test]
    fn wired_state_covers_all_four_branches() {
        assert_eq!(wired_state(WIRED_UP), "Connected");
        assert_eq!(wired_state(WIRED_DOWN), "Disabled");
        assert_eq!(wired_state("\tstatus: no carrier\n"), "Unplug");
        assert_eq!(wired_state("\tstatus: active\n"), "Disconnected");
    }

    #[test]
    fn wireless_needs_completed_handshake() {
        let connected = wireless_state(WLAN_UP, Some("COMPLETED"), Some("Home"));
        assert_eq!(connected.connection, "Connected");
        assert_eq!(connected.ssid.as_deref(), Some("Home"));

        // associated 但握手没完成
        let early = wireless_state(WLAN_UP, Some("ASSOCIATING"), Some("Home"));
        assert_eq!(early.connection, "Disconnected");
        assert_eq!(early.ssid, None);

        assert_eq!(
            wireless_state(WIRED_DOWN, Some("COMPLETED"), Some("Home")).connection,
            "Disabled"
        );
    }

    #[test]
    fn tooltip_matches_python_formatting() {
        let state = build(&Snapshot {
            card: Some("wlan0"),
            ifconfig_text: WLAN_UP,
            wpa_state: Some("COMPLETED"),
            ssid: Some("Home"),
            bssid: "00:11:22:33:44:55",
            level: Some(-35),
            noise: Some(-95),
        });
        assert_eq!(state.kind, Some("wifi"));
        assert_eq!(state.signal, Some(240));
        assert_eq!(
            state.tooltip,
            "Signal Strength: 240% \nssid Home bssid 00:11:22:33:44:55 \ninet 192.0.2.77 netmask 255.255.255.0 broadcast 192.0.2.255"
        );

        let offline = build(&Snapshot {
            card: Some("wlan0"),
            ifconfig_text: WLAN_UP,
            ..Default::default()
        });
        assert_eq!(offline.connection, "Disconnected");
        assert_eq!(offline.tooltip, "WiFi wlan0 not connected");
    }

    #[test]
    fn wired_tooltip_is_the_inet_line() {
        let state = build(&Snapshot {
            card: Some("em0"),
            ifconfig_text: WIRED_UP,
            ..Default::default()
        });
        assert_eq!(state.kind, Some("wire"));
        assert_eq!(state.connection, "Connected");
        assert_eq!(
            state.tooltip,
            "inet 192.0.2.77 netmask 255.255.255.0 broadcast 192.0.2.255"
        );
        assert_eq!(state.signal, None);
    }

    #[test]
    fn no_default_route_returns_the_fixed_state() {
        let state = build(&Snapshot::default());
        assert_eq!(state, TrayState::no_card());
        assert_eq!(
            from_netstat("Internet:\n", &Snapshot::default()),
            TrayState::no_card()
        );
    }

    #[test]
    fn json_is_single_line_and_escaped() {
        let state = TrayState {
            card: Some("wlan0".to_string()),
            kind: Some("wifi"),
            connection: "Connected",
            ssid: Some("a\"b".to_string()),
            signal: Some(240),
            tooltip: "line1\nline2\t\\".to_string(),
        };
        assert_eq!(
            state.to_json(),
            r#"{"card":"wlan0","kind":"wifi","connection":"Connected","ssid":"a\"b","signal":240,"tooltip":"line1\nline2\t\\"}"#
        );
        assert_eq!(
            TrayState::no_card().to_json(),
            r#"{"card":null,"kind":null,"connection":"Disconnected","ssid":null,"signal":null,"tooltip":"Network card is not enabled"}"#
        );
    }

    #[test]
    fn json_matches_the_python_baseline() {
        // 基准由 Python 原版 net_api.tray_state() 配合
        // json.dumps(ensure_ascii=False, separators=(',', ':')) 生成，逐字节比对。
        let cases: Vec<(TrayState, &str)> = vec![
            (
                build(&Snapshot {
                    card: Some("wlan0"),
                    ifconfig_text: WLAN_UP,
                    wpa_state: Some("COMPLETED"),
                    ssid: Some("Home"),
                    bssid: "00:11:22:33:44:55",
                    level: Some(-35),
                    noise: Some(-95),
                }),
                r#"{"card":"wlan0","kind":"wifi","connection":"Connected","ssid":"Home","signal":240,"tooltip":"Signal Strength: 240% \nssid Home bssid 00:11:22:33:44:55 \ninet 192.0.2.77 netmask 255.255.255.0 broadcast 192.0.2.255"}"#,
            ),
            (
                build(&Snapshot {
                    card: Some("wlan0"),
                    ifconfig_text: WLAN_UP,
                    wpa_state: Some("ASSOCIATING"),
                    ..Default::default()
                }),
                r#"{"card":"wlan0","kind":"wifi","connection":"Disconnected","ssid":null,"signal":null,"tooltip":"WiFi wlan0 not connected"}"#,
            ),
            (
                build(&Snapshot {
                    card: Some("em0"),
                    ifconfig_text: WIRED_UP,
                    ..Default::default()
                }),
                r#"{"card":"em0","kind":"wire","connection":"Connected","ssid":null,"signal":null,"tooltip":"inet 192.0.2.77 netmask 255.255.255.0 broadcast 192.0.2.255"}"#,
            ),
            (
                build(&Snapshot {
                    card: Some("em0"),
                    ifconfig_text: WIRED_DOWN,
                    ..Default::default()
                }),
                r#"{"card":"em0","kind":"wire","connection":"Disabled","ssid":null,"signal":null,"tooltip":""}"#,
            ),
            (
                TrayState::no_card(),
                r#"{"card":null,"kind":null,"connection":"Disconnected","ssid":null,"signal":null,"tooltip":"Network card is not enabled"}"#,
            ),
        ];
        for (state, expected) in cases {
            assert_eq!(state.to_json(), expected);
        }
    }

    #[test]
    fn bss_signal_reads_level_and_noise() {
        let sample = "bssid=00:11:22:33:44:55\nlevel=-45\nnoise=-95\n";
        assert_eq!(bss_signal(sample), (Some(-45), Some(-95)));
        assert_eq!(bss_signal(""), (None, None));
        assert_eq!(bss_signal("level=bad\nnoise=-95\n"), (None, Some(-95)));
    }
}
