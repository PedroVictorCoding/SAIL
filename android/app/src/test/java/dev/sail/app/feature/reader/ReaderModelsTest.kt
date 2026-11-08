package dev.sail.app.feature.reader

import dev.sail.app.core.net.SailApi
import dev.sail.app.feature.library.ReaderBook
import dev.sail.app.feature.library.ReadingAnchor
import dev.sail.app.feature.library.SectionInfo
import dev.sail.app.feature.library.TocNode
import kotlinx.serialization.encodeToString
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ReaderModelsTest {
    private val json = SailApi.defaultJson
    private val book = ReaderBook(id = "b", chars = 300, sections = 2)
    private val sections = listOf(SectionInfo(0, "One", 100, 2), SectionInfo(1, "Two", 200, 3))
    private val second = ReaderSection(
        index = 1,
        blocks = listOf(ReaderBlock("h1", "a".repeat(50)), ReaderBlock("p", "b".repeat(50)), ReaderBlock("p", "c".repeat(100))),
    )

    @Test
    fun `decodes a section response`() {
        val section = json.decodeFromString<ReaderSection>(
            """{"index":3,"title":"Chapter 3","blocks":[{"kind":"h2","text":"Chapter 3"},{"kind":"p","text":"It was."},{"kind":"hr","text":""}]}""",
        )
        assertEquals(3, section.index)
        assertEquals(2, section.blocks[0].headingLevel)
        assertNull(section.blocks[1].headingLevel)
        assertEquals("hr", section.blocks[2].kind)
    }

    @Test
    fun `encodes the position body the server expects`() {
        val body = json.encodeToString(PositionRequest(ReadingAnchor(1, 2, 5), 68.5f, 3))
        assertEquals("""{"anchor":{"section":1,"block":2,"char":5},"percent":68.5,"expected_revision":3}""", body)
    }

    @Test
    fun `percent counts text before the anchor like the web reader`() {
        assertEquals(0f, percentAt(ReadingAnchor(0, 0, 0), book, sections, null), 0.001f)
        // 100 chars of section 0, 50 + 50 of section 1, then 5 into the third block.
        assertEquals(205f / 3f, percentAt(ReadingAnchor(1, 2, 5), book, sections, second), 0.001f)
        // Without the section's text only whole sections count.
        assertEquals(100f / 3f, percentAt(ReadingAnchor(1, 2, 5), book, sections, null), 0.001f)
        assertEquals(0f, percentAt(ReadingAnchor(1, 0, 0), book.copy(chars = 0), sections, second), 0.001f)
    }

    @Test
    fun `percent counts code points, not UTF-16 units`() {
        val emoji = ReaderSection(0, blocks = listOf(ReaderBlock("p", "😀😀ab")))
        val tiny = ReaderBook(chars = 4)
        // Offset 4 (UTF-16) is after both emoji: two characters of four.
        assertEquals(50f, percentAt(ReadingAnchor(0, 0, 4), tiny, listOf(SectionInfo(0, null, 4, 1)), emoji), 0.001f)
    }

    @Test
    fun `chapter label is the deepest contents entry at or before the anchor`() {
        val toc = listOf(
            TocNode("Part I", 0, 0, children = listOf(TocNode("Chapter 1", 0, 0), TocNode("Chapter 2", 1, 4))),
            TocNode("Part II", 2, 0),
        )
        assertEquals("Chapter 1", chapterAt(toc, ReadingAnchor(1, 3, 0)))
        assertEquals("Chapter 2", chapterAt(toc, ReadingAnchor(1, 4, 0)))
        assertEquals("Part II", chapterAt(toc, ReadingAnchor(5, 0, 0)))
        assertNull(chapterAt(emptyList(), ReadingAnchor(0, 0, 0)))
    }

    @Test
    fun `contents fall back to sections when the book has none`() {
        val flat = flattenToc(listOf(TocNode("A", 0, 0, listOf(TocNode("A.1", 0, 3)))), sections)
        assertEquals(listOf("A", "A.1"), flat.map { it.title })
        assertEquals(listOf(0, 1), flat.map { it.depth })
        assertEquals(ReadingAnchor(0, 3, 0), flat[1].anchor)

        val fallback = flattenToc(emptyList(), listOf(SectionInfo(0, null, 1, 1), SectionInfo(1, "Two", 1, 1)))
        assertEquals(listOf("Section 1", "Two"), fallback.map { it.title })
    }

    @Test
    fun `anchors are clamped to places the server accepts`() {
        assertEquals(ReadingAnchor(1, 2, 0), clampAnchor(ReadingAnchor(9, 9, -3), sectionCount = 2, blocks = 3))
        assertEquals(ReadingAnchor(0, 0, 7), clampAnchor(ReadingAnchor(0, 0, 7), sectionCount = 1, blocks = 0))
        assertTrue(ReaderPrefs.MIN_TEXT_SIZE < ReaderPrefs.DEFAULT_TEXT_SIZE)
    }
}
