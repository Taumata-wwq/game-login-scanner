# 渠道混淆/压缩规则（来源：参考项目 bh3_login_simulation 的 app/proguard-rules.pro，
# 2026-10-17 迁移；依据「与参考项目实现一致」原则整段引入）。
# 本地化修正：移除 -printconfiguration /tmp/... （Linux 路径，Windows 构建不适用）。

# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# If your project uses WebView with JS, uncomment the following
# and specify the fully qualified class name to the JavaScript interface
# class:
#-keepclassmembers class fqcn.of.javascript.interface.for.webview {
#   public *;
#}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile
-optimizationpasses 5          # 指定代码的压缩级别
-dontusemixedcaseclassnames   # 是否使用大小写混合
-verbose

-keep class com.nearme.** { *; }
-keep class com.bun.miitmdid.core.** {*;}
-keep class audio.mve.** { *; }
-keep class com.hh.** { *; }
-keep class com.bilibili.** { *; }
-keep class bsgamesdkhttp.** { *; }
-keep class bsgamesdkio.** { *; }
-keep class com.bsgamesdk.** { *; }

-dontwarn com.tencent.bugly.**
-keep public class com.tencent.bugly.**{*;}

-keepattributes Signature
-dontwarn com.jcraft.jzlib.**
-keep class com.jcraft.jzlib.**  { *;}

-dontwarn sun.misc.**
-keep class sun.misc.** { *;}

-dontwarn retrofit2.**
-keep class retrofit2.** { *;}

-dontwarn io.reactivex.**
-keep class io.reactivex.** { *;}

-dontwarn sun.security.**
-keep class sun.security.** { *; }

# ⚠️ 收窄说明（2026-10-18）：
# `-dontwarn com.google.**` 原本是整包豁免，把 `com.google.protobuf.*` 这类**必需**的
# 运行时也一起放过了——构建照常通过，但打进去的类在 ART 上解析不到父类，
# 运行期抛 NoClassDefFoundError（小米「添加账号」闪退的真因）。
# 现改为**逐包**豁免：只放过渠道 SDK 里真正可选/反射才用到的第三方包，
# 让 `com.google.protobuf` / `com.google.gson` 这类必需依赖在缺失时**构建期就报错**。
#
# 必需（不可再豁免；缺失即构建失败）：
#   com.google.protobuf.* —— 小米 mio_sdk_base 的生成类父类链（已显式引入 protobuf-java）
#   com.google.gson.*     —— 小米 HyUtils/MiCommplatform 等 15 个类（已显式引入 gson）
#   com.google.android.gms.ads.identifier.* —— 广告标识（渠道 SDK 反射调用）
-dontwarn com.google.android.gms.**
-dontwarn com.google.android.play.**
-dontwarn com.google.firebase.**
-dontwarn com.google.common.**
-dontwarn com.google.errorprone.**
-dontwarn com.google.j2objc.**
-dontwarn com.google.api.client.**
-keep class com.google.** { *;}

-dontwarn cn.leancloud.**
-keep class cn.leancloud.** { *;}

-keep public class android.net.http.SslError
-keep public class android.webkit.WebViewClient

-dontwarn android.webkit.WebView
-dontwarn android.net.http.SslError
-dontwarn android.webkit.WebViewClient

-dontwarn android.support.**

-dontwarn org.apache.**
-keep class org.apache.** { *;}

-dontwarn okhttp3.**
-keep class okhttp3.** { *;}
-keep interface okhttp3.** { *; }

-dontwarn okio.**
-keep class okio.** { *;}

-keepattributes *Annotation*

-keep public class * implements com.bumptech.glide.module.GlideModule
-keep class * extends com.bumptech.glide.module.AppGlideModule {
 <init>(...);
}
-keep public enum com.bumptech.glide.load.ImageHeaderParser$** {
  **[] $VALUES;
  public *;
}
-keep class com.bumptech.glide.load.data.ParcelFileDescriptorRewinder$InternalRewinder {
  *** rewind();
}

-dontwarn com.geetest.sdk.**
-keep class com.geetest.sdk.**{*;}

-keep class com.nearme.** { *; }
-dontwarn com.nearme.**


