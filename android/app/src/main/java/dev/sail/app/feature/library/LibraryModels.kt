package dev.sail.app.feature.library

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import java.time.Instant
import java.util.Locale

@Serializable
data class ReaderLibrary(val books: List<ReaderBook> = emptyList())

@Serializable
data class ReaderBook(
    val id: String = "",
    val title: String = "",
    val authors: List<String> = emptyList(),
    val format: String = "txt",
    val language: String? = null,
    val sections: Int = 0,
    val chars: Long = 0,
    @SerialName("added_at") val addedAt: String? = null,
    @SerialName("opened_at") val openedAt: String? = null,
    val position: ReadingPosition? = null,
    @SerialName("cover_version") val coverVersion: String? = null,
    val metadata: BookMetadata? = null,
    val embedded: EmbeddedMetadata? = null,
) {
    val displayTitle: String
        get() = metadata?.title?.takeIf { it.isNotBlank() } ?: title.takeIf { it.isNotBlank() } ?: "Untitled"

    val displayAuthors: List<String>
        get() = metadata?.authors?.filter { it.isNotBlank() }?.takeIf { it.isNotEmpty() } ?: authors

    val authorLine: String
        get() = displayAuthors.joinToString(", ").takeIf { it.isNotBlank() } ?: "Unknown author"

    val formatLabel: String
        get() = format.uppercase(Locale.US)

    val isStarted: Boolean
        get() = position != null

    val isFinished: Boolean
        get() = (position?.percent ?: 0f) >= 100f

    val progressFraction: Float
        get() = ((position?.percent ?: 0f) / 100f).coerceIn(0f, 1f)

    val progressLabel: String
        get() = position?.let { progressText(it.percent) } ?: "Not started"

    val year: Int?
        get() = metadata?.firstPublishYear ?: embedded?.firstPublishYear

    val description: String?
        get() = metadata?.description?.takeIf { it.isNotBlank() }
            ?: embedded?.description?.takeIf { it.isNotBlank() }

    val languageLabel: String?
        get() = language?.takeIf { it.isNotBlank() }
            ?: embedded?.languages?.firstOrNull()?.takeIf { it.isNotBlank() }
}

@Serializable
data class ReadingPosition(
    val anchor: ReadingAnchor = ReadingAnchor(),
    val percent: Float = 0f,
    val revision: Long = 0,
    @SerialName("saved_at") val savedAt: String? = null,
)

@Serializable
data class ReadingAnchor(
    val section: Int = 0,
    val block: Int = 0,
    val char: Int = 0,
)

@Serializable
data class BookMetadata(
    val title: String? = null,
    val authors: List<String> = emptyList(),
    @SerialName("first_publish_year") val firstPublishYear: Int? = null,
    val subtitle: String? = null,
    val subjects: List<String> = emptyList(),
    val description: String? = null,
    val publishers: List<String> = emptyList(),
    val languages: List<String> = emptyList(),
)

@Serializable
data class EmbeddedMetadata(
    val subtitle: String? = null,
    @SerialName("first_publish_year") val firstPublishYear: Int? = null,
    val publishers: List<String> = emptyList(),
    val languages: List<String> = emptyList(),
    val subjects: List<String> = emptyList(),
    val description: String? = null,
)

@Serializable
data class ReaderBookDetail(
    val book: ReaderBook = ReaderBook(),
    val toc: List<TocNode> = emptyList(),
    val sections: List<SectionInfo> = emptyList(),
)

@Serializable
data class TocNode(
    val title: String = "",
    val section: Int = 0,
    val block: Int = 0,
    val children: List<TocNode> = emptyList(),
)

@Serializable
data class SectionInfo(
    val index: Int = 0,
    val title: String? = null,
    val chars: Long = 0,
    val blocks: Int = 0,
)

@Serializable
data class MusicLibrary(val tracks: List<MusicTrack> = emptyList())

@Serializable
data class MusicTrack(
    val source: String = "local",
    val id: String = "",
    val title: String = "",
    val artist: String? = null,
    val album: String? = null,
    @SerialName("album_artist") val albumArtist: String? = null,
    @SerialName("track_number") val trackNumber: Int? = null,
    @SerialName("disc_number") val discNumber: Int? = null,
    val year: Int? = null,
    @SerialName("duration_ms") val durationMs: Long? = null,
    val format: String? = null,
    @SerialName("has_cover") val hasCover: Boolean = false,
    @SerialName("added_at") val addedAt: String? = null,
) {
    val displayArtist: String
        get() = artist?.takeIf { it.isNotBlank() }
            ?: albumArtist?.takeIf { it.isNotBlank() }
            ?: "Unknown artist"

    val durationLabel: String
        get() = durationMs?.let(::formatDuration).orEmpty()
}

enum class LibraryTab(val label: String) {
    All("All"),
    Books("Books"),
    Notes("Notes"),
    Media("Media"),
}

fun percentText(percent: Float): String = String.format(Locale.US, "%.1f%%", percent)

fun progressText(percent: Float): String = "${percentText(percent)} by text"

fun formatDuration(durationMs: Long): String {
    val totalSeconds = (durationMs / 1000).coerceAtLeast(0)
    val hours = totalSeconds / 3600
    val minutes = (totalSeconds % 3600) / 60
    val seconds = totalSeconds % 60
    return if (hours > 0) {
        String.format(Locale.US, "%d:%02d:%02d", hours, minutes, seconds)
    } else {
        String.format(Locale.US, "%d:%02d", minutes, seconds)
    }
}

fun filterBooks(books: List<ReaderBook>, query: String): List<ReaderBook> {
    val needle = query.trim()
    if (needle.isEmpty()) return books
    return books.filter { book ->
        book.displayTitle.contains(needle, ignoreCase = true) ||
            book.title.contains(needle, ignoreCase = true) ||
            book.displayAuthors.any { it.contains(needle, ignoreCase = true) } ||
            book.authors.any { it.contains(needle, ignoreCase = true) }
    }
}

fun filterTracks(tracks: List<MusicTrack>, query: String): List<MusicTrack> {
    val needle = query.trim()
    if (needle.isEmpty()) return tracks
    return tracks.filter { track ->
        track.title.contains(needle, ignoreCase = true) ||
            track.displayArtist.contains(needle, ignoreCase = true) ||
            track.album.orEmpty().contains(needle, ignoreCase = true)
    }
}

fun continueReading(books: List<ReaderBook>): ReaderBook? = books
    .filter { it.isStarted && !it.isFinished }
    .maxWithOrNull(
        compareBy<ReaderBook> { parseInstant(it.position?.savedAt) }
            .thenBy { parseInstant(it.openedAt) }
            .thenBy { parseInstant(it.addedAt) },
    )

fun tocTitles(toc: List<TocNode>): Map<Int, String> {
    val titles = mutableMapOf<Int, String>()
    fun walk(nodes: List<TocNode>) {
        for (node in nodes) {
            if (node.title.isNotBlank()) titles.putIfAbsent(node.section, node.title)
            walk(node.children)
        }
    }
    walk(toc)
    return titles
}

private fun parseInstant(value: String?): Instant =
    value?.let { runCatching { Instant.parse(it) }.getOrNull() } ?: Instant.EPOCH
