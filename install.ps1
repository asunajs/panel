#Requires -Version 5.1
<#
.SYNOPSIS
  panel（0.4，LLRT 版）Windows 一键安装

.DESCRIPTION
  做四件事：检测架构 → 下载产物 → **强制校验 SHA256**（外加 PE 自检）→ 装进目录。

       irm https://raw.githubusercontent.com/asunajs/panel/main/install.ps1 | iex

  或者下载后带参数跑：

      .\install.ps1 -Version v0.4.2 -Prefix D:\apps\panel -AddToPath

  设计前提和 install.sh 一致：**校验和与二进制是同一个来源**，所以「https 之外」的传输
  都当成可能被中间人一起替换，必须显式确认，不默认静默降级。

.PARAMETER Version
  版本 tag，默认 latest。也可用环境变量 PANEL_VERSION。

.PARAMETER Prefix
  安装目录，默认 %LOCALAPPDATA%\Programs\panel。也可用 PANEL_PREFIX。

.PARAMETER BaseUrl
  直接指定产物下载目录（镜像 / 内网 / 本地 file://），给了就不再拼 GitHub 地址。
  也可用 PANEL_BASE_URL。

.PARAMETER AddToPath
  把安装目录加进**用户** PATH（默认只提示，不擅自改环境）。

.PARAMETER AllowHttp
  PANEL_BASE_URL 用明文 http:// 时必须显式给（校验和与产物同源，明文挡不住中间人）。

.PARAMETER AllowFile
  PANEL_BASE_URL 用 file://（离线分发）时必须显式给。

.PARAMETER AllowUnverified
  取不到 SHA256SUMS 时也继续装。**不推荐**：等于放弃完整性校验。

.EXAMPLE
  irm https://raw.githubusercontent.com/asunajs/panel/main/install.ps1 | iex

.EXAMPLE
  $env:PANEL_VERSION='v0.4.2'; $env:PANEL_BASE_URL='https://mirror.example.com/panel'; .\install.ps1
