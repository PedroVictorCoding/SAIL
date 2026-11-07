package dev.sail.app.feature.arena

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Checkbox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.sail.app.core.UiState
import dev.sail.app.core.design.EmptyState
import dev.sail.app.core.design.FlatCard
import dev.sail.app.core.design.Hue
import dev.sail.app.core.design.PrimaryButton
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.SailPill
import dev.sail.app.core.design.SailProgress
import dev.sail.app.core.design.SecondaryButton
import dev.sail.app.core.design.SectionHeader

@Composable
internal fun PlayerPane(vm: ArenaPlayerViewModel, onBack: () -> Unit, onOpenLesson: (String) -> Unit) {
    val state by vm.state.collectAsStateWithLifecycle()
    val busy by vm.busy.collectAsStateWithLifecycle()
    val pending by vm.pending.collectAsStateWithLifecycle()
    val error by vm.error.collectAsStateWithLifecycle()
    var confirmReveal by remember { mutableStateOf(false) }
    val current = (state as? UiState.Ready)?.data
    val locked = busy || pending != null
    BackHandler(locked) { }
    ArenaFrame(current?.title ?: "Activity", onBack = if (locked) null else onBack) {
        if (locked) Text("Keep this activity open until SAIL confirms the submission.", color = SailColors.Warning)
        error?.let { message ->
            EmptyState("Request did not finish", detail = message, onRetry = if (pending != null) vm::submit else null)
        }
        ArenaState(state, vm::reload) { session ->
            if (session.finished) {
                SectionHeader(if (session.mode == PlayerMode.Diagnostic) "Placement report" else "Activity complete")
                session.grade?.let { FeedbackContent(it) }
                session.report?.let { EvidenceContent(it) }
                if (session.mode == PlayerMode.Lesson) {
                    Text("Lesson progress is saved. SAIL checks certification separately.", color = SailColors.TextMuted)
                    PrimaryButton("Check certified advance", vm::certify, Modifier.fillMaxWidth(), enabled = !locked)
                    session.gate?.let { gate ->
                        SailPill(if (gate.certified) "Certified" else "Not certified")
                        gate.reasons.forEach { Text(it) }
                        if (gate.decision == "blocked_by_review") Text("Review is needed for certified advance. You can still study freely.")
                    }
                    session.nextAction?.lessonId?.let { nextId ->
                        PrimaryButton("Next lesson", { onOpenLesson(nextId) }, Modifier.fillMaxWidth(), enabled = !locked)
                    }
                }
                SecondaryButton("Return", onBack, Modifier.fillMaxWidth(), enabled = !locked)
            } else {
                val step = session.step
                if (step == null) {
                    EmptyState("No current step", detail = "Return and reload the activity from SAIL.")
                } else {
                    val lesson = session.lesson
                    if (lesson != null) {
                        val index = lesson.lesson.steps.indexOfFirst { it.id == step.id }
                        Text("Step ${index + 1} / ${lesson.lesson.steps.size}", color = SailColors.TextMuted)
                        if (lesson.lesson.steps.isNotEmpty()) SailProgress((index + 1).toFloat() / lesson.lesson.steps.size, Hue.Learn)
                    }
                    if (session.browsing) SailPill("Read only · study freely")
                    SectionHeader(step.title)
                    PlainBlocks(step.body)
                    step.code?.let { CodeText(it) }
                    step.template?.let { CodeText(it) }
                    AnswerControls(step, session.draft, !locked && session.grade == null && !session.browsing, vm::update)
                    if (step.confidence && step.kind != "explain") {
                        Text("How confident are you?", style = MaterialTheme.typography.titleSmall)
                        Column(Modifier.selectableGroup()) {
                            listOf("low", "medium", "high").forEach { confidence ->
                                Row(
                                    Modifier.fillMaxWidth().heightIn(min = 48.dp).selectable(
                                        selected = session.draft.confidence == confidence,
                                        enabled = !locked && session.grade == null && !session.browsing,
                                        role = Role.RadioButton,
                                        onClick = { vm.update(session.draft.copy(confidence = confidence)) },
                                    ), verticalAlignment = Alignment.CenterVertically,
                                ) {
                                    RadioButton(selected = session.draft.confidence == confidence, onClick = null)
                                    Text(confidence.replaceFirstChar { it.uppercase() })
                                }
                            }
                        }
                    }
                    session.hints.forEach { hint -> FlatCard(Modifier.fillMaxWidth()) { Text("Hint ${hint.level}"); Text(hint.text) } }
                    if (session.solution.isNotEmpty()) {
                        SailPill("Solution seen · assisted")
                        PlainBlocks(session.solution)
                    }
                    val grade = session.grade
                    if (grade != null) {
                        FeedbackContent(grade)
                        if (grade.gateDecision == "retry") {
                            SecondaryButton("Try again", vm::retryAnswer, Modifier.fillMaxWidth(), enabled = !locked)
                        } else if (grade.gateDecision in listOf("continue", "lesson_complete", "blocked_by_review")) {
                            PrimaryButton(if (grade.result == "unavailable") "Continue without assessment" else "Continue", vm::continueActivity, Modifier.fillMaxWidth(), enabled = !locked)
                        }
                    } else if (!session.browsing) {
                        PrimaryButton(
                            if (pending != null) "Retry same submission" else if (step.kind == "explain") "Continue" else "Check",
                            vm::submit, Modifier.fillMaxWidth(), enabled = !busy && (pending != null || session.draft.ready(step)),
                        )
                    }
                    if (session.mode != PlayerMode.Diagnostic && !session.browsing && step.kind != "explain") {
                        val used = session.hints.lastOrNull()?.hintsUsed
                        Text(if (used == null) "Hints: usage confirmed by SAIL when requested" else "Hints used: $used / ${session.hints.last().hintCount}", color = SailColors.TextMuted)
                        SecondaryButton("Get next hint", vm::hint, Modifier.fillMaxWidth(), enabled = !locked && session.grade == null && step.hintCount > 0 && (used == null || used < session.hints.last().hintCount))
                        if (session.mode == PlayerMode.Lesson) {
                            SecondaryButton("Reveal solution", { confirmReveal = true }, Modifier.fillMaxWidth(), enabled = !locked && session.grade == null && session.solution.isEmpty())
                        }
                    }
                    if (session.browsing) SecondaryButton("Return to current step", { vm.openLesson(session.id) }, Modifier.fillMaxWidth(), enabled = !locked)
                }
            }
            session.lesson?.lesson?.steps?.let { steps ->
                SectionHeader("Study freely")
                steps.forEach { step ->
                    SecondaryButton(step.title, { vm.browse(step) }, Modifier.fillMaxWidth(), enabled = !locked)
                }
            }
        }
    }
    if (confirmReveal) {
        AlertDialog(
            onDismissRequest = { confirmReveal = false },
            title = { Text("Reveal the solution?") },
            text = { Text("SAIL records this as assisted. It cannot certify an independent pass.") },
            confirmButton = { TextButton(onClick = { confirmReveal = false; vm.reveal() }, enabled = !locked) { Text("Reveal") } },
            dismissButton = { TextButton(onClick = { confirmReveal = false }) { Text("Keep trying") } },
        )
    }
}

