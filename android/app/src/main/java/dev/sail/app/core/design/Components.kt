package dev.sail.app.core.design

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import android.provider.Settings as AndroidSettings

val CardShape = RoundedCornerShape(20.dp)
val TileShape = RoundedCornerShape(16.dp)
val RowShape = RoundedCornerShape(14.dp)
val PillShape = RoundedCornerShape(999.dp)

/** Screen padding: 16 dp gutters, like the web layout. */
val ScreenGutter = 16.dp

/** A weak glow card. Pass [onClick] to make the whole card a button. */
@Composable
fun GlowCard(
    hue: Hue,
    modifier: Modifier = Modifier,
    strength: GlowStrength = GlowStrength.Weak,
    onClick: (() -> Unit)? = null,
    onClickLabel: String? = null,
    content: @Composable ColumnScope.() -> Unit,
) {
    val clickable = if (onClick != null) {
        Modifier.clickable(onClickLabel = onClickLabel, role = Role.Button, onClick = onClick)
    } else {
        Modifier
    }
    Column(
        modifier = modifier
            .glow(hue, CardShape, strength)
            .then(clickable)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        content = content,
    )
}

/** A flat card for reading, code and lesson surfaces: no glow, no motion (DESIGN.md). */
@Composable
fun FlatCard(modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    Column(
        modifier = modifier
            .glow(Hue.Neutral, CardShape, GlowStrength.None)
            .padding(16.dp),
        content = content,
    )
}

/** "MUSIC", "SYSTEM SUMMARY": the small label at the top of a card. */
@Composable
fun Eyebrow(text: String, modifier: Modifier = Modifier, dot: Color? = null) {
    Row(modifier = modifier, verticalAlignment = Alignment.CenterVertically) {
        if (dot != null) {
            Box(Modifier.size(7.dp).clip(CircleShape).background(dot))
            Spacer(Modifier.width(6.dp))
        }
        Text(text.uppercase(), style = EyebrowStyle, color = Color.White.copy(alpha = 0.82f))
    }
}

/** A stat tile: one real measurement with its label. [value] must come from an API. */
@Composable
fun StatTile(
    value: String,
    label: String,
    hue: Hue,
    modifier: Modifier = Modifier,
    icon: SailIcon? = null,
) {
    Column(
        modifier = modifier
            .glow(hue, TileShape, GlowStrength.Tile)
            .padding(start = 12.dp, end = 12.dp, top = 12.dp, bottom = 20.dp)
            .semantics(mergeDescendants = true) {},
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            if (icon != null) {
                SailIconImage(icon, contentDescription = null, tint = hue.hot, size = 18.dp)
                Spacer(Modifier.width(6.dp))
            }
            Text(value, fontSize = 22.sp, fontWeight = FontWeight.Bold, color = Color.White, maxLines = 1)
        }
        Text(label, fontSize = 11.sp, color = Color.White.copy(alpha = 0.74f), maxLines = 2)
    }
}

/** A list row: icon, title, meta line and a chevron when it opens something. */
@Composable
fun SailRow(
    title: String,
    modifier: Modifier = Modifier,
    meta: String? = null,
    icon: SailIcon? = null,
    hue: Hue = Hue.Neutral,
    onClick: (() -> Unit)? = null,
    leading: (@Composable () -> Unit)? = null,
    trailing: (@Composable RowScope.() -> Unit)? = null,
) {
    val click = if (onClick != null) Modifier.clickable(role = Role.Button, onClick = onClick) else Modifier
    Row(
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = 56.dp)
            .clip(RowShape)
            .background(CardBaseBrush)
            .border(1.dp, SailColors.Line, RowShape)
            .then(click)
            .padding(horizontal = 14.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        when {
            leading != null -> {
                leading()
                Spacer(Modifier.width(12.dp))
            }
            icon != null -> {
                Box(
                    modifier = Modifier
                        .size(40.dp)
                        .clip(RoundedCornerShape(12.dp))
                        .background(hue.color.copy(alpha = 0.28f)),
                    contentAlignment = Alignment.Center,
                ) {
                    SailIconImage(icon, contentDescription = null, size = 20.dp)
                }
                Spacer(Modifier.width(12.dp))
            }
        }
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.titleMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
            if (meta != null) {
                Text(meta, fontSize = 12.sp, color = SailColors.TextMuted, maxLines = 2, overflow = TextOverflow.Ellipsis)
            }
        }
        if (trailing != null) {
            trailing()
        } else if (onClick != null) {
            Chevron()
        }
    }
}

@Composable
fun Chevron(modifier: Modifier = Modifier) {
    Text("›", modifier = modifier.padding(start = 8.dp), fontSize = 22.sp, color = SailColors.TextSubtle)
}

/** A progress bar for a real measured fraction (0..1). Never decorative. */
@Composable
fun SailProgress(fraction: Float, hue: Hue, modifier: Modifier = Modifier) {
    val clamped = fraction.coerceIn(0f, 1f)
    Box(
        modifier = modifier
            .fillMaxWidth()
            .height(4.dp)
            .clip(PillShape)
            .background(Color.White.copy(alpha = 0.1f)),
    ) {
        if (clamped > 0f) {
            Box(
                Modifier
                    .fillMaxWidth(clamped)
                    .height(4.dp)
                    .clip(PillShape)
                    .background(hue.color),
            )
        }
    }
}

