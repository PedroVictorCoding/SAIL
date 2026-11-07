package dev.sail.app.feature.home

import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.sail.app.core.LocalAppContainer
import dev.sail.app.core.UiState
import dev.sail.app.core.design.Chevron
import dev.sail.app.core.design.EmptyState
import dev.sail.app.core.design.Eyebrow
import dev.sail.app.core.design.FlatCard
import dev.sail.app.core.design.GlowCard
import dev.sail.app.core.design.GlowStrength
import dev.sail.app.core.design.Hue
import dev.sail.app.core.design.LoadingSkeleton
import dev.sail.app.core.design.SailBrandBar
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.SailIcon
import dev.sail.app.core.design.SailIconImage
import dev.sail.app.core.design.SailPill
import dev.sail.app.core.design.ScreenGutter
import dev.sail.app.core.design.SecondaryButton
import dev.sail.app.core.design.animationsEnabled
import dev.sail.app.core.sailViewModel

@Composable
fun HomeScreen(
    onOpenTracker: () -> Unit,
    onOpenArena: () -> Unit,
    onOpenCourse: (courseId: String) -> Unit,
    onOpenLesson: (lessonId: String) -> Unit,
    onOpenLibrary: () -> Unit,
    onOpenBook: (bookId: String) -> Unit,
    onOpenDialogue: () -> Unit,
    onOpenSettings: () -> Unit,
) {
    val container = LocalAppContainer.current
    val repository = remember(container.api) { HomeRepository(container.api) }
    val viewModel: HomeViewModel = sailViewModel(key = "home") { HomeViewModel(repository) }
    val uiState by viewModel.state.collectAsState()
    val isRefreshing by viewModel.isRefreshing.collectAsState()
    val connection by container.connection.state.collectAsState()

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(SailColors.BgVoid),
    ) {
        SailBrandBar(
            connection = connection,
            onSettings = onOpenSettings,
        )

        when (val current = uiState) {
            is UiState.Loading -> {
                Column(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(horizontal = ScreenGutter, vertical = 16.dp),
                ) {
                    LoadingSkeleton(lines = 5)
                }
            }

            is UiState.Empty -> {
                Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(horizontal = ScreenGutter),
                    contentAlignment = Alignment.Center,
                ) {
                    EmptyState(
                        title = "Return briefing empty",
                        detail = current.reason,
                        action = {
                            SecondaryButton(
                                text = "Refresh",
                                onClick = { viewModel.refresh(forceRegenerateBriefing = true) },
                            )
                        },
                    )
                }
            }

            is UiState.Unavailable -> {
                Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(horizontal = ScreenGutter),
                    contentAlignment = Alignment.Center,
                ) {
                    EmptyState(
                        title = "SAIL host unavailable",
                        detail = current.message,
                        action = {
                            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                                SecondaryButton(
                                    text = "Retry",
                                    onClick = { viewModel.refresh() },
                                )
                                SecondaryButton(
                                    text = "Server settings",
                                    onClick = onOpenSettings,
                                )
                            }
                        },
                    )
                }
            }

            is UiState.Ready -> {
                HomeReadyContent(
                    dashboard = current.value,
                    isRefreshing = isRefreshing,
                    onRefresh = { viewModel.refresh(forceRegenerateBriefing = true) },
                    onDismissBriefing = { viewModel.dismissBriefing() },
                    onPlayMusic = { uri -> viewModel.playMusic(uri) },
                    onOpenTracker = onOpenTracker,
                    onOpenArena = onOpenArena,
                    onOpenCourse = onOpenCourse,
                    onOpenLesson = onOpenLesson,
                    onOpenLibrary = onOpenLibrary,
                    onOpenDialogue = onOpenDialogue,
                )
            }
        }
    }
}

