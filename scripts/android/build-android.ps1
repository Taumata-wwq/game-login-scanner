#!/usr/bin/env pwsh
# 规范的 Android 构建入口：**先跑 patch-gen（Kotlin 插件同步 + gradle 依赖注入），
# 再 cargo tauri android build**。
#
# 为什么要包一层（2026-10-18 实际踩到的坑）：
#   `src-tauri/gen/` 不入库，且 `tauri android init` 会覆盖它。项目对 gen 的本地化
#   修改（MainActivity 返回键接管、渠道 SDK、protobuf/gson 依赖、Kotlin 插件源码同步…）
#   全靠 `patch-gen.ps1` 重放。**如果改了 `src-tauri/android-plugin/*.kt` 却直接
#   `cargo tauri android build`，Rust 侧照常编译、APK 照常产出，但新 Kotlin 根本没进
#   gen ——构建成功而功能静默失效**，极难发现（当时改完 NotifyBridge.kt 就中了这一枪，
#   直到真机验证才发现 APK 里没有该类）。
#
# 因此：凡是要出 Android 产物，一律走本脚本；它会强制先同步 patch。
#
# 用法：
#   pwsh -File scripts/android/build-android.ps1                 # 默认：arm64 + armv7
#   pwsh -File scripts/android/build-android.ps1 -Abi arm64-v8a  # 仅 arm64（体积最小）
#   pwsh -File scripts/android/build-android.ps1 -Debug
#   pwsh -File scripts/android/build-android.ps1 -SkipPatch      # 明确跳过（仅排障用）
#   pwsh -File scripts/android/build-android.ps1 -CopyToDist     # 产物另存到 dist/
param(
    [string]$Abi = "arm64-v8a,armeabi-v7a",
    [string]$Arch = "arm64,arm",
    [string]$Target = "aarch64,armv7",
    [switch]$Debug,
    [switch]$SkipPatch,
    [switch]$CopyToDist
)

$ErrorActionPreference = "Stop"
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$gen = Join-Path $root "src-tauri/gen/android"

# ---- 环境（与 patch-gen 一致的路径约定；可用环境变量覆盖） ----
$sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { "G:\ProgramData\Android\Sdk" }
$ndk = if ($env:ANDROID_NDK_HOME) { $env:ANDROID_NDK_HOME } else { Join-Path $sdk "ndk/27.0.12077973" }
$env:ANDROID_HOME = $sdk
$env:ANDROID_NDK_HOME = $ndk
$env:NDK_HOME = $ndk
if (-not $env:JAVA_HOME) { $env:JAVA_HOME = "G:\Java21" }
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
$env:PATH = "$cargoBin;$env:JAVA_HOME\bin;$sdk\platform-tools;$env:PATH"

# ---- 构建产物目录钉在工作区外（2026-10-18 实测教训）----
# src-tauri/target 曾涨到 70 GB / 10 万文件。DSH 的 workspace preparation 按
# **文件数**建立沙箱与索引，10 万文件会把准备阶段拖到超时，报
#   "DSH workspace preparation failed (not a user permission denial):"（原因字段为空）
# 于是 run_code 及全部工具不可用——即"构建缓存拖垮了整个 IDE"。
# 把 target 钉到工作区外后，工作区可长期保持在数千文件量级。
# 未设置时才补默认值，尊重使用者已有配置。
if (-not $env:CARGO_TARGET_DIR) {
    $env:CARGO_TARGET_DIR = "G:\cargo-target"
}
Write-Host "  CARGO_TARGET_DIR = $env:CARGO_TARGET_DIR"

Write-Host "== Android 构建 ==" -ForegroundColor Cyan
Write-Host "  ANDROID_HOME = $sdk"
Write-Host "  NDK          = $ndk"
Write-Host "  JAVA_HOME    = $env:JAVA_HOME"
Write-Host "  ABI          = $Abi"
Write-Host "  构建类型     = $(if ($Debug) { 'debug' } else { 'release' })"

if (-not (Test-Path $gen)) {
    Write-Error "未找到 $gen ——请先运行：cargo tauri android init"
    exit 1
}

# ---- 1) 强制同步 patch（除非显式跳过） ----
if ($SkipPatch) {
    Write-Warning "已指定 -SkipPatch：跳过 patch-gen。若本次改过 android-plugin/*.kt 或依赖，产物可能不含你的改动！"
} else {
    Write-Host "`n-- 运行 patch-gen（Kotlin 同步 + gradle 依赖注入）--" -ForegroundColor Cyan
    # 注意：调用另一个 .ps1 后 `$LASTEXITCODE` 只在被调脚本显式 `exit N` 时才可靠。
    # patch-gen 正常路径以 Write-Host 结尾、不设退出码，故这里用 try/catch 判成败：
    #   - 子脚本里 Write-Error / throw 会冒泡到本作用域（$ErrorActionPreference='Stop'）；
    #   - 子脚本若 `exit 1`，本进程会带着非零码直接退出（也不会走到后面）。
    try {
        & (Join-Path $PSScriptRoot "patch-gen.ps1") -AbiList $Abi -ArchList $Arch -TargetList $Target
    } catch {
        Write-Error "patch-gen 执行失败：$_"
        exit 1
    }
    Write-Host "-- patch-gen 完成 --" -ForegroundColor Green
}

