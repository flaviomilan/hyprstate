#!/bin/sh
# Installs the latest hyprstate release binary.
#
#   curl -fsSL https://raw.githubusercontent.com/flaviomilan/hyprstate/main/install.sh | sh
#
# HYPRSTATE_VERSION      release to install, e.g. v0.2.0 (default: latest)
# HYPRSTATE_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
set -eu

repo="flaviomilan/hyprstate"
dir="${HYPRSTATE_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'hyprstate: %s\n' "$*" >&2; }
die() { say "$*"; exit 1; }

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1"; }
    fetch_to() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO- "$1"; }
    fetch_to() { wget -qO "$2" "$1"; }
else
    die "needs curl or wget"
fi
for tool in tar sha256sum uname; do
    command -v "$tool" >/dev/null 2>&1 || die "needs $tool"
done

[ "$(uname -s)" = Linux ] || die "only Linux is supported (Hyprland is Linux-only)"
case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) die "no prebuilt binary for $(uname -m); try: cargo install --locked hyprstate" ;;
esac
target="$arch-unknown-linux-musl"

version="${HYPRSTATE_VERSION:-}"
if [ -z "$version" ]; then
    version=$(fetch "https://api.github.com/repos/$repo/releases/latest" |
        sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$version" ] || die "could not find the latest release"
fi
case "$version" in v*) ;; *) version="v$version" ;; esac

name="hyprstate-$version-$target"
url="https://github.com/$repo/releases/download/$version/$name.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

say "downloading $name"
fetch_to "$url" "$tmp/$name.tar.gz" || die "download failed: $url"
fetch_to "$url.sha256" "$tmp/$name.tar.gz.sha256" || die "download failed: $url.sha256"
(cd "$tmp" && sha256sum -c --quiet "$name.tar.gz.sha256") || die "checksum mismatch; not installing"
tar -xzf "$tmp/$name.tar.gz" -C "$tmp"

mkdir -p "$dir"
install -m 755 "$tmp/$name/hyprstate" "$dir/hyprstate"
say "installed $("$dir/hyprstate" --version) to $dir/hyprstate"

case ":${PATH:-}:" in
    *":$dir:"*) ;;
    *) say "note: $dir is not on your PATH; add it to run 'hyprstate'" ;;
esac
