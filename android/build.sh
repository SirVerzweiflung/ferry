#!/usr/bin/env bash
# Builds the Ferry Android app (Ferry.apk) with as little setup as possible.
#
#   ./build.sh            build Ferry.apk
#   ./build.sh install    build and install on a USB-connected phone (adb, USB debugging on)
#
# Needs nothing but curl/tar/unzip: a JDK 17-21 is used if installed (system, SDKMAN,
# Android Studio's bundled one), otherwise Temurin 21 is downloaded once. Newer JDKs
# (22+, e.g. Java 25) are skipped because the Android Gradle plugin cannot run on them yet.
# The Android SDK is used from $ANDROID_HOME or ~/Android/Sdk (Android Studio); if neither
# exists, the command-line SDK (~300 MB) is downloaded once to ~/.local/share/ferry-android-sdk.
set -euo pipefail
cd "$(dirname "$0")"

CMDLINE_TOOLS_ZIP="commandlinetools-linux-11076708_latest.zip"
PLATFORM="platforms;android-35"
BUILD_TOOLS="build-tools;35.0.0"
TOOLS="$HOME/.local/share/ferry-android-sdk"

die() { echo "error: $*" >&2; exit 1; }

# ---- JDK (17..21) ------------------------------------------------------------
java_major() {
    "$1/bin/java" -version 2>&1 | sed -n 's/.* version "\([0-9][0-9]*\).*/\1/p' | head -1
}
pick_jdk() {
    local cands=() d v
    [ -n "${JAVA_HOME:-}" ] && cands+=("$JAVA_HOME")
    if command -v javac >/dev/null 2>&1; then
        cands+=("$(dirname "$(dirname "$(readlink -f "$(command -v javac)")")")")
    fi
    cands+=("$TOOLS/jdk" /usr/lib/jvm/* /usr/java/* "$HOME"/.sdkman/candidates/java/* \
            /opt/android-studio/jbr "$HOME"/android-studio/jbr /snap/android-studio/current/jbr \
            /usr/local/android-studio/jbr "$HOME"/.jdks/*)
    for d in "${cands[@]}"; do
        [ -x "$d/bin/javac" ] || continue
        v=$(java_major "$d")
        if [ -n "$v" ] && [ "$v" -ge 17 ] && [ "$v" -le 21 ]; then echo "$d"; return 0; fi
    done
    return 1
}
if ! JDK=$(pick_jdk); then
    case "$(uname -m)" in
        x86_64|amd64) ARCH=x64 ;;
        aarch64|arm64) ARCH=aarch64 ;;
        *) die "no JDK 17-21 found; please install one (e.g. sudo apt install openjdk-21-jdk)" ;;
    esac
    echo "==> No JDK 17-21 found (Java 22+ is too new for the Android build). Downloading Temurin 21 (one time)"
    mkdir -p "$TOOLS"
    tmp=$(mktemp -d)
    curl -fL --progress-bar -o "$tmp/jdk.tar.gz" \
        "https://api.adoptium.net/v3/binary/latest/21/ga/linux/$ARCH/jdk/hotspot/normal/eclipse"
    tar -xzf "$tmp/jdk.tar.gz" -C "$tmp"
    rm -rf "$TOOLS/jdk"
    mv "$tmp"/jdk-21* "$TOOLS/jdk"
    rm -rf "$tmp"
    JDK="$TOOLS/jdk"
fi
export JAVA_HOME="$JDK"
export PATH="$JAVA_HOME/bin:$PATH"
echo "==> Using JDK $(java_major "$JDK") at $JAVA_HOME"
# Stop any Gradle daemon that was started with the wrong Java.
[ -x ./gradlew ] && ./gradlew --stop >/dev/null 2>&1 || true

# ---- Android SDK -----------------------------------------------------------
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
[ -z "$SDK" ] && [ -d "$HOME/Android/Sdk" ] && SDK="$HOME/Android/Sdk"
[ -z "$SDK" ] && SDK="$TOOLS"
SDKMANAGER=""
for c in "$SDK/cmdline-tools/latest/bin/sdkmanager" "$SDK"/cmdline-tools/*/bin/sdkmanager; do
    [ -x "$c" ] && SDKMANAGER="$c" && break
done
if [ -z "$SDKMANAGER" ]; then
    echo "==> Downloading Android command-line tools to $SDK"
    mkdir -p "$SDK/cmdline-tools"
    tmp=$(mktemp -d)
    curl -fL --progress-bar -o "$tmp/tools.zip" "https://dl.google.com/android/repository/$CMDLINE_TOOLS_ZIP"
    command -v unzip >/dev/null || die "unzip missing (sudo apt install unzip)"
    unzip -q "$tmp/tools.zip" -d "$tmp"
    rm -rf "$SDK/cmdline-tools/latest"
    mv "$tmp/cmdline-tools" "$SDK/cmdline-tools/latest"
    rm -rf "$tmp"
    SDKMANAGER="$SDK/cmdline-tools/latest/bin/sdkmanager"
fi
if [ ! -d "$SDK/platforms/android-35" ] || [ ! -d "$SDK/build-tools/35.0.0" ]; then
    echo "==> Installing SDK platform + build tools (one time)"
    yes | "$SDKMANAGER" --sdk_root="$SDK" --licenses >/dev/null || true
    "$SDKMANAGER" --sdk_root="$SDK" "$PLATFORM" "$BUILD_TOOLS" "platform-tools"
fi
echo "sdk.dir=$SDK" > local.properties
export ANDROID_HOME="$SDK"

# ---- signing key (generated once, kept so updates install over old versions) ----
KEYDIR="$HOME/.config/ferry"
KS="$KEYDIR/android-release.jks"
PASSFILE="$KEYDIR/android-release.pass"
mkdir -p "$KEYDIR"; chmod 700 "$KEYDIR"
if [ ! -f "$KS" ]; then
    echo "==> Creating your personal signing key ($KS)"
    head -c 24 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' > "$PASSFILE"
    chmod 600 "$PASSFILE"
    keytool -genkeypair -noprompt -keystore "$KS" -storetype PKCS12 -alias ferry \
        -keyalg RSA -keysize 3072 -validity 36500 -dname "CN=Ferry personal build" \
        -storepass "$(cat "$PASSFILE")" -keypass "$(cat "$PASSFILE")" >/dev/null
fi

# ---- build -------------------------------------------------------------------
echo "==> Building"
./gradlew --no-daemon -q assembleRelease -PferryKeystore="$KS" -PferryStorePass="$(cat "$PASSFILE")"
cp app/build/outputs/apk/release/app-release.apk Ferry.apk
echo "==> Built $(pwd)/Ferry.apk ($(du -h Ferry.apk | cut -f1))"

if [ "${1:-}" = "install" ]; then
    ADB="$SDK/platform-tools/adb"
    [ -x "$ADB" ] || ADB=adb
    "$ADB" install -r Ferry.apk
    "$ADB" shell am start -n dev.ferry/.app.MainActivity >/dev/null || true
    echo "==> Installed and started on the phone"
else
    echo "    Install: copy Ferry.apk to the phone and open it, or run: ./build.sh install"
fi
