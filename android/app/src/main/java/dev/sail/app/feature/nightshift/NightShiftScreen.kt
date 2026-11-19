package dev.sail.app.feature.nightshift

import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.sail.app.core.LocalAppContainer
import dev.sail.app.core.design.EmptyState
import dev.sail.app.core.design.FlatCard
import dev.sail.app.core.design.LoadingSkeleton
import dev.sail.app.core.design.PrimaryButton
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.ScreenGutter
import dev.sail.app.core.design.SecondaryButton
import dev.sail.app.core.sailViewModel

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun NightShiftScreen(
    onBack: () -> Unit,
    onOpenSettings: () -> Unit,
    onAskInDialogue: () -> Unit,
) {
    val container = LocalAppContainer.current
    val vm: NightShiftViewModel = sailViewModel(key = "nightshift") { NightShiftViewModel(container.api) }
    val state by vm.state.collectAsState()
    var askText by remember { mutableStateOf("") }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(SailColors.Base)
            .padding(horizontal = ScreenGutter, vertical = 14.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            SecondaryButton(text = "Back", onClick = onBack)
            Text(
                text = "Night Shift",
                color = SailColors.Text,
                fontSize = 18.sp,
                fontWeight = FontWeight.SemiBold,
            )
            SecondaryButton(text = "Settings", onClick = onOpenSettings)
        }

        Spacer(Modifier.height(10.dp))

        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            StatusPill("Window ${state.data.tonight.window}")
            val mesh = state.data.mesh
            val meshLabel = when {
                mesh == null || !mesh.available -> "Host only"
                mesh.resting -> "PC (${mesh.name}) resting · using host"
                else -> "PC (${mesh.name}) ready"
            }
            StatusPill(meshLabel)
            if (state.data.external.allowed > 0) {
                StatusPill("External ${state.data.external.usedTonight}/${state.data.external.allowed}")
            }
        }

        state.error?.let { err ->
            Spacer(Modifier.height(8.dp))
            Text(err, color = SailColors.Error, fontSize = 13.sp)
        }
        state.banner?.let { msg ->
            Spacer(Modifier.height(8.dp))
            Text(msg, color = SailColors.Good, fontSize = 13.sp)
        }

        Spacer(Modifier.height(10.dp))

        if (state.loading && state.data.errands.isEmpty()) {
            LoadingSkeleton(lines = 5)
            return
        }

        LazyColumn(
            modifier = Modifier.fillMaxSize(),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            item(key = "ask_box") {
                FlatCard {
                    Text(
                        text = "Queue something for tonight",
                        color = SailColors.TextMuted,
                        fontSize = 12.sp,
                    )
                    Spacer(Modifier.height(6.dp))
                    OutlinedTextField(
                        value = askText,
                        onValueChange = { if (it.length <= 2000) askText = it },
                        placeholder = { Text("SAIL, tonight: …") },
                        modifier = Modifier.fillMaxWidth(),
                        minLines = 2,
                        maxLines = 4,
                    )
                    Spacer(Modifier.height(8.dp))
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text("${askText.length} / 2000", color = SailColors.TextMuted, fontSize = 11.sp)
                        PrimaryButton(
                            text = if (state.submittingAsk) "Sending…" else "Ask for tonight",
                            enabled = !state.submittingAsk && askText.isNotBlank(),
                            onClick = {
                                val toSend = askText
                                askText = ""
                                vm.submitAsk(toSend)
                            },
                        )
                    }
                }
            }

            state.data.morning?.let { morning ->
                item(key = "morning_report") {
                    MorningReportCard(morning)
                }
            }

            val plan = state.data.tonight.plan
            if (plan.items.isNotEmpty() || plan.deferred.isNotEmpty()) {
                item(key = "tonight_plan") {
                    TonightPlanCard(plan)
                }
            }

            if (state.data.errands.isEmpty()) {
                item(key = "empty") {
                    EmptyState(
                        title = "No Night Shift errands yet",
                        detail = "Tell SAIL what you'd like built above, and review its draft work order before tonight.",
                    )
                }
            } else {
                items(state.data.errands, key = { it.id }) { errand ->
                    ErrandCard(
                        errand = errand,
                        busy = errand.id in state.busyIds,
                        diffText = state.diffs[errand.id],
                        onApprove = { order -> vm.approve(errand.id, order) },
                        onReshape = { note -> vm.reshape(errand.id, note) },
                        onRedo = { note -> vm.redo(errand.id, note) },
                        onShip = { vm.ship(errand.id) },
                        onDrop = { vm.drop(errand.id) },
                        onToggleDiff = { vm.toggleDiff(errand.id) },
                        onAskAbout = onAskInDialogue,
                    )
                }
            }
        }
    }
}

