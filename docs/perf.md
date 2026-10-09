# Lancat 性能记录

所有内存数据都来自 `scripts/memprobe.sh`（Windows 用 `scripts/memprobe.ps1`），统计的是**整个进程树**：主进程加上全部 WebKit/WebView2 子进程。

- **priv** = USS（Private_Clean + Private_Dirty）+ SwapPss，即只属于这个进程树的内存，被换出的页也算在内。指标 A 用它。
- **PSS** = 按比例分摊共享页之后的内存。指标 B 用 PSS + SwapPss。
- 三个采样点：`idle` 是启动后面板一直收起、等待 25 秒；`open` 是面板展开 6 秒后（1 个对端、3 条消息）；`closed` 是面板收起后再等 75 秒（超过默认 60 秒的销毁时间）。
- 打开延迟取自日志 `panel shown N ms after open request`：从收到打开请求，到面板开始播放弹出动画的第一帧（requestAnimationFrame 回调）为止。

测量环境：motto-arch，Arch Linux，内核 7.0.14-zen，niri（Wayland），Intel UHD 610 + RTX 3050 Mobile（混合显卡，合成器跑在 Intel 上），webkit2gtk-4.1 2.52.4，mesa 26.1.3，nvidia 610.43.02。release 构建，`--features layer-shell`。测量时机器负载很高（其他浏览器和 Unity 占用了大部分内存，swap 一度用满），绝对值会有几 MB 的波动；结论以同一台机器上前后对比为准。

## 基线（95ea2f2 + 测量钩子）

| 进程 | idle PSS | idle priv | open PSS | open priv | closed priv |
|---|---|---|---|---|---|
| lancat 主进程（29 线程） | 93.8 | 58.4 | 98.2 | 63.2 | 52.8 |
| WebKitWebProcess（panel） | 112.6 | 62.4 | 171.5 | 117.8 | 91.1 |
| WebKitWebProcess（dock） | 99.9 | 49.6 | 98.4 | 47.0 | 46.1 |
| WebKitNetworkProcess | 20.7 | 12.6 | 20.6 | 12.6 | 10.8 |
| **合计** | **326.9** | **183.1** | **388.7** | **240.6** | **200.8** |

单位 MB。A（idle priv）= **183.1MB**；B（open PSS）= **388.7MB**；热打开延迟 127ms；二进制 11.90MB。

注意：面板用过一次以后，panel WebProcess 收起后不会回落（91MB，比启动时多 29MB）。也就是说，常驻的面板会把展开时的峰值一直留在内存里。
