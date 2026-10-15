#!/bin/sh
# Installs aether from its latest release, or from AETHER_VERSION (e.g. v0.1.1):
#   curl -fsSL https://raw.githubusercontent.com/tarikee/aether/main/install.sh | sh
# It goes to AETHER_INSTALL_DIR, by default /usr/local/bin (with sudo only when needed).
# Running it again updates aether, in the folder where the PATH finds it.
set -eu

repo=tarikee/aether

say() { printf '%s\n' "$*"; }
fail() {
    printf 'aether install: %s\n' "$*" >&2
    exit 1
}

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" "$1"
    else
        fail "needs curl or wget"
    fi
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

main() {
    build="build it with: cargo install --git https://github.com/$repo"
    case $(uname -s) in
        Linux) os=linux ;;
        Darwin) os=macos ;;
        *) fail "no ready program for $(uname -s); $build" ;;
    esac
    case $(uname -m) in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=arm64 ;;
        *) fail "no ready program for $(uname -m); $build" ;;
    esac
    # A shell under Rosetta says x86_64 on an Apple silicon Mac.
    if [ "$os" = macos ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
        arch=arm64
    fi
    name="aether-$os-$arch"

    version=${AETHER_VERSION:-latest}
    if [ "$version" = latest ]; then
        base="https://github.com/$repo/releases/latest/download"
    else
        base="https://github.com/$repo/releases/download/$version"
    fi

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 1' INT TERM

    say "Downloading $name ($version)..."
    fetch "$base/$name.tar.gz" "$tmp/$name.tar.gz" || fail "could not download $base/$name.tar.gz"
    fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || fail "could not download $base/SHA256SUMS"
    want=$(grep " $name.tar.gz\$" "$tmp/SHA256SUMS" | cut -d' ' -f1 || true)
    [ -n "$want" ] || fail "SHA256SUMS has no line for $name.tar.gz"
    [ "$(sha256 "$tmp/$name.tar.gz")" = "$want" ] || fail "$name.tar.gz does not match SHA256SUMS"
    tar -xzf "$tmp/$name.tar.gz" -C "$tmp"

    dir=${AETHER_INSTALL_DIR:-}
    if [ -z "$dir" ]; then
        # Running it again updates the aether already on the PATH, where it is, when that
        # folder can be written without sudo (a copy cargo manages is left to cargo).
        found=$(command -v aether 2>/dev/null || true)
        case $found in
            /*/aether)
                old=${found%/aether}
                if [ -w "$old" ] && [ "$old" != "$HOME/.cargo/bin" ]; then
                    dir=$old
                fi
                ;;
        esac
    fi
    dir=${dir:-/usr/local/bin}
    sudo=
    if ! mkdir -p "$dir" 2>/dev/null || [ ! -w "$dir" ]; then
        [ -z "${AETHER_INSTALL_DIR:-}" ] || fail "cannot write to $dir"
        if command -v sudo >/dev/null 2>&1; then
            say "Installing to $dir needs sudo:"
            sudo=sudo
        else
            dir="$HOME/.local/bin"
        fi
    fi
    $sudo mkdir -p "$dir"
    $sudo cp "$tmp/$name/aether" "$dir/aether.new"
    $sudo chmod 755 "$dir/aether.new"
    $sudo mv -f "$dir/aether.new" "$dir/aether"
    if [ "$os" = macos ]; then
        $sudo xattr -d com.apple.quarantine "$dir/aether" 2>/dev/null || true
    fi

    say "Installed $("$dir/aether" version) to $dir/aether"
    case ":$PATH:" in
        *":$dir:"*)
            found=$(command -v aether || true)
            if [ "$found" != "$dir/aether" ]; then
                say "Another aether comes first on your PATH: $found"
            else
                say "Type aether to start it."
            fi
            ;;
        *) say "$dir is not on your PATH; add it to your shell's profile, then open a new terminal." ;;
    esac
}

main "$@"
