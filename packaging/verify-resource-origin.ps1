<#
.SYNOPSIS
  验证安装态的前端 SPA / 能力契约**确实来自安装包**(20261102_01 P5-1 / P5-4)。

.DESCRIPTION
  背景:后端 `Settings::from_env` 的 `frontend_dir` / `capabilities_file`
  默认值指向 `<repo_root>/...` —— 那是**编译期常量 CARGO_MANIFEST_DIR** 推出来的
  **源码树**路径。安装态的机器上没有源码树 → 管理面托管不到 SPA,
  窗口加载 http://127.0.0.1:3462/ 会 404;能力契约会退到内联 fallback。

  本脚本做一次**决定性验证**:
    1. 临时把仓库里的 `frontend/dist` 改名(模拟"机器上没有源码树");
    2. 启动**已安装**的程序,请求管理面首页与能力契约;
    3. 首页必须 200 且含 `<div id="root">`;
       能力契约必须 200 且含 `desktopMethodsByRuntime`(只有真实契约文件才有,
       内联 fallback 没有);
    4. 无论成败,finally 里把 `frontend/dist` 改回来。

  用法:
    powershell -NoProfile -ExecutionPolicy Bypass -File packaging/verify-resource-origin.ps1 `
      -InstallDir <已安装目录>
#>

param(
  [Parameter(Mandatory = $true)][string]$InstallDir,
  [int]$TimeoutSeconds = 60
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$sourceDist = Join-Path $repoRoot "frontend\dist"
$backupDist = Join-Path $repoRoot "frontend\dist.__probe_off"
$exe = Join-Path $InstallDir "wrouter-desktop.exe"

$script:Failures = 0
function Check([bool]$ok, [string]$name, [string]$detail = "") {
  if ($ok) { Write-Host ("  PASS  " + $name + $(if ($detail) { "  ($detail)" } else { "" })) }
  else { $script:Failures += 1; Write-Host ("  FAIL  " + $name + $(if ($detail) { "  ($detail)" } else { "" })) }
}

if (-not (Test-Path $exe)) { Write-Error "已安装程序不存在: $exe"; exit 1 }

# 端口占用检查:必须确保 3462 上跑的是本脚本启动的那个实例。
$busy = Get-NetTCPConnection -LocalPort 3462 -State Listen -ErrorAction SilentlyContinue
if ($busy) { Write-Error "3462 已被占用(PID $($busy.OwningProcess)) —— 先停掉再跑。"; exit 1 }

Write-Host "=== 关闭源码树前端产物(模拟干净机器) ==="
if (Test-Path $backupDist) { Remove-Item -Recurse -Force $backupDist }
Move-Item $sourceDist $backupDist
Write-Host "  已改名: frontend/dist -> frontend/dist.__probe_off"

$app = $null
try {
  $app = Start-Process -FilePath $exe -PassThru
  $ready = $false
  for ($i = 0; $i -lt $TimeoutSeconds * 2; $i++) {
    Start-Sleep -Milliseconds 500
    try {
      $s = Invoke-RestMethod -Uri "http://127.0.0.1:3462/api/wr/session" -TimeoutSec 3
      if ($s.token) { $ready = $true; break }
    } catch { }
  }
  Check $ready "管理面就绪"

  if ($ready) {
    $index = Invoke-WebRequest -Uri "http://127.0.0.1:3462/" -TimeoutSec 10 -UseBasicParsing
    Check ($index.StatusCode -eq 200 -and $index.Content -match '<div id="root">') `
      "SPA 首页来自安装包(源码树已移除仍可取到)" ("status=$($index.StatusCode)")

    $caps = Invoke-WebRequest -Uri "http://127.0.0.1:3462/api/v1/capabilities" -TimeoutSec 10 -UseBasicParsing
    Check ($caps.StatusCode -eq 200 -and $caps.Content -match "desktopMethodsByRuntime") `
      "能力契约来自安装包(非内联 fallback)" ("status=$($caps.StatusCode)")
  }
}
finally {
  if ($app) { Get-Process -Name "wrouter-desktop" -ErrorAction SilentlyContinue | Stop-Process -Force }
  Start-Sleep -Seconds 1
  if (Test-Path $backupDist) {
    if (Test-Path $sourceDist) { Remove-Item -Recurse -Force $sourceDist }
    Move-Item $backupDist $sourceDist
    Write-Host "=== 已还原 frontend/dist ==="
  }
}

Write-Host ""
if ($script:Failures -gt 0) { Write-Host "失败: $script:Failures"; exit 1 }
Write-Host "全部通过"
exit 0
