package dev.sail.app

import android.os.Bundle
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
        enableEdgeToEdge()
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
    }

    override fun onStop() {
        container.connection.stop()
        super.onStop()
    }
}
