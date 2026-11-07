package dev.sail.app.core.design

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/** Colours from crates/sail-dialogue/web/tokens.css and the glow material (DESIGN.md). */
object SailColors {
    val Base = Color(0xFF030407)
    val Surface = Color(0xFF141820)
    val SurfaceRaised = Color(0xFF1D232C)
    val CardTop = Color(0xE006070A)
    val CardBottom = Color(0xEB0B0D12)
    val Line = Color(0x14FFFFFF)
    val LineStrong = Color(0x24FFFFFF)
    val Text = Color(0xFFF2F5FA)
    val TextMuted = Color(0xFFAEB8C8)
    val TextSubtle = Color(0xFF8592A6)
    val Focus = Color(0xFFFFD69F)

    val Good = Color(0xFF00B894)
    val Warning = Color(0xFFE5BE79)
    val Error = Color(0xFFE17055)
    val Listening = Color(0xFF19B5BC)
    val Thinking = Color(0xFF8567FF)
    val Speaking = Color(0xFFFF7A45)
    val Executing = Color(0xFF2D7FF9)
    val Recording = Color(0xFFE84118)

    val GlowHot = Color(0xFFDFE4FF)
    val PrimaryStart = Color(0xFFA99BFF)
    val PrimaryEnd = Color(0xFF8A7DFF)
}

/** One hue per subsystem, the same ones the desktop glow cards use. */
enum class Hue(val color: Color, val hot: Color = SailColors.GlowHot) {
    Dialogue(Color(0xFF2D7FF9)),
    Arena(Color(0xFFFF2F86), Color(0xFFFF9BD0)),
    Learn(Color(0xFF8567FF), Color(0xFFB9A8FF)),
    Music(Color(0xFFFF7A1A), Color(0xFFFFD45A)),
    System(Color(0xFF22D84A), Color(0xFFC8FF6A)),
    Library(Color(0xFF5B5CFF), Color(0xFFB9A8FF)),
    Tracker(Color(0xFF14D6C8), Color(0xFFB5FFF6)),
    Forge(Color(0xFFFF9A1A), Color(0xFFFFD45A)),
    Neutral(Color(0xFF5B7CFF)),
    Warning(Color(0xFFE5BE79), Color(0xFFFFE7B8)),
    Error(Color(0xFFE17055), Color(0xFFFFC2B0)),
}

private val SailTypography = Typography().let { base ->
    base.copy(
        headlineMedium = base.headlineMedium.copy(fontWeight = FontWeight.SemiBold, fontSize = 26.sp, letterSpacing = (-0.5).sp),
        titleLarge = base.titleLarge.copy(fontWeight = FontWeight.SemiBold, fontSize = 20.sp),
        titleMedium = base.titleMedium.copy(fontWeight = FontWeight.SemiBold, fontSize = 16.sp),
        bodyLarge = base.bodyLarge.copy(fontSize = 15.sp, lineHeight = 22.sp),
        bodyMedium = base.bodyMedium.copy(fontSize = 14.sp, lineHeight = 20.sp),
        labelSmall = base.labelSmall.copy(fontSize = 11.sp, fontWeight = FontWeight.Bold, letterSpacing = 0.9.sp),
    )
}

/** Small caps-like label above a card's content ("MUSIC", "SYSTEM SUMMARY"). */
val EyebrowStyle = TextStyle(fontSize = 11.sp, fontWeight = FontWeight.Bold, letterSpacing = 0.9.sp)

@Composable
fun SailTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = darkColorScheme(
            primary = SailColors.PrimaryStart,
            onPrimary = Color(0xFF0B0D12),
            background = SailColors.Base,
            onBackground = SailColors.Text,
            surface = SailColors.Surface,
            onSurface = SailColors.Text,
            surfaceVariant = SailColors.SurfaceRaised,
            onSurfaceVariant = SailColors.TextMuted,
            outline = SailColors.LineStrong,
            error = SailColors.Error,
        ),
        typography = SailTypography,
        content = content,
    )
}
