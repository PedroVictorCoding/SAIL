package dev.sail.app.core

import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf

/**
 * Lets a screen hide the tab bar while leaving would lose work, e.g. a course answer that is
 * being submitted or a diagnostic in progress. Locks are counted, so nested screens can each
 * hold one; each is released when its [LockNavigation] leaves the composition.
 */
class NavLock {
    private var holders by mutableIntStateOf(0)

    val locked: Boolean get() = holders > 0

    internal fun acquire() {
        holders++
    }

    internal fun release() {
        holders = (holders - 1).coerceAtLeast(0)
    }
}

val LocalNavLock = staticCompositionLocalOf { NavLock() }

/** While [locked] is true, the tab bar is hidden; the screen's own Back decides how to leave. */
@Composable
fun LockNavigation(locked: Boolean) {
    val lock = LocalNavLock.current
    DisposableEffect(lock, locked) {
        if (locked) lock.acquire()
        onDispose {
            if (locked) lock.release()
        }
    }
}
