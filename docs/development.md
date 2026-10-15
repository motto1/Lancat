# 开发、构建与发布

> 产品介绍见 [README](../README.md)。

## 构建

公共依赖：Rust ≥ 1.80。

### Slint 版（推荐）

```bash
cd crates/lancat-slint
cargo build --release                 # 正式构建（opt-level z + fat LTO，较慢）
cargo build --profile fastrel         # 快速优化构建（无 LTO），测量与日常调试用
```

`fastrel` 开了增量编译，Windows 上用 Rust 自带的 `rust-lld` 链接（见 `.cargo/config.toml`）。在 16 线程的笔记本上实测：从零编译约 4.5 分钟，改一行代码后重编约 10–13 秒（原来约 100 秒）。从零编译的大头是依赖库（Slint 编译器、字体库、ThorVG、windows 接口库）和 `lancat-slint` 本身（.slint 生成的代码，约 90 秒）。

产物：`crates/lancat-slint/target/release/lancat-slint`（Windows 为 `lancat-slint.exe`）。

`thorvg-sys` 从源码编译 ThorVG，`bindgen` 需要 **libclang** 和 C++ 编译器。

**Linux（Arch）**

```bash
sudo pacman -S --needed clang libxkbcommon wayland fontconfig
```

Debian / Ubuntu：`clang libclang-dev libxkbcommon-dev libwayland-dev libfontconfig-dev`。

- Wayland 下默认用自写 layer-shell 后端，需要合成器支持 `wlr-layer-shell`（niri / Sway / Hyprland / KDE 可以，GNOME 不行，可设 `LANCAT_SLINT_WL=0` 退回 winit）。
- 输入法走 `zwp_text_input_v3`（fcitx5 已验证）。
- 托盘走 StatusNotifierItem，需要桌面有 SNI 宿主（如 waybar 的 tray 模块）。

**Windows（10/11）**

```powershell
# Visual Studio 2022 Build Tools（“使用 C++ 的桌面开发”）
winget install Rustlang.Rustup; rustup default stable-msvc
winget install LLVM.LLVM
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
cd crates\lancat-slint
cargo build --release
```

小技巧：把 `CARGO_TARGET_DIR` 指到空间大的盘。通过 SSH 远程构建时，SSH 断开后子进程会被杀掉，长时间构建可放进一个临时的交互式计划任务里跑。

### 云端编译与发布

推送 `v*` 标签（如 `git tag v0.1.0 && git push origin v0.1.0`）后，GitHub Actions（`.github/workflows/release.yml`）会在云上编译 Windows 和 Linux 的 Slint 版（release），打包成 `Lancat-<版本>-windows-x64.zip` 和 `Lancat-<版本>-linux-x64.tar.gz`，发布到仓库的 Releases 页面。在 Actions 页面手动运行则只编译、不发布。

### Tauri 版（旧版）

需要 Node ≥ 20。

```bash
npm install
npm run tauri build -- --no-bundle            # 产物：target/release/lancat
npm run tauri build                            # 安装包（deb/rpm/AppImage、msi/nsis、dmg/app）
npm run tauri dev                              # 开发（Vite 热更新）
```

Linux 依赖：Arch `webkit2gtk-4.1 gtk3 libappindicator-gtk3 librsvg`；原生 Wayland 贴边需 `gtk-layer-shell` 并加 `--features layer-shell`。

## 运行与调试

直接运行二进制即可。同一台机器开第二个实例做测试：

```bash
# 独立数据目录、端口和名字（设置了 LANCAT_DATA_DIR 时不启用单实例锁）
LANCAT_DATA_DIR=/tmp/lc2 LANCAT_PORT=47821 LANCAT_NAME=第二台 ./lancat-slint

# 无界面节点
lancat-cli serve --port 47822 --name 测试机 --data /tmp/lc3
lancat-cli send --to 127.0.0.1:47820 --text 你好 --file ./some.zip
```

`lancat-cli` 由 `cargo build -p lancat-core --release` 生成于根目录 `target/release/`。

### 常用环境变量

