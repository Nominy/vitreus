#!/bin/sh
# Renders the Scoop manifest, Homebrew cask and winget manifests for a release.
#
#   scripts/render-packages.sh <version> <SHA256SUMS> <out-dir>
#
# out-dir/scoop/vitreus.json
# out-dir/homebrew/Casks/vitreus.rb
# out-dir/winget/manifests/n/Nominy/Vitreus/<version>/*.yaml
set -eu

[ $# -eq 3 ] || { echo "usage: $0 <version> <SHA256SUMS> <out-dir>" >&2; exit 2; }
version="${1#v}"; sums="$2"; out="$3"
root="$(cd "$(dirname "$0")/.." && pwd)"
repo="Nominy/vitreus"
base="https://github.com/$repo/releases/download/v$version"

hash_of() {
    h="$(grep " $1\$" "$sums" | cut -d' ' -f1)"
    [ -n "$h" ] || { echo "$1 missing from $sums" >&2; exit 1; }
    echo "$h"
}
win_hash="$(hash_of vitreus-windows-x64.zip)"
mac_arm_hash="$(hash_of vitreus-macos-arm64.tar.gz)"
mac_x64_hash="$(hash_of vitreus-macos-x64.tar.gz)"
# The same extension list the app lists and registers.
extensions="$(sed -n '/pub const EXTENSIONS/,/];/p' "$root/src/catalog.rs" | grep -o '"[a-z0-9]*"' | tr -d '"')"

mkdir -p "$out/scoop" "$out/homebrew/Casks"
winget="$out/winget/manifests/n/Nominy/Vitreus/$version"
mkdir -p "$winget"

cat >"$out/scoop/vitreus.json" <<EOF
{
    "version": "$version",
    "description": "A fast, no-nonsense photo viewer",
    "homepage": "https://nominy.github.io/vitreus",
    "license": "MIT",
    "architecture": {
        "64bit": {
            "url": "$base/vitreus-windows-x64.zip",
            "hash": "$win_hash"
        }
    },
    "bin": "vitreus.exe",
    "shortcuts": [["vitreus.exe", "Vitreus"]],
    "post_install": [
        "\$flags = @('--register', '--path', (Join-Path (Split-Path \$dir) 'current\\\\vitreus.exe'))",
        "if (\$global) { \$flags += '--machine' }",
        "& \"\$dir\\\\vitreus.exe\" @flags | Out-Null"
    ],
    "pre_uninstall": [
        "\$flags = @('--unregister')",
        "if (\$global) { \$flags += '--machine' }",
        "& \"\$dir\\\\vitreus.exe\" @flags | Out-Null"
    ],
    "checkver": "github",
    "autoupdate": {
        "architecture": {
            "64bit": {
                "url": "https://github.com/$repo/releases/download/v\$version/vitreus-windows-x64.zip"
            }
        },
        "hash": {
            "url": "\$baseurl/SHA256SUMS"
        }
    }
}
EOF

cat >"$out/homebrew/Casks/vitreus.rb" <<EOF
cask "vitreus" do
  arch arm: "arm64", intel: "x64"

  version "$version"
  sha256 arm:   "$mac_arm_hash",
         intel: "$mac_x64_hash"

  url "https://github.com/$repo/releases/download/v#{version}/vitreus-macos-#{arch}.tar.gz"
  name "Vitreus"
  desc "Fast, no-nonsense photo viewer"
  homepage "https://nominy.github.io/vitreus"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on macos: ">= :catalina"

  app "Vitreus.app"
  binary "#{appdir}/Vitreus.app/Contents/MacOS/vitreus"

  # Ad-hoc signed, not notarized: drop the download quarantine so it opens.
  postflight do
    system_command "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "#{appdir}/Vitreus.app"]
  end

  zap trash: "~/Library/Application Support/Vitreus"
end
EOF

cat >"$winget/Nominy.Vitreus.yaml" <<EOF
PackageIdentifier: Nominy.Vitreus
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.6.0
EOF

{
    cat <<EOF
PackageIdentifier: Nominy.Vitreus
PackageVersion: $version
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
  - RelativeFilePath: vitreus.exe
    PortableCommandAlias: vitreus
MinimumOSVersion: 10.0.17763.0
FileExtensions:
EOF
    for ext in $extensions; do echo "  - $ext"; done
    cat <<EOF
Installers:
  - Architecture: x64
    InstallerUrl: $base/vitreus-windows-x64.zip
    InstallerSha256: $(echo "$win_hash" | tr '[:lower:]' '[:upper:]')
ManifestType: installer
ManifestVersion: 1.6.0
EOF
} >"$winget/Nominy.Vitreus.installer.yaml"

cat >"$winget/Nominy.Vitreus.locale.en-US.yaml" <<EOF
PackageIdentifier: Nominy.Vitreus
PackageVersion: $version
PackageLocale: en-US
Publisher: Nominy
PublisherUrl: https://github.com/Nominy
PackageName: Vitreus
PackageUrl: https://nominy.github.io/vitreus
License: MIT
LicenseUrl: https://github.com/$repo/blob/main/LICENSE
ShortDescription: A fast, no-nonsense photo viewer
Description: Opens photos over your dimmed desktop with the folder in a film strip underneath. Run 'vitreus --register' once to list it under "Open with" and Default Apps.
Tags:
  - photo
  - image
  - viewer
  - gallery
ReleaseNotesUrl: https://github.com/$repo/releases/tag/v$version
ManifestType: defaultLocale
ManifestVersion: 1.6.0
EOF

echo "$out"
