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
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
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
import dev.sail.app.core.LocalNavLock
import dev.sail.app.core.NavLock
import dev.sail.app.core.design.SailColors
import dev.sail.app.core.design.SailIconImage
import dev.sail.app.feature.arena.ArenaScreen
import dev.sail.app.feature.arena.CourseScreen
import dev.sail.app.feature.arena.LessonScreen
import dev.sail.app.feature.arena.ReviewScreen
import dev.sail.app.feature.dialogue.DialogueScreen
import dev.sail.app.feature.calendar.CalendarScreen
import dev.sail.app.feature.dock.DockScreen
import dev.sail.app.feature.nightshift.NightShiftScreen
import dev.sail.app.feature.dock.SurfaceKind
import dev.sail.app.feature.home.HomeScreen
import dev.sail.app.feature.library.BookScreen
import dev.sail.app.feature.library.LibraryScreen
import dev.sail.app.feature.reader.ReaderScreen
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
    val navLock = remember { NavLock() }
    val charging by container.heartbeats.charging.collectAsStateWithLifecycle()
    val openDockOnCharge by container.heartbeats.openDockWhenCharging.collectAsStateWithLifecycle()

    LaunchedEffect(route) {
        if (route != Routes.DOCK) {
            container.heartbeats.setSurface(SurfaceKind.PHONE, voiceReady = false)
        }
    }

    LaunchedEffect(charging, openDockOnCharge, config) {
        if (config != null && charging && openDockOnCharge && route != Routes.DOCK) {
            nav.navigate(Routes.DOCK)
        }
    }

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
            CompositionLocalProvider(LocalNavLock provides navLock) {
                SailNavHost(nav, start)
            }
        }
        val tab = TopLevel.of(route)
        // Lessons and reviews are focused, one step per screen: no tab bar, so a pending answer
        // can't be abandoned by switching tabs. Their own Back handles leaving.
        // Other screens (a diagnostic on the course path) hold a NavLock while work is pending.
        // The reader is full-screen too: the page is the whole screen.
        val focused = route == Routes.LESSON || route == Routes.REVIEW || route == Routes.READ || navLock.locked
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
                onOpenDock = { nav.navigate(Routes.DOCK) },
                onOpenNightShift = { nav.navigate(Routes.NIGHT_SHIFT) },
                onOpenCalendar = { nav.navigate(Routes.CALENDAR) },
            )
        }
        composable(Routes.DOCK) {
            DockScreen(onExit = { nav.popBackStack() })
        }
        composable(Routes.CALENDAR) {
            CalendarScreen(onBack = { nav.popBackStack() })
        }
        composable(Routes.NIGHT_SHIFT) {
            NightShiftScreen(
                onBack = { nav.popBackStack() },
                onOpenSettings = { nav.navigate(Routes.SETTINGS) },
                onAskInDialogue = { nav.navigateTopLevel(Routes.DIALOGUE) },
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
            LibraryScreen(
                onOpenBook = { nav.navigate(Routes.book(it)) },
                onReadBook = { nav.navigate(Routes.read(it)) },
            )
        }
        composable(
            Routes.BOOK,
            arguments = listOf(navArgument("bookId") { type = NavType.StringType }),
        ) { backStack ->
            BookScreen(
                bookId = backStack.arguments?.getString("bookId").orEmpty(),
                onBack = { nav.popBackStack() },
                onRead = { bookId, section -> nav.navigate(Routes.read(bookId, section)) },
            )
        }
        composable(
            Routes.READ,
            arguments = listOf(
                navArgument("bookId") { type = NavType.StringType },
                navArgument("section") {
                    type = NavType.IntType
                    defaultValue = -1
                },
            ),
        ) { backStack ->
            ReaderScreen(
                bookId = backStack.arguments?.getString("bookId").orEmpty(),
                startSection = backStack.arguments?.getInt("section")?.takeIf { it >= 0 },
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
