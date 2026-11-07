package dev.sail.app.feature.workspace

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.LifecycleStartEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.sail.app.core.design.CardBaseBrush
import dev.sail.app.core.design.CardShape
import dev.sail.app.core.design.EmptyState
import dev.sail.app.core.design.Eyebrow
import dev.sail.app.core.design.FlatCard
import dev.sail.app.core.design.GlowCard
import dev.sail.app.core.design.Hue
import dev.sail.app.core.design.LoadingSkeleton
import dev.sail.app.core.design.PillShape
import dev.sail.app.core.design.RowShape
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.SailIcon
import dev.sail.app.core.design.SailPill
import dev.sail.app.core.design.SailProgress
import dev.sail.app.core.design.SailRow
import dev.sail.app.core.design.SailTopBar
import dev.sail.app.core.design.ScreenGutter
import dev.sail.app.core.design.SecondaryButton
import dev.sail.app.core.design.SectionHeader
import dev.sail.app.core.design.SegmentedControl
import dev.sail.app.core.design.StatTile
import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.sailViewModel

private enum class WorkspacePane { Overview, Topology, Resources }

private val MAP_POSITIONS = mapOf(
    "mind" to Offset(0.22f, 0.18f),
    "system" to Offset(0.78f, 0.16f),
    "memory" to Offset(0.16f, 0.52f),
    "core" to Offset(0.50f, 0.50f),
    "flow" to Offset(0.84f, 0.52f),
    "arena" to Offset(0.24f, 0.84f),
    "forge" to Offset(0.74f, 0.84f),
)

@Composable
fun WorkspaceScreen() {
    val vm = sailViewModel { WorkspaceViewModel(WorkspaceRepository(it.api)) }
    val data by vm.state.collectAsStateWithLifecycle()
    var paneIndex by rememberSaveable { mutableIntStateOf(0) }
    var mode by rememberSaveable { mutableIntStateOf(0) }
    var selectedId by rememberSaveable { mutableStateOf<String?>("core") }
    val pane = WorkspacePane.entries[paneIndex]

    LifecycleStartEffect(Unit) {
        vm.start()
        onStopOrDispose { vm.stop() }
    }

    Column(Modifier.fillMaxSize()) {
        SailTopBar(
            title = when (pane) {
                WorkspacePane.Overview -> "Workspace"
                WorkspacePane.Topology -> "Topology"
                WorkspacePane.Resources -> "Resources"
            },
            subtitle = if (pane == WorkspacePane.Overview) "SAIL Core" else null,
            onBack = if (pane == WorkspacePane.Overview) null else ({ paneIndex = 0 }),
            actions = { WorkspaceRefreshAction(refreshing = data.loading, onRefresh = vm::refresh) },
        )
        when (pane) {
            WorkspacePane.Overview -> Overview(
                data = data,
                mode = mode,
                onMode = { mode = it },
                selectedId = selectedId,
                onSelect = { selectedId = it },
                onOpenTopology = { paneIndex = 1 },
                onOpenResources = { paneIndex = 2 },
                onRetry = vm::refresh,
            )
            WorkspacePane.Topology -> TopologyPane(data)
            WorkspacePane.Resources -> ResourcesPane(data, onRetry = vm::refresh)
        }
    }
}

