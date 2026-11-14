<#
.SYNOPSIS
  预置 Tauri NSIS 打包工具链(20261102_01 P5-1)。

.DESCRIPTION
  `tauri build` 的 NSIS bundler 会自己下载 NSIS 3.13 与 nsis_tauri_utils.dll,
  但下载地址在 github.com —— 本机 github.com 不可达(api.github.com 可达),
  直接跑 `tauri build` 会卡在下载上。

  本脚本做一次性的工具链预置:
    1. 下载 nsis-3.13.zip → 校验 SHA1 → 解压到 %LOCALAPPDATA%\tauri\nsis-3.13
    2. 改名成 %LOCALAPPDATA%\tauri\NSIS(Tauri 期望的目录名)
    3. 下载 nsis_tauri_utils.dll → 校验 SHA1 → 放到 NSIS\Plugins\x86-unicode\additional\

  装好之后 tauri-bundler 会走「缓存命中」分支(校验 NSIS.exe 的 SHA1 与
  插件 SHA1),完全不再联网。

  下载源按顺序尝试: WROUTER_TAURI_MIRROR 指定的镜像前缀 → 默认镜像 → 直连。
  默认镜像用 ghproxy.net(国内可达),直连留给境外网络。

  幂等: 工具链已就位(SHA1 对得上)时直接退出,不重复下载。

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File packaging/fetch-tauri-nsis.ps1
#>

$ErrorActionPreference = "Stop"

# ---- 常量(取自 tauri-bundler 2.10.4 src/bundle/windows/nsis/mod.rs) ----
$NsisZipUrl = "https://github.com/tauri-apps/binary-releases/releases/download/nsis-3.13/nsis-3.13.zip"
$NsisZipSha1 = "DB14F8AF2EA346B786C6AE19343F61AAF5E09B8E"
$NsisExeSha1 = "6D45D4E8CCF18FE1FDAC9164058F356102A2ECF2"
$TauriUtilsUrl = "https://github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v0.5.3/nsis_tauri_utils.dll"
$TauriUtilsSha1 = "75197FEE3C6A814FE035788D1C34EAD39349B860"

# ---- 镜像前缀(按顺序尝试) ----
$Mirrors = @()
if (-not [string]::IsNullOrWhiteSpace($env:WROUTER_TAURI_MIRROR)) {
  $Mirrors += $env:WROUTER_TAURI_MIRROR.TrimEnd("/")
}
$Mirrors += "https://ghproxy.net"
# 直连放最后(境外网络 / ghproxy 挂了时的兜底)
$Direct = @("")

function Get-Sha1([string]$Path) {
  return (Get-FileHash -Path $Path -Algorithm SHA1).Hash.ToUpperInvariant()
}

function Get-WithMirror([string]$Url, [string]$Destination) {
  foreach ($mirror in $Mirrors) {
    $candidate = if ($mirror -eq "") { $Url } else { "$mirror/$Url" }
    Write-Host "[fetch-tauri-nsis] trying $candidate"
    try {
      Invoke-WebRequest -Uri $candidate -OutFile $Destination -UseBasicParsing -TimeoutSec 300
      return $true
    } catch {
      Write-Host "[fetch-tauri-nsis]   failed: $($_.Exception.Message)"
    }
  }
  return $false
}

# ---- 工具链目录(对齐 tauri-bundler 的 dirs::cache_dir()/tauri) ----
$cacheRoot = Join-Path $env:LOCALAPPDATA "tauri"
$nsisDir = Join-Path $cacheRoot "NSIS"
$pluginPath = Join-Path $nsisDir "Plugins\x86-unicode\additional\nsis_tauri_utils.dll"
$nsisExe = Join-Path $nsisDir "NSIS.exe"

Write-Host "[fetch-tauri-nsis] cache root: $cacheRoot"

# ---- 幂等检查 ----
if ((Test-Path $nsisExe) -and (Test-Path $pluginPath)) {
  if ((Get-Sha1 $nsisExe) -eq $NsisExeSha1 -and (Get-Sha1 $pluginPath) -eq $TauriUtilsSha1) {
    Write-Host "[fetch-tauri-nsis] toolchain already present and verified - nothing to do."
    exit 0
  }
  Write-Host "[fetch-tauri-nsis] existing toolchain failed SHA1 check - rebuilding."
}

$workDir = Join-Path $env:TEMP "wrouter-tauri-nsis"
if (Test-Path $workDir) { Remove-Item -Recurse -Force $workDir }
New-Item -ItemType Directory -Force -Path $workDir | Out-Null

# ---- 1. NSIS 本体 ----
$zipPath = Join-Path $workDir "nsis-3.13.zip"
if (-not (Get-WithMirror $NsisZipUrl $zipPath)) {
  throw "failed to download NSIS zip from all mirrors"
}
$zipSha1 = Get-Sha1 $zipPath
if ($zipSha1 -ne $NsisZipSha1) {
  throw "NSIS zip SHA1 mismatch: got $zipSha1 expected $NsisZipSha1"
}
Write-Host "[fetch-tauri-nsis] nsis-3.13.zip verified ($([math]::Round((Get-Item $zipPath).Length / 1MB, 1)) MB)"

Write-Host "[fetch-tauri-nsis] extracting..."
Expand-Archive -Path $zipPath -DestinationPath $workDir -Force
$extracted = Join-Path $workDir "nsis-3.13"
if (-not (Test-Path $extracted)) {
  throw "unexpected zip layout: $extracted not found"
}

if (Test-Path $nsisDir) { Remove-Item -Recurse -Force $nsisDir }
New-Item -ItemType Directory -Force -Path $cacheRoot | Out-Null
Move-Item -Path $extracted -Destination $nsisDir
Write-Host "[fetch-tauri-nsis] NSIS installed to $nsisDir"

# ---- 2. nsis_tauri_utils.dll 插件 ----
$dllPath = Join-Path $workDir "nsis_tauri_utils.dll"
if (-not (Get-WithMirror $TauriUtilsUrl $dllPath)) {
  throw "failed to download nsis_tauri_utils.dll from all mirrors"
}
$dllSha1 = Get-Sha1 $dllPath
if ($dllSha1 -ne $TauriUtilsSha1) {
  throw "nsis_tauri_utils.dll SHA1 mismatch: got $dllSha1 expected $TauriUtilsSha1"
}
New-Item -ItemType Directory -Force -Path (Split-Path $pluginPath -Parent) | Out-Null
Copy-Item -Force $dllPath $pluginPath
Write-Host "[fetch-tauri-nsis] plugin installed to $pluginPath"

# ---- 3. 复核(必须与 tauri-bundler 的校验口径一致) ----
if ((Get-Sha1 $nsisExe) -ne $NsisExeSha1) {
  throw "installed NSIS.exe SHA1 mismatch - tauri bundler would reject it"
}
if ((Get-Sha1 $pluginPath) -ne $TauriUtilsSha1) {
  throw "installed plugin SHA1 mismatch - tauri bundler would reject it"
}

Remove-Item -Recurse -Force $workDir
Write-Host "[fetch-tauri-nsis] toolchain ready. 'tauri build' will now bundle offline."
exit 0
