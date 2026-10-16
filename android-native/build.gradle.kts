// 根构建文件：只声明插件版本（apply false），各模块自己 apply。
// AGP 9 自带 built-in Kotlin，Android 模块不要再 apply org.jetbrains.kotlin.android。
plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.android.library) apply false
    alias(libs.plugins.kotlin.jvm) apply false          // :model 用；同时把 KGP 钉到 libs.versions.kotlin
    alias(libs.plugins.kotlin.compose) apply false
    alias(libs.plugins.roborazzi) apply false
}
