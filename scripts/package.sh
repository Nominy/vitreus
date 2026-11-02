#!/bin/sh
# Packages a built binary into the release archive the installers download.
#
#   scripts/package.sh <linux|macos|windows> <x64|arm64> <binary> <out-dir>
#
# linux   -> vitreus-linux-<arch>.tar.gz   (vitreus, vitreus.png, LICENSE)
#            vitreus-linux-<arch>.deb/.rpm (when nfpm is installed)
# macos   -> vitreus-macos-<arch>.tar.gz   (Vitreus.app, ad-hoc signed)
#            vitreus-macos-<arch>.dmg      (drag-to-Applications disk image)
# windows -> vitreus-windows-<arch>.zip    (vitreus.exe, LICENSE)
#            vitreus-setup-<arch>.exe      (when Inno Setup's iscc is installed;
#                                           set ISCC=path\to\ISCC.exe if not on PATH)
# Prints the path of each file it made.
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

# Windows tools want C:\ paths, not the /c/... ones of Git Bash.
native() {
    if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else echo "$1"; fi
}

case "$os" in
    linux)
        install -m 755 "$bin" "$stage/vitreus"
        cp "$root/assets/app/vitreus-256.png" "$stage/vitreus.png"
        cp "$root/LICENSE" "$stage/LICENSE"
        tar -czf "$out/$name.tar.gz" -C "$stage" vitreus vitreus.png LICENSE
        echo "$out/$name.tar.gz"
        if command -v nfpm >/dev/null 2>&1; then
            case "$arch" in x64) pkg_arch=amd64 ;; *) pkg_arch="$arch" ;; esac
            sed -e "s|@VERSION@|$version|; s|@ARCH@|$pkg_arch|; s|@BINARY@|$stage/vitreus|; s|@ROOT@|$root|g" \
                "$root/assets/linux/nfpm.yaml" >"$stage/nfpm.yaml"
            for kind in deb rpm; do
                nfpm package --config "$stage/nfpm.yaml" --packager "$kind" --target "$out/$name.$kind" >/dev/null
                echo "$out/$name.$kind"
            done
        else
            echo "note: nfpm not found, skipping $name.deb and $name.rpm" >&2
        fi
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
        # Drag-to-Applications disk image.
        dmg="$stage/dmg"
        mkdir -p "$dmg"
        cp -R "$app" "$dmg/"
        ln -s /Applications "$dmg/Applications"
        rm -f "$out/$name.dmg"
        hdiutil create -quiet -volname Vitreus -srcfolder "$dmg" -fs HFS+ -format UDZO "$out/$name.dmg"
        echo "$out/$name.dmg"
        ;;
    windows)
        cp "$bin" "$stage/vitreus.exe"
        cp "$root/LICENSE" "$stage/LICENSE"
        rm -f "$out/$name.zip"
        (cd "$stage" && 7z a -tzip "$out/$name.zip" vitreus.exe LICENSE >/dev/null)
        echo "$out/$name.zip"
        iscc="${ISCC:-$(command -v iscc || command -v ISCC.exe || true)}"
        if [ -n "$iscc" ] && command -v cygpath >/dev/null 2>&1; then iscc="$(cygpath -u "$iscc")"; fi
        if [ -n "$iscc" ]; then
            # MSYS would rewrite iscc's /D... switches as if they were paths.
            MSYS2_ARG_CONV_EXCL='*' "$iscc" /Q "/DAppVersion=$version" "/DBinary=$(native "$stage/vitreus.exe")" \
                "/DRoot=$(native "$root")" "/O$(native "$out")" "$(native "$root/assets/windows/vitreus.iss")"
            echo "$out/vitreus-setup-$arch.exe"
        else
            echo "note: Inno Setup (iscc) not found, skipping vitreus-setup-$arch.exe" >&2
        fi
        ;;
    *)
        echo "unknown os: $os" >&2
        exit 2
        ;;
esac
