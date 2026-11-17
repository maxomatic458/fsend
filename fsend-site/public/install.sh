#!/bin/sh
# Installs the fsend CLI from the latest GitHub release, or removes it.
#
#   curl -fsSL https://fsend.sh/install.sh | sh
#   curl -fsSL https://fsend.sh/install.sh | sh -s -- --uninstall
set -eu

RELEASES="https://github.com/maxomatic458/fsend/releases/latest/download"

say() { printf 'fsend: %s\n' "$*"; }
fail() {
    printf 'fsend: error: %s\n' "$*" >&2
    exit 1
}
has() { command -v "$1" >/dev/null 2>&1; }

usage() {
    cat <<EOF
Usage: install.sh [--dir DIR] [--uninstall]

Installs the fsend CLI into DIR, /usr/local/bin by default, or removes it.
EOF
}

target() {
    case $(uname -s) in
        Linux) os=unknown-linux-musl ;;
        Darwin) os=apple-darwin ;;
        *) fail "no build for $(uname -s); see https://fsend.sh/download" ;;
    esac
    case $(uname -m) in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) fail "no build for $(uname -m); see https://fsend.sh/download" ;;
    esac
    # A shell under Rosetta reports x86_64 on Apple silicon.
    if [ "$os" = apple-darwin ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then
        arch=aarch64
    fi
    echo "$arch-$os"
}

download() {
    if has curl; then
        curl --proto '=https' --tlsv1.2 -fsSL "$1" -o "$2"
    elif has wget; then
        wget -qO "$2" "$1"
    else
        fail "curl or wget is needed"
    fi
}

in_dir() {
    existing=$dir
    while [ ! -d "$existing" ]; do existing=$(dirname "$existing"); done
    if [ -w "$existing" ]; then
        "$@"
    elif has sudo; then
        sudo "$@"
    else
        fail "no permission to write to $dir. Run as root or pass --dir"
    fi
}

install_fsend() {
    if has sha256sum; then
        sha256=sha256sum
    elif has shasum; then
        sha256="shasum -a 256"
    else
        fail "sha256sum or shasum is needed"
    fi

    target=$(target)
    archive="fsend-$target.tar.gz"
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    say "downloading $archive"
    download "$RELEASES/$archive" "$tmp/$archive"
    # Named after the archive without extension.
    download "$RELEASES/fsend-$target.sha256" "$tmp/checksum"
    expected=$(cut -d ' ' -f 1 "$tmp/checksum")
    actual=$($sha256 "$tmp/$archive" | cut -d ' ' -f 1)
    [ "$actual" = "$expected" ] || fail "$archive does not match its checksum"

    tar -xzf "$tmp/$archive" -C "$tmp"
    in_dir mkdir -p "$dir"
    in_dir install -m 0755 "$tmp/fsend-$target/fsend" "$dir/fsend"
    say "installed $("$dir/fsend" version | head -n 1) to $dir/fsend"

    found=$(command -v fsend || true)
    if [ -z "$found" ]; then
        say "add $dir to your PATH to run fsend"
    elif [ "$found" != "$dir/fsend" ]; then
        say "another fsend at $found comes first on your PATH"
    fi
}

uninstall_fsend() {
    if [ ! -e "$dir/fsend" ]; then
        say "nothing to remove at $dir/fsend"
        return
    fi
    in_dir rm -f "$dir/fsend"
    say "removed $dir/fsend"
}

main() {
    dir=/usr/local/bin
    action=install_fsend
    while [ $# -gt 0 ]; do
        case $1 in
            --dir)
                [ $# -gt 1 ] || fail "--dir needs a folder"
                dir=$2
                shift
                ;;
            --uninstall) action=uninstall_fsend ;;
            -h | --help)
                usage
                return
                ;;
            *) fail "unknown option $1; see --help" ;;
        esac
        shift
    done
    "$action"
}

main "$@"
