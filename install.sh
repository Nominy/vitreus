#!/bin/sh
# Vitreus installer for macOS and Linux.
#
#   curl -fsSL https://nominy.github.io/vitreus/install | sh
#   curl -fsSL https://nominy.github.io/vitreus/install | sh -s -- --version v0.1.0
#   curl -fsSL https://nominy.github.io/vitreus/install | sh -s -- --help
#
# Environment: VITREUS_VERSION, VITREUS_REPO (owner/name), VITREUS_DOWNLOAD_URL
# (base URL holding the release assets; overrides GitHub).
set -eu

APP="Vitreus"
BIN="vitreus"
BUNDLE_ID="io.github.nominy.vitreus"
REPO="${VITREUS_REPO:-Nominy/vitreus}"
VERSION="${VITREUS_VERSION:-latest}"

set_default=1
system=0
source_build=0
archive=""
uninstall=0

usage() {
    cat <<'EOF'
Vitreus installer for macOS and Linux

  curl -fsSL https://nominy.github.io/vitreus/install | sh -s -- [options]

  --version <tag>   install a specific release (default: latest)
  --no-default      don't make Vitreus the default photo viewer
  --system          Linux: install for all users under /usr/local (uses sudo)
  --source          build from source with cargo instead of downloading
  --archive <file>  install a release archive you already have
  --uninstall       remove Vitreus
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || { echo "--version needs a tag" >&2; exit 2; }; VERSION="$2"; shift ;;
        --version=*) VERSION="${1#*=}" ;;
        --no-default) set_default=0 ;;
        --system) system=1 ;;
        --source) source_build=1 ;;
        --archive) [ $# -ge 2 ] || { echo "--archive needs a file" >&2; exit 2; }; archive="$2"; shift ;;
        --uninstall) uninstall=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1 (see --help)" >&2; exit 2 ;;
    esac
    shift
done

if [ -t 1 ]; then
    BOLD="$(printf '\033[1m')"; CYAN="$(printf '\033[36m')"; YELLOW="$(printf '\033[33m')"
    RED="$(printf '\033[31m')"; DIM="$(printf '\033[2m')"; RESET="$(printf '\033[0m')"
else
    BOLD=""; CYAN=""; YELLOW=""; RED=""; DIM=""; RESET=""
fi
say() { printf '%s◆%s %s\n' "$CYAN" "$RESET" "$*"; }
note() { printf '  %s%s%s\n' "$DIM" "$*" "$RESET"; }
warn() { printf '%s!%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die() { printf '%s✗%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }

OS="$(uname -s)"
case "$OS" in
    Linux) PLATFORM="linux" ;;
    Darwin) PLATFORM="macos" ;;
    *) die "$OS isn't supported by this installer. On Windows run:  irm https://nominy.github.io/vitreus/install.ps1 | iex" ;;
esac

# On macOS ask the kernel, so a Rosetta shell still gets the native build.
host_arch() {
    if [ "$PLATFORM" = "macos" ]; then
        if [ "$(sysctl -in hw.optional.arm64 2>/dev/null || true)" = "1" ]; then echo arm64; else echo x64; fi
        return
    fi
    case "$(uname -m)" in
        x86_64|amd64) echo x64 ;;
        aarch64|arm64) echo arm64 ;;
        *) uname -m ;;
    esac
}
ARCH="$(host_arch)"
ASSET="$BIN-$PLATFORM-$ARCH.tar.gz"

TMP="$(mktemp -d 2>/dev/null || mktemp -d -t vitreus)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# Runs a command as root only when the destination isn't writable by us.
as_root_for() {
    target="$1"; shift
    probe="$target"
    while [ ! -e "$probe" ]; do probe="$(dirname "$probe")"; done
    if [ -w "$probe" ]; then "$@"
    elif has sudo; then sudo "$@"
    else die "No permission to write $target (and no sudo)."
    fi
}

