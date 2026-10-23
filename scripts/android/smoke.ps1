#!/usr/bin/env pwsh
# 真机冒烟：装包 -> 启动 -> 断言无崩溃 -> 截图 ->（可选）校验界面文案。
#
# 为什么要有这个脚本
#   本项目 Android 侧的问题（Kotlin 类型错误、清单字面量转义、R8 缺类、SDK 初始化
#   失败）都只在真机上才暴露，过去每一轮都是手工重复这一串 adb 命令。这里把它固化
#   成一条命令，并给出**可判定的退出码**：0 = 通过，1 = 断言失败，2 = 环境不可用。
#
# 用法：
#   pwsh -File scripts/android/smoke.ps1
#   pwsh -File scripts/android/smoke.ps1 -SkipInstall                  # 只跑已装的包
#   pwsh -File scripts/android/smoke.ps1 -Apk dist/xxx.apk -RequireText "添加账号","扫码"
#   pwsh -File scripts/android/smoke.ps1 -Serial 541ff5c6 -OutDir dist/smoke
#
# 环境变量：ADB（adb 可执行文件路径）、ANDROID_HOME。
param(
    [string]$Apk = "dist/game-login-scanner-android-universal.apk",
    [string]$Serial = "",
    [string]$OutDir = "smoke",
    [string[]]$RequireText = @("添加账号"),
    [int]$LaunchTimeoutSec = 25,
    [int]$ObserveSec = 6,
    [switch]$SkipInstall,
    [switch]$KeepScreenOn
)

$ErrorActionPreference = "Stop"
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$pkg = "com.taumata.game_scanner"
$activity = "$pkg/.MainActivity"
$NL = [Environment]::NewLine

function Fail([string]$msg, [int]$code = 1) {
    Write-Host "FAIL: $msg" -ForegroundColor Red
    exit $code
}
function Info([string]$msg) { Write-Host "  $msg" }
function Step([string]$msg) { Write-Host "$NL== $msg" -ForegroundColor Cyan }

# ---- adb 解析：显式环境变量 -> PATH -> 常见安装位置 ----
function Resolve-Adb {
    if ($env:ADB -and (Test-Path $env:ADB)) { return $env:ADB }
    $cmd = Get-Command adb -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA "Android\Sdk\platform-tools\adb.exe"),
        "D:\Desktop\软件\scrcpy-win64-v2.1.1\adb.exe",
        "D:\Desktop\软件\QtScrcpy-win-x64-v2.1.1\adb.exe"
    )
    if ($env:ANDROID_HOME) { $candidates = @((Join-Path $env:ANDROID_HOME "platform-tools\adb.exe")) + $candidates }
    foreach ($c in $candidates) { if ($c -and (Test-Path $c)) { return $c } }
    return $null
}

$adb = Resolve-Adb
if (-not $adb) { Fail "找不到 adb：请安装 platform-tools，或设置环境变量 ADB 指向 adb.exe" 2 }
Info "adb = $adb"

# 注意：不使用 param() 块——带 [Parameter()] 的函数会获得公共参数，
# 于是 adb 自己的 "-W" 会被 PowerShell 当成 -WarningAction 的缩写而报歧义错。
function Adb {
    $out = & $adb -s $script:serial @args 2>&1
    return ($out | Out-String).Trim()
}

# ---- 设备 ----
Step "设备"
$devices = & $adb devices 2>&1 | Out-String
$online = @($devices -split "\r?\n" | Where-Object { $_ -match "\tdevice$" })
if (-not $Serial) {
    if ($online.Count -eq 0) { Fail "没有已授权的设备（adb devices 里没有 device 状态）：接上手机并开启 USB 调试" 2 }
    if ($online.Count -gt 1) { Fail ("检测到多台设备，请用 -Serial 指定：{0}{1}" -f $NL, ($online -join $NL)) 2 }
    $script:serial = ($online[0] -split "\t")[0]
} else {
    if (-not ($online | Where-Object { $_ -like "$Serial*" })) { Fail "设备 $Serial 不在线" 2 }
    $script:serial = $Serial
}
$model = Adb shell getprop ro.product.model
$rel = Adb shell getprop ro.build.version.release
$sdkVer = Adb shell getprop ro.build.version.sdk
Info "设备 = $script:serial ($model, Android $rel / SDK $sdkVer)"

if ($KeepScreenOn) {
    [void](Adb shell svc power stayon true)
    [void](Adb shell settings put system screen_off_timeout 600000)
}

# ---- 安装 ----
if (-not $SkipInstall) {
    Step "安装 APK"
    $apkPath = if ([System.IO.Path]::IsPathRooted($Apk)) { $Apk } else { Join-Path $root $Apk }
    if (-not (Test-Path $apkPath)) { Fail "APK 不存在：$apkPath" }
    Info ("APK = {0} ({1} B)" -f $apkPath, (Get-Item $apkPath).Length)
    $install = & $adb -s $script:serial install -r $apkPath 2>&1 | Out-String
    if ($install -notmatch "Success") { Fail ("安装失败：{0}{1}" -f $NL, $install) }
    Info "install: Success"
}