@Composable
private fun HomeReadyContent(
    dashboard: HomeDashboard,
    isRefreshing: Boolean,
    onRefresh: () -> Unit,
    onDismissBriefing: () -> Unit,
    onPlayMusic: (String) -> Unit,
    onOpenTracker: () -> Unit,
    onOpenArena: () -> Unit,
    onOpenCourse: (String) -> Unit,
    onOpenLesson: (String) -> Unit,
    onOpenLibrary: () -> Unit,
    onOpenDialogue: () -> Unit,
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(
            start = ScreenGutter,
            end = ScreenGutter,
            top = 8.dp,
            bottom = 28.dp,
        ),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        item(key = "hero") {
            HomeHeroHeader(
                greeting = dashboard.greeting,
                headline = dashboard.headline,
                localTimeStr = dashboard.localTimeStr,
                awayMinutes = dashboard.awayMinutes,
                onTapOrb = onOpenDialogue,
            )
        }

        if (!dashboard.actionBanner.isNullOrBlank()) {
            item(key = "banner") {
                FlatCard(padding = PaddingValues(horizontal = 14.dp, vertical = 10.dp)) {
                    Text(
                        text = dashboard.actionBanner,
                        style = MaterialTheme.typography.bodyMedium,
                        color = SailColors.InkSecondary,
                    )
                }
            }
        }

        item(key = "music_arena_row") {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(IntrinsicSize.Max),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                HomeMusicCard(
                    model = dashboard.music,
                    onPlayMusic = onPlayMusic,
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxHeight(),
                )
                HomeArenaCard(
                    model = dashboard.arena,
                    onOpenArena = onOpenArena,
                    onOpenCourse = onOpenCourse,
                    onOpenLesson = onOpenLesson,
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxHeight(),
                )
            }
        }

        item(key = "system_summary") {
            HomeSystemSummaryCard(model = dashboard.system)
        }

        item(key = "tracker_entry") {
            HomeTrackerEntryCard(onOpenTracker = onOpenTracker)
        }

        if (dashboard.eventsToWatch.isNotEmpty()) {
            item(key = "events_to_watch") {
                HomeEventsCard(events = dashboard.eventsToWatch)
            }
        }

        if (dashboard.newsRadar.isNotEmpty()) {
            item(key = "news_radar") {
                HomeNewsRadarCard(
                    items = dashboard.newsRadar,
                    onOpenTracker = onOpenTracker,
                )
            }
        }

        item(key = "quick_actions") {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 2.dp),
                horizontalArrangement = Arrangement.spacedBy(10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                SecondaryButton(
                    text = if (isRefreshing) "Refreshing…" else "Refresh briefing",
                    onClick = onRefresh,
                    enabled = !isRefreshing,
                )
                SecondaryButton(
                    text = "Library",
                    onClick = onOpenLibrary,
                )
                if (!dashboard.briefingDismissed &&
                    (dashboard.eventsToWatch.isNotEmpty() || dashboard.newsRadar.isNotEmpty())
                ) {
                    Spacer(modifier = Modifier.weight(1f))
                    Text(
                        text = "Dismiss",
                        style = MaterialTheme.typography.labelMedium,
                        color = SailColors.InkMuted,
                        modifier = Modifier
                            .clickable(role = Role.Button, onClick = onDismissBriefing)
                            .padding(horizontal = 8.dp, vertical = 6.dp),
                    )
                }
            }
        }
    }
}

@Composable
private fun HomeHeroHeader(
    greeting: String,
    headline: String,
    localTimeStr: String,
    awayMinutes: Long?,
    onTapOrb: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = 6.dp, bottom = 6.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        AmbientReturnOrb(
            modifier = Modifier
                .size(92.dp)
                .clickable(
                    role = Role.Button,
                    onClickLabel = "Open Dialogue",
                    onClick = onTapOrb,
                ),
        )
        Spacer(modifier = Modifier.height(14.dp))
        Text(
            text = greeting,
            style = MaterialTheme.typography.displayLarge.copy(
                fontSize = 32.sp,
                lineHeight = 38.sp,
                fontWeight = FontWeight.Bold,
            ),
            color = SailColors.InkPrimary,
            textAlign = TextAlign.Center,
            modifier = Modifier.semantics { heading() },
        )
        Spacer(modifier = Modifier.height(4.dp))
        Text(
            text = headline,
            style = MaterialTheme.typography.bodyLarge.copy(
                fontSize = 16.sp,
                lineHeight = 22.sp,
            ),
            color = SailColors.InkSecondary,
            textAlign = TextAlign.Center,
        )
        if (localTimeStr.isNotBlank() || (awayMinutes != null && awayMinutes > 0L)) {
            Spacer(modifier = Modifier.height(8.dp))
            val meta = buildString {
                if (localTimeStr.isNotBlank()) append(localTimeStr)
                if (awayMinutes != null && awayMinutes > 0L) {
                    if (isNotEmpty()) append(" · ")
                    append("Away ${awayMinutes}m")
                }
            }
            SailPill(
                text = meta,
                hue = SailColors.Dialogue,
            )
        }
    }
}