download() {
    if has curl; then curl -fsSL --retry 3 -o "$2" "$1"
    elif has wget; then wget -q -O "$2" "$1"
    else die "Need curl or wget to download $APP."
    fi
}

sha256() {
    if has sha256sum; then sha256sum "$1" | cut -d' ' -f1
    elif has shasum; then shasum -a 256 "$1" | cut -d' ' -f1
    else echo ""
    fi
}

fetch_release() {
    if [ -n "${VITREUS_DOWNLOAD_URL:-}" ]; then base="${VITREUS_DOWNLOAD_URL%/}"
    elif [ "$VERSION" = "latest" ]; then base="https://github.com/$REPO/releases/latest/download"
    else base="https://github.com/$REPO/releases/download/$VERSION"
    fi
    say "Downloading $APP ${DIM}($VERSION, $PLATFORM-$ARCH)${RESET}"
    download "$base/$ASSET" "$TMP/$ASSET" \
        || die "No prebuilt $APP for $PLATFORM-$ARCH at $base. Try:  sh -s -- --source"
    download "$base/SHA256SUMS" "$TMP/SHA256SUMS" || die "Couldn't download SHA256SUMS from $base."
    expected="$(grep " $ASSET\$" "$TMP/SHA256SUMS" | cut -d' ' -f1)"
    actual="$(sha256 "$TMP/$ASSET")"
    [ -n "$expected" ] || die "$ASSET is not listed in SHA256SUMS."
    if [ -z "$actual" ]; then
        warn "No sha256sum/shasum found; skipping checksum verification."
    elif [ "$expected" != "$actual" ]; then
        die "Checksum mismatch for $ASSET (expected $expected, got $actual)."
    else
        note "checksum ok"
    fi
    archive="$TMP/$ASSET"
}

# Builds from this checkout (when the script is run from one) or a fresh clone.
build_from_source() {
    has cargo || die "Building needs Rust:  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    here="$(cd "$(dirname "$0")" 2>/dev/null && pwd || true)"
    if [ -n "$here" ] && [ -f "$here/Cargo.toml" ] && [ -f "$here/scripts/package.sh" ]; then
        src="$here"
    else
        has git || die "Building from source needs git."
        ref="main"; [ "$VERSION" = "latest" ] || ref="$VERSION"
        say "Cloning $REPO ($ref)"
        git clone --quiet --depth 1 --branch "$ref" "https://github.com/$REPO.git" "$TMP/src"
        src="$TMP/src"
    fi
    say "Building $APP from source ${DIM}(first build takes a few minutes)${RESET}"
    (cd "$src" && cargo build --release --locked) \
        || die "Build failed. Linux needs: a C toolchain, pkg-config and the xkbcommon, wayland, vulkan, xcb and fontconfig dev packages."
    sh "$src/scripts/package.sh" "$PLATFORM" "$ARCH" "$src/target/release/$BIN" "$TMP" >/dev/null
    archive="$TMP/$ASSET"
}

# ------------------------------------------------------------------- Linux

# Camera RAW MIME types (shared-mime-info names; image/x-dcraw is their common parent).
LINUX_RAW_MIMES="image/x-dcraw image/x-adobe-dng image/x-canon-cr2 image/x-canon-cr3 image/x-nikon-nef image/x-nikon-nrw image/x-sony-arw image/x-sony-sr2 image/x-sony-srf image/x-fuji-raf image/x-olympus-orf image/x-panasonic-rw image/x-panasonic-rw2 image/x-pentax-pef image/x-samsung-srw image/x-sigma-x3f image/x-hasselblad-3fr image/x-phaseone-iiq"
# shellcheck disable=SC2086
LINUX_MIMES="image/jpeg;image/png;image/apng;image/gif;image/webp;image/bmp;image/x-bmp;image/x-ms-bmp;image/tiff;image/x-icon;image/vnd.microsoft.icon;image/x-tga;image/x-targa;image/qoi;image/x-qoi;image/x-portable-anymap;image/x-portable-bitmap;image/x-portable-graymap;image/x-portable-pixmap;image/vnd.radiance;image/x-exr;image/x-dds;image/svg+xml;image/heic;image/heif;image/heic-sequence;image/heif-sequence;image/avif;image/jxl;image/vnd.adobe.photoshop;image/x-photoshop;image/x-psd;$(printf '%s;' $LINUX_RAW_MIMES)"
# Raster photo types we take over as default viewer; SVG stays with its current app.
LINUX_DEFAULT_MIMES="image/jpeg image/png image/apng image/gif image/webp image/bmp image/x-bmp image/x-ms-bmp image/tiff image/x-icon image/vnd.microsoft.icon image/x-tga image/qoi image/x-portable-anymap image/x-portable-bitmap image/x-portable-graymap image/x-portable-pixmap image/heic image/heif image/avif image/jxl image/vnd.adobe.photoshop $LINUX_RAW_MIMES"

