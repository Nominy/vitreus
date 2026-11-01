#!/bin/sh
# Packages a built binary into the release archive the installers download.
#
#   scripts/package.sh <linux|macos|windows> <x64|arm64> <binary> <out-dir>
#
# linux   -> vitreus-linux-<arch>.tar.gz   (vitreus, vitreus.png, LICENSE)
# macos   -> vitreus-macos-<arch>.tar.gz   (Vitreus.app, ad-hoc signed)
# windows -> vitreus-windows-<arch>.zip    (vitreus.exe, LICENSE)
# Prints the archive path.
set -eu

[ $# -eq 4 ] || { echo "usage: $0 <linux|macos|windows> <x64|arm64> <binary> <out-dir>" >&2; exit 2; }
os="$1"; arch="$2"; bin="$3"; out="$4"
root="$(cd "$(dirname "$0")/.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)"
name="vitreus-$os-$arch"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$out"
out="$(cd "$out" && pwd)"

case "$os" in
    linux)
        install -m 755 "$bin" "$stage/vitreus"
        cp "$root/assets/app/vitreus-256.png" "$stage/vitreus.png"
        cp "$root/LICENSE" "$stage/LICENSE"
        tar -czf "$out/$name.tar.gz" -C "$stage" vitreus vitreus.png LICENSE
        echo "$out/$name.tar.gz"
        ;;
    macos)
        app="$stage/Vitreus.app"
        mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
        install -m 755 "$bin" "$app/Contents/MacOS/vitreus"
        sed "s/@VERSION@/$version/g" "$root/assets/macos/Info.plist" >"$app/Contents/Info.plist"
        iconset="$stage/vitreus.iconset"
        mkdir -p "$iconset"
        for size in 16 32 128 256 512; do
            sips -z "$size" "$size" "$root/assets/app/vitreus.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
            sips -z "$((size * 2))" "$((size * 2))" "$root/assets/app/vitreus.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
        done
        iconutil -c icns "$iconset" -o "$app/Contents/Resources/vitreus.icns"
        rm -rf "$iconset"
        # Apple Silicon refuses unsigned code; an ad-hoc signature is enough for local installs.
        codesign --force --deep --sign - "$app"
        tar -czf "$out/$name.tar.gz" -C "$stage" Vitreus.app
        echo "$out/$name.tar.gz"
        ;;
    windows)
        cp "$bin" "$stage/vitreus.exe"
        cp "$root/LICENSE" "$stage/LICENSE"
        rm -f "$out/$name.zip"
        (cd "$stage" && 7z a -tzip "$out/$name.zip" vitreus.exe LICENSE >/dev/null)
        echo "$out/$name.zip"
        ;;
    *)
        echo "unknown os: $os" >&2
        exit 2
        ;;
esac
