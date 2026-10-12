# Lancat 面板盘点（Slint 重写用，基于主库 66fa038）

来源：arch ~/Projects/Lancat `git archive HEAD`（不含他人未提交的 relay/mobile 改动）。

## 1. 窗口与停靠（src-tauri/src/dock.rs、platform/windows.rs、native_dock/*）
- 面板逻辑尺寸 360×520；`MARGIN` = 0（原生玻璃平台 Windows/macOS）或 8（Linux，CSS 阴影留边）；停靠条厚 `STRIP`=6（逻辑 px）。
- 停靠：边缘 left/right/top/bottom、`dock_offset`（0–1 位置）、`dock_length`（隐藏条长度）、`dock_monitor`；悬停 `hover_delay_ms` 后弹出，离开 `collapse_delay_ms` 后收起；`NEAR_PX`=240 判定接近。
- Windows 面板（platform/windows.rs，要原样移植到 HWND 版）：
  - `round_corners`：DWMWA_WINDOW_CORNER_PREFERENCE=ROUND（8px DWM 圆角）。
  - `glass_mode`/`accent`：SetWindowCompositionAttribute ACCENT_ENABLE_ACRYLICBLURBEHIND，tint 随主题（未激活也生效）；透明效果关（EnableTransparency=0）→ 实底 #F8F6F6（浅）/深色对应色。`system_backdrop` 为 Mica/Acrylic 备选。
  - `refresh_material`：主题切换 / ThemeChanged 时重设 tint。
  - 停放代替 hide：收起时 WS_EX_LAYERED|WS_EX_TRANSPARENT + alpha 0（点击穿透，前台交还原窗口）；`LANCAT_WIN_PARK=0` 回退。
  - 淡入 120ms / 淡出 140ms 整窗 alpha（`LANCAT_WIN_FADE_MS`），打断时从当前 alpha 连续过渡；完全显示后去 layered。
  - DWM 阴影：DwmExtendFrameIntoClientArea(1)（不用 tao shadow）；`LANCAT_WIN_SHADOW=0` 关。
  - DWM 过渡动画关闭（DWMWA_TRANSITIONS_FORCEDISABLED）。
  - 冷开门控 panel_ready（热 350ms / 冷 4s 超时）——Slint 无 WebView，冷开应 <100ms，门控可简化为“首帧渲染完成后淡入”。
  - 保活：收起后 `panel_keep_alive_s` 秒销毁 WebView；Slint 版收起只隐藏（内存小），可保留设置项含义＝“释放渲染缓冲”。
  - 低内存时释放停放面板。
- 停靠条：Windows `LancatDockStrip` 分层窗口（UpdateLayeredWindow 抗锯齿、主题色、工作区内、与面板淡入同步）；Linux native_dock/linux.rs（layer-shell）；toast：win_toast.rs（LancatDockToast，收起时来消息 ~5s）、Linux linux_toast（未提交，他人）。这些 **不重写**，Slint 面板需挂到同一状态机。
- 拖拽：win_drop.rs（OLE IDropTarget，停靠条上也能接文件）；面板内拖入文件到会话/头像 → send_files。
- 托盘 tray.rs；单实例 + CLI 开关 `--open-panel` / `--hide-panel` / `--settings`。

## 2. 组件（src/components）与功能
| 组件 | 功能要点 |
|---|---|
| Panel | 外框（Linux 8px 留边+CSS 阴影；Windows 0）、`.shell` 圆角 14、打开动画（Windows 仅 160ms 轻缩放）、悬停/离开收起逻辑（holding：草稿/最近 8s 打字/菜单/拖拽；收起延迟、400ms 复查）、拖拽热区（虚线框 2px accent，圆角 12）、底部 toast（黑 42% 圆角 10，2.6s） |
| Sidebar | 宽 66，头像 38 圆（彩色渐变，首字）、名称 10px、在线点 11、未读徽章 17 高红、「全部」群发项、右键菜单（清空聊天记录 / 删除设备）、离线 0.45 透明、拖放到头像发文件（放大 1.18） |
| Header | 高 46，标题 13/600 + 副标题 11（在线/输入中 accent）、刷新按钮（旋转）、设置齿轮（诊断红点 7px）、拖动可移到任意边缘（dock_move_start） |
| Conversation | 消息滚动列表 padding 10/10/6，日期分隔 10.5px，空态提示；上拉加载更早（get_messages before_ts） |
| MessageItem | 气泡 max 78%、padding 6/10、圆角 16（组首条尾角 5）、入 rgba(0,0,0,.06)/出 accent；入场 320ms 弹性；贴纸 160×160；文件卡 210 宽（图标 34、名 12.5/500、meta 10.5、进度条 4px、暂停/取消/重试、失败原因+「去修复」）；状态（发送中/送达/失败） |
| Composer | 附件按钮（菜单：文件…/文件夹…）、表情按钮、textarea（圆角 15、max 96 高、focus 3px accent 35% 光环）、发送圆钮 30（未就绪 0.35 透明 scale .88）；Enter 发送、Shift+Enter 换行；typing 上报 |
| EmojiPanel | 标签：最近使用 / 贴纸 / 表情；搜索（中英）；Lottie 动态表情（Noto animated emoji）+ 导入 .tgs/.json 贴纸；格子悬停缩放 |
| Sticker | Lottie 播放（lottie.ts，首帧海报缓存） |
| Settings | 本机（设备名、ID、端口）／停靠（边缘、位置、隐藏条长度、显示器、悬停弹出延迟、离开收起延迟、收起后释放面板）／接收（目录 选择…/恢复默认、单文件上限 MB）／设备与黑名单（设备列表 拉黑/移除、按 IP 拉黑多行、手动添加设备）／通用（开机自启）／外观（主题 跟随系统/浅/深 分段）／数据（清空）／关于（CC-BY 4.0 表情署名）／权限与诊断（DiagSection） |
| DiagSection | 检查列表（ok/warn/error/unknown）、详情、修复按钮（admin 标记、确认框）、演练模式提示 |
| Banner | 面板顶部问题横幅：去修复 / 关闭 |
| Empty | 无设备：正在寻找… + 手动添加输入框 |
| DropPicker | 拖入时选择目标设备 |

