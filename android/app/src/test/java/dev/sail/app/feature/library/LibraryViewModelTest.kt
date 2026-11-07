package dev.sail.app.feature.library

import dev.sail.app.core.UiState
import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import kotlinx.serialization.decodeFromString
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class LibraryViewModelTest {
    private val dispatcher = StandardTestDispatcher()
    private val json = SailApi.defaultJson

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private fun fixture(path: String): String =
        requireNotNull(javaClass.getResource(path)) { "missing test fixture $path" }.readText()

    private fun books() = json.decodeFromString<ReaderLibrary>(fixture("/fixtures/library/library.json")).books

    private fun tracks() = json.decodeFromString<MusicLibrary>(fixture("/fixtures/library/music.json")).tracks

    private class FakeSource(
        private val booksResult: ApiResult<ReaderLibrary>,
        private val musicResult: ApiResult<MusicLibrary>,
        private val bookResult: ApiResult<ReaderBookDetail> = ApiResult.Err(404, "Not used"),
    ) : LibrarySource {
        override suspend fun library(): ApiResult<ReaderLibrary> = booksResult

        override suspend fun book(id: String): ApiResult<ReaderBookDetail> = bookResult

        override suspend fun music(): ApiResult<MusicLibrary> = musicResult
    }

    @Test
    fun `loads books and music`() = runTest(dispatcher) {
        val vm = LibraryViewModel(
            FakeSource(
                booksResult = ApiResult.Ok(ReaderLibrary(books())),
                musicResult = ApiResult.Ok(MusicLibrary(tracks())),
            ),
        )
        advanceUntilIdle()

        val state = vm.state.value
        val loadedBooks = when (val result = state.books) {
            is UiState.Ready -> result.data
            else -> error("books must load")
        }
        val loadedTracks = when (val result = state.music) {
            is UiState.Ready -> result.data
            else -> error("music must load")
        }
        assertEquals(2, loadedBooks.size)
        assertEquals(1, loadedTracks.size)
        assertFalse(state.refreshing)
    }

    @Test
    fun `reports empty sections honestly`() = runTest(dispatcher) {
        val vm = LibraryViewModel(
            FakeSource(
                booksResult = ApiResult.Ok(ReaderLibrary()),
                musicResult = ApiResult.Ok(MusicLibrary()),
            ),
        )
        advanceUntilIdle()

        val state = vm.state.value
        assertEquals("Your library is empty", (state.books as UiState.Empty).title)
        assertEquals("No music yet", (state.music as UiState.Empty).title)
    }

    @Test
    fun `reports unavailable sections with the server message`() = runTest(dispatcher) {
        val error = ApiResult.Err(0, "SAIL is unreachable")
        val vm = LibraryViewModel(FakeSource(booksResult = error, musicResult = error))
        advanceUntilIdle()

        val state = vm.state.value
        assertEquals("SAIL is unreachable", (state.books as UiState.Unavailable).message)
        assertEquals("SAIL is unreachable", (state.music as UiState.Unavailable).message)
        assertFalse(state.refreshing)
    }

    @Test
    fun `search and tab selection are kept in state`() = runTest(dispatcher) {
        val vm = LibraryViewModel(
            FakeSource(
                booksResult = ApiResult.Ok(ReaderLibrary(books())),
                musicResult = ApiResult.Ok(MusicLibrary(tracks())),
            ),
        )
        advanceUntilIdle()

        vm.setQuery("online")
        vm.setTab(LibraryTab.Media)
        assertEquals("online", vm.state.value.query)
        assertEquals(LibraryTab.Media, vm.state.value.tab)
    }

    @Test
    fun `book view model surfaces a load failure`() = runTest(dispatcher) {
        val vm = BookViewModel(
            FakeSource(
                booksResult = ApiResult.Ok(ReaderLibrary()),
                musicResult = ApiResult.Ok(MusicLibrary()),
                bookResult = ApiResult.Err(404, "This book isn't in the library"),
            ),
            bookId = "missing",
        )
        advanceUntilIdle()

        assertEquals(
            "This book isn't in the library",
            (vm.state.value as UiState.Unavailable).message,
        )
    }

    @Test
    fun `book view model maps a missing book to empty`() = runTest(dispatcher) {
        val vm = BookViewModel(
            FakeSource(
                booksResult = ApiResult.Ok(ReaderLibrary()),
                musicResult = ApiResult.Ok(MusicLibrary()),
                bookResult = ApiResult.Ok(ReaderBookDetail()),
            ),
            bookId = "blank",
        )
        advanceUntilIdle()

        assertTrue(vm.state.value is UiState.Empty)
    }
}
