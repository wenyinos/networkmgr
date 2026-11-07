# NetworkMgr

> **本分支 `feat/rust-rewrite` 是 Rust 重写工作分支，也是本 fork 的默认分支。**
> 上游 `ghostbsd/networkmgr` 的 `master` 仍是下面描述的 Python + GTK3 正式实现。
> 本分支**尚未产出可安装的 Rust 版网络管理器**：目前只有迁移方案、纯逻辑层契约测试、
> 两个图形栈 spike 和 FreeBSD 15 CI。Python 版代码在本分支中仍完整保留、可照常安装。

## 本分支现状（Rust 重写进行中）

| 项 | 说明 |
|---|---|
| 目标 | 用 Rust + egui / eframe 重写，尽量保持原项目功能与 FreeBSD / GhostBSD 外部契约 |
| 方案 | [`docs/RUST_REWRITE_PLAN.md`](docs/RUST_REWRITE_PLAN.md)：外部契约冻结、分阶段迁移、A/B/C 验收矩阵、风险登记、ADR |
| 纯逻辑层 | [`rust/`](rust/)：零依赖 `networkmgr-core`，27 个契约测试，覆盖 `validate.py`、`wpa_cli` 输出解析、`ifconfig` 解析 |
| 图形栈验证 | [`rust/spikes/`](rust/spikes/)：eframe/glow 窗口与 tray-icon 托盘两个最小程序，支持 `appindicator` / `sni` 后端 |
| 远端构建 | [`.github/workflows/freebsd-rust.yml`](.github/workflows/freebsd-rust.yml)：FreeBSD 15.1 上构建 core 测试与两个 spike，产出 `networkmgr-freebsd15-spikes` |
| 本地测试 | `cd rust && cargo test`；`cd rust/spikes && cargo test --no-default-features --features sni` |
| VM 验收 | `rust/spikes/deploy-to-vm.sh`（需先下载 CI artifact）；VM 只装运行依赖，不装编译工具链 |

**不要**把本分支当成可安装版本。安装与部署请按下面 Python 版说明，或切回上游 `master`。

---

## 上游 Python 版（当前正式实现）

A network manager based on Python and GTK3 for FreeBSD, GhostBSD, and DragonFlyBSD. 

NetworkMgr supports FreeBSD rc(8) and openrc(8). 

## Installation

### On FreeBSD

See https://www.freshports.org/net-mgmt/networkmgr/#add for instructions on installing NetworkMgr.

#### Install using Python

Packages that have to be installed before installing NetworkMgr:

`pkg install sudo py311-setuptools py311-gobject3 gtk-update-icon-cache hicolor-icon-theme`

If NetworkMgr was previously installed using a package, remove it:  

`pkg delete networkmgr`

Download NetworkMgr, or clone this repository:

`git clone https://github.com/GhostBSD/networkmgr.git`

Then: 

`cd networkmgr` and `python3.8 setup.py install`

Users of NetworkMgr must be members of the _wheel_ group. To add a user: 

`pw groupmod wheel -m username`

## Starting NetworkMgr

If the desktop environment supports XDG then you can simply log out, or restart the computer. NetworkMgr should start automatically at login time.

## Managing Translations
To create a translation file.
```shell
./setup.py create_translation --locale=fr
```
To update all translation files
```shell
./setup.py update_translations
```
