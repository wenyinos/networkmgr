//! 命令执行层：把纯逻辑层与真实系统隔开。
//!
//! 所有系统命令都经过 [`Runner`] trait：A 类环境用 [`FakeRunner`] 记录调用并返回
//! 预置输出，B/C 类用 [`RealRunner`] 真正执行。这样"写了什么命令、用了什么参数"
//! 也能在 Linux 上断言，不必等到 FreeBSD。
//!
//! 超时用轮询 + 临时文件实现，而不是管道：`Command::output()` 没有超时，
//! 而先 `try_wait` 再读管道可能被写满的缓冲区卡住。输出落到临时文件后，
//! 超时可以直接 kill，不会留下读不完的管道。

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::config;
use crate::ifconfig;
use crate::state::{self, Snapshot, TrayState};
use crate::wpa;

/// 单条命令的默认超时，与方案第 15.1 节一致。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// wpa_supplicant 控制 socket 目录。
pub const WPA_CTRL_DIR: &str = "/var/run/wpa_supplicant";

/// 命令执行失败的原因。
#[derive(Debug)]
pub enum SysError {
    /// 进程起不来（命令不存在、权限不足）。
    Spawn(io::Error),
    /// 超过超时时间，已被 kill。
    Timeout { program: String, seconds: u64 },
    /// 写 `rc.conf` 前被安全校验拒绝，命令没有执行。
    Rejected(config::ConfigError),
}

impl std::fmt::Display for SysError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "failed to run command: {error}"),
            Self::Timeout { program, seconds } => {
                write!(f, "{program} did not finish within {seconds}s")
            }
            Self::Rejected(error) => write!(f, "{error}"),
        }
    }
}

impl From<config::ConfigError> for SysError {
    fn from(error: config::ConfigError) -> Self {
        Self::Rejected(error)
    }
}

impl std::error::Error for SysError {}

/// 一条命令的结果。`success` 为 false 时调用方按原 Python 版的语义处理。
///
/// stdout 与 stderr 分开保存：`wg show` 的判活**只看 stderr** 里有没有
/// `Unable to access interface: Device not configured`，合并成一个流就判不出来了。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub text: String,
    pub error: String,
}

impl CommandOutput {
    fn ok(text: impl Into<String>) -> Self {
        Self {
            success: true,
            text: text.into(),
            error: String::new(),
        }
    }
}

/// 命令执行器。
pub trait Runner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, SysError>;
}

/// 真正执行命令，带超时。
pub struct RealRunner {
    timeout: Duration,
}

impl RealRunner {
    pub fn new() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for RealRunner {
    fn default() -> Self {
        Self::new()
    }
}

static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

impl Runner for RealRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, SysError> {
        let stamp = format!(
            "{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let out_path = std::env::temp_dir().join(format!("networkmgr-{stamp}.out"));
        let err_path = std::env::temp_dir().join(format!("networkmgr-{stamp}.err"));
        let out_file = File::create(&out_path).map_err(SysError::Spawn)?;
        let err_file = File::create(&err_path).map_err(SysError::Spawn)?;

        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out_file))
            .stderr(Stdio::from(err_file))
            .spawn()
            .map_err(SysError::Spawn)?;

        let cleanup = || {
            let _ = fs::remove_file(&out_path);
            let _ = fs::remove_file(&err_path);
        };