-keepattributes InnerClasses,Signature,Exceptions,Deprecated,*Annotation*
-dontwarn android.**
# 同文件上方 `-dontwarn com.google.**` 的收窄理由同样适用于此处（这是第二段来源的
# 规则块）：保留 -keep（只是别裁剪，无副作用），但**不再整包豁免警告**，让
# protobuf / gson 等必需依赖缺失时在构建期暴露。
-dontwarn com.google.android.gms.**
-dontwarn com.google.android.play.**
-dontwarn com.google.firebase.**
-dontwarn com.google.common.**
-dontwarn com.google.errorprone.**
-dontwarn com.google.j2objc.**
-dontwarn com.google.api.client.**
-keep class android.** {*;}
-keep class com.google.** {*;}
-keep class com.android.** {*;}
-dontwarn org.apache.**
-keep class org.apache.** { *; }
-keep class sun.misc.Unsafe { *; }
-keep class com.google.** {*;}
-keep public class android.arch.core.internal.FastSafeIterableMap
-keep public class android.arch.core.util.Function
-keep public class android.arch.lifecycle.Lifecycle
-keep public class android.arch.lifecycle.Observer
-keep public class android.arch.lifecycle.ReportFragment
-keep public class android.arch.lifecycle.ViewModel
-keep public class android.support.v4.app.Fragment
-keep public class android.support.annotation.AnimatorRes
-keep public class android.support.v4.app.ActivityCompat
-keep public class android.support.design.widget.CoordinatorLayout
-keep public class android.support.v4.app.AppLaunchChecker
-keep public class android.support.v4.app.BackStackState
#-libraryjars libs/alipaySdk.jar
-dontwarn com.alipay.**
-keep class com.alipay.** {*;}
-keep class com.ut.device.** {*;}
-keep class com.ta.utdid2.** {*;}
#-libraryjars libs/eventbus-3.jar
-keep class org.greenrobot.eventbus.** { *; }
-keep class de.greenrobot.event.** { *; }
-keep class de.greenrobot.dao.** {*;}
-keepclassmembers class ** {
public void onEvent*(**);
void onEvent*(**);
}
-keepclassmembers class ** {
@org.greenrobot.eventbus.Subscribe <methods>;
}
-keep enum org.greenrobot.eventbus.ThreadMode { *; }
# Only required if you use AsyncExecutor
-keepclassmembers class * extends org.greenrobot.eventbus.util.ThrowableFailureEvent {
<init>(java.lang.Throwable);
}
#-libraryjars libs/wechat.jar
-keep class com.tencent.** {*;}
#-libraryjars libs/glide.jar
-keep class com.bumptech.glide.** {*;}
-dontwarn com.xiaomi.**
-keep class com.xiaomi.** {*;}
-keep class com.mi.** {*;}
-keep class com.wali.** {*;}
-keep class cn.com.wali.** {*;}
-keep class miui.net.**{*;}
-keep class org.xiaomi.** {*;}

-keepattributes Exceptions,InnerClasses,Signature,Deprecated,SourceFile,LineNumberTable,LocalVariable*Table,*Annotation*,Synthetic,EnclosingMethod
-keepclasseswithmembers class * extends cn.gundam.sdk.shell.even.SDKEventReceiver
-keep class cn.uc.**{
    <methods>;
    <fields>;
}
-keep class cn.gundam.**{
    <methods>;
    <fields>;
}
-keep class android.**{
    <methods>;
    <fields>;
}
-keep class org.json.**{
    <methods>;
    <fields>;
}

-keep class com.bun.miitmdid.core.** {*;}

-keep public class * extends android.app.Activity
-keep public class * extends android.app.Application
-keep public class * extends android.app.Service
-keep public class * extends android.content.BroadcastReceiver
-keep public class * extends android.content.ContentProvider
-keep public class * extends android.app.backup.BackupAgentHelper
-keep public class * extends android.preference.Preference
-keep public class com.android.vending.licensing.ILicensingService

-keepclasseswithmembernames class * {
    native <methods>;
}

-keepclasseswithmembers class * {
    public <init>(android.content.Context, android.util.AttributeSet);
}

-keepclasseswithmembers class * {
    public <init>(android.content.Context, android.util.AttributeSet, int);
}


-keepclassmembers enum * {
    public static **[] values();
    public static ** valueOf(java.lang.String);
}

-keep class * implements android.os.Parcelable {
  public static final android.os.Parcelable$Creator *;
}

