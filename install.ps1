<#
.SYNOPSIS
    Vitreus installer for Windows.

.DESCRIPTION
    irm https://nominy.github.io/vitreus/install.ps1 | iex

    Downloads the latest release, checks its SHA-256, installs it for your user
    (no admin), registers it for every supported image type ("Open with",
    Default Apps, Start menu, PATH, Apps & features) and opens Default Apps so
    you can make it the default with one click. Windows doesn't let programs
    take over file types silently, so that last click is yours.

    When piped into iex, configure with environment variables:
      $env:VITREUS_VERSION = 'v0.1.0'   # default: latest
      $env:VITREUS_SCOPE = 'Machine'    # all users; run from an elevated PowerShell
      $env:VITREUS_NO_DEFAULT = '1'     # don't open Default Apps

.EXAMPLE
    irm https://nominy.github.io/vitreus/install.ps1 | iex
.EXAMPLE
    & ([scriptblock]::Create((irm https://nominy.github.io/vitreus/install.ps1))) -Uninstall
.EXAMPLE
    .\install.ps1 -FromSource
#>
param(
    [string] $Version = $(if ($env:VITREUS_VERSION) { $env:VITREUS_VERSION } else { 'latest' }),
    [ValidateSet('User', 'Machine')]
    [string] $Scope = $(if ($env:VITREUS_SCOPE) { $env:VITREUS_SCOPE } else { 'User' }),
    [switch] $NoDefault = [bool]$env:VITREUS_NO_DEFAULT,
    # Build this checkout (or a fresh clone) with cargo instead of downloading.
    [switch] $FromSource,
    # Install a release zip you already have.
    [string] $Archive,
    [switch] $Uninstall
)

function Install-Vitreus {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    Set-StrictMode -Version Latest
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $AppName = 'Vitreus'
    $ExeName = 'vitreus.exe'
    $Repo = if ($env:VITREUS_REPO) { $env:VITREUS_REPO } else { 'Nominy/vitreus' }
    $Asset = 'vitreus-windows-x64.zip'

    $machine = $Scope -eq 'Machine'
    $hive = if ($machine) { [Microsoft.Win32.Registry]::LocalMachine } else { [Microsoft.Win32.Registry]::CurrentUser }
    $installDir = if ($machine) { Join-Path $env:ProgramFiles $AppName } else { Join-Path $env:LOCALAPPDATA "Programs\$AppName" }
    $startMenu = if ($machine) { Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs' } else { Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs' }
    $shortcut = Join-Path $startMenu "$AppName.lnk"
    $exe = Join-Path $installDir $ExeName
    $pathScope = if ($machine) { 'Machine' } else { 'User' }
    $base = if ($env:VITREUS_DOWNLOAD_URL) { $env:VITREUS_DOWNLOAD_URL.TrimEnd('/') }
            elseif ($Version -eq 'latest') { "https://github.com/$Repo/releases/latest/download" }
            else { "https://github.com/$Repo/releases/download/$Version" }

    # ASCII only: Windows PowerShell 5.1 decodes piped and BOM-less scripts as ANSI.
    function Say([string] $text) { Write-Host '> ' -ForegroundColor Cyan -NoNewline; Write-Host $text }
    function Note([string] $text) { Write-Host "  $text" -ForegroundColor DarkGray }

    if ($machine) {
        $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
        if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
            throw "Scope Machine installs for all users: run this from an elevated PowerShell (Run as administrator)."
        }
    }

    function Set-Value([string] $path, [string] $name, $value, $kind = [Microsoft.Win32.RegistryValueKind]::String) {
        $key = $hive.CreateSubKey($path)
        try { $key.SetValue($name, $value, $kind) } finally { $key.Dispose() }
    }
    # File types, Default Apps, App Paths: the binary owns that list
    # (`vitreus --register`), so Scoop/winget/zip installs register the same way.
    function Invoke-Registration([string] $flag) {
        $arguments = @($flag)
        if ($machine) { $arguments += '--machine' }
        # Native stderr must not trip ErrorActionPreference=Stop under PowerShell 5.1.
        $ErrorActionPreference = 'Continue'
        $output = & $exe @arguments 2>&1 | Out-String
        if ($LASTEXITCODE -ne 0) { throw "vitreus $flag failed: $($output.Trim())" }
    }
    function Stop-Running {
        Get-Process -Name 'vitreus' -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -and ($_.Path -ieq $exe) } |
            Stop-Process -Force
    }
    function Set-PathEntry([bool] $present) {
        $current = [Environment]::GetEnvironmentVariable('Path', $pathScope)
        $parts = @(if ($current) { $current.Split(';') | Where-Object { $_ -and ($_.TrimEnd('\') -ine $installDir) } })
        if ($present) { $parts += $installDir }
        [Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), $pathScope)
    }

    $uninstallKey = "Software\Microsoft\Windows\CurrentVersion\Uninstall\$AppName"

    function Register-Uninstaller([string] $version) {
        $icon = "`"$exe`",0"
        $script = Join-Path $installDir 'install.ps1'
        Set-Value $uninstallKey 'DisplayName' $AppName
        Set-Value $uninstallKey 'DisplayVersion' $version
        Set-Value $uninstallKey 'DisplayIcon' $icon
        Set-Value $uninstallKey 'Publisher' 'Nominy'
        Set-Value $uninstallKey 'URLInfoAbout' "https://github.com/$Repo"
        Set-Value $uninstallKey 'InstallLocation' $installDir
        Set-Value $uninstallKey 'UninstallString' "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$script`" -Uninstall -Scope $Scope"
        Set-Value $uninstallKey 'NoModify' 1 ([Microsoft.Win32.RegistryValueKind]::DWord)
        Set-Value $uninstallKey 'NoRepair' 1 ([Microsoft.Win32.RegistryValueKind]::DWord)
    }

    # ----------------------------------------------------------- uninstall

    if ($Uninstall) {
        Stop-Running
        if (Test-Path $exe) { Invoke-Registration '--unregister' }
        $hive.DeleteSubKeyTree($uninstallKey, $false)
        Set-PathEntry $false
        if (Test-Path $shortcut) { Remove-Item -Force $shortcut }
        if (Test-Path $installDir) { Remove-Item -Recurse -Force $installDir }
        Say "$AppName removed."
        return
    }

    # ------------------------------------------------------------- payload

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("vitreus-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $here = if ($ScriptPath) { Split-Path -Parent $ScriptPath } else { $null }
        $installedVersion = $Version.TrimStart('v')
        if ($FromSource) {
            if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
                throw 'Building needs Rust:  winget install Rustlang.Rustup  (then reopen the terminal)'
            }
            $src = if ($here -and (Test-Path (Join-Path $here 'Cargo.toml'))) { $here } else {
                if (-not (Get-Command git -ErrorAction SilentlyContinue)) { throw 'Building from source needs git.' }
                $ref = if ($Version -eq 'latest') { 'main' } else { $Version }
                Say "Cloning $Repo ($ref)"
                & git clone --quiet --depth 1 --branch $ref "https://github.com/$Repo.git" (Join-Path $tmp 'src')
                if ($LASTEXITCODE -ne 0) { throw 'git clone failed.' }
                Join-Path $tmp 'src'
            }
            Say 'Building from source (first build takes a few minutes)'
            Push-Location $src
            try {
                & cargo build --release --locked | Out-Host
                if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
            } finally { Pop-Location }
            $binary = Join-Path $src "target\release\$ExeName"
            $installedVersion = ((Get-Content (Join-Path $src 'Cargo.toml') | Select-String '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value)
        } else {
            if (-not $Archive) {
                $arch = $env:PROCESSOR_ARCHITECTURE
                if ($arch -eq 'ARM64') { Note 'ARM64 Windows: installing the x64 build (runs under emulation).' }
                Say "Downloading $AppName ($Version)"
                $Archive = Join-Path $tmp $Asset
                try { Invoke-WebRequest -UseBasicParsing -Uri "$base/$Asset" -OutFile $Archive }
                catch { throw "Couldn't download $base/$Asset ($($_.Exception.Message)). For an unreleased platform use -FromSource." }
                $sums = Join-Path $tmp 'SHA256SUMS'
                Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $sums
                $line = Get-Content $sums | Where-Object { $_ -match "\s\*?$([regex]::Escape($Asset))$" } | Select-Object -First 1
                if (-not $line) { throw "$Asset is not listed in SHA256SUMS." }
                $expected = ($line -split '\s+')[0].ToLowerInvariant()
                $actual = (Get-FileHash -Algorithm SHA256 $Archive).Hash.ToLowerInvariant()
                if ($expected -ne $actual) { throw "Checksum mismatch for $Asset (expected $expected, got $actual)." }
                Note 'checksum ok'
            }
            $unpacked = Join-Path $tmp 'unpacked'
            Expand-Archive -Path $Archive -DestinationPath $unpacked -Force
            $binary = Join-Path $unpacked $ExeName
            if (-not (Test-Path $binary)) { throw "The archive doesn't contain $ExeName." }
        }

        # ------------------------------------------------------- install

        Stop-Running
        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        Copy-Item -Force $binary $exe
        # Keep a copy of this script for "Apps & features" -> Uninstall.
        $selfCopy = Join-Path $installDir 'install.ps1'
        if ($ScriptPath -and ($ScriptPath -ine $selfCopy)) {
            Copy-Item -Force $ScriptPath $selfCopy
        } elseif (-not $ScriptPath) {
            try { Invoke-WebRequest -UseBasicParsing -Uri "$base/install.ps1" -OutFile $selfCopy }
            catch { Note "Couldn't fetch the uninstaller; uninstall later with the one-liner and -Uninstall." }
        }
        if ($installedVersion -eq 'latest') {
            $reported = (& $exe --version | Out-String).Trim()
            $installedVersion = if ($reported -match '(\d+\.\d+\.\d+\S*)') { $Matches[1] } else { 'latest' }
        }
        Invoke-Registration '--register'
        Register-Uninstaller $installedVersion
        Set-PathEntry $true

        New-Item -ItemType Directory -Force -Path $startMenu | Out-Null
        $shell = New-Object -ComObject WScript.Shell
        $link = $shell.CreateShortcut($shortcut)
        $link.TargetPath = $exe
        $link.WorkingDirectory = $installDir
        $link.IconLocation = "$exe,0"
        $link.Description = 'Photo viewer'
        $link.Save()

        Say "Installed $exe"
        Note 'Listed under "Open with" for every supported image type.'
        if (-not $NoDefault) {
            Say 'Opening Default Apps: pick Vitreus and click "Set default".'
            $registered = if ($machine) { 'registeredAppMachine' } else { 'registeredAppUser' }
            try { Start-Process "ms-settings:defaultapps?$registered=$AppName" } catch { Start-Process 'ms-settings:defaultapps' }
        }
        Write-Host ''
        Write-Host '  Open any photo, or run ' -NoNewline; Write-Host 'vitreus <photo>' -ForegroundColor White -NoNewline; Write-Host ' in a new terminal.'
        Write-Host ''
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

# Empty when piped into iex; the script's own path when run as a file.
$ScriptPath = $MyInvocation.MyCommand.Path
Install-Vitreus
