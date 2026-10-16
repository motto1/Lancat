// :sticker —— D 组：ThorVG（Lottie + CPU 渲染）JNI、帧缓存、StickerImage、表情目录
plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "fan.lancat.sticker"
    compileSdk = libs.versions.compileSdk.get().toInt()
    ndkVersion = libs.versions.ndk.get()

    defaultConfig {
        minSdk = libs.versions.minSdk.get().toInt()
        consumerProguardFiles("consumer-rules.pro")
        ndk {
            abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64")
        }
        externalNativeBuild {
            cmake {
                // 默认 ThorVG 1.1.0（与桌面 thorvg-sys 0.3.2+thorvg-1.1.0 一致）；换版本：-PlancatThorvg=1.1.3（需把源码放进 third_party）
                arguments += listOf(
                    "-DANDROID_STL=c++_static",
                    "-DLC_THORVG_VERSION=" + (project.findProperty("lancatThorvg") ?: "1.1.0"),
                )
                cppFlags += listOf("-std=c++14")
            }
        }
    }

    externalNativeBuild {
        cmake {
            path = file("src/main/cpp/CMakeLists.txt")
            version = "3.22.1"
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures { compose = true }

    // assets/emoji/*.z 已经是 deflate level 9，打包时不再二次压缩（运行时直接读，省一次解压）
    androidResources {
        noCompress += "z"
    }

    testOptions {
        unitTests.isReturnDefaultValues = true
    }
}

dependencies {
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.graphics)
    implementation(libs.compose.foundation)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.kotlinx.coroutines.android)

    testImplementation(libs.junit)
}
