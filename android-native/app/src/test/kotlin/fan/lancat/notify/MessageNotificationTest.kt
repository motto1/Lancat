package fan.lancat.notify

import android.app.Notification
import android.content.Context
import androidx.core.app.NotificationCompat
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import fan.lancat.model.Body
import fan.lancat.model.Peer
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
@Config(sdk = [35])
class MessageNotificationTest {
    private val ctx: Context = ApplicationProvider.getApplicationContext()
    private val peer = Peer("node-1", "Motto 的 MacBook", "macos", "192.168.1.5", true, null, 2, null, false)

    @Test fun messagingStyleConversation() {
        val lines = listOf(
            MessageNotifier.Line("你好", 1_000, false),
            MessageNotifier.Line("[文件] a.pdf", 2_000, false),
            MessageNotifier.Line("收到", 3_000, true),
        )
        val n = MessageNotifier.build(ctx, peer, lines)
        val style = NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(n)
        assertNotNull(style)
        style!!
        assertEquals(3, style.messages.size)
        assertEquals("Motto 的 MacBook", style.messages[0].person!!.name.toString())
        assertEquals("node-1", style.messages[0].person!!.key)
        assertNull(style.messages[2].person)            // 自己发的
        assertFalse(style.isGroupConversation)
        assertEquals("peer_node-1", n.shortcutId)
        assertEquals(Channels.MSG, n.channelId)
        assertEquals(Notification.CATEGORY_MESSAGE, n.category)
        assertEquals(2, n.number)
    }

    @Test fun replyAndMarkReadActions() {
        val n = MessageNotifier.build(ctx, peer, listOf(MessageNotifier.Line("hi", 1, false)))
        assertEquals(2, NotificationCompat.getActionCount(n))
        val reply = NotificationCompat.getAction(n, 0)!!
        assertEquals("回复", reply.title.toString())
        assertEquals(NotificationCompat.Action.SEMANTIC_ACTION_REPLY, reply.semanticAction)
        assertEquals(MessageNotifier.KEY_REPLY, reply.remoteInputs!!.single().resultKey)
        assertFalse(reply.showsUserInterface)
        val read = NotificationCompat.getAction(n, 1)!!
        assertEquals("标为已读", read.title.toString())
        assertEquals(NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ, read.semanticAction)
        assertTrue(read.remoteInputs.isNullOrEmpty())
    }

    @Test fun bodyTexts() {
        assertEquals("hi", MessageNotifier.bodyText(Body.Text("hi")))
        assertEquals("[表情]", MessageNotifier.bodyText(Body.Sticker("1f600")))
        assertEquals("[文件] a.zip", MessageNotifier.bodyText(Body.File("t", "a.zip", 1, null, null, TransferState.Queued)))
    }
}
