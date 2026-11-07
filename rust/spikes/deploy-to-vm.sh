#!/bin/sh
# 把远端 FreeBSD CI 产出的两个 spike 二进制拷进 freebsd15 VM 并实跑验收。
# 认证：优先 SSH 密钥；若设置 SSHPASS 且本机有 sshpass，则走密码认证。
#       脚本本身不保存任何密码。
#
# 取产物：
#   gh run download <run-id> -n networkmgr-freebsd15-spikes -D ./artifacts
#
# VM 的 IP 由 libvirt DHCP 动态分配，先取当前租约，不要硬编码：
#   virsh -c qemu:///system net-dhcp-leases default
#
# 用法：./deploy-to-vm.sh zemin@192.168.122.170 ./artifacts

set -eu

TARGET=${1:-}
ARTIFACTS=${2:-}
if [ -z "$TARGET" ] || [ -z "$ARTIFACTS" ]; then
    echo "用法: $0 <user@host> <artifact-dir>" >&2
    exit 2
fi

REMOTE_DIR=/tmp/networkmgr-spikes
for bin in eframe-window tray-status; do
    if [ ! -f "$ARTIFACTS/$bin" ]; then
        echo "缺少产物: $ARTIFACTS/$bin" >&2
        exit 1
    fi
done

# sshpass 只在本机侧使用，密码由调用方通过 SSHPASS 提供。
ssh_cmd() {
    if [ -n "${SSHPASS:-}" ] && command -v sshpass >/dev/null 2>&1; then
        sshpass -e ssh -o StrictHostKeyChecking=no "$@"
    else
        ssh "$@"
    fi
}
scp_cmd() {
    if [ -n "${SSHPASS:-}" ] && command -v sshpass >/dev/null 2>&1; then
        sshpass -e scp -o StrictHostKeyChecking=no "$@"
    else
        scp "$@"
    fi
}

ssh_cmd "$TARGET" "mkdir -p $REMOTE_DIR"
scp_cmd "$ARTIFACTS/eframe-window" "$ARTIFACTS/tray-status" "$TARGET:$REMOTE_DIR/"
ssh_cmd "$TARGET" "chmod +x $REMOTE_DIR/eframe-window $REMOTE_DIR/tray-status"

# 远端脚本用 quoted heredoc，全部变量在 guest 内展开。
ssh_cmd "$TARGET" sh -s <<'REMOTE'
set -u
cd /tmp/networkmgr-spikes
DISPLAY=:0
export DISPLAY

# Xfce 在 :0；lightdm 的 root 目录普通用户读不到，通常回退到 ~/.Xauthority。
for f in /var/run/lightdm/*/xauthority /var/run/lightdm/root/:0 "$HOME/.Xauthority"; do
    if [ -r "$f" ]; then
        XAUTHORITY="$f"
        export XAUTHORITY
        break
    fi
done
printf 'XAUTHORITY=%s\n' "${XAUTHORITY:-<not found>}"

# AppIndicator 靠 session bus 与 Xfce systray 通信；SSH 会话不继承桌面环境。
# dbus-launch 把地址写在 ~/.dbus/session-bus/<machine-id>-N，取当前会话那份。
if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    for f in "$HOME"/.dbus/session-bus/*; do
        [ -f "$f" ] || continue
        addr=$(sed -n "s/^DBUS_SESSION_BUS_ADDRESS='\([^']*\)'.*/\1/p" "$f" | head -n 1)
        if [ -n "$addr" ]; then
            DBUS_SESSION_BUS_ADDRESS="$addr"
            export DBUS_SESSION_BUS_ADDRESS
            break
        fi
    done
fi
printf 'DBUS_SESSION_BUS_ADDRESS=%s\n' "${DBUS_SESSION_BUS_ADDRESS:-<not found>}"

LIBGL_ALWAYS_SOFTWARE=1
export LIBGL_ALWAYS_SOFTWARE

echo "=== spike 1: eframe/glow 窗口 ==="
SPIKE_SECONDS=5 ./eframe-window
echo "spike1 exit=$?"

echo "=== spike 2: tray-icon 托盘 ==="
SPIKE_SECONDS=5 ./tray-status
echo "spike2 exit=$?"
REMOTE
