package dev.sail.app.core.net

import android.content.Context
import android.content.SharedPreferences
import android.util.Log
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

/** Where SAIL is and the owner token used to reach it. */
data class ServerConfig(val baseUrl: HttpUrl, val token: String)

/**
 * Stores the server URL and owner token with Android Keystore-backed encryption. If the
 * encrypted store can't be opened, nothing is written to disk: the settings live in memory
 * for this run only, and [persistent] is false so the UI can say so.
 */
class ServerSettings(context: Context) {
    private val prefs: SharedPreferences? = openEncrypted(context)
    private val state = MutableStateFlow(load())

    val config: StateFlow<ServerConfig?> = state.asStateFlow()
    val persistent: Boolean get() = prefs != null

    fun save(config: ServerConfig) {
        prefs?.edit()
            ?.putString(KEY_URL, config.baseUrl.toString())
            ?.putString(KEY_TOKEN, config.token)
            ?.apply()
        state.value = config
    }

    fun clear() {
        prefs?.edit()?.clear()?.apply()
        state.value = null
    }

    private fun load(): ServerConfig? {
        val url = prefs?.getString(KEY_URL, null)?.toHttpUrlOrNull() ?: return null
        val token = prefs.getString(KEY_TOKEN, null)?.takeIf { it.isNotBlank() } ?: return null
        return ServerConfig(url, token)
    }

    companion object {
        const val DEFAULT_URL = "https://sail.spinethorns.com"
        private const val FILE = "sail_server"
        private const val KEY_URL = "base_url"
        private const val KEY_TOKEN = "owner_token"

        /** Parses what the owner typed; adds https:// when no scheme was given. */
        fun parseUrl(input: String): HttpUrl? {
            // Decide the scheme before trimming slashes: "https://" alone must stay invalid,
            // not turn into "https:" and then the host "https".
            val trimmed = input.trim()
            if (trimmed.isEmpty()) return null
            val withScheme = if ("://" in trimmed) trimmed else "https://$trimmed"
            val url = withScheme.trimEnd('/').toHttpUrlOrNull() ?: return null
            return url.takeIf { (it.scheme == "https" || it.scheme == "http") && it.host.isNotBlank() }
        }

        private fun openEncrypted(context: Context): SharedPreferences? = try {
            val key = MasterKey.Builder(context)
                .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
                .build()
            EncryptedSharedPreferences.create(
                context,
                FILE,
                key,
                EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
            )
        } catch (error: Exception) {
            Log.w("SAIL", "Encrypted settings unavailable; keeping them in memory only", error)
            null
        }
    }
}
