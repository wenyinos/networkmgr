# NetworkMgr 0.2.3

**托盘不再以 root 运行。** 这是安全性上的一次结构性调整，也顺手解决了
「自启动起不来」的问题。

## 下载

| 架构 | 文件 |
|---|---|
| FreeBSD 15 amd64 | `networkmgr-rust-0.2.3-amd64.pkg` |

> aarch64 仍未发布：arm64 runner 上跑 FreeBSD 虚拟机实测超过 46 分钟未完成。

## 这版改了什么

### 托盘改以普通用户运行

- wpa 控制 socket 的属组交给 `wheel`（`ctrl_interface_group=wheel`），
  wheel 组成员可以直接读写，`wpa_cli` 不再需要 root。
- 桌面项 `Exec=sudo networkmgr-rust` 改为 `Exec=networkmgr-rust`。
- sudoers 里删掉了「整块二进制免密」，只留 `apply-config --fix-wpa-ctrl` 一条兜底。
- 特权操作（写 `rc.conf` / `resolv.conf`、存 wpa 配置、`ifconfig down`）改为
  **pkexec 优先**：弹桌面认证框，`sudo -n` 只作为后备。

### 为什么以前必须 sudo

FreeBSD 的 sudo 编译时带 `--with-tty-tickets`，在自启动这类**没有终端**的场景里，
即使 sudoers 配了 `NOPASSWD` 也会转去要求密码——于是「终端里能跑、开机自启不行」。
把托盘改成普通用户运行、特权操作走 pkexec（polkit 有自己的 agent，不依赖 tty），
这条链路才真正成立。

### `apply-config --fix-wpa-ctrl` 现在补两行

以前只补 `ctrl_interface=`，socket 属组仍是 root，普通用户照样连不上。
现在同时补 `ctrl_interface_group=wheel`。

## 升级到本版后要做的一步

老机器的 `/etc/wpa_supplicant.conf` 可能只有 `ctrl_interface=`：

```sh
sudo apply-config --fix-wpa-ctrl   # 会补上 ctrl_interface_group=wheel
sudo service wpa_supplicant restart
```

路由器/驱动都不需要动，只是让 socket 对 wheel 组可读。

## 安装 / 升级

```sh
sudo pkg add -f ./networkmgr-rust-0.2.3-amd64.pkg
```

卸载：`sudo pkg delete -y networkmgr-rust`。

## 已知限制

- 无线扫描/连接依赖 wpa socket 的组权限；如果你不在 `wheel` 组，需要先加入。
- 真实 EAP 认证仍需企业级 AP + FreeRADIUS（`docs/TESTING_ENTERPRISE_WPA.md`）。
- 尚未进入 FreeBSD port。
