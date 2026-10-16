// Lancat 贴纸：ThorVG Lottie 播放器的最小 C 接口。
// JNI（lc_lottie_jni.cpp）和 host 渲染测试（host/render_host.cpp）共用这一份实现，
// 保证手机上和测试里跑的是同一段代码。
//
// 行为对齐桌面端 crates/lancat-slint/src/lottie.rs：
//   - 内容按比例缩放后居中（fit-center）；
//   - 帧号 clamp 到 [0, total-1]；
//   - 像素格式 TVG_COLORSPACE_ABGR8888（预乘 alpha，内存字节序 R,G,B,A）
//     ＝ Android Bitmap.Config.ARGB_8888 的内存布局；
//   - 所有 ThorVG 调用在一把进程级互斥锁里串行（见 lc_lottie.cpp 顶部说明）。
#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct LcLottie LcLottie;

/** 解析 Lottie JSON（内部会复制一份数据）。失败返回 NULL。 */
LcLottie* lc_lottie_load(const char* json, uint32_t len);

/** 动画自身尺寸（Lottie 的 w/h）。 */
void lc_lottie_size(const LcLottie* a, float* w, float* h);

/** 总帧数（ThorVG 语义：op - ip）。 */
float lc_lottie_total_frames(const LcLottie* a);

/** 时长（秒）。 */
float lc_lottie_duration(const LcLottie* a);

/**
 * 把第 frame 帧画进调用方的缓冲区：w×h 像素，每行 stridePx 个像素（≥ w），预乘 RGBA。
 * 缓冲区会被整块清零后再画。返回 0 表示成功，否则是 ThorVG 的 Tvg_Result 或 -1。
 */
int lc_lottie_render(LcLottie* a, float frame, uint32_t* buf, uint32_t stridePx, uint32_t w, uint32_t h);

void lc_lottie_free(LcLottie* a);

/** 编进来的 ThorVG 版本，例如 "1.1.0"。 */
const char* lc_lottie_thorvg_version(void);

#ifdef __cplusplus
}
#endif
