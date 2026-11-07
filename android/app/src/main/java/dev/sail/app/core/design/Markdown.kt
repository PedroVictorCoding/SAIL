package dev.sail.app.core.design

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * A deliberately small, safe Markdown subset for assistant replies: headings, paragraphs,
 * bullet and numbered lists, fenced code, **bold**, *italic* and `code`. Links are shown as
 * text, never made clickable, and nothing is parsed as HTML.
 */
sealed interface MdBlock {
    data class Heading(val level: Int, val text: String) : MdBlock
    data class Paragraph(val text: String) : MdBlock
    data class ListItem(val marker: String, val text: String) : MdBlock
    data class Code(val language: String, val code: String) : MdBlock
}

object Markdown {
    private val heading = Regex("^(#{1,4})\\s+(.*)$")
    private val bullet = Regex("^\\s*[-*+]\\s+(.*)$")
    private val numbered = Regex("^\\s*(\\d+)[.)]\\s+(.*)$")

    fun parse(source: String): List<MdBlock> {
        val blocks = mutableListOf<MdBlock>()
        val paragraph = StringBuilder()
        fun flush() {
            if (paragraph.isNotBlank()) blocks += MdBlock.Paragraph(paragraph.toString().trim())
            paragraph.clear()
        }
        val lines = source.replace("\r\n", "\n").split('\n')
        var i = 0
        while (i < lines.size) {
            val line = lines[i]
            if (line.trimStart().startsWith("```")) {
                flush()
                val language = line.trimStart().removePrefix("```").trim()
                val code = StringBuilder()
                i++
                while (i < lines.size && !lines[i].trimStart().startsWith("```")) {
                    if (code.isNotEmpty()) code.append('\n')
                    code.append(lines[i])
                    i++
                }
                blocks += MdBlock.Code(language, code.toString())
                i++
                continue
            }
            val h = heading.find(line)
            val b = bullet.find(line)
            val n = numbered.find(line)
            when {
                line.isBlank() -> flush()
                h != null -> {
                    flush()
                    blocks += MdBlock.Heading(h.groupValues[1].length, h.groupValues[2])
                }
                b != null -> {
                    flush()
                    blocks += MdBlock.ListItem("•", b.groupValues[1])
                }
                n != null -> {
                    flush()
                    blocks += MdBlock.ListItem("${n.groupValues[1]}.", n.groupValues[2])
                }
                else -> {
                    if (paragraph.isNotEmpty()) paragraph.append(' ')
                    paragraph.append(line.trim())
                }
            }
            i++
        }
        flush()
        return blocks
    }

    /** Inline **bold**, *italic*/_italic_ and `code`; everything else is literal text. */
    fun inline(text: String): AnnotatedString = buildAnnotatedString {
        var i = 0
        while (i < text.length) {
            when {
                text.startsWith("**", i) -> {
                    val end = text.indexOf("**", i + 2)
                    if (end > i + 2) {
                        withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(text.substring(i + 2, end)) }
                        i = end + 2
                    } else {
                        append(text[i]); i++
                    }
                }
                text[i] == '`' -> {
                    val end = text.indexOf('`', i + 1)
                    if (end > i + 1) {
                        withStyle(SpanStyle(fontFamily = FontFamily.Monospace, background = Color(0x22FFFFFF))) {
                            append(text.substring(i + 1, end))
                        }
                        i = end + 1
                    } else {
                        append(text[i]); i++
                    }
                }
                (text[i] == '*' || text[i] == '_') && i + 1 < text.length && !text[i + 1].isWhitespace() -> {
                    val end = text.indexOf(text[i], i + 1)
                    if (end > i + 1) {
                        withStyle(SpanStyle(fontStyle = FontStyle.Italic)) { append(text.substring(i + 1, end)) }
                        i = end + 1
                    } else {
                        append(text[i]); i++
                    }
                }
                else -> {
                    append(text[i]); i++
                }
            }
        }
    }
}

@Composable
fun MarkdownText(source: String, modifier: Modifier = Modifier, color: Color = SailColors.Text) {
    val blocks = remember(source) { Markdown.parse(source) }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        for (block in blocks) {
            when (block) {
                is MdBlock.Heading -> Text(
                    Markdown.inline(block.text),
                    color = color,
                    fontWeight = FontWeight.SemiBold,
                    fontSize = when (block.level) { 1 -> 20.sp; 2 -> 18.sp; else -> 16.sp },
                )
                is MdBlock.Paragraph -> Text(Markdown.inline(block.text), color = color, fontSize = 15.sp, lineHeight = 22.sp)
                is MdBlock.ListItem -> Row {
                    Text(block.marker, color = SailColors.TextMuted, modifier = Modifier.padding(end = 8.dp))
                    Text(Markdown.inline(block.text), color = color, fontSize = 15.sp, lineHeight = 22.sp)
                }
                is MdBlock.Code -> Text(
                    block.code,
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(10.dp))
                        .background(Color(0xFF0E121A))
                        .horizontalScroll(rememberScrollState())
                        .padding(12.dp),
                    fontFamily = FontFamily.Monospace,
                    fontSize = 13.sp,
                    color = color,
                    softWrap = false,
                )
            }
        }
    }
}
