#Requires -Version 5.1
<#
.SYNOPSIS
    One-click ALVR build (Windows streamer + Android client).

.DESCRIPTION
    Configures the local toolchain, prepares deps if needed, then builds
    the Windows streamer and/or Android client into the build/ folder.

.PARAMETER Target
    What to build: All (default), Streamer, Client, or Launcher.

.PARAMETER SkipDeps
    Skip cargo xtask prepare-deps (use when deps/ already prepared).

.PARAMETER ForceDeps
    Always run prepare-deps even if deps look present.

.PARAMETER NoGpl
    Build streamer without FFmpeg/x264 GPL bundle (hardware encode only).

.PARAMETER DebugBuild
    Build debug profile instead of release.

.PARAMETER Ci
    Pass --ci to prepare-deps (skip Chocolatey admin elevation). Default: on.

.EXAMPLE
    .\build-all.ps1
    .\build-all.ps1 -Target Streamer
    .\build-all.ps1 -Target Client -SkipDeps
    .\build-all.ps1 -Target All -ForceDeps -NoGpl
#>
[CmdletBinding()]
param(
    [ValidateSet("All", "Streamer", "Client", "Launcher")]
    [string]$Target = "All",

    [switch]$SkipDeps,
    [switch]$ForceDeps,
    [switch]$NoGpl,
    [switch]$DebugBuild,
    [switch]$Ci
)

$ErrorActionPreference = "Stop"
$Root = $PSScriptRoot
Set-Location $Root

function Write-Step([string]$Message) {
    Write-Host ""
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Write-Ok([string]$Message) {
    Write-Host "    $Message" -ForegroundColor Green
}

function Write-Warn([string]$Message) {
    Write-Host "    $Message" -ForegroundColor Yellow
}

function Add-PathFront([string]$PathDir) {
    if ($PathDir -and (Test-Path $PathDir)) {
        if (-not (($env:Path -split ";") -contains $PathDir)) {
            $env:Path = "$PathDir;$env:Path"
        }
    }
}

function Find-FirstExisting([string[]]$Candidates) {
    foreach ($c in $Candidates) {
        if ($c -and (Test-Path $c)) { return $c }
    }
    return $null
}

function Import-VsDevEnvironment {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vsDevCmd = $null

    if (Test-Path $vswhere) {
        $installPath = & $vswhere -latest -products * `
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
            -property installationPath 2>$null
        if ($installPath) {
            $candidate = Join-Path $installPath "Common7\Tools\VsDevCmd.bat"
            if (Test-Path $candidate) { $vsDevCmd = $candidate }
        }
    }

    if (-not $vsDevCmd) {
        $vsDevCmd = Find-FirstExisting @(
            "D:\Program Files\VS2022\Common7\Tools\VsDevCmd.bat",
            "${env:ProgramFiles}\Microsoft Visual Studio\2022\Community\Common7\Tools\VsDevCmd.bat",
            "${env:ProgramFiles}\Microsoft Visual Studio\2022\Professional\Common7\Tools\VsDevCmd.bat",
            "${env:ProgramFiles}\Microsoft Visual Studio\2022\Enterprise\Common7\Tools\VsDevCmd.bat",
            "${env:ProgramFiles}\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat"
        )
    }

    if (-not $vsDevCmd) {
        throw "Visual Studio with C++ tools not found. Install VS 2022 with Desktop development with C++."
    }

    Write-Ok "VsDevCmd: $vsDevCmd"
    $raw = & cmd.exe /c "`"$vsDevCmd`" -arch=amd64 -host_arch=amd64 >nul 2>&1 && set"
    foreach ($line in $raw) {
        if ($line -match "^([^=]+)=(.*)$") {
            [System.Environment]::SetEnvironmentVariable($matches[1], $matches[2], "Process")
        }
    }

    if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
        throw "MSVC cl.exe not available after loading VsDevCmd."
    }
    Write-Ok "MSVC: $((Get-Command cl.exe).Source)"
}

function Initialize-CommonEnv {
    Write-Step "Setting up environment"

    # Cargo / Rust
    Add-PathFront "$env:USERPROFILE\.cargo\bin"
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "cargo not found. Install Rust from https://rustup.rs and reopen the shell."
    }
    Write-Ok "cargo: $((Get-Command cargo).Source) ($(cargo --version))"

    # Common tools
    Add-PathFront "C:\Program Files\Git\cmd"
    Add-PathFront "C:\Program Files\CMake\bin"
    Add-PathFront "C:\ProgramData\chocolatey\bin"

    # LLVM / libclang (bindgen)
    $llvmBin = Find-FirstExisting @(
        $env:LIBCLANG_PATH,
        "D:\Program Files\LLVM\bin",
        "C:\Program Files\LLVM\bin"
    )
    if ($llvmBin) {
        $env:LIBCLANG_PATH = $llvmBin
        Add-PathFront $llvmBin
        Write-Ok "LIBCLANG_PATH=$env:LIBCLANG_PATH"
    }
    else {
        Write-Warn "LLVM/libclang not found. Streamer build may fail at bindgen."
    }

    # pkg-config path for x264 etc.
    $env:PKG_CONFIG_PATH = Join-Path $Root "deps\windows"
    Write-Ok "PKG_CONFIG_PATH=$env:PKG_CONFIG_PATH"
}

