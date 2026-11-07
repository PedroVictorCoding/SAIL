package dev.sail.app

import android.app.Application
import dev.sail.app.core.AppContainer

class SailApplication : Application() {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        container = AppContainer(this)
    }
}
