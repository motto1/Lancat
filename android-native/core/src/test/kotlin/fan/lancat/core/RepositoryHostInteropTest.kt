package fan.lancat.core

import androidx.test.core.app.ApplicationProvider
import fan.lancat.model.Body
import fan.lancat.model.CoreEvent
import fan.lancat.model.Direction
import fan.lancat.model.TransferState
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.BufferedReader
import java.io.File
import java.io.InputStreamReader
import java.nio.file.Files
import java.util.Collections
import java.util.concurrent.TimeUnit

/**
 * The real [LancatCoreRepository] (the exact Kotlin the app runs, generated UniFFI bindings + JNA)
 * against `lancat-cli serve` on 127.0.0.1, with the host build of liblancat_ffi.
 * Isolated: loopback only, no mDNS / UDP, ports 47861/47862. Skipped unless both are given:
 *   gradlew :core:testDebugUnitTest -Plancat.ffiHostDir=G:\lancat-work\target-ffi\debug -Plancat.cli=...\lancat-cli.exe
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [33])   // < 34：绑定用 JNA Cleaner（Robolectric 没有 android.system.SystemCleaner 的实现）
class RepositoryHostInteropTest {
    private var cli: Process? = null
    private val cliLines = Collections.synchronizedList(mutableListOf<String>())

    @After
    fun tearDown() {
        cli?.let { p ->
            runCatching { p.outputStream.write("quit\n".toByteArray()); p.outputStream.flush() }
            p.waitFor(2, TimeUnit.SECONDS)
            p.destroyForcibly()
        }
    }

    private fun cliCmd(l: String) {
        cli!!.outputStream.write((l + "\n").toByteArray())
        cli!!.outputStream.flush()
    }

    private suspend fun cliWait(vararg needles: String) {
        withTimeout(30_000) {
            while (cliLines.toList().none { l -> needles.all { l.contains(it) } }) kotlinx.coroutines.delay(100)
        }
    }

    private suspend fun waitFor(what: () -> Boolean) {
        withTimeout(10_000) { while (!what()) kotlinx.coroutines.delay(50) }
    }

    @Test
    fun repositoryTalksToCoreCli() = runBlocking {
        val exe = System.getProperty("lancat.cli")
        assumeTrue("set -Plancat.cli and -Plancat.ffiHostDir", exe != null && System.getProperty("jna.library.path") != null)
        val tmp = Files.createTempDirectory("lc-repo").toFile()
        val cliData = File(tmp, "cli-data").apply { mkdirs() }
        val cliRecv = File(tmp, "cli-recv").apply { mkdirs() }
        val pb = ProcessBuilder(
            exe, "serve", "--port", "47861", "--udp-port", "47861", "--no-mdns", "--no-udp", "--name", "repo-cli",
            "--data", cliData.path, "--recv", cliRecv.path,
        )
        pb.environment()["LANCAT_BIND_IP"] = "127.0.0.1"
        pb.environment()["RUST_LOG"] = "warn"
        pb.redirectError(ProcessBuilder.Redirect.INHERIT)
        cli = pb.start()
        Thread {
            BufferedReader(InputStreamReader(cli!!.inputStream, Charsets.UTF_8)).lineSequence().forEach { cliLines += it }
        }.apply { isDaemon = true }.start()

        org.robolectric.shadows.ShadowLog.stream = System.out
        val ctx = ApplicationProvider.getApplicationContext<android.content.Context>()
        val repo = LancatCoreRepository(ctx) {
            it.copy(port = 47862u, udpPort = 47862u, enableMdns = false, enableUdp = false, bindIp = "127.0.0.1", portFallback = false, initLogging = false)
        }
        val events = Collections.synchronizedList(mutableListOf<CoreEvent>())
        val collector = CoroutineScope(Dispatchers.Default).launch { repo.events.collect { events += it } }

        withTimeout(20_000) { repo.self.first { it != null } }
        println("self: ${repo.self.value}")

        // add by address, as the UI's manual-add does
        withTimeout(20_000) {
            while (repo.peers.value.none { it.name == "repo-cli" && it.online }) {
                repo.addManualPeer("127.0.0.1:47861")
                kotlinx.coroutines.delay(300)
            }
        }
        val peer = repo.peers.value.first { it.name == "repo-cli" }
        println("peer: $peer")
        assertEquals("127.0.0.1", peer.ip)

        // ---- out: text, delivered
        val conv = repo.messages(peer.id)
        repo.sendText(listOf(peer.id), "你好 from repository")
        cliWait("\"type\":\"message\"", "你好 from repository")
        withTimeout(20_000) {
            conv.first { l -> l.any { it.dir == Direction.OUT && (it.body as? Body.Text)?.text == "你好 from repository" && it.delivered } }
        }
        println("ok: text out delivered")

        // ---- out: sticker
        repo.sendSticker(listOf(peer.id), "1f44d")
        cliWait("\"kind\":\"emoji\"", "1f44d")
        withTimeout(10_000) { conv.first { l -> l.any { (it.body as? Body.Sticker)?.emojiId == "1f44d" } } }
        println("ok: sticker out")

        // ---- out: file (plain path, as after the content:// copy) with progress -> Done
        val src = File(tmp, "repo-out.bin")
        src.writeBytes(ByteArray(3 * 1024 * 1024 + 17) { (it * 31 + 7).toByte() })
        repo.sendFiles(listOf(peer.id), listOf(src.toURI().toString()))
        val outFile = withTimeout(60_000) {
            conv.first { l -> l.any { (it.body as? Body.File)?.let { b -> b.name == "repo-out.bin" && b.state == TransferState.Done } == true } }
        }.first { (it.body as? Body.File)?.name == "repo-out.bin" }
        waitFor { events.any { it is CoreEvent.TransferProgress && it.messageId == outFile.id && it.state == TransferState.Done } }
        assertTrue(File(cliRecv, "repo-out.bin").readBytes().contentEquals(src.readBytes()))
        println("ok: file out done, bytes match; progress events: ${events.count { it is CoreEvent.TransferProgress && it.messageId == outFile.id }}")

        // ---- in: text -> MessageReceived + conversation
        cliCmd("text ${repo.self.value!!.id} hello repo")   // id: the Robolectric device name has a space
        withTimeout(20_000) { conv.first { l -> l.any { it.dir == Direction.IN && (it.body as? Body.Text)?.text == "hello repo" } } }
        waitFor { events.any { it is CoreEvent.MessageReceived && (it.message.body as? Body.Text)?.text == "hello repo" } }
        println("ok: text in + MessageReceived")

        // ---- in: file -> Done with a local uri
        val src2 = File(tmp, "repo-in.bin").apply { writeBytes(ByteArray(1024 * 1024) { (it * 13).toByte() }) }
        cliCmd("file ${repo.self.value!!.id} ${src2.path}")
        val inFile = withTimeout(60_000) {
            conv.first { l -> l.any { it.dir == Direction.IN && (it.body as? Body.File)?.state == TransferState.Done } }
        }.first { it.dir == Direction.IN && it.body is Body.File }
        val b = inFile.body as Body.File
        println("ok: file in done -> ${b.localUri}")
        // Uri.fromFile on a Windows host (Robolectric) yields file://C%3A%5C...; on a device it is file:///storage/...
        val u = b.localUri!!
        val local = if (u.startsWith("file://") && !u.startsWith("file:///")) File(java.net.URLDecoder.decode(u.removePrefix("file://"), "UTF-8")) else File(java.net.URI(u))
        assertTrue(local.readBytes().contentEquals(src2.readBytes()))

        // ---- history paging, unread / markRead, clear
        val older = repo.loadOlder(peer.id, conv.value.last().timeMs, 2)
        assertTrue(older >= 1)
        repo.markRead(peer.id)
        assertEquals(0, repo.peers.value.first { it.id == peer.id }.unread)
        repo.clearHistory(peer.id)
        withTimeout(10_000) { conv.first { it.isEmpty() } }
        println("ok: paging / markRead / clear")

        collector.cancel()
        repo.shutdown()
    }
}
