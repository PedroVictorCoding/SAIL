package dev.sail.app.feature.tracker

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.graphics.ColorMatrix
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import coil.compose.AsyncImage
import dev.sail.app.core.LocalAppContainer
import dev.sail.app.core.UiState
import dev.sail.app.core.design.CardShape
import dev.sail.app.core.design.EmptyState
import dev.sail.app.core.design.Eyebrow
import dev.sail.app.core.design.GlowCard
import dev.sail.app.core.design.GlowStrength
import dev.sail.app.core.design.Hue
import dev.sail.app.core.design.LoadingSkeleton
import dev.sail.app.core.design.PillShape
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.SailPill
import dev.sail.app.core.design.SailTopBar
import dev.sail.app.core.design.ScreenGutter
import dev.sail.app.core.design.SecondaryButton
import dev.sail.app.core.sailViewModel
import java.util.Locale
import kotlin.math.roundToInt

@Composable
fun TrackerScreen(onBack: () -> Unit) {
    val container = LocalAppContainer.current
    val repository = remember(container.api) { TrackerRepository(container.api) }
    val viewModel: TrackerViewModel = sailViewModel(key = "tracker") { TrackerViewModel(repository) }
    val uiState by viewModel.state.collectAsState()
    val isRefreshing by viewModel.isRefreshing.collectAsState()

    val readyDashboard = (uiState as? UiState.Ready)?.data
    val subtitle = readyDashboard?.clockSubtitle ?: "UTC"
    val isLive = readyDashboard?.isLive ?: false

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(SailColors.Base),
    ) {
        SailTopBar(
            title = "Tracker",
            subtitle = subtitle,
            onBack = onBack,
            actions = {
                SailPill(
                    text = if (isLive) "LIVE" else "SYNC",
                    dot = if (isLive) SailColors.Good else SailColors.Warning,
                )
            },
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
                        title = current.title,
                        detail = current.detail,
                        onRetry = { viewModel.refreshNow() },
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
                        title = "Tracker unavailable",
                        detail = current.message,
                        onRetry = { viewModel.load() },
                    )
                }
            }

            is UiState.Ready -> {
                TrackerReadyContent(
                    dashboard = current.data,
                    isRefreshing = isRefreshing,
                    onSelectTopic = { topic -> viewModel.selectTopic(topic) },
                    onRefresh = { viewModel.refreshNow() },
                )
            }
        }
    }
}

@Composable
private fun TrackerReadyContent(
    dashboard: TrackerDashboard,
    isRefreshing: Boolean,
    onSelectTopic: (String?) -> Unit,
    onRefresh: () -> Unit,
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(
            start = ScreenGutter,
            end = ScreenGutter,
            top = 10.dp,
            bottom = 28.dp,
        ),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        item(key = "top_summary_row") {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(IntrinsicSize.Max),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                TrackerAnomaliesCard(
                    model = dashboard.anomaliesCard,
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxHeight(),
                )
                TrackerMarketsCard(
                    model = dashboard.marketsCard,
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxHeight(),
                )
            }
        }

        item(key = "world_map") {
            TrackerMapSnapshotCard(model = dashboard.mapCard)
        }

        item(key = "news_signals") {
            TrackerNewsCard(
                stories = dashboard.filteredStories,
                topics = dashboard.topics,
                selectedTopic = dashboard.selectedTopic,
                onSelectTopic = onSelectTopic,
            )
        }

        if (dashboard.anomaliesCard.items.isNotEmpty()) {
            item(key = "anomaly_feed") {
                TrackerAnomalyFeedCard(anomalies = dashboard.anomaliesCard.items)
            }
        }

        if (dashboard.marketsCard.quotes.size > 1 || dashboard.marketsCard.fx.isNotEmpty()) {
            item(key = "markets_tape") {
                TrackerMarketsTapeCard(
                    quotes = dashboard.marketsCard.quotes,
                    fx = dashboard.marketsCard.fx,
                    odds = dashboard.marketsCard.odds,
                )
            }
        }

        if (dashboard.conditions.isNotEmpty() || dashboard.quakes.isNotEmpty()) {
            item(key = "world_conditions") {
                TrackerWorldConditionsCard(
                    conditions = dashboard.conditions,
                    quakes = dashboard.quakes,
                )
            }
        }

        item(key = "tracker_footer") {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 4.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                SecondaryButton(
                    text = if (isRefreshing) "Refreshing…" else "Refresh board",
                    onClick = onRefresh,
                    enabled = !isRefreshing,
                )
                if (dashboard.sourcesTotalCount > 0) {
                    Text(
                        text = "${dashboard.sourcesHealthyCount}/${dashboard.sourcesTotalCount} sources online",
                        style = MaterialTheme.typography.labelSmall,
                        color = SailColors.TextSubtle,
                    )
                }
            }
        }
    }
}

