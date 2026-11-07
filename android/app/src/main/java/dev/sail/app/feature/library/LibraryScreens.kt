package dev.sail.app.feature.library

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items as gridItems
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import coil.compose.AsyncImage
import coil.request.ImageRequest
import dev.sail.app.core.LocalAppContainer
import dev.sail.app.core.UiState
import dev.sail.app.core.design.CardBaseBrush
import dev.sail.app.core.design.EmptyState
import dev.sail.app.core.design.Eyebrow
import dev.sail.app.core.design.FlatCard
import dev.sail.app.core.design.GlowCard
import dev.sail.app.core.design.Hue
import dev.sail.app.core.design.LoadingSkeleton
import dev.sail.app.core.design.PillShape
import dev.sail.app.core.design.RowShape
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.SailPill
import dev.sail.app.core.design.SailProgress
import dev.sail.app.core.design.SailRow
import dev.sail.app.core.design.SailTopBar
import dev.sail.app.core.design.ScreenGutter
import dev.sail.app.core.design.SectionHeader
import dev.sail.app.core.design.SegmentedControl
import dev.sail.app.core.sailViewModel
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

/** Search, Books/Notes/Media tabs, Continue reading, Your library. */
@Composable
fun LibraryScreen(onOpenBook: (bookId: String) -> Unit) {
    val vm = sailViewModel { LibraryViewModel(LibraryRepository(it.api)) }
    val state by vm.state.collectAsStateWithLifecycle()
    val selectedTab = state.tab

    Column(Modifier.fillMaxSize()) {
        SailTopBar(
            title = "Library",
            subtitle = "Books and media",
            actions = {
                RefreshAction(refreshing = state.refreshing, onRefresh = vm::refresh)
            },
        )
        LazyVerticalGrid(
            columns = GridCells.Adaptive(minSize = 132.dp),
            modifier = Modifier.fillMaxSize(),
            contentPadding = PaddingValues(start = ScreenGutter, end = ScreenGutter, bottom = 28.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            item(span = { GridItemSpan(maxLineSpan) }) {
                LibrarySearchField(query = state.query, onQuery = vm::setQuery)
            }
            item(span = { GridItemSpan(maxLineSpan) }) {
                SegmentedControl(
                    options = LibraryTab.entries.map { it.label },
                    selected = selectedTab.ordinal,
                    onSelect = { index -> vm.setTab(LibraryTab.entries[index]) },
                    hue = Hue.Library,
                )
            }

            if (selectedTab == LibraryTab.Notes) {
                item(span = { GridItemSpan(maxLineSpan) }) {
                    EmptyState(
                        title = "Notes aren't in this build yet",
                        detail = "This client shows books and media from SAIL. Notes will appear when the app has a notes API.",
                    )
                }
            }

            if (selectedTab == LibraryTab.All || selectedTab == LibraryTab.Books) {
                when (val books = state.books) {
                    UiState.Loading -> item(span = { GridItemSpan(maxLineSpan) }) {
                        LoadingSkeleton(lines = 5, modifier = Modifier.padding(top = 6.dp))
                    }
                    is UiState.Unavailable -> item(span = { GridItemSpan(maxLineSpan) }) {
                        EmptyState(
                            title = "Library unavailable",
                            detail = books.message,
                            onRetry = vm::refresh,
                        )
                    }
                    is UiState.Empty -> item(span = { GridItemSpan(maxLineSpan) }) {
                        EmptyState(title = books.title, detail = books.detail)
                    }
                    is UiState.Ready -> {
                        val visible = filterBooks(books.data, state.query)
                        val featured = continueReading(books.data)
                        if (featured != null && selectedTab == LibraryTab.All) {
                            item(span = { GridItemSpan(maxLineSpan) }) {
                                ContinueReadingCard(
                                    book = featured,
                                    coverUrl = vm.coverUrl(featured),
                                    onOpenBook = onOpenBook,
                                )
                            }
                        }
                        item(span = { GridItemSpan(maxLineSpan) }) {
                            SectionHeader("Your library")
                        }
                        if (visible.isEmpty()) {
                            item(span = { GridItemSpan(maxLineSpan) }) {
                                EmptyState(
                                    title = "Nothing matches your search",
                                    detail = "Try a different title or author.",
                                )
                            }
                        }
                        gridItems(visible, key = { it.id }) { book ->
                            BookTile(book = book, coverUrl = vm.coverUrl(book), onOpenBook = onOpenBook)
                        }
                    }
                }
            }

            if (selectedTab == LibraryTab.All || selectedTab == LibraryTab.Media) {
                when (val music = state.music) {
                    UiState.Loading -> if (selectedTab == LibraryTab.Media) {
                        item(span = { GridItemSpan(maxLineSpan) }) {
                            LoadingSkeleton(lines = 4, modifier = Modifier.padding(top = 6.dp))
                        }
                    }
                    is UiState.Unavailable -> if (selectedTab == LibraryTab.Media) {
                        item(span = { GridItemSpan(maxLineSpan) }) {
                            EmptyState(
                                title = "Music unavailable",
                                detail = music.message,
                                onRetry = vm::refresh,
                            )
                        }
                    }
                    is UiState.Empty -> if (selectedTab == LibraryTab.Media) {
                        item(span = { GridItemSpan(maxLineSpan) }) {
                            EmptyState(title = music.title, detail = music.detail)
                        }
                    }
                    is UiState.Ready -> {
                        val visible = filterTracks(music.data, state.query)
                        item(span = { GridItemSpan(maxLineSpan) }) {
                            SectionHeader(if (selectedTab == LibraryTab.All) "Music" else "Your music")
                        }
                        if (visible.isEmpty()) {
                            item(span = { GridItemSpan(maxLineSpan) }) {
                                EmptyState(
                                    title = "Nothing matches your search",
                                    detail = "Try a different track, artist or album.",
                                )
                            }
                        }
                        gridItems(
                            items = visible,
                            key = { "track:${it.id}" },
                            span = { GridItemSpan(maxLineSpan) },
                        ) { track ->
                            TrackRow(track = track, coverUrl = vm.trackCoverUrl(track))
                        }
                    }
                }
            }
        }
    }
}

/** One book: cover, metadata, sections and the saved reading position. */
@Composable
fun BookScreen(bookId: String, onBack: () -> Unit) {
    val vm = sailViewModel(key = bookId) { BookViewModel(LibraryRepository(it.api), bookId) }
    val state by vm.state.collectAsStateWithLifecycle()
    val readyBook = when (val current = state) {
        is UiState.Ready -> current.data.book
        else -> null
    }

    Column(Modifier.fillMaxSize()) {
        SailTopBar(
            title = readyBook?.displayTitle ?: "Book",
            subtitle = readyBook?.authorLine,
            onBack = onBack,
            actions = { RefreshAction(refreshing = false, onRefresh = vm::load) },
        )
        when (val current = state) {
            UiState.Loading -> LoadingSkeleton(
                lines = 8,
                modifier = Modifier.padding(ScreenGutter),
            )
            is UiState.Unavailable -> EmptyState(
                title = "This book couldn't load",
                detail = current.message,
                onRetry = vm::load,
                modifier = Modifier.padding(ScreenGutter),
            )
            is UiState.Empty -> EmptyState(
                title = current.title,
                detail = current.detail,
                modifier = Modifier.padding(ScreenGutter),
            )
            is UiState.Ready -> BookDetail(
                detail = current.data,
                coverUrl = vm.coverUrl(current.data.book),
            )
        }
    }
}

@Composable
private fun BookDetail(detail: ReaderBookDetail, coverUrl: String?) {
    val book = detail.book
    val toc = remember(detail.toc) { tocTitles(detail.toc) }
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(start = ScreenGutter, end = ScreenGutter, bottom = 28.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            GlowCard(hue = Hue.Library) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    BookCover(
                        book = book,
                        coverUrl = coverUrl,
                        modifier = Modifier.width(92.dp).height(138.dp),
                    )
                    Spacer(Modifier.width(14.dp))
                    Column(Modifier.weight(1f)) {
                        Text(
                            book.displayTitle,
                            style = MaterialTheme.typography.titleLarge,
                            maxLines = 3,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Text(
                            book.authorLine,
                            fontSize = 13.sp,
                            color = SailColors.TextMuted,
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Spacer(Modifier.height(8.dp))
                        Text(
                            listOfNotNull(
                                book.year?.toString(),
                                book.languageLabel,
                                book.formatLabel,
                            ).joinToString(" · "),
                            fontSize = 12.sp,
                            color = SailColors.TextSubtle,
                        )
                        Spacer(Modifier.height(4.dp))
                        Text(
                            "${book.sections} sections · ${groupedNumber(book.chars)} chars",
                            fontSize = 12.sp,
                            color = SailColors.TextSubtle,
                        )
                        if (!book.isStarted) {
                            Spacer(Modifier.height(8.dp))
                            SailPill("Not started", dot = SailColors.TextSubtle)
                        }
                    }
                }
            }
        }
        item { PositionCard(book) }
        if (!book.description.isNullOrBlank()) {
            item {
                FlatCard {
                    Eyebrow("About")
                    Spacer(Modifier.height(6.dp))
                    Text(
                        book.description.orEmpty(),
                        fontSize = 14.sp,
                        lineHeight = 21.sp,
                        color = SailColors.TextMuted,
                    )
                }
            }
        }
        item { SectionHeader("Contents") }
        if (detail.sections.isEmpty()) {
            item {
                EmptyState(
                    title = "No sections",
                    detail = "SAIL didn't find a section list for this book.",
                )
            }
        } else {
            items(detail.sections, key = { it.index }) { section ->
                SectionRow(
                    section = section,
                    tocTitle = toc[section.index],
                    current = book.position?.anchor?.section == section.index,
                )
            }
        }
    }
}

