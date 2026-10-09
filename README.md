# Lancat

局域网零配置、零确认的即时分享工具：文件、文件夹、文本、动态表情。默认是一条贴在屏幕边缘的 6px 隐藏条，鼠标悬停（或拖着文件靠近）就弹出聊天小窗，鼠标离开后带回弹动画收起。

> 完整产品计划见 [docs/PLAN.md](docs/PLAN.md)。

## 功能一览

| 模块 | 已实现 |
|---|---|
| 发现 | mDNS `_lancat._tcp.local.`（TXT：`id/name/os/ver`）＋ UDP 47820 每 5s 广播心跳兜底，15s 收不到判离线；跳过 Docker/VMware/WSL/veth 等虚拟网卡；设备 UUID 持久化，名称默认取主机名；可手动添加 `IP[:端口]`（每 5s 探测） |
| 传输 | TCP 47820 上的 axum：WebSocket 传 `text / emoji / typing / ack / file_offer`（每对设备一条长连接，id 小的一方拨号，断线自动重连，另有 `POST /msg` HTTP 兜底）；文件 `PUT /files/{tid}/{chunk}` 4MB 分块、4 块并发，断点续传（接收端 sidecar 记录已收块，重启后仍可续），每块和整文件 BLAKE3 校验 |
| 安全 | 只接受私有网段（RFC1918、链路本地、100.64/10、回环、IPv6 ULA/链路本地）；设备黑名单 + IP 黑名单；收到的文件去掉可执行位、永不自动打开或执行；相对路径清洗（防 `..`）；可设单文件大小上限 |
| 存储 | SQLite（rusqlite bundled）：设备、每设备一个会话（未读数、最后预览）、消息永久保存；离线时发出的消息排队，对方上线自动补发；一键清空 |
| 窗口 | `dock` 6px 透明置顶条（不进任务栏，可设边、位置、长度、显示器）；`panel` 约 360×520 弹窗 |
| 交互 | 悬停 150ms 弹出（spring：stiffness 300 / damping 24）；拖文件进入立即弹出；离开 800ms 回弹收起；输入框有焦点、表情面板/菜单打开、正在传输、已钉住时不收起；动画只用 transform/opacity |
| 会话 | 左侧头像栏（在线点、未读数、右键拉黑/清空/删除）、右侧独立会话、底部输入框/表情/附件；拖到头像发给该设备、拖到会话区发给当前会话、拖到顶部「全部」群发；Shift(或 Ctrl/⌘)+点击多选后发送 |
| 表情 | 内置 116 个 Noto Animated Emoji（Lottie，CC BY 4.0），lottie-web 渲染，只播放可见项、面板隐藏时全部冻结；二级玻璃表情面板：分类、最近使用、中英文搜索；单独一个表情显示为 160px 动态贴纸；导入 Telegram `.tgs`（gzip Lottie）或 Lottie `.json` 贴纸并发送 |
| 系统 | 托盘（打开/钉住/接收文件夹/设置/开机自启/退出，提示未读数）、单实例、开机自启（tauri-plugin-autostart）、设置窗口 |
| 视觉 | 系统字体栈（SF / Segoe UI Variable / PingFang 替代）、深浅色跟随系统或手动、强调色；Win11 Acrylic（Mica 兜底）、macOS NSVisualEffectView、Linux CSS 半透明降级 |

## 目录结构

```
crates/lancat-core/        纯 Rust 核心（无 GUI 依赖，可无头测试）
  src/discovery.rs           mDNS + UDP 心跳
  src/server.rs              axum 路由（/info /ws /msg /files /finish）+ 私网/黑名单守卫
  src/link.rs                每设备 WebSocket 长连接（握手、心跳、重连、分发）
  src/transfer.rs            发送端：分块、并发、重试、续传
  src/files.rs               接收端：IncomingTransfer、安全路径、重名、BLAKE3
  src/store.rs               SQLite：devices / conversations / messages / kv(settings)
  src/node.rs                Node 门面：对外 API、事件广播、离线判定、补发
  src/bin/lancat-cli.rs      无界面节点（serve / send），用于测试与脚本
  tests/two_nodes.rs         两实例互通集成测试
src-tauri/                 Tauri 2 外壳
  src/commands.rs            IPC 命令（消息、文件、设置、贴纸…）
  src/dock.rs                dock/panel 窗口、几何计算、光标轮询、弹出/收起
  src/tray.rs                托盘
  src/platform/{linux,windows,macos}.rs   #[cfg(target_os)] 隔离的平台代码
src/                       Svelte 5 + TypeScript 前端（按窗口 label 渲染 Dock / Panel / Settings）
  lib/spring.ts              spring 物理 → Web Animations 关键帧
  lib/lottie.ts              表情加载、播放控制、.tgs 解析（fflate）
  assets/emoji/              Noto Animated Emoji 子集 + LICENSE.md
scripts/cli-e2e.sh         双进程端到端测试（文本、10MB 文件、文件夹、mDNS/UDP 互相发现）
scripts/fetch-emoji.py     重新下载/压缩内置表情
scripts/preview/           无 Rust 的 UI 预览（mock Tauri API + 无头 Chrome 截图）
```