        let deadline = Instant::now() + self.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let text = fs::read_to_string(&out_path).unwrap_or_default();
                    let error = fs::read_to_string(&err_path).unwrap_or_default();
                    cleanup();
                    return Ok(CommandOutput {
                        success: status.success(),
                        text,
                        error,
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    cleanup();
                    return Err(SysError::Spawn(error));
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                cleanup();
                return Err(SysError::Timeout {
                    program: program.to_string(),
                    seconds: self.timeout.as_secs(),
                });
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

/// 测试用执行器：按 `program arg1 arg2` 匹配预置输出，并记录全部调用。
pub struct FakeRunner {
    responses: BTreeMap<String, CommandOutput>,
    calls: RefCell<Vec<String>>,
}

impl FakeRunner {
    pub fn new() -> Self {
        Self {
            responses: BTreeMap::new(),
            calls: RefCell::new(Vec::new()),
        }
    }

    /// 预置一条响应，键是 `program arg1 arg2` 形式的完整命令行。
    pub fn expect(mut self, command: &str, success: bool, text: &str) -> Self {
        self.responses.insert(
            command.to_string(),
            CommandOutput {
                success,
                text: text.to_string(),
                error: String::new(),
            },
        );
        self
    }

    /// 预置一条带 stderr 的响应，用于 `wg show` 这类要看错误输出的命令。
    pub fn expect_with_error(mut self, command: &str, text: &str, error: &str) -> Self {
        self.responses.insert(
            command.to_string(),
            CommandOutput {
                success: false,
                text: text.to_string(),
                error: error.to_string(),
            },
        );
        self
    }

    /// 已发生的调用，按顺序。
    pub fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Default for FakeRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner for FakeRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, SysError> {
        let key = std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        self.calls.borrow_mut().push(key.clone());
        Ok(self
            .responses
            .get(&key)
            .cloned()
            .unwrap_or_else(|| CommandOutput::ok("")))
    }
}

/// 面向 NetworkMgr 的系统操作包装。
pub struct Sys<'a, R: Runner> {
    runner: &'a R,
}

impl<'a, R: Runner> Sys<'a, R> {
    pub fn new(runner: &'a R) -> Self {
        Self { runner }
    }

    /// `sysrc name=value`；非法名字或值在发命令前就被拒绝。
    ///
    /// 校验失败返回 [`SysError::Rejected`]，**不会**静默跳过——否则调用方会以为
    /// 写成功，而 `rc.conf` 里什么都没有。
    pub fn sysrc_set(&self, name: &str, value: &str) -> Result<(), SysError> {
        let args = config::sysrc_set_args(name, value)?;
        self.call(&args)
    }

    /// `sysrc -x name`。
    pub fn sysrc_unset(&self, name: &str) -> Result<(), SysError> {
        let args = config::sysrc_unset_args(name)?;
        self.call(&args)
    }

    /// `ifconfig <nic>`，失败时返回空串（与 Python `_ifconfig` 一致）。
    pub fn ifconfig(&self, nic: &str) -> String {
        self.text("ifconfig", &[nic])
    }

    /// `netstat -rn`。
    pub fn netstat_rn(&self) -> String {
        self.text("netstat", &["-rn"])
    }

    /// 执行 `ifconfig <args...>`，返回是否成功。
    pub fn ifconfig_args(&self, args: &[&str]) -> bool {
        self.run("ifconfig", args)
            .is_some_and(|output| output.success)
    }

    /// `wpa_cli -i <nic> status` 解析后的键值；daemon 不可达时为空表。
    pub fn wpa_status(&self, nic: &str) -> BTreeMap<String, String> {
        let text = self.text("wpa_cli", &["-i", nic, "status"]);
        wpa::wpa_status(&text)
    }

    /// `wpa_cli -i <nic> bss current` 的原始输出。
    pub fn wpa_bss_current(&self, nic: &str) -> String {
        self.text("wpa_cli", &["-i", nic, "bss", "current"])
    }

    /// `wpa_cli -i <nic> list_networks` 的原始输出。
    pub fn wpa_list_networks(&self, nic: &str) -> String {
        self.text("wpa_cli", &["-i", nic, "list_networks"])
    }

    /// `wpa_cli -i <nic> scan_results` 的原始输出。
    pub fn wpa_scan_results(&self, nic: &str) -> String {
        self.text("wpa_cli", &["-i", nic, "scan_results"])
    }

    /// 触发一次扫描；`wpa_cli` 返回 OK 才算成功。
    pub fn wpa_scan(&self, nic: &str) -> bool {
        self.run("wpa_cli", &["-i", nic, "scan"])
            .is_some_and(|output| output.success && !output.text.contains("FAIL"))
    }

    /// `wpa_cli add_network`，返回新的网络 id；失败返回 `None`。
    pub fn wpa_add_network(&self, nic: &str) -> Option<String> {
        let output = self.run("wpa_cli", &["-i", nic, "add_network"])?;
        if !output.success || output.text.contains("FAIL") {
            return None;
        }
        wpa::parse_network_id(&output.text)
    }

    /// `wpa_cli set_network <id> <name> <value>`；只有 daemon 回 OK 才算成功。
    pub fn wpa_set_network(&self, nic: &str, id: &str, name: &str, value: &str) -> bool {
        self.wpa_command(nic, &["set_network", id, name, value])
    }

    /// `wpa_cli enable_network <id>`。
    pub fn wpa_enable_network(&self, nic: &str, id: &str) -> bool {
        self.wpa_command(nic, &["enable_network", id])
    }

    /// `wpa_cli enable_network all`：恢复 `select_network` 禁用的其他网络。
    pub fn wpa_enable_all(&self, nic: &str) -> bool {
        self.wpa_command(nic, &["enable_network", "all"])
    }

    /// `wpa_cli select_network <id>`：只选一个网络，其他会被禁用。
    pub fn wpa_select_network(&self, nic: &str, id: &str) -> bool {
        self.wpa_command(nic, &["select_network", id])
    }

    /// `wpa_cli remove_network <id>`。
    pub fn wpa_remove_network(&self, nic: &str, id: &str) -> bool {
        self.wpa_command(nic, &["remove_network", id])
    }

    /// `wpa_cli reconfigure`，让 daemon 重新读取内存中的配置。
    pub fn wpa_reconfigure(&self, nic: &str) -> bool {
        self.wpa_command(nic, &["reconfigure"])
    }

    /// `wpa_cli disconnect`：断开当前关联，保留已保存的网络块。
    pub fn wpa_disconnect(&self, nic: &str) -> bool {
        self.wpa_command(nic, &["disconnect"])
    }

    /// 更新已保存网络的 psk，用于密码输错后的重试。
    ///
    /// 只改 psk，不删除重加，块里的其他字段（scan_ssid、proto 等）保持不变。
    pub fn wpa_update_psk(&self, nic: &str, id: &str, psk: &str) -> bool {
        self.wpa_set_network(nic, id, "psk", &wpa::plain_quoted(psk))
    }

    /// 已保存网络里与 `ssid` 同名的第一个 id，顺序与 `list_networks` 一致。
    pub fn wpa_saved_id(&self, nic: &str, ssid: &str) -> Option<String> {
        wpa::wpa_networks(&self.wpa_list_networks(nic))
            .into_iter()
            .find(|network| network.ssid == ssid)
            .map(|network| network.id)
    }

    /// 删除所有同名网络块。
    ///
    /// 从最大的 id 开始删：`wpa_supplicant` 在删除后会把后面的 id 前移，
    /// 倒序删除可以让一次读到的 id 全部有效，不必边删边重读。
    pub fn wpa_remove_by_ssid(&self, nic: &str, ssid: &str) -> bool {
        let mut ids: Vec<(u32, String)> = wpa::wpa_networks(&self.wpa_list_networks(nic))
            .into_iter()
            .filter(|network| network.ssid == ssid)
            .filter_map(|network| network.id.parse::<u32>().ok().map(|id| (id, network.id)))
            .collect();
        ids.sort_by(|a, b| b.0.cmp(&a.0));
        let mut removed = false;
        for (_, id) in ids {
            if self.wpa_remove_network(nic, &id) {
                removed = true;
            }
        }
        removed
    }

    /// 新建并启用一个 network block，返回 id。
    ///
    /// 任一步失败都会删掉半成品：残留的块会让下一次连接选中错误的口令，
    /// 而且它会以 disabled 状态留在 `wpa_supplicant.conf` 里。
    pub fn wpa_save_network(
        &self,
        nic: &str,
        ssid: &str,
        security: &str,
        psk: Option<&str>,
    ) -> Option<String> {
        let fields = wpa::connect_fields(ssid, security, psk)?;
        self.wpa_save_fields(nic, &fields)
    }

    /// 企业级网络：字段顺序由 `config::eap_network_fields` 决定。
    pub fn wpa_save_eap_network(
        &self,
        nic: &str,
        ssid: &str,
        eap: &config::EapConfig,
    ) -> Option<String> {
        let fields = config::eap_network_fields(ssid, eap);
        self.wpa_save_fields(nic, &fields)
    }

    /// 用已构造好的 `set_network` 字段新建并启用一个 network block。
    ///
    /// 任一步失败都删掉半成品，避免留下一个永远连不上的块。
    pub fn wpa_save_fields(&self, nic: &str, fields: &[config::NetworkField]) -> Option<String> {
        let id = self.wpa_add_network(nic)?;
        for (name, value) in fields {
            if !self.wpa_set_network(nic, &id, name, value) {
                self.wpa_remove_network(nic, &id);
                return None;
            }
        }
        if !self.wpa_enable_network(nic, &id) {
            self.wpa_remove_network(nic, &id);
            return None;
        }
        Some(id)
    }

    /// 等待 `ssid` 真正连上。
    ///
    /// 只看 `wpa_state=COMPLETED` 不够：`select_network` 之后的一小段时间里
    /// daemon 仍会报告上一次的关联，所以 SSID 必须一起匹配。
    pub fn wpa_wait_for_connection(&self, nic: &str, ssid: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.wpa_status(nic);
            if status.get("wpa_state").map(String::as_str) == Some("COMPLETED")
                && status.get("ssid").map(String::as_str) == Some(ssid)
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(500));
        }
    }

