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

---

## 第二轮（22:00–22:20）：阶段 1 收尾与阶段 2 起步

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 6 | 实现 `i18n`：GNU MO 读取器 | 构建 | `rust/src/i18n.rs`（326 行 / 7 测试） | ✅ 完成 |
| 7 | `dump-state` 进 CI 产物并与 Python 实机比对 | 构建 | workflow 四产物；VM 实跑逐字节一致 | ✅ 完成 |
| 8 | 阶段 2 起步：配置窗口骨架 | 构建 | `rust/spikes/src/bin/config-window.rs`（377 行 / 5 测试） | ✅ 完成 |
| 9 | 更新方案、README 与报告 | 写作 | 方案第 22 节、README 仓库内容表 | ✅ 完成 |

### 关键结果

- `i18n.rs`：GNU MO 解析（magic 大小端、偏移表、单复数取第一形态、`Language:` header、
  `LANG` 收敛、缺失回退原文）。用 `msgfmt po/zh_CN.po -o /tmp/zh_CN.mo` 编译真实目录，
  `NETWORKMGR_MO=... cargo test` 端到端读取通过。
- **两个坑**：① MO 布局是「全部 msgid 连续放，再放全部 msgstr」，译串偏移必须用所有 msgid 总长；
  ② `fuzzy` 条目不会写入 `.mo`，当前分支 `po/zh_CN.po` 仍是旧版（16 译 / 9 fuzzy / 24 未译），
  所以测试只断言「至少有一条译文」，不写死措辞。
- `config-window`：接口下拉、IPv4/IPv6 双标签、DHCP/Manual 与 SLAAC/Manual 切换、逐字段校验、
  生成 `rc.conf` 行与 `resolv.conf` 预览；**不写盘**。`rust/spikes` 新增 `networkmgr-core` 路径依赖。
- **`dump-state` 实机比对**：CI run `42379533273` 的四产物中，`dump-state` scp 进 VM 运行；
  同一时刻采集 `netstat -rn` / `ifconfig vtnet0`，本机 Python 版复算与 Rust 输出**逐字节一致**。
  输出：`{"card":"vtnet0","kind":"wire","connection":"Connected","ssid":null,"signal":null,"tooltip":"inet 192.168.122.170 netmask 255.255.255.0 broadcast 192.168.122.255"}`。
- 测试：core **65 passed**、spikes **6 passed**；`cargo fmt --check` 干净。

### 未执行 / 仍关闭

- 无线分支的实机比对：VM 无无线网卡，需 C 类实机（`X200-BSD` / GhostBSD）。
- 配置窗口的写盘与接口重启：属阶段 2 后续，骨架只做校验与预览。
- C 类验收、P0/P1 风险、PR #133 合并（zh_CN 旧译）全部未变；本轮无可安装二进制。

---

## 第三轮（22:20–22:30）：阶段 2 写盘链路

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 10 | 实现 `sys.rs` 命令执行层（超时、错误语义、FakeRunner） | 构建 | `rust/src/sys.rs`（455 行 / 9 测试） | ✅ 完成 |
| 11 | 配置窗口接写盘（`--apply`，默认 dry-run） | 构建 | `config-window.rs` 新增 91 行 | ✅ 完成 |
| 12 | `dump-state` 复用 `Sys`，与托盘同源 | 构建 | `dump-state.rs` 从 60 行缩到 12 行 | ✅ 完成 |

### 关键结果

- **超时不用管道**：轮询 `try_wait` + 输出落临时文件，超时直接 kill。`Command::output()` 没有超时，而先 `try_wait` 再读管道会被写满的缓冲区卡死——这是本轮实现里最容易埋雷的地方。
- **`sysrc_set` 校验失败返回 `SysError::Rejected`**，最初写成静默 `Ok(())`，等于让调用方以为写成功；已改正并加了断言。
- `wpa_save_config` 成功后补 `chmod 0600`，原因是 daemon 自己的 chmod 被 `#ifdef ANDROID` 包住，FreeBSD 上文件权限由 umask 决定。
- 配置窗口默认 dry-run，`--apply` 才写盘；写 `/etc/rc.conf` 与 `/etc/resolv.conf`，不重启接口（重启属阶段 2 收尾）。
- 测试：core **74 passed**、spikes **6 passed**。