@Composable
private fun AmbientReturnOrb(modifier: Modifier = Modifier) {
    val animate = animationsEnabled()
    val pulse = if (animate) {
        val transition = rememberInfiniteTransition(label = "homeOrb")
        val value by transition.animateFloat(
            initialValue = 0.94f,
            targetValue = 1.06f,
            animationSpec = infiniteRepeatable(
                animation = tween(durationMillis = 3600, easing = FastOutSlowInEasing),
                repeatMode = RepeatMode.Reverse,
            ),
            label = "orbPulse",
        )
        value
    } else {
        1f
    }

    Canvas(
        modifier = modifier.semantics {
            contentDescription = "SAIL living light orb"
            role = Role.Image
        },
    ) {
        val center = Offset(size.width / 2f, size.height / 2f)
        val outerRadius = (size.minDimension / 2f) * pulse
        val coreRadius = size.minDimension * 0.34f

        drawCircle(
            brush = Brush.radialGradient(
                colors = listOf(
                    SailColors.Dialogue.copy(alpha = 0.42f),
                    SailColors.Learn.copy(alpha = 0.22f),
                    Color.Transparent,
                ),
                center = center,
                radius = outerRadius,
            ),
            radius = outerRadius,
            center = center,
        )

        drawCircle(
            brush = Brush.radialGradient(
                colors = listOf(
                    Color(0xFFE0F7FA),
                    SailColors.Dialogue,
                    Color(0xFF1D4ED8),
                    Color(0xFF0A192F),
                ),
                center = Offset(center.x - coreRadius * 0.22f, center.y - coreRadius * 0.24f),
                radius = coreRadius * 1.35f,
            ),
            radius = coreRadius,
            center = center,
        )

        drawCircle(
            color = Color.White.copy(alpha = 0.36f),
            radius = coreRadius,
            center = center,
            style = Stroke(width = 1.2.dp.toPx()),
        )
    }
}

