package dev.sail.app.feature.reader

import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.BinaryBody
import dev.sail.app.feature.library.ReadingAnchor
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class SegmentTest {
    private fun blocks(vararg text: String) = text.map { ReaderBlock("p", it) }

    @Test
    fun `splits sentences like the web reader`() {
        val segments = segmentSection(blocks("One. Two! “Three?” Four"), section = 2)
        assertEquals(listOf("One. ", "Two! ", "“Three?” ", "Four"), segments.map { it.text })
        assertEquals(ReadingAnchor(2, 0, 5), segments[1].anchor)
        assertEquals(10, segments[1].end)
    }

    @Test
    fun `long sentences are cut at a space past half the budget`() {
        val text = "word ".repeat(30).trim()
        val segments = segmentSection(blocks(text), section = 0, budget = 40)
        assertTrue(segments.all { it.text.length <= 40 })
        assertTrue(segments.dropLast(1).all { it.text.endsWith(" ") })
        assertEquals(text, segments.joinToString("") { it.text })
    }

    @Test
    fun `never splits a surrogate pair and skips blank blocks`() {
        val segments = segmentSection(blocks("a😀b", "", "   "), section = 0, budget = 2)
        assertEquals(listOf("a", "😀", "b"), segments.map { it.text })
        assertTrue(segments.all { it.block == 0 })
    }

    @Test
    fun `finds the sentence at a place and under a press`() {
        val segments = segmentSection(blocks("One. Two.", "Three."), section = 0)
        assertEquals(1, segmentIndexAt(segments, ReadingAnchor(0, 0, 6)))
        assertEquals(2, segmentIndexAt(segments, ReadingAnchor(0, 1, 0)))
        assertEquals(3, segmentIndexAt(segments, ReadingAnchor(0, 1, 6)))
        assertEquals(0, segmentAt(segments, block = 0, char = 2))
        assertEquals(1, segmentAt(segments, block = 0, char = 8))
        // Past the end of a block's text (pressing to the right of the last line).
        assertEquals(1, segmentAt(segments, block = 0, char = 99))
        assertNull(segmentAt(segments, block = 5, char = 0))
    }

    @Test
    fun `reads the speech response`() {
        val ok = speechOutcome(ApiResult.Ok(BinaryBody(ByteArray(4), mapOf("x-sample-rate" to "24000"))))
        assertTrue(ok is SpeechOutcome.Audio && ok.pcm.sampleRate == 24000)
        assertTrue(speechOutcome(ApiResult.Ok(BinaryBody(ByteArray(3), mapOf("x-sample-rate" to "24000")))) is SpeechOutcome.Failed)
        assertTrue(speechOutcome(ApiResult.Ok(BinaryBody(ByteArray(4), emptyMap()))) is SpeechOutcome.Failed)
        assertEquals(SpeechOutcome.Busy, speechOutcome(ApiResult.Err(429, "busy")))
        assertEquals(SpeechOutcome.Failed("The host voice is not running."), speechOutcome(ApiResult.Err(503, "The host voice is not running.")))
    }
}

/** Plays each segment for 100 ms of virtual time; pause holds it. */
class FakeSink : PcmSink {
    val played = mutableListOf<Int>()
    val paused = MutableStateFlow(false)
    var stops = 0
    var releases = 0

    override suspend fun play(pcm: Pcm) {
        played += pcm.sampleRate // the fake speaker puts the segment's text length here
        delay(50)
        paused.first { !it }
        delay(50)
    }

    override fun pause() {
        paused.value = true
    }

    override fun resume() {
        paused.value = false
    }

    override fun stop() {
        stops++
        paused.value = false
    }

    override fun release() {
        releases++
    }
}

class FakeSpeaker : Speaker {
    val requests = mutableListOf<Triple<String, String, Float>>()
    var busyFirst = 0
    var fail: String? = null

    override suspend fun speak(text: String, voice: String, speed: Float): SpeechOutcome {
        requests += Triple(text, voice, speed)
        delay(10)
        if (busyFirst > 0) {
            busyFirst--
            return SpeechOutcome.Busy
        }
        fail?.let { return SpeechOutcome.Failed(it) }
        return SpeechOutcome.Audio(Pcm(ByteArray(2), text.length + 1000))
    }
}