### 未执行 / 仍关闭

- **VM 上没跑 `--apply`**：VM 只有一块网卡，改静态地址会立刻断 SSH。要验真机写盘，得给 VM 临时挂第二块网卡，或等 C 类环境。
- 配置窗口没有调用 `service netif restart`（阶段 2 收尾项）。
- C 类实机验收、P0/P1 风险维持原状。

---

## 第四轮（22:30–22:55）：真机写盘验证

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 13 | 新增 `apply-config` 诊断命令，让写盘脱离 GUI 可验 | 构建 | `rust/src/bin/apply-config.rs`（200 行） | ✅ 完成 |
| 14 | 在 VM 用 `epair0a` 真机验证 `sysrc` + `resolv.conf` 写入 | 分析 | 第 24 节记录 + 全量清理 | ✅ 完成 |

### 关键结果

- **不碰 `vtnet0`**：`ifconfig epair0 create` 出 `epair0a`/`epair0b` 作靶子，写盘只作用于它，SSH 全程未断。
- dry-run 打印、`--apply` 执行、`sysrc -n` 回读、`/etc/rc.conf` 实际内容、`/etc/resolv.conf` 改写五项全部符合预期，退出码 0。
- **最有价值的一行**：`sysrc` 写出的是 `ifconfig_epair0a="inet ..."`，自动补双引号、**不做转义**。这把「`rc.conf` 的防线是写入值本身」从注释变成了实测证据。
- 清理完整：`sysrc -x` 删变量、`resolv.conf` 从备份还原、`epair0a` destroy，接口回到 `vtnet0 lo0`。
- CI run `42382051444` 成功（9m8s），artifact 里五个二进制（新增 `apply-config`，561 KB）。

### 未执行

- 配置窗口的写盘 + `service netif restart` 闭环：`epair` 不承载真实流量，重启后地址生效与连通性需要真实网卡或第二块 virtio 网卡。
- C 类实机验收、P0/P1 风险、PR #133 合并，均未变。

---

## 第五轮（00:04–00:15）：配置窗口改用 COSMIC / iced 视觉

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 15 | 按参考图重做配置窗口，支持明暗切换 | 构建 | `config-window.rs` +445/−120 行，6 个测试 | ✅ 完成 |
| 16 | 截图入库并在 README 增加「界面预览」 | 写作 | `docs/images/config-{light,dark}.png` | ✅ 完成 |
| 17 | 统一更新 README / AGENTS.md / 方案 / 报告 / 看板 | 写作 | 各文档同步到阶段 3 骨架状态 | ✅ 完成 |

### 关键结果

- 配色**逐像素取色**：明色选中 `#006CA0`、卡片 `#FAFAFA`、窗口 `#EBEBEB`；
  暗色选中 `#69E4E3`、卡片 `#2B2B2B`、窗口 `#181818`。
- 布局改为 252 px 左侧导航（选中项为整行胶囊）+ 右侧卡片式表单；卡片圆角 12、内边距 16；
  按钮统一 16 圆角胶囊。主题切换在侧边栏底部，另支持 `NETWORKMGR_THEME=dark` 启动即暗色。
- **egui 0.37 API 变化**：`SidePanel` 已并入 `Panel`，且不再提供 `exact_width`；
  改为自绘两栏背景 + `ui.scope_builder(UiBuilder::new().max_rect(..))`。
- 环境坑：KWin Wayland 下 `grim` 报 compositor 不支持 screencopy，改用 `spectacle -b -n -o`；
  `pkill -f 'target/debug/config-window'` 会匹配到执行命令的 shell 自身导致自杀，必须用 `pkill -x`。

### 未执行

- 导航图标仍是 emoji，未换成 `src/icons/` 的 PNG。
- 卡片与窗口背景对比度偏柔和，可按需再拉开。
- 阶段 3 剩余项（扫描连接流程、300 ms 动画、Forget）与 C 类实机未变。

---