@Composable
private fun Overview(
    data: WorkspaceData,
    mode: Int,
    onMode: (Int) -> Unit,
    selectedId: String?,
    onSelect: (String) -> Unit,
    onOpenTopology: () -> Unit,
    onOpenResources: () -> Unit,
    onRetry: () -> Unit,
) {
    val subsystems = remember(data) { buildSubsystems(data) }
    val selected = subsystems.firstOrNull { it.id == selectedId }
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(start = ScreenGutter, end = ScreenGutter, bottom = 28.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    data.updatedAt?.let { "Updated ${clockLabel(it)}" } ?: "Live subsystem status",
                    modifier = Modifier.weight(1f),
                    fontSize = 12.sp,
                    color = SailColors.TextSubtle,
                )
                if (data.loading) {
                    Text("Refreshing…", fontSize = 12.sp, color = SailColors.TextSubtle)
                }
            }
        }
        item {
            SegmentedControl(
                options = listOf("Map", "List"),
                selected = mode,
                onSelect = onMode,
                hue = Hue.Neutral,
            )
        }
        if (data.isEmpty && !data.loading) {
            item {
                EmptyState(
                    title = "Workspace status unavailable",
                    detail = workspaceError(data) ?: "SAIL did not answer the status calls.",
                    onRetry = onRetry,
                )
            }
        } else if (data.isEmpty) {
            item { LoadingSkeleton(lines = 6) }
        } else {
            item {
                if (mode == 0) {
                    SubsystemMap(
                        subsystems = subsystems,
                        selectedId = selectedId,
                        onSelect = onSelect,
                    )
                } else {
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        for (subsystem in subsystems) {
                            SubsystemRow(subsystem = subsystem, onSelect = onSelect)
                        }
                    }
                }
            }
            if (selected != null) {
                item { SubsystemDetail(subsystem = selected, data = data) }
            }
            item {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    SecondaryButton(
                        text = "Topology",
                        onClick = onOpenTopology,
                        modifier = Modifier.weight(1f),
                    )
                    SecondaryButton(
                        text = "Resources",
                        onClick = onOpenResources,
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        }
    }
}

@Composable
private fun SubsystemMap(
    subsystems: List<SubsystemStatus>,
    selectedId: String?,
    onSelect: (String) -> Unit,
) {
    val byId = subsystems.associateBy { it.id }
    BoxWithConstraints(
        Modifier
            .fillMaxWidth()
            .height(360.dp)
            .clip(CardShape)
            .background(CardBaseBrush)
            .border(1.dp, SailColors.Line, CardShape),
    ) {
        Canvas(Modifier.fillMaxSize()) {
            val width = size.width
            val height = size.height
            for ((from, to) in WORKSPACE_LINKS) {
                val a = MAP_POSITIONS[from] ?: continue
                val b = MAP_POSITIONS[to] ?: continue
                val fromState = byId[from]?.state ?: LiveState.NoStatus
                val toState = byId[to]?.state ?: LiveState.NoStatus
                val lineColor = when {
                    fromState == LiveState.NoStatus || toState == LiveState.NoStatus ->
                        Color.White.copy(alpha = 0.08f)
                    fromState == LiveState.Offline || toState == LiveState.Offline ->
                        SailColors.Error.copy(alpha = 0.2f)
                    else -> Color.White.copy(alpha = 0.2f)
                }
                drawLine(
                    color = lineColor,
                    start = Offset(width * a.x, height * a.y),
                    end = Offset(width * b.x, height * b.y),
                    strokeWidth = 1.dp.toPx(),
                )
            }
            for ((id, position) in MAP_POSITIONS) {
                val subsystem = byId[id] ?: continue
                val centerX = width * position.x
                val centerY = height * position.y
                val radiusX = 42.dp.toPx()
                val radiusY = 15.dp.toPx()
                val diamond = Path().apply {
                    moveTo(centerX, centerY - radiusY)
                    lineTo(centerX + radiusX, centerY)
                    lineTo(centerX, centerY + radiusY)
                    lineTo(centerX - radiusX, centerY)
                    close()
                }
                val hue = hueFor(id).color
                drawPath(
                    path = diamond,
                    color = hue.copy(alpha = if (subsystem.state == LiveState.NoStatus) 0.06f else 0.16f),
                )
                drawPath(
                    path = diamond,
                    color = hue.copy(alpha = 0.22f),
                    style = Stroke(width = 1.dp.toPx()),
                )
            }
        }
        for (subsystem in subsystems) {
            val position = MAP_POSITIONS[subsystem.id] ?: continue
            Box(
                modifier = Modifier
                    .offset(
                        x = maxWidth * position.x - 42.dp,
                        y = maxHeight * position.y - 24.dp,
                    )
                    .width(84.dp)
                    .heightIn(min = 48.dp)
                    .clip(RowShape)
                    .background(Color(0xF20B0D12))
                    .border(
                        width = 1.dp,
                        color = if (subsystem.id == selectedId) SailColors.Focus else SailColors.Line,
                        shape = RowShape,
                    )
                    .clickable(
                        onClickLabel = "Inspect ${subsystem.name}",
                        role = Role.Button,
                    ) { onSelect(subsystem.id) }
                    .padding(horizontal = 4.dp, vertical = 5.dp),
                contentAlignment = Alignment.Center,
            ) {
                Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Box(
                            Modifier
                                .size(6.dp)
                                .clip(CircleShape)
                                .background(stateColor(subsystem.state)),
                        )
                        Spacer(Modifier.width(4.dp))
                        Text(
                            subsystem.name,
                            fontSize = 10.sp,
                            fontWeight = FontWeight.SemiBold,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    Text(
                        subsystem.state.label,
                        fontSize = 9.sp,
                        color = SailColors.TextSubtle,
                        maxLines = 1,
                    )
                }
            }
        }
    }
}

