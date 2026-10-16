// host 渲染测试：用与 Android JNI 完全相同的 lc_lottie.cpp + ThorVG 源码，在 Windows/Linux 上
//   1) 把指定帧渲染成 PNG（预乘 → 直通 alpha 后写出）
//   2) 统计单帧渲染耗时
//
// 用法：
//   render_host png|rgba <out_dir> <px> <frames: 例如 0,mid,last 或 0,30,60> <json...>
//   render_host bench <px> <json...>          每个文件渲染全部帧，输出每帧平均/中位/最大 ms
//   render_host raw   <out.rgba> <px> <frame> <json>   直接写预乘 RGBA（与桌面 render_raw 字节一致，用来比对）

#include "lc_lottie.h"
#include "thorvg_capi.h"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

static bool readFile(const char* p, std::vector<char>& out)
{
    FILE* f = fopen(p, "rb");
    if (!f) return false;
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    out.resize(n);
    size_t r = fread(out.data(), 1, n, f);
    fclose(f);
    return r == (size_t)n;
}

// ---- 最小 PNG 写出（stored deflate，无压缩依赖）
static uint32_t crcTable[256];
static void crcInit()
{
    for (uint32_t n = 0; n < 256; n++) {
        uint32_t c = n;
        for (int k = 0; k < 8; k++) c = (c & 1) ? 0xEDB88320u ^ (c >> 1) : c >> 1;
        crcTable[n] = c;
    }
}
static uint32_t crc(const uint8_t* b, size_t n, uint32_t c = 0xFFFFFFFFu)
{
    for (size_t i = 0; i < n; i++) c = crcTable[(c ^ b[i]) & 0xff] ^ (c >> 8);
    return c;
}
static void be32(std::vector<uint8_t>& v, uint32_t x)
{
    v.push_back(x >> 24); v.push_back(x >> 16); v.push_back(x >> 8); v.push_back(x);
}
static void chunk(std::vector<uint8_t>& png, const char* type, const std::vector<uint8_t>& data)
{
    be32(png, (uint32_t)data.size());
    std::vector<uint8_t> td(type, type + 4);
    td.insert(td.end(), data.begin(), data.end());
    png.insert(png.end(), td.begin(), td.end());
    be32(png, crc(td.data(), td.size()) ^ 0xFFFFFFFFu);
}
static bool writePng(const char* path, const uint32_t* px, uint32_t w, uint32_t h)
{
    std::vector<uint8_t> raw;
    raw.reserve((size_t)(w * 4 + 1) * h);
    for (uint32_t y = 0; y < h; y++) {
        raw.push_back(0);
        for (uint32_t x = 0; x < w; x++) {
            uint32_t p = px[(size_t)y * w + x];
            uint8_t r = p & 0xff, g = (p >> 8) & 0xff, b = (p >> 16) & 0xff, a = p >> 24;
            if (a && a < 255) {  // 预乘 → 直通
                r = (uint8_t)std::min(255, (r * 255 + a / 2) / a);
                g = (uint8_t)std::min(255, (g * 255 + a / 2) / a);
                b = (uint8_t)std::min(255, (b * 255 + a / 2) / a);
            }
            raw.push_back(r); raw.push_back(g); raw.push_back(b); raw.push_back(a);
        }
    }
    std::vector<uint8_t> z = {0x78, 0x01};
    uint32_t s1 = 1, s2 = 0;
    for (uint8_t c : raw) { s1 = (s1 + c) % 65521; s2 = (s2 + s1) % 65521; }
    size_t off = 0;
    while (off < raw.size() || raw.empty()) {
        size_t n = std::min<size_t>(65535, raw.size() - off);
        bool last = off + n >= raw.size();
        z.push_back(last ? 1 : 0);
        z.push_back(n & 0xff); z.push_back(n >> 8);
        z.push_back(~n & 0xff); z.push_back((~n >> 8) & 0xff);
        z.insert(z.end(), raw.begin() + off, raw.begin() + off + n);
        off += n;
        if (last) break;
    }
    be32(z, (s2 << 16) | s1);
    std::vector<uint8_t> png = {0x89, 'P', 'N', 'G', 0x0d, 0x0a, 0x1a, 0x0a};
    std::vector<uint8_t> ihdr;
    be32(ihdr, w); be32(ihdr, h);
    ihdr.push_back(8); ihdr.push_back(6); ihdr.push_back(0); ihdr.push_back(0); ihdr.push_back(0);
    chunk(png, "IHDR", ihdr);
    chunk(png, "IDAT", z);
    chunk(png, "IEND", {});
    FILE* f = fopen(path, "wb");
    if (!f) return false;
    fwrite(png.data(), 1, png.size(), f);
    fclose(f);
    return true;
}