## 第六轮（09:00–09:20）：托盘连接流程、Forget 与 Restart 确认

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 18 | 补 `wpa` 连接字段与 `add/set/enable/select/remove/reconfigure` 命令层 | 构建 | `wpa.rs` +75 行 / 4 测试，`sys.rs` +210 行 / 6 测试 | ✅ 完成 |
| 19 | 托盘接线：扫描连接、PSK/WEP 密码框、Forget 子菜单、连上才保存 | 构建 | `networkmgr.rs` +320 行 / 1 测试 | ✅ 完成 |
| 20 | Restart Networking 加确认框并改为分接口重启 | 构建 | 同上，截图 `docs/images/restart-confirm.png` | ✅ 完成 |
| 21 | FreeBSD CI 与 VM 实机验收 | 分析 | CI run `42463440516`；VM 截图与日志 | ✅ 完成（限有线） |

### 关键结果

- **连接流程对齐 Python**：已保存网络直接 `select_network`；开放网络建块即连；
  PSK/WEP 弹 GTK 密码框；企业级提示走配置窗口。新建块任一步失败自动删除，
  连上之后才 `save_config`，结束后 `enable_network all` 恢复其他网络。
- **Forget 改为子菜单**：列出全部已保存网络（包括不在扫描范围内的），
  倒序删除同名块后持久化；没有确认框，与 Python 行为一致。
- **Restart 确认框**：VM 实测无参数 `service netif restart` 会让 SSH 长时间不恢复，
  现在必须先确认，且只重启当前默认网卡。
- **rustfmt 版本差异**：本机旧版不换行、CI 新版要求换行，CI 首跑失败；
  改成两版都接受的多行 `let connected_here = ...` 后通过（`a1bbaa3`）。
- 测试：core **86 passed**、GUI **10 passed**、`cargo fmt --check` 干净；
  CI 在 FreeBSD 15.1 编译出 1.5 MB 的 `networkmgr`。

### VM 验收（B 类）

- 以 zemin 身份运行（root 连不上 zemin 的 session bus，dbus EXTERNAL auth 拒绝）。
- 托盘启动输出正确的状态 JSON，面板出现 22×22 托盘图标；
  左键菜单显示 `Wired vtnet0 Connected` / `Disable vtnet0` / `Configure vtnet0` /
  `Restart Networking` / `Close Network Manager`。
- 用 `xdotool` 键盘导航触发 Restart，确认框显示
  `Restart networking on vtnet0?` + 断线提示 + Cancel/Restart；
  点 Cancel 后 `vtnet0` 仍 active、IP 不变、SSH 未断，日志无重启记录。

### 未执行 / 仍关闭

- **VM 无无线网卡**：扫描连接、密码框、Forget 的真实链路无法在 B 类环境走通，
  只能靠 FakeRunner 单测与 CI 编译覆盖，仍需 C 类实机。
- 300 ms 连接动画、企业级 EAP 凭据收集仍未实现。
- 测试环境变更：VM 内用 `pkg install` 装了 `xdotool` 仅用于菜单点击，不属于项目依赖。

---

## 第七轮（17:30–18:10）：本地化接入、devd 迁移与删除旧 Python

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 22 | i18n 全局入口 `init()` / `tr()` / `tr!` 宏 | 构建 | `i18n.rs` +85 行 / 1 个端到端测试 | ✅ 完成 |
| 23 | 托盘与配置窗口全部字符串接入 `tr!` | 构建 | `networkmgr.rs`、`config-window.rs` 约 130 处 | ✅ 完成 |
| 24 | devd 三脚本迁移为 `networkmgr-devd` | 构建 | `devd.rs` 约 500 行 / 10 测试；`networkmgr.conf` 改路径 | ✅ 完成 |
| 25 | 删除旧 Python 实现 | 清理 | 13 个文件（`NetworkMgr/`、入口、`setup.py`、`src/*.py`） | ✅ 完成 |
| 26 | VM 中文菜单与 devd 实机验收 | 分析 | `docs/images/tray-menu-zh_CN.png`、epair attach 记录 | ✅ 完成 |
| 27 | README / 方案 / 报告 / 看板同步 | 写作 | README 界面预览与纯 Rust 说明、方案第 29 节 | ✅ 完成 |

### 关键结果