linux_paths() {
    if [ "$1" = 1 ]; then prefix="/usr/local"; else prefix="$HOME/.local"; fi
    bin_path="$prefix/bin/$BIN"
    desktop_file="$prefix/share/applications/$BIN.desktop"
    icon_root="$prefix/share/icons/hicolor"
    icon_file="$icon_root/256x256/apps/$BIN.png"
}

linux_refresh() {
    if has update-desktop-database; then as_root_for "$prefix/share/applications" update-desktop-database -q "$prefix/share/applications" 2>/dev/null || true; fi
    if has gtk-update-icon-cache && [ -f "$icon_root/index.theme" ]; then as_root_for "$icon_root" gtk-update-icon-cache -q -t -f "$icon_root" 2>/dev/null || true; fi
}

linux_install() {
    linux_paths "$system"
    tar -xzf "$archive" -C "$TMP"
    [ -f "$TMP/$BIN" ] || die "Archive doesn't contain $BIN."

    as_root_for "$bin_path" mkdir -p "$(dirname "$bin_path")" "$(dirname "$desktop_file")" "$(dirname "$icon_file")"
    as_root_for "$bin_path" install -m 755 "$TMP/$BIN" "$bin_path"
    if [ -f "$TMP/$BIN.png" ]; then as_root_for "$icon_file" install -m 644 "$TMP/$BIN.png" "$icon_file"; fi
    cat >"$TMP/$BIN.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=$APP
GenericName=Photo Viewer
Comment=Fast, no-nonsense photo viewer
Exec=$bin_path %f
Icon=$BIN
Terminal=false
Categories=Graphics;Viewer;Photography;
MimeType=$LINUX_MIMES
StartupWMClass=$BIN
EOF
    as_root_for "$desktop_file" install -m 644 "$TMP/$BIN.desktop" "$desktop_file"
    linux_refresh
    say "Installed $BOLD$bin_path$RESET"

    if [ "$set_default" = 1 ]; then
        if has xdg-mime; then
            # shellcheck disable=SC2086
            xdg-mime default "$BIN.desktop" $LINUX_DEFAULT_MIMES && say "$APP is now your default photo viewer"
        else
            warn "xdg-mime not found: choose $APP under \"Open With\" to make it the default."
        fi
    fi

    if has ldd && ldd "$bin_path" 2>/dev/null | grep -q "not found"; then
        warn "Some system libraries are missing:"
        ldd "$bin_path" | grep "not found" | sed 's/^/    /' >&2
        note "Debian/Ubuntu: sudo apt install libxkbcommon-x11-0 libwayland-client0 libvulkan1 mesa-vulkan-drivers libfontconfig1"
        note "Fedora:        sudo dnf install libxkbcommon-x11 wayland vulkan-loader mesa-vulkan-drivers fontconfig"
        note "Arch:          sudo pacman -S libxkbcommon-x11 wayland vulkan-icd-loader fontconfig"
    fi
    case ":$PATH:" in *":$prefix/bin:"*) ;; *) note "Add $prefix/bin to PATH to run '$BIN' from a terminal." ;; esac
}

