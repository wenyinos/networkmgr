# NetworkMgr 0.2.2

0.2.1 的补丁版本。0.2.1 试图自动清理 `$XDG_RUNTIME_DIR/tray-icon`，但**实际不生效**，
本版补全。

## 下载

| 架构 | 文件 |
|---|---|
| FreeBSD 15 amd64 | `networkmgr-rust-0.2.2-amd64.pkg` |

> aarch64 仍未发布：arm64 runner 上跑 FreeBSD 虚拟机实测超过 46 分钟未完成。
> 需要时可用 `workflow_dispatch` 指定 `arch=aarch64` 单独补构建。

## 本版修复

**0.2.1 的自动清理没有生效，重启后普通用户仍报 `EACCES`**

- **为什么 0.2.1 不行**：它用 `std::fs::remove_dir_all` 删 `$XDG_RUNTIME_DIR/tray-icon`。
  删除目录里的文件需要对**该目录本身**有写权限，而 root 留下的目录是 `0755`、
  普通用户只有 `r-x`，所以 `remove_dir_all` 直接失败，代码里的 `if ... is_ok()` 静默跳过，
  什么都没做。
- **0.2.2 的做法**：`remove_dir_all` 失败时改用 `rename`，把目录改名成
  `tray-icon.stale-<pid>`。重命名只需要**父目录**的写权限，而父目录
  （`$XDG_RUNTIME_DIR`）属于当前用户，所以一定能成功。旧目录被挪开之后，
  tray-icon 会重新创建自己的缓存目录。
- 顺带保留 0.2.1 的错误提示：`build()` 失败时会打印
  `rm -rf "$XDG_RUNTIME_DIR/tray-icon"` 的排查命令。

## 安装 / 升级

```sh
# 全新安装
sudo pkg add ./networkmgr-rust-0.2.2-amd64.pkg
# 从 0.2.x 升级
sudo pkg add -f ./networkmgr-rust-0.2.2-amd64.pkg
```

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
