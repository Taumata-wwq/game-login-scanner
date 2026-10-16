# 对 tauri android init 生成的 src-tauri/gen/android 应用本地化补丁。
#
# 背景：gen/android 不入库（.gitignore），且 tauri android init 会覆盖。
# 本脚本在每次 init 之后运行，把项目所需的本地化修改重新打上（幂等，可复现）。
#
# 用法：pwsh -File scripts/android/patch-gen.ps1
param(
    [string]$JavaHomeAscii = "G:/Java21",
    [string]$CargoBin = "C:/Users/$env:USERNAME/.cargo/bin/cargo.exe",
    [string]$GradleMirror = "https://mirrors.cloud.tencent.com/gradle/gradle-8.14.3-bin.zip",
    # 打包的 ABI 列表（决定 APK 体积）：默认只打真机架构，剔除 x86/x86_64（仅供模拟器）。
    # 实测 release .so：arm64 13.8MB / armv7 约 12MB；debug .so 每个 176~185MB（4 个 ABI ≈ 700MB）。
    # 若要极限压缩（仅 64 位真机）：-AbiList "arm64-v8a" -ArchList "arm64" -TargetList "aarch64"
    [string]$AbiList = "arm64-v8a,armeabi-v7a",
    [string]$ArchList = "arm64,arm",
    [string]$TargetList = "aarch64,armv7"
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
    # 注意：必须按“整行完全匹配”判断是否已存在。曾用 `^key=` 的前缀匹配 +
    # Add-Content：当文件末行没有换行符时，新属性会被**拼接**到最后一行上
    # （实测产出 `android.nonFinalResIds=falseabiList=arm64-v8a,...`，
    #   Gradle 报 "Cannot parse project property ... as boolean" 直接构建失败）。
    $lines = Get-Content $props
    $hit = $false
    foreach ($l in $lines) { if ($l -match ("^\s*" + [regex]::Escape($key) + "\s*=")) { $hit = $true; break } }
    if ($hit) {
        Write-Host "  = $key 已存在"
        return
    }
    # 读全文 → 补齐末尾换行 → 追加 → 整体写回（UTF-8 无 BOM，不可用 Add-Content：
    # 它在末行无换行的文件上会拼接）。
    $raw = [System.IO.File]::ReadAllText($props)
    if ($raw.Length -gt 0 -and -not $raw.EndsWith("`n")) { $raw += "`r`n" }
    $raw += $line + "`r`n"
    [System.IO.File]::WriteAllText($props, $raw, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "  + $line"
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

# 兼容清理：历史遗留的旧版插件源码目录（主包下 game_scanner/plugins）。
# 它会与 scanner/plugins 版同时参与编译，干扰 R8 的保留判定——2026-10-17
# 真机实证：屏幕捕获组件 "not found" + 帧桥类被 R8 移除即由此叠加引发。
$legacyDst = Join-Path $gen "app/src/main/java/com/taumata/game_scanner/plugins"
if (Test-Path $legacyDst) {
    Remove-Item -Recurse -Force $legacyDst
    Write-Host "  - 清理历史残留目录 game_scanner/plugins（旧版插件源码）"
} else {
    Write-Host "  = 无历史残留目录"
}

Write-Host "== 5b. 渠道 SDK 模块引入（mi_sdk，可选；vendor 本地材料）=="
# 渠道 SDK 二进制不入库（再分发条款限制）：vendor/mi_sdk 存在则引入小米 SDK 模块，
# 不存在则跳过（不影响基础构建）。模块来源：参考项目 bh3_login_simulation 的 mi_sdk。
$miSrc = Join-Path $root "vendor/mi_sdk"
$miDst = Join-Path $gen "mi_sdk"
if (-not (Test-Path $miSrc)) {
    Write-Host "  ! 未找到 vendor/mi_sdk（跳过渠道 SDK 引入；渠道登录将不可用）"
} else {
    # 镜像拷贝（先清后拷，保证与 vendor 一致）
    if (Test-Path $miDst) { Remove-Item -Recurse -Force $miDst }
    Copy-Item -Recurse -Force $miSrc $miDst
    Write-Host "  -> mi_sdk 模块已同步（gen/android/mi_sdk）"

    # settings.gradle：幂等追加 include（LF、UTF-8 无 BOM）
    $settingsPath = Join-Path $gen "settings.gradle"
    $sc = [System.IO.File]::ReadAllText($settingsPath)
    if ($sc -notmatch ":mi_sdk") {
        if (-not $sc.EndsWith("`n")) { $sc += "`n" }
        $sc += "include ':mi_sdk'`n"
        [System.IO.File]::WriteAllText($settingsPath, $sc, (New-Object System.Text.UTF8Encoding($false)))
        Write-Host "  + settings.gradle: include ':mi_sdk'"
    } else {
        Write-Host "  = settings.gradle 已含 :mi_sdk"
    }

    # app/build.gradle.kts：幂等插入模块依赖（CRLF 保持，仅替换首个 dependencies 块声明）
    $appGradlePath = Join-Path $gen "app/build.gradle.kts"
    $ag = [System.IO.File]::ReadAllText($appGradlePath)
    if ($ag -notmatch [regex]::Escape('project(":mi_sdk")')) {
        $ag = $ag.Replace("dependencies {", "dependencies {`r`n    implementation(project(`":mi_sdk`"))")
        [System.IO.File]::WriteAllText($appGradlePath, $ag, (New-Object System.Text.UTF8Encoding($false)))
        Write-Host '  + app/build.gradle.kts: implementation(project(":mi_sdk"))'
    } else {
        Write-Host "  = app/build.gradle.kts 已含 :mi_sdk 依赖"
    }
}

Write-Host "== 5c. 渠道混淆规则（参考项目 proguard 规则，幂等刷新）=="
# 渠道 SDK 需要整套 keep/dontwarn（含 -ignorewarnings 与 -dontwarn com.google.**，
# 否则 R8 full mode 因小米 SDK 的 protobuf 隐式引用报 Missing class 失败）。
# 规则在 gen/android/app/proguard-rules.pro 以标记段形式追加，每次刷新为最新。
$chanRules = Join-Path $root "scripts/android/channel-proguard-rules.pro"
$proguardPath = Join-Path $gen "app/proguard-rules.pro"
if (-not (Test-Path $chanRules)) {
    Write-Host "  ! 未找到 scripts/android/channel-proguard-rules.pro（跳过）"
} else {
    $marker = "# === channel proguard rules (from bh3_login_simulation) ==="
    $pg = [System.IO.File]::ReadAllText($proguardPath)
    if ($pg.Contains($marker)) {
        # 移除旧段后重写（支持规则文件更新）
        $pg = $pg.Substring(0, $pg.IndexOf($marker))
    }
    $rules = [System.IO.File]::ReadAllText($chanRules)
    $pg = $pg.TrimEnd() + "`n`n" + $marker + "`n" + $rules
    [System.IO.File]::WriteAllText($proguardPath, $pg, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "  + app/proguard-rules.pro 渠道路段已刷新"
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

# 相机权限（摄像头扫码）：独立幂等检查，插在 INTERNET 行之前。
if ($c -notmatch "android.permission.CAMERA") {
    $camPerm = '    <uses-permission android:name="android.permission.CAMERA" />' + $nl
    $c = $c.Replace(
        '    <uses-permission android:name="android.permission.INTERNET" />',
        $camPerm + '    <uses-permission android:name="android.permission.INTERNET" />'
    )
    $changed = $true
    Write-Host "  + CAMERA 权限（摄像头扫码）"
} else {
    Write-Host "  = CAMERA 权限已存在"
}

# 兼容修复：早期模板写入的 ".plugins.ScreenCaptureService" 为相对名，解析为
# com.taumata.game_scanner.plugins.*（该类不存在——插件实际包为 scanner.plugins）
# → 运行时组件 not found、屏幕捕获无法启动（2026-10-17 真机实证）。
$badSvc = 'android:name=".plugins.ScreenCaptureService"'
if ($c.Contains($badSvc)) {
    $c = $c.Replace($badSvc, 'android:name="com.taumata.scanner.plugins.ScreenCaptureService"')
    $changed = $true
    Write-Host "  ~ 修正 ScreenCaptureService 类名（相对名 -> 全限定名）"
}
if ($c -notmatch "com\.taumata\.scanner\.plugins\.ScreenCaptureService") {
    $svc = @(
        '        <service',
        '            android:name="com.taumata.scanner.plugins.ScreenCaptureService"',
        '            android:exported="false"',
        '            android:foregroundServiceType="mediaProjection" />',
        '    </application>'
    ) -join $nl
    $c = $c.Replace('    </application>', $svc)
    $changed = $true
    Write-Host "  + service 声明（com.taumata.scanner.plugins.ScreenCaptureService, mediaProjection）"
} else {
    Write-Host "  = service 声明已存在"
}

if ($changed) {
    [System.IO.File]::WriteAllText($manifest, $c, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "  Manifest 已更新"
}

Write-Host "== 7. gradle.properties：ABI 裁剪（APK 体积） =="
# tauri 生成的 RustPlugin.kt 读取 abiList / archList / targetList 三个 project 属性；
# 缺省为 4 个 ABI（含 x86/x86_64 模拟器架构），debug 下 .so 合计 ~700MB。
# 这里收窄为真实机型架构，使发布 APK 落在几十 MB 量级。
Add-Prop "abiList"    "abiList=$AbiList"
Add-Prop "archList"   "archList=$ArchList"
Add-Prop "targetList" "targetList=$TargetList"

Write-Host "== 8. release 签名配置（真机安装 / 分发用） =="
# tauri 默认只给 debug 包签名（debug keystore）；release 包不加签名配置会产出
# 无法安装的 unsigned APK。这里生成一个本地 release keystore（不入库，位于 gen/ 下）
# 并把签名配置写进 app/build.gradle.kts（幂等：已存在则跳过，不覆盖已有 keystore）。
# 必须放在 app/ 下：app/build.gradle.kts 里的 file("keystore.properties")
# 以 app 模块目录为基准解析（放在 gen/android 根目录会解析不到 → 产出 -unsigned.apk）。
$appDir = Join-Path $gen "app"
$ksPath = Join-Path $appDir "game-scanner-release.jks"
$ksProps = Join-Path $appDir "keystore.properties"
# 迁移：早期版本把这两个文件放在 gen/android 根目录。
$oldKs = Join-Path $gen "game-scanner-release.jks"
$oldProps = Join-Path $gen "keystore.properties"
if ((Test-Path $oldKs) -and -not (Test-Path $ksPath)) { Move-Item $oldKs $ksPath -Force; Write-Host "  → 迁移 keystore 到 app/" }
if ((Test-Path $oldProps) -and -not (Test-Path $ksProps)) {
    # storeFile 是相对路径，迁移后仍指向同目录的 jks，内容无需改动。
    Move-Item $oldProps $ksProps -Force; Write-Host "  → 迁移 keystore.properties 到 app/"
}
$keytool = Join-Path (Split-Path $JavaHomeAscii -Parent) "Java21/bin/keytool.exe"
if (-not (Test-Path $keytool)) { $keytool = "G:/Java21/bin/keytool.exe" }

if ((Test-Path $ksProps) -and (Test-Path $ksPath)) {
    Write-Host "  = keystore 已存在（跳过生成）"
} elseif (Test-Path $keytool) {
    $pass = -join ((1..24) | ForEach-Object { "0123456789abcdefghijklmnopqrstuvwxyz"[(Get-Random -Max 36)] })
    & $keytool -genkeypair -v -keystore $ksPath -alias game-scanner `
        -keyalg RSA -keysize 2048 -validity 10000 `
        -storepass $pass -keypass $pass `
        -dname "CN=Game Login Scanner, OU=Dev, O=Taumata, L=CN, ST=CN, C=CN" 2>&1 | Out-Null
    @(
        "storeFile=game-scanner-release.jks",
        "storePassword=$pass",
        "keyAlias=game-scanner",
        "keyPassword=$pass"
    ) | Set-Content -Path $ksProps -Encoding ASCII
    Write-Host "  + 生成 game-scanner-release.jks + keystore.properties（本地签名用，勿入库）"
} else {
    Write-Host "  ! 未找到 keytool，跳过签名配置"
}

if (Test-Path $ksProps) {
    $gradleTxt = Get-Content $appGradle -Raw
    if ($gradleTxt -notmatch "keystore.properties") {
        $lines = Get-Content $appGradle
        $out = New-Object System.Collections.ArrayList
        foreach ($l in $lines) {
            if ($l -match "^android \{") {
                [void]$out.Add('// release 签名参数（由 scripts/android/patch-gen.ps1 生成；不入库）')
                [void]$out.Add('val keystoreProperties = Properties().apply {')
                [void]$out.Add('    val f = file("keystore.properties")')
                [void]$out.Add('    if (f.exists()) { f.inputStream().use { load(it) } }')
                [void]$out.Add('}')
                [void]$out.Add('')
            }
            [void]$out.Add($l)
            if ($l -match '^\s+namespace = ') {
                [void]$out.Add('    signingConfigs {')
                [void]$out.Add('        if (keystoreProperties.getProperty("storeFile") != null) {')
                [void]$out.Add('            create("release") {')
                [void]$out.Add('                storeFile = file(keystoreProperties.getProperty("storeFile"))')
                [void]$out.Add('                storePassword = keystoreProperties.getProperty("storePassword")')
                [void]$out.Add('                keyAlias = keystoreProperties.getProperty("keyAlias")')
                [void]$out.Add('                keyPassword = keystoreProperties.getProperty("keyPassword")')
                [void]$out.Add('            }')
                [void]$out.Add('        }')
                [void]$out.Add('    }')
            }
            if ($l -match '^\s+getByName\("release"\) \{') {
                [void]$out.Add('            signingConfig = signingConfigs.findByName("release")')
            }
        }
        $out | Set-Content $appGradle
        Write-Host "  + app/build.gradle.kts 已加签名配置（signingConfigs.release）"
    } else {
        Write-Host "  = 签名配置已存在"
    }
}

Write-Host "全部补丁完成。"