- **翻译链路打通**：`i18n.rs` 用 `OnceLock` 保存进程级目录，`tr!` 直接返回译串；
  `LANG`/`LC_ALL` 选择语言，`NETWORKMGR_LOCALE_DIR` 与 `NETWORKMGR_MO` 用于未安装运行。
  `NETWORKMGR_MO=/tmp/zh_CN.mo cargo test global_translation` 断言
  “Restart Networking” → “重启网络”。
- **7 个 po 全部补齐**：配置窗口与新增菜单项共 27 条 msgid，de / nb_NO / pt_Br / ru / sv / zh_CN
  均已翻译；`msgfmt --check` 通过。`zh_CN.po` 采用 `i18n/complete-zh-cn` 分支已修好的旧条目。
- **区域名回退**：`de_DE` → `de`，与 gettext 一致；新增单测覆盖。
- **中文窗口实测**：本机 `LANG=zh_CN.UTF-8` 运行配置窗口，明暗两套截图已覆盖
  `docs/images/config-{light,dark}.png`；德文界面也验证了按语言切换。
- **托盘中文实测**：VM 上 `LANG=zh_CN.UTF-8` 启动，菜单依次显示
  “有线 vtnet0 已连接 / 禁用 vtnet0 / 配置 vtnet0 / 重启网络 / 退出网络管理器”。
- **devd 迁移**：`is_not_nic` 保留原正则的任意位置匹配语义；`link_down` 倒序/标记文件/
  DHCP 移交逐条对齐。VM 实测 `attach epair99a` 写入 `ifconfig_epair99a=DHCP`，
  参数错误退出 1，过滤名退出 0，清理后只剩 `vtnet0 lo0`。
- **旧代码清理**：删除 `NetworkMgr/`、`networkmgr`、`networkmgr_configuration`、`setup.py`、
  `.pylintrc`、三个 devd Python 脚本；保留图标、desktop、sudoers、wpa 模板与 `po/`。
- 测试：core **98 passed**、GUI **10 passed**、`cargo fmt --check` 干净。

### 未执行 / 遗留

- **远端 CI 未产出本次二进制**：`anyvm` 下载连续 504（外部服务），改用 VM 内
  `pkg install rust` + 本机 Cargo registry 离线构建，3 分 37 秒编译成功。
- 7 个 po 已补齐本次条目；未来新增但未翻译的字符串仍会按 gettext 规则回退英文。
- FreeBSD port 尚未迁移到 Rust，仍没有可安装包；`sudoers`/desktop 的安装路径需在 port 阶段定稿。
- C 类实机（真实 Wi-Fi / EAP）仍未验收。

---

## 第八轮（19:00–19:15）：应用图标与窗口图标

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 28 | 设计应用图标（完整版 + 小尺寸简化版） | 构建 | 2 个 SVG + 13 个尺寸 PNG | ✅ 完成 |
| 29 | 配置窗口接入窗口图标 | 构建 | `networkmgr-64.rgba` + `with_icon()` | ✅ 完成 |
| 30 | desktop 增加应用图标引用 | 构建 | `Icon=networkmgr` | ✅ 完成 |
| 31 | 更新 README 截图与文档 | 写作 | 明暗截图、方案第 30 节 | ✅ 完成 |

### 关键结果

- 图标为蓝色渐变圆角方块 + 白色三节点拓扑；16/20/24 px 改用三个大圆点的简化版，
  实测 16 px 下清晰可辨，完整版在该尺寸会糊。
- 13 个 hicolor 尺寸全部生成（16–512），SVG 源文件入库。
- 窗口图标用 64×64 未预乘 RGBA 内嵌，`eframe`/`egui` 直接读取，不引入图像解码依赖。
- 本机实跑验证：明暗两套主题的标题栏左上角都显示新图标；README 截图已更新。

---

## 第九轮（19:35–19:45）：托盘「配置网卡」接线

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 32 | Configure 菜单启动配置窗口 | 构建 | 契约入口优先 + `--interface` 预选 | ✅ 完成 |
| 33 | 配置窗口列出真实接口 | 构建 | `Sys::ifconfig_list()` + 回退示例 | ✅ 完成 |
| 34 | 测试与文档 | 写作 | core 100 / GUI 11；方案第 31 节 | ✅ 完成 |