@Composable
private fun SubsystemRow(subsystem: SubsystemStatus, onSelect: (String) -> Unit) {
    SailRow(
        title = subsystem.name,
        meta = subsystem.detail,
        icon = iconFor(subsystem.id),
        hue = hueFor(subsystem.id),
        onClick = { onSelect(subsystem.id) },
        trailing = { SailPill(subsystem.state.label, dot = stateColor(subsystem.state)) },
    )
}

@Composable
private fun SubsystemDetail(subsystem: SubsystemStatus, data: WorkspaceData) {
    GlowCard(hue = hueFor(subsystem.id)) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Eyebrow(subsystem.name, dot = stateColor(subsystem.state))
            Spacer(Modifier.weight(1f))
            SailPill(subsystem.state.label, dot = stateColor(subsystem.state))
        }
        Spacer(Modifier.height(6.dp))
        Text(subsystem.detail, fontSize = 13.sp, color = SailColors.TextMuted)
        Spacer(Modifier.height(10.dp))
        when (subsystem.id) {
            "core" -> CoreDetail(data)
            "mind" -> MindDetail(data)
            "system" -> SystemDetail(data)
            "forge" -> ForgeDetail(data)
            else -> Text(
                "This build has no live status endpoint for ${subsystem.name}.",
                fontSize = 12.sp,
                color = SailColors.TextSubtle,
            )
        }
    }
}

@Composable
private fun CoreDetail(data: WorkspaceData) {
    when (val result = data.health) {
        is ApiResult.Ok -> {
            val health = result.data
            DetailRow("Speech", "${health.speech?.readyCount ?: 0}/3 ready")
            DetailRow("LLM", if (health.llm?.ready == true) "Ready" else "Not ready")
            shortModel(health.llm?.model)?.let { DetailRow("Model", it) }
            DetailRow("Sample rate", health.sampleRate?.let { "$it Hz" } ?: "—")
        }
        is ApiResult.Err -> Text(result.message, fontSize = 12.sp, color = SailColors.Warning)
        null -> Text("No health response yet.", fontSize = 12.sp, color = SailColors.TextSubtle)
    }
}

@Composable
private fun MindDetail(data: WorkspaceData) {
    when (val result = data.mind) {
        is ApiResult.Ok -> {
            val mind = result.data
            DetailRow("Accepting requests", if (mind.acceptingRequests) "Yes" else "No")
            DetailRow("Models", "${mind.models.count { it.state == "ready" }}/${mind.models.size} ready")
            mind.metrics?.let {
                DetailRow("Inference", "${it.active} active · ${it.queued} queued")
            }
            for (model in mind.models.take(4)) {
                Row(Modifier.fillMaxWidth().padding(vertical = 3.dp)) {
                    Text(
                        shortModel(model.model) ?: model.model,
                        modifier = Modifier.weight(1f),
                        fontSize = 12.sp,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        if (model.hasError) "error" else model.state,
                        fontSize = 12.sp,
                        color = if (model.hasError) SailColors.Error else SailColors.TextMuted,
                    )
                }
            }
            when (val catalog = data.catalog) {
                is ApiResult.Ok -> {
                    val models = catalog.data
                    DetailRow("Running", shortModel(models.running) ?: models.runningFile ?: "none")
                    DetailRow("Default", shortModel(models.default) ?: models.default ?: "—")
                    DetailRow(
                        "Installed",
                        "${models.models.count { it.installed }} of ${models.models.size}",
                    )
                    if (models.restartRequired) {
                        Text(
                            "Restart SAIL to apply the default model.",
                            fontSize = 11.sp,
                            color = SailColors.Warning,
                        )
                    }
                }
                is ApiResult.Err -> DetailRow("Catalog", catalog.message)
                null -> Unit
            }
        }
        is ApiResult.Err -> Text(result.message, fontSize = 12.sp, color = SailColors.Warning)
        null -> Text("No Mind response yet.", fontSize = 12.sp, color = SailColors.TextSubtle)
    }
}