@Composable
private fun PositionCard(book: ReaderBook) {
    val position = book.position
    FlatCard {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Eyebrow("Reading position", dot = Hue.Library.color)
            Spacer(Modifier.weight(1f))
            Text(book.progressLabel, fontSize = 12.sp, color = SailColors.TextMuted)
        }
        Spacer(Modifier.height(8.dp))
        SailProgress(book.progressFraction, Hue.Library)
        Spacer(Modifier.height(8.dp))
        val saved = savedLabel(position?.savedAt)
        Text(
            if (position == null) {
                "Not started"
            } else {
                listOfNotNull(
                    book.progressLabel,
                    saved?.let { "saved $it" },
                ).joinToString(" · ")
            },
            fontSize = 12.sp,
            color = SailColors.TextSubtle,
        )
        if (position != null) {
            Text(
                "Section ${position.anchor.section + 1} · block ${position.anchor.block + 1}",
                fontSize = 12.sp,
                color = SailColors.TextSubtle,
            )
        }
    }
}

@Composable
private fun SectionRow(section: SectionInfo, tocTitle: String?, current: Boolean) {
    val trailing: (@Composable RowScope.() -> Unit)? = if (current) {
        { SailPill("Here", dot = Hue.Library.hot) }
    } else {
        null
    }
    SailRow(
        title = section.title?.takeIf { it.isNotBlank() }
            ?: tocTitle?.takeIf { it.isNotBlank() }
            ?: "Section ${section.index + 1}",
        meta = "${groupedNumber(section.chars)} chars · ${section.blocks} blocks",
        leading = {
            Box(
                modifier = Modifier
                    .size(32.dp)
                    .clip(CircleShape)
                    .background(Hue.Library.color.copy(alpha = 0.28f)),
                contentAlignment = Alignment.Center,
            ) {
                Text("${section.index + 1}", fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
            }
        },
        trailing = trailing,
    )
}

@Composable
private fun ContinueReadingCard(
    book: ReaderBook,
    coverUrl: String?,
    onOpenBook: (String) -> Unit,
) {
    GlowCard(
        hue = Hue.Library,
        onClick = { onOpenBook(book.id) },
        onClickLabel = "Continue reading ${book.displayTitle}",
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            BookCover(
                book = book,
                coverUrl = coverUrl,
                modifier = Modifier.width(64.dp).height(96.dp),
            )
            Spacer(Modifier.width(14.dp))
            Column(Modifier.weight(1f)) {
                Eyebrow("Continue reading", dot = Hue.Library.color)
                Spacer(Modifier.height(4.dp))
                Text(
                    book.displayTitle,
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    book.authorLine,
                    fontSize = 12.sp,
                    color = SailColors.TextMuted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Spacer(Modifier.height(8.dp))
                SailProgress(book.progressFraction, Hue.Library)
                Spacer(Modifier.height(4.dp))
                Text(
                    listOfNotNull(
                        book.progressLabel,
                        savedLabel(book.position?.savedAt)?.let { "saved $it" },
                    ).joinToString(" · "),
                    fontSize = 11.sp,
                    color = SailColors.TextSubtle,
                )
            }
        }
    }
}

@Composable
private fun BookTile(
    book: ReaderBook,
    coverUrl: String?,
    onOpenBook: (String) -> Unit,
) {
    Column(
        modifier = Modifier
            .clip(RowShape)
            .clickable(role = Role.Button, onClickLabel = "Open ${book.displayTitle}") {
                onOpenBook(book.id)
            }
            .padding(4.dp),
    ) {
        BookCover(
            book = book,
            coverUrl = coverUrl,
            modifier = Modifier.fillMaxWidth().aspectRatio(2f / 3f),
        )
        Spacer(Modifier.height(8.dp))
        Text(
            book.displayTitle,
            fontSize = 14.sp,
            fontWeight = FontWeight.SemiBold,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
        Text(
            book.authorLine,
            fontSize = 12.sp,
            color = SailColors.TextMuted,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        Text(
            "${book.formatLabel} · ${book.progressLabel}",
            fontSize = 11.sp,
            color = SailColors.TextSubtle,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun BookCover(book: ReaderBook, coverUrl: String?, modifier: Modifier = Modifier) {
    val container = LocalAppContainer.current
    val context = LocalContext.current
    val shape = RoundedCornerShape(12.dp)
    Box(
        modifier = modifier
            .clip(shape)
            .background(CardBaseBrush)
            .border(1.dp, SailColors.Line, shape),
    ) {
        Column(
            modifier = Modifier.fillMaxSize().padding(10.dp),
            verticalArrangement = Arrangement.SpaceBetween,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text(
                book.formatLabel,
                fontSize = 9.sp,
                letterSpacing = 1.sp,
                color = SailColors.TextSubtle,
            )
            Text(
                initials(book.displayTitle),
                fontSize = 22.sp,
                fontWeight = FontWeight.Bold,
                color = Color.White.copy(alpha = 0.82f),
                textAlign = TextAlign.Center,
                maxLines = 3,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                book.displayAuthors.firstOrNull().orEmpty(),
                fontSize = 9.sp,
                color = SailColors.TextSubtle,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        if (coverUrl != null) {
            AsyncImage(
                model = ImageRequest.Builder(context).data(coverUrl).crossfade(false).build(),
                imageLoader = container.imageLoader,
                contentDescription = "Cover of ${book.displayTitle}",
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize(),
            )
        }
    }
}

@Composable
private fun TrackRow(track: MusicTrack, coverUrl: String?) {
    val container = LocalAppContainer.current
    val context = LocalContext.current
    SailRow(
        title = track.title.takeIf { it.isNotBlank() } ?: "Untitled track",
        meta = listOfNotNull(
            track.displayArtist,
            track.album?.takeIf { it.isNotBlank() },
            track.durationLabel.takeIf { it.isNotBlank() },
        ).joinToString(" · "),
        leading = {
            Box(
                modifier = Modifier
                    .size(40.dp)
                    .clip(RoundedCornerShape(10.dp))
                    .background(Hue.Music.color.copy(alpha = 0.24f)),
                contentAlignment = Alignment.Center,
            ) {
                if (coverUrl != null) {
                    AsyncImage(
                        model = ImageRequest.Builder(context).data(coverUrl).crossfade(false).build(),
                        imageLoader = container.imageLoader,
                        contentDescription = null,
                        contentScale = ContentScale.Crop,
                        modifier = Modifier.fillMaxSize(),
                    )
                } else {
                    Text("♪", fontSize = 18.sp, color = Hue.Music.hot)
                }
            }
        },
    )
}

@Composable
private fun LibrarySearchField(query: String, onQuery: (String) -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .clip(PillShape)
            .background(Color(0xB30A0C10))
            .border(1.dp, SailColors.Line, PillShape)
            .padding(horizontal = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("⌕", fontSize = 20.sp, color = SailColors.TextSubtle)
        Spacer(Modifier.width(10.dp))
        BasicTextField(
            value = query,
            onValueChange = onQuery,
            modifier = Modifier
                .weight(1f)
                .heightIn(min = 48.dp)
                .semantics { contentDescription = "Search books, authors and music" },
            singleLine = true,
            textStyle = TextStyle(color = SailColors.Text, fontSize = 15.sp),
            cursorBrush = SolidColor(SailColors.Focus),
            decorationBox = { inner ->
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.CenterStart) {
                    if (query.isEmpty()) {
                        Text(
                            "Search books, authors, music",
                            fontSize = 15.sp,
                            color = SailColors.TextSubtle,
                        )
                    }
                    inner()
                }
            },
        )
        if (query.isNotEmpty()) {
            Box(
                modifier = Modifier
                    .size(48.dp)
                    .clip(CircleShape)
                    .clickable(onClickLabel = "Clear search", role = Role.Button) { onQuery("") },
                contentAlignment = Alignment.Center,
            ) {
                Text("×", fontSize = 18.sp, color = SailColors.TextMuted)
            }
        }
    }
}

@Composable
private fun RefreshAction(refreshing: Boolean, onRefresh: () -> Unit) {
    Box(
        modifier = Modifier
            .heightIn(min = 48.dp)
            .clip(PillShape)
            .clickable(enabled = !refreshing, role = Role.Button, onClick = onRefresh)
            .padding(horizontal = 10.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            if (refreshing) "Refreshing…" else "Refresh",
            fontSize = 13.sp,
            color = if (refreshing) SailColors.TextSubtle else SailColors.TextMuted,
        )
    }
}

private fun initials(title: String): String {
    val letters = title
        .split(' ', '\t', '\n')
        .filter { it.isNotBlank() }
        .take(2)
        .mapNotNull { it.firstOrNull()?.uppercaseChar() }
        .joinToString("")
    return letters.ifBlank { "?" }
}

private fun groupedNumber(value: Long): String = String.format(Locale.US, "%,d", value)

private fun savedLabel(value: String?): String? = value?.let { raw ->
    runCatching { Instant.parse(raw) }.getOrNull()?.let { instant ->
        DateTimeFormatter.ofPattern("d MMM, HH:mm")
            .withZone(ZoneId.systemDefault())
            .format(instant)
    }
}
