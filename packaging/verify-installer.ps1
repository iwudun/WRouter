<#
.SYNOPSIS
  WRouter Windows 安装包实测(20261102_01 P5-1 / P5-2 / P5-4)。

.DESCRIPTION
  对 Tauri NSIS 安装器做端到端静默验证:

    1. 静默安装到指定目录(NSIS `/S` + `/D=`)
    2. 核查安装产物:主程序 / 前端资源 / 卸载器
    3. 核查快捷方式:开始菜单 + 桌面
    4. 核查注册表:HKCU 卸载项
    5. 启动已安装的程序 → 等管理面就绪 → 取 session 令牌 →
       调 getAppInfo / getConfig,断言版本号与既有配置可读
    6. 采集进程树 RSS(Tauri 壳 + WebView2 子进程)用于体积/内存对比
    7. 关闭程序 → 静默卸载 → 核查程序文件 / 快捷方式 / 注册表已清干净,
       且用户数据目录(默认 %APPDATA%\WRouter\data)保留

  所有校验点逐条输出 PASS/FAIL,最后汇总。失败即退出码 1。

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File packaging/verify-installer.ps1 `
    -InstallerPath build/dist/<stamp>/win/WRouter_0.2.3_x64-setup.exe

.NOTES
  版本号断言从**安装包文件名**推导(WRouter_<ver>_x64-setup.exe),不写死 ——
  写死会让每次升版都必须改脚本,漏改就变成一条假 FAIL。
#>

param(
  [Parameter(Mandatory = $true)][string]$InstallerPath,
  [string]$InstallDir = "",
  [switch]$SkipUninstall
)

$ErrorActionPreference = "Stop"
$script:Failures = 0
$script:Passes = 0

function Pass([string]$name, [string]$detail = "") {
  $script:Passes += 1
  Write-Host ("  PASS  " + $name + $(if ($detail) { "  ($detail)" } else { "" }))
}

function Fail([string]$name, [string]$detail = "") {
  $script:Failures += 1
  Write-Host ("  FAIL  " + $name + $(if ($detail) { "  ($detail)" } else { "" }))
}

function Check([bool]$ok, [string]$name, [string]$detail = "") {
  if ($ok) { Pass $name $detail } else { Fail $name $detail }
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Installer = (Resolve-Path $InstallerPath).Path
if (-not $InstallDir) {
  $InstallDir = Join-Path $env:TEMP ("wrouter-install-test-" + (Get-Date -Format "yyyyMMddHHmmss"))
}

Write-Host "=============================================="
Write-Host " WRouter 安装包实测"
Write-Host " installer : $Installer"
Write-Host " target dir: $InstallDir"
Write-Host "=============================================="

# ---------------------------------------------------------------------------
# 前置:端口占用检查
# ---------------------------------------------------------------------------
$portBusy = Get-NetTCPConnection -LocalPort 3462 -State Listen -ErrorAction SilentlyContinue
if ($portBusy) {
  Write-Host "  WARN  3462 已被占用:$($portBusy.OwningProcess) —— 先停掉再跑本脚本。"
  exit 1
}

# ---------------------------------------------------------------------------
# 1. 静默安装
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "=== 1. 静默安装 ==="
if (Test-Path $InstallDir) { Remove-Item -Recurse -Force $InstallDir }
# NSIS: /S 静默;/D=<dir> 指定安装目录,必须是最后一个参数且不能带引号。
$proc = Start-Process -FilePath $Installer -ArgumentList "/S", "/D=$InstallDir" -PassThru -Wait
Check ($proc.ExitCode -eq 0) "安装器退出码 0" ("exit=$($proc.ExitCode)")
if ($proc.ExitCode -ne 0) { exit 1 }

$exe = Join-Path $InstallDir "wrouter-desktop.exe"
$uninstaller = Join-Path $InstallDir "uninstall.exe"
Check (Test-Path $exe) "主程序已安装" $exe
Check (Test-Path $uninstaller) "卸载器已安装" $uninstaller
# bundleVCRuntime=true → VC 运行库随包(干净机器上没有 VC redist 也能启动)
Check (Test-Path (Join-Path $InstallDir "msvcp140.dll")) "VC 运行库已随包(bundleVCRuntime)"
# 前端 SPA:由 bundle.resources 带出。**Windows 上 Tauri v2 的 resource_dir
# 就是安装目录本身**(不是 <安装目录>\resources)→ SPA 落在 <安装目录>\frontend\。
# 后端通过 WROUTER_FRONTEND_DIR 指向它(D5:窗口加载 http://127.0.0.1:3462/)。
$resourcesDir = $InstallDir
$hasIndex = Test-Path (Join-Path $resourcesDir "frontend\index.html")
Check $hasIndex "前端 SPA 资源已随包" (Join-Path $resourcesDir "frontend")
Check (Test-Path (Join-Path $resourcesDir "wrouter-capabilities.json")) "能力契约已随包"

# ---------------------------------------------------------------------------
# 2. 快捷方式
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "=== 2. 快捷方式 ==="
$startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\WRouter"
$startLnk = Join-Path $startMenu "WRouter.lnk"
Check (Test-Path $startLnk) "开始菜单快捷方式" $startLnk
$desktopDir = [Environment]::GetFolderPath("Desktop")
$desktopLnk = Join-Path $desktopDir "WRouter.lnk"
Check (Test-Path $desktopLnk) "桌面快捷方式" $desktopLnk

# ---------------------------------------------------------------------------
# 3. 注册表(卸载项)
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "=== 3. 注册表 ==="
$uninstallKey = Get-ChildItem "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall" -ErrorAction SilentlyContinue |
  Where-Object { $_.PSChildName -like "WRouter*" } |
  Select-Object -First 1
Check ($null -ne $uninstallKey) "HKCU 卸载项存在" $(if ($uninstallKey) { $uninstallKey.PSChildName } else { "" })

# ---------------------------------------------------------------------------
# 4. 启动 → 管理面可用 → 数据目录可读
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "=== 4. 启动与数据目录兼容 ==="
$app = Start-Process -FilePath $exe -PassThru
$ready = $false
$token = ""
for ($i = 0; $i -lt 60; $i++) {
  Start-Sleep -Milliseconds 500
  try {
    $session = Invoke-RestMethod -Uri "http://127.0.0.1:3462/api/wr/session" -TimeoutSec 3
    if ($session.token) { $token = $session.token; $ready = $true; break }
  } catch {
    # 还没起来
  }
}
Check $ready "管理面就绪(/api/wr/session 拿到令牌)" $(if ($token) { "token 长度 $($token.Length)" } else { "" })

if ($ready) {
  $headers = @{ "x-wrouter-web-auth" = $token }
  $info = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:3462/api/wr/rpc" `
    -Headers $headers -ContentType "application/json" `
    -Body '{"method":"getAppInfo","args":[]}' -TimeoutSec 10
  Check ($info.ok -eq $true) "getAppInfo RPC 可用"
  $version = $info.value.version
  # 期望版本从**安装包文件名**推导(WRouter_<ver>_x64-setup.exe),不再写死 ——
  # 写死会让每次升版都必须改脚本,漏改就变成一条假 FAIL(20261102_01 v0.2.3 实测)。
  $expectedVersion = $null
  if ($InstallerPath -match 'WRouter_([0-9]+\.[0-9]+\.[0-9]+[^_]*)_x64-setup\.exe$') {
    $expectedVersion = $Matches[1]
  }
  if ($expectedVersion) {
    Check ($version -eq $expectedVersion) "版本号为 $expectedVersion" ("got=$version")
  } else {
    Check ($true) "版本号已读取(未从文件名解析出期望值,跳过比对)" ("got=$version")
  }

  $cfg = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:3462/api/wr/rpc" `
    -Headers $headers -ContentType "application/json" `
    -Body '{"method":"getConfig","args":[]}' -TimeoutSec 10
  Check ($cfg.ok -eq $true) "getConfig RPC 可用(既有配置可读)"
  Check ($null -ne $cfg.value) "配置内容非空" ("providers=$(@($cfg.value.providers).Count)")

  # 管理面静态托管(P1):SPA 首页 + 静态资源 + 深链回落
  $index = Invoke-WebRequest -Uri "http://127.0.0.1:3462/" -TimeoutSec 10 -UseBasicParsing
  Check ($index.StatusCode -eq 200 -and $index.Content -match '<div id="root">') `
    "SPA 首页可从管理面取到" ("status=$($index.StatusCode)")
  $caps = Invoke-WebRequest -Uri "http://127.0.0.1:3462/api/v1/capabilities" -TimeoutSec 10 -UseBasicParsing
  Check ($caps.StatusCode -eq 200 -and $caps.Content -match "wrouter.capabilities") `
    "能力契约可从管理面取到" ("status=$($caps.StatusCode)")
}

# 数据目录:默认 %APPDATA%\WRouter\data(P5-4)
$dataDir = Join-Path $env:APPDATA "WRouter\data"
Check (Test-Path $dataDir) "数据目录位于 %APPDATA%\WRouter\data" $dataDir
Check (Test-Path (Join-Path $dataDir "config.sqlite")) "config.sqlite 已就位"
Check (Test-Path (Join-Path $dataDir "auth_token")) "auth_token 已就位"
$tokenFile = Join-Path $dataDir "auth_token"
if (Test-Path $tokenFile) {
  $persisted = (Get-Content $tokenFile -Raw).Trim()
  Check ($persisted -eq $token) "auth_token 与运行中的令牌一致(未被重新生成)"
}

# ---------------------------------------------------------------------------
# 5. 进程树 RSS(与 Electron 对比的实测数据)
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "=== 5. 内存占用(RSS) ==="
Start-Sleep -Seconds 3
# 只统计**本应用自己的进程树**(wrouter-desktop.exe 及其后代)。
# 直接按进程名扫 msedgewebview2.exe 会把机器上其它应用(别的 Tauri/Electron
# 程序)的 WebView2 实例也算进来 —— 那是噪声,不是本应用的占用。
$all = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue
$roots = @($all | Where-Object { $_.Name -eq "wrouter-desktop.exe" })
$tree = New-Object System.Collections.Generic.HashSet[int]
foreach ($r in $roots) { [void]$tree.Add([int]$r.ProcessId) }
$changed = $true
while ($changed) {
  $changed = $false
  foreach ($p in $all) {
    if ($tree.Contains([int]$p.ParentProcessId) -and -not $tree.Contains([int]$p.ProcessId)) {
      [void]$tree.Add([int]$p.ProcessId)
      $changed = $true
    }
  }
}
$treeProcs = @($all | Where-Object { $tree.Contains([int]$_.ProcessId) })
$totalRss = 0
foreach ($p in $treeProcs) { $totalRss += [double]$p.WorkingSetSize }
$shellRss = 0
foreach ($p in $treeProcs) { if ($p.Name -eq "wrouter-desktop.exe") { $shellRss += [double]$p.WorkingSetSize } }
$wvCount = @($treeProcs | Where-Object { $_.Name -eq "msedgewebview2.exe" }).Count
Write-Host ("  进程树: wrouter-desktop.exe x$($roots.Count), msedgewebview2.exe x$wvCount")
Write-Host ("  进程树 RSS 合计: {0:N1} MB" -f ($totalRss / 1MB))
Write-Host ("  桌面壳自身 RSS:  {0:N1} MB" -f ($shellRss / 1MB))

# ---------------------------------------------------------------------------
# 6. 关闭 + 卸载
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "=== 6. 关闭并卸载 ==="
Get-Process -Name "wrouter-desktop" -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 2

if (-not $SkipUninstall) {
  if (Test-Path $uninstaller) {
    $u = Start-Process -FilePath $uninstaller -ArgumentList "/S" -PassThru -Wait
    Check ($u.ExitCode -eq 0) "卸载器退出码 0" ("exit=$($u.ExitCode)")
    Start-Sleep -Seconds 2
  }
  Check (-not (Test-Path $exe)) "安装目录已清除"
  Check (-not (Test-Path $startLnk)) "开始菜单快捷方式已清除"
  Check (-not (Test-Path $desktopLnk)) "桌面快捷方式已清除"
  $left = Get-ChildItem "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall" -ErrorAction SilentlyContinue |
    Where-Object { $_.PSChildName -like "WRouter*" } | Select-Object -First 1
  Check ($null -eq $left) "注册表卸载项已清除"
  Check (Test-Path $dataDir) "用户数据目录保留(不随卸载删除)"
}

# ---------------------------------------------------------------------------
# 汇总
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "=============================================="
Write-Host (" 通过: $script:Passes   失败: $script:Failures")
Write-Host "=============================================="
if ($script:Failures -gt 0) { exit 1 }
exit 0
