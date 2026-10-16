package fan.lancat.share

import android.content.ContentResolver
import android.content.Intent
import android.net.Uri
import androidx.core.content.IntentCompat
import androidx.core.content.pm.ShortcutManagerCompat
import fan.lancat.shortcut.ShortcutPublisher

/** 一次分享的内容。uris 只含已通过安全检查的 content://。 */
data class SharePayload(
    val uris: List<Uri>,
    val text: String?,
    val subject: String?,
    val mime: String?,
    /** 从 Direct Share 进来时对应的设备 id（快捷方式 id 解析出来的） */
    val shortcutPeerId: String?,
    /** 被拒绝的条目数（file://、指向本应用私有 provider 的 uri 等），用于提示 */
    val rejected: Int = 0,
) {
    val isEmpty: Boolean get() = uris.isEmpty() && text.isNullOrBlank()
}

/**
 * 解析 ACTION_SEND / ACTION_SEND_MULTIPLE。纯函数，Robolectric 单测覆盖。
 *
 * - SEND：EXTRA_STREAM（单个 Uri），否则 ClipData 里的 uri；EXTRA_TEXT 作为文字（可与文件同时存在，如图片 + 说明）
 * - SEND_MULTIPLE：EXTRA_STREAM（ArrayList<Uri>）+ ClipData，去重
 * - 只接受 content://。file:// 一律拒绝（Android 7 起发送方本就不该给 file://，接受它会被诱导读出本应用私有文件）；
 *   authority 属于本应用（如 FileProvider app.lancat.files）的也拒绝，防止外部应用借分享把我们的私有文件发出去
 * - Direct Share：Intent.EXTRA_SHORTCUT_ID（ShortcutManagerCompat.EXTRA_SHORTCUT_ID）→ 设备 id
 */
object ShareIntentParser {
    fun parse(intent: Intent?, ownPackage: String): SharePayload {
        if (intent == null) return SharePayload(emptyList(), null, null, null, null)
        val action = intent.action
        if (action != Intent.ACTION_SEND && action != Intent.ACTION_SEND_MULTIPLE) {
            return SharePayload(emptyList(), null, null, intent.type, null)
        }
        val raw = LinkedHashSet<Uri>()
        if (action == Intent.ACTION_SEND) {
            IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)?.let { raw += it }
        } else {
            IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)?.let { raw += it }
        }
        // ClipData：很多应用只放在这里（或两处都放），合并去重
        intent.clipData?.let { clip ->
            for (i in 0 until clip.itemCount) clip.getItemAt(i).uri?.let { raw += it }
        }

        var rejected = 0
        val uris = raw.filter { u ->
            val ok = isAcceptable(u, ownPackage)
            if (!ok) rejected++
            ok
        }

        var text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()
        if (text.isNullOrBlank() && uris.isEmpty()) {
            // 少数应用只放 ClipData 文本
            text = intent.clipData?.let { c -> (0 until c.itemCount).mapNotNull { c.getItemAt(it).text?.toString() }.joinToString("\n") }
        }
        text = text?.takeIf { it.isNotBlank() }

        val shortcutId = intent.getStringExtra(ShortcutManagerCompat.EXTRA_SHORTCUT_ID)
        return SharePayload(
            uris = uris,
            text = text,
            subject = intent.getStringExtra(Intent.EXTRA_SUBJECT),
            mime = intent.type,
            shortcutPeerId = shortcutId?.let { ShortcutPublisher.peerIdOf(it) },
            rejected = rejected,
        )
    }

    fun isAcceptable(u: Uri, ownPackage: String): Boolean {
        if (u.scheme != ContentResolver.SCHEME_CONTENT) return false
        val auth = u.authority ?: return false
        if (auth == ownPackage || auth.startsWith("$ownPackage.")) return false
        val path = u.encodedPath ?: return true
        if (path.contains("/../") || path.endsWith("/..")) return false
        return true
    }
}
