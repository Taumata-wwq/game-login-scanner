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
# ---------------------------------------------------------------------------
# 换行常量：补丁文本里不再散落反引号转义，也便于跟随文件实际换行。
$LF = [string][char]10
$CRLF = [string][char]13 + [string][char]10

# ---------------------------------------------------------------------------
# 统一补丁入口（P1-1）。
#
# 背景：早先每个补丁都写成裸的 $text.Replace(anchor, insert)，而判定条件用的是
# 「插入串是否已存在」——插入串在**锚点缺失时必然不存在**，于是补丁静默变成 no-op，
# 却照样打印「+ …」成功日志。最危险的后果不是构建失败，而是**静默产出错误产物**：
# 签名块锚点漂移后 release 长期未签名、权限没进 manifest 却以为进了、依赖没加却
# 以为加了（后者表现为真机 NoClassDefFoundError 崩溃）。
# 因此统一走这里：锚点必须存在、命中次数必须符合预期、替换后文本必须真的变化，
# 否则直接 throw——可见的失败远好过静默的错误产物。
# ---------------------------------------------------------------------------
$script:PatchApplied = 0
function Set-Patch {
    param(
        [Parameter(Mandatory = $true)][string]$Text,
        [Parameter(Mandatory = $true)][string]$Anchor,
        [Parameter(Mandatory = $true)][string]$Insert,
        [Parameter(Mandatory = $true)][string]$Label,
        [int]$Expect = 1
    )
    if (-not $Text.Contains($Anchor)) {
        throw ("[补丁锚点缺失] " + $Label + $LF + "  锚点: " + $Anchor + $LF +
            "  gen/ 结构可能已被 tauri 或 AGP 升级改动，请同步更新 patch-gen.ps1")
    }
    $hits = ([regex]::Matches($Text, [regex]::Escape($Anchor))).Count
    if ($hits -ne $Expect) {
        throw ("[补丁锚点不唯一] " + $Label + $LF + "  锚点: " + $Anchor + $LF +
            "  期望命中 $Expect 次，实际 $hits 次")
    }
    $out = $Text.Replace($Anchor, $Insert)
    if ($out -eq $Text) {
        throw ("[补丁未生效] " + $Label + $LF + "  插入串与锚点相同，替换后文本未变化")
    }
    $script:PatchApplied++
    Write-Host ("  + " + $Label)
    return $out
}

Write-Host "== 1. gradle.properties 补丁 =="
$props = Join-Path $gen "gradle.properties"
function Add-Prop($key, $line) {
    # 注意：必须按“整行完全匹配”判断是否已存在。曾用前缀匹配 + Add-Content：
    # 当文件末行没有换行符时，新属性会被**拼接**到最后一行上（实测产出
    # android.nonFinalResIds=falseabiList=arm64-v8a,...，Gradle 报
    # "Cannot parse project property ... as boolean" 直接构建失败）。
    #
    # P1-2：命中时还要比对**值**。早期只判「key 出现过」就 return，于是
    # -AbiList / -ArchList / -TargetList / -JavaHomeAscii 这些参数一旦 gen 里
    # 已有一份（哪怕值不对）就被静默忽略——用户以为收窄了 ABI，实际仍打 4 个架构。
    $raw = [System.IO.File]::ReadAllText($props)
    $rx = [regex]::new("^\s*" + [regex]::Escape($key) + "\s*=",
        [System.Text.RegularExpressions.RegexOptions]::Multiline)
    $m = $rx.Match($raw)
    if ($m.Success) {
        $lineEnd = $raw.IndexOf($LF, $m.Index)
        if ($lineEnd -lt 0) { $lineEnd = $raw.Length }
        $current = $raw.Substring($m.Index, $lineEnd - $m.Index).TrimEnd([char]13)
        if ($current.Trim() -eq $line.Trim()) {
            Write-Host "  = $key 已存在"
            return
        }
        $newRaw = $raw.Substring(0, $m.Index) + $line + $raw.Substring($lineEnd)
        [System.IO.File]::WriteAllText($props, $newRaw, (New-Object System.Text.UTF8Encoding($false)))
        Write-Host "  ~ $key 已存在但值不同，已更新为：$line"
        return
    }
    # 未命中：读全文 → 补齐末尾换行 → 追加 → 整体写回（UTF-8 无 BOM；不可用
    # Add-Content，它在末行无换行的文件上会拼接）。
    if ($raw.Length -gt 0 -and -not $raw.EndsWith($LF)) { $raw += $CRLF }
    $raw += $line + $CRLF
    [System.IO.File]::WriteAllText($props, $raw, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "  + $line"
}

# Jetifier：渠道 SDK（mi_sdk 的 MiFileProvider 继承 android.support.v4.content.FileProvider）
# 仍是旧 support 库字节码，不转换则运行时 ClassNotFoundException →
# 「Unable to get provider com.xiaomi.gamecenter.sdk.utils.MiFileProvider」直接崩溃
# （2026-10-17 真机实证）。本项目其余依赖均为 androidx。
Add-Prop "android.useAndroidX" "android.useAndroidX=true"
Add-Prop "android.enableJetifier" "android.enableJetifier=true"
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
    $btHits = 0
    foreach ($l in $ls) {
        [void]$out.Add($l)
        # 锚点必须捕获 compileSdk 号：tauri 升级 SDK 时 buildToolsVersion 要跟着走；
        # 旧写法把锚点写死成 compileSdk = 36，漂移时**静默不插入**（P1-1 同类）。
        if ($l -match "^(\s*)compileSdk = (\d+)") {
            [void]$out.Add($Matches[1] + 'buildToolsVersion = "' + $Matches[2] + '.0.0"')
            $btHits++
        }
    }
    if ($btHits -ne 1) {
        throw ("[补丁锚点缺失] buildToolsVersion 插入点匹配到 $btHits 次（应为 1）：" + $LF +
            "  未找到 compileSdk = <n> 行；app/build.gradle.kts 结构可能已被 tauri 升级改动")
    }
    $out | Set-Content $appGradle
    Write-Host '  + buildToolsVersion = "<compileSdk>.0.0"'
} else {
    Write-Host "  = buildToolsVersion 已存在"
}


