package dev.sail.app.feature.arena

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.sail.app.core.LockNavigation
import dev.sail.app.core.UiState
import dev.sail.app.core.sailViewModel
import dev.sail.app.core.design.EmptyState
import dev.sail.app.core.design.GlowCard
import dev.sail.app.core.design.Hue
import dev.sail.app.core.design.LoadingSkeleton
import dev.sail.app.core.design.PrimaryButton
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.SailIcon
import dev.sail.app.core.design.SailPill
import dev.sail.app.core.design.SailProgress
import dev.sail.app.core.design.SailRow
import dev.sail.app.core.design.SailTopBar
import dev.sail.app.core.design.SecondaryButton
import dev.sail.app.core.design.SectionHeader
import dev.sail.app.core.design.StatTile

@Composable
internal fun ArenaFrame(title: String, subtitle: String? = null, onBack: (() -> Unit)? = null, content: @Composable ColumnScope.() -> Unit) {
    Column(Modifier.fillMaxSize()) {
        SailTopBar(title, subtitle = subtitle, onBack = onBack)
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
            content = content,
        )
    }
}

@Composable
internal fun <T> ArenaState(state: UiState<T>, retry: () -> Unit, content: @Composable (T) -> Unit) {
    when (state) {
        UiState.Loading -> LoadingSkeleton()
        is UiState.Empty -> EmptyState(state.title, detail = state.detail)
        is UiState.Unavailable -> EmptyState("Arena unavailable", detail = state.message, onRetry = retry)
        is UiState.Ready -> content(state.data)
    }
}

@Composable
fun ArenaScreen(onOpenCourse: (String) -> Unit, onOpenLesson: (String) -> Unit, onOpenReviews: () -> Unit) {
    val vm = sailViewModel { ArenaViewModel(ArenaRepository(it.api)) }
    val state by vm.catalog.collectAsStateWithLifecycle()
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { vm.loadCatalog() }
    ArenaFrame("Arena", "Learn · Evaluate · Practice") {
        ArenaState(state, vm::loadCatalog) { catalog ->
            CatalogContent(catalog, onOpenCourse, onOpenLesson, onOpenReviews)
        }
    }
}

@Composable
internal fun CatalogContent(catalog: Catalog, onOpenCourse: (String) -> Unit, onOpenLesson: (String) -> Unit, onOpenReviews: () -> Unit) {
    val ready = catalog.courses.filter { it.status == "ready" }
    if (ready.isNotEmpty() && ready.all { it.progress != null }) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            StatTile(ready.sumOf { it.progress?.due ?: 0 }.toString(), "Due reviews", Hue.Music, Modifier.weight(1f))
            StatTile(ready.sumOf { it.progress?.retained ?: 0 }.toString(), "Retained", Hue.Tracker, Modifier.weight(1f))
            StatTile("${ready.sumOf { it.progress?.lessonsCompleted ?: 0 }} / ${ready.sumOf { it.lessonCount }}", "Lessons done", Hue.Learn, Modifier.weight(1f))
        }
    }
    ready.firstOrNull { it.next?.kind == "lesson" }?.let { resume ->
        resume.next?.lessonId?.let { id ->
            PrimaryButton("Continue · ${resume.title}", { onOpenLesson(id) }, Modifier.fillMaxWidth())
        }
    }
    SectionHeader("Courses")
    if (catalog.courses.isEmpty()) EmptyState("No courses available")
    catalog.courses.forEach { course ->
        GlowCard(Hue.Arena, Modifier.fillMaxWidth(), onClick = if (course.status == "ready") ({ onOpenCourse(course.id) }) else null) {
            SailRow(course.title, meta = if (course.status == "ready") "${course.lessonCount} lessons · ${course.minutes} min" else "Planned · no lessons available", icon = SailIcon.Arena, hue = Hue.Learn)
            Spacer(Modifier.height(8.dp))
            Text(course.summary, color = SailColors.TextMuted)
            course.progress?.let { progress ->
                Spacer(Modifier.height(12.dp))
                Text("${progress.lessonsCompleted} / ${course.lessonCount} lessons done", style = MaterialTheme.typography.labelMedium)
                if (course.lessonCount > 0) SailProgress(progress.lessonsCompleted.toFloat() / course.lessonCount, Hue.Learn)
            }
        }
    }
    SailRow("Due reviews", meta = "Fresh problems, checked after a delay", icon = SailIcon.Pulse, hue = Hue.Music, onClick = onOpenReviews)
}

