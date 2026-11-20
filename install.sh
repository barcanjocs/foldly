#!/bin/sh
# Installs the latest foldly release for this machine:
#
#   curl -fsSL https://raw.githubusercontent.com/barcanjocs/foldly/main/install.sh | sh
#
# Environment overrides:
#   FOLDLY_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
#   FOLDLY_VERSION      a release tag such as v0.1.0 (default: latest)

set -eu

REPO="barcanjocs/foldly"
INSTALL_DIR="${FOLDLY_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${FOLDLY_VERSION:-latest}"

err() {
    echo "foldly install: $*" >&2
    exit 1
}

case "$(uname -s)" in
    Linux) os="unknown-linux-gnu" ;;
    Darwin) os="apple-darwin" ;;
    *) err "unsupported OS $(uname -s); foldly supports Linux and macOS" ;;
esac

case "$(uname -m)" in
    x86_64 | amd64) arch="x86_64" ;;
    arm64 | aarch64) arch="aarch64" ;;
    *) err "unsupported CPU $(uname -m); foldly supports x86_64 and arm64" ;;
esac

# A shell running under Rosetta reports x86_64 on Apple silicon; install the native build.
if [ "$os" = "apple-darwin" ] && [ "$arch" = "x86_64" ] &&
    [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then
    arch="aarch64"
fi

asset="foldly-$arch-$os.tar.gz"
if [ "$VERSION" = "latest" ]; then
    base="https://github.com/$REPO/releases/latest/download"
else
    base="https://github.com/$REPO/releases/download/$VERSION"
fi

command -v curl >/dev/null 2>&1 || err "curl is required"
command -v tar >/dev/null 2>&1 || err "tar is required"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $asset ($VERSION)..."
curl -fsSL "$base/$asset" -o "$tmp/$asset" || err "download failed: $base/$asset"
curl -fsSL "$base/$asset.sha256" -o "$tmp/$asset.sha256" || err "checksum download failed: $base/$asset.sha256"

expected="$(cut -d ' ' -f 1 <"$tmp/$asset.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/$asset" | cut -d ' ' -f 1)"
else
    actual="$(shasum -a 256 "$tmp/$asset" | cut -d ' ' -f 1)"
fi
[ "$expected" = "$actual" ] || err "checksum mismatch for $asset; not installing"

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$INSTALL_DIR"
mv -f "$tmp/foldly" "$INSTALL_DIR/foldly"
chmod 755 "$INSTALL_DIR/foldly"

echo "Installed $("$INSTALL_DIR/foldly" --version 2>/dev/null || echo foldly) to $INSTALL_DIR/foldly"

# The Linux build plays audio through ALSA, which desktop distributions include.
if [ "$os" = "unknown-linux-gnu" ] && command -v ldconfig >/dev/null 2>&1 &&
    ! ldconfig -p 2>/dev/null | grep -q 'libasound\.so\.2'; then
    echo
    echo "foldly needs the ALSA library to play audio. Install it with one of:"
    echo "  sudo apt install libasound2      # Debian, Ubuntu"
    echo "  sudo dnf install alsa-lib        # Fedora"
    echo "  sudo pacman -S alsa-lib          # Arch"
fi

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        echo
        echo "$INSTALL_DIR is not on your PATH. Add it, e.g.:"
        echo "  bash/zsh: add  export PATH=\"$INSTALL_DIR:\$PATH\"  to ~/.bashrc or ~/.zshrc"
        echo "  fish:     fish_add_path $INSTALL_DIR"
        ;;
esac

echo
echo "Start it with:  foldly ~/Music"
