package dev.sail.app.core.design

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/** Phase-0 stand-in for a screen that an agent is still building. Remove when it lands. */
@Composable
fun ScreenNotBuiltYet(title: String, subtitle: String? = null, onBack: (() -> Unit)? = null) {
    Column(Modifier.fillMaxSize()) {
        SailTopBar(title = title, subtitle = subtitle, onBack = onBack)
        EmptyState(
            title = "$title isn't in this build yet",
            detail = "This screen is still being built.",
            modifier = Modifier.padding(ScreenGutter),
        )
    }
}
