package fan.lancat.service

import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import fan.lancat.model.TransferState
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

@RunWith(AndroidJUnit4::class)
@Config(sdk = [35])
class ServicePolicyTest {
    private val ctx: Context = ApplicationProvider.getApplicationContext()

    @After fun reset() = ServiceController.resetForTest()

    @Test fun types() {
        assertEquals(ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE, FgsPolicy.types(true, 35))
        assertEquals(ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC, FgsPolicy.types(false, 35))
        assertEquals(0, FgsPolicy.types(true, 28))
    }

    @Test fun shouldRun() {
        val s = ServiceController.State()
        assertFalse(FgsPolicy.shouldRun(s, 100))
        assertTrue(FgsPolicy.shouldRun(s.copy(keepAlive = true), 100))
        assertTrue(FgsPolicy.shouldRun(s.copy(active = 1), 100))
        assertTrue(FgsPolicy.shouldRun(s.copy(sending = 1), 100))
        assertTrue(FgsPolicy.shouldRun(s.copy(graceUntilMs = 200), 100))
        assertFalse(FgsPolicy.shouldRun(s.copy(graceUntilMs = 50), 100))
    }

    @Test fun transferTracking() {
        ServiceController.onTransfer("a", TransferState.Queued)
        ServiceController.onTransfer("b", TransferState.Running(1, 10, 1, 1))
        assertEquals(2, ServiceController.state.value.active)
        ServiceController.onTransfer("a", TransferState.Done)
        ServiceController.onTransfer("b", TransferState.Failed("x", true))
        assertEquals(0, ServiceController.state.value.active)
        assertTrue(ServiceController.state.value.graceUntilMs > 0)   // 可重试失败保留授权一段时间
    }

    @Test fun sendIntentForwardsGrant() {
        val u1 = Uri.parse("content://com.example/1")
        val u2 = Uri.parse("content://com.example/2")
        val i = LancatService.sendIntent(ctx, listOf("p1"), listOf(u1, u2), "hi")
        assertEquals(LancatService.ACTION_SEND, i.action)
        assertTrue(i.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION != 0)
        assertEquals(2, i.clipData!!.itemCount)
        assertEquals(u2, i.clipData!!.getItemAt(1).uri)
        assertEquals(arrayListOf("p1"), i.getStringArrayListExtra(LancatService.EXTRA_PEERS))
        val t = LancatService.sendIntent(ctx, listOf("p1"), emptyList(), "hi")
        assertEquals(0, t.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION)
    }
}
