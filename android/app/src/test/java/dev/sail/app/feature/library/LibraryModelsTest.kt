package dev.sail.app.feature.library

import dev.sail.app.core.net.SailApi
import dev.sail.app.core.net.ServerConfig
import kotlinx.serialization.decodeFromString
import okhttp3.HttpUrl.Companion.toHttpUrl
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class LibraryModelsTest {
    private val json = SailApi.defaultJson

    private fun fixture(path: String): String =
        requireNotNull(javaClass.getResource(path)) { "missing test fixture $path" }.readText()

    private fun library() = json.decodeFromString<ReaderLibrary>(fixture("/fixtures/library/library.json"))

    @Test
    fun `decodes the reader library response`() {
        val books = library().books
        assertEquals(2, books.size)

        val online = books.first()
        assertEquals("Online Title", online.displayTitle)
        assertEquals(listOf("Online Author"), online.displayAuthors)
        assertEquals("Online Author", online.authorLine)
        assertEquals(2001, online.year)
        assertEquals("Online description.", online.description)
        assertEquals("EPUB", online.formatLabel)
        assertEquals("42.5% by text", online.progressLabel)
        assertEquals(0.425f, online.progressFraction, 0.0001f)
        assertTrue(online.isStarted)
        assertFalse(online.isFinished)
        assertEquals(1, online.position?.anchor?.section)
        assertEquals("EN", online.languageLabel?.uppercase())

        val plain = books.last()
        assertEquals("Plain Book", plain.displayTitle)
        assertEquals("Unknown author", plain.authorLine)
        assertEquals("Not started", plain.progressLabel)
        assertFalse(plain.isStarted)
        assertNull(plain.coverVersion)
    }

    @Test
    fun `decodes book detail with toc and section info`() {
        val detail = json.decodeFromString<ReaderBookDetail>(fixture("/fixtures/library/book_detail.json"))

        assertEquals("abc123", detail.book.id)
        assertEquals(2, detail.sections.size)
        val titles = tocTitles(detail.toc)
        assertEquals("Chapter One", titles[0])
        assertEquals("A Part", titles[1])
    }

    @Test
    fun `filters books by displayed title author and raw title`() {
        val books = library().books

        assertEquals(listOf("abc123"), filterBooks(books, "online").map { it.id })
        assertEquals(listOf("abc123"), filterBooks(books, "FILE author").map { it.id })
        assertEquals(listOf("abc123"), filterBooks(books, "embedded title").map { it.id })
        assertEquals(listOf("def456"), filterBooks(books, "plain").map { it.id })
        assertEquals(2, filterBooks(books, "  ").size)
        assertTrue(filterBooks(books, "nothing").isEmpty())
    }

    @Test
    fun `continue reading picks the most recent unfinished book`() {
        val books = library().books
        assertEquals("abc123", continueReading(books)?.id)

        val finished = books.first().copy(
            position = books.first().position?.copy(percent = 100f),
        )
        val older = books.last().copy(
            position = ReadingPosition(
                percent = 4f,
                savedAt = "2026-11-03T09:00:00Z",
            ),
        )
        assertEquals("def456", continueReading(listOf(finished, older))?.id)
        assertNull(continueReading(listOf(finished)))
    }

    @Test
    fun `decodes music library and filters tracks`() {
        val tracks = json.decodeFromString<MusicLibrary>(fixture("/fixtures/library/music.json")).tracks

        val track = tracks.single()
        assertEquals("Night Drive", track.title)
        assertEquals("The Coast", track.displayArtist)
        assertEquals("4:32", track.durationLabel)
        assertTrue(track.hasCover)
        assertEquals(listOf("track1"), filterTracks(tracks, "neon").map { it.id })
        assertEquals(listOf("track1"), filterTracks(tracks, "coast").map { it.id })

        val noArtist = track.copy(artist = null, albumArtist = null)
        assertEquals("Unknown artist", noArtist.displayArtist)
    }

    @Test
    fun `formats durations and percentages`() {
        assertEquals("4:32", formatDuration(272_000))
        assertEquals("0:09", formatDuration(9_400))
        assertEquals("1:02:03", formatDuration(3_723_000))
        assertEquals("42.5%", percentText(42.5f))
        assertEquals("42.5% by text", progressText(42.5f))
    }

    @Test
    fun `cover urls use the configured host and cache busting version`() {
        val api = SailApi(config = { ServerConfig("https://sail.example".toHttpUrl(), "token") })
        val repository = LibraryRepository(api)
        val books = library().books

        assertEquals(
            "https://sail.example/api/reader/books/abc123/cover?v=cover-test",
            repository.coverUrl(books.first()),
        )
        assertNull(repository.coverUrl(books.last()))

        val track = MusicTrack(id = "track1", hasCover = true)
        assertEquals("https://sail.example/api/music/tracks/track1/cover", repository.trackCoverUrl(track))
        assertNull(repository.trackCoverUrl(track.copy(hasCover = false)))
    }
}
