package dev.sail.app.feature.arena

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.sail.app.core.UiState
import dev.sail.app.core.net.ApiResult
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.util.UUID

enum class PlayerMode(val path: String) { Lesson("lessons"), Diagnostic("diagnostics"), Review("reviews") }

data class AnswerDraft(
    val choices: List<String> = emptyList(),
    val order: List<String> = emptyList(),
    val text: String = "",
    val code: String = "",
    val confidence: String? = null,
) {
    fun answer(step: Step): JsonObject? = when (step.kind) {
        "explain" -> null
        "choice", "predict" -> buildJsonObject { put("choice", JsonArray(choices.map { JsonPrimitive(it) })) }
        "order" -> buildJsonObject { put("order", JsonArray(order.map { JsonPrimitive(it) })) }
        "fill" -> buildJsonObject { put("text", text) }
        "rust_edit" -> buildJsonObject { put("code", code) }
        else -> null
    }

    fun ready(step: Step): Boolean {
        if (step.confidence && confidence !in listOf("low", "medium", "high")) return false
        return when (step.kind) {
            "explain" -> true
            "choice", "predict" -> choices.isNotEmpty()
            "order" -> order.size == step.items.size && order.toSet() == step.items.map { it.id }.toSet()
            "fill" -> text.isNotBlank()
            "rust_edit" -> code.isNotBlank()
            else -> false
        }
    }
}

data class PlayerSession(
    val mode: PlayerMode,
    val id: String,
    val title: String,
    val step: Step?,
    val lesson: LessonView? = null,
    val draft: AnswerDraft = AnswerDraft(),
    val grade: Grade? = null,
    val hints: List<Hint> = emptyList(),
    val solution: List<Block> = emptyList(),
    val gate: Gate? = null,
    val report: EvidenceList? = null,
    val finished: Boolean = false,
    val browsing: Boolean = false,
    val nextAction: NextAction? = null,
)

