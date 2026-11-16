package dev.sail.app.core

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import coil.ImageLoader
import dev.sail.app.core.net.Connection
import dev.sail.app.core.net.SailApi
import dev.sail.app.core.net.ServerSettings
import dev.sail.app.feature.dock.Heartbeats
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.Dispatchers

/** App-wide singletons. Features get them through [sailViewModel], not globals. */
class AppContainer(context: Context) {
    val appScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    val settings = ServerSettings(context)
    val api = SailApi(config = { settings.config.value })
    val connection = Connection(api, settings, appScope)
    val heartbeats = Heartbeats(context.applicationContext, api, appScope)

    /** Coil loader on the authenticated client, for covers and map tiles served by SAIL. */
    val imageLoader: ImageLoader = ImageLoader.Builder(context)
        .okHttpClient(api.client)
        .crossfade(false)
        .build()
}

val LocalAppContainer = staticCompositionLocalOf<AppContainer> {
    error("AppContainer is provided by MainActivity")
}

/**
 * A ViewModel built from the app's services:
 * `val vm = sailViewModel { ArenaViewModel(ArenaRepository(it.api)) }`.
 * Pass [key] when one screen type shows different items (e.g. a course id).
 */
@Composable
inline fun <reified VM : ViewModel> sailViewModel(
    key: String? = null,
    crossinline create: (AppContainer) -> VM,
): VM {
    val container = LocalAppContainer.current
    return viewModel(
        key = key,
        factory = viewModelFactory { initializer { create(container) } },
    )
}
