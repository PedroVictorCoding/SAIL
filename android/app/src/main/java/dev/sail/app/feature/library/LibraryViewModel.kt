package dev.sail.app.feature.library

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.sail.app.core.UiState
import dev.sail.app.core.net.map
import dev.sail.app.core.toUiState
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class LibraryUiState(
    val books: UiState<List<ReaderBook>> = UiState.Loading,
    val music: UiState<List<MusicTrack>> = UiState.Loading,
    val query: String = "",
    val tab: LibraryTab = LibraryTab.All,
    val refreshing: Boolean = false,
)

class LibraryViewModel(private val repository: LibrarySource) : ViewModel() {
    private val mutable = MutableStateFlow(LibraryUiState())
    val state: StateFlow<LibraryUiState> = mutable.asStateFlow()

    init {
        refresh()
    }

    fun refresh() {
        if (mutable.value.refreshing) return
        mutable.update { it.copy(refreshing = true) }
        viewModelScope.launch {
            val results = coroutineScope {
                val books = async { repository.library() }
                val music = async { repository.music() }
                books.await() to music.await()
            }
            mutable.update { current ->
                current.copy(
                    books = results.first.map { it.books }.toUiState(
                        emptyTitle = "Your library is empty",
                        emptyDetail = "Books you import into SAIL appear here.",
                    ) { it.isEmpty() },
                    music = results.second.map { it.tracks }.toUiState(
                        emptyTitle = "No music yet",
                        emptyDetail = "Tracks in SAIL's music library appear here.",
                    ) { it.isEmpty() },
                    refreshing = false,
                )
            }
        }
    }

    fun setQuery(query: String) {
        mutable.update { it.copy(query = query) }
    }

    fun setTab(tab: LibraryTab) {
        mutable.update { it.copy(tab = tab) }
    }

    fun coverUrl(book: ReaderBook): String? = repository.coverUrl(book)

    fun trackCoverUrl(track: MusicTrack): String? = repository.trackCoverUrl(track)
}

class BookViewModel(
    private val repository: LibrarySource,
    private val bookId: String,
) : ViewModel() {
    private val mutable = MutableStateFlow<UiState<ReaderBookDetail>>(UiState.Loading)
    val state: StateFlow<UiState<ReaderBookDetail>> = mutable.asStateFlow()

    init {
        load()
    }

    fun load() {
        mutable.value = UiState.Loading
        viewModelScope.launch {
            mutable.value = repository.book(bookId).toUiState(
                emptyTitle = "This book is empty",
                emptyDetail = "SAIL has no sections for this book.",
            ) { it.book.id.isBlank() }
        }
    }

    fun coverUrl(book: ReaderBook): String? = repository.coverUrl(book)
}
