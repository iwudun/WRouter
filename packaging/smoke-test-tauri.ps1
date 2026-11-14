# WRouter Tauri 桌面壳冒烟(方案 20261102_01 · P3 / P4)。
#
# 用法:
#   powershell -ExecutionPolicy Bypass -File packaging/smoke-test-tauri.ps1
#   powershell -ExecutionPolicy Bypass -File packaging/smoke-test-tauri.ps1 -ExePath <exe>
#
# 覆盖(P3 验收项):
#   1. 桌面壳进程起来,管理面端口开始监听
#   2. **同进程**:进程表里只有桌面壳一个 wrouter 进程,没有 wr-backend/wr.exe 子进程
#   3. `GET /api/wr/session` 返回令牌(loopback 放行)
#   4. `GET /` 返回 SPA 首页,静态资源可达
#   5. 桥接层自检:`window.wr.runtime === "desktop"`(壳内 eval 回传)
#   6. 退出后端口释放(没有残留进程占端口)
#
# 覆盖(P4 验收项):
#   7. 自定义标题栏:页面里有拖拽区(data-tauri-drag-region)与自绘窗口控制键
#   8. 单实例:二次启动**不产生第二个实例**(第二个进程自行退出)
#   9. 开机自启:注册表登录项随开关增删(备份原值 → 改 → 验 → 原样还原)