    /// `service netif restart [iface]`。
    ///
    /// 指定接口时只重启该接口；不带参数会重启所有接口，实测可能让 SSH
    /// 长时间不恢复，所以调用方必须优先传接口名。
    pub fn netif_restart(&self, iface: Option<&str>) -> Result<(), SysError> {
        let mut args = vec![
            "service".to_string(),
            "netif".to_string(),
            "restart".to_string(),
        ];
        if let Some(iface) = iface {
            args.push(iface.to_string());
        }
        self.call(&args)
    }

    fn wpa_command(&self, nic: &str, args: &[&str]) -> bool {
        let mut full = vec!["-i", nic];
        full.extend_from_slice(args);
        self.run("wpa_cli", &full)
            .is_some_and(|output| output.success && !output.text.contains("FAIL"))
    }

    /// `sysrc -n <name>`，变量没设过时返回空串。
    pub fn sysrc_get(&self, name: &str) -> String {
        self.text("sysrc", &["-n", name])
    }

    /// `wg show <device>` 判活：stdout 空且 stderr 报设备未配置才算断开。
    pub fn wg_connected(&self, device: &str) -> bool {
        match self.runner.run("wg", &["show", device]) {
            Ok(output) => crate::wg::status(&output.text, &output.error, false) == "Connected",
            Err(_) => false,
        }
    }