function Initialize-AndroidEnv {
    Write-Step "Setting up Android environment"

    $javaHome = Find-FirstExisting @(
        $env:JAVA_HOME,
        "D:\Enviroments\jdk-11.0.2",
        "D:\Environments\jdk-11.0.2",
        "C:\Program Files\Android\Android Studio\jbr",
        "C:\Program Files\Android\Android Studio\jre",
        "C:\Program Files\Eclipse Adoptium\jdk-17*",
        "C:\Program Files\Java\jdk-17*",
        "C:\Program Files\Microsoft\jdk-17*"
    )
    # Expand wildcard candidates
    if (-not $javaHome) {
        $glob = @(
            "C:\Program Files\Eclipse Adoptium\jdk-*",
            "C:\Program Files\Java\jdk-*",
            "C:\Program Files\Microsoft\jdk-*"
        ) | ForEach-Object { Get-Item $_ -ErrorAction SilentlyContinue } | Sort-Object FullName -Descending
        if ($glob) { $javaHome = $glob[0].FullName }
    }

    if (-not $javaHome) {
        throw "JAVA_HOME not found. Install JDK 11+ and set JAVA_HOME."
    }
    $env:JAVA_HOME = $javaHome
    Add-PathFront (Join-Path $env:JAVA_HOME "bin")
    Write-Ok "JAVA_HOME=$env:JAVA_HOME"

    $androidHome = Find-FirstExisting @(
        $env:ANDROID_HOME,
        $env:ANDROID_SDK_ROOT,
        "D:\Software\AndroidSDK",
        "$env:LOCALAPPDATA\Android\Sdk"
    )
    if (-not $androidHome) {
        throw "Android SDK not found. Set ANDROID_HOME or install the SDK."
    }
    $env:ANDROID_HOME = $androidHome
    # Prefer ANDROID_HOME; clear deprecated SDK_ROOT if identical to avoid cargo-apk warning noise
    if ($env:ANDROID_SDK_ROOT -and ($env:ANDROID_SDK_ROOT -ne $env:ANDROID_HOME)) {
        Write-Warn "ANDROID_SDK_ROOT=$env:ANDROID_SDK_ROOT differs from ANDROID_HOME; using ANDROID_HOME"
    }
    Remove-Item Env:ANDROID_SDK_ROOT -ErrorAction SilentlyContinue
    Add-PathFront (Join-Path $env:ANDROID_HOME "platform-tools")
    Write-Ok "ANDROID_HOME=$env:ANDROID_HOME"

    $ndkHome = $null
    if ($env:ANDROID_NDK_HOME -and (Test-Path $env:ANDROID_NDK_HOME)) {
        $ndkHome = $env:ANDROID_NDK_HOME
    }
    else {
        $ndkRoot = Join-Path $env:ANDROID_HOME "ndk"
        if (Test-Path $ndkRoot) {
            $latest = Get-ChildItem $ndkRoot -Directory | Sort-Object Name -Descending | Select-Object -First 1
            if ($latest) { $ndkHome = $latest.FullName }
        }
    }
    if (-not $ndkHome) {
        throw "Android NDK not found under ANDROID_HOME\ndk. Install e.g. ndk;27.2.12479018"
    }
    $env:ANDROID_NDK_HOME = $ndkHome
    $env:ANDROID_NDK_ROOT = $ndkHome
    Write-Ok "ANDROID_NDK_HOME=$env:ANDROID_NDK_HOME"

    if (-not (Get-Command cargo-apk -ErrorAction SilentlyContinue) -and
        -not (Test-Path "$env:USERPROFILE\.cargo\bin\cargo-apk.exe")) {
        Write-Warn "cargo-apk not found; prepare-deps will install it."
    }
}