### 关键结果

- 托盘点击 Configure 时依次尝试 `/usr/local/bin/networkmgr_configuration` 与
  `config-window`，用 `--interface <nic>` 打开并预选网卡。
- 配置窗口启动时读取 `ifconfig -l`；读不到时保留示例接口，不影响本机开发。
- 本机验证 `config-window --interface vtnet0` 下拉框正确预选。
- 真实菜单点击依赖 appindicator 后端，留待 C 类实机。

---

## 第十轮（20:00–20:15）：连接动画与企业级 EAP

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 35 | 300 ms 连接动画 | 构建 | `ConnectingGuard` + 0→4 格循环 + 刷新避让 | ✅ 完成 |
| 36 | 企业级 EAP 凭据对话框 | 构建 | PEAP/TTLS/TLS 表单 + 字段校验 | ✅ 完成 |
| 37 | EAP 写入与 po 补全 | 构建 | `wpa_save_eap_network`；2 条文案 × 7 语言 | ✅ 完成 |

### 关键结果

- 动画与 Python 版一致：300 ms 一帧，连接期间循环 0→4 格，结束恢复真实信号；
  `ConnectingGuard` 处理所有 early return，30 秒刷新不会覆盖动画帧。
- EAP 对话框支持 PEAP/TTLS/TLS；TLS 自动置灰口令与内层认证，并要求证书路径。
- 写入路径复用 `config::eap_network_fields()`，失败删块、连上才保存。
- core **101 passed**、GUI **12 passed**；真实 EAP 认证仍需 C 类实机。

---

## 第十一轮（21:30）：VM 模拟 wpa 命令序列

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 38 | 在 VM 用 `wpa_supplicant -D wired` 模拟 daemon | 分析 | `PONG`；EAP/PSK/OPEN 字段全部 `OK` | ✅ 完成 |
| 39 | 清理模拟环境 | 清理 | epair 销毁、daemon 停止、临时文件删除 | ✅ 完成 |

### 关键结果

- EAP 的 8 个字段（ssid/key_mgmt/eap/identity/password/phase2/anonymous_identity/ca_cert）
  以及 enable/select 全部被真实 wpa_supplicant 接受。
- PSK 与 OPEN 序列同样通过。
- `save_config` 在 wired 模拟驱动下返回 `FAIL`，该驱动不实现保存；无线落盘未验证。
- 物理机只做过只读采集，期间出现无线断连/黑屏重启后已停止使用；
  未在物理机上执行任何重启或写配置操作。

---

## 第十二轮（21:30–21:45）：功能补齐 2–7

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 40 | 连接失败 3 次重试与重开凭据框 | 构建 | `ConnectOutcome` 状态机 + `wpa_update_psk` | ✅ 完成 |
| 41 | 断开无线菜单 | 构建 | `Disconnect from %s` + `wpa_cli disconnect` | ✅ 完成 |
| 42 | 配置窗口写盘后重启接口 | 构建 | `service netif restart <nic>` | ✅ 完成 |
| 43 | 扫描后立即刷新菜单 | 构建 | `UiRequest::Refresh` | ✅ 完成 |
| 44 | 配置窗口接口过滤 | 构建 | `is_configurable_interface()` | ✅ 完成 |
| 45 | 配置窗口 Wi-Fi 凭据卡片 | 构建 | PSK/EAP/OPEN 表单 + `--apply` 写 daemon | ✅ 完成 |

### 关键结果

- 主线程统一消费 `UiRequest::{Action, ConnectOutcome, Refresh}`；连接结果不再由工作线程
  直接删块或保存，而是交回主线程决定重试/清理。
- 失败重试对齐 Python：最多 3 次；已保存 PSK 只改 psk，新建块删除重建；EAP 重开对话框。
- 配置窗口的 Wi-Fi 卡片默认 dry-run，`--apply` 才写 daemon 并 `save_config`。
- 有线/无线共用「网络」页：标题按接口类型显示 `Wi-Fi` 或「以太网」，
  Wi-Fi 凭据卡片只在 `wlan*` 接口下显示。
- core **102**、GUI **12** 测试通过；新增 2 条文案同步 7 个 po。

---