| 变量 | 作用 |
|---|---|
| `LANCAT_DATA_DIR` | 数据目录（同时关闭单实例锁） |
| `LANCAT_PORT` / `LANCAT_UDP_PORT` | TCP / UDP 端口（默认 47820） |
| `LANCAT_NAME` | 设备名（默认主机名） |
| `LANCAT_RECV_DIR` | 接收目录 |
| `LANCAT_BIND_IP` | 只绑定某个地址 |
| `LANCAT_PROBE` | 额外单播探测目标，`IP:UDP端口,...` |
| `LANCAT_NO_MDNS=1` / `LANCAT_NO_UDP=1` | 关闭 mDNS / UDP 发现 |
| `LANCAT_PANEL_KEEP_ALIVE` | 面板收起后多少秒释放资源（0 = 常驻，默认 60） |
| `LANCAT_OPEN_ON_START=1` | 启动即弹出面板（截图/调试） |
| `LANCAT_SLINT_WL=0` | Linux：不用自写 layer-shell 后端，退回 winit |
| `LANCAT_SLINT_NODOCK=1` | 不创建停靠条 |
| `LANCAT_SLINT_THEME` | 强制主题 |
| `LANCAT_FIX_DRY_RUN=1` | 诊断「一键修复」只演示不执行 |
| `RUST_LOG=debug` | 详细日志 |

## 测试

```bash
cargo test -p lancat-core --release   # 单元测试 + 集成测试
scripts/cli-e2e.sh                    # 双进程：serve + send（10MB 文件、文件夹、文本），两个 serve 自动互相发现并发消息
```

集成测试覆盖：文本双向、表情、多块文件、重名、文件夹、空文件、黑名单、离线排队补发、暂停/取消、进度、发现、静默死链替换（`silently_dead_link_is_replaced_when_the_peer_dials_again`）、虚拟网段信任与网卡分类等。arch 与 Windows 上核心测试均全部通过（Windows 30 passed）。

实机验证（2026-10-14）：arch（Wayland/niri）与 Windows 11 通过 Tailscale 自动发现、双向建立连接。

## 更新

Slint 版内置更新（设置 →「更新」）：

- 当前版本显示在设置里；「检查更新」查询 GitHub Releases 的最新版本（`motto1/Lancat`）。
- 「自动检查更新」默认开：启动 30 秒后查一次，之后每 6 小时一次；有新版本时停靠条弹提醒（Windows 另有托盘气泡），点它直接跳到「更新」。
- 「立即更新」下载本平台的单文件（`lancat-slint-windows-x64.exe` / `lancat-slint-linux-x64`），用发布里的 `SHA256SUMS` 校验，替换程序文件后自动重启到新版。程序所在目录需要可写（不要放在 Program Files 之类的位置）。
- 网络请求用系统自带的 `curl`（Windows 10 1803 起自带），走系统代理与证书。

发布新版本：把 `Cargo.toml` 的版本号改好，推一个 `v*` 标签（如 `v0.2.1`）。云端编译会把标签里的版本号写进程序，并上传压缩包、单文件和 `SHA256SUMS`。

## 已知问题与待办

- 局域网 / 虚拟网内**传输未加密**，没有限速；公网连接走 https 隧道时是 wss 加密，直接走 frp 之类的裸 TCP 时不加密（应用层加密待做）。
- **网卡类型手动覆盖**只在底层（`Settings.iface_kinds`），设置页还没有界面；目前 arch 和 Windows 上自动识别都正确。
- 大于 `/24` 的三层虚拟网段不做扫描，需要靠虚拟网 CLI、gossip 或手动添加地址。
- 三台以上设备、跨多种虚拟网的完整场景尚未系统测试。
- Linux Slint 版：GNOME（mutter）不支持 layer-shell，只能退回 winit，贴边效果受限；没有系统级模糊。
- 深色主题下 Slint 版文字比旧版略细（软件渲染器无对比度增强）；多行段落逐行行距与旧版差 ≤1 px。
- macOS 尚无 Slint 版；旧版的 macOS 代码未在实机编译运行。
- 未做代码签名，Windows SmartScreen 可能拦截。
- 移动端（旧版有 Android 实验）尚未完成。
