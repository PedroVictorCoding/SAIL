package dev.sail.app

import android.graphics.Color
import android.os.Bundle
import androidx.activity.SystemBarStyle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.CompositionLocalProvider
import dev.sail.app.core.LocalAppContainer
import dev.sail.app.core.design.SailTheme
import dev.sail.app.ui.SailApp

class MainActivity : ComponentActivity() {
    private val container get() = (application as SailApplication).container

    override fun onCreate(savedInstanceState: Bundle?) {
        // SAIL is always dark: light status/navigation icons on transparent bars.
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
        )
        super.onCreate(savedInstanceState)
        setContent {
            CompositionLocalProvider(LocalAppContainer provides container) {
                SailTheme {
                    SailApp()
                }
            }
        }
    }

    override fun onStart() {
        super.onStart()
        container.connection.start()
        container.heartbeats.start()
    }

    override fun onStop() {
        container.heartbeats.stop()
        container.connection.stop()
        super.onStop()
    }
}