# ---- 2) 同步校验：确保每个源目录的 .kt 都已进 gen ----
# 这是对"构建成功但功能静默失效"的最后一道防线：patch-gen 若因某种原因漏拷/漏跑，
# 这里直接报错而不是产出一个缺功能的包。
# 覆盖两个源目录：android-plugin（总是）与 android-plugin-huawei（仅当华为渠道启用）。
$pluginSrc = Join-Path $root "src-tauri/android-plugin"
$huaweiPluginSrc = Join-Path $root "src-tauri/android-plugin-huawei"
$huaweiJson = Join-Path $root "vendor/huawei/agconnect-services.json"
$pluginDst = Join-Path $gen "app/src/main/java/com/taumata/scanner/plugins"
# 华为插件只在 vendor/huawei/agconnect-services.json 存在时才由 patch-gen 拷入 gen，
# 未启用时把它算进"应当存在"的清单会误报。
$srcDirs = @()
if (Test-Path $pluginSrc) { $srcDirs += $pluginSrc }
if ((Test-Path $huaweiJson) -and (Test-Path $huaweiPluginSrc)) { $srcDirs += $huaweiPluginSrc }
if ($srcDirs.Count -gt 0) {
    $missing = @()
    foreach ($srcDir in $srcDirs) {
        $srcTag = Split-Path $srcDir -Leaf
        Get-ChildItem $srcDir -Filter *.kt | ForEach-Object {
            $dst = Join-Path $pluginDst $_.Name
            if (-not (Test-Path $dst)) {
                $missing += "$($_.Name)（$srcTag 未同步）"
            } elseif ((Get-Item $dst).LastWriteTime -lt $_.LastWriteTime) {
                # 时间戳更旧的 gen 副本 ⇒ 源码更新而未同步
                $missing += "$($_.Name)（gen 副本较旧）"
            }
        }
    }
    if ($missing.Count -gt 0) {
        Write-Error "插件源码未同步到 gen，构建会产出缺功能的 APK：`n  " +
            ($missing -join "`n  ") + "`n请重跑 patch-gen（或去掉 -SkipPatch）。"
        exit 1
    }
    Write-Host "-- Kotlin 插件同步校验通过（gen 与源码一致）--" -ForegroundColor Green
}

# ---- 2b) 渠道 SDK 依赖完整性（把运行期 NoClassDefFoundError 提前到构建期） ----
# 背景：小米「添加账号」曾因缺 protobuf/gson 运行时而闪退，而当时的 `-dontwarn
# com.google.**` 让 R8 放过了缺失父类——**构建成功、运行期才炸**。
# 本检查扫描渠道 SDK jar 的外部引用，找出没有提供方的必需包（详见该脚本注释）。
$depCheck = Join-Path $PSScriptRoot "check_channel_deps.py"
if (Test-Path $depCheck) {
    Write-Host "`n-- 渠道 SDK 依赖完整性检查 --" -ForegroundColor Cyan
    $py = Get-Command python -ErrorAction SilentlyContinue
    if (-not $py) {
        Write-Warning "未找到 python，跳过渠道依赖检查（建议安装 Python 以启用此把关）"
    } else {
        & python $depCheck
        if ($LASTEXITCODE -ne 0) {
            Write-Error "渠道 SDK 依赖不完整（见上方清单）。补齐依赖后重试；" +
                "若确属可缺席的反射依赖，加入 check_channel_deps.py 的 OPTIONAL_PREFIXES。"
            exit 1
        }
        Write-Host "-- 渠道依赖检查通过 --" -ForegroundColor Green
    }
}

# ---- 3) 提链构建 ----
$typeFlag = if ($Debug) { "--debug" } else { "" }
Write-Host "`n-- cargo tauri android build $typeFlag --" -ForegroundColor Cyan
$buildArgs = @("tauri", "android", "build", "-t", ($Target.Split(",")[0]))
if ($Debug) { $buildArgs += "--debug" }
Push-Location $root
try {
    & cargo @buildArgs
    $code = $LASTEXITCODE
} finally {
    Pop-Location
}
if ($code -ne 0) { Write-Error "构建失败（exit $code）"; exit $code }

# ---- 4) 报告产物 ----
$apkPath = Join-Path $gen "app/build/outputs/apk/universal/$(if ($Debug) { 'debug' } else { 'release' })/app-universal-$(if ($Debug) { 'debug' } else { 'release' }).apk"
if (Test-Path $apkPath) {
    $size = (Get-Item $apkPath).Length
    Write-Host "`n== 构建完成 ==" -ForegroundColor Green
    Write-Host ("  APK : {0}" -f $apkPath)
    Write-Host ("  大小: {0:N0} B" -f $size)
    if ($CopyToDist) {
        $dist = Join-Path $root "dist"
        New-Item -ItemType Directory -Force -Path $dist | Out-Null
        $dst = Join-Path $dist "game-login-scanner-android-universal.apk"
        Copy-Item $apkPath $dst -Force
        $hash = (Get-FileHash $dst -Algorithm SHA256).Hash.Substring(0, 16)
        Write-Host ("  已复制到 dist/（SHA256 前16位 {0}）" -f $hash)
    }
} else {
    Write-Warning "未在预期路径找到 APK：$apkPath"
}
