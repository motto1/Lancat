// :core —— UniFFI 绑定（src/main/kotlin/fan/lancat/core/ffi，自动生成）+ cargo-ndk 构建的 liblancat_ffi.so + LancatCoreRepository
//
// .so 不进 git：本地 / CI 先在 Lancat/crates/lancat-ffi 下运行
//   cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 --platform 26 -o <android2>/core/src/main/jniLibs build --release --lib
// 绑定重新生成（API 变了才需要）：
//   cargo run --features bindgen --bin uniffi-bindgen -- generate --library <jniLibs>/arm64-v8a/liblancat_ffi.so \
//     --language kotlin --no-format --out-dir <android2>/core/src/main/kotlin
plugins {
    alias(libs.plugins.android.library)
}

android {
    namespace = "fan.lancat.core"
    compileSdk = libs.versions.compileSdk.get().toInt()
    ndkVersion = libs.versions.ndk.get()
    defaultConfig {
        minSdk = libs.versions.minSdk.get().toInt()
        consumerProguardFiles("consumer-rules.pro")
    }
    testOptions {
        unitTests {
            isReturnDefaultValues = true
            all {
                // 主机互通测试：-Plancat.ffiHostDir=<含 lancat_ffi.dll/.so 的目录> -Plancat.cli=<lancat-cli 可执行文件>；不给就跳过
                (findProperty("lancat.ffiHostDir") as String?)?.let { d -> it.systemProperty("jna.library.path", d) }
                (findProperty("lancat.cli") as String?)?.let { c -> it.systemProperty("lancat.cli", c) }
                it.systemProperty("user.timezone", "Asia/Shanghai")
                it.jvmArgs("--add-opens=java.base/jdk.internal.access=ALL-UNNAMED", "--add-opens=java.base/java.io=ALL-UNNAMED")
            }
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    api(project(":model"))
    implementation(libs.kotlinx.coroutines.android)
    // UniFFI 的 Kotlin 绑定经 JNA 调 .so；Android 要 aar（带各 ABI 的 libjnidispatch.so）
    implementation("androidx.annotation:annotation:1.9.1")   // 生成的绑定用 @RequiresApi / @Keep
    implementation("${libs.jna.get().module}:${libs.versions.jna.get()}@aar")

    testImplementation(libs.junit)
    testImplementation(libs.robolectric)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation("androidx.test:core-ktx:1.7.0")
    testImplementation(libs.jna)   // 桌面版 jar：带 Windows/Linux 的 jnidispatch，Robolectric 里加载主机 lancat_ffi
}
