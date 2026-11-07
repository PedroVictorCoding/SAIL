package dev.sail.app.feature.arena

import androidx.compose.runtime.Composable
import dev.sail.app.core.design.ScreenNotBuiltYet

// Owner: Codex. Split into more files under feature/arena as you like; keep these signatures.

/** Arena home: real stat tiles, Courses list, due reviews. */
@Composable
fun ArenaScreen(
    onOpenCourse: (courseId: String) -> Unit,
    onOpenLesson: (lessonId: String) -> Unit,
    onOpenReviews: () -> Unit,
) {
    ScreenNotBuiltYet("Arena", subtitle = "Learn · Evaluate · Practice")
}

/** One course's path: lessons with their server-side state, diagnostic, review checkpoint. */
@Composable
fun CourseScreen(courseId: String, onOpenLesson: (lessonId: String) -> Unit, onBack: () -> Unit) {
    ScreenNotBuiltYet("Course", onBack = onBack)
}

/** Lesson player: one step per screen, Check, hint ladder, reveal. [onOpenLesson] opens the next one. */
@Composable
fun LessonScreen(lessonId: String, onBack: () -> Unit, onOpenLesson: (lessonId: String) -> Unit) {
    ScreenNotBuiltYet("Lesson", onBack = onBack)
}

/** Due review queue. */
@Composable
fun ReviewScreen(onBack: () -> Unit) {
    ScreenNotBuiltYet("Due reviews", onBack = onBack)
}