Write-Host "== 3b. 资源裁剪（release 体积） =="
# 两个开关都是「不给就浪费」的类型，实测合计 -1.13MB（17.24MB -> 16.12MB）：
#  - isShrinkResources：R8 已删除代码，但资源仍全量打包（华为 SDK 的 dimen/string/
#    drawable 未被引用也照进 arsc）。开启后按 R8 产出的资源引用图裁剪，-0.10MB。
#  - resourceConfigurations：AGP 默认打包依赖库里全部 85+ 种 locale（华为/小米/vivo
#    SDK 自带），而本应用 UI 全中文、只在 zh/en 环境使用。裁到 zh/en 后 -1.03MB，
#    主要是 strings/drawable 的多语言副本。
# 幂等：按标记行判断，已存在则跳过。
$agRc = [System.IO.File]::ReadAllText($appGradle)
$nlRc = if ($agRc.Contains("`r`n")) { "`r`n" } else { "`n" }
# 2026-10-22 起**关闭** isShrinkResources：AGP 的资源裁剪会把「代码里按字符串
# 动态引用」的 assets 一并删掉。UC/九游 SDK 正是这种形态——它靠 DexClassLoader 从
# getAssets() 读 assets/ucgamesdk/lib/ucsdk.apk 与各 ABI 的 lib_ucsdk_*.so，字符串
# 拼接而成，裁剪器看不到引用 → 实测 release 包内 assets/ucgamesdk/ 整目录消失
# （merge 中间产物里还在，说明是裁剪而非合并问题），真机 UC 登录必然失败。
# 代价：+0.10MB 的未引用资源（此前记的收益），远小于一个渠道不可用。
if ($agRc -match 'isShrinkResources\s*=\s*true') {
    $agRc = $agRc -replace 'isShrinkResources\s*=\s*true', 'isShrinkResources = false'
    Write-Host "  ~ isShrinkResources 由 true 改为 false（保住 UC assets）"
} elseif ($agRc -notmatch 'isShrinkResources') {
    $agRc = Set-Patch -Text $agRc -Anchor 'isMinifyEnabled = true' -Label "isShrinkResources = false（保住 UC assets）" -Insert ('isMinifyEnabled = true' + $nlRc + '            isShrinkResources = false')
} else {
    Write-Host "  = isShrinkResources = false 已存在"
}
if ($agRc -notmatch 'resourceConfigurations') {
    $agRc = Set-Patch -Text $agRc -Anchor 'minSdk = 24' -Label 'resourceConfigurations = ["zh", "en"]' -Insert ('minSdk = 24' + $nlRc + '        resourceConfigurations += listOf("zh", "en")')
} else {
    Write-Host "  = resourceConfigurations 已存在"
}