@Composable
private fun TrackerAnomaliesCard(
    model: TrackerAnomaliesCardModel,
    modifier: Modifier = Modifier,
) {
    GlowCard(
        hue = Hue.Error,
        strength = GlowStrength.Tile,
        modifier = modifier,
    ) {
        Eyebrow(
            text = "ANOMALIES",
            dot = SailColors.Error,
        )
        Spacer(modifier = Modifier.height(10.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = model.countLabel,
                style = MaterialTheme.typography.titleLarge.copy(
                    fontSize = 20.sp,
                    lineHeight = 25.sp,
                    fontWeight = FontWeight.Bold,
                ),
                color = SailColors.Text,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            if (!model.topSigmaLabel.isNullOrBlank()) {
                Spacer(modifier = Modifier.width(6.dp))
                SailPill(
                    text = model.topSigmaLabel,
                    dot = SailColors.Error,
                )
            }
        }
        Spacer(modifier = Modifier.height(6.dp))
        Text(
            text = model.topHeadline,
            style = MaterialTheme.typography.bodyMedium.copy(
                fontSize = 13.sp,
                lineHeight = 18.sp,
            ),
            color = SailColors.TextMuted,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun TrackerMarketsCard(
    model: TrackerMarketsCardModel,
    modifier: Modifier = Modifier,
) {
    val pillDot = if (model.isPositive) Hue.Tracker.color else SailColors.Error
    GlowCard(
        hue = Hue.Tracker,
        strength = GlowStrength.Tile,
        modifier = modifier,
    ) {
        Eyebrow(
            text = "MARKETS",
            dot = Hue.Tracker.color,
        )
        Spacer(modifier = Modifier.height(10.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = model.headline,
                style = MaterialTheme.typography.titleLarge.copy(
                    fontSize = 20.sp,
                    lineHeight = 25.sp,
                    fontWeight = FontWeight.Bold,
                ),
                color = SailColors.Text,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            if (!model.changePctLabel.isNullOrBlank()) {
                Spacer(modifier = Modifier.width(6.dp))
                SailPill(
                    text = model.changePctLabel,
                    dot = pillDot,
                )
            }
        }
        Spacer(modifier = Modifier.height(6.dp))
        Text(
            text = model.subtitle,
            style = MaterialTheme.typography.bodyMedium.copy(
                fontSize = 13.sp,
                lineHeight = 18.sp,
            ),
            color = SailColors.TextMuted,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun TrackerMapSnapshotCard(model: TrackerMapCardModel) {
    val container = LocalAppContainer.current
    val darkTileFilter = remember {
        ColorFilter.colorMatrix(
            ColorMatrix(
                floatArrayOf(
                    0.18f, 0f, 0f, 0f, 4f,
                    0f, 0.34f, 0f, 0f, 16f,
                    0f, 0f, 0.38f, 0f, 24f,
                    0f, 0f, 0f, 0.85f, 0f,
                ),
            ),
        )
    }

    GlowCard(
        hue = Hue.Tracker,
        strength = GlowStrength.Weak,
    ) {
        BoxWithConstraints(
            modifier = Modifier
                .fillMaxWidth()
                .height(216.dp)
                .clip(CardShape)
                .background(
                    Brush.verticalGradient(
                        colors = listOf(
                            Color(0xFF07191E),
                            Color(0xFF061318),
                            Color(0xFF040B0F),
                        ),
                    ),
                )
                .semantics {
                    contentDescription = "World tracker map with ${model.pins.size} monitored places"
                },
        ) {
            val mapWidth = maxWidth
            val mapHeight = maxHeight
            val cols = model.viewport.cols.coerceAtLeast(1)
            val rows = model.viewport.rows.coerceAtLeast(1)

            Column(modifier = Modifier.fillMaxSize()) {
                for (row in 0 until rows) {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .weight(1f),
                    ) {
                        for (col in 0 until cols) {
                            val idx = row * cols + col
                            val url = model.tileUrls.getOrNull(idx)
                            Box(
                                modifier = Modifier
                                    .weight(1f)
                                    .fillMaxHeight(),
                            ) {
                                if (!url.isNullOrBlank()) {
                                    AsyncImage(
                                        model = url,
                                        imageLoader = container.imageLoader,
                                        contentDescription = null,
                                        contentScale = ContentScale.Crop,
                                        colorFilter = darkTileFilter,
                                        modifier = Modifier.fillMaxSize(),
                                    )
                                }
                            }
                        }
                    }
                }
            }

            Canvas(modifier = Modifier.fillMaxSize()) {
                val w = size.width
                val h = size.height

                for (i in 1..5) {
                    val x = w * (i / 6f)
                    drawLine(
                        color = Hue.Tracker.color.copy(alpha = 0.10f),
                        start = Offset(x, 0f),
                        end = Offset(x, h),
                        strokeWidth = 1.dp.toPx(),
                    )
                }
                for (j in 1..3) {
                    val y = h * (j / 4f)
                    drawLine(
                        color = Hue.Tracker.color.copy(alpha = 0.10f),
                        start = Offset(0f, y),
                        end = Offset(w, y),
                        strokeWidth = 1.dp.toPx(),
                    )
                }

                model.routes.forEach { route ->
                    if (route.points.size >= 2) {
                        val path = Path()
                        route.points.forEachIndexed { idx, pt ->
                            val px = pt.x * w
                            val py = pt.y * h
                            if (idx == 0) path.moveTo(px, py) else path.lineTo(px, py)
                        }
                        drawPath(
                            path = path,
                            color = Hue.Dialogue.color.copy(alpha = 0.65f),
                            style = Stroke(width = 2.dp.toPx(), cap = StrokeCap.Round),
                        )
                    }
                }

                model.quakes.forEach { quake ->
                    val center = Offset(quake.point.x * w, quake.point.y * h)
                    val radius = (quake.magnitude.toFloat().coerceIn(2.5f, 8.0f) * 2.8f).dp.toPx()
                    drawCircle(
                        color = SailColors.Warning.copy(alpha = 0.24f),
                        radius = radius,
                        center = center,
                    )
                    drawCircle(
                        color = SailColors.Warning.copy(alpha = 0.78f),
                        radius = radius,
                        center = center,
                        style = Stroke(width = 1.2.dp.toPx()),
                    )
                }

                model.aircraftPoints.forEach { ac ->
                    drawCircle(
                        color = Hue.Dialogue.color.copy(alpha = 0.72f),
                        radius = 2.2.dp.toPx(),
                        center = Offset(ac.x * w, ac.y * h),
                    )
                }

                model.pins.forEach { pin ->
                    val center = Offset(pin.point.x * w, pin.point.y * h)
                    drawCircle(
                        color = Hue.Tracker.color.copy(alpha = 0.28f),
                        radius = if (pin.primary) 14.dp.toPx() else 11.dp.toPx(),
                        center = center,
                    )
                    drawCircle(
                        color = Hue.Tracker.color,
                        radius = if (pin.primary) 5.dp.toPx() else 4.dp.toPx(),
                        center = center,
                    )
                    drawCircle(
                        color = Color.White,
                        radius = 1.8.dp.toPx(),
                        center = center,
                    )
                }
            }

            model.pins.forEach { pin ->
                val targetX = (mapWidth * pin.point.x) - 36.dp
                val targetY = (mapHeight * pin.point.y) - 32.dp
                val clampedX = targetX.coerceIn(8.dp, (mapWidth - 96.dp).coerceAtLeast(8.dp))
                val clampedY = targetY.coerceIn(8.dp, (mapHeight - 34.dp).coerceAtLeast(8.dp))

                Row(
                    modifier = Modifier
                        .offset {
                            IntOffset(
                                x = clampedX.roundToPx(),
                                y = clampedY.roundToPx(),
                            )
                        }
                        .clip(PillShape)
                        .background(Color(0xCC08151B))
                        .border(1.dp, Hue.Tracker.color.copy(alpha = 0.45f), PillShape)
                        .padding(horizontal = 8.dp, vertical = 3.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(5.dp),
                ) {
                    Box(
                        modifier = Modifier
                            .size(6.dp)
                            .clip(CircleShape)
                            .background(Hue.Tracker.color),
                    )
                    Text(
                        text = pin.label,
                        style = MaterialTheme.typography.labelSmall.copy(
                            fontSize = 11.sp,
                            fontWeight = FontWeight.SemiBold,
                        ),
                        color = SailColors.Text,
                        maxLines = 1,
                    )
                }
            }

            if (model.airborneTotal > 0 || model.quakes.isNotEmpty()) {
                Row(
                    modifier = Modifier
                        .align(Alignment.BottomEnd)
                        .padding(10.dp),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    if (model.airborneTotal > 0) {
                        SailPill(
                            text = "${model.airborneTotal} airborne",
                            dot = Hue.Dialogue.color,
                        )
                    }
                    if (model.quakes.isNotEmpty()) {
                        SailPill(
                            text = "${model.quakes.size} seismic",
                            dot = SailColors.Warning,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun TrackerNewsCard(
    stories: List<TrackerStory>,
    topics: List<String>,
    selectedTopic: String?,
    onSelectTopic: (String?) -> Unit,
) {
    GlowCard(
        hue = Hue.Learn,
        strength = GlowStrength.Weak,
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Eyebrow(
                text = "NEWS",
                dot = Hue.Learn.color,
            )
            Text(
                text = "·  Top signals",
                style = MaterialTheme.typography.bodyMedium.copy(
                    fontSize = 14.sp,
                    fontWeight = FontWeight.Medium,
                ),
                color = SailColors.TextMuted,
            )
        }

        if (topics.size > 1) {
            Spacer(modifier = Modifier.height(10.dp))
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                TopicFilterChip(
                    label = "All",
                    selected = selectedTopic == null,
                    onClick = { onSelectTopic(null) },
                )
                topics.forEach { topic ->
                    TopicFilterChip(
                        label = topic,
                        selected = selectedTopic.equals(topic, ignoreCase = true),
                        onClick = { onSelectTopic(topic) },
                    )
                }
            }
        }

        Spacer(modifier = Modifier.height(12.dp))

        if (stories.isEmpty()) {
            Text(
                text = "No news stories available for this filter.",
                style = MaterialTheme.typography.bodyMedium,
                color = SailColors.TextMuted,
            )
        } else {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                stories.take(10).forEachIndexed { index, story ->
                    if (index > 0) {
                        HorizontalDivider(
                            thickness = 1.dp,
                            color = SailColors.Line,
                        )
                    }
                    TrackerStoryRow(story = story)
                }
            }
        }
    }
}

@Composable
private fun TopicFilterChip(
    label: String,
    selected: Boolean,
    onClick: () -> Unit,
) {
    val bg = if (selected) Hue.Learn.color.copy(alpha = 0.22f) else SailColors.SurfaceRaised
    val border = if (selected) Hue.Learn.color.copy(alpha = 0.55f) else SailColors.Line
    val textColor = if (selected) SailColors.Text else SailColors.TextMuted

    Box(
        modifier = Modifier
            .clip(PillShape)
            .background(bg)
            .border(1.dp, border, PillShape)
            .clickable(role = Role.Tab, onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 5.dp),
    ) {
        Text(
            text = label.replaceFirstChar {
                if (it.isLowerCase()) it.titlecase(Locale.US) else it.toString()
            },
            style = MaterialTheme.typography.labelSmall,
            color = textColor,
        )
    }
}

@Composable
private fun TrackerStoryRow(story: TrackerStory) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = 2.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Text(
            text = TrackerFormatting.formatStoryTime(story.published),
            style = MaterialTheme.typography.labelSmall.copy(
                fontFamily = FontFamily.Monospace,
                fontSize = 13.sp,
                fontWeight = FontWeight.Medium,
            ),
            color = SailColors.TextSubtle,
            modifier = Modifier.padding(top = 2.dp),
        )
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = story.title,
                style = MaterialTheme.typography.bodyLarge.copy(
                    fontSize = 15.sp,
                    lineHeight = 21.sp,
                    fontWeight = FontWeight.SemiBold,
                ),
                color = SailColors.Text,
            )
            Spacer(modifier = Modifier.height(3.dp))
            val meta = buildString {
                if (story.source.isNotBlank()) append(story.source)
                if (story.topic.isNotBlank()) {
                    if (isNotEmpty()) append(" · ")
                    append(story.topic)
                }
                if (story.also.isNotEmpty()) {
                    if (isNotEmpty()) append(" · ")
                    append("+${story.also.size} source${if (story.also.size == 1) "" else "s"}")
                }
            }
            if (meta.isNotEmpty()) {
                Text(
                    text = meta,
                    style = MaterialTheme.typography.bodyMedium.copy(fontSize = 12.sp),
                    color = SailColors.TextMuted,
                )
            }
        }
    }
}

@Composable
private fun TrackerAnomalyFeedCard(anomalies: List<TrackerAnomaly>) {
    GlowCard(
        hue = Hue.Error,
        strength = GlowStrength.Weak,
    ) {
        Eyebrow(
            text = "ANOMALY LOG",
            dot = SailColors.Error,
        )
        Spacer(modifier = Modifier.height(10.dp))
        Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
            anomalies.take(6).forEach { anomaly ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.Top,
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(
                            text = anomaly.headline,
                            style = MaterialTheme.typography.bodyLarge.copy(fontWeight = FontWeight.SemiBold),
                            color = SailColors.Text,
                        )
                        if (anomaly.detail.isNotBlank()) {
                            Text(
                                text = anomaly.detail,
                                style = MaterialTheme.typography.bodyMedium,
                                color = SailColors.TextMuted,
                            )
                        }
                    }
                    Spacer(modifier = Modifier.width(8.dp))
                    SailPill(
                        text = TrackerFormatting.formatSigma(anomaly.zScore),
                        dot = if (anomaly.severity.equals("critical", ignoreCase = true) ||
                            anomaly.severity.equals("elevated", ignoreCase = true)
                        ) {
                            SailColors.Error
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
private fun TrackerMarketsTapeCard(
    quotes: List<TrackerQuote>,
    fx: List<TrackerFxRate>,
    odds: List<TrackerOddsMarket>,
) {
    GlowCard(
        hue = Hue.Tracker,
        strength = GlowStrength.Weak,
    ) {
        Eyebrow(
            text = "MARKETS & FX",
            dot = Hue.Tracker.color,
        )
        Spacer(modifier = Modifier.height(10.dp))
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            quotes.take(6).forEach { quote ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        text = quote.label.ifBlank { quote.symbol },
                        style = MaterialTheme.typography.bodyMedium.copy(fontWeight = FontWeight.Medium),
                        color = SailColors.Text,
                    )
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            text = TrackerFormatting.formatQuotePrice(quote.price),
                            style = MaterialTheme.typography.bodyMedium.copy(
                                fontFamily = FontFamily.Monospace,
                                fontWeight = FontWeight.SemiBold,
                            ),
                            color = SailColors.Text,
                        )
                        SailPill(
                            text = TrackerFormatting.formatPercentChange(quote.changePct),
                            dot = if (quote.changePct >= 0.0) Hue.Tracker.color else SailColors.Error,
                        )
                    }
                }
            }
            if (fx.isNotEmpty()) {
                HorizontalDivider(thickness = 1.dp, color = SailColors.Line)
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    fx.take(3).forEach { rate ->
                        Column(modifier = Modifier.weight(1f)) {
                            Text(
                                text = "${rate.base}/${rate.quote}",
                                style = MaterialTheme.typography.labelSmall,
                                color = SailColors.TextSubtle,
                            )
                            Text(
                                text = TrackerFormatting.formatFxRate(rate.rate),
                                style = MaterialTheme.typography.bodyMedium.copy(
                                    fontFamily = FontFamily.Monospace,
                                    fontWeight = FontWeight.SemiBold,
                                ),
                                color = SailColors.Text,
                            )
                        }
                    }
                }
            }
            if (odds.isNotEmpty()) {
                HorizontalDivider(thickness = 1.dp, color = SailColors.Line)
                odds.take(2).forEach { market ->
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            text = market.question,
                            style = MaterialTheme.typography.bodyMedium,
                            color = SailColors.TextMuted,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f),
                        )
                        Spacer(modifier = Modifier.width(8.dp))
                        SailPill(
                            text = "${(market.probability * 100.0).roundToInt()}%",
                            dot = Hue.Dialogue.color,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun TrackerWorldConditionsCard(
    conditions: List<TrackerConditions>,
    quakes: List<TrackerQuake>,
) {
    GlowCard(
        hue = Hue.Dialogue,
        strength = GlowStrength.Weak,
    ) {
        Eyebrow(
            text = "WORLD CONDITIONS",
            dot = Hue.Dialogue.color,
        )
        Spacer(modifier = Modifier.height(10.dp))
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            conditions.forEach { cond ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column {
                        Text(
                            text = cond.place,
                            style = MaterialTheme.typography.bodyLarge.copy(fontWeight = FontWeight.SemiBold),
                            color = SailColors.Text,
                        )
                        if (cond.condition.isNotBlank()) {
                            Text(
                                text = cond.condition,
                                style = MaterialTheme.typography.bodyMedium,
                                color = SailColors.TextMuted,
                            )
                        }
                    }
                    Text(
                        text = String.format(Locale.US, "%.1f°C", cond.tempC),
                        style = MaterialTheme.typography.titleMedium.copy(
                            fontFamily = FontFamily.Monospace,
                            fontWeight = FontWeight.SemiBold,
                        ),
                        color = SailColors.Text,
                    )
                }
            }
            if (quakes.isNotEmpty()) {
                if (conditions.isNotEmpty()) {
                    HorizontalDivider(thickness = 1.dp, color = SailColors.Line)
                }
                quakes.take(3).forEach { quake ->
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            text = quake.place,
                            style = MaterialTheme.typography.bodyMedium,
                            color = SailColors.TextMuted,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f),
                        )
                        Spacer(modifier = Modifier.width(8.dp))
                        SailPill(
                            text = String.format(Locale.US, "M%.1f", quake.magnitude),
                            dot = SailColors.Warning,
                        )
                    }
                }
            }
        }
    }
}
