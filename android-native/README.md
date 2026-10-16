# Lancat 原生安卓版（v2）

Kotlin + Jetpack Compose 界面，网络 / 发现 / 传输 / 断点续传全部复用桌面同一份 Rust 核心
（`crates/lancat-core`，经 `crates/lancat-ffi` 的 UniFFI 绑定），与桌面 Lancat v0.2.8（Windows / Linux）同一协议互通。

| 模块 | 内容 |
|---|---|
| `:model` | 纯 Kotlin 契约：`LancatRepository`、`Peer` / `Message` / `TransferState` … |
| `:core` | UniFFI 生成的绑定（`fan.lancat.core.ffi`）+ `liblancat_ffi.so` + `LancatCoreRepository`（契约的真实实现） |
| `:sticker` | ThorVG 1.1.0（Lottie + CPU 渲染，与桌面相同版本）JNI、帧缓存、`StickerImage`、116 个 Noto 动态表情 |
| `:ui` | Compose 主题 / 组件 / 页面：设备列表、聊天、表情面板、设置、分享选择页；自带导航栈（侧滑 / 预测性返回） |
| `:app` | 单 Activity、前台服务、系统分享（含 Direct Share）、通知（消息 / 传输进度）、快捷方式、快速设置 Tile、组播锁、网络变化 |

## 构建

```sh
# 1. Rust 核心（每次改了 lancat-core / lancat-ffi 才需要）
cd crates/lancat-ffi
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 --platform 26 \
  -o ../../android-native/core/src/main/jniLibs build --release --lib
# 2. APK（app/build/outputs/apk/release/app-release.apk，测试密钥签名）
cd ../../android-native && ./gradlew :app:assembleRelease
```

需要 JDK 17+（用 21 测试）、Android SDK platform 37、NDK 27.2.12479018、CMake 3.22.1、Rust 的三个 android target 和 cargo-ndk。

API 变了要重新生成绑定（用主机 debug 库，release .so 是 strip 过的）：

```sh
cargo build --lib && cargo run --features bindgen --bin uniffi-bindgen -- generate \
  --library <target>/debug/liblancat_ffi.{so,dll} --language kotlin --no-format \
  --out-dir ../../android-native/core/src/main/kotlin
```

## 测试

- `./gradlew :model:test :sticker:testDebugUnitTest :ui:testDebugUnitTest :app:testDebugUnitTest`
- 仓库 ↔ 核心真机外互通（Robolectric 里跑真实 `LancatCoreRepository` + 主机版 liblancat_ffi 对 `lancat-cli serve`，只走 127.0.0.1）：
  `./gradlew :core:testDebugUnitTest -Plancat.ffiHostDir=<含 lancat_ffi.dll/.so 的目录> -Plancat.cli=<lancat-cli>`
- Rust 侧：`crates/lancat-ffi/tests/smoke_cli.rs`（对 lancat-cli）、`tests/desktop_interop.rs`（对正在运行的桌面版，见文件头）

## 许可

整个 App 链接 GPL-3.0 的 lancat-core，按 GPL-3.0 分发。第三方：ThorVG（MIT）、Noto Animated Emoji（CC BY 4.0）、
JNA（Apache-2.0 / LGPL-2.1 双许可，按 Apache-2.0 使用）、AndroidX / Kotlin（Apache-2.0）。没有使用 Telegram 的代码。
