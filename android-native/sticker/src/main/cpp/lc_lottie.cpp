// Lancat 贴纸：ThorVG Lottie 播放器（见 lc_lottie.h）。
//
// 线程模型：
//   ThorVG 编译时不开 THORVG_THREAD_SUPPORT（与桌面 thorvg-sys 的 features=["vendored","lottie"] 一致），
//   CPU 渲染器的内存池、加载器管理器、ImageLoader::cs 等是进程级共享状态。桌面端用一把全局锁把
//   所有 ThorVG 调用串行化（lottie.rs: tvg_lock，参见 thorvg#4120）。这里同样：每个入口先拿 g_lock。
//   Kotlin 侧可以从任意线程调用；真正的并发度由 Kotlin 的渲染调度器决定。

#include "lc_lottie.h"

#include <mutex>
#include <new>
#include <string.h>

#include "thorvg_capi.h"

#ifndef LC_THORVG_VERSION
#define LC_THORVG_VERSION "unknown"
#endif

namespace {

std::mutex g_lock;
bool g_inited = false;

void ensureInit()
{
    // 调用方已持有 g_lock
    if (!g_inited) {
        tvg_engine_init(0);
        g_inited = true;
    }
}

}  // namespace

struct LcLottie
{
    Tvg_Animation anim = nullptr;
    Tvg_Paint pic = nullptr;     // 由 canvas 持有（加进 canvas 后随 canvas 销毁）
    Tvg_Canvas canvas = nullptr;
    float w = 0, h = 0;          // 动画原始尺寸
    float total = 0, duration = 0;
    // 当前绑定的目标（变化时才重新 set_target / 重新布局）
    uint32_t* tbuf = nullptr;
    uint32_t tstride = 0, tw = 0, th = 0;
};

extern "C" LcLottie* lc_lottie_load(const char* json, uint32_t len)
{
    if (!json || len == 0) return nullptr;
    std::lock_guard<std::mutex> g(g_lock);
    ensureInit();

    auto a = new (std::nothrow) LcLottie;
    if (!a) return nullptr;

    a->anim = tvg_animation_new();
    if (!a->anim) { delete a; return nullptr; }
    a->pic = tvg_animation_get_picture(a->anim);
    if (tvg_picture_load_data(a->pic, json, len, "lottie+json", nullptr, true) != TVG_RESULT_SUCCESS) {
        tvg_animation_del(a->anim);
        delete a;
        return nullptr;
    }
    tvg_picture_get_size(a->pic, &a->w, &a->h);
    if (a->w <= 0 || a->h <= 0) {
        tvg_animation_del(a->anim);
        delete a;
        return nullptr;
    }
    tvg_animation_get_total_frame(a->anim, &a->total);
    tvg_animation_get_duration(a->anim, &a->duration);

    a->canvas = tvg_swcanvas_create(TVG_ENGINE_OPTION_DEFAULT);
    if (!a->canvas) {
        tvg_animation_del(a->anim);
        delete a;
        return nullptr;
    }
    // canvas 接管 picture；动画对象本身仍需 tvg_animation_del
    tvg_canvas_add(a->canvas, a->pic);
    return a;
}

extern "C" void lc_lottie_size(const LcLottie* a, float* w, float* h)
{
    if (w) *w = a ? a->w : 0;
    if (h) *h = a ? a->h : 0;
}

extern "C" float lc_lottie_total_frames(const LcLottie* a) { return a ? a->total : 0; }

extern "C" float lc_lottie_duration(const LcLottie* a) { return a ? a->duration : 0; }

extern "C" int lc_lottie_render(LcLottie* a, float frame, uint32_t* buf, uint32_t stridePx, uint32_t w, uint32_t h)
{
    if (!a || !buf || w == 0 || h == 0 || stridePx < w) return -1;
    std::lock_guard<std::mutex> g(g_lock);

    if (a->tbuf != buf || a->tstride != stridePx || a->tw != w || a->th != h) {
        // ThorVG 只认 canvas 状态，不在 Updating/Drawing 中（sync 之后）重设目标是安全的
        auto r = tvg_swcanvas_set_target(a->canvas, buf, stridePx, w, h, TVG_COLORSPACE_ABGR8888);
        if (r != TVG_RESULT_SUCCESS) return (int)r;
        if (a->tw != w || a->th != h) {
            // fit-center：与桌面 Player::new 相同（正方形时 s = px / max(w,h)）
            float sx = (float)w / a->w, sy = (float)h / a->h;
            float s = sx < sy ? sx : sy;
            float dw = a->w * s, dh = a->h * s;
            tvg_picture_set_size(a->pic, dw, dh);
            // translate 是绝对设置（Paint::translate 覆盖变换的平移分量）
            tvg_paint_translate(a->pic, ((float)w - dw) / 2.0f, ((float)h - dh) / 2.0f);
        }
        a->tbuf = buf; a->tstride = stridePx; a->tw = w; a->th = h;
    }

    float maxf = a->total - 1.0f;
    if (maxf < 0) maxf = 0;
    if (!(frame >= 0)) frame = 0;  // 也处理 NaN
    if (frame > maxf) frame = maxf;
    tvg_animation_set_frame(a->anim, frame);  // 同一帧会返回 InsufficientCondition，属正常

    // 只清 w 列，stride 余下的部分不属于图像；ThorVG draw(clear=true) 也会清目标区域
    for (uint32_t y = 0; y < h; ++y) memset(buf + (size_t)y * stridePx, 0, (size_t)w * 4);
    auto r = tvg_canvas_update(a->canvas);
    if (r != TVG_RESULT_SUCCESS && r != TVG_RESULT_INSUFFICIENT_CONDITION) return (int)r;
    r = tvg_canvas_draw(a->canvas, true);
    if (r != TVG_RESULT_SUCCESS) return (int)r;
    r = tvg_canvas_sync(a->canvas);
    if (r != TVG_RESULT_SUCCESS) return (int)r;
    return 0;
}

extern "C" void lc_lottie_free(LcLottie* a)
{
    if (!a) return;
    std::lock_guard<std::mutex> g(g_lock);
    tvg_canvas_destroy(a->canvas);  // 一并释放 picture
    tvg_animation_del(a->anim);
    delete a;
}

extern "C" const char* lc_lottie_thorvg_version(void) { return LC_THORVG_VERSION; }