@OptIn(ExperimentalCoroutinesApi::class)
class NarratorTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private val segments = segmentSection(listOf(ReaderBlock("p", "A. Bb. Ccc. Dddd.")), section = 0)

    private fun narrator(speaker: FakeSpeaker, sink: FakeSink, started: MutableList<Int>, ended: () -> Unit = {}) =
        Narrator(CoroutineScope(dispatcher), speaker, sink, onStarted = { started += it }, onEnded = ended)

    @Test
    fun `reads in order, fetching ahead one request at a time`() = runTest(dispatcher) {
        val speaker = FakeSpeaker()
        val sink = FakeSink()
        val started = mutableListOf<Int>()
        var ended = false
        val n = narrator(speaker, sink, started) { ended = true }
        n.load(segments, 0, "af_heart", 1.2f, autoplay = true)
        advanceUntilIdle()

        assertEquals(listOf(0, 1, 2, 3), started)
        assertEquals(segments.map { it.text.length + 1000 }, sink.played)
        assertEquals(segments.map { it.text }, speaker.requests.map { it.first })
        assertTrue(speaker.requests.all { it.second == "af_heart" && it.third == 1.2f })
        assertTrue(ended)
    }

    @Test
    fun `retries while the host voice is busy`() = runTest(dispatcher) {
        val speaker = FakeSpeaker().apply { busyFirst = 2 }
        val started = mutableListOf<Int>()
        narrator(speaker, FakeSink(), started).load(segments.take(1), 0, "v", 1f, autoplay = true)
        advanceUntilIdle()
        assertEquals(3, speaker.requests.size)
        assertEquals(listOf(0), started)
    }

    @Test
    fun `pause holds the sentence and play resumes it`() = runTest(dispatcher) {
        val sink = FakeSink()
        val started = mutableListOf<Int>()
        val n = narrator(FakeSpeaker(), sink, started)
        n.load(segments, 0, "v", 1f, autoplay = true)
        advanceTimeBy(30)
        runCurrent()
        assertEquals(NarrationStatus.Playing, n.state.value.status)
        n.pause()
        advanceTimeBy(5_000)
        assertEquals(NarrationStatus.Paused, n.state.value.status)
        assertEquals(listOf(0), started)
        n.play()
        advanceUntilIdle()
        assertEquals(listOf(0, 1, 2, 3), started)
    }

    @Test
    fun `seek jumps to a sentence and keeps playing`() = runTest(dispatcher) {
        val sink = FakeSink()
        val started = mutableListOf<Int>()
        val n = narrator(FakeSpeaker(), sink, started)
        n.load(segments, 0, "v", 1f, autoplay = true)
        advanceTimeBy(30)
        n.seek(3)
        advanceUntilIdle()
        assertEquals(listOf(0, 3), started)
        assertTrue(sink.stops >= 1)
    }

    @Test
    fun `seek while paused moves without playing`() = runTest(dispatcher) {
        val started = mutableListOf<Int>()
        val n = narrator(FakeSpeaker(), FakeSink(), started)
        n.load(segments, 0, "v", 1f, autoplay = false)
        n.seek(2)
        advanceUntilIdle()
        assertEquals(NarratorState(NarrationStatus.Paused, 2), n.state.value)
        assertTrue(started.isEmpty())
    }

    @Test
    fun `a failed request is an error that play retries`() = runTest(dispatcher) {
        val speaker = FakeSpeaker().apply { fail = "The host voice is not running." }
        val started = mutableListOf<Int>()
        val n = narrator(speaker, FakeSink(), started)
        n.load(segments, 1, "v", 1f, autoplay = true)
        advanceUntilIdle()
        assertEquals(NarratorState(NarrationStatus.Error, 1, "The host voice is not running."), n.state.value)

        speaker.fail = null
        n.play()
        advanceUntilIdle()
        assertEquals(listOf(1, 2, 3), started)
    }

    @Test
    fun `a new voice refetches the current sentence and stop releases the output`() = runTest(dispatcher) {
        val speaker = FakeSpeaker()
        val sink = FakeSink()
        val n = narrator(speaker, sink, mutableListOf())
        n.load(segments, 0, "a", 1f, autoplay = true)
        advanceTimeBy(30)
        n.configure("b", 1f)
        advanceUntilIdle()
        assertEquals("b", speaker.requests.last().second)
        assertEquals(segments[0].text, speaker.requests.first { it.second == "b" }.first)

        n.stop()
        assertEquals(NarrationStatus.Off, n.state.value.status)
        assertEquals(1, sink.releases)
    }
}
