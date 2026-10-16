// Lancat Android v2 根工程（A 组维护；其他组改依赖请写 work/android/CONTRACT-CHANGES.md）
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.PREFER_SETTINGS)
    repositories {
        // Google Maven 只放 androidx / com.android / com.google 的东西；其余直接走 Maven Central（少一次握手，也少一个失败点）
        google {
            content {
                includeGroupByRegex("androidx\\..*")
                includeGroupByRegex("com\\.android(\\..*)?")
                includeGroupByRegex("com\\.google\\..*")
            }
        }
        mavenCentral()
    }
}
rootProject.name = "lancat-android2"
include(":core", ":model", ":sticker", ":ui", ":app")