param(
  [string]$ExePath = "",
  [int]$WebPort = 3462,
  [int]$ReadyTimeoutSec = 90
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $ExePath) {
  # 20261102_01 P5-1:桌面壳默认 target 是 x86_64-pc-windows-msvc
  # (desktop/src-tauri/.cargo/config.toml)→ 产物在带 target 的子目录下。
  $ExePath = Join-Path $repoRoot "desktop\src-tauri\target\x86_64-pc-windows-msvc\debug\wrouter-desktop.exe"
}
if (-not (Test-Path $ExePath)) {
  Write-Error "找不到桌面壳可执行文件: $ExePath`n先构建: cd desktop/src-tauri; cargo build"
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

# 开机自启用的注册表位置(auto-launch 0.6 的固定路径)。
$runKey = "HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run"
$runValueName = "WRouter"

# 冒烟用独立数据目录,避免污染 %APPDATA%\WRouter\data 的真实数据。
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("wrouter-tauri-smoke-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Path $tempDir -Force | Out-Null
$stderrFile = Join-Path $tempDir "shell.stderr.log"
$stdoutFile = Join-Path $tempDir "shell.stdout.log"

Write-Host ("[smoke] 桌面壳: {0}" -f $ExePath)
Write-Host ("[smoke] 临时数据目录: {0}" -f $tempDir)

$env:WROUTER_DATA_DIR = $tempDir
$env:WROUTER_DESKTOP_SELFTEST = "1"
# 临时数据目录里 launchAtLogin 恒为 false → 照常对账会删掉用户机器上
# 真实的「登录时启动」条目。开机自启另有专项校验(带备份还原),这里跳过。
$env:WROUTER_DESKTOP_SKIP_AUTOSTART_SYNC = "1"
$process = Start-Process -FilePath $ExePath -PassThru -RedirectStandardError $stderrFile -RedirectStandardOutput $stdoutFile -WindowStyle Normal
Write-Host ("[smoke] pid={0}" -f $process.Id)

try {
  # ---- 1. 管理面端口就绪 ----
  $ready = $false
  $deadline = (Get-Date).AddSeconds($ReadyTimeoutSec)
  while ((Get-Date) -lt $deadline) {
    if ($process.HasExited) {
      Fail "进程存活" ("桌面壳提前退出, exit=" + $process.ExitCode)
      break
    }
    try {
      $tcp = New-Object System.Net.Sockets.TcpClient
      $tcp.Connect("127.0.0.1", $WebPort)
      $tcp.Close()
      $ready = $true
      break
    } catch {
      Start-Sleep -Milliseconds 500
    }
  }
  Check "管理面端口监听" $ready ("127.0.0.1:" + $WebPort)

  if ($ready) {
    # ---- 2. 同进程:没有后端子进程 ----
    $children = Get-CimInstance Win32_Process -Filter ("ParentProcessId=" + $process.Id) -ErrorAction SilentlyContinue
    $backendChildren = @($children | Where-Object { $_.Name -match '^(wr|wr-backend|wr-backend\.exe|wr\.exe)$' })
    Check "同进程(无 wr-backend 子进程)" ($backendChildren.Count -eq 0) ("子进程数=" + $backendChildren.Count + " 全部子进程=" + (@($children).Count))

    # ---- 3. session 令牌 ----
    $session = Invoke-WebRequest -Uri ("http://127.0.0.1:" + $WebPort + "/api/wr/session") -UseBasicParsing -TimeoutSec 10
    $sessionJson = $session.Content | ConvertFrom-Json
    Check "session 令牌" ((-not [string]::IsNullOrWhiteSpace($sessionJson.token))) ("token 长度=" + $sessionJson.token.Length)

    # ---- 4. SPA 首页 + 静态资源 ----
    $index = Invoke-WebRequest -Uri ("http://127.0.0.1:" + $WebPort + "/") -UseBasicParsing -TimeoutSec 10
    $hasRoot = $index.Content -match 'id="root"'
    Check "SPA 首页" (($index.StatusCode -eq 200) -and $hasRoot) ("status=" + $index.StatusCode + " 含 root 节点=" + $hasRoot)

    $assetMatch = [regex]::Match($index.Content, 'src="\.?/?(assets/[^"]+\.js)"')
    if ($assetMatch.Success) {
      $assetPath = $assetMatch.Groups[1].Value
      $asset = Invoke-WebRequest -Uri ("http://127.0.0.1:" + $WebPort + "/" + $assetPath) -UseBasicParsing -TimeoutSec 10
      Check "静态资源" ($asset.StatusCode -eq 200) ($assetPath + " status=" + $asset.StatusCode)
    } else {
      Fail "静态资源" $false "首页里没解析到 assets/*.js"
    }

    # ---- 5. 桥接层自检 ----
    $selftest = $null
    $selftestDeadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $selftestDeadline) {
      if (Test-Path $stderrFile) {
        $content = Get-Content $stderrFile -Raw -ErrorAction SilentlyContinue
        if ($content -and ($content -match '\[wrouter-desktop\]\[selftest\]')) {
          $selftest = ($content -split "`n" | Where-Object { $_ -match '\[selftest\]' } | Select-Object -Last 1)
          break
        }
      }
      Start-Sleep -Milliseconds 500
    }
    if ($selftest) {
      Check "桥接层自检 runtime=desktop" ($selftest -match '"runtime":"desktop"') $selftest.Trim()
    } else {
      Fail "桥接层自检 runtime=desktop" $false ("没读到自检输出;stderr=" + $(if (Test-Path $stderrFile) { (Get-Content $stderrFile -Raw) } else { "(无)" }))
    }

    # ---- 7. 自定义标题栏(P4-4)----
    # 拖拽区:页面里必须存在 data-tauri-drag-region 元素(Tauri 的拖拽脚本
    # 认这个属性,不认 -webkit-app-region)。
    $dragRegions = 0
    $windowControls = 0
    $controlsNoDrag = ""
    if ($selftest) {
      $json = $selftest -replace '^\[wrouter-desktop\]\[selftest\]\s*', ''
      try {
        $parsed = $json | ConvertFrom-Json
        $dragRegions = [int]$parsed.dragRegions
        $windowControls = [int]$parsed.windowControls
        $controlsNoDrag = [string]$parsed.windowControlsNoDrag
      } catch {
        $dragRegions = -1
      }
    }
    Check "标题栏拖拽区(data-tauri-drag-region)" ($dragRegions -ge 1) ("dragRegions=" + $dragRegions)
    Check "自绘窗口控制键(min/max/close)" ($windowControls -eq 3) ("按钮数=" + $windowControls + " no-drag 标记=" + $controlsNoDrag)

    # ---- 7b. 窗口层命令探针(P4-4 / P4-5)----
    # 真调一遍壳命令(capability 授权 + IPC 通道是否真的通)。
    $probe = $null
    $probeDeadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $probeDeadline) {
      if (Test-Path $stderrFile) {
        $probeContent = Get-Content $stderrFile -Raw -ErrorAction SilentlyContinue
        if ($probeContent -and ($probeContent -match '\[wrouter-desktop\]\[shellprobe\]')) {
          $probe = ($probeContent -split "`n" | Where-Object { $_ -match '\[shellprobe\]' } | Select-Object -Last 1)
          break
        }
      }
      Start-Sleep -Milliseconds 500
    }
    if ($probe) {
      $probeJson = ($probe -replace '^\[wrouter-desktop\]\[shellprobe\]\s*', '') | ConvertFrom-Json
      $probeOk = ($probeJson.isMaximized -is [bool]) -and ($probeJson.applyTheme -eq "ok") -and ($probeJson.toggleMaximizeRestored -eq "ok")
      Check "窗口层命令通道(capability 授权 + IPC)" $probeOk ("isMaximized=" + $probeJson.isMaximized + " applyTheme=" + $probeJson.applyTheme + " toggleMaximize=" + $probeJson.toggleMaximize + " restored=" + $probeJson.toggleMaximizeRestored)
    } else {
      Fail "窗口层命令通道(capability 授权 + IPC)" $false "没读到 shellprobe 输出"
    }

    # ---- 8. 单实例(P4-2)----
    # 二次启动:第二个进程应自行退出(插件把参数转给首个实例后终止它),
    # 且系统里始终只有一个 wrouter-desktop 进程。
    $before = @(Get-Process -Name "wrouter-desktop" -ErrorAction SilentlyContinue).Count
    $second = Start-Process -FilePath $ExePath -PassThru -RedirectStandardError (Join-Path $tempDir "second.stderr.log") -RedirectStandardOutput (Join-Path $tempDir "second.stdout.log") -WindowStyle Normal
    $exited = $second.WaitForExit(20000)
    $after = @(Get-Process -Name "wrouter-desktop" -ErrorAction SilentlyContinue).Count
    Check "单实例(二次启动不产生第二实例)" ($exited -and $after -le 1) ("第二实例 20s 内退出=" + $exited + " 进程数 前=" + $before + " 后=" + $after)
    Check "单实例后首个实例仍存活" ((-not $process.HasExited)) ("首个实例 pid=" + $process.Id)
  }
} finally {
  # ---- 6. 退出后端口释放 ----
  if (-not $process.HasExited) {
    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    $process.WaitForExit(10000) | Out-Null
  }
  Start-Sleep -Milliseconds 800
  $stillUp = $false
  try {
    $tcp = New-Object System.Net.Sockets.TcpClient
    $tcp.Connect("127.0.0.1", $WebPort)
    $tcp.Close()
    $stillUp = $true
  } catch {
    $stillUp = $false
  }
  Check "退出后端口释放" (-not $stillUp) ("127.0.0.1:" + $WebPort + " 仍被占用=" + $stillUp)
  Remove-Item Env:WROUTER_DATA_DIR -ErrorAction SilentlyContinue
  Remove-Item Env:WROUTER_DESKTOP_SELFTEST -ErrorAction SilentlyContinue
  Remove-Item Env:WROUTER_DESKTOP_SKIP_AUTOSTART_SYNC -ErrorAction SilentlyContinue
}