[System.IO.File]::WriteAllText($appGradle, $agRc, (New-Object System.Text.UTF8Encoding($false)))

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
    # P2-6：只覆盖不删除会在 gen 里留下**孤儿 .kt**（源目录已删掉的插件仍参与编译，
    # 且 R8 的保留判定会被多余类干扰）。华为插件来自 android-plugin-huawei，
    # 此处不能删，故排除两边都存在的名字。
    $keep = @{}
    $huaweiSrcDir = Join-Path $root "src-tauri/android-plugin-huawei"
    if (Test-Path $huaweiSrcDir) {
        Get-ChildItem -Path $huaweiSrcDir -Filter *.kt | ForEach-Object { $keep[$_.Name] = $true }
    }
    Get-ChildItem -Path $pluginDst -Filter *.kt | ForEach-Object {
        if ((Test-Path (Join-Path $pluginSrc $_.Name)) -or $keep.ContainsKey($_.Name)) { return }
        Remove-Item $_.FullName -Force
        Write-Host "  - 清理孤儿插件 $($_.Name)（源目录已无此文件）"
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

Write-Host "== 5a. MainActivity 返回键接管（修复 BACK 键 SIGABRT）=="
# 崩溃机制（2026-10-17 真机实证）：TauriActivity 把 handleBackNavigation 关成 false，
# 于是 wry 不注册 OnBackPressedCallback，BACK 直接走系统 finish() →
# onStop → Rust.stop() → tao Event::Stop → 事件循环退出 →
# tao::platform_impl::android::run 里 ::std::process::exit(exit_code) →
# __cxa_finalize 析构某个 joinable std::thread → std::terminate → SIGABRT
# （Abort message: 'terminating'，栈含 std::__1::thread::~thread+20）。
#
# 修法：本 Activity 独占返回键，先问前端是否消费（取景态 / 抽屉 / 弹窗），
# 未消费则 moveTaskToBack（回桌面保活），永不 finish，从而绕开 process::exit。
$mainActivity = Join-Path $gen "app/src/main/java/com/taumata/game_scanner/MainActivity.kt"
$mainKt = @"
package com.taumata.game_scanner

import android.content.Intent
import android.graphics.Color
import android.os.Bundle
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import com.taumata.scanner.plugins.NotifyBridge

class MainActivity : TauriActivity() {
    private var web: WebView? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        // 状态栏/导航栏图标恒为浅色。
        //
        // enableEdgeToEdge() 的默认参数是 SystemBarStyle.auto(...)，它按**系统**
        // 深浅色模式决定图标明暗：系统处于浅色模式时置 isAppearanceLightStatusBars
        // = true（深色图标）。而本应用界面恒为深色，于是状态栏的时间/电量等图标
        // 变成深色、压在同为深色的背景上看不清（真机截图实证）。
        // SystemBarStyle.dark() 的 detectDarkMode 恒为 true → isAppearanceLight-
        // StatusBars = false → 图标恒为浅色，与是否浅色模式无关。
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
        )
        super.onCreate(savedInstanceState)

        // 通知「停止扫码」按钮 → 本 Activity → 前端。用回调注册避免持有 Activity 引用。
        NotifyBridge.setStopScanCallback {
            web?.post {
                web?.evaluateJavascript(
                    "(window.__gsStopScanFromNotification&&window.__gsStopScanFromNotification())",
                    null,
                )
            }
        }

        // 返回键全权由本 Activity 接管。绝不调用 Activity.finish()：
        // 那会触发 onStop -> Rust.stop() -> tao 事件循环退出 -> std::process::exit，
        // 而 exit 时静态析构 joinable std::thread 会 std::terminate（SIGABRT）。
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                val w = web
                if (w == null) {
                    moveTaskToBack(true)
                    return
                }
                // 前端消费优先（关闭取景 / 收起抽屉 / 关弹窗），未消费则退到桌面。
                w.evaluateJavascript("(window.__gsHandleBack?window.__gsHandleBack():false)===true") { r ->
                    if (r != null && r.trim('"') == "true") return@evaluateJavascript
                    moveTaskToBack(true)
                }
            }
        })

        // 冷启动即由通知带入（应用未运行 → 点通知直接启动本 Activity）：在
        // onCreate 里就尝试派发一次跳转目标。
        handleOpenTarget(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        // 应用已在运行（singleTask + SINGLE_TOP）：通知点击只走到这里。
        setIntent(intent)
        handleOpenTarget(intent)
    }

    /**
     * 通知点击携带的跳转目标（`live:<platform>:<room>`）→ 前端定位到该直播间。
     *
     * 用户从通知进来时最想要的就是"回到那个直播间"，而不是自己再翻侧栏去找
     * （后台可能同时在扫多个房间）。前端函数不存在时静默忽略（例如启动早期）。
     */
    private fun handleOpenTarget(intent: Intent?) {
        val target = intent?.getStringExtra(NotifyBridge.EXTRA_OPEN_TARGET) ?: return
        if (target.isEmpty()) return
        intent.removeExtra(NotifyBridge.EXTRA_OPEN_TARGET)  // 只消费一次，避免重建时重复跳
        // 冷启动时 webview 可能尚未创建：短轮询等待就绪（最多约 3 秒），就绪后派发一次。
        //
        // 投递用 `window.decorView` 而非 TauriActivity 的 view：后者不存在（TauriActivity
        // 未暴露该属性，真机编译期报 Unresolved reference: view）；decorView 一定已就绪。
        var tries = 0
        val probe = object : Runnable {
            override fun run() {
                val w = web
                if (w == null) {
                    if (tries++ < 30) window?.decorView?.postDelayed(this, 100)
                    return
                }
                // 单引号转义：目标是内部生成的 `live:<platform>:<room>`，只含字母数字与冒号，
                // 这里仍做一次替换以防万一（避免 JS 字符串注入）。
                val safe = target.replace("'", "")
                // ⚠️ `$safe` 必须写成 `` `$safe ``：本段是 PowerShell 双引号 here-string，
                // 不转义的 `$safe` 会被 **PowerShell** 在生成期插值成空串（该变量只存在于
                // Kotlin），生成的 Kotlin 就变成 `__gsOpenTarget('')` —— 通知点击永远跳不动
                // （真机实测：传 live:bilibili:6 也不切换房间，日志无「已从通知打开直播间」）。
                w.evaluateJavascript("window.__gsOpenTarget&&window.__gsOpenTarget('`$safe')", null)
            }
        }
        window?.decorView?.post(probe)
    }

    override fun onWebViewCreate(webView: WebView) {
        super.onWebViewCreate(webView)
        web = webView
    }

    override fun onDestroy() {
        // 清掉静态回调，避免 Activity 重建后旧实例被 NotifyBridge 持有而泄漏。
        NotifyBridge.clearStopScanCallback()
        super.onDestroy()
    }
}
"@
[System.IO.File]::WriteAllText($mainActivity, $mainKt, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "  MainActivity.kt 已重写（返回键 -> 前端消费 / moveTaskToBack；系统栏图标恒为浅色）"

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
    $nlAg = if ($ag.Contains($CRLF)) { $CRLF } else { $LF }
    if ($ag -notmatch [regex]::Escape('project(":mi_sdk")')) {
        $ag = Set-Patch -Text $ag -Anchor "dependencies {" -Label 'app/build.gradle.kts: implementation(project(":mi_sdk"))' -Insert ('dependencies {' + $nlAg + '    implementation(project(":mi_sdk"))')
        [System.IO.File]::WriteAllText($appGradlePath, $ag, (New-Object System.Text.UTF8Encoding($false)))

    } else {
        Write-Host "  = app/build.gradle.kts 已含 :mi_sdk 依赖"
    }

    # protobuf-java：小米 SDK（mio_sdk_base）内含**生成的 protobuf 类**
    # （org.xiaomi.gamecenter.milink.msg.NoticeConfigProtos 等，实现
    # com.google.protobuf.ProtocolMessageEnum），但该 jar **不自带** protobuf 运行时，
    # 参考项目是靠其它依赖（leancloud 等）传递引入的，我们这条依赖链上没有。
    #
    # 缺它的后果不是「编译失败」，而是**运行时崩溃**（真机实证）：
    #   java.lang.NoClassDefFoundError:
    #     Failed resolution of: Lorg/xiaomi/gamecenter/milink/msg/NoticeConfigProtos$SdkType;
    #   at com.xiaomi.gamecenter.sdk.MiCommplatform.onUserAgreed
    #   at ChannelLoginPlugin.xiaomiLogin
    # 即「一点小米添加账号就闪退」。原因是 R8 被 channel-proguard-rules.pro 里的
    # `-dontwarn com.google.**` 放过了缺失的父类（GeneratedMessageV3/Descriptors…），
    # 构建照常通过，但打进去的类在 ART 上无法解析父类链 → 抛 NoClassDefFoundError。
    # 加真实依赖后父类链完整；这也解释了为何 debug 包（不混淆）同样会崩。
    if ($ag -notmatch [regex]::Escape('protobuf-java')) {
        $ag = [System.IO.File]::ReadAllText($appGradlePath)
        $ag = Set-Patch -Text $ag -Anchor "dependencies {" -Label 'app/build.gradle.kts: implementation("com.google.protobuf:protobuf-java:3.25.5")' -Insert ('dependencies {' + $nlAg + '    implementation("com.google.protobuf:protobuf-java:3.25.5")')
        [System.IO.File]::WriteAllText($appGradlePath, $ag, (New-Object System.Text.UTF8Encoding($false)))

    } else {
        Write-Host "  = app/build.gradle.kts 已含 protobuf-java 依赖"
    }

    # gson：同类问题（真机实证）。小米 SDK 里 **15 个类**引用 gson，包括
    # `com.xiaomi.gamecenter.sdk.utils.HyUtils` 与 `MiCommplatform` 本身：
    #   java.lang.NoClassDefFoundError: com.xiaomi.gamecenter.sdk.utils.HyUtils
    #   Caused by: ClassNotFoundException: com.google.gson.Gson
    # 修完 protobuf 后**紧接着**暴露出来的下一个缺口——说明缺的是一整组运行时库，
    # 故一次补齐（gson + eventbus 由 mi_sdk/libs 自带，此处只缺 gson）。
    if ($ag -notmatch [regex]::Escape('com.google.code.gson')) {
        $ag = [System.IO.File]::ReadAllText($appGradlePath)
        $ag = Set-Patch -Text $ag -Anchor "dependencies {" -Label 'app/build.gradle.kts: implementation("com.google.code.gson:gson:2.11.0")' -Insert ('dependencies {' + $nlAg + '    implementation("com.google.code.gson:gson:2.11.0")')
        [System.IO.File]::WriteAllText($appGradlePath, $ag, (New-Object System.Text.UTF8Encoding($false)))

    } else {
        Write-Host "  = app/build.gradle.kts 已含 gson 依赖"
    }
}

