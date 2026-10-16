package fan.lancat.notify

import android.app.Notification
import android.content.Context
import androidx.core.app.NotificationCompat
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import fan.lancat.model.Direction
import fan.lancat.model.TransferState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

@RunWith(AndroidJUnit4::class)
class TransferNotificationTest {
    private val ctx: Context = ApplicationProvider.getApplicationContext()
    private fun meta(dir: Direction, local: String? = null) = TransferNotifier.Meta(
        peerId = "p1", peerName = "MacBook", messageId = "m1", name = "photo.jpg",
        size = 4_000_000, mime = "image/jpeg", localUri = local, dir = dir,
    )
    private fun titles(n: Notification) = (n.actions ?: emptyArray()).map { it.title.toString() }

    @Test @Config(sdk = [35])
    fun runningLegacyProgress() {
        val n = TransferNotifier.build(ctx, meta(Direction.OUT), TransferState.Running(1_000_000, 4_000_000, 2_000_000, 2), "Download/Lancat")!!
        assertEquals(1000, n.extras.getInt(Notification.EXTRA_PROGRESS_MAX))
        assertEquals(250, n.extras.getInt(Notification.EXTRA_PROGRESS))
        assertFalse(n.extras.getBoolean(Notification.EXTRA_PROGRESS_INDETERMINATE))
        assertTrue(n.flags and Notification.FLAG_ONGOING_EVENT != 0)
        assertEquals(listOf("暂停", "取消"), titles(n))
        val text = n.extras.getCharSequence(Notification.EXTRA_TEXT).toString()
        assertTrue(text, text.contains("977 KB / 3.8 MB") && text.contains("1.9 MB/s") && text.contains("剩余 2 秒"))
        assertEquals(Channels.XFER, n.channelId)
        // 低版本不写推广请求
        assertFalse(n.extras.getBoolean("android.requestPromotedOngoing"))
    }

    @Test @Config(sdk = [36])
    fun runningProgressStyleAndPromotedOnAndroid16() {
        val n = TransferNotifier.build(ctx, meta(Direction.OUT), TransferState.Running(2_000_000, 4_000_000, 1, null), "Download/Lancat")!!
        assertEquals("android.app.Notification\$ProgressStyle", n.extras.getString(Notification.EXTRA_TEMPLATE))
        assertEquals(500, n.extras.getInt(Notification.EXTRA_PROGRESS))
        assertTrue(n.extras.getBoolean("android.requestPromotedOngoing"))
        assertEquals("50%", NotificationCompat.getShortCriticalText(n))
    }

    @Test @Config(sdk = [36])
    fun incomingIsNotPromoted() {
        val n = TransferNotifier.build(ctx, meta(Direction.IN), TransferState.Running(1, 4_000_000, 1, 10), "Download/Lancat")!!
        assertFalse(n.extras.getBoolean("android.requestPromotedOngoing"))
    }

    @Test @Config(sdk = [35])
    fun pausedHasResume() {
        val n = TransferNotifier.build(ctx, meta(Direction.OUT), TransferState.Paused(1, 4_000_000), "Download/Lancat")!!
        assertEquals(listOf("继续", "取消"), titles(n))
        assertEquals(0, n.flags and Notification.FLAG_ONGOING_EVENT)
    }

    @Test @Config(sdk = [35])
    fun doneIncomingHasOpenAndReveal() {
        val n = TransferNotifier.build(ctx, meta(Direction.IN, "content://media/external/downloads/42"), TransferState.Done, "Download/Lancat")!!
        assertEquals(listOf("打开", "在文件夹中显示"), titles(n))
        assertTrue(n.flags and Notification.FLAG_AUTO_CANCEL != 0)
        assertEquals("已收到 photo.jpg", n.extras.getCharSequence(Notification.EXTRA_TITLE).toString())
        assertEquals(0, n.extras.getInt(Notification.EXTRA_PROGRESS_MAX))
    }

    @Test @Config(sdk = [35])
    fun doneOutgoingNoActions() {
        val n = TransferNotifier.build(ctx, meta(Direction.OUT), TransferState.Done, "Download/Lancat")!!
        assertTrue(titles(n).isEmpty())
    }

    @Test @Config(sdk = [35])
    fun failedRetryable() {
        val n = TransferNotifier.build(ctx, meta(Direction.OUT), TransferState.Failed("对方已离线", true), "Download/Lancat")!!
        assertEquals(listOf("重试"), titles(n))
        val n2 = TransferNotifier.build(ctx, meta(Direction.OUT), TransferState.Failed("磁盘已满", false), "Download/Lancat")!!
        assertTrue(titles(n2).isEmpty())
    }

    @Test @Config(sdk = [35])
    fun queuedIndeterminateAndCancelledRemoved() {
        val n = TransferNotifier.build(ctx, meta(Direction.OUT), TransferState.Queued, "Download/Lancat")
        assertNotNull(n)
        assertTrue(n!!.extras.getBoolean(Notification.EXTRA_PROGRESS_INDETERMINATE))
        assertNull(TransferNotifier.build(ctx, meta(Direction.OUT), TransferState.Cancelled, "Download/Lancat"))
    }
}
