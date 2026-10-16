// :app —— C 组（system）。Application、单 Activity、前台服务、分享入口、通知、快捷方式、Tile、权限。
// 手写 ServiceLocator 把 :core 的 LancatCoreRepository 注入 :ui，不用 Hilt。
//
// :core / :ui 未就绪时用 src/stub 里的占位实现编译（包 fan.lancat.stub，签名与契约一致）。
// 集成时 gradle.properties 设 lancat.useStubs=false（或命令行 -Plancat.useStubs=false），
// 改用 src/real/kotlin/fan/lancat/Bridge.kt（import fan.lancat.core.LancatCoreRepository / fan.lancat.ui.*）。
plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

val useStubs: Boolean = (findProperty("lancat.useStubs") as String?)?.toBoolean() ?: true

// 版本号：云端按标签注入（v0.2.9 → LANCAT_VERSION=0.2.9），本地用默认值。
// versionCode = (x·10000 + y·100 + z)·100 + 安卓修订号（-android.N），与 :model 的 AppVersion.versionCode 同一公式，
// 随版本单调递增（旧 Slint 版 0.2.7 = 207、Compose 首版 0.2.8 = 208，都比新公式小，可直接覆盖安装）。
val lancatVersion = System.getenv("LANCAT_VERSION")?.takeIf { it.isNotBlank() } ?: "0.2.8-android.1"
fun versionCodeOf(v: String): Int {
    val m = Regex("""^v?(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-android\.?(\d+))?$""").find(v.trim())
        ?: error("LANCAT_VERSION '$v' 不是 x.y.z 或 x.y.z-android.N")
    val (a, b, c, r) = m.destructured
    return ((a.toInt() * 10000 + (b.toIntOrNull() ?: 0) * 100 + (c.toIntOrNull() ?: 0)) * 100) + (r.toIntOrNull() ?: 0)
}
val lancatCode = System.getenv("LANCAT_VERSION_CODE")?.takeIf { it.isNotBlank() }?.toInt() ?: versionCodeOf(lancatVersion)

android {
    // 命名空间按契约用 fan.lancat；applicationId 沿用旧工程（Slint 版与 Tauri mobile 都是 app.lancat），
    // 配合同一把签名密钥，旧版用户直接覆盖升级，桌面图标位置和通知设置都保留
    namespace = "fan.lancat"
    compileSdk = libs.versions.compileSdk.get().toInt()
    ndkVersion = libs.versions.ndk.get()

    defaultConfig {
        applicationId = "app.lancat"
        minSdk = libs.versions.minSdk.get().toInt()
        targetSdk = libs.versions.targetSdk.get().toInt()
        versionCode = lancatCode
        versionName = lancatVersion
        ndk { abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64") }
    }

    // 与旧工程同一把测试密钥（口令公开，见旧工程 app/build.gradle），签名一致才能覆盖安装
    signingConfigs {
        create("lancat") {
            storeFile = file("lancat-test.p12")
            storeType = "pkcs12"
            storePassword = "lancat-android"
            keyAlias = "lancat"
            keyPassword = "lancat-android"
        }
    }

    buildTypes {
        debug { signingConfig = signingConfigs.getByName("lancat") }
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("lancat")
        }
    }

    sourceSets {
        getByName("main") {
            kotlin.srcDir(if (useStubs) "src/stub/kotlin" else "src/real/kotlin")
        }
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    lint {
        abortOnError = false
        checkReleaseBuilds = false
        lintConfig = file("lint.xml")
        htmlReport = true
        xmlReport = true
    }
    testOptions {
        unitTests {
            isIncludeAndroidResources = true
            all {
                it.systemProperty("user.timezone", "Asia/Shanghai")
                it.maxHeapSize = "2g"
                // Robolectric 4.17 的 FileDescriptor 拦截器在 JDK 21 上要这个
                it.jvmArgs("--add-opens=java.base/jdk.internal.access=ALL-UNNAMED", "--add-opens=java.base/java.io=ALL-UNNAMED")
            }
        }
    }
}

dependencies {
    implementation(project(":model"))
    if (!useStubs) {
        implementation(project(":core"))
        implementation(project(":ui"))
    }

    implementation(libs.androidx.core.ktx)              // 1.19.1：NotificationCompat.ProgressStyle / setRequestPromotedOngoing / setShortCriticalText
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.kotlinx.coroutines.android)
    implementation("androidx.sharetarget:sharetarget:1.2.0")   // API 26–28 Direct Share 兼容层（ChooserTargetService）；已登记 CONTRACT-CHANGES.md

    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.foundation)
    implementation(libs.compose.material3)

    testImplementation(libs.junit)
    testImplementation(libs.robolectric)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation("androidx.test:core-ktx:1.7.0")
    testImplementation("androidx.test.ext:junit-ktx:1.3.0")
}