@Composable
internal fun PlainBlocks(blocks: List<Block>) {
    blocks.forEach { block ->
        if (block.kind == "code") CodeText(block.text)
        else Text(block.text, style = if (block.kind == "h") MaterialTheme.typography.titleMedium else MaterialTheme.typography.bodyLarge)
    }
}

@Composable
private fun CodeText(text: String) {
    FlatCard(Modifier.fillMaxWidth()) {
        Text(text, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodyMedium)
    }
}

@Composable
internal fun FeedbackContent(grade: Grade) {
    Column(Modifier.semantics { liveRegion = LiveRegionMode.Polite }, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        SailPill(when (grade.result) { "unavailable" -> "Evaluation unavailable"; "correct" -> "Checked"; "incorrect" -> "Try again"; else -> "Unknown result" })
        Text(grade.feedback.summary)
        Text(grade.evidenceLabel(), color = SailColors.TextMuted)
        grade.feedback.unavailableReason?.let { Text(it) }
        PlainBlocks(grade.feedback.explanation)
        grade.feedback.diagnostics.forEach { diagnostic ->
            Text("${diagnostic.level} · line ${diagnostic.line}:${diagnostic.column} · ${diagnostic.message}", fontFamily = FontFamily.Monospace)
        }
        grade.feedback.stdout?.takeIf { it.isNotEmpty() }?.let { CodeText(it) }
        grade.gateReasons.forEach { Text(it) }
        grade.reviewDueAt?.let { Text("Review due: $it", color = SailColors.TextMuted) }
    }
}

