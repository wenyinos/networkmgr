# NetworkMgr 0.2.1

0.2.0 的补丁版本，修一个**重启后托盘起不来**的问题。

## 下载

| 架构 | 文件 |
|---|---|
| FreeBSD 15 amd64 | `networkmgr-rust-0.2.1-amd64.pkg` |

> aarch64 仍未发布：arm64 runner 上跑 FreeBSD 虚拟机实测超过 46 分钟未完成。
> 需要时可用 `workflow_dispatch` 指定 `arch=aarch64` 单独补构建。

## 本版修复

**重启后普通用户无法启动托盘，报 `Error: OsError(Os { code: 13, kind: PermissionDenied })`**

- **根因**：tray-icon 会在 `$XDG_RUNTIME_DIR/tray-icon` 下建缓存目录。只要用 `root`
  跑过一次托盘（例如手工验收），该目录就归 root 所有；之后普通用户登录桌面再启动，
  写不进去就会 `EACCES` 退出。
- **为什么错误信息误导**：tray-icon 用的是 `io::Error::last_os_error()`，拿到的是**上一次**
  操作残留的 errno，`truss` 里甚至看不到这次失败的系统调用。
- **修复**：
  1. 启动时检查 `$XDG_RUNTIME_DIR/tray-icon` 的属主，不是当前用户就删掉重建
     （目录里只有缓存，代价为零）；
  2. `TrayIconBuilder::build()` 失败时打印可执行的排查提示，不再只丢一个错误码。

## 安装

```sh
sudo pkg add ./networkmgr-rust-0.2.1-amd64.pkg
# 或
sudo pkg install -y ./networkmgr-rust-0.2.1-amd64.pkg
```

安装后从应用菜单启动「NetworkMgr (Rust)」，或运行 `sudo networkmgr-rust`。
卸载：`sudo pkg delete -y networkmgr-rust`。

## 与 Python 版共存

包名、二进制、desktop、sudoers、devd 规则、wpa 模板、图标与 gettext domain
全部使用独立的 `-rust` 名称，不会覆盖 `net-mgmt/networkmgr`。

## 已知限制

- 真实无线关联受 FreeBSD 15 驱动限制：MT7601U 能扫描但关联超时，AR9271 已无 `athn` 驱动。
- 真实 EAP 认证仍需企业级 AP 与 FreeRADIUS，方案见 `docs/TESTING_ENTERPRISE_WPA.md`。
- 写 `rc.conf`、`ifconfig down`、`service netif restart` 等特权操作在普通用户模式下尚未
  全部接入提权通道；目前只有「修复控制接口」走通了 `sudo`/`pkexec`。
- 尚未进入 FreeBSD port。
