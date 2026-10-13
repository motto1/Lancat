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
