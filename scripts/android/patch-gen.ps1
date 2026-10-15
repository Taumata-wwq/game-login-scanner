# 对 tauri android init 生成的 src-tauri/gen/android 应用本地化补丁。
#
# 背景：gen/android 不入库（.gitignore），且 tauri android init 会覆盖。
# 本脚本在每次 init 之后运行，把项目所需的本地化修改重新打上（幂等，可复现）。
#
# 用法：pwsh -File scripts/android/patch-gen.ps1
param(
    [string]$JavaHomeAscii = "G:/Java21",
    [string]$CargoBin = "C:/Users/$env:USERNAME/.cargo/bin/cargo.exe",
    [string]$GradleMirror = "https://mirrors.cloud.tencent.com/gradle/gradle-8.14.3-bin.zip"
)

$ErrorActionPreference = "Stop"
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$gen = Join-Path $root "src-tauri/gen/android"

if (-not (Test-Path $gen)) {
    Write-Error "未找到 $gen，请先运行 cargo tauri android init"
    exit 1
}

Write-Host "== 1. gradle.properties 补丁 =="
$props = Join-Path $gen "gradle.properties"
function Add-Prop($key, $line) {
    $content = Get-Content $props -Raw
    if ($content -notmatch ("(?m)^" + [regex]::Escape($key) + "=")) {
        Add-Content $props $line
        Write-Host "  + $line"
    } else {
        Write-Host "  = $key 已存在"
    }
}
Add-Prop "org.gradle.daemon" "org.gradle.daemon=false"
Add-Prop "org.gradle.java.home" "org.gradle.java.home=$JavaHomeAscii"
Add-Prop "android.overridePathCheck" "android.overridePathCheck=true"

Write-Host "== 2. gradle-wrapper.properties 镜像 =="
$wrapper = Join-Path $gen "gradle/wrapper/gradle-wrapper.properties"
(Get-Content $wrapper) -replace "^distributionUrl=.*", "distributionUrl=$GradleMirror" | Set-Content $wrapper
Write-Host "  distributionUrl -> $GradleMirror"

Write-Host "== 3. app/build.gradle.kts buildToolsVersion =="
$appGradle = Join-Path $gen "app/build.gradle.kts"
if ((Get-Content $appGradle -Raw) -notmatch "buildToolsVersion") {
    $ls = Get-Content $appGradle
    $out = New-Object System.Collections.ArrayList
    foreach ($l in $ls) {
        [void]$out.Add($l)
        if ($l -match "^    compileSdk = 36") {
            [void]$out.Add('    buildToolsVersion = "36.0.0"')
        }
    }
    $out | Set-Content $appGradle
    Write-Host '  + buildToolsVersion = "36.0.0"'
} else {
    Write-Host "  = buildToolsVersion 已存在"
}

Write-Host "== 4. BuildTask.kt（cmd 包装 cargo + 注入 PATH）=="
$buildTask = Join-Path $gen "buildSrc/src/main/java/com/taumata/game_scanner/kotlin/BuildTask.kt"
$cargoDir = $CargoBin.Substring(0, $CargoBin.LastIndexOf("/"))
$kt = @"
import java.io.File
import org.gradle.api.DefaultTask
import org.gradle.api.GradleException
import org.gradle.api.logging.LogLevel
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.TaskAction

open class BuildTask : DefaultTask() {
    @Input
    var rootDirRel: String? = null
    @Input
    var target: String? = null
    @Input
    var release: Boolean? = null

    @TaskAction
    fun assemble() {
        val rootDirRel = rootDirRel ?: throw GradleException("rootDirRel cannot be null")
        val target = target ?: throw GradleException("target cannot be null")
        val release = release ?: throw GradleException("release cannot be null")

        // 通过 cmd.exe 调用 cargo：Windows 上 Gradle 直接 spawn 绝对路径会
        // 错误拼成 "cargo.exe.bat"（A problem occurred starting process）。
        val cargoBin = "$CargoBin"
        val tauriArgs = mutableListOf("tauri", "android", "android-studio-script")
        if (project.logger.isEnabled(LogLevel.DEBUG)) {
            tauriArgs.add("-vv")
        } else if (project.logger.isEnabled(LogLevel.INFO)) {
            tauriArgs.add("-v")
        }
        if (release) {
            tauriArgs.add("--release")
        }
        tauriArgs.add("--target")
        tauriArgs.add(target)

        project.exec {
            workingDir(File(project.projectDir, rootDirRel))
            commandLine(listOf("cmd", "/c", cargoBin) + tauriArgs)
            environment("PATH", "$cargoDir;" + System.getenv("PATH"))
        }.assertNormalExitValue()
    }
}
"@
Set-Content -Path $buildTask -Value $kt -Encoding UTF8
Write-Host "  BuildTask.kt 已重写"

Write-Host "== 5. Kotlin 插件源码同步（src-tauri/android-plugin -> gen/android）=="
$pluginSrc = Join-Path $root "src-tauri/android-plugin"
# 插件独立包名（不含下划线，避免 JNI 符号 _1 转义）：
# com.taumata.scanner.plugins
$pluginDst = Join-Path $gen "app/src/main/java/com/taumata/scanner/plugins"
if (-not (Test-Path $pluginSrc)) {
    Write-Host "  ! 未找到 $pluginSrc（跳过）"
} else {
    New-Item -ItemType Directory -Force -Path $pluginDst | Out-Null
    Get-ChildItem -Path $pluginSrc -Filter *.kt | ForEach-Object {
        # 二进制复制：不改动编码（Kotlin 源含中文注释，须保持 UTF-8）。
        Copy-Item $_.FullName -Destination $pluginDst -Force
        Write-Host "  -> $($_.Name)"
    }
}

Write-Host "== 6. AndroidManifest：屏幕捕获权限与前台服务声明 =="
$manifest = Join-Path $gen "app/src/main/AndroidManifest.xml"
$c = Get-Content $manifest -Raw
$nl = if ($c.Contains("`r`n")) { "`r`n" } else { "`n" }
$changed = $false

if ($c -notmatch "FOREGROUND_SERVICE_MEDIA_PROJECTION") {
    $permLines = @(
        '    <uses-permission android:name="android.permission.INTERNET" />',
        '    <uses-permission android:name="android.permission.FOREGROUND_SERVICE" />',
        '    <uses-permission android:name="android.permission.FOREGROUND_SERVICE_MEDIA_PROJECTION" />',
        '    <uses-permission android:name="android.permission.POST_NOTIFICATIONS" />'
    ) -join $nl
    $c = $c.Replace(
        '    <uses-permission android:name="android.permission.INTERNET" />',
        $permLines
    )
    $changed = $true
    Write-Host "  + 4 条权限（INTERNET/FOREGROUND_SERVICE/FOREGROUND_SERVICE_MEDIA_PROJECTION/POST_NOTIFICATIONS）"
} else {
    Write-Host "  = 权限已存在"
}

if ($c -notmatch "ScreenCaptureService") {
    $svc = @(
        '        <service',
        '            android:name=".plugins.ScreenCaptureService"',
        '            android:exported="false"',
        '            android:foregroundServiceType="mediaProjection" />',
        '    </application>'
    ) -join $nl
    $c = $c.Replace('    </application>', $svc)
    $changed = $true
    Write-Host "  + service 声明（.plugins.ScreenCaptureService, mediaProjection）"
} else {
    Write-Host "  = service 声明已存在"
}

if ($changed) {
    [System.IO.File]::WriteAllText($manifest, $c, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "  Manifest 已更新"
}

Write-Host "全部补丁完成。"