-keepattributes InnerClasses
-keep class com.tencent.bugly.**{*;}
-keep class com.tencent.stat.**{*;}
-keep class com.tencent.smtt.**{*;}
-keep class com.tencent.beacon.**{*;}
-keep class com.tencent.mm.**{*;}
-keep class com.tencent.apkupdate.**{*;}
-keep class com.tencent.tmassistantsdk.**{*;}
-keep class org.apache.http.** { * ;}
-keep class com.qq.jce.**{*;}
-keep class com.qq.taf.**{*;}
-keep class com.tencent.connect.**{*;}
-keep class com.tencent.map.**{*;}
-keep class com.tencent.open.**{*;}
-keep class com.tencent.qqconnect.**{*;}
-keep class com.tencent.mobileqq.**{*;}
-keep class com.tencent.tauth.**{*;}
-keep class com.tencent.feedback.**{*;}
-keep class common.**{*;}
-keep class exceptionupload.**{*;}
-keep class mqq.**{*;}
-keep class qimei.**{*;}
-keep class strategy.**{*;}
-keep class userinfo.**{*;}
-keep class com.pay.**{*;}
-keep class com.demon.plugin.**{*;}
-keep class com.tencent.midas.**{*;}
-keep class oicq.wlogin_sdk.**{*;}
-keep class com.tencent.ysdk.**{*;}
-keep public class * extends android.app.Service
-keep public class * extends android.content.BroadcastReceiver
-keep class com.tencent.android.tpush.** {* ;}
-keep class com.tencent.mid.** {* ;}
-keep class com.qq.taf.jce.** {*;}
-keep class com.tencent.a.**{*;}
-keep class com.tencent.wxop.**{*;}
-keep class com.jg.**{*;}
-keep class com.tencent.qqdownloader.pay.**{*;}
-keepclasseswithmembernames class * {
    native <methods>;
}

-keep public class com.tencent.safecloud.**{*;}
-keep class com.bun.miitmdid.** {*;}
-keep class com.tencent.pipe.** {*;}

-dontwarn java.nio.file.Files
-dontwarn java.nio.file.Path
-dontwarn java.nio.file.OpenOption
-dontwarn org.codehaus.mojo.animal_sniffer.IgnoreJRERequirement

# 设备指纹SDK
-keep public class com.tencent.safecloud.**{*;}
-keep class com.tencent.tgpa.** {*;}
-keep public class qfc.**{*;}

#HttpDNS相关
-keep class com.tencent.msdk.dns.**{*;}

# 防沉迷接口
-keep class com.tencent.ysdk.module.AntiAddiction.impl.AntiAddictModule {public static *;}
-keep interface com.tencent.ysdk.module.AntiAddiction.listener.AntiAddictListener {*;}
-keep interface com.tencent.ysdk.module.AntiAddiction.listener.LoginAntiAddictCallBack {*;}
-keep class com.tencent.ysdk.module.AntiAddiction.model.AntiAddictRet {*;}
-keep class com.tencent.ysdk.module.AntiAddiction.AntiAddictionApi {*;}
-keep interface com.tencent.ysdk.module.AntiAddiction.AntiAddictionInterface {*;}
-keep interface com.tencent.ysdk.module.AntiAddiction.listener.AntiRegisterWindowCloseListener {*;}
-keep interface com.tencent.ysdk.module.AntiAddiction.listener.QueryCertificationCallback {*;}
-keep class com.tencent.ysdk.module.AntiAddiction.model.CertificationRect {*;}

