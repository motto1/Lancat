package fan.lancat.notify

import kotlin.math.roundToLong

/** 与桌面 formatBytes 一致：<10 保留一位小数，否则取整 */
object Format {
    private val UNITS = arrayOf("B", "KB", "MB", "GB", "TB")

    fun bytes(n: Long): String {
        if (n < 1024) return "$n B"
        var v = n.toDouble()
        var i = 0
        while (v >= 1024 && i < UNITS.size - 1) { v /= 1024; i++ }
        return if (v < 10) "${"%.1f".format(java.util.Locale.ROOT, v)} ${UNITS[i]}" else "${v.roundToLong()} ${UNITS[i]}"
    }

    fun speed(bps: Long): String = "${bytes(bps.coerceAtLeast(0))}/s"

    fun eta(sec: Long): String = when {
        sec < 0 -> ""
        sec < 60 -> "剩余 $sec 秒"
        sec < 3600 -> "剩余 ${sec / 60} 分 ${sec % 60} 秒"
        else -> "剩余 ${sec / 3600} 小时 ${sec % 3600 / 60} 分"
    }

    /** 状态栏 chip 的极短文字（≤7 字符才完整显示） */
    fun percent(bytes: Long, total: Long): String =
        if (total <= 0) "…" else "${(bytes * 100 / total).coerceIn(0, 100)}%"

    fun permille(bytes: Long, total: Long): Int =
        if (total <= 0) 0 else (bytes * 1000 / total).toInt().coerceIn(0, 1000)
}
