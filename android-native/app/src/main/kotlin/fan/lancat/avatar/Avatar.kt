package fan.lancat.avatar

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.LinearGradient
import android.graphics.Paint
import android.graphics.Shader
import android.graphics.Typeface
import kotlin.math.abs

/**
 * 头像算法，与桌面端完全一致（Lancat crates/lancat-slint/src/main.rs avatar_colors / initials，
 * 以及 src/lib/api.ts avatarColor / initials）：
 * - 颜色：对设备 id 的 UTF-16 码元做 h = h*31 + c（u32 回绕），取 HUES[h % 9]，hsl(hue, 70%, 55%)
 * - 渐变：自上而下，顶部 = 82% 本色 + 18% 白，底部 = 本色（CSS: linear-gradient(180deg, color-mix(c 82%, white), c)）
 * - 文字：名字里第一个 CJK 字；否则按空白/-/_/. 分词，≥2 段取两段首字母；否则前两个字符。大写，白色粗体
 * - 拉黑：灰度（0.2126R + 0.7152G + 0.0722B）
 */
object Avatar {
    private val HUES = floatArrayOf(211f, 262f, 330f, 14f, 32f, 145f, 175f, 195f, 290f)

    fun hueIndex(id: String): Int {
        var h = 0
        for (c in id) h = h * 31 + c.code      // Int 溢出与 u32 wrapping 位模式相同
        return (h.toUInt() % HUES.size.toUInt()).toInt()
    }

    fun hue(id: String): Float = HUES[hueIndex(id)]

    /** hsl → rgb（0..1），与桌面 hsl() 同一公式 */
    fun hsl(h: Float, s: Float, l: Float): FloatArray {
        val c = (1f - abs(2f * l - 1f)) * s
        val hp = h / 60f
        val x = c * (1f - abs(hp % 2f - 1f))
        val (r, g, b) = when (hp.toInt()) {
            0 -> Triple(c, x, 0f)
            1 -> Triple(x, c, 0f)
            2 -> Triple(0f, c, x)
            3 -> Triple(0f, x, c)
            4 -> Triple(x, 0f, c)
            else -> Triple(c, 0f, x)
        }
        val m = l - c / 2f
        return floatArrayOf(r + m, g + m, b + m)
    }

    /** 返回 (顶部色, 底部色) ARGB */
    fun colors(id: String, gray: Boolean = false): Pair<Int, Int> {
        var (r, g, b) = hsl(hue(id), 0.7f, 0.55f).let { Triple(it[0], it[1], it[2]) }
        if (gray) {
            val y = 0.2126f * r + 0.7152f * g + 0.0722f * b
            r = y; g = y; b = y
        }
        val bottom = Color.rgb(to8(r), to8(g), to8(b))
        val top = Color.rgb(to8(r * 0.82f + 0.18f), to8(g * 0.82f + 0.18f), to8(b * 0.82f + 0.18f))
        return top to bottom
    }

    private fun to8(v: Float) = (v.coerceIn(0f, 1f) * 255f + 0.5f).toInt()

    fun initials(name: String): String {
        val n = name.trim()
        if (n.isEmpty()) return "?"
        n.firstOrNull { it in '\u3400'..'\u9fff' }?.let { return it.toString() }
        val parts = n.split(Regex("[\\s\\-_.]+")).filter { it.isNotEmpty() }
        if (parts.size >= 2) {
            return (parts[0].codePointPrefix(1) + parts[1].codePointPrefix(1)).uppercase()
        }
        return n.codePointPrefix(2).uppercase()
    }

    private fun String.codePointPrefix(count: Int): String {
        val end = offsetByCodePoints(0, minOf(count, codePointCount(0, length)))
        return substring(0, end)
    }

    /**
     * 画头像位图。
     * @param adaptive true：画满整张方形（给 IconCompat.createWithAdaptiveBitmap，系统再裁成圆/方），
     *   文字缩到中间 72/108 的安全区内；false：画圆形，透明背景（通知 Person 图标等）。
     */
    fun bitmap(id: String, name: String, sizePx: Int, adaptive: Boolean, gray: Boolean = false): Bitmap {
        val bmp = Bitmap.createBitmap(sizePx, sizePx, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bmp)
        val (top, bottom) = colors(id, gray)
        val p = Paint(Paint.ANTI_ALIAS_FLAG)
        p.shader = LinearGradient(0f, 0f, 0f, sizePx.toFloat(), top, bottom, Shader.TileMode.CLAMP)
        val s = sizePx.toFloat()
        if (adaptive) canvas.drawRect(0f, 0f, s, s, p) else canvas.drawCircle(s / 2, s / 2, s / 2, p)

        val text = initials(name)
        val visible = if (adaptive) s * 72f / 108f else s       // 实际可见直径
        val tp = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            color = Color.WHITE
            typeface = Typeface.create(Typeface.DEFAULT, Typeface.BOLD)
            textAlign = Paint.Align.CENTER
            textSize = visible * (if (text.length >= 2) 0.38f else 0.44f)
        }
        val fm = tp.fontMetrics
        canvas.drawText(text, s / 2, s / 2 - (fm.ascent + fm.descent) / 2, tp)
        return bmp
    }
}
