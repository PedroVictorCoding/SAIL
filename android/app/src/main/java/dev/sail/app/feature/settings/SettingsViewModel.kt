package dev.sail.app.feature.settings

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.sail.app.core.net.AccessToken
import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.Connection
import dev.sail.app.core.net.SailApi
import dev.sail.app.core.net.ServerConfig
import dev.sail.app.core.net.ServerSettings
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

@Serializable
data class OwnerTokenResponse(val token: String, val configured: Boolean = true)

data class SettingsUiState(
    val url: String,
    val token: String,
    /** Optional Cloudflare Access service token (both parts or neither). */
    val accessId: String = "",
    val accessSecret: String = "",
    val busy: Boolean = false,
    /** Result of the last check, shown under the form. */
    val message: String? = null,
    val messageIsError: Boolean = false,
    val persistent: Boolean = true,
    val configured: Boolean = false,
)

/** First-run setup and later edits: server URL and owner token, checked before saving. */
class SettingsViewModel(
    private val settings: ServerSettings,
    private val connection: Connection,
    private val probe: (ServerConfig) -> SailApi = { draft -> SailApi(config = { draft }) },
) : ViewModel() {
    private val current = settings.config.value
    private val mutable = MutableStateFlow(
        SettingsUiState(
            url = current?.baseUrl?.toString()?.trimEnd('/') ?: ServerSettings.DEFAULT_URL,
            token = current?.token.orEmpty(),
            accessId = current?.access?.clientId.orEmpty(),
            accessSecret = current?.access?.clientSecret.orEmpty(),
            persistent = settings.persistent,
            configured = current != null,
        ),
    )
    val state: StateFlow<SettingsUiState> = mutable.asStateFlow()

    fun onUrlChange(value: String) = mutable.update { it.copy(url = value, message = null) }

    fun onTokenChange(value: String) = mutable.update { it.copy(token = value, message = null) }

    fun onAccessIdChange(value: String) = mutable.update { it.copy(accessId = value, message = null) }

    fun onAccessSecretChange(value: String) = mutable.update { it.copy(accessSecret = value, message = null) }

    /** Null when both Access fields are empty; an error message when only one is filled. */
    private fun accessToken(): Result<AccessToken?> {
        val id = state.value.accessId.trim()
        val secret = state.value.accessSecret.trim()
        return when {
            id.isEmpty() && secret.isEmpty() -> Result.success(null)
            id.isEmpty() || secret.isEmpty() -> Result.failure(IllegalArgumentException("Enter both the Access client ID and secret, or neither"))
            else -> Result.success(AccessToken(id, secret))
        }
    }

    /** Asks the server for its owner token (works only where the server allows it, e.g. on the tailnet). */
    fun fetchToken() {
        val url = ServerSettings.parseUrl(state.value.url) ?: return fail("Enter a valid server address")
        val access = accessToken().getOrElse { return fail(it.message.orEmpty()) }
        launchBusy {
            when (val result = probe(ServerConfig(url, "", access)).get("/api/guardian/token", OwnerTokenResponse.serializer())) {
                is ApiResult.Ok -> mutable.update {
                    it.copy(token = result.data.token, message = "Token received from SAIL", messageIsError = false)
                }
                is ApiResult.Err -> fail("Couldn't get a token: ${result.message}")
            }
        }
    }

    /** Checks the address and token against the server, then saves them. */
    fun save(onSaved: () -> Unit) {
        val url = ServerSettings.parseUrl(state.value.url) ?: return fail("Enter a valid server address")
        val token = state.value.token.trim()
        if (token.length < 32) return fail("The owner token is at least 32 characters")
        val access = accessToken().getOrElse { return fail(it.message.orEmpty()) }
        val draft = ServerConfig(url, token, access)
        launchBusy {
            val api = probe(draft)
            val health = api.get("/api/health", JsonElement.serializer())
            if (health is ApiResult.Err) return@launchBusy fail("SAIL didn't answer: ${health.message}")
            // A Bearer-protected read proves the token, not just the address.
            val auth = api.get("/api/tracker/config", JsonElement.serializer())
            if (auth is ApiResult.Err && auth.unauthorized) return@launchBusy fail("SAIL refused this owner token")
            settings.save(draft)
            connection.refresh()
            mutable.update { it.copy(message = "Connected", messageIsError = false, configured = true) }
            onSaved()
        }
    }

    fun forget() {
        settings.clear()
        connection.refresh()
        mutable.update { it.copy(token = "", configured = false, message = "Server forgotten", messageIsError = false) }
    }

    private fun fail(message: String) = mutable.update { it.copy(busy = false, message = message, messageIsError = true) }

    private fun launchBusy(block: suspend () -> Unit) {
        mutable.update { it.copy(busy = true, message = null) }
        viewModelScope.launch {
            try {
                block()
            } finally {
                mutable.update { it.copy(busy = false) }
            }
        }
    }
}