linux_uninstall() {
    for scope in 0 1; do
        linux_paths "$scope"
        for f in "$bin_path" "$desktop_file" "$icon_file"; do
            if [ -e "$f" ]; then as_root_for "$f" rm -f "$f"; note "removed $f"; fi
        done
        linux_refresh
    done
}

# ------------------------------------------------------------------- macOS

MAC_UTIS="public.jpeg public.png com.compuserve.gif org.webmproject.webp public.tiff com.microsoft.bmp com.microsoft.ico com.truevision.tga-image public.radiance com.ilm.openexr-image com.microsoft.dds public.pbm public.heic public.heif public.avif public.jpeg-xl public.jxl com.adobe.photoshop-image com.adobe.raw-image com.canon.cr2-raw-image com.canon.cr3-raw-image com.nikon.raw-image com.nikon.nrw-raw-image com.sony.arw-raw-image com.sony.sr2-raw-image com.sony.raw-image com.fuji.raw-image com.olympus.or-raw-image com.olympus.raw-image com.panasonic.rw2-raw-image com.panasonic.raw-image com.leica.rwl-raw-image com.pentax.raw-image com.samsung.raw-image com.sigma.raw-image com.hasselblad.3fr-raw-image com.phaseone.raw-image"
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

mac_install() {
    tar -xzf "$archive" -C "$TMP"
    [ -d "$TMP/$APP.app" ] || die "Archive doesn't contain $APP.app."
    if [ -w /Applications ]; then apps="/Applications"; else apps="$HOME/Applications"; mkdir -p "$apps"; fi
    bundle="$apps/$APP.app"
    rm -rf "$bundle"
    ditto "$TMP/$APP.app" "$bundle"
    xattr -dr com.apple.quarantine "$bundle" 2>/dev/null || true
    "$LSREGISTER" -f "$bundle" 2>/dev/null || true
    mkdir -p "$HOME/.local/bin"
    ln -sf "$bundle/Contents/MacOS/$BIN" "$HOME/.local/bin/$BIN"
    say "Installed $BOLD$bundle$RESET"

    if [ "$set_default" = 1 ]; then
        # shellcheck disable=SC2086
        if osascript -l JavaScript - "$BUNDLE_ID" $MAC_UTIS >/dev/null 2>&1 <<'EOF'
ObjC.import('CoreServices');
function run(argv) {
    for (const uti of argv.slice(1)) {
        $.LSSetDefaultRoleHandlerForContentType($(uti), 0xFFFFFFFF, $(argv[0]));
    }
}
EOF
        then say "$APP is now your default photo viewer"
        else warn "Couldn't set the default automatically. Finder > Get Info on a photo > Open with > $APP > Change All."
        fi
    fi
    case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) note "Add ~/.local/bin to PATH to run '$BIN' from a terminal." ;; esac
}

mac_uninstall() {
    for bundle in "/Applications/$APP.app" "$HOME/Applications/$APP.app"; do
        if [ -e "$bundle" ]; then
            "$LSREGISTER" -u "$bundle" 2>/dev/null || true
            as_root_for "$bundle" rm -rf "$bundle"
            note "removed $bundle"
        fi
    done
    rm -f "$HOME/.local/bin/$BIN"
}

# --------------------------------------------------------------------------

if [ "$uninstall" = 1 ]; then
    if [ "$PLATFORM" = "macos" ]; then mac_uninstall; else linux_uninstall; fi
    say "$APP removed."
    exit 0
fi

if [ -n "$archive" ]; then [ -f "$archive" ] || die "No such file: $archive"
elif [ "$source_build" = 1 ]; then build_from_source
else fetch_release
fi

if [ "$PLATFORM" = "macos" ]; then mac_install; else linux_install; fi
printf '\n  Open any photo, or run %s%s <photo>%s\n\n' "$BOLD" "$BIN" "$RESET"
