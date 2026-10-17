# Slint 版与旧版（Svelte / WebView）对齐记录

对照方法：同一份数据集（本机「我的电脑」+ lancat-cli 对端 MacBook、书房台式机），同一屏幕位置（左缘 30%），
Windows 1.5x 缩放下分别跑旧版 `lancat.exe` 和新版 `lancat-slint.exe`，按同一脚本截图
（`conv / tip / emoji / attach / menu / toast / broadcast / banner / settings / diag / settings2-4`，浅色 + 深色），
逐张做像素差，再按墨迹行（ink rows）量偏移。脚本在 Windows 的 `G:\lancat-work\sl\`（scene.ps1 / all.ps1 / perf.ps1）。

## 已对齐（像素级，1.5x 下 ≤1 物理像素）

| 区域 | 旧版规则 | Slint 做法 |
|---|---|---|
| 表情面板静止格 | 系统彩色字形 25px（Segoe UI Emoji），悬停才播 Lottie | `glyph.rs`：Direct2D 软件光栅 + DirectWrite `ENABLE_COLOR_FONT` 画成位图，按墨迹居中；磁盘缓存 `cache/posters/glyphw-*`；非 Windows 退回 Lottie 首帧 |
| 表情搜索框 | 面板 1px 边 + margin 8 8 4，padding 4 9，12px 字 normal 行高 18 → 框高 26 | y 9、高 26，下方留 5（网格对齐） |
| 时间分隔 | 10.5px Segoe UI Variable Text，行框 12.67，margin 10 0 6 | 行框 12.67，y 8 居中 |
| 顶栏状态行 | 13px 标题 + 11px 状态 | 行距 -0.67、整体上移 0.67 |
| 右键菜单 / 附件菜单条目 | padding 6 + 12.5px 中文行框 16.67 = 28.67 | MenuItem 高 28.67；附件菜单 padding 3/5 |
| 设置分区标题 h2 | margin 8 0 4，文字在 15px 行框内居中 | 27 高容器内 y 8、高 15 |
| 诊断头 | .head margin 8 0 4 | padding-top 9（按钮与条目实测对齐） |
| 诊断条目 | .title 行框 16.67；.summary 11.5px line-height 1.45 | 标题行 min-height 16.67；说明按行数 × 16.675 定高、居中；标题与说明间距 2 |
| 设置窗口标题栏 | 带应用图标；深浅色跟系统，不跟应用主题 | WM_SETICON（小 16、大 32 × 缩放）；`set_normal_dark(theme_is_dark("system"))` |
| 侧栏点击 | 点按钮会夺走输入框焦点（焦点环消失） | 选设备 / 全部时 `input.clear-focus()` |
| 已开着的设置窗口再「去修复」 | 只滚到诊断 + 闪烁 + 重查，不重载 | `is_open()` 时 `raise()` + `goto_diag()` |
| 收起后释放（panel_keep_alive_s） | 收起超过设定秒数销毁网页进程，0 = 常驻 | 收起 400 ms 后修剪工作集；超过 keep-alive 释放表情字形 / 贴纸首帧 / 正在播的贴纸，下次弹出按需重建 |

更早的对齐（外壳动画、消息入场、可选中、会话历史缓存、贴纸播放规则、提示气泡、设置控件、软件渲染器画不了的阴影 / 旋转 / 圆角裁剪改由 `fx.rs`、`compose.rs` 合成）见提交 05c20f3 及之前。

## Windows：GPU 呈现与合成（`src/gpu.rs`）

- 面板窗口带 `WS_EX_NOREDIRECTIONBITMAP`，内容是 D3D11 composition swapchain（预乘 alpha）挂在 DirectComposition visual 上；整窗淡入淡出改在呈现着色器里乘透明度，停放时仍加 `WS_EX_LAYERED | WS_EX_TRANSPARENT` 点击穿透。设备和着色器在后台线程建，好了再接上交换链。
- `compose.rs` 的分层合成搬到像素着色器：背景模糊（同一套降采样 + 三次盒式）、饱和度、liquid 位移、圆角覆盖、层的缩放/平移/透明度和预乘 over。Slint 仍在 CPU 上逐层光栅化，层的缓冲上传成纹理；只有姿态在动的帧（入场动画、拖放结束的回弹、toast）探测无变化后直接复用纹理重合成。与 CPU 合成逐像素比对：每通道差 ≤2（偶发 ≤7，来自冻结模糊的取样时刻）。
- 动画中事件循环按 swapchain 的 frame-latency 句柄（显示器刷新率）要帧；Slint 层渲染仍是主要开销。
- `LANCAT_CPU_COMPOSE=1`：整条走原来的 CPU 路径（GDI 呈现）。没有硬件设备时用 WARP 只做呈现、合成走 CPU；连 WARP 都失败则把面板窗口重建成 CPU 版。`LANCAT_GPU_COMPOSE=0`：GPU 只呈现。`LANCAT_GPU_TEST=warp|fail` 用来测降级路径。
- 停靠条 ⇄ 面板的变形窗口（`dock/morph.rs`）、停靠条、toast、设置窗口也走同一个 D3D 设备（各自一条 DComp 交换链）；设置 → 通用 → 渲染方式可当场在 GPU / CPU 间切换（两个平台共用 `render.rs`）。

## GPU 路径的健壮性（v0.2.10，Windows + Linux）

| 项 | Windows（`gpu.rs` / `win.rs` / `passthru.rs`） | Linux / Wayland（`gl.rs` / `wl.rs`） |
|---|---|---|
| 点击穿透 | DComp 窗口没有逐像素命中、`HTTRANSPARENT` 挡不住别的进程：光标落在透明处（toast 卡片外、面板圆角外 alpha 0 处）时给窗口加 `WS_EX_LAYERED \| WS_EX_TRANSPARENT`（LWA 255，画面不变），回到实心处去掉；16 ms 线程定时器只在有窗口登记时跑，按住鼠标键时不切换。变形窗口固定 layered + transparent。`LANCAT_PASSTHRU=0` 关、`=force` 测试。实测 `WindowFromPoint`：toast 外沿 / 面板角 → 下面的窗口，卡片 / 面板中间 → Lancat | 面板 surface 设输入区域 = 卡片（去掉外圈 FRAME 阴影边），收起中为空区域（整块穿透）；toast 原来就只让卡片接指针。CPU（wl_shm）路径同样受益 |
| 设备 / 上下文丢失 | `Present` 返回 `DXGI_ERROR_DEVICE_REMOVED / RESET / HUNG / DRIVER_INTERNAL_ERROR` → 所有窗口当场换成 CPU 呈现（画面不断），1.5 s 后后台重建设备再接回 GPU；60 s 内连续 3 次以上就留在 CPU，设置页显示「GPU 不可用」。`LANCAT_GPU_TEST=lose[:ms]` 假装丢一次（实测：丢 → CPU 变形 → 重建 0.3 s → GPU 变形） | `eglSwapBuffers` 返回 `EGL_CONTEXT_LOST` → 丢掉所有 EGL surface，退回 wl_shm，1.5 s 后后台重建（实测重建 15 ms）；同样的次数上限和测试钩子 |
| 首次展开 | 设备接上后预建变形窗口的交换链（按当前布局尺寸）并清一帧：首次展开准备 24 ms → 4 ms | GL 上下文在后台线程建（Mesa 加载驱动 + 编译 ~0.3 s 不再挡启动），好了唤醒事件循环接上，期间 wl_shm |
| 展开前截屏 | 光标进停靠条（hover_delay 之前）或拖着东西靠近时后台先截好（`capture_raw`，1.5 s 内有效），展开时只上传 + GPU 模糊：12–25 ms → 1.6–2.4 ms | 同样在光标进条时先发 wlr-screencopy；展开不再等 25–30 ms 截图。分数缩放（1.25 / 1.5 / 1.75）下按区域截会被 niri 判 `invalid buffer`（协议错误、进程退出），改为截整个输出再自己裁 |
| 展开交接掉帧 | 变形淡出阶段面板已接手时，面板的 Present 不再等自己的 frame-latency 句柄（事件循环跟变形窗口的节拍）：交接期最大帧间隔 14–25 ms → 7–10 ms | 面板的 CPU 路径也按合成器 frame 回调出帧，不再 8 ms 轮询空画 |
| DPI / 缩放 | 面板 `WM_DPICHANGED` → `WM_SIZE` → `ResizeBuffers`；变形窗口、toast、停靠条的交换链每次按窗口尺寸 resize（`Surface::resize` / `Blit::resize`），尺寸来自当次的缩放 | niri 1.25 / 1.5 / 1.75 实测：面板、变形、toast 都正常，截图对齐 |
| GPU / CPU 对比 | 变形、面板、toast 逐像素比对每通道 ≤2 | toast GPU 与 CPU 截图除文字外一致 |

## 有意不同 / 无法对齐

- 深色主题下旧版文字略粗：Chromium 在深底上做了对比度增强，Slint 软件渲染器的灰度抗锯齿没有这一步。
- 多行文字的逐行行距：Slint `Text` 没有 line-height，多行段落按总高度对齐、首行逐像素一致，之后每行差 ≤1px。
- 「收起后释放面板」默认值：旧版二进制是旧 core（默认 0 = 常驻），新 core 默认 60 s，属于 core 的默认值变化，不是界面差异。
- 截图里「连接情况」文案不同（谁最后连进来），取决于两个 cli 对端的上线顺序，不是界面差异。

## 内存 / CPU（Windows，1.5x，同一数据集，本机 + 2 个在线对端）

见 `perf.ps1` 输出；整进程树合计（旧版含 WebView2 子进程）。

| 阶段 | 旧版（6 进程） | Slint 版（1 进程） |
|---|---|---|
| 启动后、未弹出 | 工作集 52.5 MB，私有 155.8 MB，CPU 0.04% | 工作集 30.0 MB，私有 8.8 MB，CPU 0.02% |
| 弹出、静止 | 工作集 186.6 MB，私有 188.0 MB，CPU 0.06% | 工作集 57.1 MB，私有 37.2 MB，CPU 0.03% |
| 收起 75 s 后 | 工作集 55.8 MB，私有 174.7 MB，CPU 0.03% | 工作集 2.8 MB，私有 34.4 MB，CPU 0.01% |

（2026-10-13，mottolaptop，CPU 为整机核数归一化的平均占用）
