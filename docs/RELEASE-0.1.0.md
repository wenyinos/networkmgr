# NetworkMgr 0.1.0

Rust 重写后的首个预览版本，目标平台 **FreeBSD / GhostBSD / DragonFlyBSD**。

## 下载

| 架构 | 文件 |
|---|---|
| FreeBSD 15 amd64 | `networkmgr-0.1.0-amd64.pkg` |
| FreeBSD 15 aarch64 | `networkmgr-0.1.0-aarch64.pkg` |

## 安装

```sh
sudo pkg add ./networkmgr-0.1.0-amd64.pkg
# 或
sudo pkg install -y ./networkmgr-0.1.0-amd64.pkg
```

安装后重新登录桌面会话；托盘通过 XDG 自启动。卸载：

```sh
sudo pkg delete -y networkmgr
```

## 包含内容

- `networkmgr`：系统托盘（有线 / 无线 / WireGuard、扫描连接、PSK/WEP/EAP、
  隐藏网络、Forget、Disconnect、3 次重试、300 ms 动画、7 种语言）
- `networkmgr_configuration`：COSMIC 风格配置窗口（以太网 / Wi-Fi / VPN 三页、
  IPv4/IPv6、DHCP/静态/SLAAC、Wi-Fi 凭据、证书选择器、`--apply` 写盘并重启接口）
- `networkmgr-devd`：devd attach / link-up / link-down
- `dump-state`、`apply-config`：诊断命令（含 `--fix-wpa-ctrl`）
- desktop、sudoers、devd 规则、wpa_supplicant 模板、hicolor 图标、7 个 `.mo`

## 已知限制

- 真实无线关联受 FreeBSD 15 驱动限制：MT7601U 能扫描但关联超时，AR9271 已无
  `athn` 驱动；UI/状态层由模拟验证。
- 真实 EAP 认证仍需企业级 AP（PEAP/TTLS/TLS 表单与写盘逻辑已就绪）。
- 尚未进入 FreeBSD port；请用 pkg 安装/卸载。
- 老机器的 `/etc/wpa_supplicant.conf` 若缺 `ctrl_interface`，运行
  `sudo apply-config --fix-wpa-ctrl`，再重启 `wpa_supplicant`。

## 源码

https://github.com/wenyinos/networkmgr/tree/feat/rust-rewrite