function Test-WindowsDepsReady {
    $win = Join-Path $Root "deps\windows"
    return (Test-Path (Join-Path $win "x264")) -and
        (Test-Path (Join-Path $win "ffmpeg")) -and
        (Test-Path (Join-Path $win "libvpl"))
}

function Test-AndroidDepsReady {
    $oxr = Join-Path $Root "deps\android_openxr\arm64-v8a\libopenxr_loader.so"
    $cargoApk = Test-Path "$env:USERPROFILE\.cargo\bin\cargo-apk.exe"
    $target = (& rustup target list --installed 2>$null) -contains "aarch64-linux-android"
    return (Test-Path $oxr) -and $cargoApk -and $target
}

function Invoke-Xtask {
    param([Parameter(Mandatory)][string[]]$Args)
    Write-Host "    cargo xtask $($Args -join ' ')" -ForegroundColor DarkGray
    & cargo xtask @Args
    if ($LASTEXITCODE -ne 0) {
        throw "cargo xtask failed with exit code $LASTEXITCODE"
    }
}

function Invoke-PrepareDeps([string]$Platform) {
    $prepareArgs = @("prepare-deps", "--platform", $Platform)
    if ($Ci) { $prepareArgs += "--ci" }
    Invoke-Xtask $prepareArgs
}

# ---- main ----
$sw = [System.Diagnostics.Stopwatch]::StartNew()
Write-Host "ALVR one-click build" -ForegroundColor Magenta
Write-Host "Root: $Root"
# --ci by default (skip choco UAC); pass -Ci:$false to disable
if (-not $PSBoundParameters.ContainsKey("Ci")) { $Ci = $true }

Write-Host "Target: $Target | SkipDeps=$SkipDeps ForceDeps=$ForceDeps NoGpl=$NoGpl DebugBuild=$DebugBuild Ci=$Ci"

Initialize-CommonEnv

$profileFlags = if ($DebugBuild) { @() } else { @("--release") }
$gplFlags = if ($NoGpl) { @() } else { @("--gpl") }

$needStreamer = $Target -in @("All", "Streamer")
$needClient = $Target -in @("All", "Client")
$needLauncher = $Target -eq "Launcher"

if ($needStreamer -or $needLauncher) {
    Write-Step "Loading MSVC environment"
    Import-VsDevEnvironment
}

if ($needStreamer) {
    $runDeps = $ForceDeps -or ((-not $SkipDeps) -and -not (Test-WindowsDepsReady))
    if ($SkipDeps -and -not (Test-WindowsDepsReady)) {
        Write-Warn "SkipDeps set but windows deps look incomplete; build may fail."
    }
    if ($runDeps) {
        Write-Step "Preparing Windows streamer dependencies"
        Invoke-PrepareDeps "windows"
    }
    else {
        Write-Step "Skipping Windows prepare-deps (deps present or SkipDeps)"
    }

    Write-Step "Building Windows streamer"
    Invoke-Xtask (@("build-streamer") + $profileFlags + $gplFlags)
    $dash = Join-Path $Root "build\alvr_streamer_windows\ALVR Dashboard.exe"
    if (Test-Path $dash) {
        Write-Ok "Streamer: $dash"
    }
}

if ($needClient) {
    Initialize-AndroidEnv
    $runDeps = $ForceDeps -or ((-not $SkipDeps) -and -not (Test-AndroidDepsReady))
    if ($SkipDeps -and -not (Test-AndroidDepsReady)) {
        Write-Warn "SkipDeps set but android deps look incomplete; build may fail."
    }
    if ($runDeps) {
        Write-Step "Preparing Android client dependencies"
        Invoke-PrepareDeps "android"
    }
    else {
        Write-Step "Skipping Android prepare-deps (deps present or SkipDeps)"
    }

    Write-Step "Building Android client"
    Invoke-Xtask (@("build-client") + $profileFlags)
    $apk = Join-Path $Root "build\alvr_client_android\alvr_client_android.apk"
    if (Test-Path $apk) {
        $sizeMb = [math]::Round((Get-Item $apk).Length / 1MB, 2)
        Write-Ok "Client APK: $apk ($sizeMb MB)"
    }
}

if ($needLauncher) {
    Write-Step "Building launcher"
    Invoke-Xtask (@("build-launcher") + $profileFlags)
}

$sw.Stop()
Write-Host ""
Write-Host "Done in $([math]::Round($sw.Elapsed.TotalMinutes, 1)) min." -ForegroundColor Green
Write-Host "Outputs under: $(Join-Path $Root 'build')"
