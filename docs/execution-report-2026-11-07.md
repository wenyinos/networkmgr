# 今日自主任务报告（2026-11-07）

目标：按 `docs/RUST_REWRITE_PLAN.md` 第 6 节推进 **阶段 1（逻辑层 Rust 化）** 的全部剩余任务。
分支 `feat/rust-rewrite`，验收环境 A 类（本机 Fedora 契约测试）+ B 类（`freebsd15` VM，仅用于 GUI spike）。

## 一、今日任务清单

| # | 任务 | 类型 | 预计产出 | 完成标准 | 状态 |
|---|---|---|---|---|---|
| 1 | 移植 `config`：`rc.conf` 安全写值、`resolv.conf` 生成/解析、PSK/WEP/Open/EAP 字段 | 构建 | `rust/src/config.rs` + 单测 | `cargo test` 全绿；字段顺序与 Python 原版逐项一致 | ✅ 完成 |
| 2 | 移植 `wg`：隧道发现、`# Name =` 显示名、`wg show` 判活、`wg-quick` 参数 | 构建 | `rust/src/wg.rs` + 单测 | ≥5 用例覆盖 4 个函数；`Connected`/`Disconnected` 判据与 `wg_api.py` 一致 | ✅ 完成 |
| 3 | 实现 `state`：`tray_state()` 等价聚合 + 稳定 JSON + `--dump-state` | 构建 | `rust/src/state.rs`、`rust/src/bin/dump-state.rs` | 5 种状态输出与 Python `json.dumps` 逐字节相同 | ✅ 完成（A 类） |
| 4 | 实现 `i18n`：纯 Rust `.mo` 读取器 | 构建 | `rust/src/i18n.rs` | 能读出简中 `.mo`，缺文件回退原文 | ⏸ 未执行 |
| 5 | 写执行报告、更新方案文档、生成看板并提交 | 写作 | 本文件 + 方案第 21 节 + `kanban.html` | 报告四段齐全；提交推送成功 | ✅ 完成 |

## 二、执行结果

### 1. `config.rs`（P0，安全红线）

- 产出：`rust/src/config.rs`（592 行，含 13 个测试）。
- 移植内容：`RC_CONF_NAME_REGEX` / `RC_CONF_SAFE_REGEX` 等价校验、`sysrc set/unset` 参数构造、
  `ifconfig_<nic>` 值生成（含 `WPA DHCP` / `WPA inet ...` 无线分支）、`resolv.conf` 生成与解析、
  `dhclient.leases` 取最后一条网关、WEP hex/ASCII 判定、PSK/WPA2/WPA3/WEP/Open/EAP 字段构造、
  `phase2` 的 `auth=` / `autheap=` 分支。
- 关键结论：**`rc.conf` 的防线是值本身，不是 `Command` 参数列表**。`rc_conf_value_is_safe`
  拒绝引号、`$`、反引号、反斜杠与换行，空格放行；这条规则一旦被绕过，`sysrc` 写进去的就是
  root 开机执行的 shell 代码。
- 交叉核对：用 Python 原版 `save_psk_network` / `save_open_network` / `save_eap_network`
  （mock `_save_network`）生成字段列表，Rust 测试逐项比对，**5 类 PSK/WEP 与 4 类 EAP 全部一致**，
  包括 `phase2` 为空串时省略该字段、TLS 不带 password/phase2 这两个易错点。

### 2. `wg.rs`（P1）

- 产出：`rust/src/wg.rs`（217 行，含 6 个测试）。
- 移植内容：`service_state`（sysrc 失败/空 → `NO`）、`device_name`、`display_name`
  （`# Name = ` 第一个等号截断）、`status`（stdout 空且 stderr 含 `Device not configured` → 断开）、
  `build_dictionary`（重名后者覆盖）、`wg_quick_args`。
- 有意的行为保留：Python 的 `str.replace('.conf','')` 会替换**所有**出现，`archive.conf.bak`
  会变成 `archive.bak`；这会影响设备名，因此原样保留并在注释里标注，不做"顺手修正"。

### 3. `state.rs` + `dump-state`（阶段 1 验收核心）

- 产出：`rust/src/state.rs`（509 行，含 11 个测试）、`rust/src/bin/dump-state.rs`（59 行）。
- 移植内容：`_is_disabled`（只看 UP 标志）、`card_has_address`、`card_has_carrier`、
  `_inet_line`、`subnet_hex_to_dec`、`_wireless_state`、`_wired_state`、`tray_state()` 的
  tooltip 拼接（含 `% \n`、` \n` 这些空格细节）、单行 JSON 序列化。
- **验收证据**：用 Python 原版 `tray_state()`（mock `default_card` / `_ifconfig` / `wpa_status` /
  `wpa_cli`）生成 5 种状态的紧凑 JSON，作为字面量写进 `json_matches_the_python_baseline`，
  Rust 侧逐字节相等：wifi 已连、wifi 未连、有线已连、有线 disabled、无默认路由。
- 限制：`dump-state` 在 Linux 上只验证了 JSON 形状；**与 Python 版在同一台 FreeBSD 上的
  逐字段比对尚未执行**（需 B/C 类环境）。

### 4. 测试与提交

- `cargo test`：**58 passed**（阶段 1 前 27 → config 后 40 → state/wg 后 57 → JSON 基准 58），0 failed。
- `cargo fmt --check` 干净。
- 提交 `9609826` `feat: port config, wg and tray state logic to Rust`，5 文件 +1380 行。

## 三、未执行任务

- **任务 4 `i18n.rs`（.mo 读取器）**：未执行。它依赖先在目标环境编译 `po/*.po` 生成 `.mo`
  才能做端到端验证，本轮时间用于把 P0 的 `config` 与阶段 1 验收核心 `state` 做扎实。
  建议下一轮先在 VM 内 `msgfmt` 出 7 个 `.mo`，再写读取器并用真实文件断言。

## 四、下一步

1. 在 `freebsd15` VM 上用 `msgfmt` 生成 `zh_CN.mo` 等 7 份，落地 `i18n.rs` 与真实 `.mo` 测试。
2. 把 `dump-state` 纳入远端 CI 产物，在 B 类环境与 Python `tray_state()` 做一次逐字段 diff。
3. 阶段 2 起步：`networkmgr_configuration` 的 eframe 配置窗口骨架（IPv4/IPv6 双标签）。
4. 仍未关闭：第 9 节 P0（VM 无无线网卡）、P1（缺 `ctrl_interface`）、C 类实机验收。
