package fan.lancat.ui.components

import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp

/**
 * 线性图标，路径逐字取自桌面 src/components/Icon.svelte（24×24 viewBox，stroke 1.7，圆头圆角）。
 * 桌面没有的几枚（back/copy/forward/keyboard/more/…）按同一风格补画。
 */
object LancatIcons {
    private val cache = HashMap<String, ImageVector>()

    private val paths: Map<String, String> = mapOf(
        "smile" to "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z M8.5 14.5s1.2 1.8 3.5 1.8 3.5-1.8 3.5-1.8 M9 9.5h.01 M15 9.5h.01",
        "clip" to "M20 11.5 12.4 19a5 5 0 0 1-7.1-7.1l7.9-7.8a3.3 3.3 0 0 1 4.7 4.7l-7.9 7.8a1.7 1.7 0 0 1-2.4-2.4l7.3-7.2",
        "send" to "M5 12h13 M12.5 5.5 19 12l-6.5 6.5",
        "pin" to "M9 4h6 M10 4v5l-3 4h10l-3-4V4 M12 13v7",
        "gear" to "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z",
        "grid" to "M4 4h6v6H4z M14 4h6v6h-6z M4 14h6v6H4z M14 14h6v6h-6z",
        "doc" to "M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z M14 3v5h5",
        "folder" to "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
        "image" to "M4 5h16v14H4z M4 16l5-5 4 4 2-2 5 5 M15.5 9.5h.01",
        "film" to "M4 5h16v14H4z M8 5v14 M16 5v14 M4 9h4 M4 15h4 M16 9h4 M16 15h4",
        "music" to "M9 18V6l11-2v12 M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z M20 16a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z",
        "archive" to "M4 7h16v13H4z M3 3h18v4H3z M10 11h4",
        "check" to "M5 12.5 10 17.5 19 7",
        "checks" to "M2.5 12.5 7 17 16 7 M10.5 16l1 1 9-10",
        "clock" to "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z M12 7v5l3 2",
        "alert" to "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z M12 8v5 M12 16h.01",
        "retry" to "M4 12a8 8 0 0 1 14-5.3L20 9 M20 4v5h-5 M20 12a8 8 0 0 1-14 5.3L4 15 M4 20v-5h5",
        "reveal" to "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z M12 10v6 M9 13l3 3 3-3",
        "search" to "M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14Z M20 20l-4-4",
        "plus" to "M12 5v14 M5 12h14",
        "x" to "M6 6l12 12 M18 6 6 18",
        "external" to "M14 4h6v6 M20 4l-9 9 M18 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4",
        "pause" to "M9 6v12 M15 6v12",
        "play" to "M8 5.5v13l10.5-6.5z",
        "radar" to "M12 12m-1 0a1 1 0 1 0 2 0a1 1 0 1 0-2 0 M7.8 16.2a6 6 0 0 1 0-8.4 M16.2 7.8a6 6 0 0 1 0 8.4 M5 19a10 10 0 0 1 0-14 M19 5a10 10 0 0 1 0 14",
        "trash" to "M4 7h16 M10 11v6 M14 11v6 M6 7l1 13h10l1-13 M9 7V4h6v3",
        "block" to "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z M5.6 5.6l12.8 12.8",
        "clock2" to "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z M12 7v5h4",
        "heart" to "M12 20s-7-4.4-7-10a4 4 0 0 1 7-2.6A4 4 0 0 1 19 10c0 5.6-7 10-7 10Z",
        "paw" to "M8 9a1.8 2.2 0 1 0 0-.1Z M16 9a1.8 2.2 0 1 0 0-.1Z M5 13a1.6 2 0 1 0 0-.1Z M19 13a1.6 2 0 1 0 0-.1Z M12 13c-3 0-5 3-5 5 0 1.5 1.5 2 2.5 1.6 1-.4 1.6-.6 2.5-.6s1.5.2 2.5.6c1 .4 2.5-.1 2.5-1.6 0-2-2-5-5-5Z",
        "flag" to "M5 21V4 M5 4h11l-2 4 2 4H5",
        "food" to "M7 3v8 M5 3v5a2 2 0 0 0 4 0V3 M7 11v10 M17 21V3c-2 0-4 2-4 6v4h4",
        "ball" to "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z M3.5 9.5c5 1 12 1 17 0 M3.5 14.5c5-1 12-1 17 0 M12 3c-3 5-3 13 0 18 M12 3c3 5 3 13 0 18",
        "bulb" to "M9 18h6 M10 21h4 M12 3a6 6 0 0 0-3.5 10.9c.6.5 1 1.2 1 2.1h5c0-.9.4-1.6 1-2.1A6 6 0 0 0 12 3Z",
        "sticker" to "M20 12V8a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v8a4 4 0 0 0 4 4h4 M20 12l-8 8 M20 12h-4a4 4 0 0 0-4 4v4",
        "hand" to "M8 13V5.5a1.5 1.5 0 0 1 3 0V12 M11 11V4.5a1.5 1.5 0 0 1 3 0V12 M14 11.5V6a1.5 1.5 0 0 1 3 0v7.5 M8 12.5 6.8 11a1.6 1.6 0 0 0-2.5 2l3.2 4.6A6 6 0 0 0 12.4 20H13a5 5 0 0 0 5-5v-3",
        "upload" to "M12 16V4 M7 9l5-5 5 5 M4 20h16",
        // —— 以下为移动端补画，风格同上 ——
        "back" to "M15 5l-7 7 7 7",
        "chevron" to "M9 6l6 6-6 6",
        "copy" to "M9 9h10v11H9z M5 15V4h10",
        "forward" to "M14 5l6 6-6 6 M20 11H10a6 6 0 0 0-6 6v2",
        "keyboard" to "M3 7h18v11H3z M7 11h.01 M11 11h.01 M15 11h.01 M17 11h.01 M7 14.5h10",
        "more" to "M12 6h.01 M12 12h.01 M12 18h.01",
        "selectall" to "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z M8 12.5l3 3 5-6",
        "clipboard" to "M9 4h6v3H9z M9 5H6v16h12V5h-3 M9 12h6 M9 16h4",
        "info" to "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z M12 11v5 M12 8h.01",
        "monitor" to "M3 5h18v11H3z M8 20h8 M12 16v4",
        "phone" to "M8 3h8v18H8z M11 18h2",
        "relay" to "M5 12h4 M15 12h4 M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z M3 12h.01 M21 12h.01",
        "unblock" to "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z M8.5 12.5l2.5 2.5 4.5-5",
        "edit" to "M4 20h4L19 9l-4-4L4 16z M13.5 6.5l4 4",
        "down" to "M6 9l6 6 6-6",
    )

    fun get(name: String): ImageVector = cache.getOrPut(name) { build(name, paths[name] ?: paths.getValue("doc")) }

    private fun build(name: String, d: String): ImageVector {
        val b = ImageVector.Builder(name = name, defaultWidth = 24.dp, defaultHeight = 24.dp, viewportWidth = 24f, viewportHeight = 24f)
        // 与 Icon.svelte 一样按 " M" 拆成多条 path
        val segs = d.split(" M").mapIndexed { i, s -> if (i == 0) s else "M$s" }
        val filled = name == "play"
        for (seg in segs) {
            b.addPath(
                pathData = addPathNodes(seg),
                fill = if (filled) SolidColor(Color.Black) else null,
                stroke = SolidColor(Color.Black),
                strokeLineWidth = 1.7f,
                strokeLineCap = StrokeCap.Round,
                strokeLineJoin = StrokeJoin.Round,
            )
        }
        return b.build()
    }

    val Smile get() = get("smile")
    val Clip get() = get("clip")
    val Send get() = get("send")
    val Gear get() = get("gear")
    val Grid get() = get("grid")
    val Back get() = get("back")
    val Search get() = get("search")
    val Plus get() = get("plus")
    val X get() = get("x")
    val Keyboard get() = get("keyboard")
}