@Composable
private fun AnswerControls(step: Step, draft: AnswerDraft, enabled: Boolean, update: (AnswerDraft) -> Unit) {
    when (step.kind) {
        "choice", "predict" -> Column(if (step.multi) Modifier else Modifier.selectableGroup(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            step.options.forEach { option ->
                val selected = option.id in draft.choices
                Row(
                    Modifier.fillMaxWidth().heightIn(min = 52.dp)
                        .background(if (selected) Hue.Learn.color.copy(alpha = 0.25f) else SailColors.Surface, RoundedCornerShape(14.dp))
                        .border(1.dp, if (selected) Hue.Learn.hot else SailColors.LineStrong, RoundedCornerShape(14.dp))
                        .selectable(selected, enabled = enabled, role = if (step.multi) Role.Checkbox else Role.RadioButton, onClick = {
                            update(draft.copy(choices = if (step.multi) {
                                if (selected) draft.choices - option.id else draft.choices + option.id
                            } else listOf(option.id)))
                        }).padding(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (step.multi) Checkbox(selected, onCheckedChange = null, enabled = enabled)
                    else RadioButton(selected, onClick = null, enabled = enabled)
                    Text(option.text, modifier = Modifier.weight(1f), fontFamily = if (option.code) FontFamily.Monospace else FontFamily.Default)
                }
            }
        }
        "order" -> {
            Text("Arrange the statements from top to bottom.", color = SailColors.TextMuted)
            draft.order.forEachIndexed { index, id ->
                FlatCard(Modifier.fillMaxWidth()) {
                    Text("${index + 1}. ${step.items.find { it.id == id }?.text.orEmpty()}")
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        SecondaryButton("Up", { update(draft.copy(order = moveAnswer(draft.order, index, -1))) }, Modifier.weight(1f), enabled && index > 0)
                        SecondaryButton("Down", { update(draft.copy(order = moveAnswer(draft.order, index, 1))) }, Modifier.weight(1f), enabled && index < draft.order.lastIndex)
                    }
                }
            }
        }
        "fill" -> OutlinedTextField(
            value = draft.text, onValueChange = { update(draft.copy(text = it)) }, enabled = enabled,
            label = { Text(step.placeholder ?: "Expression") }, modifier = Modifier.fillMaxWidth(),
        )
        "rust_edit" -> {
            Text("Code runs only in SAIL's isolated server runner. If it is unavailable, this step is not assessed.", color = SailColors.TextMuted)
            OutlinedTextField(
                value = draft.code, onValueChange = { update(draft.copy(code = it)) }, enabled = enabled,
                label = { Text("Rust source") }, modifier = Modifier.fillMaxWidth(), minLines = 8,
                textStyle = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
            )
        }
        "explain" -> Unit
        else -> EmptyState("Unsupported exercise", detail = "This app cannot submit this step kind.")
    }
}

internal fun moveAnswer(items: List<String>, index: Int, delta: Int): List<String> {
    val target = index + delta
    if (index !in items.indices || target !in items.indices) return items
    return items.toMutableList().apply { val item = removeAt(index); add(target, item) }
}
