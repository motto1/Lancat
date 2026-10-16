package fan.lancat.share

import android.content.Context
import android.net.Uri
import fan.lancat.model.LancatRepository
import fan.lancat.service.LancatService
import java.util.concurrent.atomic.AtomicBoolean

/**
 * 交给分享半屏的仓库包装：其余调用原样转发，只有「发送」改道走 LancatService，
 * 这样无论 :ui 的 ShareSheetContent 自己怎么调 sendFiles / sendText，uri 授权都会先转给前台服务。
 *
 * 文字分享：契约里 ShareSheetContent 只拿到 uris。约定半屏选好设备后总是调用 sendFiles(peers, uris)
 * （uris 可能为空），这里把本次分享附带的文字一并发出（只发一次）。
 */
class ShareRoutingRepository(
    private val base: LancatRepository,
    private val context: Context,
    private val payload: SharePayload,
) : LancatRepository by base {
    private val textSent = AtomicBoolean(false)

    override suspend fun sendFiles(peerIds: List<String>, uris: List<String>) {
        if (peerIds.isEmpty()) return
        val text = if (textSent.compareAndSet(false, true)) payload.text else null
        val list = uris.mapNotNull { runCatching { Uri.parse(it) }.getOrNull() }
            .filter { ShareIntentParser.isAcceptable(it, context.packageName) }
        LancatService.send(context, peerIds, list, text)
    }

    override suspend fun sendText(peerIds: List<String>, text: String) {
        if (peerIds.isEmpty()) return
        textSent.set(true)
        LancatService.send(context, peerIds, emptyList(), text)
    }
}
