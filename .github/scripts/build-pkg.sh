#!/bin/sh
# 在 FreeBSD 内把 release 二进制打成一个可安装的 .pkg。
#
# 与 Python 版 networkmgr 共存：包名、二进制、desktop、sudoers、devd、图标、
# gettext domain 全部独立，安装路径不与官方 networkmgr 重叠。
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

# 运行所需二进制：托盘与配置窗口加 -rust，避免覆盖 Python 版同名入口。
install_file 755 "$BUILD_DIR/networkmgr" "$STAGE/usr/local/bin/networkmgr-rust"
install_file 755 "$BUILD_DIR/config-window" \
    "$STAGE/usr/local/bin/networkmgr_configuration-rust"
install_file 755 "$BUILD_DIR/networkmgr-devd" "$STAGE/usr/local/bin/networkmgr-devd"
install_file 755 "$BUILD_DIR/dump-state" "$STAGE/usr/local/bin/dump-state"
install_file 755 "$BUILD_DIR/apply-config" "$STAGE/usr/local/bin/apply-config"

# 共存版 desktop：放 applications，不写 autostart，默认不抢 Python 版的自启动。
install_file 644 "$REPO_ROOT/src/networkmgr-rust.desktop" \
    "$STAGE/usr/local/share/applications/networkmgr-rust.desktop"
# sudoers 独立文件，只放行 networkmgr-rust。
install_file 440 "$REPO_ROOT/src/sudoers.d/networkmgr-rust" \
    "$STAGE/usr/local/etc/sudoers.d/networkmgr-rust"
# devd 规则以 .sample 安装：与 Python 版规则同时启用会重复处理事件，
# 切换时由用户复制成 .conf。
install_file 644 "$REPO_ROOT/src/networkmgr-rust.conf.sample" \
    "$STAGE/usr/local/etc/devd/networkmgr-rust.conf.sample"
# wpa 模板放独立目录，不覆盖 Python 版的 share/networkmgr。
install_file 644 "$REPO_ROOT/src/wpa_supplicant.conf" \
    "$STAGE/usr/local/share/networkmgr-rust/wpa_supplicant.conf"

# 应用图标改名为 networkmgr-rust；不安装 nm-*（Python 版正在使用）。
for source_dir in "$REPO_ROOT"/src/icons/*/; do
    size=$(basename "$source_dir")
    if [ "$size" = "scalable" ]; then
        mkdir -p "$STAGE/usr/local/share/icons/hicolor/scalable/apps"
        if [ -f "$source_dir/apps/networkmgr.svg" ]; then
            cp "$source_dir/apps/networkmgr.svg" \
                "$STAGE/usr/local/share/icons/hicolor/scalable/apps/networkmgr-rust.svg"
        fi
        if [ -f "$source_dir/apps/networkmgr-small.svg" ]; then
            cp "$source_dir/apps/networkmgr-small.svg" \
                "$STAGE/usr/local/share/icons/hicolor/scalable/apps/networkmgr-rust-small.svg"
        fi
        continue
    fi
    if [ -f "$source_dir/apps/networkmgr.png" ]; then
        mkdir -p "$STAGE/usr/local/share/icons/hicolor/$size/apps"
        cp "$source_dir/apps/networkmgr.png" \
            "$STAGE/usr/local/share/icons/hicolor/$size/apps/networkmgr-rust.png"
    fi
done

# gettext：独立 domain，避免覆盖 Python 版的 NetworkMgr.mo。
for source in "$REPO_ROOT"/po/*.po; do
    lang=$(basename "$source" .po)
    target="$STAGE/usr/local/share/locale/$lang/LC_MESSAGES"
    mkdir -p "$target"
    msgfmt "$source" -o "$target/NetworkMgr-rust.mo"
done

# plist 是相对 PREFIX（/usr/local）的路径列表。
(
    cd "$STAGE/usr/local"
    find . -type f | sed 's|^\./||' | sort
) > "$STAGE/plist"

cat > "$STAGE/+MANIFEST" <<MANIFEST
name: networkmgr-rust
version: "$VERSION"
origin: "net-mgmt/networkmgr-rust"
comment: "Network manager for FreeBSD desktops (Rust rewrite)"
maintainer: "wenyinos@users.noreply.github.com"
www: "https://github.com/wenyinos/networkmgr"
prefix: "/usr/local"
desc: <<EOD
NetworkMgr is a graphical network manager for FreeBSD, GhostBSD and
DragonFlyBSD desktops. This is the Rust rewrite. It installs alongside
the Python-based net-mgmt/networkmgr package: binaries, desktop entry,
sudoers, devd sample, icons and gettext domain all use the -rust suffix.
EOD
MANIFEST

mkdir -p "$OUT_DIR"
pkg create -M "$STAGE/+MANIFEST" -r "$STAGE" -p "$STAGE/plist" \
    -o "$OUT_DIR" -f txz

for package in "$OUT_DIR"/networkmgr-rust-*.pkg; do
    case "$package" in
        *-"$ARCH".pkg) ;;
        *) mv "$package" "${package%.pkg}-$ARCH.pkg" ;;
    esac
done

ls -lh "$OUT_DIR"/networkmgr-rust-*.pkg
echo "built networkmgr-rust $VERSION for $ARCH"
