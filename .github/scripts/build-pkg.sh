#!/bin/sh
# 在 FreeBSD 内把 release 二进制打成一个可安装的 .pkg。
#
# 用法：
#   BUILD_DIR=/tmp/nm-bin REPO_ROOT=$(pwd) VERSION=0.1.0 \
#       OUT_DIR=artifacts sh .github/scripts/build-pkg.sh
#
# 依赖：pkg（环境自带）、msgfmt（pkg install gettext-tools）。
set -eu

VERSION=${VERSION:-0.1.0}
BUILD_DIR=${BUILD_DIR:?BUILD_DIR is required}
REPO_ROOT=${REPO_ROOT:?REPO_ROOT is required}
OUT_DIR=${OUT_DIR:-artifacts}

case "$(uname -m)" in
    amd64) ARCH=amd64 ;;
    arm64 | aarch64) ARCH=aarch64 ;;
    *) ARCH=$(uname -m) ;;
esac

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

install_file() {
    mode=$1
    source=$2
    target=$3
    mkdir -p "$(dirname "$target")"
    install -m "$mode" "$source" "$target"
}

mkdir -p "$STAGE/usr/local"

# 运行所需二进制；spike 程序（eframe-window / tray-status）不随包发布。
install_file 755 "$BUILD_DIR/networkmgr" "$STAGE/usr/local/bin/networkmgr"
install_file 755 "$BUILD_DIR/config-window" "$STAGE/usr/local/bin/networkmgr_configuration"
install_file 755 "$BUILD_DIR/networkmgr-devd" "$STAGE/usr/local/bin/networkmgr-devd"
install_file 755 "$BUILD_DIR/dump-state" "$STAGE/usr/local/bin/dump-state"
install_file 755 "$BUILD_DIR/apply-config" "$STAGE/usr/local/bin/apply-config"

# 系统集成文件。
install_file 644 "$REPO_ROOT/src/networkmgr.desktop" \
    "$STAGE/usr/local/etc/xdg/autostart/networkmgr.desktop"
install_file 440 "$REPO_ROOT/src/sudoers.d/networkmgr" \
    "$STAGE/usr/local/etc/sudoers.d/networkmgr"
install_file 644 "$REPO_ROOT/src/networkmgr.conf" \
    "$STAGE/usr/local/etc/devd/networkmgr.conf"
install_file 644 "$REPO_ROOT/src/wpa_supplicant.conf" \
    "$STAGE/usr/local/share/networkmgr/wpa_supplicant.conf"

# hicolor 图标（含 nm-* 与 networkmgr 应用图标）。
for source_dir in "$REPO_ROOT"/src/icons/*/; do
    size=$(basename "$source_dir")
    mkdir -p "$STAGE/usr/local/share/icons/hicolor/$size/apps"
    cp "$source_dir/apps/"* "$STAGE/usr/local/share/icons/hicolor/$size/apps/"
done

# gettext 目录。
for source in "$REPO_ROOT"/po/*.po; do
    lang=$(basename "$source" .po)
    target="$STAGE/usr/local/share/locale/$lang/LC_MESSAGES"
    mkdir -p "$target"
    msgfmt "$source" -o "$target/NetworkMgr.mo"
done

# plist 是相对 PREFIX（/usr/local）的路径列表。
(
    cd "$STAGE/usr/local"
    find . -type f | sed 's|^\./||' | sort
) > "$STAGE/plist"

cat > "$STAGE/+MANIFEST" <<MANIFEST
name: networkmgr
version: "$VERSION"
origin: "net-mgmt/networkmgr"
comment: "Network manager for FreeBSD desktops"
maintainer: "wenyinos@users.noreply.github.com"
www: "https://github.com/wenyinos/networkmgr"
prefix: "/usr/local"
desc: <<EOD
NetworkMgr is a graphical network manager for FreeBSD, GhostBSD and
DragonFlyBSD desktops. It provides a tray menu with wired, wireless and
WireGuard controls, a COSMIC-style configuration window, and devd hooks.
EOD
MANIFEST

mkdir -p "$OUT_DIR"
pkg create -M "$STAGE/+MANIFEST" -r "$STAGE" -p "$STAGE/plist" \
    -o "$OUT_DIR" -f txz

for package in "$OUT_DIR"/networkmgr-*.pkg; do
    case "$package" in
        *-"$ARCH".pkg) ;;
        *) mv "$package" "${package%.pkg}-$ARCH.pkg" ;;
    esac
done

ls -lh "$OUT_DIR"/networkmgr-*.pkg
echo "built networkmgr $VERSION for $ARCH"
