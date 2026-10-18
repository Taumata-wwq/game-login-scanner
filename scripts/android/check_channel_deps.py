#!/usr/bin/env python3
"""渠道 SDK 依赖完整性检查（构建期把关，把运行期崩溃提前到构建期）。

## 为什么需要它

2026-10-18 真机踩到的坑：小米渠道「添加账号」直接闪退，栈是
    NoClassDefFoundError: org.xiaomi.gamecenter.milink.msg.NoticeConfigProtos$SdkType
    Caused by: ClassNotFoundException: com.google.protobuf.GeneratedMessageV3
根因是该 jar 自带**生成的** protobuf 类却不带 protobuf 运行时，而
`channel-proguard-rules.pro` 里的 `-dontwarn com.google.**` 让 R8 放过了缺失父类——
**构建成功、运行期才炸**。这类问题靠构建日志完全看不出来（当时是先崩两次、再逐个补库
才发现缺了 protobuf 和 gson）。

本脚本把这一步前置：扫描渠道 SDK 的每个 jar，列出它引用、但**本工程依赖集里没有**
的第三方包根（排除 Android/AndroidX/Kotlin/自身/已显式引入的），有缺失就非零退出。

## 用法

    python scripts/android/check_channel_deps.py            # 检查
    python scripts/android/check_channel_deps.py --verbose   # 打印全部引用统计

依赖集来源：`src-tauri/gen/android/app/build.gradle.kts` 的 dependencies 块里声明的
坐标（maven 坐标 → 包根映射见 KNOWN_PROVIDERS）；以及渠道 SDK 自带的 libs/*.jar。
"""
import argparse
import os
import re
import sys
import zipfile

# CONSTANT_Class 里存的是**内部名**（`com/foo/Bar`），不带 `L…;` 描述符外壳；
# 只有数组类型才写成 `[Lcom/foo/Bar;`。早先按描述符形态匹配，导致一个都没命中、
# 检查恒为"通过"（负向测试两次都没报出来就是这个原因）。
#
# ⚠️ 捕获组必须是**整条内部名**：早先写成 `([A-Za-z0-9_$]*)(?:/...)` 只捕获到第一段，
# 于是包根退化成 "java" / "com" / "org" 这种单段名（实测误报 10 项）。
CLS_NAME = re.compile(rb"^([A-Za-z0-9_$]+(?:/[A-Za-z0-9_$]+)+)$")
CLS_ARRAY = re.compile(rb"^\[+L([A-Za-z0-9_$/]+);$")

# 平台 / 框架 / 自身实现，无需外部 jar 提供。
PLATFORM_PREFIXES = (
    "java/", "javax/", "android/", "androidx/", "kotlin/", "kotlinx/",
    "dalvik/", "sun/", "org/w3c/", "org/xml/", "org/json/", "org/intellij/",
    "org/jetbrains/", "org/greenrobot/",  # eventbus 由 mi_sdk/libs 自带，下方会再确认
    "com/taumata/", "org/xiaomi/", "com/xiaomi/",
)

# maven 坐标 → 它提供哪些包根。只列渠道 SDK 真正会用到的运行时库。
KNOWN_PROVIDERS = {
    "com.google.protobuf:protobuf-java": ("com/google/protobuf/",),
    "com.google.code.gson:gson": ("com/google/gson/",),
    "com.google.android.material:material": ("com/google/android/material/",),
    "com.huawei.hms:hwid": ("com/huawei/hms/", "com/huawei/agconnect/"),
    "com.huawei.hms:game": ("com/huawei/hms/", "com/huawei/agconnect/"),
    "androidx.appcompat:appcompat": ("androidx/",),
    "androidx.webkit:webkit": ("androidx/",),
    "androidx.activity:activity-ktx": ("androidx/",),
    "androidx.lifecycle:lifecycle-process": ("androidx/",),
}

