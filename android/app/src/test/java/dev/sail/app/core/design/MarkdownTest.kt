package dev.sail.app.core.design

import org.junit.Assert.assertEquals
import org.junit.Test

class MarkdownTest {
    @Test
    fun parsesBlocks() {
        val blocks = Markdown.parse(
            """
            # Title
            First line
            continues here.

            - one
            2. two
            ```rust
            let x = 1;
            ```
            """.trimIndent(),
        )
        assertEquals(
            listOf(
                MdBlock.Heading(1, "Title"),
                MdBlock.Paragraph("First line continues here."),
                MdBlock.ListItem("•", "one"),
                MdBlock.ListItem("2.", "two"),
                MdBlock.Code("rust", "let x = 1;"),
            ),
            blocks,
        )
    }

    @Test
    fun inlineMarkupIsStyledAndHtmlStaysLiteral() {
        val text = Markdown.inline("**bold** and `code` <b>not html</b> 2 * 3")
        assertEquals("bold and code <b>not html</b> 2 * 3", text.text)
        assertEquals(2, text.spanStyles.size)
    }
}