@Composable
private fun SystemDetail(data: WorkspaceData) {
    when (val result = data.metrics) {
        is ApiResult.Ok -> {
            val metrics = result.data
            DetailRow("CPU", "${formatPercent(metrics.cpuPercent)} · ${metrics.cpuCores ?: "—"} cores")
            DetailRow("Load", formatDecimal(metrics.load1))
            DetailRow("RAM", "${formatBytes(metrics.ramUsedBytes)} of ${formatBytes(metrics.ramTotalBytes)}")
            DetailRow(
                "Swap",
                "${formatBytes(metrics.swapUsedBytes)} of ${formatBytes(metrics.swapTotalBytes)}",
            )
            DetailRow("Uptime", formatUptime(metrics.uptimeSecs))
            metrics.version?.let { DetailRow("Version", it) }
            for (disk in metrics.disks) {
                Spacer(Modifier.height(6.dp))
                Text(disk.path, fontSize = 11.sp, color = SailColors.TextSubtle)
                Spacer(Modifier.height(4.dp))
                SailProgress(
                    fraction = disk.usedFraction,
                    hue = if (disk.usedFraction >= 0.9f) Hue.Warning else Hue.System,
                )
                Text(
                    "${formatBytes(disk.usedBytes)} of ${formatBytes(disk.totalBytes)}",
                    fontSize = 11.sp,
                    color = SailColors.TextSubtle,
                )
            }
            metrics.gpus.firstOrNull()?.let { gpu ->
                DetailRow("GPU", "${gpu.card} · ${formatPercent(gpu.busyPercent)}")
            }
        }
        is ApiResult.Err -> Text(result.message, fontSize = 12.sp, color = SailColors.Warning)
        null -> Text("No system metrics yet.", fontSize = 12.sp, color = SailColors.TextSubtle)
    }
}

@Composable
private fun ForgeDetail(data: WorkspaceData) {
    when (val result = data.plans) {
        is ApiResult.Ok -> {
            val plans = result.data.plans
            DetailRow("Plans", plans.size.toString())
            if (plans.isEmpty()) {
                Text("No plans yet.", fontSize = 12.sp, color = SailColors.TextSubtle)
            }
            for (view in plans.take(5)) {
                Row(Modifier.fillMaxWidth().padding(vertical = 3.dp)) {
                    Text(
                        view.plan.goal.ifBlank { view.plan.id },
                        modifier = Modifier.weight(1f),
                        fontSize = 12.sp,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        humanPlanState(view.plan.state),
                        fontSize = 11.sp,
                        color = SailColors.TextMuted,
                    )
                }
            }
            if (plans.size > 5) {
                Text("+${plans.size - 5} more", fontSize = 11.sp, color = SailColors.TextSubtle)
            }
        }
        is ApiResult.Err -> Text(result.message, fontSize = 12.sp, color = SailColors.Warning)
        null -> Text("No plans response yet.", fontSize = 12.sp, color = SailColors.TextSubtle)
    }
}

@Composable
private fun TopologyPane(data: WorkspaceData) {
    val subsystems = remember(data) { buildSubsystems(data) }
    val links = remember(subsystems) { buildTopology(subsystems) }
    val byId = subsystems.associateBy { it.id }
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(start = ScreenGutter, end = ScreenGutter, bottom = 28.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item {
            Text(
                "Structure comes from SAIL's subsystem map. Live state is shown only where this build has an endpoint.",
                fontSize = 13.sp,
                color = SailColors.TextMuted,
            )
        }
        items(links, key = { "${it.from}:${it.to}" }) { link ->
            val from = byId[link.from]
            val to = byId[link.to]
            if (from != null && to != null) {
                SailRow(
                    title = "${from.name} ↔ ${to.name}",
                    meta = link.detail,
                    icon = iconFor(link.from),
                    hue = hueFor(link.from),
                    trailing = { SailPill(link.state.label, dot = stateColor(link.state)) },
                )
            }
        }
        item {
            FlatCard {
                Eyebrow("Not on this map")
                Spacer(Modifier.height(6.dp))
                Text(
                    "Dialogue has no node in this build. Memory, Flow and Arena appear but stay gray because this client has no live status endpoint for them yet.",
                    fontSize = 13.sp,
                    color = SailColors.TextMuted,
                )
            }
        }
    }
}

