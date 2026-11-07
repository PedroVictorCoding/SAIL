package dev.sail.app.feature.home

import androidx.compose.runtime.Composable
import dev.sail.app.core.design.ScreenNotBuiltYet

/** Owner: Gemini. Return briefing, Music, Arena resume, System summary, link to Tracker. */
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
    ScreenNotBuiltYet("Home")
}
