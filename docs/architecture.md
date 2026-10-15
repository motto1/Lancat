# 架构与协议

> 产品介绍见 [README](../README.md)。这里是给想读代码、改协议的人看的细节。

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

### 公网连接（不在同一个网络）

两台设备既不在同一局域网、也不在同一个虚拟网时，可以让一端把本机 TCP 47820 通过 **Cloudflare Tunnel**、**frp** 或端口转发开放到公网，另一端在「设置 → 公网连接」（或侧栏「+ 添加」）填这个地址和配对码：

1. 开放的一端打开「允许通过公网地址连接」，第一次会生成配对码；
2. 连接的一端为每个地址保持一条 WebSocket（`https://` 地址自动用 `wss://`），断线按退避重连；
3. 首次握手带 `code:<配对码>`，对方回一个 token，之后都用 token；输错配对码有全局限速；
4. 公网来的连接只在带有效配对码或 token 时才被接受，局域网的「免配对」规则不适用于它。

### 自动中转

每条连接上会附带一份小 gossip：「我现在连着谁、他们的地址」。一台设备自己找不到某个对端时：

1. 先试别人告诉它的地址（另一个网段、Tailscale 地址……）。多数「连不上」只是「发现不了」（组播被过滤、不同网段），这一步就能直连；
2. 还不行，就通过往返时延最低的那台设备中转，消息和文件都包在 `WsMsg::Relay` 里转发。只转一跳，只在中转设备自己连着的两端之间转。

直连永远优先：只在没有直连时才走中转，地址尝试会随每次 gossip 重复（限频），一旦打通就切回直连。旧版本对端会忽略这两类消息。

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
    src/remote.rs               公网连接：配对码 / token、wss 拨号与重连
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

## 性能

Windows 11，1.5x 缩放，同一数据集（本机 + 2 个在线对端），整进程树合计：

| 阶段 | 旧版（6 进程） | Slint 版（1 进程） |
|---|---|---|
| 启动后、未弹出 | 工作集 52.5 MB，私有 155.8 MB | 工作集 30.0 MB，私有 8.8 MB |
| 弹出、静止 | 工作集 186.6 MB，私有 188.0 MB | 工作集 57.1 MB，私有 37.2 MB |
| 收起 75 s 后 | 工作集 55.8 MB，私有 174.7 MB | 工作集 2.8 MB，私有 34.4 MB |

CPU 空闲占用均在 0.01%–0.06%。测量方法与 Linux 数据见 [docs/perf.md](perf.md)。