@Composable
private fun HomeMusicCard(
    model: HomeMusicCardModel,
    onPlayMusic: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val playableUri = model.playableUri
    GlowCard(
        hue = Hue.Music,
        strength = GlowStrength.Soft,
        modifier = modifier,
        padding = PaddingValues(16.dp),
        onClick = if (playableUri != null) {
            { onPlayMusic(playableUri) }
        } else {
            null
        },
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Eyebrow(
                text = "MUSIC",
                color = SailColors.Music,
            )
            if (!model.badge.isNullOrBlank()) {
                SailPill(
                    text = if (model.confirmed) "PLAYING" else model.badge,
                    hue = SailColors.Music,
                )
            }
        }
        Spacer(modifier = Modifier.height(10.dp))
        Text(
            text = model.title,
            style = MaterialTheme.typography.titleMedium.copy(
                fontSize = 18.sp,
                lineHeight = 23.sp,
                fontWeight = FontWeight.Bold,
            ),
            color = SailColors.InkPrimary,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
        Spacer(modifier = Modifier.height(6.dp))
        Text(
            text = model.subtitle,
            style = MaterialTheme.typography.bodyMedium.copy(
                fontSize = 13.sp,
                lineHeight = 18.sp,
            ),
            color = SailColors.InkSecondary,
            maxLines = 3,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun HomeArenaCard(
    model: HomeArenaCardModel,
    onOpenArena: () -> Unit,
    onOpenCourse: (String) -> Unit,
    onOpenLesson: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val handleClick: () -> Unit = {
        val lessonId = model.nextLessonId
        val courseId = model.courseId
        when {
            !lessonId.isNullOrBlank() -> onOpenLesson(lessonId)
            !courseId.isNullOrBlank() -> onOpenCourse(courseId)
            else -> onOpenArena()
        }
    }

    GlowCard(
        hue = Hue.Learn,
        strength = GlowStrength.Soft,
        modifier = modifier,
        padding = PaddingValues(16.dp),
        onClick = handleClick,
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Eyebrow(
                text = "ARENA",
                color = SailColors.Learn,
            )
            if (model.dueCount > 0) {
                SailPill(
                    text = "${model.dueCount} DUE",
                    hue = SailColors.Learn,
                )
            }
        }
        Spacer(modifier = Modifier.height(10.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = model.title,
                style = MaterialTheme.typography.titleMedium.copy(
                    fontSize = 18.sp,
                    lineHeight = 23.sp,
                    fontWeight = FontWeight.Bold,
                ),
                color = SailColors.InkPrimary,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Chevron(tint = SailColors.InkSecondary)
        }
        Spacer(modifier = Modifier.height(6.dp))
        Text(
            text = model.subtitle,
            style = MaterialTheme.typography.bodyMedium.copy(
                fontSize = 13.sp,
                lineHeight = 18.sp,
            ),
            color = SailColors.InkSecondary,
            maxLines = 3,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun HomeSystemSummaryCard(model: HomeSystemCardModel) {
    GlowCard(
        hue = Hue.System,
        strength = GlowStrength.Medium,
        padding = PaddingValues(18.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Eyebrow(
                text = "SYSTEM SUMMARY",
                color = SailColors.System,
            )
            if (model.cpuLabel != null) {
                SailPill(
                    text = "CPU ${model.cpuLabel}",
                    hue = SailColors.System,
                    dot = if (model.isHealthy) SailColors.Success else SailColors.Warning,
                )
            }
        }
        Spacer(modifier = Modifier.height(10.dp))
        Text(
            text = model.workspaceName,
            style = MaterialTheme.typography.titleLarge.copy(
                fontSize = 22.sp,
                lineHeight = 28.sp,
                fontWeight = FontWeight.Bold,
            ),
            color = SailColors.InkPrimary,
        )
        Spacer(modifier = Modifier.height(2.dp))
        Text(
            text = model.statusSummary,
            style = MaterialTheme.typography.bodyMedium.copy(
                fontSize = 14.sp,
                lineHeight = 20.sp,
            ),
            color = SailColors.InkSecondary,
        )
        Spacer(modifier = Modifier.height(14.dp))
        HorizontalDivider(
            thickness = 1.dp,
            color = SailColors.BorderSubtle,
        )
        Spacer(modifier = Modifier.height(12.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = "Storage",
                    style = MaterialTheme.typography.labelMedium,
                    color = SailColors.InkMuted,
                )
                Spacer(modifier = Modifier.height(3.dp))
                Text(
                    text = model.storageLabel,
                    style = MaterialTheme.typography.titleMedium.copy(
                        fontFamily = FontFamily.Monospace,
                        fontWeight = FontWeight.SemiBold,
                        fontSize = 15.sp,
                    ),
                    color = SailColors.InkPrimary,
                )
            }
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = "Memory",
                    style = MaterialTheme.typography.labelMedium,
                    color = SailColors.InkMuted,
                )
                Spacer(modifier = Modifier.height(3.dp))
                Text(
                    text = model.memoryLabel,
                    style = MaterialTheme.typography.titleMedium.copy(
                        fontFamily = FontFamily.Monospace,
                        fontWeight = FontWeight.SemiBold,
                        fontSize = 15.sp,
                    ),
                    color = SailColors.InkPrimary,
                )
            }
        }

        if (!model.forgeSummary.isNullOrBlank() || !model.lastEditedFile.isNullOrBlank()) {
            Spacer(modifier = Modifier.height(12.dp))
            HorizontalDivider(
                thickness = 1.dp,
                color = SailColors.BorderSubtle,
            )
            Spacer(modifier = Modifier.height(10.dp))
            val footer = listOfNotNull(model.forgeSummary, model.lastEditedFile)
                .joinToString(separator = " · ")
            Text(
                text = footer,
                style = MaterialTheme.typography.labelMedium,
                color = SailColors.InkSecondary,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
private fun HomeTrackerEntryCard(onOpenTracker: () -> Unit) {
    GlowCard(
        hue = Hue.Tracker,
        strength = GlowStrength.Soft,
        padding = PaddingValues(16.dp),
        onClick = onOpenTracker,
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                SailIconImage(
                    icon = SailIcon.Pulse,
                    tint = SailColors.Tracker,
                    size = 18.dp,
                )
                Eyebrow(
                    text = "SITUATION ROOM",
                    color = SailColors.Tracker,
                )
            }
            SailPill(
                text = "LIVE",
                hue = SailColors.Tracker,
                dot = SailColors.Success,
            )
        }
        Spacer(modifier = Modifier.height(8.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = "Open Tracker",
                    style = MaterialTheme.typography.titleMedium.copy(
                        fontSize = 18.sp,
                        fontWeight = FontWeight.Bold,
                    ),
                    color = SailColors.InkPrimary,
                )
                Spacer(modifier = Modifier.height(3.dp))
                Text(
                    text = "Anomalies, live markets, world map pins, and clustered news signals.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = SailColors.InkSecondary,
                )
            }
            Chevron(tint = SailColors.InkSecondary)
        }
    }
}

@Composable
private fun HomeEventsCard(events: List<BriefingEvent>) {
    GlowCard(
        hue = Hue.Warning,
        strength = GlowStrength.Soft,
        padding = PaddingValues(16.dp),
    ) {
        Eyebrow(
            text = "EVENTS TO WATCH",
            color = SailColors.Warning,
        )
        Spacer(modifier = Modifier.height(10.dp))
        Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
            events.forEach { event ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(
                            text = event.title,
                            style = MaterialTheme.typography.bodyLarge.copy(fontWeight = FontWeight.SemiBold),
                            color = SailColors.InkPrimary,
                        )
                        val meta = listOfNotNull(
                            event.dueRelative.takeIf { it.isNotBlank() },
                            event.source.takeIf { it.isNotBlank() },
                        ).joinToString(" · ")
                        if (meta.isNotEmpty()) {
                            Text(
                                text = meta,
                                style = MaterialTheme.typography.labelMedium,
                                color = SailColors.InkSecondary,
                            )
                        }
                    }
                    SailPill(
                        text = event.urgency.uppercase(),
                        hue = if (event.urgency.equals("high", ignoreCase = true)) {
                            SailColors.Danger
                        } else {
                            SailColors.Warning
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun HomeNewsRadarCard(
    items: List<BriefingNewsItem>,
    onOpenTracker: () -> Unit,
) {
    GlowCard(
        hue = Hue.Dialogue,
        strength = GlowStrength.Soft,
        padding = PaddingValues(16.dp),
        onClick = onOpenTracker,
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Eyebrow(
                text = "NEWS RADAR",
                color = SailColors.Dialogue,
            )
            Chevron(tint = SailColors.InkSecondary)
        }
        Spacer(modifier = Modifier.height(10.dp))
        Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
            items.forEach { item ->
                Column {
                    Text(
                        text = item.title,
                        style = MaterialTheme.typography.bodyLarge.copy(fontWeight = FontWeight.SemiBold),
                        color = SailColors.InkPrimary,
                    )
                    if (item.summary.isNotBlank()) {
                        Spacer(modifier = Modifier.height(2.dp))
                        Text(
                            text = item.summary,
                            style = MaterialTheme.typography.bodyMedium,
                            color = SailColors.InkSecondary,
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    val meta = listOfNotNull(
                        item.source.takeIf { it.isNotBlank() },
                        item.relevantInterest.takeIf { it.isNotBlank() },
                    ).joinToString(" · ")
                    if (meta.isNotEmpty()) {
                        Spacer(modifier = Modifier.height(2.dp))
                        Text(
                            text = meta,
                            style = MaterialTheme.typography.labelMedium,
                            color = SailColors.InkMuted,
                        )
                    }
                }
            }
        }
    }
}