@Composable
private fun StatusPill(text: String, color: Color = SailColors.TextMuted) {
    Text(
        text = text,
        color = color,
        fontSize = 12.sp,
        modifier = Modifier
            .background(Color(0x14FFFFFF), RoundedCornerShape(999.dp))
            .border(1.dp, SailColors.Line, RoundedCornerShape(999.dp))
            .padding(horizontal = 10.dp, vertical = 4.dp),
    )
}

@Composable
private fun MorningReportCard(morning: MorningReport) {
    val context = LocalContext.current
    FlatCard {
        Text(
            text = "Morning report · ${morning.date}",
            color = SailColors.Text,
            fontSize = 15.sp,
            fontWeight = FontWeight.SemiBold,
        )
        Spacer(Modifier.height(6.dp))
        if (morning.spoken.isNotBlank()) {
            Text(morning.spoken, color = SailColors.Text, fontSize = 14.sp)
            Spacer(Modifier.height(6.dp))
        }
        morning.halted?.let { halted ->
            Text(halted, color = SailColors.Error, fontSize = 13.sp)
            Spacer(Modifier.height(6.dp))
        }
        Text(
            text = "${morning.ready} ready · ${morning.needsYou} needs you · ${morning.failed} failed · ${morning.notReached} not reached",
            color = SailColors.TextMuted,
            fontSize = 12.sp,
        )
        morning.items.forEach { item ->
            Spacer(Modifier.height(4.dp))
            val prUrl = parseIssueDraftPrUrl(item)
            if (prUrl != null) {
                Text(
                    text = "• ${item.title}: ${item.line}",
                    color = SailColors.Thinking,
                    fontSize = 12.sp,
                    textDecoration = TextDecoration.Underline,
                    modifier = Modifier.clickable {
                        runCatching {
                            context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(prUrl)))
                        }
                    },
                )
            } else {
                Text("• ${item.title}: ${item.line}", color = SailColors.TextMuted, fontSize = 12.sp)
            }
        }
    }
}

