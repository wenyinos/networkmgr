//! WireGuard 隧道发现与状态判定，移植 `NetworkMgr/wg_api.py`。
//!
//! 纯逻辑：目录列举、`.conf` 去名、`# Name =` 显示名、`wg show` 判活都只处理
//! 输入文本；真正的 `sysrc` / `wg` / `wg-quick` 调用由上层执行。

/// 一条隧道在托盘里的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WgConfig {
    /// `Connected` 或 `Disconnected`。
    pub state: &'static str,
    /// `# Name = ` 注释里的显示名，没有时用设备名。
    pub info: String,
}

/// `wg_dictionary()` 的等价结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WgDictionary {
    /// `wireguard_enable` 的裸值，`YES` 或 `NO`。
    pub service: String,
    /// Python 版恒为空串，保留字段以免 JSON 结构漂移。
    pub default: String,
    /// 设备名 → 状态，按配置文件名排序后插入。
    pub configs: Vec<(String, WgConfig)>,
}

/// `sysrc -n wireguard_enable` 的结果转成 `YES` / `NO`。
///
/// 命令失败（变量没设过，sysrc 报错）与空输出都按 `NO` 处理，与 rc 脚本
/// 第 69 行的 `: ${wireguard_enable="NO"}` 默认值一致。
pub fn service_state(output: Option<&str>) -> String {
    match output {
        Some(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => "NO".to_string(),
    }
}

/// 配置文件名去掉 `.conf` 得到设备名。
///
/// 精确复刻 Python 的 `wgconfig.replace('.conf', '')`：**替换所有出现**，
/// 不只是后缀。`wg.conf.bak` 会变成 `wg.bak`，这是原版行为，改动它会改变
/// 设备名，因此保留并在此注明。
pub fn device_name(file_name: &str) -> String {
    file_name.replace(".conf", "")
}

/// 从配置内容里取显示名：第一行含 `# Name = ` 的等号后半段。
///
/// Python 用 `line.split('=')[1].strip()`，只取第一个等号之后的部分；
/// 这里用 `split_once('=')` 保持同样的截断点。
pub fn display_name(content: &str, fallback: &str) -> String {
    for line in content.lines() {
        if line.contains("# Name = ") {
            if let Some((_, value)) = line.split_once('=') {
                let value = value.trim();
                if !value.is_empty() {
                    return value.to_string();
                }
            }
            break;
        }
    }
    fallback.to_string()
}

/// `wg show <device>` 的输出转成连接状态。
///
/// 原版判据：stdout 为空且 stderr 含 `Unable to access interface: Device not
/// configured` 时算断开，其余都算连接。命令本身不存在时原版会抛异常，这里由
/// `spawn_failed` 显式表达，按断开处理并让调用方记录日志。
pub fn status(stdout: &str, stderr: &str, spawn_failed: bool) -> &'static str {
    if spawn_failed {
        return "Disconnected";
    }
    if stdout.is_empty() && stderr.contains("Unable to access interface: Device not configured") {
        "Disconnected"
    } else {
        "Connected"
    }
}

/// 把目录里的 `(配置文件名, 内容)` 列表组装成字典。
///
/// 调用方负责像 `sorted(os.listdir())` 那样先排序；设备重名时后者覆盖前者，
/// 与 Python 的 `configs[wg_device] = ...` 行为一致。
pub fn build_dictionary(service: &str, configs: &[(String, String, bool)]) -> WgDictionary {
    let mut entries: Vec<(String, WgConfig)> = Vec::with_capacity(configs.len());
    for (file_name, content, connected) in configs {
        let device = device_name(file_name);
        let info = display_name(content, &device);
        let state = if *connected {
            "Connected"
        } else {
            "Disconnected"
        };
        if let Some(existing) = entries.iter_mut().find(|(name, _)| *name == device) {
            existing.1 = WgConfig { state, info };
        } else {
            entries.push((device, WgConfig { state, info }));
        }
    }
    WgDictionary {
        service: service.to_string(),
        default: String::new(),
        configs: entries,
    }
}