static std::string stem(const char* p)
{
    std::string s(p);
    size_t a = s.find_last_of("/\\");
    if (a != std::string::npos) s = s.substr(a + 1);
    size_t d = s.rfind('.');
    if (d != std::string::npos) s = s.substr(0, d);
    return s;
}

static double nowMs()
{
    using namespace std::chrono;
    return duration<double, std::milli>(steady_clock::now().time_since_epoch()).count();
}

int main(int argc, char** argv)
{
    crcInit();
    if (argc < 2) { fprintf(stderr, "usage: see source\n"); return 2; }
    std::string mode = argv[1];
    printf("thorvg %s\n", lc_lottie_thorvg_version());

    if ((mode == "png" || mode == "rgba") && argc >= 6) {
        const char* outDir = argv[2];
        uint32_t px = (uint32_t)atoi(argv[3]);
        std::string spec = argv[4];
        for (int i = 5; i < argc; i++) {
            std::vector<char> js;
            if (!readFile(argv[i], js)) { printf("ERR read %s\n", argv[i]); continue; }
            LcLottie* a = lc_lottie_load(js.data(), (uint32_t)js.size());
            if (!a) { printf("ERR load %s\n", argv[i]); continue; }
            float total = lc_lottie_total_frames(a), w, h;
            lc_lottie_size(a, &w, &h);
            printf("INFO %s w=%.0f h=%.0f total=%.2f dur=%.3f\n", stem(argv[i]).c_str(), w, h, total, lc_lottie_duration(a));
            std::vector<uint32_t> buf((size_t)px * px);
            size_t p0 = 0;
            while (p0 <= spec.size()) {
                size_t p1 = spec.find(',', p0);
                if (p1 == std::string::npos) p1 = spec.size();
                std::string t = spec.substr(p0, p1 - p0);
                p0 = p1 + 1;
                if (t.empty()) continue;
                float fr = t == "mid" ? (float)(int)(total / 2) : t == "last" ? total - 1 : (float)atof(t.c_str());
                int r = lc_lottie_render(a, fr, buf.data(), px, px, px);
                char path[1024];
                snprintf(path, sizeof path, "%s/%s_%u_f%03d.%s", outDir, stem(argv[i]).c_str(), px, (int)fr, mode.c_str());
                if (r != 0) printf("ERR render %s f=%g r=%d\n", argv[i], fr, r);
                else if (mode == "png") writePng(path, buf.data(), px, px);
                else {
                    FILE* f = fopen(path, "wb");
                    if (f) { fwrite(buf.data(), 4, buf.size(), f); fclose(f); }
                }
            }
            lc_lottie_free(a);
        }
        return 0;
    }


    if (mode == "dbg" && argc >= 3) {
        std::vector<char> js;
        readFile(argv[2], js);
        const uint32_t px = 160;
        auto count = [&](std::vector<uint32_t>& b) { size_t n = 0; for (auto v : b) n += (v >> 24) != 0; return n; };
        tvg_engine_init(0);
        for (int variant = 0; variant < 4; variant++) {
            std::vector<uint32_t> buf(px * px, 0);
            Tvg_Animation an = tvg_animation_new();
            Tvg_Paint pic = tvg_animation_get_picture(an);
            int r1 = tvg_picture_load_data(pic, js.data(), (uint32_t)js.size(), "lottie+json", nullptr, true);
            float w, h; tvg_picture_get_size(pic, &w, &h);
            Tvg_Canvas c = tvg_swcanvas_create(TVG_ENGINE_OPTION_DEFAULT);
            if (variant == 0) { // desktop order
                tvg_picture_set_size(pic, px, px);
                tvg_swcanvas_set_target(c, buf.data(), px, px, px, TVG_COLORSPACE_ABGR8888);
                tvg_canvas_add(c, pic);
            } else if (variant == 1) { // add first, then target+size
                tvg_canvas_add(c, pic);
                tvg_swcanvas_set_target(c, buf.data(), px, px, px, TVG_COLORSPACE_ABGR8888);
                tvg_picture_set_size(pic, px, px);
            } else if (variant == 2) { // size first, add, then target
                tvg_picture_set_size(pic, px, px);
                tvg_canvas_add(c, pic);
                tvg_swcanvas_set_target(c, buf.data(), px, px, px, TVG_COLORSPACE_ABGR8888);
            } else { // add, size, target
                tvg_canvas_add(c, pic);
                tvg_picture_set_size(pic, px, px);
                tvg_swcanvas_set_target(c, buf.data(), px, px, px, TVG_COLORSPACE_ABGR8888);
            }
            int r2 = tvg_animation_set_frame(an, 30);
            int r3 = tvg_canvas_update(c);
            int r4 = tvg_canvas_draw(c, true);
            int r5 = tvg_canvas_sync(c);
            printf("variant %d load=%d frame=%d upd=%d draw=%d sync=%d nonzero=%zu (w=%g)\n", variant, r1, r2, r3, r4, r5, count(buf), w);
            tvg_canvas_destroy(c);
            tvg_animation_del(an);
        }
        LcLottie* a = lc_lottie_load(js.data(), (uint32_t)js.size());
        std::vector<uint32_t> buf(px * px, 0);
        int r = lc_lottie_render(a, 30, buf.data(), px, px, px);
        printf("lc r=%d nonzero=%zu\n", r, count(buf));
        r = lc_lottie_render(a, 31, buf.data(), px, px, px);
        printf("lc2 r=%d nonzero=%zu\n", r, count(buf));
        return 0;
    }

    if (mode == "bench" && argc >= 4) {
        uint32_t px = (uint32_t)atoi(argv[2]);
        std::vector<uint32_t> buf((size_t)px * px);
        std::vector<double> all;
        for (int i = 3; i < argc; i++) {
            std::vector<char> js;
            if (!readFile(argv[i], js)) continue;
            double t0 = nowMs();
            LcLottie* a = lc_lottie_load(js.data(), (uint32_t)js.size());
            double tl = nowMs() - t0;
            if (!a) { printf("ERR load %s\n", argv[i]); continue; }
            int total = (int)lc_lottie_total_frames(a);
            std::vector<double> t;
            for (int f = 0; f < total; f++) {
                double s = nowMs();
                lc_lottie_render(a, (float)f, buf.data(), px, px, px);
                t.push_back(nowMs() - s);
            }
            lc_lottie_free(a);
            std::vector<double> st = t;
            std::sort(st.begin(), st.end());
            double sum = 0;
            for (double x : t) sum += x;
            printf("BENCH %s px=%u load=%.2fms frames=%d avg=%.3f p50=%.3f p95=%.3f max=%.3f\n", stem(argv[i]).c_str(), px, tl,
                   total, sum / std::max<size_t>(1, t.size()), st.empty() ? 0 : st[st.size() / 2],
                   st.empty() ? 0 : st[st.size() * 95 / 100], st.empty() ? 0 : st.back());
            all.insert(all.end(), t.begin(), t.end());
        }
        std::sort(all.begin(), all.end());
        double sum = 0;
        for (double x : all) sum += x;
        if (!all.empty())
            printf("TOTAL px=%u frames=%zu avg=%.3f p50=%.3f p95=%.3f p99=%.3f max=%.3f\n", px, all.size(), sum / all.size(),
                   all[all.size() / 2], all[all.size() * 95 / 100], all[all.size() * 99 / 100], all.back());
        return 0;
    }

    if (mode == "raw" && argc >= 6) {
        uint32_t px = (uint32_t)atoi(argv[3]);
        std::vector<char> js;
        if (!readFile(argv[5], js)) return 1;
        LcLottie* a = lc_lottie_load(js.data(), (uint32_t)js.size());
        if (!a) return 1;
        std::vector<uint32_t> buf((size_t)px * px);
        lc_lottie_render(a, (float)atof(argv[4]), buf.data(), px, px, px);
        FILE* f = fopen(argv[2], "wb");
        fwrite(buf.data(), 4, buf.size(), f);
        fclose(f);
        lc_lottie_free(a);
        return 0;
    }
    fprintf(stderr, "bad args\n");
    return 2;
}
