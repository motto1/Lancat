package fan.lancat.share

import android.content.ClipData
import android.content.Intent
import android.net.Uri
import androidx.core.content.pm.ShortcutManagerCompat
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

@RunWith(AndroidJUnit4::class)
@Config(sdk = [35])
class ShareIntentParserTest {
    private val own = "app.lancat"
    private val img = Uri.parse("content://com.example.provider/images/1")
    private val img2 = Uri.parse("content://com.example.provider/images/2")

    @Test fun singleStream() {
        val i = Intent(Intent.ACTION_SEND).setType("image/png").putExtra(Intent.EXTRA_STREAM, img)
        val p = ShareIntentParser.parse(i, own)
        assertEquals(listOf(img), p.uris)
        assertNull(p.text)
        assertEquals("image/png", p.mime)
        assertNull(p.shortcutPeerId)
    }

    @Test fun multipleStreamsDedupWithClipData() {
        val i = Intent(Intent.ACTION_SEND_MULTIPLE).setType("*/*")
            .putParcelableArrayListExtra(Intent.EXTRA_STREAM, arrayListOf(img, img2))
        i.clipData = ClipData("x", arrayOf("*/*"), ClipData.Item(img)).apply { addItem(ClipData.Item(img2)) }
        assertEquals(listOf(img, img2), ShareIntentParser.parse(i, own).uris)
    }

    @Test fun clipDataOnly() {
        val i = Intent(Intent.ACTION_SEND).setType("image/*")
        i.clipData = ClipData.newRawUri("x", img)
        assertEquals(listOf(img), ShareIntentParser.parse(i, own).uris)
    }

    @Test fun textOnly() {
        val i = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, "https://example.com 你好")
            .putExtra(Intent.EXTRA_SUBJECT, "标题")
        val p = ShareIntentParser.parse(i, own)
        assertTrue(p.uris.isEmpty())
        assertEquals("https://example.com 你好", p.text)
        assertEquals("标题", p.subject)
    }

    @Test fun streamWithCaption() {
        val i = Intent(Intent.ACTION_SEND).setType("image/jpeg").putExtra(Intent.EXTRA_STREAM, img).putExtra(Intent.EXTRA_TEXT, "说明")
        val p = ShareIntentParser.parse(i, own)
        assertEquals(listOf(img), p.uris)
        assertEquals("说明", p.text)
    }

    @Test fun rejectsFileSchemeAndOwnProvider() {
        val i = Intent(Intent.ACTION_SEND_MULTIPLE).setType("*/*").putParcelableArrayListExtra(
            Intent.EXTRA_STREAM,
            arrayListOf(
                Uri.parse("file:///data/data/app.lancat/shared_prefs/x.xml"),
                Uri.parse("content://app.lancat.files/share/abc/secret"),
                Uri.parse("content://evil/../x/.."),
                img,
            ),
        )
        val p = ShareIntentParser.parse(i, own)
        assertEquals(listOf(img), p.uris)
        assertEquals(3, p.rejected)
    }

    @Test fun directShareShortcutId() {
        val i = Intent(Intent.ACTION_SEND).setType("image/png").putExtra(Intent.EXTRA_STREAM, img)
            .putExtra(ShortcutManagerCompat.EXTRA_SHORTCUT_ID, "peer_node-123")
        assertEquals("node-123", ShareIntentParser.parse(i, own).shortcutPeerId)
        i.putExtra(ShortcutManagerCompat.EXTRA_SHORTCUT_ID, "other")
        assertNull(ShareIntentParser.parse(i, own).shortcutPeerId)
    }

    @Test fun emptyAndWrongAction() {
        assertTrue(ShareIntentParser.parse(Intent(Intent.ACTION_SEND).setType("text/plain"), own).isEmpty)
        assertTrue(ShareIntentParser.parse(Intent(Intent.ACTION_VIEW, img), own).isEmpty)
        assertTrue(ShareIntentParser.parse(null, own).isEmpty)
    }
}