class ArenaPlayerViewModel(
    private val repository: ArenaService,
    private val uuid: () -> String = { UUID.randomUUID().toString() },
) : ViewModel() {
    private val mutableState = MutableStateFlow<UiState<PlayerSession>>(UiState.Empty("Choose an activity"))
    val state = mutableState.asStateFlow()
    private val mutableBusy = MutableStateFlow(false)
    val busy = mutableBusy.asStateFlow()
    private val mutableError = MutableStateFlow<String?>(null)
    val error = mutableError.asStateFlow()
    private val mutablePending = MutableStateFlow<AttemptRequest?>(null)
    val pending = mutablePending.asStateFlow()
    private var retryLoad: (() -> Unit)? = null
    private val session: PlayerSession? get() = (mutableState.value as? UiState.Ready)?.data

    private fun atStep(current: PlayerSession, step: Step?): PlayerSession = current.copy(
        step = step,
        draft = AnswerDraft(order = step?.items.orEmpty().map { it.id }, code = step?.starter.orEmpty()),
        grade = null, hints = emptyList(), solution = emptyList(), browsing = false,
    )

    private fun action(block: suspend () -> Unit) {
        if (mutableBusy.value) return
        mutableBusy.value = true
        mutableError.value = null
        viewModelScope.launch {
            try { block() } finally { mutableBusy.value = false }
        }
    }

    private fun show(value: PlayerSession) { mutableState.value = UiState.Ready(value) }

    private fun loadError(error: ApiResult.Err) {
        if (session == null) mutableState.value = UiState.Unavailable(error.message)
        else mutableError.value = error.message
    }

    fun openLesson(id: String) {
        if (mutablePending.value != null) return
        retryLoad = { openLesson(id) }
        val current = session
        if (current?.mode == PlayerMode.Lesson && current.id == id && !current.browsing) return
        if (current == null) mutableState.value = UiState.Loading
        action { loadLesson(id) }
    }

    private suspend fun loadLesson(id: String) {
        when (val result = repository.lesson(id)) {
            is ApiResult.Err -> loadError(result)
            is ApiResult.Ok -> {
                val view = result.data
                val complete = view.progress.state == "completed"
                val step = view.lesson.steps.find { it.id == view.progress.currentStepId }
                if (!complete && step == null) {
                    mutableState.value = UiState.Unavailable("SAIL did not return the current lesson step.")
                } else {
                    show(atStep(PlayerSession(PlayerMode.Lesson, id, view.lesson.title, step, lesson = view), step).copy(finished = complete))
                }
            }
        }
    }

    fun openDiagnostic(courseId: String, info: DiagnosticInfo?) {
        if (mutablePending.value != null) return
        retryLoad = { openDiagnostic(courseId, info) }
        if (session == null) mutableState.value = UiState.Loading
        action {
            if (info?.state == "done" && info.sessionId != null) {
                val current = PlayerSession(PlayerMode.Diagnostic, info.sessionId, "Placement report", null, finished = true)
                when (val result = repository.report(info.sessionId)) {
                    is ApiResult.Ok -> show(current.copy(report = result.data))
                    is ApiResult.Err -> loadError(result)
                }
            } else {
                when (val result = repository.diagnostic(courseId, info?.sessionId)) {
                    is ApiResult.Err -> loadError(result)
                    is ApiResult.Ok -> {
                        val data = result.data
                        val current = atStep(PlayerSession(PlayerMode.Diagnostic, data.id, "Placement check", data.step), data.step)
                        show(current.copy(finished = data.done))
                        if (data.done) loadReport(current)
                    }
                }
            }
        }
    }

    fun reload() { retryLoad?.invoke() }

    fun openReview(item: ReviewItem) {
        if (mutableBusy.value || mutablePending.value != null) return
        show(atStep(PlayerSession(PlayerMode.Review, item.id, item.objectiveTitle, item.step), item.step))
    }

    fun update(draft: AnswerDraft) {
        val current = session ?: return
        if (!mutableBusy.value && mutablePending.value == null && current.grade == null) show(current.copy(draft = draft))
    }

    fun submit() {
        val current = session ?: return
        val step = current.step ?: return
        if (current.browsing || current.finished || current.grade != null) return
        if (mutablePending.value == null && !current.draft.ready(step)) return
        val request = mutablePending.value ?: AttemptRequest(
            attemptId = uuid(), stepId = if (current.mode == PlayerMode.Review) null else step.id,
            answer = current.draft.answer(step), confidence = current.draft.confidence,
        )
        action {
            mutablePending.value = request
            when (val result = repository.attempt(current.mode, current.id, request)) {
                is ApiResult.Err -> {
                    // A transport/server failure may hide a committed answer. Keep the exact UUID and body.
                    if (result.status in 400..499 && result.status !in listOf(401, 429)) mutablePending.value = null
                    mutableError.value = result.message
                }
                is ApiResult.Ok -> {
                    if (result.data.result !in listOf("correct", "incorrect", "unavailable")) {
                        mutableError.value = "SAIL returned an unknown result. Retry the same submission."
                    } else {
                        mutablePending.value = null
                        show(current.copy(grade = result.data))
                    }
                }
            }
        }
    }

    fun retryAnswer() {
        val current = session ?: return
        if (!mutableBusy.value && mutablePending.value == null) show(current.copy(grade = null))
    }

    fun hint() {
        val current = session ?: return
        val step = current.step ?: return
        if (current.mode == PlayerMode.Diagnostic || current.browsing || mutablePending.value != null) return
        action {
            when (val result = repository.hint(current.mode, current.id, step.id)) {
                is ApiResult.Ok -> show(current.copy(hints = current.hints + result.data))
                is ApiResult.Err -> mutableError.value = "${result.message} Hint usage may have changed on SAIL."
            }
        }
    }

    fun reveal() {
        val current = session ?: return
        val step = current.step ?: return
        if (current.mode != PlayerMode.Lesson || current.browsing || mutablePending.value != null) return
        action {
            when (val result = repository.reveal(current.id, step.id)) {
                is ApiResult.Ok -> show(current.copy(solution = result.data.solution))
                is ApiResult.Err -> mutableError.value = "${result.message} SAIL may have recorded the reveal."
            }
        }
    }

    fun continueActivity() {
        val current = session ?: return
        val grade = current.grade ?: return
        if (mutablePending.value != null || grade.gateDecision !in listOf("continue", "lesson_complete", "blocked_by_review")) return
        action {
            when (current.mode) {
                PlayerMode.Lesson -> loadLesson(current.id)
                PlayerMode.Review -> show(current.copy(finished = true))
                PlayerMode.Diagnostic -> {
                    if (grade.done) {
                        show(current.copy(finished = true, step = null))
                        loadReport(current)
                    } else if (grade.next != null) show(atStep(current, grade.next))
                    else mutableError.value = "SAIL did not return the next placement step."
                }
            }
        }
    }

    private suspend fun loadReport(current: PlayerSession) {
        when (val result = repository.report(current.id)) {
            is ApiResult.Ok -> show(current.copy(finished = true, step = null, report = result.data))
            is ApiResult.Err -> mutableError.value = result.message
        }
    }

    fun certify() {
        val current = session ?: return
        if (current.mode != PlayerMode.Lesson || !current.finished || mutablePending.value != null) return
        action {
            when (val result = repository.advance(current.id)) {
                is ApiResult.Ok -> {
                    show(current.copy(gate = result.data))
                    if (result.data.certified) {
                        val courseId = current.lesson?.lesson?.courseId
                        if (courseId != null) {
                            when (val next = repository.next(courseId)) {
                                is ApiResult.Ok -> show(current.copy(gate = result.data, nextAction = next.data))
                                is ApiResult.Err -> mutableError.value = next.message
                            }
                        }
                    }
                }
                is ApiResult.Err -> mutableError.value = result.message
            }
        }
    }

    fun browse(step: Step) {
        val current = session ?: return
        if (current.mode != PlayerMode.Lesson || mutableBusy.value || mutablePending.value != null) return
        show(atStep(current, step).copy(browsing = true, finished = false))
    }
}
