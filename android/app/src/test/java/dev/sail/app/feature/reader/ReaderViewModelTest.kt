package dev.sail.app.feature.reader

import dev.sail.app.core.UiState
import dev.sail.app.core.net.ApiResult
import dev.sail.app.feature.library.ReaderBook
import dev.sail.app.feature.library.ReaderBookDetail
import dev.sail.app.feature.library.ReadingAnchor
import dev.sail.app.feature.library.ReadingPosition
import dev.sail.app.feature.library.SectionInfo
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class ReaderViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private data class Save(val anchor: ReadingAnchor, val percent: Float, val revision: Long)

    private class FakeSource(position: ReadingPosition?, val texts: List<String>? = null) : ReaderSource {
        var speech: ApiResult<SpeechInfo> = ApiResult.Ok(SpeechInfo(available = true, voices = listOf("af_heart", "bf_emma")))
        val spoken = mutableListOf<String>()
        val saves = mutableListOf<Save>()
        val loadedSections = mutableListOf<Int>()
        var next: ((Save) -> SaveOutcome)? = null
        private var serverRevision = position?.revision ?: 0
        val detail = ReaderBookDetail(
            book = ReaderBook(id = "b", title = "Book", chars = 300, sections = 3, position = position),
            sections = listOf(SectionInfo(0, "One", 100, 2), SectionInfo(1, "Two", 100, 3), SectionInfo(2, "Three", 100, 1)),
        )

        override suspend fun book(id: String): ApiResult<ReaderBookDetail> = ApiResult.Ok(detail)

        override suspend fun section(id: String, index: Int): ApiResult<ReaderSection> {
            loadedSections += index
            val count = detail.sections[index].blocks
            texts?.let { t -> return ApiResult.Ok(ReaderSection(index, blocks = List(count) { ReaderBlock("p", t[index]) })) }
            return ApiResult.Ok(ReaderSection(index, blocks = List(count) { ReaderBlock("p", "x".repeat(100 / count)) }))
        }

        override suspend fun savePosition(id: String, anchor: ReadingAnchor, percent: Float, expectedRevision: Long): SaveOutcome {
            val save = Save(anchor, percent, expectedRevision)
            saves += save
            next?.let { return it(save) }
            serverRevision = expectedRevision + 1
            return SaveOutcome.Saved(ReadingPosition(anchor, percent, serverRevision))
        }

        override suspend fun speechInfo(): ApiResult<SpeechInfo> = speech

        override suspend fun speak(text: String, voice: String, speed: Float): SpeechOutcome {
            spoken += text
            return SpeechOutcome.Audio(Pcm(ByteArray(2), 24_000))
        }
    }

    private class MemoryPrefs : ReaderPrefsStore {
        var stored = ReaderPrefs()

        override fun load() = stored

        override fun save(prefs: ReaderPrefs) {
            stored = prefs
        }
    }

    // Not backgroundScope: advanceUntilIdle stops once only background work is left.
    private val persist = CoroutineScope(dispatcher)

    private fun reader(
        source: FakeSource,
        start: Int? = null,
        prefs: ReaderPrefsStore = MemoryPrefs(),
        sink: FakeSink? = null,
    ) = ReaderViewModel(source, prefs, "b", start, persistScope = persist, sinkFactory = sink?.let { s -> { _ -> s } })


    /** Finish the scroll the ViewModel asked for, landing exactly where it asked. */
    private fun ReaderViewModel.land(anchor: ReadingAnchor? = null) {
        val request = requireNotNull(state.value.scroll) { "no scroll pending" }
        scrolled(request.token, anchor ?: request.anchor)
    }

    @Test
    fun `resumes at the saved position without saving it again`() = runTest(dispatcher) {
        val source = FakeSource(ReadingPosition(ReadingAnchor(1, 2, 5), 60f, revision = 3))
        val vm = reader(source)
        advanceUntilIdle()

        assertEquals(listOf(1), source.loadedSections)
        assertEquals(ReadingAnchor(1, 2, 5), vm.state.value.scroll?.anchor)
        vm.land()
        advanceUntilIdle()
        assertTrue(source.saves.isEmpty())
        assertNull(vm.state.value.scroll)
    }

    @Test
    fun `reading saves once after the page settles, with the last revision`() = runTest(dispatcher) {
        val source = FakeSource(ReadingPosition(ReadingAnchor(1, 0, 0), 33f, revision = 3))
        val vm = reader(source)
        advanceUntilIdle()
        vm.land()

        vm.visible(ReadingAnchor(1, 1, 0))
        advanceTimeBy(300)
        vm.visible(ReadingAnchor(1, 2, 10))
        advanceUntilIdle()
        // One save, for the latest place, at section 0 (100) + 2 blocks (66) + 10 chars.
        assertEquals(listOf(Save(ReadingAnchor(1, 2, 10), 176f / 3f, 3)), source.saves.map { it.copy(percent = 176f / 3f) })
        assertEquals(176f / 3f, source.saves.single().percent, 0.01f)
        assertEquals(SaveStatus.Saved, vm.state.value.save)

        vm.visible(ReadingAnchor(1, 2, 20))
        advanceUntilIdle()
        assertEquals(4L, source.saves.last().revision)
    }

    @Test
    fun `opening at a chosen section is saved`() = runTest(dispatcher) {
        val source = FakeSource(null)
        val vm = reader(source, start = 2)
        advanceUntilIdle()
        assertEquals(listOf(2), source.loadedSections)
        vm.land()
        advanceUntilIdle()
        assertEquals(ReadingAnchor(2, 0, 0), source.saves.single().anchor)
        assertEquals(0L, source.saves.single().revision)
    }

    @Test
    fun `next and previous pages cross sections`() = runTest(dispatcher) {
        val source = FakeSource(ReadingPosition(ReadingAnchor(1, 0, 0), 33f, revision = 1))
        val vm = reader(source)
        advanceUntilIdle()
        vm.land()

        vm.previousSection(atEnd = true)
        advanceUntilIdle()
        val back = vm.state.value.scroll!!
        assertTrue(back.toEnd)
        assertEquals(ReadingAnchor(0, 1, 0), back.anchor)
        vm.land(ReadingAnchor(0, 1, 40))
        advanceUntilIdle()
        assertEquals(ReadingAnchor(0, 1, 40), source.saves.last().anchor)

        vm.nextSection()
        advanceUntilIdle()
        assertEquals(ReadingAnchor(1, 0, 0), vm.state.value.scroll?.anchor)
        assertEquals(listOf(1, 0, 1), source.loadedSections)
    }

    @Test
    fun `a newer save elsewhere is a conflict, never overwritten silently`() = runTest(dispatcher) {
        val remote = ReadingPosition(ReadingAnchor(2, 0, 0), 90f, revision = 7)
        val source = FakeSource(ReadingPosition(ReadingAnchor(1, 0, 0), 33f, revision = 3))
        val vm = reader(source)
        advanceUntilIdle()
        vm.land()
        source.next = { SaveOutcome.Conflict(remote) }

        vm.visible(ReadingAnchor(1, 1, 0))
        advanceUntilIdle()
        assertEquals(remote, vm.state.value.conflict)

        // While the conflict is open nothing more is saved.
        vm.visible(ReadingAnchor(1, 2, 0))
        vm.flush()
        advanceUntilIdle()
        assertEquals(1, source.saves.size)

        source.next = null
        vm.keepHere()
        advanceUntilIdle()
        assertNull(vm.state.value.conflict)
        assertEquals(Save(ReadingAnchor(1, 1, 0), source.saves.last().percent, 7), source.saves.last())
    }

    @Test
    fun `going to the other device's place loads it without saving`() = runTest(dispatcher) {
        val remote = ReadingPosition(ReadingAnchor(2, 0, 0), 90f, revision = 7)
        val source = FakeSource(ReadingPosition(ReadingAnchor(1, 0, 0), 33f, revision = 3))
        val vm = reader(source)
        advanceUntilIdle()
        vm.land()
        source.next = { SaveOutcome.Conflict(remote) }
        vm.visible(ReadingAnchor(1, 1, 0))
        advanceUntilIdle()

        source.next = null
        vm.goThere()
        advanceUntilIdle()
        assertEquals(ReadingAnchor(2, 0, 0), vm.state.value.scroll?.anchor)
        vm.land()
        advanceUntilIdle()
        assertEquals(1, source.saves.size)

        // The next move saves against the other device's revision.
        vm.visible(ReadingAnchor(2, 0, 30))
        advanceUntilIdle()
        assertEquals(7L, source.saves.last().revision)
    }

    @Test
    fun `a failed save is reported and retried on flush`() = runTest(dispatcher) {
        val source = FakeSource(null)
        val vm = reader(source)
        advanceUntilIdle()
        vm.land()
        source.next = { SaveOutcome.Failed("SAIL is unreachable") }

        vm.visible(ReadingAnchor(0, 1, 0))
        advanceUntilIdle()
        assertEquals(SaveStatus.Failed("SAIL is unreachable"), vm.state.value.save)

        source.next = null
        vm.flush()
        advanceUntilIdle()
        assertEquals(2, source.saves.size)
        assertEquals(SaveStatus.Saved, vm.state.value.save)
    }

    @Test
    fun `flush saves immediately instead of waiting`() = runTest(dispatcher) {
        val source = FakeSource(null)
        val vm = reader(source)
        advanceUntilIdle()
        vm.land()
        vm.visible(ReadingAnchor(0, 1, 0))
        vm.flush()
        advanceTimeBy(10)
        assertEquals(1, source.saves.size)
        advanceUntilIdle()
        assertEquals(1, source.saves.size)
    }

    @Test
    fun `text size changes are stored, clamped and keep the place`() = runTest(dispatcher) {
        val prefs = MemoryPrefs()
        val vm = reader(FakeSource(ReadingPosition(ReadingAnchor(1, 2, 5), 60f, revision = 1)), prefs = prefs)
        advanceUntilIdle()
        vm.land()

        vm.updatePrefs { it.copy(textSize = 99) }
        assertEquals(ReaderPrefs.MAX_TEXT_SIZE, prefs.stored.textSize)
        assertEquals(ReadingAnchor(1, 2, 5), vm.state.value.scroll?.anchor)

        vm.land()
        vm.updatePrefs { it.copy(theme = ReaderTheme.Sepia) }
        assertNull(vm.state.value.scroll)
        assertEquals(ReaderTheme.Sepia, prefs.stored.theme)
    }

    @Test
    fun `a book with no sections is an honest empty state`() = runTest(dispatcher) {
        val source = object : ReaderSource {
            override suspend fun book(id: String) = ApiResult.Ok(ReaderBookDetail(ReaderBook(id = "b")))
            override suspend fun section(id: String, index: Int): ApiResult<ReaderSection> = error("not loaded")
            override suspend fun savePosition(id: String, anchor: ReadingAnchor, percent: Float, expectedRevision: Long) =
                error("not saved")
        }
        val vm = ReaderViewModel(source, MemoryPrefs(), "b", null, persist)
        advanceUntilIdle()
        assertTrue(vm.state.value.book is UiState.Empty)
        assertNotNull((vm.state.value.book as UiState.Empty).detail)
    }

    // --- Read-aloud -------------------------------------------------------------------

    private fun oneBlockSource(position: ReadingPosition? = null): FakeSource {
        val source = object {
            val texts = List(3) { i -> "S${i}a. S${i}b. S${i}c." }
        }
        return FakeSource(position, source.texts)
    }

    @Test
    fun `listening starts at the sentence on the page and saves the spoken place`() = runTest(dispatcher) {
        val source = FakeSource(ReadingPosition(ReadingAnchor(1, 1, 0), 40f, revision = 2))
        val sink = FakeSink()
        val vm = reader(source, sink = sink)
        advanceUntilIdle()
        vm.land()

        vm.listen()
        runCurrent()
        advanceTimeBy(30)
        runCurrent()
        val n = vm.state.value.narration
        assertEquals(NarrationStatus.Playing, n.status)
        assertEquals(1, n.segment?.block)
        assertEquals("af_heart", n.voice)
        // Looking elsewhere while listening doesn't move the saved place.
        vm.visible(ReadingAnchor(1, 0, 0))
        assertEquals(ReadingAnchor(1, 1, 0), vm.state.value.anchor)
        vm.stopNarration()
        advanceUntilIdle()
        assertTrue(source.saves.any { it.anchor == ReadingAnchor(1, 1, 0) })
    }

    @Test
    fun `an unavailable host voice is reported, not faked`() = runTest(dispatcher) {
        val source = FakeSource(null).apply { speech = ApiResult.Ok(SpeechInfo(available = false, reason = "The host voice is not running.")) }
        val vm = reader(source, sink = FakeSink())
        advanceUntilIdle()
        vm.land()
        vm.listen()
        advanceUntilIdle()
        assertEquals(NarrationStatus.Off, vm.state.value.narration.status)
        assertEquals("The host voice is not running.", vm.state.value.narration.message)
        assertTrue(source.spoken.isEmpty())
    }

    @Test
    fun `a long press only marks a sentence until it is confirmed`() = runTest(dispatcher) {
        val source = oneBlockSource()
        val vm = reader(source, sink = FakeSink())
        advanceUntilIdle()
        vm.land()
        vm.listen()
        advanceTimeBy(30)
        runCurrent()
        assertEquals(0, vm.state.value.narration.index)

        vm.selectAt(block = 0, char = 11) // inside "S0c."
        assertEquals("S0c.", vm.state.value.narration.pending?.text)
        advanceTimeBy(60)
        runCurrent()
        // Still reading in order: the mark alone moved nothing.
        assertTrue(vm.state.value.narration.index <= 1)

        vm.confirmSelection()
        advanceTimeBy(30)
        runCurrent()
        val n = vm.state.value.narration
        assertNull(n.pending)
        assertEquals(2, n.index)
        assertEquals(NarrationStatus.Playing, n.status)
        assertTrue(n.undo != null)
    }

    @Test
    fun `undo returns to where the voice was`() = runTest(dispatcher) {
        val vm = reader(oneBlockSource(), sink = FakeSink())
        advanceUntilIdle()
        vm.land()
        vm.listen()
        advanceTimeBy(30)
        runCurrent()
        val before = vm.state.value.narration.segment
        vm.selectAt(0, 11)
        vm.confirmSelection()
        advanceTimeBy(30)
        runCurrent()

        vm.undoJump()
        advanceTimeBy(30)
        runCurrent()
        assertEquals(before, vm.state.value.narration.segment)
        assertNull(vm.state.value.narration.undo)
    }

    @Test
    fun `an unconfirmed mark clears itself`() = runTest(dispatcher) {
        val vm = reader(oneBlockSource(), sink = FakeSink())
        advanceUntilIdle()
        vm.land()
        vm.selectAt(0, 6)
        assertTrue(vm.state.value.narration.pending != null)
        advanceTimeBy(8_001)
        runCurrent()
        assertNull(vm.state.value.narration.pending)
        // Pressing a gap between blocks marks nothing.
        vm.selectAt(9, 0)
        assertNull(vm.state.value.narration.pending)
    }

    @Test
    fun `confirming a mark while not listening starts listening there`() = runTest(dispatcher) {
        val source = oneBlockSource()
        val vm = reader(source, sink = FakeSink())
        advanceUntilIdle()
        vm.land()
        vm.selectAt(0, 6)
        vm.confirmSelection()
        advanceTimeBy(30)
        runCurrent()
        assertEquals("S0b. ", vm.state.value.narration.segment?.text)
        assertEquals("S0b. ", source.spoken.first())
    }

    @Test
    fun `narration carries on into the next section`() = runTest(dispatcher) {
        val source = oneBlockSource()
        val vm = reader(source, sink = FakeSink())
        advanceUntilIdle()
        vm.land()
        vm.listen()
        // Section 0 is six sentences of ~100 ms each.
        advanceTimeBy(800)
        runCurrent()
        vm.land()
        advanceTimeBy(150)
        runCurrent()
        assertEquals(1, vm.state.value.sectionIndex)
        assertTrue(source.spoken.contains("S1a. "))
        assertTrue(vm.state.value.narration.active)
    }

    @Test
    fun `scrolling away stops following until asked`() = runTest(dispatcher) {
        val vm = reader(oneBlockSource(), sink = FakeSink())
        advanceUntilIdle()
        vm.land()
        vm.listen()
        advanceTimeBy(30)
        runCurrent()
        vm.userScrolled()
        assertEquals(false, vm.state.value.narration.follow)
        vm.followNarration()
        assertEquals(true, vm.state.value.narration.follow)
        vm.stopNarration()
        advanceUntilIdle()
    }
}
