#!/bin/sh
# 把远端 FreeBSD CI 产出的两个 spike 二进制拷进 freebsd15 VM 并实跑验收。
# 不保存任何密码：依赖 SSH 密钥或交互式认证。
#
# 取产物的两种方式：
#   gh run download <run-id> -n networkmgr-freebsd15-spikes -D ./artifacts
#   或直接从 GitHub Actions 页面下载后解压到 ./artifacts
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

ssh "$TARGET" "mkdir -p $REMOTE_DIR"
scp "$ARTIFACTS/eframe-window" "$ARTIFACTS/tray-status" "$TARGET:$REMOTE_DIR/"
ssh "$TARGET" "chmod +x $REMOTE_DIR/eframe-window $REMOTE_DIR/tray-status"

# 远端脚本用 quoted heredoc，全部变量在 guest 内展开。
ssh "$TARGET" sh -s <<'REMOTE'
set -u
cd /tmp/networkmgr-spikes
DISPLAY=:0
export DISPLAY

# Xfce 在 :0，Xauthority 由 lightdm 提供；找不到时回退到家目录。
# 若自动探测失败，可在运行前手工设置 VM_XAUTHORITY 并改用显式路径。
for f in /var/run/lightdm/*/xauthority /var/run/lightdm/root/:0 "$HOME/.Xauthority"; do
    if [ -r "$f" ]; then
        XAUTHORITY="$f"
        export XAUTHORITY
        break
    fi
done
printf 'XAUTHORITY=%s\n' "${XAUTHORITY:-<not found>}"

LIBGL_ALWAYS_SOFTWARE=1
export LIBGL_ALWAYS_SOFTWARE

echo "=== spike 1: eframe/glow 窗口 ==="
SPIKE_SECONDS=3 ./eframe-window
echo "spike1 exit=$?"

echo "=== spike 2: tray-icon 托盘 ==="
SPIKE_SECONDS=3 ./tray-status
echo "spike2 exit=$?"
REMOTE
