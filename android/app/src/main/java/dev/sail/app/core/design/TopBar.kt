package dev.sail.app.core.design

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.sail.app.core.net.ConnectionState

/**
 * A screen's title bar: back button (when [onBack] is set), centred title and subtitle, and
 * optional [actions] on the right. Each screen draws its own bar.
 */
@Composable
fun SailTopBar(
    title: String,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    onBack: (() -> Unit)? = null,
    actions: (@Composable RowScope.() -> Unit)? = null,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = 56.dp)
            .padding(horizontal = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(48.dp), contentAlignment = Alignment.Center) {
            if (onBack != null) {
                Box(
                    modifier = Modifier
                        .size(48.dp)
                        .clip(CircleShape)
                        .clickable(onClickLabel = "Back", role = Role.Button, onClick = onBack),
                    contentAlignment = Alignment.Center,
                ) {
                    Text("‹", fontSize = 30.sp, color = SailColors.Text)
                }
            }
        }
        Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
            Text(
                title,
                modifier = Modifier.semantics { heading() },
                fontSize = 17.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                textAlign = TextAlign.Center,
            )
            if (subtitle != null) {
                Text(subtitle, fontSize = 12.sp, color = SailColors.TextMuted, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
        Row(Modifier.heightIn(min = 48.dp).padding(end = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            if (actions != null) actions() else Spacer(Modifier.width(40.dp))
        }
    }
}

/** Home's bar: the nabla mark and wordmark on the left, connection pill on the right. */
@Composable
fun SailBrandBar(
    connection: ConnectionState,
    modifier: Modifier = Modifier,
    onSettings: (() -> Unit)? = null,
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = 56.dp)
            .padding(horizontal = ScreenGutter),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        SailIconImage(SailIcon.Mark, contentDescription = null, size = 22.dp)
        Spacer(Modifier.width(10.dp))
        Text("SAIL", fontWeight = FontWeight.Bold, letterSpacing = 5.sp, fontSize = 16.sp)
        Spacer(Modifier.weight(1f))
        ConnectionPill(connection)
        if (onSettings != null) {
            Box(
                modifier = Modifier
                    .size(48.dp)
                    .clip(CircleShape)
                    .clickable(onClickLabel = "Settings", role = Role.Button, onClick = onSettings),
                contentAlignment = Alignment.Center,
            ) {
                SailIconImage(SailIcon.Settings, contentDescription = "Settings", size = 20.dp, tint = SailColors.TextMuted)
            }
        }
    }
}

@Composable
fun ConnectionPill(state: ConnectionState, modifier: Modifier = Modifier) {
    val (label, dot) = when (state) {
        ConnectionState.Online -> "Online" to SailColors.Good
        ConnectionState.Checking -> "Checking" to SailColors.TextSubtle
        ConnectionState.NotConfigured -> "Not set up" to SailColors.Warning
        ConnectionState.Unauthorized -> "Token refused" to SailColors.Error
        is ConnectionState.Offline -> "Offline" to SailColors.Warning
    }
    SailPill(label, modifier = modifier, dot = dot)
}