## 第十三轮（21:45–21:55）：配置窗口布局重构

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 46 | 三页签导航（以太网 / Wi-Fi / VPN） | 构建 | `Nav::{Wire,Wireless,Vpn}` + `switch_nav` | ✅ 完成 |
| 47 | VPN 页隧道列表与启停 | 构建 | 读取 wg 配置目录 + `wg-quick up/down` | ✅ 完成 |
| 48 | 明暗按钮移至左下角、布局收紧 | 构建 | 侧栏 220、卡片边距 12、间距 8 | ✅ 完成 |
| 49 | 三页中文截图与新文案 | 写作 | README 三图；2 条文案 × 7 语言 | ✅ 完成 |

### 关键结果

- 删除两个无用占位页；有线/无线/ VPN 各自独立页面。
- 明暗切换按剩余高度推到底部，与参考图一致。
- `NETWORKMGR_PAGE=wire|wireless|vpn` 可直接打开指定页，截图与脚本验收更方便。
- 测试仍为 core **102** / GUI **12**。

---

## 第十四轮（22:30–22:55）：VM 模拟无线 UI 验证

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 50 | USB 无线网卡实测（MT7601U / AR9271） | 分析 | 能扫描；关联超时 / 无驱动 | ✅ 完成（受限） |
| 51 | `epair` 改名 `wlan0` + 假 `wpa_cli`/`netstat` 模拟 | 构建 | 托盘无线状态与菜单截图 | ✅ 完成 |
| 52 | 清理模拟环境 | 清理 | 销毁 wlan0、停 daemon、卸载 xdotool | ✅ 完成 |

### 关键结果

- MT7601U：`mtw` 驱动 + 手动固件能扫描，关联阶段 10 秒超时后本地 deauth；
  AR9271：FreeBSD 15 已移除 `athn`，没有可安装驱动。
- 模拟：托盘输出无线状态 JSON，主菜单与「可用连接」子菜单完整，当前 SSID 被跳过，
  扫描结果信号百分比正确。
- 截图：`docs/images/tray-menu-wifi-mock.png`、`docs/images/tray-menu-scan-mock.png`。
- 真实关联/握手/EAP/save_config 仍未验证；模拟仅覆盖 UI 与状态聚合。

---

## 第十五轮（22:55–23:10）：wpa_cli 控制接口缺失的修复

| # | 任务 | 类型 | 产出 | 状态 |
|---|---|---|---|---|
| 53 | core 补全函数与文件修复 | 构建 | `has/ensure_ctrl_interface` + `fix_ctrl_interface_file` | ✅ 完成 |
| 54 | `apply-config --fix-wpa-ctrl` | 构建 | 备份、写入、0600、幂等 | ✅ 完成 |
| 55 | 配置窗口与托盘检测提示 | 构建 | 红色提示 + 修复按钮/菜单项 | ✅ 完成 |
| 56 | 7 条文案 × 7 语言 | 写作 | po 更新 | ✅ 完成 |

### 关键结果

- 精确匹配 `ctrl_interface`（忽略注释，`ctrl_interface_group` 不算），幂等修复。
- 修复前备份 `.bak-networkmgr`，写后 0600；不自动重启服务，避免静默断网。
- 本机以 root 实测：第一次写入并备份，第二次返回“已存在”。
- 配置窗口无线页在 `wpa_cli` 不可用时显示中文红色提示与「修复控制接口」。
- core **104**、GUI **12** 测试通过。

---

## 第十六轮（23:15–23:25）：验收边界调整

用户决定将以下三项按当前边界标记完成，不再等待硬件：

| 项 | 验证依据 |
|---|---|
| 真实无线 | MT7601U 扫描与关联尝试；AR9271 无驱动；模拟 UI 与状态聚合 |
| 300 ms 连接动画 | 代码与模拟状态；真实连接时未观察 |
| WireGuard 实机 | 代码与 CI；未在真实隧道上验证 |

仍开放：真实 EAP 认证（t35，需要企业级 AP）、FreeBSD port 打包、证书文件选择器、
隐藏网络、外语翻译复核。

这不是“真实硬件全部通过”，只是按用户要求关闭这三项，避免继续等待不稳定的硬件。