@Composable
private fun ResourcesPane(data: WorkspaceData, onRetry: () -> Unit) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(start = ScreenGutter, end = ScreenGutter, bottom = 28.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item { SectionHeader("Host") }
        when (val result = data.metrics) {
            is ApiResult.Ok -> {
                val metrics = result.data
                item {
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        StatTile(
                            value = formatPercent(metrics.cpuPercent),
                            label = "CPU",
                            hue = Hue.System,
                            modifier = Modifier.weight(1f),
                        )
                        StatTile(
                            value = formatBytes(metrics.ramUsedBytes),
                            label = "RAM used",
                            hue = Hue.System,
                            modifier = Modifier.weight(1f),
                        )
                        StatTile(
                            value = formatUptime(metrics.uptimeSecs),
                            label = "Uptime",
                            hue = Hue.Library,
                            modifier = Modifier.weight(1f),
                        )
                    }
                }
                if (metrics.gpus.isNotEmpty()) {
                    item {
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                            for (gpu in metrics.gpus.take(2)) {
                                StatTile(
                                    value = formatPercent(gpu.busyPercent),
                                    label = "GPU ${gpu.card}",
                                    hue = Hue.Forge,
                                    modifier = Modifier.weight(1f),
                                )
                            }
                        }
                    }
                }
                if (metrics.disks.isNotEmpty()) {
                    item {
                        FlatCard {
                            for (disk in metrics.disks) {
                                Eyebrow("Disk ${disk.path}")
                                Spacer(Modifier.height(4.dp))
                                SailProgress(
                                    fraction = disk.usedFraction,
                                    hue = if (disk.usedFraction >= 0.9f) Hue.Warning else Hue.System,
                                )
                                Spacer(Modifier.height(4.dp))
                                Text(
                                    "${formatBytes(disk.usedBytes)} of ${formatBytes(disk.totalBytes)} (${formatPercent(disk.usedFraction.toDouble() * 100)})",
                                    fontSize = 11.sp,
                                    color = SailColors.TextSubtle,
                                )
                                Spacer(Modifier.height(8.dp))
                            }
                        }
                    }
                }
            }
            is ApiResult.Err -> item {
                EmptyState(
                    title = "Host metrics unavailable",
                    detail = result.message,
                    onRetry = onRetry,
                )
            }
            null -> item { LoadingSkeleton(lines = 4) }
        }

        item { SectionHeader("Mind") }
        when (val result = data.mind) {
            is ApiResult.Ok -> {
                val mind = result.data
                item {
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        StatTile(
                            value = "${mind.models.count { it.state == "ready" }}/${mind.models.size}",
                            label = "Models ready",
                            hue = Hue.Learn,
                            modifier = Modifier.weight(1f),
                        )
                        StatTile(
                            value = "${mind.metrics?.active ?: 0} / ${mind.metrics?.queued ?: 0}",
                            label = "Active / queued",
                            hue = Hue.Learn,
                            modifier = Modifier.weight(1f),
                        )
                        StatTile(
                            value = "${mind.limits?.maxParallel ?: 0}",
                            label = "Max parallel",
                            hue = Hue.Learn,
                            modifier = Modifier.weight(1f),
                        )
                    }
                }
                items(mind.models, key = { it.model }) { model ->
                    SailRow(
                        title = shortModel(model.model) ?: model.model,
                        meta = if (model.hasError) "${model.state} · last error" else model.state,
                        hue = Hue.Learn,
                        trailing = {
                            SailPill(
                                if (model.hasError) "error" else model.state,
                                dot = if (model.hasError) SailColors.Error else SailColors.Good,
                            )
                        },
                    )
                }
                val catalog = data.catalog.valueOrNull()
                if (catalog != null) {
                    item { SectionHeader("Model catalog") }
                    items(catalog.models.filter { it.installed }, key = { it.id }) { model ->
                        SailRow(
                            title = model.name.ifBlank { model.id },
                            meta = listOfNotNull(
                                model.sizeGb?.let { "${formatDecimal(it)} GB" },
                                if (model.running) "running" else null,
                                if (model.default) "default" else null,
                            ).joinToString(" · "),
                            hue = Hue.Learn,
                            trailing = { SailPill(if (model.running) "running" else "installed") },
                        )
                    }
                }
            }
            is ApiResult.Err -> item {
                EmptyState(
                    title = "Mind status unavailable",
                    detail = result.message,
                    onRetry = onRetry,
                )
            }
            null -> item { LoadingSkeleton(lines = 4) }
        }

        item { SectionHeader("Limits") }
        val mind = data.mind.valueOrNull()
        val limits = mind?.limits
        if (limits != null) {
            item {
                FlatCard {
                    DetailRow("Max parallel", limits.maxParallel.toString())
                    DetailRow("Max queued", limits.maxQueued.toString())
                    DetailRow("Max output tokens", limits.maxOutputTokens.toString())
                    DetailRow("Queue timeout", "${limits.queueTimeoutMs / 1000}s")
                    DetailRow("Inference timeout", "${limits.inferenceTimeoutMs / 1000}s")
                }
            }
        }
        val resources = mind?.resources
        if (resources != null) {
            item {
                FlatCard {
                    Eyebrow("Admission")
                    Spacer(Modifier.height(6.dp))
                    DetailRow("Voice turns", resources.voiceTurns.toString())
                    DetailRow("Admitted inference", resources.admittedInference.toString())
                    DetailRow("Speech workers", resources.speechWorkers.toString())
                    DetailRow("RAM headroom", formatBytes(resources.ramHeadroomBytes))
                    DetailRow("Resident models", resources.residentHeavyModels.toString())
                    DetailRow("Rejected", resources.rejected.toString())
                    DetailRow("Preemptions", resources.preemptions.toString())
                }
            }
        }
    }
}