@Composable
private fun TonightPlanCard(plan: NightPlan) {
    FlatCard {
        Text("Tonight's plan", color = SailColors.Text, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(6.dp))
        plan.items.forEach { item ->
            val label = when (item) {
                is NightItem.ErrandRef -> "Errand ${item.id}"
                is NightItem.IssueRef -> "Issue #${item.number}"
            }
            Text("Planned: $label", color = SailColors.Good, fontSize = 13.sp)
        }
        plan.deferred.forEach { def ->
            val label = when (val item = def.item) {
                is NightItem.ErrandRef -> "Errand ${item.id}"
                is NightItem.IssueRef -> "Issue #${item.number}"
            }
            Text("Deferred: $label — ${def.reason}", color = SailColors.TextMuted, fontSize = 12.sp)
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ErrandCard(
    errand: Errand,
    busy: Boolean,
    diffText: String?,
    onApprove: (WorkOrder?) -> Unit,
    onReshape: (String) -> Unit,
    onRedo: (String) -> Unit,
    onShip: () -> Unit,
    onDrop: () -> Unit,
    onToggleDiff: () -> Unit,
    onAskAbout: () -> Unit,
) {
    val context = LocalContext.current
    var noteMode by remember(errand.id) { mutableStateOf<String?>(null) }
    var noteText by remember(errand.id) { mutableStateOf("") }

    FlatCard {
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            StatusPill(stateLabel(errand.state), stateColor(errand.state))
            StatusPill(errand.source.name.lowercase())
            if (errand.externalUsed) {
                val cli = when (val s = errand.state) {
                    is ErrandState.Ready -> s.result.ranOn.name
                    is ErrandState.NeedsYou -> s.result.ranOn.name
                    is ErrandState.Failed -> s.result?.ranOn?.name ?: "Claude Code"
                    else -> "Claude Code"
                }
                StatusPill("Sent to $cli", SailColors.Thinking)
            }
        }

        Spacer(Modifier.height(8.dp))
        Text(errand.ask, color = SailColors.Text, fontSize = 15.sp, fontWeight = FontWeight.Medium)

        if (errand.notes.isNotEmpty()) {
            Spacer(Modifier.height(4.dp))
            Text(
                text = "Owner notes: ${errand.notes.joinToString(" · ") { it.text }}",
                color = SailColors.TextMuted,
                fontSize = 12.sp,
            )
        }

        Spacer(Modifier.height(8.dp))
        when (val s = errand.state) {
            is ErrandState.Shaping -> {
                Text("Shaping work order in the background…", color = SailColors.TextMuted, fontSize = 13.sp)
            }
            is ErrandState.Proposed -> {
                ProposedEditor(
                    order = s.order,
                    busy = busy,
                    onApprove = onApprove,
                    onReshapeClick = { noteMode = "reshape" },
                    onDrop = onDrop,
                )
            }
            is ErrandState.Unshapeable -> {
                s.reasons.forEach { reason ->
                    Text("• $reason", color = SailColors.Error, fontSize = 13.sp)
                }
                Spacer(Modifier.height(8.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    SecondaryButton("Split or reshape", onClick = { noteMode = "reshape" }, enabled = !busy)
                    SecondaryButton("Drop", enabled = !busy, onClick = onDrop)
                }
            }
            is ErrandState.Queued -> {
                Text(
                    "Work order: ${s.order.title} (Size ${s.order.size.name})",
                    color = SailColors.TextMuted,
                    fontSize = 13.sp,
                )
                Spacer(Modifier.height(6.dp))
                SecondaryButton("Drop", enabled = !busy, onClick = onDrop)
            }
            is ErrandState.Working -> {
                Text(
                    "${s.order.title} · ${ranOnLabel(s.ranOn)}",
                    color = SailColors.TextMuted,
                    fontSize = 13.sp,
                )
            }
            is ErrandState.Ready -> {
                ResultSection(s.result, diffText, busy, onToggleDiff)
                Spacer(Modifier.height(8.dp))
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    PrimaryButton("Ship", enabled = !busy, onClick = onShip)
                    SecondaryButton("Redo with a note", onClick = { noteMode = "redo" }, enabled = !busy)
                    SecondaryButton("Ask about this", enabled = !busy, onClick = onAskAbout)
                    SecondaryButton("Drop", enabled = !busy, onClick = onDrop)
                }
            }
            is ErrandState.NeedsYou -> {
                Text("Decision needed: ${s.question}", color = SailColors.Warning, fontSize = 13.sp)
                Spacer(Modifier.height(6.dp))
                ResultSection(s.result, diffText, busy, onToggleDiff)
                Spacer(Modifier.height(8.dp))
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    PrimaryButton("Ship", enabled = !busy, onClick = onShip)
                    SecondaryButton("Redo with a note", onClick = { noteMode = "redo" }, enabled = !busy)
                    SecondaryButton("Ask about this", enabled = !busy, onClick = onAskAbout)
                    SecondaryButton("Drop", enabled = !busy, onClick = onDrop)
                }
            }
            is ErrandState.Failed -> {
                Text(s.reason, color = SailColors.Error, fontSize = 13.sp)
                s.result?.let {
                    Spacer(Modifier.height(6.dp))
                    ResultSection(it, diffText, busy, onToggleDiff)
                }
                Spacer(Modifier.height(8.dp))
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    SecondaryButton("Redo with a note", onClick = { noteMode = "redo" }, enabled = !busy)
                    SecondaryButton("Ask about this", enabled = !busy, onClick = onAskAbout)
                    SecondaryButton("Drop", enabled = !busy, onClick = onDrop)
                }
            }
            is ErrandState.Shipping -> {
                ResultSection(s.result, diffText, busy = true, onToggleDiff = onToggleDiff)
            }
            is ErrandState.Shipped -> {
                val safeUri = runCatching { Uri.parse(s.prUrl) }.getOrNull()
                    ?.takeIf { it.scheme == "https" || it.scheme == "http" }
                if (safeUri != null) {
                    PrimaryButton("Open draft PR", onClick = {
                        context.startActivity(Intent(Intent.ACTION_VIEW, safeUri))
                    })
                    Spacer(Modifier.height(6.dp))
                }
                ResultSection(s.result, diffText, busy, onToggleDiff)
            }
            is ErrandState.Dropped -> {
                Text("Dropped at ${s.at}", color = SailColors.TextMuted, fontSize = 12.sp)
            }
        }

        noteMode?.let { mode ->
            Spacer(Modifier.height(8.dp))
            OutlinedTextField(
                value = noteText,
                onValueChange = { if (it.length <= 1000) noteText = it },
                placeholder = { Text(if (mode == "reshape") "Note for reshaping…" else "Note for redo…") },
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(6.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                PrimaryButton(
                    text = "Send",
                    enabled = !busy && noteText.isNotBlank(),
                    onClick = {
                        val text = noteText
                        noteText = ""
                        noteMode = null
                        if (mode == "reshape") onReshape(text) else onRedo(text)
                    },
                )
                SecondaryButton("Cancel", onClick = {
                    noteMode = null
                    noteText = ""
                })
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ProposedEditor(
    order: WorkOrder,
    busy: Boolean,
    onApprove: (WorkOrder?) -> Unit,
    onReshapeClick: () -> Unit,
    onDrop: () -> Unit,
) {
    var title by remember(order) { mutableStateOf(order.title) }
    var why by remember(order) { mutableStateOf(order.why) }
    val acceptance = remember(order) { mutableStateListOf(*order.acceptance.toTypedArray()) }

    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("SAIL's draft work order · Size ${order.size.name}", color = SailColors.Warning, fontSize = 12.sp, fontWeight = FontWeight.Medium)
        OutlinedTextField(
            value = title,
            onValueChange = { if (it.length <= 100) title = it },
            label = { Text("Title") },
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = why,
            onValueChange = { if (it.length <= 600) why = it },
            label = { Text("Why") },
            modifier = Modifier.fillMaxWidth(),
        )
        acceptance.forEachIndexed { idx, item ->
            OutlinedTextField(
                value = item,
                onValueChange = { if (it.length <= 200) acceptance[idx] = it },
                label = { Text("Check ${idx + 1}") },
                modifier = Modifier.fillMaxWidth(),
            )
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            order.targetFiles.forEach { file -> StatusPill(file) }
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            PrimaryButton(
                text = "Approve for tonight",
                enabled = !busy,
                onClick = {
                    onApprove(order.copy(title = title, why = why, acceptance = acceptance.toList()))
                },
            )
            SecondaryButton("Reshape with a note", enabled = !busy, onClick = onReshapeClick)
            SecondaryButton("Drop", enabled = !busy, onClick = onDrop)
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ResultSection(
    result: NightResult,
    diffText: String?,
    busy: Boolean,
    onToggleDiff: () -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(ranOnLabel(result.ranOn), color = SailColors.TextMuted, fontSize = 12.sp)
        Text(result.summary, color = SailColors.Text, fontSize = 13.sp)
        val t = result.tests
        if (t != null) {
            StatusPill(
                "Tests: ${t.passed} passed, ${t.failed} failed (${t.durationSecs}s)",
                if (t.exitOk) SailColors.Good else SailColors.Error,
            )
        } else {
            StatusPill("Tests did not run", SailColors.Warning)
        }
        if (result.gates.isNotEmpty()) {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                result.gates.forEach { gate ->
                    StatusPill("${gate.name}: ${gate.detail}", if (gate.passed) SailColors.Good else SailColors.Error)
                }
            }
        }
        result.review?.concerns?.takeIf { it.isNotEmpty() }?.let { concerns ->
            concerns.forEach { c -> Text("• $c", color = SailColors.Warning, fontSize = 12.sp) }
        }
        result.modelNote?.let { note ->
            Text("Model-written note: $note", color = SailColors.TextMuted, fontSize = 12.sp)
        }
        SecondaryButton(
            text = if (diffText != null) "Hide diff" else "View diff",
            enabled = !busy,
            onClick = onToggleDiff,
        )
        if (diffText != null) {
            DiffView(diffText)
        }
    }
}

private fun ranOnLabel(ranOn: RanOn): String = when (ranOn.kind) {
    RanOnKind.EXTERNAL -> "Sent to ${ranOn.name} · ${ranOn.model}"
    RanOnKind.MESH -> "PC (${ranOn.name}) · ${ranOn.model}"
    RanOnKind.HOST -> "Host · ${ranOn.model}"
}

private fun stateLabel(state: ErrandState): String = when (state) {
    is ErrandState.Shaping -> "Shaping"
    is ErrandState.Proposed -> "SAIL's draft"
    is ErrandState.Unshapeable -> "Couldn't shape"
    is ErrandState.Queued -> "Queued"
    is ErrandState.Working -> "Working · ${state.stage}"
    is ErrandState.Ready -> "Ready"
    is ErrandState.NeedsYou -> "Needs you"
    is ErrandState.Failed -> "Failed"
    is ErrandState.Shipping -> "Shipping…"
    is ErrandState.Shipped -> "Shipped"
    is ErrandState.Dropped -> "Dropped"
}

private fun stateColor(state: ErrandState): Color = when (state) {
    is ErrandState.Ready, is ErrandState.Shipped, is ErrandState.Queued -> SailColors.Good
    is ErrandState.Proposed, is ErrandState.NeedsYou -> SailColors.Warning
    is ErrandState.Unshapeable, is ErrandState.Failed -> SailColors.Error
    is ErrandState.Shaping, is ErrandState.Working, is ErrandState.Shipping -> SailColors.Thinking
    is ErrandState.Dropped -> SailColors.TextMuted
}
