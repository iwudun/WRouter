# WRouter 服务控制冒烟(方案 20261102_01 · P6)。
#
# 用法:
#   powershell -NoProfile -ExecutionPolicy Bypass -File packaging/smoke-test-service-control.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File packaging/smoke-test-service-control.ps1 -ExePath <exe>
#
# 覆盖(P6 验收项,真机):
#   1. 桌面壳起来,管理面 3462 就绪,拿到 session 令牌
#   2. `getRuntimeControl` → 桌面壳内嵌语义(managedBy=embedded / canPause=true /
#      canExit=false / exitAction=exitApp)
#   3. `setGatewayEnabled(false)` → **网关面 3460 释放监听**(客户端 connection
#      refused),管理面 3462 **继续在线**(页面不废)
#   4. `getGatewayStatus` 报 stopped
#   5. `setGatewayEnabled(true)` → 3460 重新监听并回到 200
#   6. `stopService` 在内嵌模式下**必须被拒绝**(停后端只会让窗口变废)
#   7. 收尾:进程退出后两个端口都释放
#
# 同一套断言对**独立后端进程**同样成立(用 -ExePath 指向 wr-backend 时
# managedBy 应为 process,stopService 应被接受)—— 见脚本末尾的说明。

param(
  [string]$ExePath = "",
  [int]$WebPort = 3462,
  [int]$GatewayPort = 3460,
  [int]$ReadyTimeoutSec = 90
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $ExePath) {
  # 20261102_01 P5-1:桌面壳默认 target 是 x86_64-pc-windows-msvc
  $ExePath = Join-Path $repoRoot "desktop\src-tauri\target\x86_64-pc-windows-msvc\debug\wrouter-desktop.exe"
}
if (-not (Test-Path $ExePath)) {
  Write-Error "找不到桌面壳可执行文件: $ExePath`n先构建: cd desktop/src-tauri; cargo build --target x86_64-pc-windows-msvc"
}

$results = New-Object System.Collections.ArrayList
function Pass($name, $detail) {
  $null = $results.Add([pscustomobject]@{ Name = $name; Ok = $true; Detail = $detail })
  Write-Host ("  PASS  {0}  {1}" -f $name, $detail)
}
function Fail($name, $detail) {
  $null = $results.Add([pscustomobject]@{ Name = $name; Ok = $false; Detail = $detail })
  Write-Host ("  FAIL  {0}  {1}" -f $name, $detail)
}
function Check($name, $condition, $detail) {
  if ($condition) { Pass $name $detail } else { Fail $name $detail }
}

# TCP 连通性探针(返回 $true = 连得上)。
function Test-PortOpen([int]$port) {
  try {
    $tcp = New-Object System.Net.Sockets.TcpClient
    $tcp.Connect("127.0.0.1", $port)
    $tcp.Close()
    return $true
  } catch {
    return $false
  }
}

# 轮询直到条件成立。
function Wait-Until([scriptblock]$predicate, [int]$timeoutSec) {
  $deadline = (Get-Date).AddSeconds($timeoutSec)
  while ((Get-Date) -lt $deadline) {
    if (& $predicate) { return $true }
    Start-Sleep -Milliseconds 300
  }
  return $false
}

# 调管理 RPC(带令牌)。
#
# 注意(P6 实测踩坑):PowerShell 5.1 的 `ConvertFrom-Json "[false]"` 会把
# **单元素数组拆成标量** → args 变成 `false` 而不是 `[false]` → 后端
# 取不到布尔参数直接 500。所以这里不用 JSON 字符串中转,直接用 PowerShell
# 对象构造 args,并用 @() 强制成数组。
function Invoke-Rpc([string]$method, $rpcArgs = @()) {
  $payload = @{ method = $method; args = @($rpcArgs) } | ConvertTo-Json -Depth 6 -Compress
  try {
    $response = Invoke-WebRequest `
      -Uri ("http://127.0.0.1:" + $WebPort + "/api/wr/rpc") `
      -Method Post `
      -Headers @{ "x-wrouter-web-auth" = $script:token } `
      -ContentType "application/json" `
      -Body $payload `
      -UseBasicParsing -TimeoutSec 15
    return @{ Status = [int]$response.StatusCode; Json = ($response.Content | ConvertFrom-Json); Raw = $response.Content }
  } catch {
    $status = 0
    $raw = ""
    if ($_.Exception.Response) {
      try { $status = [int]$_.Exception.Response.StatusCode } catch { $status = 0 }
      try {
        $reader = New-Object System.IO.StreamReader($_.Exception.Response.GetResponseStream())
        $raw = $reader.ReadToEnd()
      } catch { $raw = "" }
    }
    return @{ Status = $status; Json = $null; Raw = $raw; Error = $_.Exception.Message }
  }
}

