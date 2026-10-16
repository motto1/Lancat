package fan.lancat.ui.util

import androidx.compose.runtime.staticCompositionLocalOf
import java.util.Calendar
import java.util.Locale
import kotlin.math.ceil

/** 「现在」的来源。截图测试里换成固定时间，保证每次渲染一致。 */
fun interface Clock { fun now(): Long }

val LocalClock = staticCompositionLocalOf<Clock> { Clock { System.currentTimeMillis() } }

object Fmt {
    private fun cal(ms: Long) = Calendar.getInstance().apply { timeInMillis = ms }

    private fun sameDay(a: Calendar, b: Calendar) =
        a.get(Calendar.YEAR) == b.get(Calendar.YEAR) && a.get(Calendar.DAY_OF_YEAR) == b.get(Calendar.DAY_OF_YEAR)

    fun hm(ms: Long): String {
        val c = cal(ms)
        return String.format(Locale.ROOT, "%02d:%02d", c.get(Calendar.HOUR_OF_DAY), c.get(Calendar.MINUTE))
    }

    /** 会话列表右侧时间：今天 HH:mm，昨天，本周 周X，今年 M月d日，更早 yyyy/M/d。 */
    fun listTime(ms: Long, now: Long): String {
        val d = cal(ms); val n = cal(now)
        if (sameDay(d, n)) return hm(ms)
        val y = cal(now).apply { add(Calendar.DAY_OF_YEAR, -1) }
        if (sameDay(d, y)) return "昨天"
        val diffDays = (startOfDay(now) - startOfDay(ms)) / 86_400_000L
        if (diffDays in 0..6) return WEEK[d.get(Calendar.DAY_OF_WEEK) - 1]
        if (d.get(Calendar.YEAR) == n.get(Calendar.YEAR)) return "${d.get(Calendar.MONTH) + 1}月${d.get(Calendar.DAY_OF_MONTH)}日"
        return "${d.get(Calendar.YEAR)}/${d.get(Calendar.MONTH) + 1}/${d.get(Calendar.DAY_OF_MONTH)}"
    }

    /** 聊天里的日期标签：今天 / 昨天 / M月d日 周X / yyyy年M月d日。 */
    fun dateLabel(ms: Long, now: Long): String {
        val d = cal(ms); val n = cal(now)
        if (sameDay(d, n)) return "今天"
        val y = cal(now).apply { add(Calendar.DAY_OF_YEAR, -1) }
        if (sameDay(d, y)) return "昨天"
        val md = "${d.get(Calendar.MONTH) + 1}月${d.get(Calendar.DAY_OF_MONTH)}日"
        if (d.get(Calendar.YEAR) == n.get(Calendar.YEAR)) return "$md ${WEEK[d.get(Calendar.DAY_OF_WEEK) - 1]}"
        return "${d.get(Calendar.YEAR)}年$md"
    }

    fun startOfDay(ms: Long): Long = cal(ms).apply {
        set(Calendar.HOUR_OF_DAY, 0); set(Calendar.MINUTE, 0); set(Calendar.SECOND, 0); set(Calendar.MILLISECOND, 0)
    }.timeInMillis

    private val WEEK = arrayOf("周日", "周一", "周二", "周三", "周四", "周五", "周六")

    /** 同桌面 formatSize：B / KB / MB / GB，一位小数。 */
    fun size(bytes: Long): String {
        if (bytes < 1024) return "$bytes B"
        val units = arrayOf("KB", "MB", "GB", "TB")
        var v = bytes / 1024.0; var i = 0
        while (v >= 1024 && i < units.size - 1) { v /= 1024; i++ }
        return if (v >= 100) String.format(Locale.ROOT, "%.0f %s", v, units[i]) else String.format(Locale.ROOT, "%.1f %s", v, units[i])
    }

    /** 同桌面 MessageItem：剩 N 秒 / 剩 N 分 / 剩 N.N 小时。 */
    fun eta(sec: Long?): String {
        if (sec == null || sec < 1) return ""
        return when {
            sec < 60 -> "剩 $sec 秒"
            sec < 3600 -> "剩 ${ceil(sec / 60.0).toLong()} 分"
            else -> String.format(Locale.ROOT, "剩 %.1f 小时", sec / 3600.0)
        }
    }

    fun os(os: String): String = when (os) {
        "windows" -> "Windows"; "macos" -> "macOS"; "linux" -> "Linux"; "android" -> "Android"; "ios" -> "iOS"
        else -> os
    }
}