## 架构

```
┌──────────── WebView（Svelte）────────────┐
│ dock 隐藏条 │ panel 聊天窗 │ 表情面板 │ 设置 │
└───────────────┬──────────────────────────┘
   invoke(命令) │ ▲ emit("lancat://event", CoreEvent)
┌───────────────┴─┴────── Rust ────────────────┐
│ src-tauri：窗口/托盘/自启/单实例/平台材质        │
│ lancat-core::Node                              │
│  Discovery(mDNS+UDP) Transport(axum WS+HTTP)   │
│  Store(SQLite)  事件 broadcast                  │
└───────────────────────────────────────────────┘
```

- WebView 只负责显示；网络、文件读写和状态都在 Rust 里。
- 事件：`peer`、`peer_removed`、`message`、`message_update`、`typing`、`progress`、`history_cleared`、`settings`。
- 数据目录：Tauri `app_data_dir`（Linux `~/.local/share/app.lancat.desktop/`），内含 `lancat.db`、`stickers/`。接收目录默认是系统「下载」，传输中的分块在 `<接收目录>/.lancat-part/`。

### 协议摘要

| 方向 | 内容 |
|---|---|
| UDP 广播 47820 | `{"lancat":1,"device":{id,name,os,ver,port}}` 每 5s，每张物理网卡的广播地址 + 255.255.255.255 |
| `GET /ws` | 首帧双方互发 `{"type":"hello","device":…}`，之后 `text{id,text,ts}` / `emoji{id,emoji,ts}` / `file_offer` / `typing` / `ack{id}` / `ping` / `pong` |
| `POST /files/{tid}` | `FileManifest{transfer_id,msg_id,from,name,size,chunk_size,hash,ts}` → `{have:[已收块],complete}` |
| `PUT /files/{tid}/{i}` | 第 i 块原始字节，头 `x-lancat-chunk-hash: <blake3>` |
| `POST /finish/{tid}` | 整文件 BLAKE3 校验并移入接收目录（重名自动 `name (1).ext`）；不一致返回 409，发送端重传一次 |

所有请求带 `x-lancat-id: <设备 UUID>`。

## 构建

公共依赖：Rust ≥ 1.80、Node ≥ 20、npm。Tauri CLI 作为 devDependency（`@tauri-apps/cli`），无需 `cargo install`。

```bash
npm install
npm run tauri build -- --no-bundle            # 产物：target/release/lancat
npm run tauri build                            # 生成安装包（deb/rpm/AppImage、msi/nsis、dmg/app）
npm run tauri dev                              # 开发（Vite 热更新）
```

### Linux

- 依赖（Arch）：`webkit2gtk-4.1 gtk3 libappindicator-gtk3 librsvg`；（Debian/Ubuntu）：`libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev`。
- **X11 / XWayland（默认构建）**：贴边需要绝对定位和置顶，普通 Wayland 客户端做不到，所以检测到 Wayland 时自动以 `GDK_BACKEND=x11` 走 XWayland。
- **原生 Wayland（推荐 niri / Sway / Hyprland / KDE）**：用 wlr-layer-shell 贴边，需要系统装有 `gtk-layer-shell`：
  ```bash
  npm run tauri build -- --no-bundle --features layer-shell
  ```
  该构建在 Wayland 下自动使用 layer-shell（`LANCAT_X11=1` 可强制回到 XWayland）。GNOME(mutter) 不支持 layer-shell，请用默认构建。
- 防火墙（如启用 ufw/firewalld）：放行 TCP/UDP 47820 和 UDP 5353。

### macOS（13+）

```bash
xcode-select --install
npm install && npm run tauri build            # 生成 .app / .dmg
```
- 已开启 `macOSPrivateApi`（透明窗口必需），应用以 Accessory 方式运行（无 Dock 图标，只有菜单栏图标）。
- 分发需签名与公证（`APPLE_SIGNING_IDENTITY` 等环境变量，见 Tauri 文档）。
- 首次收发时系统会询问“允许接受传入连接”，选允许。

### Windows（11+）

```powershell
# Visual Studio 2022 Build Tools（“使用 C++ 的桌面开发”）+ WebView2（Win11 自带）
winget install Rustlang.Rustup; rustup default stable-msvc
npm install
npm run tauri build                            # 生成 msi / nsis 安装包
```
- 首次运行会弹一次 UAC，为 Lancat 添加入站防火墙规则（专用/域网络），只问一次。

### 两个实例同机互测

```bash
# 第二个实例：独立数据目录、端口和名字（设置了 LANCAT_DATA_DIR 时不启用单实例锁）
LANCAT_DATA_DIR=/tmp/lc2 LANCAT_PORT=47821 LANCAT_NAME=第二台 target/release/lancat
# 或者用无界面节点
target/release/lancat-cli serve --port 47822 --name 测试机 --data /tmp/lc3
target/release/lancat-cli send --to 127.0.0.1:47820 --text 你好 --file ./some.zip
```
其他环境变量：`LANCAT_RECV_DIR`、`LANCAT_UDP_PORT`、`LANCAT_NO_MDNS=1`、`LANCAT_NO_UDP=1`、`LANCAT_OPEN_ON_START=1`（启动即弹出面板，调试/截图用）、`RUST_LOG=debug`。