@Composable
fun CourseScreen(courseId: String, onOpenLesson: (String) -> Unit, onBack: () -> Unit) {
    val vm = sailViewModel(key = "course-$courseId") { ArenaViewModel(ArenaRepository(it.api)) }
    val player = sailViewModel(key = "diagnostic-$courseId") { ArenaPlayerViewModel(ArenaRepository(it.api)) }
    val detail by vm.detail.collectAsStateWithLifecycle()
    val next by vm.next.collectAsStateWithLifecycle()
    val evidence by vm.evidence.collectAsStateWithLifecycle()
    var section by rememberSaveable(courseId) { mutableStateOf("path") }
    LockNavigation(locked = section == "diagnostic" || section == "reviews")
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { vm.loadCourse(courseId) }
    if (section == "diagnostic") {
        PlayerPane(player, { section = "path"; vm.loadCourse(courseId) }, onOpenLesson)
        return
    }
    if (section == "reviews") {
        CourseReviews(courseId, { section = "path"; vm.loadCourse(courseId) })
        return
    }
    val leave = { if (section == "path") onBack() else section = "path" }
    BackHandler(section != "path") { section = "path" }
    ArenaFrame((detail as? UiState.Ready)?.data?.course?.title ?: "Course", onBack = leave) {
        ArenaState(detail, { vm.loadCourse(courseId) }) { data ->
            if (section == "evidence") {
                SectionHeader("What SAIL thinks you know")
                ArenaState(evidence, { vm.loadEvidence(courseId) }) { EvidenceContent(it) }
            } else {
                Text(data.course.summary, color = SailColors.TextMuted)
                ArenaState(next, { vm.loadCourse(courseId) }) { action ->
                    action.reasons.forEach { Text(it, color = SailColors.TextMuted) }
                    when (action.kind) {
                        "lesson" -> action.lessonId?.let { PrimaryButton("Continue lesson", { onOpenLesson(it) }, Modifier.fillMaxWidth()) }
                        "review" -> PrimaryButton("Review checkpoint", { section = "reviews" }, Modifier.fillMaxWidth())
                        "done" -> SailPill("Current lessons certified · reviews continue")
                    }
                }
                data.diagnostic?.let { info ->
                    SecondaryButton(
                        when (info.state) { "done" -> "View placement report"; "in_progress" -> "Resume placement check"; else -> "Start placement check" },
                        { player.openDiagnostic(courseId, info); section = "diagnostic" }, Modifier.fillMaxWidth(),
                    )
                }
                Text("Study freely. Certification and review gates are checked by SAIL.", color = SailColors.TextMuted)
                data.chapters.forEach { chapter ->
                    SectionHeader(chapter.title)
                    chapter.lessons.forEach { lesson ->
                        SailRow(
                            lesson.title,
                            meta = "${lesson.minutes} min · ${lesson.state.replace('_', ' ')} · ${if (lesson.certified) "certified" else "not certified"}",
                            icon = SailIcon.Arena, hue = Hue.Learn, onClick = { onOpenLesson(lesson.id) },
                        )
                    }
                }
                SecondaryButton("Due reviews", { section = "reviews" }, Modifier.fillMaxWidth())
                SecondaryButton("What SAIL thinks you know", { section = "evidence" }, Modifier.fillMaxWidth())
            }
        }
    }
}

