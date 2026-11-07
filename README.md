# NetworkMgr（Rust 重写）

用 Rust + egui / eframe 重写 NetworkMgr，目标平台仍是 FreeBSD / GhostBSD / DragonFlyBSD，
并尽量保持原项目的外部契约：安装路径、sudoers、devd 事件、`rc.conf`、
`wpa_supplicant.conf`、XDG 自启动与 gettext 翻译。

> **本分支 `feat/rust-rewrite` 是本 fork 的默认分支，仍处于开发阶段，
> 尚无可安装的 Rust 版网络管理器。** 上游 `ghostbsd/networkmgr` 的 `master`
> 仍是正式发行版本；本分支不能按发行版方式安装使用。

## 仓库内容

| 路径 | 说明 |
|---|---|
| [`docs/RUST_REWRITE_PLAN.md`](docs/RUST_REWRITE_PLAN.md) | 完整方案：外部契约冻结、四阶段迁移、A/B/C 验收矩阵、风险登记、ADR、实测记录 |
| [`rust/`](rust/) | 零依赖 `networkmgr-core`：**65 个契约测试**，覆盖配置校验、`wpa_cli` 输出解析、`ifconfig` / `netstat` / `sysrc` 解析、`rc.conf` / `resolv.conf` 安全写入、WireGuard、托盘状态聚合、MO 翻译目录；另含 `dump-state` 诊断二进制 |
| [`rust/spikes/`](rust/spikes/) | 三个图形栈程序：eframe/glow 最小窗口、tray-icon 原生菜单托盘、配置窗口骨架（IPv4/IPv6 双标签 + 校验预览） |
| [`.github/workflows/freebsd-rust.yml`](.github/workflows/freebsd-rust.yml) | 远端 FreeBSD 15.1 构建：core 测试 + 两个 spike，产出 `networkmgr-freebsd15-spikes` |
| [`rust/spikes/deploy-to-vm.sh`](rust/spikes/deploy-to-vm.sh) | 把 CI 产物拷进 `freebsd15` VM 实跑验收 |

## 本地开发

```sh
cd rust
cargo test                    # 纯逻辑层契约测试，Linux 宿主可直接跑
cargo fmt --check

cd spikes
cargo fmt --check
cargo test --no-default-features --features sni   # A 类环境无 GTK 开发库时用 SNI 后端
cargo build --release --locked --bins             # 默认 appindicator 后端，需 GTK3
```

两个 spike 都可用 `SPIKE_SECONDS=<秒>` 自动退出，便于无人值守验收。
托盘后端用 feature 切换：`appindicator`（默认，需 GTK 主循环）或 `sni`（自带 D-Bus worker）。

## 远端构建与 VM 验收

本机不能交叉编译 FreeBSD，VM 也不装编译工具链，二进制一律由远端 CI 产出：

```sh
gh run download <run-id> -n networkmgr-freebsd15-spikes -D ./artifacts
virsh -c qemu:///system net-dhcp-leases default      # 取 VM 当前 DHCP 地址，不要硬编码
./rust/spikes/deploy-to-vm.sh zemin@<vm-ip> ./artifacts
```

## 验收边界

- **A｜本机 Fedora**：纯逻辑层契约测试、格式检查、GUI spike 编译与起窗预验证。
- **B｜FreeBSD 15 VM**：CI 产物在 Xfce 下的窗口与托盘实跑**已通过**（方案第 20 节）；有线网络、静态 IP、devd 链路仍待验收，VM 无无线网卡。
- **C｜实体机 / GhostBSD**：真实 Wi-Fi 扫描与连接、企业级 EAP、桌面集成与打包验收。

VM 绿灯不等于验收完成；阶段 3 结束必须同时有 B 与 C 的记录。详见方案第 13 节。

## 已知限制

- 功能迁移尚未开始，现阶段只有方案、纯逻辑层、图形栈 spike 与 CI，没有网络管理二进制。
- 未部署 NetworkMgr 的 FreeBSD 主机可能缺 `wpa_supplicant.conf` 的 `ctrl_interface`，
  导致 `wpa_cli` 全链路不可用（方案第 9 节 P1 风险）。
- 真实 `wpa_cli` 正常输出样本仍缺，解析层由标准格式 mock 样本覆盖，
  daemon 可达性与扫描时序仍待 C 类实机验收。