# 已知「可选/反射才用」的包根：渠道 SDK 会在缺少时自行降级，允许缺席。
OPTIONAL_PREFIXES = (
    "com/google/android/gms/",      # 广告标识（OAID/GAID），反射调用
    "com/google/firebase/",
    "com/google/common/",            # Guava（部分渠道 SDK 内部可选）
    "com/google/errorprone/",
    "com/google/j2objc/",
    "cn/leancloud/",                 # 参考项目用到的推送，本工程未引入
    "com/tencent/bugly/",
    "io/reactivex/",
    "retrofit2/",
    "okhttp3/", "okio/",
    "com/bumptech/glide/",
    "com/squareup/",
    "org/apache/http/",
    "com/tencent/ysdk/",
    "com/nearme/",                   # OPPO
    "com/qihoo/",
    "com/meizu/",
    "do/do/",                        # 参考项目混淆残留
    "com/android/apksig/",
    "com/mi/milink/sdk/session/",    # 小米 milink 内部可选会话类
    # 小米 SDK 内置的扫码/条码解析（仅其自有扫码页用；本应用用自己的解码链路）。
    "com/google/zxing/",
    # OAID 供应商实现，反射加载，缺失时 SDK 会降级到其它供应商。
    "com/bun/supplier/",
    "com/bun/miitmdid/",
    # 广告标识读取（GAID），反射调用。
    "com/google/goyt/",
    "com/google/android/gms/",
    # XML 拉取解析：Android 平台自带 org.xmlpull，vivo 的 union sdk 把它当编译期
    # 依赖写进常量池；运行期由系统提供，无需外部 jar。
    "org/xmlpull/",
    # 小米 SDK 内嵌的第三方支付（payeco）收银台，仅其自有支付路径使用，本应用不触发。
    "com/payeco/",
)


def _uleb(data, i):
    """读 ULEB128，返回 (value, next_index)。"""
    r = 0
    s = 0
    while True:
        b = data[i]
        i += 1
        r |= (b & 0x7F) << s
        if not (b & 0x80):
            return r, i
        s += 7


def class_refs(data):
    """解析单个 .class 的常量池，返回它引用的全部类型描述符包名。

    直接解析常量池（CONSTANT_Class=7 → 指向 CONSTANT_Utf8 的描述符），
    比在整块字节里正则扫**准确得多**：后者无法区分「一个完整描述符常量」与
    「相邻常量的字节拼接」，会误报 `Lcom/...`、`Ljava/lang/Object` 之类。
    只取 CONSTANT_Class：那是真正的类型引用（方法签名里的类型已由各自 Class 项覆盖）。
    """
    if len(data) < 10 or data[:4] != b"\xca\xfe\xba\xbe":
        return set()
    count = int.from_bytes(data[8:10], "big")
    i = 10
    utf8 = {}       # index -> bytes
    class_idx = []  # CONSTANT_Class 指向的 Utf8 索引
    idx = 1
    while idx < count:
        tag = data[i]
        i += 1
        if tag == 1:  # Utf8
            ln = int.from_bytes(data[i:i + 2], "big")
            i += 2
            utf8[idx] = data[i:i + ln]
            i += ln
        elif tag == 7:  # Class
            class_idx.append(int.from_bytes(data[i:i + 2], "big"))
            i += 2
        elif tag in (8, 16, 19, 20):  # String / MethodType / Module / Package
            i += 2
        elif tag in (15,):  # MethodHandle
            i += 3
        elif tag in (3, 4, 9, 10, 11, 12, 17, 18):  # int/float/ref/nameandtype/dynamic
            i += 4
        elif tag in (5, 6):  # long/double 占两个槽位
            i += 8
            idx += 1
        else:
            # 未知 tag：无法安全继续，放弃该文件（宁可漏报也不误报）
            return set()
        idx += 1
    out = set()
    for ci in class_idx:
        raw = utf8.get(ci)
        if raw is None:
            continue
        # 普通内部名，或数组形态 `[Lcom/foo/Bar;`。
        m = CLS_NAME.match(raw) or CLS_ARRAY.match(raw)
        if m:
            out.add(m.group(1).decode("ascii"))
    return out


def jar_classes(path):
    """返回 (definitions, references)。

    **必须区分两者**：definitions 是本 jar 真正提供的类（.class 路径），
    references 是它引用到的全部类型（含外部依赖）。早先只用一份集合做「是否已
    提供」的判定，结果把 *被引用* 的 protobuf 也算成「已提供」，缺依赖永远测不出来
    （负向测试当场暴露）。
    """
    refs = set()
    defs = set()
    try:
        with zipfile.ZipFile(path) as z:
            for n in z.namelist():
                if not n.endswith(".class"):
                    continue
                defs.add(n[: -len(".class")])
                refs |= class_refs(z.read(n))
    except Exception as e:  # 损坏/非 zip
        print(f"  ! 无法读取 {path}: {e}", file=sys.stderr)
    return defs, refs


def jar_descs(path):
    """兼容旧调用：只关心引用集。"""
    return jar_classes(path)[1]