    /// 读取 WireGuard 配置目录，返回 `(文件名, 内容, 是否在运行)`，按文件名排序。
    pub fn wg_configs(&self, dir: &std::path::Path) -> Vec<(String, String, bool)> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
            .into_iter()
            .filter_map(|name| {
                let content = fs::read_to_string(dir.join(&name)).ok()?;
                let device = crate::wg::device_name(&name);
                Some((name, content, self.wg_connected(&device)))
            })
            .collect()
    }

    /// `wpa_cli -i <nic> save_config`，成功后把配置文件权限压回 0600。
    ///
    /// daemon 自己的 chmod 是 `#ifdef ANDROID`，FreeBSD 上 rename 之后权限由
    /// umask 决定，而文件里全是口令，所以每次都要补一次。
    pub fn wpa_save_config(&self, nic: &str) -> bool {
        let output = self.run("wpa_cli", &["-i", nic, "save_config"]);
        let ok = output.is_some_and(|output| output.success && !output.text.contains("FAIL"));
        if ok {
            restrict_permissions("/etc/wpa_supplicant.conf");
        }
        ok
    }

    /// `service <name> <action>`；忽略退出码，与 Python 的 `check=False` 一致。
    pub fn service(&self, name: &str, action: &str) -> Result<(), SysError> {
        self.call(&["service".to_string(), name.to_string(), action.to_string()])
    }

    /// `service <name> <action> <iface>`，例如 `service netif start em0`。
    pub fn service_iface(&self, name: &str, action: &str, iface: &str) -> Result<(), SysError> {
        self.call(&[
            "service".to_string(),
            name.to_string(),
            action.to_string(),
            iface.to_string(),
        ])
    }

    /// `/etc/pccard_ether <nic> start|startchildren`。
    pub fn pccard_ether(&self, nic: &str, action: &str) -> bool {
        self.run("/etc/pccard_ether", &[nic, action])
            .is_some_and(|output| output.success)
    }

    /// `ifconfig -l ether`：所有以太网接口名。
    pub fn ifconfig_list_ether(&self) -> String {
        self.text("ifconfig", &["-l", "ether"])
    }

    /// `ifconfig -l`：所有接口名，空格分隔。
    pub fn ifconfig_list(&self) -> String {
        self.text("ifconfig", &["-l"])
    }

    /// `chown <owner:group> <path>`，用于把 `wpa_supplicant.conf` 交回 root:wheel。
    pub fn chown(&self, path: &str, owner: &str) -> bool {
        self.run("chown", &[owner, path])
            .is_some_and(|output| output.success)
    }

    /// `wg-quick up|down <device>`。
    pub fn wg_quick(&self, action: &str, device: &str) -> Result<(), SysError> {
        self.call(&crate::wg::wg_quick_args(action, device))
    }

    /// 组装一次完整的托盘状态，等价 `net_api.tray_state()`。
    pub fn tray_state(&self) -> TrayState {
        let netstat = self.netstat_rn();
        let Some(card) = ifconfig::default_route_interface(&netstat).map(str::to_string) else {
            return TrayState::no_card();
        };
        let text = self.ifconfig(&card);

        let (wpa_state, ssid, bssid, level, noise) = if card.contains("wlan") {
            let status = self.wpa_status(&card);
            let bss = wpa::parse_key_values(&self.wpa_bss_current(&card));
            let read = |key: &str| {
                bss.get(key)
                    .and_then(|value| value.trim().parse::<i32>().ok())
            };
            let wpa_state = status.get("wpa_state").cloned();
            let ssid = status.get("ssid").cloned();
            let bssid = status.get("bssid").cloned().unwrap_or_default();
            (wpa_state, ssid, bssid, read("level"), read("noise"))
        } else {
            (None, None, String::new(), None, None)
        };

        state::build(&Snapshot {
            card: Some(&card),
            ifconfig_text: &text,
            wpa_state: wpa_state.as_deref(),
            ssid: ssid.as_deref(),
            bssid: &bssid,
            level,
            noise,
        })
    }

    fn text(&self, program: &str, args: &[&str]) -> String {
        self.run(program, args)
            .map(|output| output.text)
            .unwrap_or_default()
    }

    fn run(&self, program: &str, args: &[&str]) -> Option<CommandOutput> {
        self.runner.run(program, args).ok()
    }

    fn call(&self, args: &[String]) -> Result<(), SysError> {
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        self.runner.run(borrowed[0], &borrowed[1..]).map(|_| ())
    }
}