# 注意（2026-10-21 审计 P3-14）：此处原为参考项目整段拷来的 `-ignorewarnings`。它让 R8
# 忽略**所有**警告，包括「缺失类」，正好抵消本文件上方（59-76 / 123-132）刻意收窄
# `-dontwarn com.google.**` 的目的：构建通过、运行期 NoClassDefFoundError（小米「添加
# 账号」闪退的真因）。移除后，必需依赖一旦缺失就在构建期失败；渠道 SDK 内可选的第三方
# 包各自已有逐包 -dontwarn 覆盖，不需要这行。
#
# 2026-10-21 实测（移除 -ignorewarnings 后的首次 release 构建）：R8 报 27 个「缺失类」，
# 全部是厂商 SDK 引用了「只在对应 ROM 上才存在的系统 API」或可选组件，编译期必然缺失：
#   com.huawei.android.* / com.huawei.system.BuildEx / com.huawei.libcore.io.* → EMUI 系统 API
#   com.huawei.hianalytics.* / appgallery.log / secure.* / ohos.localability.*  → HMS 可选组件
#   org.bouncycastle.crypto.*                                                  → 可选 BC Provider
# 这些属于「预期缺类」，用逐包 -dontwarn 精确声明；com.google.protobuf / com.google.gson
# 这类真依赖不在其中，仍由构建期强制检查兜住（缺了就直接失败）。
-dontwarn com.huawei.android.**
-dontwarn com.huawei.appgallery.**
-dontwarn com.huawei.hianalytics.**
-dontwarn com.huawei.libcore.**
-dontwarn com.huawei.ohos.**
-dontwarn com.huawei.secure.**
-dontwarn com.huawei.system.**
-dontwarn org.bouncycastle.crypto.**
-keepattributes *Annotation*
-keepattributes Exceptions
-keepattributes InnerClasses
-keepattributes Signature
-keepattributes SourceFile,LineNumberTable
-keep class com.huawei.hianalytics.**{*;}
-keep class com.huawei.updatesdk.**{*;}
-keep class com.huawei.hms.**{*;}
-keep interface com.huawei.hms.analytics.type.HAEventType{*;}
-keep interface com.huawei.hms.analytics.type.HAParamType{*;}
-keep class com.huawei.hms.analytics.HiAnalyticsInstance{*;}
-keep class com.huawei.hms.analytics.HiAnalytics{*;}

-keep class repeackage.com.uodis.opendevice.aidl.** {*; }
-keep interface repeackage.com.uodis.opendevice.aidl.* {*; }
-keep class repeackage.com.asus.msa.SupplementaryDID.* {*; }
-keep interface repeackage.com.asus.msa.SupplementaryDID.* {* ; }
-keep class repeackage.com.bun.lib.* { *; }
-keep interface repeackage.com.bun.lib.* {* ; }
-keep class repeackage.com.heytap.openid.* { *; }
-keep interface repeackage.com.heytap.openid.* { *; }
-keep class repeackage.com.samsung.android.deviceidservice.* { *; }
-keep interface repeackage.com.samsung.android.deviceidservice.* { *; }
-keep class repeackage.com.zui.deviceidservice.* {* ; }
-keep interface repeackage.com.zui.deviceidservice.* { *; }
-keep class repeackage.com.coolpad.deviceidsupport.* { *; }
-keep interface repeackage.com.coolpad.deviceidsupport.* {* ; }
-keep class repeackage.com.android.creator.* { *; }
-keep interface repeackage.com.android.creator.* { *; }
-keep class repeackage.com.google.android.gms.ads.identifier.internal.* {* ; }
-keep interface repeackage.com.google.android.gms.ads.identifier.internal.* { *; }
-keep class repeackage.com.oplus.stdid.* {*; }
-keep interface repeackage.com.oplus.stdid.** {*; }
-keep class com.huawei.hms.ads.** {*; }
-keep interface com.huawei.hms.ads.** {*; }
-keep class com.hihonor.ads.** {*; }
-keep interface com.hihonor.ads.** {*; }
-keep class repeackage.com.qiku.id.** { *; }
-keep interface repeackage.com.qiku.id.* { *; }

# === 本工程插件类：反射调用，必须保留原名与全部成员 ===
# ChannelLoginPlugin 用 Class.forName("...HuaweiLogin") + getMethod 反射调用，
# R8 看不到字符串引用会改名（实测被改成 z2.m）并裁剪方法，导致
# "华为渠道尚未启用（缺少 AGC 配置或 HMS 依赖）：init [class android.app.Activity, class z2.j]"。
# HuaweiLoginSink 作为反射方法的形参类型同样不能改名。
-keep class com.taumata.scanner.plugins.HuaweiLogin { *; }
-keep interface com.taumata.scanner.plugins.HuaweiLoginSink { *; }
-keep class com.taumata.scanner.plugins.ChannelLoginPlugin { *; }
-keep class com.taumata.scanner.plugins.ChannelLoginArgs { *; }
-keep class com.taumata.scanner.plugins.EmptyArgs { *; }