# 冒烟用独立数据目录,避免污染 %APPDATA%\WRouter\data 的真实数据。
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("wrouter-p6-smoke-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Path $tempDir -Force | Out-Null
$stderrFile = Join-Path $tempDir "shell.stderr.log"
$stdoutFile = Join-Path $tempDir "shell.stdout.log"

Write-Host ("[p6-smoke] 可执行文件: {0}" -f $ExePath)
Write-Host ("[p6-smoke] 临时数据目录: {0}" -f $tempDir)

$env:WROUTER_DATA_DIR = $tempDir
$env:WROUTER_DESKTOP_SKIP_AUTOSTART_SYNC = "1"
Remove-Item env:WROUTER_MANAGED_BY -ErrorAction SilentlyContinue
$process = Start-Process -FilePath $ExePath -PassThru -RedirectStandardError $stderrFile -RedirectStandardOutput $stdoutFile -WindowStyle Normal
Write-Host ("[p6-smoke] pid={0}" -f $process.Id)

try {
  # ---- 1. 管理面就绪 + 令牌 ----
  $ready = Wait-Until { Test-PortOpen $WebPort } $ReadyTimeoutSec
  Check "管理面端口监听" $ready ("127.0.0.1:" + $WebPort)
  if (-not $ready) { throw "管理面未就绪,后续断言无法执行" }

  $session = Invoke-WebRequest -Uri ("http://127.0.0.1:" + $WebPort + "/api/wr/session") -UseBasicParsing -TimeoutSec 10
  $script:token = ($session.Content | ConvertFrom-Json).token
  Check "session 令牌" ((-not [string]::IsNullOrWhiteSpace($script:token))) ("token 长度=" + $script:token.Length)

  # ---- 2. getRuntimeControl:桌面壳内嵌语义 ----
  $control = Invoke-Rpc "getRuntimeControl"
  $value = $control.Json.value
  Check "getRuntimeControl 可调用" ($control.Status -eq 200) ("status=" + $control.Status)
  Check "managedBy=embedded" ($value.managedBy -eq "embedded") ("managedBy=" + $value.managedBy)
  Check "canPause=true" ($value.canPause -eq $true) ("canPause=" + $value.canPause)
  Check "内嵌模式 canExit=false" ($value.canExit -eq $false) ("canExit=" + $value.canExit)
  Check "exitAction=exitApp" ($value.exitAction -eq "exitApp") ("exitAction=" + $value.exitAction)

  # ---- 3. 暂停网关面:3460 释放,3462 仍在 ----
  $pause = Invoke-Rpc "setGatewayEnabled" $false
  Check "setGatewayEnabled(false)" ($pause.Status -eq 200 -and $pause.Json.value.state -eq "stopped") ("state=" + $pause.Json.value.state)
  $closed = Wait-Until { -not (Test-PortOpen $GatewayPort) } 20
  Check "暂停后网关面 3460 已释放" $closed ("127.0.0.1:" + $GatewayPort + " 连不上")
  Check "暂停后管理面 3462 仍在线" (Test-PortOpen $WebPort) ("127.0.0.1:" + $WebPort)

  # ---- 4. getGatewayStatus 报 stopped ----
  $status = Invoke-Rpc "getGatewayStatus"
  Check "getGatewayStatus=stopped" ($status.Json.value.state -eq "stopped") ("state=" + $status.Json.value.state)
  Check "getGatewayStatus.gatewayEnabled=false" ($status.Json.value.gatewayEnabled -eq $false) ("gatewayEnabled=" + $status.Json.value.gatewayEnabled)

  # ---- 5. 恢复网关面 ----
  $resume = Invoke-Rpc "setGatewayEnabled" $true
  Check "setGatewayEnabled(true)" ($resume.Status -eq 200) ("status=" + $resume.Status)
  $reopened = Wait-Until { Test-PortOpen $GatewayPort } 30
  Check "恢复后网关面 3460 重新监听" $reopened ("127.0.0.1:" + $GatewayPort + " 连得上")

  # ---- 6. 内嵌模式下 stopService 必须被拒绝 ----
  $stop = Invoke-Rpc "stopService"
  Check "内嵌模式 stopService 被拒绝" ($stop.Status -ge 400) ("status=" + $stop.Status + " body=" + $stop.Raw)
  Start-Sleep -Seconds 2
  Check "拒绝后桌面壳仍存活" (-not $process.HasExited) ("hasExited=" + $process.HasExited)
  Check "拒绝后管理面仍在线" (Test-PortOpen $WebPort) ("127.0.0.1:" + $WebPort)
} catch {
  Fail "冒烟执行" $_.Exception.Message
} finally {
  # ---- 7. 收尾 ----
  if (-not $process.HasExited) {
    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
  }
  Start-Sleep -Seconds 2
  $webFreed = -not (Test-PortOpen $WebPort)
  $gwFreed = -not (Test-PortOpen $GatewayPort)
  Check "退出后管理面端口释放" $webFreed ("127.0.0.1:" + $WebPort)
  Check "退出后网关面端口释放" $gwFreed ("127.0.0.1:" + $GatewayPort)
}

$failed = @($results | Where-Object { -not $_.Ok })
Write-Host ""
Write-Host ("[p6-smoke] {0}/{1} PASS" -f ($results.Count - $failed.Count), $results.Count)
if ($failed.Count -gt 0) {
  Write-Host "[p6-smoke] 失败项:"
  foreach ($item in $failed) { Write-Host ("  - {0}: {1}" -f $item.Name, $item.Detail) }
  exit 1
}
exit 0