## 测试

```bash
cargo test -p lancat-core            # 单元测试 + 两实例集成测试（文本双向、表情、9MB 三块文件、重名、文件夹、空文件、黑名单、离线排队补发）
scripts/cli-e2e.sh                   # 双进程：serve + send（10MB 文件、文件夹、文本），以及两个 serve 通过 mDNS/UDP 自动互相发现并发消息
npm run check                        # svelte-check 类型检查
scripts/preview/shot.sh "window=panel&theme=dark" out.png   # 无头 Chrome 截图（mock 数据）
```

## Windows 构建机待办

1. 安装 VS 2022 Build Tools（C++ 桌面开发）、rustup（stable-msvc）、Node 20+。
2. `npm install && npm run tauri build`，修正 `src-tauri/src/platform/windows.rs` 可能的编译错误（`windows` crate 0.61 的函数签名，如 `ShellExecuteW` 的 `hwnd` 参数是否为 `Option<HWND>`、`DwmSetWindowAttribute` 指针类型）。
3. 验证：Acrylic 是否生效（失败会退到 Mica）；DWM 圆角；6px WebView2 条能否收到拖入事件，以及 `GetAsyncKeyState` 轮询兜底是否能在 OLE 拖拽时弹出面板；托盘左键切换面板。
4. 验证 UAC 防火墙规则只请求一次（kv 键 `win_firewall_rule`），以及拒绝后仍能主动发起连接。
5. 多显示器与 125%/150% 缩放下 dock/panel 的位置（`dock.rs::compute` 使用物理像素和 `work_area`）。
6. 代码签名（signtool / Azure Trusted Signing），否则 SmartScreen 会拦截；可选 updater。
7. 测量空闲内存（目标 < 50MB）和千兆网速（目标 ≥ 100MB/s）。

## TODO-verify 清单（只在 Linux 上验证过）

- `platform/windows.rs`：Acrylic/Mica、DWM 圆角、`GetAsyncKeyState` 拖拽感应、UAC `netsh` 防火墙规则 —— 均未在 Windows 上编译运行。
- `platform/macos.rs`：NSVisualEffectView（window-vibrancy）、`CGEventSourceButtonState` 拖拽感应、Accessory 激活策略 —— 未在 macOS 上编译运行。
- macOS 26 的 NSGlassEffectView（Liquid Glass）尚未接入（目前统一用 NSVisualEffectView）。
- macOS/Windows 上原生材质铺满整个窗口，面板内容的弹出动画在材质层之上进行（材质本身不做 spring 动画），观感需实机确认。
- 托盘左键切换面板：Linux 托盘只能弹菜单，Windows/macOS 未验证。
- 开机自启：Linux 写 `~/.config/autostart/Lancat.desktop`（插件行为），Windows 注册表 / macOS LaunchAgent 未验证。

## 已知问题

- Linux Wayland：默认构建走 XWayland。在 niri 这类平铺合成器上，XWayland 窗口会被平铺（贴边失效），请使用 `--features layer-shell` 构建（本机已验证 niri 下两个 layer surface 正常创建、贴右边）。layer-shell 下拿不到全局光标坐标，悬停/拖入只靠 WebView 自身事件。
- Linux 没有系统级模糊，玻璃效果是 CSS 半透明降级。
- 内存：Linux 上 WebKitGTK 偏重。本机实测（release，面板打开、1 个对端）PSS：lancat 主进程约 96MB，两个 WebKitWebProcess（dock 与 panel 各一个）约 109MB / 139MB，WebKitNetworkProcess 约 19MB，远超计划中 Windows < 50MB 的目标。可优化方向：dock 隐藏条改为原生 GTK/Win32 窗口而非 WebView、面板隐藏时卸载表情模块、Windows 上 WebView2 实测后再定。
- release 二进制约 11.3MB（未打包，`opt-level="s"` + thin LTO + strip，含 116 个内置表情）。
- 拖入感应在 Linux 依赖 GTK 把拖拽事件交给 6px 的 WebView；极窄条在某些合成器上可能难以命中，可在设置里把隐藏条长度调大。
- 传输未加密（计划里的可选 TLS 未做）、未做限速。
- 计划中的 M5 移动端、自动更新、多块并发的单连接 HTTP/2 优化尚未开始。
- Telegram `.tgs` 中使用表达式（expressions）的动画在 lottie-web light 渲染器下可能不完整。

## 许可与致谢

- 代码：MIT。
- 内置动态表情：Google **Noto Animated Emoji**，以 **CC BY 4.0** 许可使用（https://creativecommons.org/licenses/by/4.0/），仅做了数值精度压缩，详见 `src/assets/emoji/LICENSE.md`（同时随应用分发在 `licenses/noto-animated-emoji.md`）。
- Telegram 贴纸不内置，只能由用户自行导入。
