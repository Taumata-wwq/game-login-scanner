#!/usr/bin/env pwsh
# 快速 Kotlin 门禁：只编译 Kotlin/资源，不跑 cargo、不出 APK。
#
# 为什么需要它
#   本项目 Android 侧的 Kotlin 错误（类型不匹配、SDK API 变更、清单字面量）只有编译
#   才暴露，而完整 build-android.ps1 要跑 cargo + NDK + R8，约 5~10 分钟。这个脚本
#   只做 patch-gen + :app:compileUniversalReleaseKotlin，实测约 1~3 分钟（增量更快），
#   够在改动 Kotlin 后立刻验证。
#
# 为什么没放进 CI
#   vendor/（UC/OPPO/vivo/腾讯/小米 SDK，约 29 MB）按设计不入库，Kotlin 侧 import
#   这些 SDK；没有它们的 runner 一定编译失败。所以在 CI 里加这条只会得到一条红门禁，
#   改成"本地必跑"。
#
# 用法：
#   pwsh -File scripts/android/check-kotlin.ps1
#   pwsh -File scripts/android/check-kotlin.ps1 -Offline     # 依赖已缓存（本机常态）
#   pwsh -File scripts/android/check-kotlin.ps1 -SkipPatch   # 明确跳过 patch-gen
param(
    [switch]$Offline,
    [switch]$SkipPatch
)

$ErrorActionPreference = "Stop"
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$gen = Join-Path $root "src-tauri/gen/android"

$sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { "G:\ProgramData\Android\Sdk" }
$env:ANDROID_HOME = $sdk
if (-not $env:JAVA_HOME) { $env:JAVA_HOME = "G:\Java21" }
$env:PATH = "$env:JAVA_HOME\bin;$sdk\platform-tools;$env:PATH"

if (-not (Test-Path (Join-Path $gen "gradlew.bat"))) {
    Write-Host "FAIL: 找不到 gen/android/gradlew.bat（先跑一次 scripts/android/build-android.ps1 生成 gen/）" -ForegroundColor Red
    exit 2
}

if (-not $SkipPatch) {
    Write-Host "== patch-gen（同步 Kotlin 插件与 gradle 注入）" -ForegroundColor Cyan
    & pwsh -NoProfile -File (Join-Path $PSScriptRoot "patch-gen.ps1")
    if ($LASTEXITCODE -ne 0) { Write-Host "FAIL: patch-gen 失败" -ForegroundColor Red; exit 1 }
}

Write-Host "== Kotlin 编译（:app:compileUniversalReleaseKotlin）" -ForegroundColor Cyan
$gradleArgs = @(":app:compileUniversalReleaseKotlin", "--console=plain")
if ($Offline) { $gradleArgs += "--offline" }
Push-Location $gen
try {
    & .\gradlew.bat @gradleArgs
    $code = $LASTEXITCODE
} finally {
    Pop-Location
}
if ($code -ne 0) { Write-Host "FAIL: Kotlin 编译失败（exit $code）" -ForegroundColor Red; exit 1 }
Write-Host "PASS  Kotlin 编译通过" -ForegroundColor Green
exit 0