/// `wg-quick up|down <device>` 的参数，供上层 `Command` 直接使用。
pub fn wg_quick_args(action: &str, device: &str) -> Vec<String> {
    vec![
        "wg-quick".to_string(),
        action.to_string(),
        device.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        "[Interface]\n",
        "# Name = Home Tunnel\n",
        "PrivateKey = <redacted>\n",
        "Address = 10.66.0.2/32\n",
        "\n",
        "[Peer]\n",
        "PublicKey = <redacted>\n",
        "Endpoint = 192.0.2.10:51820\n",
    );

    #[test]
    fn service_state_falls_back_to_no() {
        assert_eq!(service_state(Some("YES\n")), "YES");
        assert_eq!(service_state(Some("NO")), "NO");
        assert_eq!(service_state(Some("  ")), "NO");
        assert_eq!(service_state(None), "NO");
    }

    #[test]
    fn device_name_strips_every_conf_like_python() {
        assert_eq!(device_name("wg0.conf"), "wg0");
        assert_eq!(device_name("home.conf.conf"), "home");
        // Python 的 str.replace 会替换所有出现，这里保留同样行为
        assert_eq!(device_name("archive.conf.bak"), "archive.bak");
    }

    #[test]
    fn display_name_reads_the_name_comment() {
        assert_eq!(display_name(SAMPLE, "wg0"), "Home Tunnel");
        assert_eq!(display_name("[Interface]\nPrivateKey = x\n", "wg0"), "wg0");
        // 等号后为空时回退，避免显示空菜单项
        assert_eq!(display_name("# Name = \n", "wg0"), "wg0");
        // `a=b` 只在第一个等号处截断
        assert_eq!(display_name("# Name = a=b\n", "wg0"), "a=b");
    }

    #[test]
    fn status_matches_the_python_rule() {
        let not_configured = "Unable to access interface: Device not configured\n";
        assert_eq!(status("", not_configured, false), "Disconnected");
        // stdout 非空即便 stderr 也报了错，原版仍判为 Connected
        assert_eq!(status("peer: ...\n", not_configured, false), "Connected");
        assert_eq!(status("", "", false), "Connected");
        assert_eq!(status("", "", true), "Disconnected");
    }

    #[test]
    fn dictionary_orders_and_shapes_like_python() {
        let entries = vec![
            ("home.conf".to_string(), SAMPLE.to_string(), true),
            (
                "office.conf".to_string(),
                "[Interface]\n".to_string(),
                false,
            ),
        ];
        let dictionary = build_dictionary("YES", &entries);
        assert_eq!(dictionary.service, "YES");
        assert_eq!(dictionary.default, "");
        assert_eq!(dictionary.configs.len(), 2);
        assert_eq!(dictionary.configs[0].0, "home");
        assert_eq!(dictionary.configs[0].1.state, "Connected");
        assert_eq!(dictionary.configs[0].1.info, "Home Tunnel");
        assert_eq!(dictionary.configs[1].0, "office");
        assert_eq!(dictionary.configs[1].1.state, "Disconnected");
        assert_eq!(dictionary.configs[1].1.info, "office");
    }

    #[test]
    fn duplicate_devices_keep_the_last_entry() {
        let entries = vec![
            (
                "wg0.conf".to_string(),
                "# Name = first\n".to_string(),
                false,
            ),
            (
                "wg0.conf".to_string(),
                "# Name = second\n".to_string(),
                true,
            ),
        ];
        let dictionary = build_dictionary("NO", &entries);
        assert_eq!(dictionary.configs.len(), 1);
        assert_eq!(dictionary.configs[0].1.info, "second");
        assert_eq!(dictionary.configs[0].1.state, "Connected");
    }

    #[test]
    fn quick_arguments_are_a_plain_list() {
        assert_eq!(
            wg_quick_args("up", "wg0"),
            vec!["wg-quick".to_string(), "up".to_string(), "wg0".to_string()]
        );
    }
}
