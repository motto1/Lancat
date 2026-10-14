# Lancat

在自己的设备之间零配置、零确认地即时分享：文件、文件夹、文本、动态表情。

平时它只是贴在屏幕边缘的一条 6px 隐藏条；鼠标悬停、或者拖着文件靠近，就弹出聊天小窗，鼠标离开后带回弹动画收起。同一局域网里的设备自动互相发现；Tailscale、EasyTier、ZeroTier、NetBird、Hamachi、Radmin VPN 这类虚拟局域网里的设备，也和局域网设备一样直接连上，不用填 IP、不用配对、不弹确认。

> 完整产品计划见 [docs/PLAN.md](docs/PLAN.md)，新旧前端对齐记录见 [docs/slint-parity.md](docs/slint-parity.md)，性能记录见 [docs/perf.md](docs/perf.md)。

---

## 目录

- [两个前端](#两个前端)
- [功能](#功能)
- [网络与发现](#网络与发现)
- [安全模型](#安全模型)
- [仓库结构](#仓库结构)
- [架构](#架构)
- [协议](#协议)
- [构建](#构建)
- [运行与调试](#运行与调试)
- [测试](#测试)
- [性能](#性能)
- [已知问题与待办](#已知问题与待办)
- [许可与致谢](#许可与致谢)

---

## 两个前端

仓库里有两套界面，共用同一个 Rust 核心 `lancat-core`、同一个数据目录和端口：

| | **Slint 版（当前主力）** `crates/lancat-slint` | **Tauri 版（旧版，已归档）** `src-tauri` + `src/` |
|---|---|---|
| 界面 | Slint 1.18 软件渲染，直接调用 `lancat-core` | Svelte 5 + TypeScript，跑在 WebView 里 |
| 进程 | 单进程 | 主进程 + 多个 WebView 子进程 |
| Windows | 自写 Win32 Platform：软件渲染 → 预乘 BGRA DIB，Acrylic + DWM 阴影 + 淡入，自写停靠条、托盘、toast | WebView2 |
| Linux | Wayland 下自写 SCTK 后端（wlr-layer-shell + wl_shm + text-input-v3 输入法），SNI 托盘；`LANCAT_SLINT_WL=0` 退回 winit | WebKitGTK，可选 gtk-layer-shell |
| 表情 | ThorVG（原生 C++，CPU 光栅）播放 Lottie | lottie-web |
| 空闲内存（Windows） | 私有约 9 MB | 私有约 156 MB |

Slint 版的逻辑、界面和动画按旧版 1:1 对齐（像素差 ≤1 物理像素，见 `docs/slint-parity.md`）。两者不能同时运行（共用端口 47820 和数据目录）。

---

## 功能

| 模块 | 说明 |
|---|---|
| 停靠条 | 6px 透明置顶条，不进任务栏；可设边（左/右/上/下）、位置、长度、显示器 |
| 弹出 / 收起 | 悬停 150 ms 弹出（spring：stiffness 300 / damping 24）；拖文件靠近立即弹出；离开 800 ms 回弹收起。输入框有焦点、表情面板或菜单打开、正在传输、已钉住时不收起 |
| 会话 | 左侧设备栏（在线点、未读数、右键拉黑/清空/删除），每台设备一个独立会话；顶部「全部」群发；Shift / Ctrl / ⌘ + 点击多选后发送 |
| 拖放 | 拖到头像发给该设备，拖到会话区发给当前会话，拖到「全部」群发 |
| 传输 | 文件、文件夹、文本、表情；4 MB 分块、4 块并发、断点续传（重启后仍可续）、每块与整文件 BLAKE3 校验；可暂停/取消；进度实时显示 |
| 表情 | 内置 116 个 Noto Animated Emoji（Lottie）；二级表情面板：分类、最近使用、中英文搜索；单独一个表情显示为 160 px 动态贴纸；可导入 Telegram `.tgs` 或 Lottie `.json` 贴纸 |
| 离线消息 | 对方离线时消息排队，上线后自动补发；经中转发出的未确认消息在中转链路重建后补发 |
| 存储 | SQLite：设备、会话（未读数、最后预览）、消息永久保存；一键清空 |
| 通知 | 面板收起时新消息弹 toast；Windows 另有托盘气泡 |
| 诊断 | 设置页「连接诊断」：防火墙、接收目录、端口、对端是否连得进来等逐项检查，带一键修复；面板收起时出问题也会弹 toast 提醒 |
| 系统 | 托盘（打开/钉住/接收文件夹/设置/开机自启/退出、未读数）、单实例、开机自启 |
| 视觉 | 系统字体栈、深浅色跟随系统或手动、强调色；Win11 Acrylic（Mica 兜底） |

---

## 网络与发现

### 设备发现

发现是**事件触发**的，空闲时不发广播：

1. **触发时机**：启动、网络变化（每 5 s 比对网卡地址，不发包）、睡眠唤醒、手动刷新；零在线设备时每 3 分钟补探一次。
2. **每次触发**：在 0 s / 1 s / 3 s 各发一轮 UDP「有人吗」探测，然后停止。
3. **探测目标**：
   - 每张可广播的网卡发定向广播（含 `255.255.255.255`）；
   - 已记住设备的上次地址、手动添加的地址，单播；
   - 虚拟网的对端列表，单播：
     - Tailscale：`tailscale status --json`
     - EasyTier：`easytier-cli -o json peer list`
     - NetBird：`netbird status --json`
   - 三层（L3，不能广播）虚拟网卡：在上面几步之后约 1 s，对本网卡所在 `/24` 做有界单播扫描（每批 50 个、批间约 20 ms）；网段大于 `/24` 不扫，靠 CLI、gossip 或手动地址。
4. **收到探测**的一方单播回复自己的信息，一次交换双方互相发现。
5. **mDNS** `_lancat._tcp.local.`（TXT：`id/name/os/ver`），只在物理网卡上运行。
6. **在线判定**：每对设备一条 WebSocket 长连接，30 s ping，连续 2 次无回应判离线；连接断开立即复探，3 s 无回应判离线；正常退出发 bye，对方立即离线。对端重拨时会检查旧连接是否还活着（2 s 内有帧或 3 s 内回 Pong），静默死链立即被新连接替换。

### 网卡分类：物理 / 虚拟

每张网卡按**系统给出的标记**分类，而不是靠名字猜：

| 平台 | 依据 |
|---|---|
| Linux | sysfs：有没有 `device/`（真实硬件）、`tun_flags`（TUN/TAP）、`type`（三层 / 点对点）等 |
| Windows | `GetIfTable2` 的 `MIB_IF_ROW2.HardwareInterface`、`MediaType`（如 `NdisMediumIP` 表示三层隧道） |

实测：arch 上 `wlan0` 物理、`tun0`（EasyTier）虚拟 L3、`tailscale0` 虚拟 L3、`virbr0` 虚拟；Windows 上 `Wi-Fi` 物理、`VMware Network Adapter VMnet1/VMnet8` 虚拟、`Tailscale` 虚拟 L3。

万一识别错了，可以在 `Settings.iface_kinds`（网卡名 → `"lan"` / `"virtual"`）里手动覆盖。这个开关目前只在底层，设置页还没有对应界面。

代理软件的伪网卡（`198.18.0.0/15`，如 FlClash / Clash 的 TUN）会被排除，不参与发现。

### 路径选择

同一台设备有多条路可走时，按网卡类别排优先级（不看延迟、MAC 或网卡名）：

1. 物理局域网
2. 其他普通 IPv4
3. 虚拟 / overlay 网络
4. IPv6

### 自动中转

每条连接上会附带一份小 gossip：「我现在连着谁、他们的地址」。一台设备自己找不到某个对端时：

1. 先试别人告诉它的地址（另一个网段、Tailscale 地址……）。多数「连不上」只是「发现不了」（组播被过滤、不同网段），这一步就能直连；
2. 还不行，就通过往返时延最低的那台设备中转，消息和文件都包在 `WsMsg::Relay` 里转发。只转一跳，只在中转设备自己连着的两端之间转。

直连永远优先：只在没有直连时才走中转，地址尝试会随每次 gossip 重复（限频），一旦打通就切回直连。旧版本对端会忽略这两类消息。

---

## 安全模型

Lancat 的前提是：**能进入你的局域网或虚拟局域网的，都是你自己的设备**。所以不做配对、不做认证、不弹「是否接收」确认。

- **接受的来源**：私有网段（RFC1918、链路本地、`100.64.0.0/10`、回环、IPv6 ULA / 链路本地）、本机虚拟网卡所在网段（可以是非私有地址，如 Hamachi `25.0.0.0/8`、Radmin VPN `26.0.0.0/8`、EasyTier 常见的 `10.126.126.0/24`），以及手动添加的地址。手动地址只拒绝未指定地址和组播地址。
- **黑名单**：设备黑名单 + IP 黑名单。
- **收到的文件**：去掉可执行位，永不自动打开或执行；相对路径清洗（防 `..` 穿越）；重名自动 `name (1).ext`；可设单文件大小上限。
- **传输未加密**。如果网络里有你不信任的设备，不要用 Lancat，或者只在加密的虚拟网（Tailscale、WireGuard 等）上用。
- **Windows 防火墙**：首次运行会为 Lancat 添加入站规则（所有网络类型；「专用」只是推荐）。诊断页能检查并一键修复。

---

## 仓库结构

```
Cargo.toml                    根 workspace：lancat-core + src-tauri（旧版）
crates/
  lancat-core/                纯 Rust 核心，无 GUI 依赖，可无头测试
    src/node.rs                 Node 门面：对外 API、事件广播、离线判定、补发
    src/discovery.rs            事件触发 UDP 探测 / 单播回复 / bye / mDNS / 虚拟网 CLI / L3 扫描
    src/netutil.rs              网卡枚举与分类（Linux sysfs、Windows IpHelper）、网段信任、路径优先级
    src/server.rs               axum 路由（/info /ws /msg /files /finish）+ 来源守卫
    src/link.rs                 每设备 WebSocket 长连接：握手、保活、离线判定、重连、死链替换
    src/relay.rs                gossip 与一跳自动中转
    src/transfer.rs             发送端：分块、并发、重试、续传、暂停/取消
    src/files.rs                接收端：安全路径、重名、BLAKE3
    src/progress.rs             传输进度
    src/protocol.rs             线上消息类型
    src/store.rs                SQLite：devices / conversations / messages / kv(settings)
    src/events.rs, errors.rs
    src/bin/lancat-cli.rs       无界面节点（serve / send），测试与脚本用
    tests/                      two_nodes / discovery / pause_cancel / progress 集成测试
  lancat-slint/               Slint 前端（独立 workspace，自带 Cargo.lock）
    ui/*.slint                  app / settings / toast / common / icons
    src/main.rs                 启动节点、绑定界面与事件
    src/win.rs                  Windows：自写 Slint Platform（DIB、Acrylic、DWM、IME）
    src/dock/                   Windows：停靠条、拖放、托盘、toast
    src/wl.rs, dock_wl.rs, toast_wl.rs   Linux：SCTK layer-shell 后端、停靠、toast
    src/compose.rs, fx.rs       分层增量合成；软件渲染器画不了的阴影/旋转/圆角裁剪
    src/lottie.rs, emoji*.rs, glyph.rs   ThorVG Lottie 线程、表情目录、系统彩色字形
    src/settings_ui.rs          设置窗口
    src/diag/                   连接诊断与一键修复
    src/filedlg.rs, icons.rs, metrics.rs
    assets/                     图标、托盘位图
src-tauri/                    旧版 Tauri 2 外壳（窗口、托盘、IPC、平台代码；含 Android 实验）
src/                          旧版 Svelte 5 前端；src/assets/emoji/ 为内置表情（两版共用）
scripts/
  cli-e2e.sh                  双进程端到端测试
  fetch-emoji.py              重新下载/压缩内置表情
  memprobe.sh / memprobe.ps1  整进程树内存测量
  preview/                    旧版无 Rust 的 UI 预览（mock + 无头 Chrome 截图）
docs/                         PLAN.md、perf.md、slint-parity.md、slint/inventory.md
```

---

## 架构

```
┌──────────────── Slint 前端（单进程）────────────────┐
│ 停靠条 │ 聊天面板 │ 表情面板 │ 设置 / 诊断 │ toast │ 托盘 │
│  Win32 Platform（Windows） / SCTK layer-shell（Linux） │
└──────────────────────┬──────────────────────────────┘
        直接调用 Node API │ ▲ CoreEvent（broadcast）
┌──────────────────────┴─┴──── lancat-core ───────────┐
│ Discovery（UDP 探测 + mDNS + 虚拟网 CLI + L3 扫描）     │
│ Link（每设备 WS 长连接） Relay（gossip + 一跳中转）      │
│ Server（axum HTTP/WS） Transfer / Files（分块续传）     │
│ Store（SQLite）                                       │
└─────────────────────────────────────────────────────┘
```

- 网络、文件读写和全部状态都在 `lancat-core` 里；前端只负责显示和交互。
- 事件：`peer`、`peer_removed`、`message`、`message_update`、`typing`、`progress`、`history_cleared`、`settings` 等。
- 数据目录：`<系统数据目录>/app.lancat.desktop/`（Linux `~/.local/share/app.lancat.desktop/`，Windows `%APPDATA%\app.lancat.desktop\`），内含 `lancat.db`、`stickers/`、缓存。两版共用。
- 接收目录默认是系统「下载」，传输中的分块在 `<接收目录>/.lancat-part/`。

---

## 协议

| 方向 | 内容 |
|---|---|
| UDP 47820 | 探测 / 回复 / bye：`{"lancat":1,"device":{id,name,os,ver,port}}` |
| mDNS 5353 | `_lancat._tcp.local.`，TXT `id/name/os/ver` |
| `GET /ws` | 首帧双方互发 `{"type":"hello","device":…}`，之后 `text` / `emoji` / `file_offer` / `typing` / `ack` / `ping` / `pong` / `peers`（gossip）/ `relay` |
| `POST /msg` | WebSocket 不可用时的 HTTP 兜底 |
| `POST /files/{tid}` | `FileManifest{transfer_id,msg_id,from,name,size,chunk_size,hash,ts}` → `{have:[已收块],complete}` |
| `PUT /files/{tid}/{i}` | 第 i 块原始字节，头 `x-lancat-chunk-hash: <blake3>` |
| `POST /finish/{tid}` | 整文件 BLAKE3 校验并移入接收目录；不一致返回 409，发送端重传一次 |

所有请求带 `x-lancat-id: <设备 UUID>`。每对设备一条长连接，id 小的一方拨号，断线自动重连。

需要放行的端口：**TCP 47820、UDP 47820、UDP 5353**。

---

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

---

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

---

## 测试

```bash
cargo test -p lancat-core --release   # 单元测试 + 集成测试
scripts/cli-e2e.sh                    # 双进程：serve + send（10MB 文件、文件夹、文本），两个 serve 自动互相发现并发消息
```

集成测试覆盖：文本双向、表情、多块文件、重名、文件夹、空文件、黑名单、离线排队补发、暂停/取消、进度、发现、静默死链替换（`silently_dead_link_is_replaced_when_the_peer_dials_again`）、虚拟网段信任与网卡分类等。arch 与 Windows 上核心测试均全部通过（Windows 30 passed）。

实机验证（2026-10-14）：arch（Wayland/niri）与 Windows 11 通过 Tailscale 自动发现、双向建立连接。

---

## 性能

Windows 11，1.5x 缩放，同一数据集（本机 + 2 个在线对端），整进程树合计：

| 阶段 | 旧版（6 进程） | Slint 版（1 进程） |
|---|---|---|
| 启动后、未弹出 | 工作集 52.5 MB，私有 155.8 MB | 工作集 30.0 MB，私有 8.8 MB |
| 弹出、静止 | 工作集 186.6 MB，私有 188.0 MB | 工作集 57.1 MB，私有 37.2 MB |
| 收起 75 s 后 | 工作集 55.8 MB，私有 174.7 MB | 工作集 2.8 MB，私有 34.4 MB |

CPU 空闲占用均在 0.01%–0.06%。测量方法与 Linux 数据见 [docs/perf.md](docs/perf.md)。

---

## 已知问题与待办

- **传输未加密**，没有限速。
- **网卡类型手动覆盖**只在底层（`Settings.iface_kinds`），设置页还没有界面；目前 arch 和 Windows 上自动识别都正确。
- 大于 `/24` 的三层虚拟网段不做扫描，需要靠虚拟网 CLI、gossip 或手动添加地址。
- 三台以上设备、跨多种虚拟网的完整场景尚未系统测试。
- Linux Slint 版：GNOME（mutter）不支持 layer-shell，只能退回 winit，贴边效果受限；没有系统级模糊。
- 深色主题下 Slint 版文字比旧版略细（软件渲染器无对比度增强）；多行段落逐行行距与旧版差 ≤1 px。
- macOS 尚无 Slint 版；旧版的 macOS 代码未在实机编译运行。
- 未做代码签名，Windows SmartScreen 可能拦截。
- 自动更新、移动端（旧版有 Android 实验）尚未完成。

---

## 许可与致谢

- 代码：MIT。
- 内置动态表情：Google **Noto Animated Emoji**，以 **CC BY 4.0** 许可使用（https://creativecommons.org/licenses/by/4.0/），仅做了数值精度压缩，详见 `src/assets/emoji/LICENSE.md`。
- Telegram 贴纸不内置，只能由用户自行导入。
- UI 框架 [Slint](https://slint.dev)，Lottie 渲染 [ThorVG](https://www.thorvg.org)。