## 3. 前端状态（lib/state.svelte.ts、ui.svelte.ts、diag.svelte.ts）
- peers（排序：在线优先）、messages 按会话合并、selected（单个或「全部」）、unread、typing（对端）、progress（msg_id→done/total）、settings、toast、diag status/alert、ui.recentlyTyped（8s）。
- 主题：settings.theme system/light/dark → data-theme；系统深浅跟随。

## 4. Tauri 命令（全部要在 Slint 版改为直接调用 lancat-core / 本地模块）
ui_log get_me list_peers refresh_peers get_messages send_text send_emoji send_files send_typing mark_read retry_transfer pause_transfer cancel_transfer clear_history delete_message dock_move_start get_settings set_settings set_blocked forget_peer add_peer receive_dir is_transferring reveal_file list_stickers import_sticker get_sticker delete_sticker get_diagnostics diag_status dismiss_alert open_diagnostics diag_take_goto run_fix
- 事件：core `CoreEvent`（peer / peer_removed / message / message_update / typing / progress / history_cleared / settings / alert）经 `Node::subscribe()` broadcast；面板事件 panel://pinned panel://visible panel://collapse panel://button-up；诊断 lancat://diag。
- lancat-core 公共 API（node.rs）：start(NodeConfig) me port subscribe shutdown refresh_peers settings set_settings receive_dir check_receive_dir inbound_reports peers peer_view add_peer_addr set_blocked forget_peer send_text send_emoji send_typing messages mark_read clear_history delete_message is_transferring；文件发送在 transfer.rs。

## 5. 视觉 token（styles/global.css）
- 字体：Segoe UI Variable / PingFang SC / Noto Sans CJK SC / Microsoft YaHei UI …；基础 13px；mono Cascadia Code。
- accent #0a84ff（Windows 用系统 AccentColor）、accent-contrast #fff；radius 14 / radius-sm 9；online #30d158；danger #ff3b30。
- 动效：dur-fast 120 / dur 160 / dur-slow 220；ease cubic-bezier(.2,.8,.3,1)；ease-spring cubic-bezier(.2,.9,.3,1.4)。
- 浅色：glass rgba(246,246,248,.86)（native-glass .42）、glass-strong rgba(252,252,253,.94)、border rgba(0,0,0,.1)、highlight rgba(255,255,255,.75)、sidebar rgba(0,0,0,.035)、text #1d1d1f、text-2 rgba(60,60,67,.72)、text-3 rgba(60,60,67,.45)、sep rgba(0,0,0,.08)、bubble-in rgba(0,0,0,.06)、hover rgba(0,0,0,.05)、field rgba(255,255,255,.7)。
- 深色：glass rgba(36,36,40,.82)（native .38 → rgba(30,30,34,.38)）、glass-strong rgba(44,44,48,.94)、border rgba(255,255,255,.12)、highlight .14、sidebar rgba(255,255,255,.04)、text #f5f5f7、text-2 rgba(235,235,245,.68)、text-3 .38、sep rgba(255,255,255,.09)、bubble-in .1、hover .07、field .07。
- Windows 玻璃附加：顶部 sheen（radial 90%×55% at 12% 0 + 线性 0→22%）、内描边 rim-top/side/low（浅 .85/.35/rgba(0,0,0,.07)，深 .22/.09/rgba(0,0,0,.35)）。
- 阴影（Linux CSS）：0 0 0 .5px rgba(0,0,0,.12), 0 12px 40px rgba(0,0,0,.22), 0 2px 8px rgba(0,0,0,.08)。

## 6. 旧版截图基准 work/slint/ref/
- 尚未采集（需停用户实例起 CDP 测试实例；计划与阶段 2 的 Windows 实测合并做一次）。