# ---------------------------------------------------------------------------
# 9. 开机自启(P4-3)—— 独立进程 + 备份还原
# ---------------------------------------------------------------------------
# 这一项不能在上面那个进程里做:它需要**改配置**再让壳读配置。
# 做法:起一个独立壳 → 通过管理面 RPC 改 launchAtLogin → 重启壳 →
#       查注册表 → 还原注册表原值 + 还原配置。
#
# 注册表副作用全程备份:finally 里把原值写回去(原来没有就删掉)。
$autostartBackup = $null
$autostartHadValue = $false
try {
  if (Test-Path $runKey) {
    $existing = Get-ItemProperty -Path $runKey -Name $runValueName -ErrorAction SilentlyContinue
    if ($existing) {
      $autostartBackup = [string]$existing.$runValueName
      $autostartHadValue = $true
    }
  }
  Write-Host ("[smoke] 开机自启: 原注册表值存在={0}" -f $autostartHadValue)

  # 起一个独立壳(自己的临时数据目录 + 自己的端口,避免与主流程抢端口)。
  $autoDir = Join-Path $tempDir "autostart"
  New-Item -ItemType Directory -Path $autoDir -Force | Out-Null
  $autoPort = 3472
  $env:WROUTER_DATA_DIR = $autoDir
  $env:WROUTER_WEB_PORT = [string]$autoPort
  Remove-Item Env:WROUTER_DESKTOP_SKIP_AUTOSTART_SYNC -ErrorAction SilentlyContinue
  $auto = Start-Process -FilePath $ExePath -PassThru -RedirectStandardError (Join-Path $autoDir "err.log") -RedirectStandardOutput (Join-Path $autoDir "out.log") -WindowStyle Normal
  try {
    $autoReady = $false
    $autoDeadline = (Get-Date).AddSeconds($ReadyTimeoutSec)
    while ((Get-Date) -lt $autoDeadline) {
      try {
        $tcp = New-Object System.Net.Sockets.TcpClient
        $tcp.Connect("127.0.0.1", $autoPort)
        $tcp.Close()
        $autoReady = $true
        break
      } catch {
        Start-Sleep -Milliseconds 500
      }
    }
    if (-not $autoReady) {
      Fail "开机自启(壳未就绪)" $false "独立壳 90s 内未监听 $autoPort"
    } else {
      # 取令牌,改配置(launchAtLogin = true)。
      $autoSession = (Invoke-WebRequest -Uri ("http://127.0.0.1:" + $autoPort + "/api/wr/session") -UseBasicParsing -TimeoutSec 10).Content | ConvertFrom-Json
      $config = (Invoke-WebRequest -Uri ("http://127.0.0.1:" + $autoPort + "/api/wr/rpc") -UseBasicParsing -TimeoutSec 10 -Method POST -ContentType "application/json" -Headers @{ "x-wrouter-web-auth" = $autoSession.token } -Body '{"method":"getConfig","args":[]}').Content | ConvertFrom-Json
      $cfg = $config.value
      $cfg.launchAtLogin = $true
      $saveBody = @{ method = "saveConfig"; args = @($cfg) } | ConvertTo-Json -Depth 12 -Compress
      $saved = (Invoke-WebRequest -Uri ("http://127.0.0.1:" + $autoPort + "/api/wr/rpc") -UseBasicParsing -TimeoutSec 15 -Method POST -ContentType "application/json" -Headers @{ "x-wrouter-web-auth" = $autoSession.token } -Body $saveBody).Content | ConvertFrom-Json
      Check "launchAtLogin 配置已落盘" ($saved.ok -and $saved.value.launchAtLogin -eq $true) ("saved.launchAtLogin=" + $saved.value.launchAtLogin)

      # 前端侧保存后立刻调壳命令同步(本次用 RPC 触发不了壳命令,
      # 所以这里直接等壳的下一次启动对账 —— 重启壳验证)。
      Stop-Process -Id $auto.Id -Force -ErrorAction SilentlyContinue
      $auto.WaitForExit(10000) | Out-Null
      Start-Sleep -Milliseconds 800

      $auto2 = Start-Process -FilePath $ExePath -PassThru -RedirectStandardError (Join-Path $autoDir "err2.log") -RedirectStandardOutput (Join-Path $autoDir "out2.log") -WindowStyle Normal
      try {
        $auto2Ready = $false
        $auto2Deadline = (Get-Date).AddSeconds($ReadyTimeoutSec)
        while ((Get-Date) -lt $auto2Deadline) {
          try {
            $tcp = New-Object System.Net.Sockets.TcpClient
            $tcp.Connect("127.0.0.1", $autoPort)
            $tcp.Close()
            $auto2Ready = $true
            break
          } catch {
            Start-Sleep -Milliseconds 500
          }
        }
        Start-Sleep -Seconds 2
        $entry = $null
        if (Test-Path $runKey) {
          $props = Get-ItemProperty -Path $runKey -Name $runValueName -ErrorAction SilentlyContinue
          if ($props) { $entry = [string]$props.$runValueName }
        }
        Check "开机自启写入注册表" ((-not [string]::IsNullOrWhiteSpace($entry)) -and ($entry -like "*wrouter-desktop.exe*")) ("注册表值=" + $entry)
      } finally {
        if (-not $auto2.HasExited) {
          Stop-Process -Id $auto2.Id -Force -ErrorAction SilentlyContinue
          $auto2.WaitForExit(10000) | Out-Null
        }
      }
    }
  } finally {
    if (-not $auto.HasExited) {
      Stop-Process -Id $auto.Id -Force -ErrorAction SilentlyContinue
      $auto.WaitForExit(10000) | Out-Null
    }
  }
} catch {
  Fail "开机自启(异常)" $false $_.Exception.Message
} finally {
  # 还原注册表原值(原来没有 → 删掉;原来有 → 写回原值)。
  try {
    if ($autostartHadValue) {
      Set-ItemProperty -Path $runKey -Name $runValueName -Value $autostartBackup -ErrorAction SilentlyContinue
      Write-Host ("[smoke] 开机自启: 已还原注册表原值")
    } elseif (Test-Path $runKey) {
      Remove-ItemProperty -Path $runKey -Name $runValueName -ErrorAction SilentlyContinue
      Write-Host ("[smoke] 开机自启: 已删除测试写入的注册表值")
    }
  } catch {
    Write-Host ("[smoke] 开机自启: 还原注册表失败 —— {0}" -f $_.Exception.Message)
  }
  Remove-Item Env:WROUTER_DATA_DIR -ErrorAction SilentlyContinue
  Remove-Item Env:WROUTER_WEB_PORT -ErrorAction SilentlyContinue
}

$passed = @($results | Where-Object { $_.Ok }).Count
$total = $results.Count
Write-Host ""
Write-Host ("[smoke] 结果: {0}/{1} PASS" -f $passed, $total)
if ($passed -ne $total) {
  Write-Host "[smoke] 失败明细:"
  $results | Where-Object { -not $_.Ok } | ForEach-Object { Write-Host ("  - {0}: {1}" -f $_.Name, $_.Detail) }
  exit 1
}
exit 0
