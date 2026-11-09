# NetworkMgr 0.2.0

Rust 重写的第二个预览版本，目标平台 **FreeBSD / GhostBSD / DragonFlyBSD**。
这是**缺陷修复版**，主要解决 0.1.0 在干净系统上装完启动即崩溃的问题。

## 下载

| 架构 | 文件 |
|---|---|
| FreeBSD 15 amd64 | `networkmgr-rust-0.2.0-amd64.pkg` |

> aarch64 包暂未发布：arm64 runner 上跑 FreeBSD 虚拟机实测超过 46 分钟仍未完成。
> 需要时可用 `workflow_dispatch` 指定 `arch=aarch64` 单独补构建。

## 相对 0.1.0 的修复

- **P0｜干净系统上托盘 panic（退出码 101）**
  `libayatana-appindicator` 由 tray-icon 通过 `dlopen` 加载，不在 ELF 的 `NEEDED` 里，
  `pkg create` 的自动依赖扫描发现不了，0.1.0 的包里也没有手写依赖，于是装完启动就崩。
  现在 `+MANIFEST` 显式声明 `gtk3` 与 `libayatana-appindicator`，`pkg add` 会自动带上；
  程序启动前还会自检，缺库时输出中英双语提示而不是 Rust backtrace。
- **P1｜启动固定延迟 30–75 秒**
  session bus 探测顺序改为「环境变量 → `~/.dbus/session-bus/` → `/tmp` 扫描」；
  探测目标从 `org.kde.StatusNotifierWatcher` 改为 bus 本身（Cinnamon 不提供 watcher，
  旧逻辑会把活着的 bus 也判成不可达）；`dbus-send` 加 1 秒回复超时。
- **P2｜只认 Xfce 面板**
  面板/窗口管理器探测覆盖 Cinnamon、GNOME、Xfce、MATE、Plasma、Budgie、LXDE。
- **P3｜root 父进程常驻**
  `relaunch_as()` 从 `.status()` 改为 `.spawn()`，root 进程立即退出，不再长期挂着。
- **普通用户点「修复控制接口」只报"没有权限"**
  改为 `sudo -n` → `pkexec`（桌面认证框）→ 手动命令提示；sudoers 增加限定参数的免密条目
  `apply-config --fix-wpa-ctrl`。

## 安装

```sh
sudo pkg add ./networkmgr-rust-0.2.0-amd64.pkg
# 或
sudo pkg install -y ./networkmgr-rust-0.2.0-amd64.pkg
```

安装后从应用菜单启动「NetworkMgr (Rust)」，或运行 `sudo networkmgr-rust`。
卸载：`sudo pkg delete -y networkmgr-rust`。

## 与 Python 版共存

包名、二进制、desktop、sudoers、devd 规则、wpa 模板、图标与 gettext domain
全部使用独立的 `-rust` 名称，不会覆盖 `net-mgmt/networkmgr`。
devd 规则以 `.sample` 安装，接管事件时再复制为 `.conf` 并停用旧规则。

## 已知限制

- 真实无线关联受 FreeBSD 15 驱动限制：MT7601U 能扫描但关联超时，AR9271 已无 `athn` 驱动。
- 真实 EAP 认证仍需企业级 AP 与 FreeRADIUS，方案见 `docs/TESTING_ENTERPRISE_WPA.md`。
- 写 `rc.conf`、`ifconfig down`、`service netif restart` 等特权操作在普通用户模式下尚未
  全部接入提权通道；目前只有「修复控制接口」走通了 `sudo`/`pkexec`。
- 尚未进入 FreeBSD port。