Step "包信息"
$dump = Adb shell dumpsys package $pkg
if ($dump -notmatch "versionName=(\S+)") { Fail "设备上没有安装 $pkg" }
$verName = $Matches[1]
$verCode = if ($dump -match "versionCode=(\d+)") { $Matches[1] } else { "?" }
Info "versionName=$verName versionCode=$verCode"

# ---- 启动 ----
Step "冷启动"
[void](Adb shell logcat -c)
[void](Adb shell am force-stop $pkg)
$start = Adb shell am start -W -n $activity
Info (($start -split "\r?\n" | Where-Object { $_ -match "Status|TotalTime|Error" }) -join "; ")

$focused = ""
$deadline = (Get-Date).AddSeconds($LaunchTimeoutSec)
while ((Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 500
    $win = Adb shell dumpsys window
    if ($win -match "mCurrentFocus=Window\{\S+ \S+ ([^\}]+)\}") {
        $focused = $Matches[1].Trim()
        if ($focused -like "$pkg/*") { break }
    }
}
if ($focused -notlike "$pkg/*") { Fail "启动后前台不是本应用（当前：$focused）" }
Info "前台 = $focused"

Start-Sleep -Seconds $ObserveSec

# ---- 日志断言 ----
Step "崩溃断言（启动期日志）"
$pidText = (Adb shell pidof $pkg).Trim()
$log = if ($pidText) { Adb shell logcat -d "--pid=$pidText" } else { Adb shell logcat -d }
$patterns = @(
    @{ Name = "FATAL EXCEPTION"; Re = "FATAL EXCEPTION" },
    @{ Name = "AndroidRuntime RuntimeException"; Re = "AndroidRuntime.*RuntimeException" },
    @{ Name = "NoClassDefFoundError"; Re = "NoClassDefFoundError" },
    @{ Name = "ClassNotFoundException"; Re = "ClassNotFoundException" },
    @{ Name = "UnsatisfiedLinkError"; Re = "UnsatisfiedLinkError" }
)
$hits = @()
foreach ($p in $patterns) {
    $m = [regex]::Matches($log, $p.Re)
    if ($m.Count -gt 0) { $hits += ("{0} x {1}" -f $p.Name, $m.Count) }
}
if ($hits.Count -gt 0) {
    Info ("命中：{0}" -f ($hits -join ", "))
    $logDir = if ([System.IO.Path]::IsPathRooted($OutDir)) { $OutDir } else { Join-Path $root $OutDir }
    New-Item -ItemType Directory -Force -Path $logDir | Out-Null
    $logPath = Join-Path $logDir ("smoke-logcat-" + [DateTime]::Now.ToString("yyyyMMdd-HHmmss") + ".txt")
    Set-Content -Path $logPath -Value $log -Encoding UTF8
    Fail "日志断言失败（完整日志：$logPath）"
}
Info "无 FATAL / NoClassDefFound / UnsatisfiedLink"

# ---- 界面文案 ----
if ($RequireText.Count -gt 0) {
    Step "界面文案断言"
    [void](Adb shell uiautomator dump /sdcard/smoke-ui.xml)
    $ui = Adb exec-out cat /sdcard/smoke-ui.xml
    [void](Adb shell rm -f /sdcard/smoke-ui.xml)
    $missing = @()
    foreach ($t in $RequireText) { if ($ui -notmatch [regex]::Escape($t)) { $missing += $t } }
    if ($missing.Count -gt 0) { Fail ("界面上找不到：{0}" -f ($missing -join ", ")) }
    Info ("文案齐全：{0}" -f ($RequireText -join " / "))
}

# ---- 截图 ----
Step "截图"
$outDir = if ([System.IO.Path]::IsPathRooted($OutDir)) { $OutDir } else { Join-Path $root $OutDir }
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$shot = Join-Path $outDir ("smoke-" + [DateTime]::Now.ToString("yyyyMMdd-HHmmss") + ".png")
# 走「设备内截图 -> adb pull」而不是 exec-out：PowerShell 会把管道输出当文本解码，
# 直接把 PNG 字节写盘会损坏（实测报 "无法将值 \uFFFDPNG 转换为类型 System.Byte"）。
[void](Adb shell screencap -p /sdcard/smoke-shot.png)
$pullOut = & $adb -s $script:serial pull /sdcard/smoke-shot.png $shot 2>&1 | Out-String
[void](Adb shell rm -f /sdcard/smoke-shot.png)
if ($pullOut -notmatch "1 file pulled|pulled") { Fail ("截图拉取失败：{0}" -f $pullOut) }
$shotSize = (Get-Item $shot).Length
if ($shotSize -lt 10000) { Fail "截图异常（$shotSize B）：$shot" }
Info "截图 = $shot ($shotSize B)"

Write-Host ("$NL PASS  版本 {0}({1}) / 设备 {2} / 截图 {3}" -f $verName, $verCode, $model, $shot) -ForegroundColor Green
exit 0
