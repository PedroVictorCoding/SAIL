package dev.sail.app.feature.library

import androidx.compose.runtime.Composable
import dev.sail.app.core.design.ScreenNotBuiltYet

// Owner: Kilo. Keep these signatures.

/** Search, Books/Notes/Media tabs, Continue reading, Your library. */
@Composable
fun LibraryScreen(onOpenBook: (bookId: String) -> Unit) {
    ScreenNotBuiltYet("Library")
}

/** One book: cover, metadata, sections and the saved reading position. */
@Composable
fun BookScreen(bookId: String, onBack: () -> Unit) {
    ScreenNotBuiltYet("Book", onBack = onBack)
}