/// 把敏感文件的权限压回 0600。
#[cfg(unix)]
fn restrict_permissions(path: &str) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysrc_set_issues_the_expected_command() {
        let fake = FakeRunner::new();
        let sys = Sys::new(&fake);
        sys.sysrc_set("ifconfig_em0", "inet 10.0.0.2 netmask 255.255.255.0")
            .unwrap();
        assert_eq!(
            fake.calls(),
            vec!["sysrc ifconfig_em0=inet 10.0.0.2 netmask 255.255.255.0".to_string()]
        );
    }

    #[test]
    fn unsafe_values_are_rejected_before_running_anything() {
        let fake = FakeRunner::new();
        let sys = Sys::new(&fake);
        let error = sys.sysrc_set("ifconfig_em0", "DHCP; reboot").unwrap_err();
        assert!(matches!(error, SysError::Rejected(_)));
        assert!(fake.calls().is_empty());
        assert!(error.to_string().contains("Refusing to write"));
    }

    #[test]
    fn sysrc_unset_uses_the_minus_x_form() {
        let fake = FakeRunner::new();
        let sys = Sys::new(&fake);
        sys.sysrc_unset("defaultrouter").unwrap();
        assert_eq!(fake.calls(), vec!["sysrc -x defaultrouter".to_string()]);
    }

    #[test]
    fn tray_state_walks_the_wired_path() {
        let fake = FakeRunner::new()
            .expect(
                "netstat -rn",
                true,
                "default            192.168.122.1      UGS          vtnet0\n",
            )
            .expect(
                "ifconfig vtnet0",
                true,
                "vtnet0: flags=1008843<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST,LOWER_UP> metric 0 mtu 1500\n\tstatus: active\n\tinet 192.168.122.170 netmask 0xffffff00 broadcast 192.168.122.255\n",
            );
        let state = Sys::new(&fake).tray_state();
        assert_eq!(state.kind, Some("wire"));
        assert_eq!(state.connection, "Connected");
        assert_eq!(
            state.tooltip,
            "inet 192.168.122.170 netmask 255.255.255.0 broadcast 192.168.122.255"
        );
        // 有线路径不应去问 daemon
        assert!(!fake.calls().iter().any(|call| call.starts_with("wpa_cli")));
    }

    #[test]
    fn tray_state_walks_the_wireless_path() {
        let fake = FakeRunner::new()
            .expect(
                "netstat -rn",
                true,
                "default            192.168.122.1      UGS          wlan0\n",
            )
            .expect(
                "ifconfig wlan0",
                true,
                "wlan0: flags=1008843<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST> metric 0 mtu 1500\n\tstatus: associated\n\tinet 192.0.2.77 netmask 0xffffff00 broadcast 192.0.2.255\n",
            )
            .expect(
                "wpa_cli -i wlan0 status",
                true,
                "wpa_state=COMPLETED\nssid=Home\nbssid=00:11:22:33:44:55\n",
            )
            .expect(
                "wpa_cli -i wlan0 bss current",
                true,
                "level=-35\nnoise=-95\n",
            );
        let state = Sys::new(&fake).tray_state();
        assert_eq!(state.kind, Some("wifi"));
        assert_eq!(state.connection, "Connected");
        assert_eq!(state.ssid.as_deref(), Some("Home"));
        assert_eq!(state.signal, Some(240));
    }

    #[test]
    fn tray_state_without_a_default_route_is_stable() {
        let fake = FakeRunner::new().expect("netstat -rn", true, "Internet:\n");
        assert_eq!(Sys::new(&fake).tray_state(), TrayState::no_card());
    }

    #[test]
    fn wg_connected_reads_stderr() {
        let fake = FakeRunner::new().expect_with_error(
            "wg show wg0",
            "",
            "Unable to access interface: Device not configured\n",
        );
        assert!(!Sys::new(&fake).wg_connected("wg0"));

        let up = FakeRunner::new().expect("wg show wg0", true, "peer: abc\n");
        assert!(Sys::new(&up).wg_connected("wg0"));
    }

    #[test]
    fn wpa_scan_reports_fail_output() {
        let ok = FakeRunner::new().expect("wpa_cli -i wlan0 scan", true, "OK\n");
        assert!(Sys::new(&ok).wpa_scan("wlan0"));
        let failed = FakeRunner::new().expect("wpa_cli -i wlan0 scan", true, "FAIL\n");
        assert!(!Sys::new(&failed).wpa_scan("wlan0"));
    }

    #[test]
    fn save_config_reports_failure_on_fail_output() {
        let fake = FakeRunner::new().expect("wpa_cli -i wlan0 save_config", true, "FAIL\n");
        assert!(!Sys::new(&fake).wpa_save_config("wlan0"));
    }

    #[test]
    fn add_network_returns_the_daemon_id() {
        let fake = FakeRunner::new().expect("wpa_cli -i wlan0 add_network", true, "3\n");
        assert_eq!(
            Sys::new(&fake).wpa_add_network("wlan0"),
            Some("3".to_string())
        );

        let failed = FakeRunner::new().expect("wpa_cli -i wlan0 add_network", true, "FAIL\n");
        assert_eq!(Sys::new(&failed).wpa_add_network("wlan0"), None);
    }

    #[test]
    fn save_network_writes_the_python_fields_in_order() {
        let fake = FakeRunner::new()
            .expect("wpa_cli -i wlan0 add_network", true, "3\n")
            .expect(
                "wpa_cli -i wlan0 set_network 3 ssid P\"Home\"",
                true,
                "OK\n",
            )
            .expect(
                "wpa_cli -i wlan0 set_network 3 key_mgmt WPA-PSK",
                true,
                "OK\n",
            )
            .expect("wpa_cli -i wlan0 set_network 3 proto RSN", true, "OK\n")
            .expect(
                "wpa_cli -i wlan0 set_network 3 psk \"secret\"",
                true,
                "OK\n",
            )
            .expect("wpa_cli -i wlan0 enable_network 3", true, "OK\n");
        let id = Sys::new(&fake).wpa_save_network("wlan0", "Home", "WPA2-PSK", Some("secret"));
        assert_eq!(id, Some("3".to_string()));
        assert_eq!(
            fake.calls(),
            vec![
                "wpa_cli -i wlan0 add_network",
                "wpa_cli -i wlan0 set_network 3 ssid P\"Home\"",
                "wpa_cli -i wlan0 set_network 3 key_mgmt WPA-PSK",
                "wpa_cli -i wlan0 set_network 3 proto RSN",
                "wpa_cli -i wlan0 set_network 3 psk \"secret\"",
                "wpa_cli -i wlan0 enable_network 3",
            ]
        );
    }

    #[test]
    fn save_network_removes_the_block_when_a_field_fails() {
        let fake = FakeRunner::new()
            .expect("wpa_cli -i wlan0 add_network", true, "7\n")
            .expect(
                "wpa_cli -i wlan0 set_network 7 ssid P\"Home\"",
                true,
                "OK\n",
            )
            .expect(
                "wpa_cli -i wlan0 set_network 7 key_mgmt WPA-PSK",
                true,
                "OK\n",
            )
            .expect("wpa_cli -i wlan0 set_network 7 proto RSN", true, "OK\n")
            .expect("wpa_cli -i wlan0 set_network 7 psk \"bad\"", true, "FAIL\n");
        assert_eq!(
            Sys::new(&fake).wpa_save_network("wlan0", "Home", "WPA2-PSK", Some("bad")),
            None
        );
        assert!(fake
            .calls()
            .contains(&"wpa_cli -i wlan0 remove_network 7".to_string()));
        assert!(!fake
            .calls()
            .contains(&"wpa_cli -i wlan0 enable_network 7".to_string()));
    }

    #[test]
    fn eap_network_writes_identity_password_and_phase2() {
        let fake = FakeRunner::new()
            .expect("wpa_cli -i wlan0 add_network", true, "4\n")
            .expect(
                "wpa_cli -i wlan0 set_network 4 ssid P\"Corp\"",
                true,
                "OK\n",
            )
            .expect(
                "wpa_cli -i wlan0 set_network 4 key_mgmt WPA-EAP",
                true,
                "OK\n",
            )
            .expect("wpa_cli -i wlan0 set_network 4 eap PEAP", true, "OK\n")
            .expect(
                "wpa_cli -i wlan0 set_network 4 identity P\"alice\"",
                true,
                "OK\n",
            )
            .expect(
                "wpa_cli -i wlan0 set_network 4 password P\"secret\"",
                true,
                "OK\n",
            )
            .expect(
                "wpa_cli -i wlan0 set_network 4 phase2 P\"auth=MSCHAPV2\"",
                true,
                "OK\n",
            )
            .expect("wpa_cli -i wlan0 enable_network 4", true, "OK\n");
        let eap = config::EapConfig {
            eap_method: Some("PEAP".to_string()),
            identity: "alice".to_string(),
            password: "secret".to_string(),
            ..Default::default()
        };
        assert_eq!(
            Sys::new(&fake).wpa_save_eap_network("wlan0", "Corp", &eap),
            Some("4".to_string())
        );
    }

    #[test]
    fn saved_id_and_remove_by_ssid_use_the_list_networks_table() {
        let table = concat!(
            "network id / ssid / bssid / flags\n",
            "0\tHome\tany\t[CURRENT]\n",
            "1\tCafe\tany\t\n",
            "2\tHome\tany\t\n",
        );
        let fake = FakeRunner::new()
            .expect("wpa_cli -i wlan0 list_networks", true, table)
            .expect("wpa_cli -i wlan0 remove_network 2", true, "OK\n")
            .expect("wpa_cli -i wlan0 remove_network 0", true, "OK\n");
        let sys = Sys::new(&fake);
        assert_eq!(sys.wpa_saved_id("wlan0", "Home"), Some("0".to_string()));
        assert_eq!(sys.wpa_saved_id("wlan0", "Missing"), None);
        assert!(sys.wpa_remove_by_ssid("wlan0", "Home"));
        // 倒序删除，避免删掉低 id 后高 id 前移。
        let removals: Vec<_> = fake
            .calls()
            .into_iter()
            .filter(|call| call.starts_with("wpa_cli -i wlan0 remove_network"))
            .collect();
        assert_eq!(
            removals,
            vec![
                "wpa_cli -i wlan0 remove_network 2",
                "wpa_cli -i wlan0 remove_network 0",
            ]
        );
    }

    #[test]
    fn netif_restart_can_target_one_interface() {
        let fake = FakeRunner::new();
        Sys::new(&fake).netif_restart(Some("wlan0")).unwrap();
        assert_eq!(fake.calls(), vec!["service netif restart wlan0"]);

        let all = FakeRunner::new();
        Sys::new(&all).netif_restart(None).unwrap();
        assert_eq!(all.calls(), vec!["service netif restart"]);
    }

    #[test]
    fn ifconfig_list_returns_all_interfaces() {
        let fake = FakeRunner::new().expect("ifconfig -l", true, "em0 lo0 wlan0\n");
        assert_eq!(Sys::new(&fake).ifconfig_list(), "em0 lo0 wlan0\n");
    }

    #[test]
    fn wait_for_connection_requires_the_ssid_to_match() {
        let fake = FakeRunner::new()
            .expect(
                "wpa_cli -i wlan0 status",
                true,
                "wpa_state=COMPLETED\nssid=Old\n",
            )
            .expect(
                "wpa_cli -i wlan0 status",
                true,
                "wpa_state=COMPLETED\nssid=Old\n",
            );
        assert!(!Sys::new(&fake).wpa_wait_for_connection(
            "wlan0",
            "Home",
            Duration::from_millis(600)
        ));
    }

    #[test]
    fn real_runner_times_out_instead_of_hanging() {
        let runner = RealRunner::with_timeout(Duration::from_millis(150));
        let started = Instant::now();
        let error = runner.run("sleep", &["5"]).unwrap_err();
        assert!(matches!(error, SysError::Timeout { .. }));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn real_runner_captures_output_and_status() {
        let runner = RealRunner::with_timeout(Duration::from_secs(5));
        let output = runner.run("sh", &["-c", "printf hi; exit 3"]).unwrap();
        assert_eq!(output.text, "hi");
        assert!(!output.success);
    }
}