def gradle_provided_roots(app_gradle):
    """从 build.gradle.kts 的 dependencies 块里读出的坐标 → 其提供的包根。"""
    roots = set()
    if not os.path.exists(app_gradle):
        return roots
    txt = open(app_gradle, encoding="utf-8").read()
    for m in re.finditer(r'implementation\(\s*"([^"]+)"\s*\)', txt):
        coord = m.group(1)
        for known, prefixes in KNOWN_PROVIDERS.items():
            # 只比对 group:artifact（忽略版本）
            base = coord.rsplit(":", 1)[0] if coord.count(":") >= 2 else coord
            if base == known:
                roots.update(prefixes)
    return roots


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--verbose", action="store_true", help="打印所有引用的包根统计")
    ap.add_argument("--root", default=None, help="仓库根（默认为脚本上两级）")
    args = ap.parse_args()

    root = args.root or os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
    mi_libs = os.path.join(root, "vendor", "mi_sdk", "libs")
    chan_libs = os.path.join(root, "vendor", "channel_sdk")
    app_gradle = os.path.join(
        root, "src-tauri", "gen", "android", "app", "build.gradle.kts"
    )

    jars = []
    for d in (mi_libs, chan_libs):
        if os.path.isdir(d):
            jars += [
                os.path.join(d, f)
                for f in os.listdir(d)
                if f.lower().endswith((".jar", ".aar"))
            ]

    if not jars:
        print("! 未找到渠道 SDK jar（vendor/mi_sdk/libs 或 vendor/channel_sdk 为空）")
        print("  渠道登录将不可用；如需渠道功能，请放入 SDK 后重跑。")
        return 0  # 缺 SDK 不是"依赖不完整"，不阻断构建

    provided = gradle_provided_roots(app_gradle)
    # 渠道 SDK 自带 jar 之间互相提供（如 eventbus / onetrack 就在同一 libs 目录）
    for j in jars:
        if j.lower().endswith(".jar"):
            provided.add("__self__")  # 占位：同目录 jar 视为已提供，见下

    print(f"== 渠道 SDK 依赖完整性检查 ==")
    print(f"  扫描 jar: {len(jars)} 个")
    print(f"  gradle 显式提供的包根: {len(provided)} 个")
    print(f"  build.gradle: {app_gradle}")

    # 区分「各 jar 定义的类」与「它们引用的类」：前者用于判定互相提供的依赖，
    # 后者才是需要找提供方的外部引用。
    own_defs = set()
    per_jar = {}
    for j in jars:
        defs, refs = jar_classes(j)
        per_jar[j] = refs
        own_defs |= defs

    # eventbus / onetrack 由 mi_sdk/libs 自带，视为已提供
    bundled_roots = (
        "org/greenrobot/eventbus/",
        "com/xiaomi/onetrack/",
    )

    missing = {}
    all_roots = {}
    for j, descs in per_jar.items():
        for d in descs:
            if d.startswith(PLATFORM_PREFIXES):
                continue
            if d.startswith(OPTIONAL_PREFIXES):
                continue
            if d in own_defs:  # 由渠道 SDK 自带的某个 jar 真正定义
                continue
            if any(d.startswith(p) for p in provided if p != "__self__"):
                continue
            if d.startswith(bundled_roots):
                continue
            parts = d.split("/")
            root_pkg = "/".join(parts[:3]) if len(parts) >= 3 else "/".join(parts[:2])
            all_roots[root_pkg] = all_roots.get(root_pkg, 0) + 1
            missing.setdefault(root_pkg, set()).add(os.path.basename(j))

    if args.verbose:
        print("\n-- 全部第三方包根（前 40）--")
        for r, c in sorted(all_roots.items(), key=lambda kv: -kv[1])[:40]:
            print(f"   {r:<44} refs={c}")

    if not missing:
        print("\n[OK] 未发现缺失的必需依赖（渠道 SDK 的外部引用都有提供方）。")
        return 0

    print(f"\n[FAIL] 发现 {len(missing)} 个**未提供**的包根——运行期很可能")
    print("       NoClassDefFoundError（构建期看不出来，正是当初小米闪退的成因）：")
    for r in sorted(missing, key=lambda k: -all_roots[k]):
        jars = ", ".join(sorted(missing[r]))
        print(f"   {r:<44} refs={all_roots[r]:<5} 来自: {jars}")
    print("\n  处理方式：在 patch-gen.ps1 的 mi_sdk 段补上对应 implementation(...)，")
    print("  或若确属「反射才用、可缺席」的包，加入本脚本的 OPTIONAL_PREFIXES。")
    return 1


if __name__ == "__main__":
    sys.exit(main())
