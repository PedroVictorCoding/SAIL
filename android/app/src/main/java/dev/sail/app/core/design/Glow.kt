package dev.sail.app.core.design

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.withTransform
import androidx.compose.ui.unit.dp

/** How much light a surface carries. Tiles are stronger than ordinary cards (DESIGN.md). */
enum class GlowStrength(
    val hueAlpha: Float,
    val hotAlpha: Float,
    val bandHeight: Float,
    val hotHeight: Float,
    val haloElevation: Float,
) {
    None(0f, 0f, 0f, 0f, 0f),
    Weak(0.42f, 0.30f, 1.2f, 0.7f, 14f),
    Tile(0.62f, 0.55f, 1.0f, 0.55f, 18f),
}

val CardBaseBrush = Brush.verticalGradient(
    0.4f to SailColors.CardTop,
    1f to SailColors.CardBottom,
)

/**
 * The SAIL glow material: a near-black card with its hue pooled as light along the bottom
 * edge and a halo of the same hue. Light only comes from below; there's no top highlight and
 * nothing repeats, so no coloured line can appear on the top border.
 */
fun Modifier.glow(
    hue: Hue,
    shape: Shape,
    strength: GlowStrength = GlowStrength.Weak,
): Modifier {
    val base = if (strength.haloElevation > 0f) {
        shadow(
            elevation = strength.haloElevation.dp,
            shape = shape,
            ambientColor = hue.color,
            spotColor = hue.color,
        )
    } else {
        this
    }
    return base
        .clip(shape)
        .background(CardBaseBrush)
        .drawBehind {
            if (strength == GlowStrength.None) return@drawBehind
            val w = size.width
            val h = size.height
            ellipseGlow(
                center = Offset(w / 2f, h + 14.dp.toPx()),
                radiusX = w * 0.95f,
                radiusY = h * strength.bandHeight,
                color = hue.color.copy(alpha = strength.hueAlpha),
            )
            ellipseGlow(
                center = Offset(w / 2f, h + 6.dp.toPx()),
                radiusX = w * 0.55f,
                radiusY = h * strength.hotHeight,
                color = hue.hot.copy(alpha = strength.hotAlpha),
            )
        }
        .border(1.dp, SailColors.Line, shape)
}

private fun DrawScope.ellipseGlow(center: Offset, radiusX: Float, radiusY: Float, color: Color) {
    if (radiusX <= 0f || radiusY <= 0f) return
    withTransform({ scale(1f, radiusY / radiusX, pivot = center) }) {
        drawCircle(
            brush = Brush.radialGradient(
                colors = listOf(color, Color.Transparent),
                center = center,
                radius = radiusX,
            ),
            radius = radiusX,
            center = center,
        )
    }
}
