#!/usr/bin/env bash
# Builds the Windows installer (dist/FerrySetup-<version>.exe) on Linux.
#
# One-time setup:
#   Debian/Ubuntu: sudo apt install mingw-w64 nsis
#   Fedora:        sudo dnf install mingw64-gcc mingw32-nsis
#   Arch:          sudo pacman -S mingw-w64-gcc nsis
#   plus Rust:     rustup target add x86_64-pc-windows-gnu   (done automatically below)
set -euo pipefail
cd "$(dirname "$0")"

TARGET=x86_64-pc-windows-gnu
die() { echo "error: $*" >&2; exit 1; }

# Files from older Ferry versions that were unpacked over this folder
if [ -f src/sys.rs ] && [ -d src/sys ]; then
    echo "==> Removing src/sys.rs left over from an older Ferry version"
    rm -f src/sys.rs
fi

command -v x86_64-w64-mingw32-gcc >/dev/null \
    || die "mingw-w64 missing: sudo apt install mingw-w64   (Fedora: sudo dnf install mingw64-gcc)"
command -v makensis >/dev/null \
    || die "NSIS missing: sudo apt install nsis   (Fedora: sudo dnf install mingw32-nsis)"

# ---- a Rust toolchain that has the Windows standard library -----------------
has_target() { # $@ = rustc command
    local root
    root=$("$@" --print sysroot 2>/dev/null) || return 1
    [ -d "$root/lib/rustlib/$TARGET" ]
}
RUSTUP=$(command -v rustup || true)
[ -z "$RUSTUP" ] && [ -x "$HOME/.cargo/bin/rustup" ] && RUSTUP="$HOME/.cargo/bin/rustup"
CARGO=(cargo)
if ! command -v cargo >/dev/null || ! has_target rustc; then
    if [ -n "$RUSTUP" ]; then
        # Use rustup's stable toolchain explicitly: a distro-packaged cargo/rustc earlier
        # in PATH cannot use targets added by rustup.
        echo "==> Preparing Rust stable + $TARGET via rustup (one time)"
        "$RUSTUP" toolchain install stable --profile minimal >/dev/null
        "$RUSTUP" target add --toolchain stable "$TARGET"
        CARGO=("$RUSTUP" run stable cargo)
        has_target "$RUSTUP" run stable rustc || die "rustup could not install $TARGET"
    else
        die "your Rust has no Windows target and rustup is not installed.
  Either install rustup:   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  or (Fedora) install:     sudo dnf install rust-std-static-x86_64-pc-windows-gnu
  then run this script again."
    fi
fi

VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
echo "==> Building ferry $VERSION for Windows"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
export WINDRES=${WINDRES:-x86_64-w64-mingw32-windres}
# Link the mingw runtime statically so the .exe needs no extra DLLs on Windows.
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-C target-feature=+crt-static"
"${CARGO[@]}" build --release --target "$TARGET"

BIN="$PWD/target/$TARGET/release"
mkdir -p dist
OUT="$PWD/dist/FerrySetup-$VERSION.exe"

# Optional code signing (removes the SmartScreen warning once the certificate has reputation,
# and makes Defender's heuristics far less likely to flag Ferry). See WINDOWS-DEFENDER.md.
sign() {
    [ -n "${FERRY_SIGN_PFX:-}" ] || return 0
    command -v osslsigncode >/dev/null || die "FERRY_SIGN_PFX is set but osslsigncode is missing (sudo apt install osslsigncode)"
    osslsigncode sign -pkcs12 "$FERRY_SIGN_PFX" -pass "${FERRY_SIGN_PASS:-}" -h sha256 \
        -n "Ferry" ${FERRY_SIGN_URL:+-i "$FERRY_SIGN_URL"} -t "${FERRY_SIGN_TIMESTAMP:-http://timestamp.digicert.com}" \
        -in "$1" -out "$1.signed" && mv "$1.signed" "$1"
    echo "    signed $(basename "$1")"
}
if [ -n "${FERRY_SIGN_PFX:-}" ]; then
    echo "==> Signing the programs"
    sign "$BIN/ferry.exe"
    sign "$BIN/ferryd.exe"
fi

echo "==> Packaging installer"
( cd packaging/windows && makensis -V2 -DVERSION="$VERSION" -DSRC="$BIN" -DOUTFILE="$OUT" ferry.nsi )
if [ -n "${FERRY_SIGN_PFX:-}" ]; then
    echo "==> Signing the installer"
    sign "$OUT"
fi

echo "==> Done: $OUT ($(du -h "$OUT" | cut -f1))"
if [ -z "${FERRY_SIGN_PFX:-}" ]; then
    echo "    Not code-signed: see WINDOWS-DEFENDER.md for SmartScreen / Defender warnings."
fi
