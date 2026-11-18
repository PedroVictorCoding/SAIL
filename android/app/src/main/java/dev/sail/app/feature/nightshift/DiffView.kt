package dev.sail.app.feature.nightshift

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
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
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.sail.app.core.design.SailColors

const val COLLAPSE_DIFF_LINES_THRESHOLD = 400 // files over 400 lines collapsed by default (spec)

data class ParsedDiffFile(
    val path: String,
    val added: Int,
    val removed: Int,
    val lines: List<String>,
) {
    val collapsedByDefault: Boolean
        get() = lines.size > COLLAPSE_DIFF_LINES_THRESHOLD
}

fun parseUnifiedDiff(diffText: String): List<ParsedDiffFile> {
    if (diffText.isBlank()) return emptyList()
    val files = mutableListOf<ParsedDiffFile>()
    var currentPath = ""
    var added = 0
    var removed = 0
    var currentLines = mutableListOf<String>()

    fun flush() {
        if (currentLines.isNotEmpty()) {
            files += ParsedDiffFile(
                path = currentPath.ifEmpty { "patch" },
                added = added,
                removed = removed,
                lines = currentLines.toList(),
            )
        }
    }

    for (line in diffText.split('\n')) {
        if (line.startsWith("diff --git ")) {
            flush()
            val parts = line.removePrefix("diff --git ").split(" b/")
            currentPath = if (parts.size > 1) parts[1] else line.removePrefix("diff --git ")
            added = 0
            removed = 0
            currentLines = mutableListOf(line)
        } else {
            currentLines.add(line)
            if (line.startsWith("+") && !line.startsWith("+++")) added += 1
            else if (line.startsWith("-") && !line.startsWith("---")) removed += 1
        }
    }
    flush()
    return files
}

@Composable
fun DiffView(diffText: String) {
    val files = remember(diffText) { parseUnifiedDiff(diffText) }
    if (files.isEmpty()) {
        Text("Empty diff.", color = SailColors.TextMuted, fontSize = 13.sp)
        return
    }
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        files.forEach { file ->
            var expanded by remember(file.path, file.lines.size) {
                mutableStateOf(!file.collapsedByDefault)
            }
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(Color(0xCC0A0C10), RoundedCornerShape(14.dp))
                    .border(1.dp, SailColors.Line, RoundedCornerShape(14.dp)),
            ) {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { expanded = !expanded }
                        .padding(horizontal = 12.dp, vertical = 10.dp),
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    Text(
                        text = file.path,
                        color = SailColors.Text,
                        fontFamily = FontFamily.Monospace,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.Medium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f).padding(end = 8.dp),
                    )
                    Text(
                        text = "+${file.added} −${file.removed}" + if (!expanded) " (${file.lines.size}L)" else "",
                        color = SailColors.TextMuted,
                        fontFamily = FontFamily.Monospace,
                        fontSize = 12.sp,
                    )
                }
                if (expanded) {
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .horizontalScroll(rememberScrollState())
                            .padding(bottom = 8.dp),
                    ) {
                        file.lines.forEach { line ->
                            val bg = when {
                                line.startsWith("+") && !line.startsWith("+++") -> Color(0x1F00B894)
                                line.startsWith("-") && !line.startsWith("---") -> Color(0x1FE17055)
                                line.startsWith("@@") -> Color(0x2E496F9D)
                                else -> Color.Transparent
                            }
                            val fg = when {
                                line.startsWith("+") && !line.startsWith("+++") -> Color(0xFFB7F9E4)
                                line.startsWith("-") && !line.startsWith("---") -> Color(0xFFFFC5B8)
                                line.startsWith("@@") -> Color(0xFFA7D0FF)
                                line.startsWith("diff ") || line.startsWith("index ") ||
                                    line.startsWith("---") || line.startsWith("+++") -> SailColors.TextMuted
                                else -> SailColors.Text
                            }
                            Text(
                                text = line,
                                color = fg,
                                fontFamily = FontFamily.Monospace,
                                fontSize = 11.sp,
                                maxLines = 1,
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .background(bg)
                                    .padding(horizontal = 10.dp, vertical = 1.dp),
                            )
                        }
                    }
                }
            }
        }
    }
}