@Composable
internal fun EvidenceContent(data: EvidenceList) {
    data.summary?.let { Text(it) }
    if (data.objectives.isEmpty()) EmptyState("No evidence yet")
    data.objectives.forEach { item ->
        GlowCard(Hue.Arena, Modifier.fillMaxWidth()) {
            Text(item.title, style = MaterialTheme.typography.titleMedium)
            SailPill(if (item.notAssessed) "${item.state} · not assessed" else item.state)
            Text(item.why, color = SailColors.TextMuted)
            Text("Independent passes: ${item.independentPasses} · Assisted: ${item.assistedPasses} · Failures: ${item.failures}")
            item.lastIndependentSuccess?.let { Text("Last independent success: $it", color = SailColors.TextMuted) }
            item.lastDelayedSuccess?.let { Text("Last delayed success: $it", color = SailColors.TextMuted) }
            item.retentionDueAt?.let { Text("Review due: $it", color = SailColors.TextMuted) }
        }
    }
}

@Composable
fun LessonScreen(lessonId: String, onBack: () -> Unit, onOpenLesson: (String) -> Unit) {
    val vm = sailViewModel(key = "lesson-$lessonId") { ArenaPlayerViewModel(ArenaRepository(it.api)) }
    androidx.compose.runtime.LaunchedEffect(lessonId) { vm.openLesson(lessonId) }
    PlayerPane(vm, onBack, onOpenLesson)
}

@Composable
fun ReviewScreen(onBack: () -> Unit) {
    val vm = sailViewModel(key = "review-catalog") { ArenaViewModel(ArenaRepository(it.api)) }
    val state by vm.catalog.collectAsStateWithLifecycle()
    var course by rememberSaveable { mutableStateOf<String?>(null) }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { vm.loadCatalog() }
    val selected = course
    if (selected != null) {
        CourseReviews(selected, { course = null; vm.loadCatalog() })
    } else {
        ArenaFrame("Due reviews", onBack = onBack) {
            ArenaState(state, vm::loadCatalog) { data ->
                data.courses.filter { it.status == "ready" }.forEach { item ->
                    SailRow(item.title, meta = item.progress?.let { "${it.due} due" } ?: "Check review queue", icon = SailIcon.Arena, onClick = { course = item.id })
                }
                if (data.courses.none { it.status == "ready" }) EmptyState("No ready courses")
            }
        }
    }
}

@Composable
private fun CourseReviews(courseId: String, onBack: () -> Unit) {
    val vm = sailViewModel(key = "reviews-$courseId") { ArenaViewModel(ArenaRepository(it.api)) }
    val player = sailViewModel(key = "review-player-$courseId") { ArenaPlayerViewModel(ArenaRepository(it.api)) }
    val state by vm.queue.collectAsStateWithLifecycle()
    var playing by rememberSaveable { mutableStateOf(false) }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { vm.loadReviews(courseId) }
    if (playing) {
        PlayerPane(player, { playing = false; vm.loadReviews(courseId) }, {})
    } else {
        BackHandler { onBack() }
        ArenaFrame("Review checkpoint", onBack = onBack) {
            ArenaState(state, { vm.loadReviews(courseId) }) { data ->
                Text("${data.catchUp.totalDue} due · up to ${data.catchUp.cap} per session · ${data.catchUp.sessions} catch-up sessions", color = SailColors.TextMuted)
                Text("${data.catchUp.reviewedToday} reviewed recently", color = SailColors.TextMuted)
                if (data.items.isEmpty()) EmptyState("No reviews due", detail = "SAIL will schedule the next delayed check.")
                data.items.forEach { item ->
                    SailRow(item.objectiveTitle, meta = "Due ${item.dueAt} · ${item.reason}", icon = SailIcon.Pulse, hue = Hue.Learn, onClick = { player.openReview(item); playing = true })
                }
            }
        }
    }
}
