package dev.sail.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.NavHostController
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import dev.sail.app.core.LocalAppContainer
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.SailIconImage
import dev.sail.app.feature.arena.ArenaScreen
import dev.sail.app.feature.arena.CourseScreen
import dev.sail.app.feature.arena.LessonScreen
import dev.sail.app.feature.arena.ReviewScreen
import dev.sail.app.feature.dialogue.DialogueScreen
import dev.sail.app.feature.home.HomeScreen
import dev.sail.app.feature.library.BookScreen
import dev.sail.app.feature.library.LibraryScreen
import dev.sail.app.feature.settings.SettingsScreen
import dev.sail.app.feature.tracker.TrackerScreen
import dev.sail.app.feature.workspace.WorkspaceScreen

@Composable
fun SailApp() {
    val container = LocalAppContainer.current
    val config by container.settings.config.collectAsStateWithLifecycle()
    val nav = rememberNavController()
    val entry by nav.currentBackStackEntryAsState()
    val route = entry?.destination?.route
    // Fixed for the life of the NavHost; finishing Settings navigates to Home itself.
    val start = remember { if (config == null) Routes.SETTINGS else Routes.HOME }

    Column(
        Modifier
            .fillMaxSize()
            .background(SailColors.Base)
            .windowInsetsPadding(WindowInsets.safeDrawing.only(WindowInsetsSides.Horizontal + WindowInsetsSides.Top)),
    ) {
        Box(
            Modifier
                .weight(1f)
                .fillMaxWidth()
                .imePadding(),
        ) {
            SailNavHost(nav, start)
        }
        val tab = TopLevel.of(route)
        // Lessons and reviews are focused, one step per screen: no tab bar, so a pending answer
        // can't be abandoned by switching tabs. Their own Back handles leaving.
        val focused = route == Routes.LESSON || route == Routes.REVIEW
        if (tab != null && config != null && !focused) {
            BottomNav(selected = tab, onSelect = { nav.navigateTopLevel(it.route) })
        }
    }
}

@Composable
private fun SailNavHost(nav: NavHostController, start: String) {
    NavHost(navController = nav, startDestination = start) {
        composable(Routes.HOME) {
            HomeScreen(
                onOpenTracker = { nav.navigate(Routes.TRACKER) },
                onOpenArena = { nav.navigateTopLevel(Routes.ARENA) },
                onOpenCourse = { nav.navigate(Routes.course(it)) },
                onOpenLesson = { nav.navigate(Routes.lesson(it)) },
                onOpenLibrary = { nav.navigateTopLevel(Routes.LIBRARY) },
                onOpenBook = { nav.navigate(Routes.book(it)) },
                onOpenDialogue = { nav.navigateTopLevel(Routes.DIALOGUE) },
                onOpenSettings = { nav.navigate(Routes.SETTINGS) },
            )
        }
        composable(Routes.TRACKER) {
            TrackerScreen(onBack = { nav.popBackStack() })
        }
        composable(Routes.DIALOGUE) {
            DialogueScreen()
        }
        composable(Routes.ARENA) {
            ArenaScreen(
                onOpenCourse = { nav.navigate(Routes.course(it)) },
                onOpenLesson = { nav.navigate(Routes.lesson(it)) },
                onOpenReviews = { nav.navigate(Routes.REVIEW) },
            )
        }
        composable(
            Routes.COURSE,
            arguments = listOf(navArgument("courseId") { type = NavType.StringType }),
        ) { backStack ->
            CourseScreen(
                courseId = backStack.arguments?.getString("courseId").orEmpty(),
                onOpenLesson = { nav.navigate(Routes.lesson(it)) },
                onBack = { nav.popBackStack() },
            )
        }
        composable(
            Routes.LESSON,
            arguments = listOf(navArgument("lessonId") { type = NavType.StringType }),
        ) { backStack ->
            LessonScreen(
                lessonId = backStack.arguments?.getString("lessonId").orEmpty(),
                onBack = { nav.popBackStack() },
                onOpenLesson = { next ->
                    nav.navigate(Routes.lesson(next)) { popUpTo(Routes.LESSON) { inclusive = true } }
                },
            )
        }
        composable(Routes.REVIEW) {
            ReviewScreen(onBack = { nav.popBackStack() })
        }
        composable(Routes.WORKSPACE) {
            WorkspaceScreen()
        }
        composable(Routes.LIBRARY) {
            LibraryScreen(onOpenBook = { nav.navigate(Routes.book(it)) })
        }
        composable(
            Routes.BOOK,
            arguments = listOf(navArgument("bookId") { type = NavType.StringType }),
        ) { backStack ->
            BookScreen(
                bookId = backStack.arguments?.getString("bookId").orEmpty(),
                onBack = { nav.popBackStack() },
            )
        }
        composable(Routes.SETTINGS) {
            SettingsScreen(
                onDone = {
                    nav.navigate(Routes.HOME) {
                        popUpTo(nav.graph.id) { inclusive = true }
                        launchSingleTop = true
                    }
                },
                onBack = if (nav.previousBackStackEntry != null) ({ nav.popBackStack() }) else null,
            )
        }
    }
}

private fun NavHostController.navigateTopLevel(route: String) {
    navigate(route) {
        popUpTo(graph.findStartDestination().id) { saveState = true }
        launchSingleTop = true
        restoreState = true
    }
}

@Composable
private fun BottomNav(selected: TopLevel, onSelect: (TopLevel) -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(Color(0xF006070A))
            .border(width = 1.dp, color = SailColors.Line)
            .windowInsetsPadding(WindowInsets.navigationBars)
            .padding(horizontal = 8.dp, vertical = 6.dp)
            .selectableGroup(),
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        for (item in TopLevel.entries) {
            val isSelected = item == selected
            // Selected has shape (pill) and weight as well as colour (DESIGN.md).
            Column(
                modifier = Modifier
                    .weight(1f)
                    .heightIn(min = 52.dp)
                    .clip(RoundedCornerShape(14.dp))
                    .background(if (isSelected) Color(0x295B7CFF) else Color.Transparent)
                    .selectable(selected = isSelected, role = Role.Tab, onClick = { onSelect(item) })
                    .padding(vertical = 6.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                SailIconImage(
                    item.icon,
                    contentDescription = null,
                    tint = if (isSelected) Color(0xFFB9C6FF) else SailColors.TextSubtle,
                )
                Text(
                    item.label,
                    fontSize = 11.sp,
                    fontWeight = if (isSelected) FontWeight.SemiBold else FontWeight.Normal,
                    color = if (isSelected) Color.White else SailColors.TextSubtle,
                    maxLines = 1,
                )
            }
        }
    }
}
