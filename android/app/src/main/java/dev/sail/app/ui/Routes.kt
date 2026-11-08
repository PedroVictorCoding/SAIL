package dev.sail.app.ui

import android.net.Uri
import dev.sail.app.core.design.SailIcon

/** Every navigation route. Features navigate through the callbacks SailApp gives them. */
object Routes {
    const val HOME = "home"
    const val DIALOGUE = "dialogue"
    const val ARENA = "arena"
    const val COURSE = "arena/course/{courseId}"
    const val LESSON = "arena/lesson/{lessonId}"
    const val REVIEW = "arena/review"
    const val WORKSPACE = "workspace"
    const val LIBRARY = "library"
    const val BOOK = "library/book/{bookId}"
    /** The e-reader. `section` is optional: absent resumes at the saved position. */
    const val READ = "library/read/{bookId}?section={section}"
    const val TRACKER = "tracker"
    const val SETTINGS = "settings"

    fun course(id: String) = "arena/course/${Uri.encode(id)}"
    fun lesson(id: String) = "arena/lesson/${Uri.encode(id)}"
    fun book(id: String) = "library/book/${Uri.encode(id)}"
    fun read(id: String, section: Int? = null) =
        "library/read/${Uri.encode(id)}" + (section?.let { "?section=$it" } ?: "")
}

enum class TopLevel(val route: String, val label: String, val icon: SailIcon) {
    Home(Routes.HOME, "Home", SailIcon.Home),
    Dialogue(Routes.DIALOGUE, "Dialogue", SailIcon.Dialogue),
    Arena(Routes.ARENA, "Arena", SailIcon.Arena),
    Workspace(Routes.WORKSPACE, "Workspace", SailIcon.Spaces),
    Library(Routes.LIBRARY, "Library", SailIcon.Entertainment),
    ;

    companion object {
        /** The tab a route belongs to: "arena/lesson/x" selects Arena. Tracker sits under Home. */
        fun of(route: String?): TopLevel? = when {
            route == null -> null
            route == Routes.TRACKER -> Home
            else -> entries.firstOrNull { route == it.route || route.startsWith(it.route + "/") }
        }
    }
}
