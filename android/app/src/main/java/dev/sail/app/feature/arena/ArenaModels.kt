package dev.sail.app.feature.arena

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject

@Serializable
data class Catalog(val courses: List<Course>)

@Serializable
data class Course(
    val id: String,
    val title: String,
    val status: String,
    val summary: String,
    @SerialName("lesson_count") val lessonCount: Int,
    val minutes: Int,
    val progress: CourseProgress? = null,
    val next: NextAction? = null,
)

@Serializable
data class CourseProgress(
    @SerialName("lessons_completed") val lessonsCompleted: Int,
    val objectives: Int,
    val provisional: Int,
    val retained: Int,
    val due: Int,
)

@Serializable
data class NextAction(
    val kind: String,
    @SerialName("lesson_id") val lessonId: String? = null,
    val reasons: List<String> = emptyList(),
)

@Serializable
data class CourseDetail(
    val course: Course,
    val chapters: List<Chapter>,
    val diagnostic: DiagnosticInfo? = null,
)

@Serializable
data class Chapter(val id: String, val title: String, val lessons: List<PathLesson>)

@Serializable
data class PathLesson(
    val id: String,
    val title: String,
    val minutes: Int,
    val state: String,
    val certified: Boolean,
)

@Serializable
data class DiagnosticInfo(
    val id: String,
    @SerialName("session_id") val sessionId: String? = null,
    val state: String,
)

@Serializable
data class Block(val kind: String, val text: String)

@Serializable
data class Option(val id: String, val text: String, val code: Boolean = false)

@Serializable
data class Step(
    val id: String,
    val kind: String,
    val title: String,
    val body: List<Block>,
    @SerialName("hint_count") val hintCount: Int,
    val confidence: Boolean,
    val options: List<Option> = emptyList(),
    val multi: Boolean = false,
    val code: String? = null,
    val items: List<Option> = emptyList(),
    val template: String? = null,
    val placeholder: String? = null,
    val starter: String? = null,
)

@Serializable
data class Lesson(
    val id: String,
    @SerialName("course_id") val courseId: String,
    val title: String,
    val steps: List<Step>,
)

@Serializable
data class LessonProgress(
    @SerialName("current_step_id") val currentStepId: String? = null,
    @SerialName("completed_step_ids") val completedStepIds: List<String>,
    val state: String,
)

@Serializable
data class LessonView(val lesson: Lesson, val progress: LessonProgress)

@Serializable
data class DiagnosticView(val id: String, val step: Step? = null, val done: Boolean)

@Serializable
data class AttemptRequest(
    @SerialName("attempt_id") val attemptId: String,
    @SerialName("step_id") val stepId: String? = null,
    val answer: JsonObject? = null,
    val confidence: String? = null,
)

@Serializable
data class Grade(
    val result: String,
    @SerialName("assistance_state") val assistanceState: String,
    @SerialName("provisional_vs_retained") val evidence: String,
    @SerialName("gate_decision") val gateDecision: String,
    @SerialName("gate_reasons") val gateReasons: List<String>,
    @SerialName("review_due_at") val reviewDueAt: String? = null,
    @SerialName("next_step_id") val nextStepId: String? = null,
    val feedback: Feedback,
    val next: Step? = null,
    val done: Boolean = false,
)

@Serializable
data class Feedback(
    val summary: String,
    val explanation: List<Block>,
    val diagnostics: List<Diagnostic>,
    val stdout: String? = null,
    @SerialName("unavailable_reason") val unavailableReason: String? = null,
)

@Serializable
data class Diagnostic(val line: Int, val column: Int, val level: String, val message: String)

@Serializable
data class Hint(
    val level: Int,
    val text: String,
    @SerialName("hints_used") val hintsUsed: Int,
    @SerialName("hint_count") val hintCount: Int,
)

@Serializable
data class Reveal(val solution: List<Block>)

@Serializable
data class Gate(
    @SerialName("gate_decision") val decision: String,
    @SerialName("gate_reasons") val reasons: List<String>,
    val certified: Boolean,
)

@Serializable
data class ReviewQueue(val items: List<ReviewItem>, @SerialName("catch_up") val catchUp: CatchUp)

@Serializable
data class CatchUp(
    @SerialName("total_due") val totalDue: Int,
    val cap: Int,
    val sessions: Int,
    @SerialName("reviewed_today") val reviewedToday: Int = 0,
)

@Serializable
data class ReviewItem(
    val id: String,
    @SerialName("objective_title") val objectiveTitle: String,
    @SerialName("due_at") val dueAt: String,
    val reason: String,
    val step: Step,
)

@Serializable
data class EvidenceList(val objectives: List<ObjectiveEvidence>, val summary: String? = null)

@Serializable
data class ObjectiveEvidence(
    @SerialName("objective_id") val objectiveId: String,
    val title: String,
    val state: String,
    @SerialName("independent_passes") val independentPasses: Int,
    @SerialName("assisted_passes") val assistedPasses: Int,
    val failures: Int,
    @SerialName("last_independent_success") val lastIndependentSuccess: String? = null,
    @SerialName("last_delayed_success") val lastDelayedSuccess: String? = null,
    @SerialName("retention_due_at") val retentionDueAt: String? = null,
    val why: String,
    @SerialName("not_assessed") val notAssessed: Boolean,
)

fun Grade.evidenceLabel(): String = when {
    result == "unavailable" -> "Not assessed"
    result != "correct" -> "No independent pass on this attempt"
    assistanceState != "independent" -> "Assisted · not an independent pass"
    evidence == "retained" -> "Retained · checked after a delay"
    evidence == "provisional" -> "Provisional · delayed review still needed"
    else -> "Exposure recorded"
}
