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

    private class FakeSource(position: ReadingPosition?) : ReaderSource {
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
            return ApiResult.Ok(ReaderSection(index, blocks = List(count) { ReaderBlock("p", "x".repeat(100 / count)) }))
        }

        override suspend fun savePosition(id: String, anchor: ReadingAnchor, percent: Float, expectedRevision: Long): SaveOutcome {
            val save = Save(anchor, percent, expectedRevision)
            saves += save
            next?.let { return it(save) }
            serverRevision = expectedRevision + 1
            return SaveOutcome.Saved(ReadingPosition(anchor, percent, serverRevision))
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

    private fun reader(source: FakeSource, start: Int? = null, prefs: ReaderPrefsStore = MemoryPrefs()) =
        ReaderViewModel(source, prefs, "b", start, persistScope = persist)

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
}