@Composable
private fun DetailRow(label: String, value: String) {
    Row(Modifier.fillMaxWidth().padding(vertical = 3.dp)) {
        Text(
            label,
            modifier = Modifier.weight(1f),
            fontSize = 12.sp,
            color = SailColors.TextMuted,
        )
        Text(
            value,
            fontSize = 12.sp,
            color = SailColors.Text,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun WorkspaceRefreshAction(refreshing: Boolean, onRefresh: () -> Unit) {
    Box(
        modifier = Modifier
            .heightIn(min = 48.dp)
            .clip(PillShape)
            .clickable(enabled = !refreshing, role = Role.Button, onClick = onRefresh)
            .padding(horizontal = 10.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            if (refreshing) "Refreshing…" else "Refresh",
            fontSize = 13.sp,
            color = if (refreshing) SailColors.TextSubtle else SailColors.TextMuted,
        )
    }
}

private fun stateColor(state: LiveState): Color = when (state) {
    LiveState.Active -> SailColors.Good
    LiveState.Idle -> SailColors.TextSubtle
    LiveState.Attention -> SailColors.Warning
    LiveState.Offline -> SailColors.Error
    LiveState.NoStatus -> SailColors.TextSubtle
}

private fun iconFor(id: String): SailIcon = when (id) {
    "core" -> SailIcon.Core
    "mind" -> SailIcon.Mind
    "system" -> SailIcon.Sense
    "forge" -> SailIcon.Forge
    "memory" -> SailIcon.Mesh
    "flow" -> SailIcon.Flow
    "arena" -> SailIcon.Arena
    else -> SailIcon.Spaces
}

private fun hueFor(id: String): Hue = when (id) {
    "mind" -> Hue.Learn
    "system" -> Hue.System
    "forge" -> Hue.Forge
    "arena" -> Hue.Arena
    "memory" -> Hue.Library
    else -> Hue.Neutral
}
