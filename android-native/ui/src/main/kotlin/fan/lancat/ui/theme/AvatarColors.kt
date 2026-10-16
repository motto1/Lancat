package fan.lancat.ui.theme

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.lerp

/**
 * 头像配色，逐行移植自桌面端：
 *  - src/lib/api.ts `avatarColor(id)`：h = h*31 + charCode（u32 回绕），hsl(HUES[h % 9] 70% 55%)
 *  - crates/lancat-slint/src/dock/toast.rs `avatar_color`：同一算法（按 UTF-16 单元累加）
 *  - Sidebar.svelte `.avatar` 背景：linear-gradient(180deg, color-mix(c 82%, white), c)
 * 注意桌面端用的是设备 id 而不是名字做 hash，这里保持一致，保证同一台设备在两端颜色相同。
 */
object AvatarColors {
    private val HUES = floatArrayOf(211f, 262f, 330f, 14f, 32f, 145f, 175f, 195f, 290f)

    /** 与 api.ts 相同的 u32 hash（Kotlin Int 乘加天然 32 位回绕，取模前按无符号处理）。 */
    fun hash(id: String): Long {
        var h = 0
        for (ch in id) h = h * 31 + ch.code   // String 按 UTF-16 单元迭代，与 encode_utf16 一致
        return h.toLong() and 0xFFFF_FFFFL
    }

    fun hue(id: String): Float = HUES[(hash(id) % HUES.size).toInt()]

    /** hsl(hue 70% 55%) */
    fun base(id: String): Color = hsl(hue(id), 0.70f, 0.55f)

    /** 渐变顶部色：color-mix(in srgb, c 82%, white) */
    fun top(id: String): Color = lerp(Color.White, base(id), 0.82f)

    fun hsl(h: Float, s: Float, l: Float): Color {
        val c = (1f - kotlin.math.abs(2f * l - 1f)) * s
        val x = c * (1f - kotlin.math.abs((h / 60f) % 2f - 1f))
        val m = l - c / 2f
        val (r, g, b) = when ((h / 60f).toInt()) {
            0 -> Triple(c, x, 0f)
            1 -> Triple(x, c, 0f)
            2 -> Triple(0f, c, x)
            3 -> Triple(0f, x, c)
            4 -> Triple(x, 0f, c)
            else -> Triple(c, 0f, x)
        }
        return Color(r + m, g + m, b + m, 1f)
    }

    /**
     * 头像文字，移植自 api.ts `initials`：
     * 有 CJK 取第一个汉字；按空白和 -_. 分词，≥2 段取两段首字母；否则取前两个字符；全部大写。
     */
    fun initials(name: String): String {
        val n = name.trim()
        if (n.isEmpty()) return "?"
        n.firstOrNull { it in '\u3400'..'\u9fff' }?.let { return it.toString() }
        val parts = n.split(Regex("[\\s\\-_.]+")).filter { it.isNotEmpty() }
        if (parts.size >= 2) return (parts[0].take(1) + parts[1].take(1)).uppercase()
        return n.take(2).uppercase()
    }
}