/** Segmented control ("Map | List", "All | Books | Notes | Media"). */
@Composable
fun SegmentedControl(
    options: List<String>,
    selected: Int,
    onSelect: (Int) -> Unit,
    modifier: Modifier = Modifier,
    hue: Hue = Hue.Neutral,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clip(PillShape)
            .background(Color(0xB30A0C10))
            .border(1.dp, SailColors.Line, PillShape)
            .padding(4.dp)
            .selectableGroup(),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        options.forEachIndexed { index, label ->
            val isSelected = index == selected
            Box(
                modifier = Modifier
                    .weight(1f)
                    .heightIn(min = 40.dp)
                    .clip(PillShape)
                    .background(if (isSelected) hue.color.copy(alpha = 0.45f) else Color.Transparent)
                    .selectable(selected = isSelected, role = Role.Tab, onClick = { onSelect(index) }),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    label,
                    fontSize = 14.sp,
                    fontWeight = if (isSelected) FontWeight.SemiBold else FontWeight.Normal,
                    color = if (isSelected) Color.White else SailColors.TextMuted,
                )
            }
        }
    }
}

/** Plain dark pill; state is shown by the dot only (DESIGN.md: pills don't glow). */
@Composable
fun SailPill(text: String, modifier: Modifier = Modifier, dot: Color? = null) {
    Row(
        modifier = modifier
            .clip(PillShape)
            .background(Color(0x990A0C10))
            .border(1.dp, SailColors.LineStrong, PillShape)
            .padding(horizontal = 10.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (dot != null) {
            Box(Modifier.size(7.dp).clip(CircleShape).background(dot))
            Spacer(Modifier.width(6.dp))
        }
        Text(text, fontSize = 12.sp, color = SailColors.TextMuted)
    }
}

@Composable
fun PrimaryButton(text: String, onClick: () -> Unit, modifier: Modifier = Modifier, enabled: Boolean = true) {
    Box(
        modifier = modifier
            .heightIn(min = 48.dp)
            .clip(PillShape)
            .alpha(if (enabled) 1f else 0.5f)
            .background(Brush.horizontalGradient(listOf(SailColors.PrimaryStart, SailColors.PrimaryEnd)))
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .padding(horizontal = 20.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, color = Color(0xFF0B0D12), fontWeight = FontWeight.SemiBold, fontSize = 16.sp)
    }
}

@Composable
fun SecondaryButton(text: String, onClick: () -> Unit, modifier: Modifier = Modifier, enabled: Boolean = true) {
    Box(
        modifier = modifier
            .heightIn(min = 48.dp)
            .clip(PillShape)
            .alpha(if (enabled) 1f else 0.5f)
            .background(SailColors.SurfaceRaised)
            .border(1.dp, SailColors.LineStrong, PillShape)
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .padding(horizontal = 18.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, color = SailColors.Text, fontWeight = FontWeight.SemiBold)
    }
}

/** Honest empty / unavailable state, with an optional retry. */
@Composable
fun EmptyState(
    title: String,
    modifier: Modifier = Modifier,
    detail: String? = null,
    onRetry: (() -> Unit)? = null,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .clip(RowShape)
            .border(1.dp, SailColors.LineStrong, RowShape)
            .padding(horizontal = 16.dp, vertical = 18.dp)
            .semantics(mergeDescendants = true) {},
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(title, fontWeight = FontWeight.SemiBold, color = SailColors.Text)
        if (detail != null) {
            Spacer(Modifier.height(4.dp))
            Text(detail, fontSize = 13.sp, color = SailColors.TextMuted)
        }
        if (onRetry != null) {
            Spacer(Modifier.height(12.dp))
            SecondaryButton("Retry", onClick = onRetry)
        }
    }
}

/** Placeholder lines while loading. Static when the system turns animations off. */
@Composable
fun LoadingSkeleton(modifier: Modifier = Modifier, lines: Int = 3) {
    val animate = animationsEnabled()
    val shimmer = if (animate) {
        val transition = rememberInfiniteTransition(label = "skeleton")
        val value by transition.animateFloat(
            initialValue = 0.05f,
            targetValue = 0.11f,
            animationSpec = infiniteRepeatable(tween(900, easing = LinearEasing), RepeatMode.Reverse),
            label = "skeleton-alpha",
        )
        value
    } else {
        0.08f
    }
    Column(modifier.semantics { }, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        repeat(lines) { index ->
            Box(
                Modifier
                    .fillMaxWidth(if (index % 3 == 2) 0.6f else 1f)
                    .height(14.dp)
                    .clip(RoundedCornerShape(7.dp))
                    .background(Color.White.copy(alpha = shimmer)),
            )
        }
    }
}

@Composable
fun SectionHeader(
    title: String,
    modifier: Modifier = Modifier,
    action: String? = null,
    onAction: (() -> Unit)? = null,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .padding(top = 18.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            title,
            modifier = Modifier.weight(1f).semantics { heading() },
            style = MaterialTheme.typography.titleLarge,
        )
        if (action != null && onAction != null) {
            Text(
                "$action ›",
                modifier = Modifier
                    .clip(PillShape)
                    .clickable(role = Role.Button, onClick = onAction)
                    .padding(horizontal = 8.dp, vertical = 12.dp),
                fontSize = 13.sp,
                color = SailColors.TextMuted,
            )
        }
    }
}

/** False when the owner turned animations off (Settings › Accessibility › Remove animations). */
@Composable
fun animationsEnabled(): Boolean {
    val resolver = LocalContext.current.contentResolver
    val scale = AndroidSettings.Global.getFloat(resolver, AndroidSettings.Global.ANIMATOR_DURATION_SCALE, 1f)
    return scale != 0f
}
