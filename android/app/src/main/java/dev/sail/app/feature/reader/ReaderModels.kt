package dev.sail.app.feature.reader

import dev.sail.app.feature.library.ReaderBook
import dev.sail.app.feature.library.ReadingAnchor
import dev.sail.app.feature.library.ReadingPosition
import dev.sail.app.feature.library.SectionInfo
import dev.sail.app.feature.library.TocNode
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** `GET /api/reader/books/{id}/sections/{index}`: plain-text blocks, never book HTML. */
@Serializable
data class ReaderSection(
    val index: Int = 0,
    val title: String? = null,
    val blocks: List<ReaderBlock> = emptyList(),
)

/** One block. `\n` inside [text] is a line break the book asked for (poetry, code). */
@Serializable
data class ReaderBlock(
    val kind: String = "p",
    val text: String = "",
) {
    val headingLevel: Int?
        get() = if (kind.length == 2 && kind[0] == 'h') kind[1].digitToIntOrNull()?.takeIf { it in 1..6 } else null
}

@Serializable
data class PositionRequest(
    val anchor: ReadingAnchor,
    val percent: Float,
    @SerialName("expected_revision") val expectedRevision: Long,
)

@Serializable
data class PositionResponse(val position: ReadingPosition? = null)

/** What a position save did. A conflict carries the place saved on the other device. */
sealed interface SaveOutcome {
    data class Saved(val position: ReadingPosition) : SaveOutcome
    data class Conflict(val remote: ReadingPosition) : SaveOutcome
    data class Failed(val message: String) : SaveOutcome
}

/**
 * How far into the book [anchor] is, by text, 0–100: the same measure as the web reader
 * (code points before the anchor over the book's total), so both clients agree.
 * [section] is the loaded section the anchor points into, if it is the one loaded.
 */
fun percentAt(
    anchor: ReadingAnchor,
    book: ReaderBook,
    sections: List<SectionInfo>,
    section: ReaderSection?,
): Float {
    if (book.chars <= 0) return 0f
    val before = sections.filter { it.index < anchor.section }.sumOf { it.chars }
    val blocks = section?.takeIf { it.index == anchor.section }?.blocks.orEmpty()
    val prior = blocks.take(anchor.block).sumOf { it.text.codePointCount(0, it.text.length).toLong() }
    val inBlock = blocks.getOrNull(anchor.block)?.text?.let { text ->
        text.codePointCount(0, anchor.char.coerceIn(0, text.length))
    } ?: 0
    val here = before + prior + inBlock
    return (here.toFloat() / book.chars * 100f).coerceIn(0f, 100f)
}

/** The deepest contents entry at or before [anchor], for the "Chapter · 12.3%" label. */
fun chapterAt(toc: List<TocNode>, anchor: ReadingAnchor): String? {
    var best: TocNode? = null
    fun visit(nodes: List<TocNode>) {
        for (node in nodes) {
            val atOrBefore = node.section < anchor.section ||
                (node.section == anchor.section && node.block <= anchor.block)
            if (atOrBefore && node.title.isNotBlank()) {
                val current = best
                if (current == null || node.section > current.section ||
                    (node.section == current.section && node.block >= current.block)
                ) {
                    best = node
                }
            }
            visit(node.children)
        }
    }
    visit(toc)
    return best?.title
}

/** Contents flattened for the sheet, with depth for indentation. */
data class TocEntry(val title: String, val anchor: ReadingAnchor, val depth: Int)

fun flattenToc(toc: List<TocNode>, sections: List<SectionInfo>): List<TocEntry> {
    val out = mutableListOf<TocEntry>()
    fun walk(nodes: List<TocNode>, depth: Int) {
        for (node in nodes) {
            if (node.title.isNotBlank()) {
                out += TocEntry(node.title, ReadingAnchor(node.section, node.block, 0), depth)
            }
            walk(node.children, depth + 1)
        }
    }
    walk(toc, 0)
    if (out.isNotEmpty()) return out
    // No contents in the book: list its sections instead.
    return sections.map { info ->
        TocEntry(info.title?.takeIf { it.isNotBlank() } ?: "Section ${info.index + 1}", ReadingAnchor(info.index, 0, 0), 0)
    }
}

/** An anchor the server will accept for a section of [blocks] blocks. */
fun clampAnchor(anchor: ReadingAnchor, sectionCount: Int, blocks: Int): ReadingAnchor {
    val section = anchor.section.coerceIn(0, (sectionCount - 1).coerceAtLeast(0))
    val block = anchor.block.coerceIn(0, (blocks - 1).coerceAtLeast(0))
    return ReadingAnchor(section, block, anchor.char.coerceAtLeast(0))
}

/** Reading surfaces. Night matches SAIL; Paper and Sepia are for daylight. */
enum class ReaderTheme(val label: String, val background: Long, val text: Long, val muted: Long, val accent: Long) {
    Night("Night", 0xFF07080C, 0xFFE4E8EF, 0xFF8592A6, 0xFFA99BFF),
    Paper("Paper", 0xFFF7F4EE, 0xFF22201C, 0xFF6E685E, 0xFF5B4FC4),
    Sepia("Sepia", 0xFFEFE3CB, 0xFF3A2E21, 0xFF7A6A55, 0xFF8A5A2B),
}

data class ReaderPrefs(
    val theme: ReaderTheme = ReaderTheme.Night,
    val textSize: Int = DEFAULT_TEXT_SIZE,
    val serif: Boolean = true,
    val keepScreenOn: Boolean = true,
) {
    companion object {
        const val MIN_TEXT_SIZE = 14
        const val MAX_TEXT_SIZE = 28
        const val DEFAULT_TEXT_SIZE = 18
    }
}