Write-Host "== 5c. 渠道混淆规则（参考项目 proguard 规则，幂等刷新）=="
# 渠道 SDK 需要整套 keep/dontwarn（含 -dontwarn com.google.**，否则 R8 full mode 因
# 小米 SDK 的 protobuf 隐式引用报 Missing class 失败）。注意**不要**加 -ignorewarnings：
# 它会把「必需依赖缺失」一并吞掉，正是小米「添加账号」NoClassDefFoundError 的成因
# （2026-10-21 审计 P3-14 已从 channel-proguard-rules.pro 移除）。
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

Write-Host "== 5d. 渠道 SDK 二进制同步（vendor/channel_sdk -> app/libs，可选）=="
# 与 5b 的 mi_sdk（源码模块）不同：vivo 等厂商只提供 jar/aar + .so 二进制，
# 无法以 Gradle 模块引入，改为放进 app/libs 并声明 fileTree 依赖。
# 目录结构沿用参考项目 app/libs：*.jar/*.aar 平铺，.so 按 ABI 子目录放（需
# sourceSets.jniLibs 指向 libs，否则 .so 不进 APK → 运行时 UnsatisfiedLinkError）。
$sdkSrc = Join-Path $root "vendor/channel_sdk"
$sdkDst = Join-Path $gen "app/libs"
if (-not (Test-Path $sdkSrc)) {
    Write-Host "  ! 未找到 vendor/channel_sdk（跳过；仅 mi_sdk 渠道可用）"
} else {
    New-Item -ItemType Directory -Force -Path $sdkDst | Out-Null
    Get-ChildItem -Path $sdkSrc -Recurse -File | ForEach-Object {
        $rel = $_.FullName.Substring($sdkSrc.Length).TrimStart('\', '/')
        $dst = Join-Path $sdkDst $rel
        New-Item -ItemType Directory -Force -Path (Split-Path $dst -Parent) | Out-Null
        Copy-Item $_.FullName -Destination $dst -Force
        Write-Host "  -> libs/$rel"
    }

    $ag2Path = Join-Path $gen "app/build.gradle.kts"
    $ft = 'implementation(fileTree(mapOf("dir" to "libs", "include" to listOf("*.jar", "*.aar"))))'
    $ag2 = [System.IO.File]::ReadAllText($ag2Path)
    # 换行符跟随文件（tauri 生成的是 LF；早期版本曾被改写为 CRLF，不能写死）。
    $nl2 = if ($ag2.Contains("`r`n")) { "`r`n" } else { "`n" }
    if ($ag2 -notmatch [regex]::Escape('fileTree(mapOf("dir" to "libs"')) {
        $ag2 = Set-Patch -Text $ag2 -Anchor "dependencies {" -Label "app/build.gradle.kts: fileTree(libs) 依赖" -Insert ("dependencies {" + $nl2 + "    $ft")
        [System.IO.File]::WriteAllText($ag2Path, $ag2, (New-Object System.Text.UTF8Encoding($false)))

    } else {
        Write-Host "  = fileTree(libs) 依赖已存在"
    }
    $ag2 = [System.IO.File]::ReadAllText($ag2Path)
    if ($ag2 -notmatch "jniLibs.srcDirs") {
        $srcSets = @(
            "    sourceSets {",
            '        getByName("main") {',
            '            jniLibs.srcDirs("libs")',
            "        }",
            "    }"
        ) -join $nl2
        $ag2 = Set-Patch -Text $ag2 -Anchor ("android {" + $nl2) -Label "app/build.gradle.kts: jniLibs.srcDirs(libs)" -Insert ("android {" + $nl2 + $srcSets + $nl2)
        [System.IO.File]::WriteAllText($ag2Path, $ag2, (New-Object System.Text.UTF8Encoding($false)))

    } else {
        Write-Host "  = jniLibs 配置已存在"
    }
}

Write-Host "== 5e. 华为渠道（条件启用：需 vendor/huawei/agconnect-services.json）=="
# 华为联运（HMS 游戏服务）依赖**应用级** AGC 配置：包名 + 签名指纹 + app_id 都由
# 华为开发者项目决定，无法随仓库分发。因此这里做成条件启用——文件不存在则完全不动
# 工程（主构建对 HMS 零依赖）；存在时才引入 maven 仓库 / agconnect 插件 / HMS SDK /
# 配置文件 / 华为登录源码。凭据放置处见 vendor/huawei/ 下的说明。
$huaweiJson = Join-Path $root "vendor/huawei/agconnect-services.json"
if (-not (Test-Path $huaweiJson)) {
    Write-Host "  ! 未找到 vendor/huawei/agconnect-services.json（跳过华为渠道；其余渠道不受影响）"
} else {
    # 1) 配置文件：AGC 插件要求放在 **app 模块根目录**
    $appJsonDst = Join-Path $gen "app/agconnect-services.json"
    Copy-Item $huaweiJson $appJsonDst -Force
    Write-Host "  -> app/agconnect-services.json 已同步"

    # 2) 根 build.gradle.kts：华为 maven 仓库 + agconnect 插件 classpath
    $rootGradle = Join-Path $gen "build.gradle.kts"
    $rg = [System.IO.File]::ReadAllText($rootGradle)
    if ($rg -notmatch "developer\.huawei\.com/repo") {
        # P1-1 同类：[regex]::Replace 匹配不到时同样静默无操作，先显式校验锚点。
        if (-not [regex]::IsMatch($rg, '(?m)^([ 	]*)mavenCentral\(\)(\r?\n)([ 	]*)\}')) {
            throw "[补丁锚点缺失] 根 build.gradle.kts 未找到 mavenCentral() 仓库块（华为渠道）"
        }
        if (-not [regex]::IsMatch($rg, '(?m)^([ 	]*)classpath\("org\.jetbrains\.kotlin:kotlin-gradle-plugin:[^"]+"\)(\r?\n)')) {
            throw "[补丁锚点缺失] 根 build.gradle.kts 未找到 kotlin-gradle-plugin classpath 行（华为渠道）"
        }
        $rg = [regex]::Replace(
            $rg,
            '(?m)^([ \t]*)mavenCentral\(\)(\r?\n)([ \t]*)\}',
            "`${1}mavenCentral()`${2}`${1}maven { url = uri(`"https://developer.huawei.com/repo/`") }`${2}`${3}}"
        )
        $rg = [regex]::Replace(
            $rg,
            '(?m)^([ \t]*)classpath\("org\.jetbrains\.kotlin:kotlin-gradle-plugin:[^"]+"\)(\r?\n)',
            "`${0}`${1}classpath(`"com.huawei.agconnect:agcp:1.9.1.301`")`${2}"
        )
        [System.IO.File]::WriteAllText($rootGradle, $rg, (New-Object System.Text.UTF8Encoding($false)))
        Write-Host "  + 根 build.gradle.kts: 华为 maven 仓库 + agcp 1.9.1.301"
    } else {
        Write-Host "  = 根 build.gradle.kts 已含华为仓库/插件"
    }

    # 3) app/build.gradle.kts：应用 agconnect 插件 + HMS 依赖（hwid 登录 / game 取角色）
    $appGradle2 = Join-Path $gen "app/build.gradle.kts"
    $ag3 = [System.IO.File]::ReadAllText($appGradle2)
    $nl3 = if ($ag3.Contains("`r`n")) { "`r`n" } else { "`n" }
    if ($ag3 -notmatch 'id\("com\.huawei\.agconnect"\)') {
        $ag3 = Set-Patch -Text $ag3 -Anchor ('    id("rust")' + $nl3 + '}') -Label 'app/build.gradle.kts: id("com.huawei.agconnect")' -Insert ('    id("rust")' + $nl3 + '    id("com.huawei.agconnect")' + $nl3 + '}')

    }
    foreach ($dep in @(
        'implementation("com.huawei.hms:hwid:6.8.0.300")',
        'implementation("com.huawei.hms:game:6.8.0.300")'
    )) {
        # 判断必须比对**整条**依赖串：只比版本号会把同版本的另一条依赖误判为已存在
        # （实测 hwid 先插入后，game 因共享 "6.8.0.300" 被跳过）。
        if ($ag3 -notmatch [regex]::Escape($dep)) {
            $ag3 = Set-Patch -Text $ag3 -Anchor "dependencies {" -Label ("app/build.gradle.kts: " + $dep) -Insert ("dependencies {" + $nl3 + "    " + $dep)

        }
    }
    [System.IO.File]::WriteAllText($appGradle2, $ag3, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "  + app/build.gradle.kts: agconnect 插件 + hwid/game 6.8.0.300"

    # 4) 华为登录源码（独立目录；不参与常规构建，仅在此处同步）
    $huaweiSrc = Join-Path $root "src-tauri/android-plugin-huawei"
    $pluginDst2 = Join-Path $gen "app/src/main/java/com/taumata/scanner/plugins"
    if (Test-Path $huaweiSrc) {
        New-Item -ItemType Directory -Force -Path $pluginDst2 | Out-Null
        Get-ChildItem -Path $huaweiSrc -Filter *.kt | ForEach-Object {
            Copy-Item $_.FullName -Destination $pluginDst2 -Force
            Write-Host "  -> $($_.Name)"
        }
        # P2-6 同类：源目录删掉的文件若不同步清理，gen 里会留下孤儿华为插件。
        if (Test-Path $pluginSrc) {
            Get-ChildItem -Path $pluginDst2 -Filter *.kt | ForEach-Object {
                if ((Test-Path (Join-Path $pluginSrc $_.Name)) -or (Test-Path (Join-Path $huaweiSrc $_.Name))) { return }
                Remove-Item $_.FullName -Force
                Write-Host "  - 清理孤儿插件 $($_.Name)（两个源目录均已无此文件）"
            }
        }
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
    $c = Set-Patch -Text $c -Anchor '    <uses-permission android:name="android.permission.INTERNET" />' -Insert $permLines -Label "4 条权限（INTERNET/FOREGROUND_SERVICE/FOREGROUND_SERVICE_MEDIA_PROJECTION/POST_NOTIFICATIONS）"
    $changed = $true

} else {
    Write-Host "  = 权限已存在"
}

# 相机权限（摄像头扫码）：独立幂等检查，插在 INTERNET 行之前。
if ($c -notmatch "android.permission.CAMERA") {
    $camPerm = '    <uses-permission android:name="android.permission.CAMERA" />' + $nl
    $c = Set-Patch -Text $c -Anchor '    <uses-permission android:name="android.permission.INTERNET" />' -Insert ($camPerm + '    <uses-permission android:name="android.permission.INTERNET" />') -Label "CAMERA 权限（摄像头扫码）"
    $changed = $true

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
    $c = Set-Patch -Text $c -Anchor '    </application>' -Insert $svc -Label "service 声明（com.taumata.scanner.plugins.ScreenCaptureService, mediaProjection）"
    $changed = $true

} else {
    Write-Host "  = service 声明已存在"
}

# NotifyActionReceiver：承接常驻通知上「停止扫码」按钮的广播（2026-10-19 新增）。
# exported=false：只由本应用的 PendingIntent 触发，不对外暴露（避免被第三方伪装广播
# 触发停止扫码）。
if ($c -notmatch "com\.taumata\.scanner\.plugins\.NotifyActionReceiver") {
    $rcv = @(
        '        <receiver',
        '            android:name="com.taumata.scanner.plugins.NotifyActionReceiver"',
        '            android:exported="false" />',
        '    </application>'
    ) -join $nl
    $c = Set-Patch -Text $c -Anchor '    </application>' -Insert $rcv -Label "receiver 声明（NotifyActionReceiver，通知动作广播）"
    $changed = $true

} else {
    # 声明已存在也可能 `exported` 被改成 true（例如排障时临时放开过）——
    # 仅凭"名字出现过"就跳过会让错误的 exported 一直留在包里，故这里显式纠正回 false。
    $rcvPat = '(?s)(NotifyActionReceiver[^>]*?android:exported=")true(")'
    if ($c -match $rcvPat) {
        $c = $c -replace $rcvPat, '${1}false${2}'
        $changed = $true
        Write-Host "  ~ receiver 声明已存在，但 exported 被改为 true —— 已纠正回 false"
    } else {
        Write-Host "  = receiver 声明已存在（exported=false）"
    }
}

if ($changed) {
    [System.IO.File]::WriteAllText($manifest, $c, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "  Manifest 已更新"
}

Write-Host "== 6b. AndroidManifest：渠道 SDK 声明（vivo） =="
# vivo 联合 SDK 以 jar 形式引入，组件/权限不会随 aar 清单自动合并，必须手写到主清单：
#   - 权限 vivo.game.permission.OPEN_JUMP_INTENTS
#   - meta-data vivo_union_sdk（SDK 版本号）
#   - UnionActivity + vivounion://openjump 深链（SDK 登录 UI 容器）
# 声明内容照参考项目 AndroidManifest.xml（vivo 段）。
$mf2 = Join-Path $gen "app/src/main/AndroidManifest.xml"
$mc = [System.IO.File]::ReadAllText($mf2)
$nl2 = if ($mc.Contains("`r`n")) { "`r`n" } else { "`n" }
$mChg = $false

if ($mc -notmatch "vivo\.game\.permission\.OPEN_JUMP_INTENTS") {
    $mc = Set-Patch -Text $mc -Anchor '    <uses-permission android:name="android.permission.INTERNET" />' -Label "vivo 权限（OPEN_JUMP_INTENTS）" -Insert ('    <uses-permission android:name="vivo.game.permission.OPEN_JUMP_INTENTS" />' + $nl2 + '    <uses-permission android:name="android.permission.INTERNET" />')
    $mChg = $true

} else {
    Write-Host "  = vivo 权限已存在"
}

if ($mc -notmatch 'android:name="vivo_union_sdk"') {
    $meta = @(
        '        <meta-data',
        '            android:name="vivo_union_sdk"',
        '            android:value="4.6.0.1" />',
        '    </application>'
    ) -join $nl2
    $mc = Set-Patch -Text $mc -Anchor '    </application>' -Insert $meta -Label "vivo meta-data（vivo_union_sdk=4.6.0.1）"
    $mChg = $true

} else {
    Write-Host "  = vivo meta-data 已存在"
}

if ($mc -notmatch "com\.vivo\.unionsdk\.ui\.UnionActivity") {
    $act = @(
        '        <activity',
        '            android:name="com.vivo.unionsdk.ui.UnionActivity"',
        '            android:configChanges="orientation|keyboardHidden|navigation|screenSize"',
        '            android:exported="true"',
        '            android:theme="@android:style/Theme.Dialog">',
        '            <intent-filter>',
        '                <action android:name="android.intent.action.VIEW" />',
        '',
        '                <category android:name="android.intent.category.DEFAULT" />',
        '                <category android:name="android.intent.category.BROWSABLE" />',
        '',
        '                <data',
        '                    android:host="union.vivo.com"',
        '                    android:path="/openjump"',
        '                    android:scheme="vivounion" />',
        '            </intent-filter>',
        '        </activity>',
        '    </application>'
    ) -join $nl2
    $mc = Set-Patch -Text $mc -Anchor '    </application>' -Insert $act -Label "vivo UnionActivity 声明"
    $mChg = $true

} else {
    Write-Host "  = vivo UnionActivity 已存在"
}

# B 站 SDK（BSGameSdk）读取 meta-data BSGameSdk_PaidGame（是否付费游戏；
# 崩坏3 为免费游戏 → false）。渠道 aar 自带的清单不含该字段，需主清单声明。
if ($mc -notmatch 'android:name="BSGameSdk_PaidGame"') {
    $bmeta = @(
        '        <meta-data',
        '            android:name="BSGameSdk_PaidGame"',
        '            android:value="false" />',
        '    </application>'
    ) -join $nl2
    $mc = Set-Patch -Text $mc -Anchor '    </application>' -Insert $bmeta -Label "B站 SDK meta-data（BSGameSdk_PaidGame=false）"
    $mChg = $true

} else {
    Write-Host "  = B站 SDK meta-data 已存在"
}

# 应用宝 / Wegame 的网页登录容器（本项目自建 Activity，源码在 android-plugin）。
# 无 SDK 清单可合并，必须显式声明；theme 取系统无标题样式即可（全屏 WebView）。
if ($mc -notmatch "ChannelWebLoginActivity") {
    $webAct = @(
        '        <activity',
        '            android:name="com.taumata.scanner.plugins.ChannelWebLoginActivity"',
        '            android:configChanges="orientation|keyboardHidden|navigation|screenSize"',
        '            android:exported="false"',
        '            android:theme="@android:style/Theme.NoTitleBar" />',
        '    </application>'
    ) -join $nl2
    $mc = Set-Patch -Text $mc -Anchor '    </application>' -Insert $webAct -Label "渠道网页登录 Activity（应用宝 / Wegame）"
    $mChg = $true
} else {
    # P2-10：与 NotifyActionReceiver 同理——声明已存在也可能 exported 被改成 true
    # （排障时临时放开过就会留在包里，等于把登录容器暴露给任意应用），显式纠正回 false。
    $webPat = '(?s)(ChannelWebLoginActivity[^>]*?android:exported=")true(")'
    if ($mc -match $webPat) {
        $mc = $mc -replace $webPat, '${1}false${2}'
        $mChg = $true
        Write-Host "  ~ 渠道网页登录 Activity 已存在，但 exported 被改为 true —— 已纠正回 false"
    } else {
        Write-Host "  = 渠道网页登录 Activity 已存在（exported=false）"
    }
}


if ($mChg) {
    [System.IO.File]::WriteAllText($mf2, $mc, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "  Manifest（渠道段）已更新"
}


Write-Host "== 6c. AndroidManifest：UC / OPPO 渠道声明（条件：vendor/channel_sdk 有对应 aar）=="
# UC（net-sdk-*.aar）与 OPPO（gamesdk-*.aar）都以 aar 形式引入，清单会自动合并；
# 但参考项目仍在**主清单**里显式声明这些组件/meta-data，原因是：
#   - UC 的 PullupActivity 需要 scheme="ng<gameId>"（aar 内只声明 "ng"），且
#     taskAffinity/theme 与 aar 默认值冲突 → 必须 tools:node="replace" 整条覆盖；
#   - OPPO 的 debug_mode / is_offline_game / app_key 三条 meta-data 由 SDK 直接读
#     PackageManager，aar 清单里没有，只能写在主清单。
# 声明内容照参考项目 AndroidManifest.xml（UC 段 / OPPO 段），逐字对齐。
$mf3 = Join-Path $gen "app/src/main/AndroidManifest.xml"
$m3 = [System.IO.File]::ReadAllText($mf3)
# 自愈：早期版本曾把换行拼成字面量 "\n" 写进清单（XML 解析直接失败）。
# 这里一次性纠正，避免「清单已损坏 + 幂等检查命中 → 再也不修」的死锁。
if ($m3.Contains('\n')) {
    $m3 = $m3.Replace('\n', [string][char]10)
    [System.IO.File]::WriteAllText($mf3, $m3, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "  ~ 清单里发现字面量 \n，已纠正为真实换行"
}
$nl3 = if ($m3.Contains($CRLF)) { $CRLF } else { $LF }
$m3Chg = $false

$ucAar = Get-ChildItem -Path (Join-Path $root "vendor/channel_sdk") -Filter "net-sdk-*.aar" -ErrorAction SilentlyContinue | Select-Object -First 1
$oppoAar = Get-ChildItem -Path (Join-Path $root "vendor/channel_sdk") -Filter "gamesdk-*.aar" -ErrorAction SilentlyContinue | Select-Object -First 1

if (-not $ucAar -and -not $oppoAar) {
    Write-Host "  ! vendor/channel_sdk 无 UC/OPPO aar（跳过）"
} else {
    if ($ucAar -and $m3 -notmatch 'xmlns:tools=') {
        $m3 = $m3.Replace('<manifest xmlns:android="http://schemas.android.com/apk/res/android">', '<manifest xmlns:android="http://schemas.android.com/apk/res/android"' + $nl3 + '    xmlns:tools="http://schemas.android.com/tools">')
        $m3Chg = $true
        Write-Host "  + manifest 根元素补 xmlns:tools"
    }

    if ($ucAar) {
        if ($m3 -notmatch 'cn\.uc\.gamesdk\.activity\.PullupActivity') {
            $ucBlock = @(
                '        <!-- UC / 九游（net-sdk aar 清单只声明 scheme="ng"，需整条覆盖为 ng<gameId>） -->',
                '        <activity',
                '            android:name="cn.gundam.sdk.shell.activity.ThemeProxyActivity"',
                '            android:exported="true" />',
                '        <activity',
                '            android:name="cn.uc.gamesdk.activity.PullupActivity"',
                '            android:excludeFromRecents="true"',
                '            android:exported="true"',
                '            android:label="PullupActivity"',
                '            android:launchMode="singleTop"',
                '            android:taskAffinity="cn.uc.gamesdk.demo.diff"',
                '            android:theme="@android:style/Theme.Translucent"',
                '            tools:node="replace">',
                '            <intent-filter>',
                '                <action android:name="android.intent.action.VIEW" />',
                '',
                '                <category android:name="android.intent.category.DEFAULT" />',
                '                <category android:name="android.intent.category.BROWSABLE" />',
                '',
                '                <data android:scheme="ng654463" />',
                '            </intent-filter>',
                '        </activity>',
                '        <activity',
                '            android:name="cn.gundam.sdk.shell.activity.ProxyActivity"',
                '            android:configChanges="keyboardHidden|orientation|screenSize"',
                '            android:exported="true"',
                '            android:theme="@android:style/Theme.Translucent.NoTitleBar"',
                '            android:windowSoftInputMode="adjustResize">',
                '            <intent-filter>',
                '                <action android:name="cn.uc.gamesdk.sdkweb" />',
                '',
                '                <category android:name="android.intent.category.DEFAULT" />',
                '            </intent-filter>',
                '        </activity>',
                '        <service',
                '            android:name="cn.gundam.sdk.shell.service.ProxyService"',
                '            android:exported="true"',
                '            android:process=":bgservice" />',
                '    </application>'
            ) -join $nl3
            $m3 = Set-Patch -Text $m3 -Anchor '    </application>' -Insert $ucBlock -Label "UC 组件声明（4 条）"
            $m3Chg = $true
        } else {
            Write-Host "  = UC 组件声明已存在"
        }
    }

    if ($oppoAar) {
        if ($m3 -notmatch 'android:name="app_key"') {
            $oppoMeta = @(
                '        <!-- OPPO：SDK 直接读 PackageManager 的 meta-data（aar 清单里没有） -->',
                '        <meta-data',
                '            android:name="debug_mode"',
                '            android:value="true" />',
                '        <meta-data',
                '            android:name="is_offline_game"',
                '            android:value="false" />',
                '        <meta-data',
                '            android:name="app_key"',
                '            android:value="7NEgcusdF1ss0cK8CSwo8Ksk0" />',
                '    </application>'
            ) -join $nl3
            $m3 = Set-Patch -Text $m3 -Anchor '    </application>' -Insert $oppoMeta -Label "OPPO meta-data（app_key / debug_mode / is_offline_game）"
            $m3Chg = $true
        } else {
            Write-Host "  = OPPO meta-data 已存在"
        }

        if ($m3 -notmatch 'com\.heytap\.htms') {
            $q = @(
                '    <!-- Android 11+ 包可见性：渠道 SDK 需要查询的第三方包（照参考项目 queries） -->',
                '    <queries>',
                '        <package android:name="com.heytap.htms" />',
                '        <package android:name="com.heytap.openid" />',
                '        <package android:name="com.miHoYo.bh3.nearme.gamecenter" />',
                '        <package android:name="com.xiaomi.gamecenter.sdk.service" />',
                '        <package android:name="com.huawei.hwid" />',
                '        <package android:name="com.huawei.hms" />',
                '    </queries>',
                '    <application'
            ) -join $nl3
            $m3 = Set-Patch -Text $m3 -Anchor '    <application' -Insert $q -Label "queries（渠道 SDK 包可见性）"
            $m3Chg = $true
        } else {
            Write-Host "  = queries 已存在"
        }
    }

    if ($m3Chg) {
        [System.IO.File]::WriteAllText($mf3, $m3, (New-Object System.Text.UTF8Encoding($false)))
        Write-Host "  Manifest（UC/OPPO 段）已更新"
    }
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
# P2-9：keytool 必须从**本脚本实际使用的 JAVA_HOME** 推导。早期写法是
# Join-Path (Split-Path $JavaHomeAscii -Parent) "Java21/bin/keytool.exe"，
# 只在默认 G:/Java21 下凑巧成立——换成 JDK 17 或其它路径就找不到 keytool，
# 于是静默跳过签名，产出无法安装的 unsigned release APK。
$keytool = Join-Path $JavaHomeAscii "bin/keytool.exe"
if (-not (Test-Path $keytool)) { $keytool = Join-Path (Split-Path $JavaHomeAscii -Parent) "Java21/bin/keytool.exe" }

if ((Test-Path $ksProps) -and (Test-Path $ksPath)) {
    Write-Host "  = keystore 已存在（跳过生成）"
} elseif (Test-Path $keytool) {
    $pass = -join ((1..24) | ForEach-Object { "0123456789abcdefghijklmnopqrstuvwxyz"[(Get-Random -Max 36)] })
    & $keytool -genkeypair -v -keystore $ksPath -alias game-scanner `
        -keyalg RSA -keysize 2048 -validity 10000 `
        -storepass $pass -keypass $pass `
        -dname "CN=Game Login Scanner, OU=Dev, O=Taumata, L=CN, ST=CN, C=CN" 2>&1 | Out-Null
    # 先确认 keytool 真的产出了 jks，再写 keystore.properties：否则会留下一份
    # 指向不存在 keystore 的配置，Gradle 侧静默无签名（P2-9）。
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $ksPath)) {
        throw ("[签名 keystore 生成失败] keytool 退出码 $LASTEXITCODE，未产出 $ksPath" + $LF +
            "  请检查 -JavaHomeAscii（当前：$JavaHomeAscii）与 keytool：$keytool")
    }
    @(
        "storeFile=game-scanner-release.jks",
        "storePassword=$pass",
        "keyAlias=game-scanner",
        "keyPassword=$pass"
    ) | Set-Content -Path $ksProps -Encoding ASCII
    Write-Host "  + 生成 game-scanner-release.jks + keystore.properties（本地签名用，勿入库）"
} else {
    Write-Host "  ! 未找到 keytool（$keytool），跳过签名配置"
}

if (Test-Path $ksProps) {
    # P1-3：守卫必须看**我们自己写入的标记**。旧守卫是 -notmatch "keystore.properties"
    # ——该字符串同时出现在下面 Properties() 块里，于是锚点一旦漂移、块没插进去，
    # 后续每次运行都会因「文本里已经有这个字符串」而跳过，签名块永远补不上。
    $signMarker = '// release 签名参数（由 scripts/android/patch-gen.ps1 生成；不入库）'
    $gradleTxt = Get-Content $appGradle -Raw
    if ($gradleTxt -notmatch [regex]::Escape($signMarker)) {
        $lines = Get-Content $appGradle
        # 三个真锚点先校验唯一性：行投影匹配不到就静默少插，结果是无签名配置的 release。
        $nAndroid = @($lines | Where-Object { $_ -match '^android \{' }).Count
        $nNamespace = @($lines | Where-Object { $_ -match '^\s+namespace = ' }).Count
        $nRelease = @($lines | Where-Object { $_ -match '^\s+getByName\("release"\) \{' }).Count
        if ($nAndroid -ne 1 -or $nNamespace -ne 1 -or $nRelease -ne 1) {
            throw ("[补丁锚点异常] 签名配置插入点：android = $nAndroid，namespace = $nNamespace，release = $nRelease（各应为 1）")
        }
        $out = New-Object System.Collections.ArrayList
        foreach ($l in $lines) {
            if ($l -match '^android \{') {
                [void]$out.Add($signMarker)
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
        # 写入后自校验：真正落盘了才算成功（锚点漂移时这里会可见地失败）。
        $after = Get-Content $appGradle -Raw
        if ($after -notmatch [regex]::Escape($signMarker) -or
            $after -notmatch 'signingConfigs \{' -or
            $after -notmatch 'signingConfig = signingConfigs\.findByName\("release"\)') {
            throw "[补丁未生效] 签名配置写入后校验失败（signingConfigs / signingConfig 行缺失）"
        }
        Write-Host "  + app/build.gradle.kts 已加签名配置（signingConfigs.release）"
    } else {
        Write-Host "  = 签名配置已存在"
    }
}


Write-Host ("全部补丁完成（本次实际应用 " + $script:PatchApplied + " 处补丁，其余为已存在）。")