#>
[CmdletBinding()]
param(
  [string]$Version = $(if ($env:PANEL_VERSION) { $env:PANEL_VERSION } else { 'latest' }),
  [string]$Prefix  = $env:PANEL_PREFIX,
  [string]$BaseUrl = $env:PANEL_BASE_URL,
  [switch]$AddToPath,
  [switch]$AllowHttp,
  [switch]$AllowFile,
  [switch]$AllowUnverified
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ---- fork 之后改这一行 ----
$Repo = if ($env:PANEL_REPO) { $env:PANEL_REPO } else { 'asunajs/panel' }
$BinName = 'panel'

function Die([string]$msg) {
  Write-Host "错误 " -ForegroundColor Red -NoNewline
  Write-Host $msg
  exit 1
}
function Info([string]$msg) { Write-Host "· $msg" -ForegroundColor Cyan }
function Ok([string]$msg)   { Write-Host "✓ $msg" -ForegroundColor Green }
function Warn([string]$msg) { Write-Host "警告 $msg" -ForegroundColor Yellow }

# ---------- 1. 检测架构 ----------
# 现在只发 x86_64：ARM64 的 Windows 能跑 x64 的 exe（系统自带模拟），
# 所以 ARM64 机器走 x64 产物是**可用**的，但要说明白，别让人以为装错了。
$procArch = $env:PROCESSOR_ARCHITECTURE
switch ($procArch) {
  'AMD64' { $arch = 'x86_64'; $note = '' }
  'ARM64' { $arch = 'x86_64'; $note = '（本机是 ARM64，用 x64 产物；Windows 自带 x64 模拟）' }
  default { Die "不支持的架构: $procArch（目前只发 x86_64-pc-windows-msvc）" }
}

# ---------- 2. 决定下载地址 + scheme 白名单 ----------
if ($BaseUrl) {
  $Base = $BaseUrl.TrimEnd('/')
} elseif ($Version -eq 'latest') {
  $Base = "https://github.com/$Repo/releases/latest/download"
} else {
  $Base = "https://github.com/$Repo/releases/download/$Version"
}

$HttpsOnly = $true
if ($Base -like 'https://*') {
  # ok
} elseif ($Base -like 'http://*') {
  if (-not $AllowHttp) {
    Die @"
PANEL_BASE_URL 用的是明文 http://，拒绝安装。
  原因：SHA256SUMS 和二进制从同一个地址取，走明文时中间人可以把两者一起换掉，
        校验和只能防传输出错，防不了人。请改 https://，或换能上 TLS 的镜像。
  确实只能走明文内网，显式加 -AllowHttp（或 PANEL_ALLOW_HTTP=1）再跑。
"@
  }
  $HttpsOnly = $false
  Warn 'http:// 下载没有传输层保护：校验和与二进制同源，中间人可以两个一起换。'
} elseif ($Base -like 'file://*') {
  if (-not $AllowFile) {
    Die 'PANEL_BASE_URL 用的是 file://（离线分发），需要显式确认（-AllowFile / PANEL_ALLOW_FILE=1）。'
  }
  $HttpsOnly = $false
} else {
  Die "不支持的 PANEL_BASE_URL：$Base（只允许 https://；http:// 要 -AllowHttp，file:// 要 -AllowFile）"
}

$Asset = "$BinName-$arch-pc-windows-msvc.exe"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("panel-install-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null

function Fetch([string]$url, [string]$out) {
  if ($url -like 'file://*') {
    Copy-Item -LiteralPath $url.Substring(7) -Destination $out -Force
    return
  }
  # Windows PowerShell 5.1 默认可能还是 TLS1.0，连 github 会失败 —— 显式抬到 1.2。
  try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch { }
  # 注意别把这包成 $args —— 那是 PowerShell 的自动变量，赋值会抛错。
  $req = @{ Uri = $url; OutFile = $out; UseBasicParsing = $true; TimeoutSec = 120 }
  Invoke-WebRequest @req | Out-Null
}

try {
  Info "平台: windows/$arch$note"
  Info "下载 $Asset …"
  try {
    Fetch "$Base/$Asset" (Join-Path $tmp $Asset)
  } catch {
    Die @"
下载失败。检查仓库名/网络，或该版本是否已发布 Windows 产物。
  预期地址: $Base/$Asset

  0.4.x 的 Windows 产物是**新加**的：早于它的版本只有 Linux 产物（install.sh 那份）。
  也可以从源码自己编：cargo build --release（见 README 的「从源码构建」）。
"@
  }

  # ---------- 3. 校验 SHA256（强制） ----------
  $sumsPath = Join-Path $tmp 'SHA256SUMS'
  $haveSums = $true
  try { Fetch "$Base/SHA256SUMS" $sumsPath } catch { $haveSums = $false }

  $actual = (Get-FileHash -Path (Join-Path $tmp $Asset) -Algorithm SHA256).Hash.ToLower()
  if ($haveSums) {
    $line = Select-String -Path $sumsPath -Pattern ([regex]::Escape($Asset) + '$') | Select-Object -First 1
    if (-not $line) {
      Warn 'SHA256SUMS 里没有本产物的条目，清单里现有的：'
      Get-Content $sumsPath | ForEach-Object { Write-Host "  $_" }
      Die "拒绝安装：拿不到 $Asset 的期望摘要（多半是这个版本没出 Windows 产物）。"
    }
    $expect = ($line.Line -split '\s+')[0].ToLower()
    if ($expect -ne $actual) {
      Die "SHA256 校验失败！期望 $expect，实际 $actual"
    }
    Ok "SHA256 校验通过（$Asset）"
  } else {
    if (-not $AllowUnverified) {
      Die @"
取不到 $Base/SHA256SUMS，拒绝安装。
  没有校验和就确认不了装的是不是被替换过的二进制。
  离线/自建镜像确实没有清单的，显式加 -AllowUnverified 再跑（不推荐）。
"@
    }
    Warn '取不到 SHA256SUMS，已按 -AllowUnverified 跳过校验 —— 装进去的东西没有任何完整性保证。'
  }

  # ---------- 4. 产物自检（PE + 体积） ----------
  # 校验和过了也可能是错的产物（镜像把 404 页当 200 返回之类），所以再看一眼文件本身。
  $bin = Join-Path $tmp $Asset
  $len = (Get-Item $bin).Length
  if ($len -lt 5MB) { Die "产物只有 $([math]::Round($len/1MB,2)) MB，不像一个完整的面板（预期 10MB+）" }
  $fs = [System.IO.File]::OpenRead($bin)
  try {
    $sig = New-Object byte[] 2
    $fs.Read($sig, 0, 2) | Out-Null
  } finally { $fs.Dispose() }
  if ($sig[0] -ne 0x4D -or $sig[1] -ne 0x5A) { Die '产物不是 PE 文件（缺 MZ 头）' }
  Ok "产物自检通过（$([math]::Round($len/1MB,2)) MB，PE32+）"

  # ---------- 5. 落盘 ----------
  if (-not $Prefix) { $Prefix = Join-Path $env:LOCALAPPDATA 'Programs\panel' }
  New-Item -ItemType Directory -Path $Prefix -Force | Out-Null
  $dest = Join-Path $Prefix "$BinName.exe"
  if (Test-Path $dest) {
    try {
      Copy-Item -LiteralPath $dest -Destination "$dest.old" -Force
      Remove-Item "$dest.old" -Force
    } catch {
      Die "旧的 $dest 正在运行（被占用），先停掉面板再装。`n  停：Get-Process panel -ErrorAction SilentlyContinue | Stop-Process"
    }
  }
  Copy-Item -LiteralPath $bin -Destination $dest -Force
  Ok "已安装: $dest"

  # ---------- 6. 自检运行 ----------
  # 装的这份能不能跑起来，比「文件复制成功」有用得多。
  $v = & $dest version 2>&1
  if ($LASTEXITCODE -ne 0) { Die "装好的程序跑不起来：$v" }
  Ok "版本自检: $v"

  # ---------- 7. PATH 提示 ----------
  $userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
  $inPath = $userPath -and ($userPath -split ';' | Where-Object { $_.TrimEnd('\') -eq $Prefix.TrimEnd('\') })
  if (-not $inPath) {
    if ($AddToPath) {
      $base = if ($userPath) { $userPath.TrimEnd(';') } else { '' }
      [Environment]::SetEnvironmentVariable('PATH', ($base + ';' + $Prefix), 'User')
      Ok "已加入用户 PATH（新开的终端生效）"
    } else {
      Warn "安装目录不在 PATH 里。加进去（当前用户，不用管理员）："
      Write-Host "    setx PATH `"%PATH%;$Prefix`""
      Write-Host "  或者直接用全路径：`"$dest`""
    }
  }

  Write-Host ''
  Write-Host '接下来：' -ForegroundColor Cyan
  Write-Host "  1) 启动：`"$dest`""
  Write-Host '  2) 浏览器打开 http://127.0.0.1:8787 —— 首次打开会引导你设一个口令。'
  Write-Host '  3) 想让它登录时自动起来：'
  Write-Host "       & '$dest' service print     # 看要跑的那条 schtasks 命令"
  Write-Host "       & '$dest' service install   # 直接建计划任务"
  Write-Host ''
  Write-Host '  常用环境变量：' -ForegroundColor Cyan
  Write-Host '    PANEL_TOKEN=你的令牌     跳过口令引导（服务化 / 无头部署用）'
  Write-Host '    PANEL_BIND=0.0.0.0:8787  让别的设备访问（默认只听 127.0.0.1:8787）'
  Write-Host '    PANEL_DATA_DIR=D:\panel\data  数据目录（默认 ./data，相对当前目录）'
  Write-Host ''
  Write-Host '  ⚠ 不要把没有口令的面板暴露到公网；公网访问走 HTTPS 反向代理。' -ForegroundColor Yellow
  Write-Host '  ⚠ Windows 产物是新加的，运行时细节见 docs/windows.md（已知差异与验收表）。' -ForegroundColor Yellow
}
finally {
  Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
