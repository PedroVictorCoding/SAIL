package dev.sail.app.core.net

import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.DeserializationStrategy
import kotlinx.serialization.SerializationException
import kotlinx.serialization.SerializationStrategy
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.serializer
import okhttp3.Call
import okhttp3.Callback
import okhttp3.HttpUrl
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.io.IOException
import javax.net.ssl.SSLException
import kotlin.coroutines.resume

/**
 * The one client for SAIL's HTTP and WebSocket APIs. Paths are server paths such as
 * "/api/learn/courses". Every call returns an [ApiResult]; nothing throws for HTTP or network
 * failures. The owner token is only ever sent to the configured SAIL host.
 */
class SailApi(
    private val config: () -> ServerConfig?,
    val json: Json = defaultJson,
    baseClient: OkHttpClient = OkHttpClient(),
) {
    /**
     * Shared client that adds the owner token (and the Cloudflare Access service token, if set)
     * for SAIL's host only. Use it for images too.
     */
    val client: OkHttpClient = baseClient.newBuilder()
        .addInterceptor(AuthInterceptor(config))
        .build()

    suspend fun <T> get(path: String, deserializer: DeserializationStrategy<T>): ApiResult<T> =
        call("GET", path, null, deserializer)

    suspend fun <T> delete(path: String, deserializer: DeserializationStrategy<T>): ApiResult<T> =
        call("DELETE", path, null, deserializer)

    suspend fun <B, T> post(
        path: String,
        body: B,
        bodySerializer: SerializationStrategy<B>,
        deserializer: DeserializationStrategy<T>,
    ): ApiResult<T> = call("POST", path, encode(body, bodySerializer), deserializer)

    suspend fun <B, T> put(
        path: String,
        body: B,
        bodySerializer: SerializationStrategy<B>,
        deserializer: DeserializationStrategy<T>,
    ): ApiResult<T> = call("PUT", path, encode(body, bodySerializer), deserializer)

    /** POST with an empty body, e.g. "/api/learn/reviews/{id}/hint". */
    suspend fun <T> post(path: String, deserializer: DeserializationStrategy<T>): ApiResult<T> =
        call("POST", path, ByteArray(0).toRequestBody(null), deserializer)

    suspend inline fun <reified T> get(path: String): ApiResult<T> = get(path, serializer<T>())

    suspend inline fun <reified T> post(path: String): ApiResult<T> = post(path, serializer<T>())

    /** POST a JSON body: `api.postJson<AttemptRequest, Grade>(path, request)`. */
    suspend inline fun <reified B, reified T> postJson(path: String, body: B): ApiResult<T> =
        post(path, body, serializer<B>(), serializer<T>())

    suspend inline fun <reified B, reified T> putJson(path: String, body: B): ApiResult<T> =
        put(path, body, serializer<B>(), serializer<T>())

    /**
     * POST a JSON body and receive raw bytes, e.g. PCM from "/api/reader/speech". Errors are
     * read like [call]'s: the server's `error` message when it sends one.
     */
    suspend fun <B> postForBytes(
        path: String,
        body: B,
        bodySerializer: SerializationStrategy<B>,
    ): ApiResult<BinaryBody> {
        val target = url(path) ?: return ApiResult.Err(0, "SAIL isn't set up yet")
        val request = Request.Builder()
            .url(target)
            .header("Accept", "application/octet-stream")
            .header("Cache-Control", "no-store")
            .post(encode(body, bodySerializer))
            .build()
        val response = try {
            client.newCall(request).await()
        } catch (error: SSLException) {
            return ApiResult.Err(0, "This phone doesn't trust SAIL's certificate")
        } catch (error: IOException) {
            return ApiResult.Err(0, "SAIL is unreachable")
        }
        return response.use {
            accessRefusal(it)?.let { refusal -> return@use refusal }
            val bytes = try {
                it.body?.bytes() ?: ByteArray(0)
            } catch (error: IOException) {
                return@use ApiResult.Err(0, "The response from SAIL was cut off")
            }
            if (!it.isSuccessful) {
                ApiResult.Err(it.code, errorMessage(it.code, bytes.decodeToString()))
            } else {
                ApiResult.Ok(BinaryBody(bytes, it.headers.names().associate { name -> name.lowercase() to it.header(name).orEmpty() }))
            }
        }
    }

    suspend inline fun <reified B> postJsonForBytes(path: String, body: B): ApiResult<BinaryBody> =
        postForBytes(path, body, serializer<B>())

    /** Absolute URL of a server path (covers, map tiles), or null when SAIL isn't configured. */
    fun url(path: String): HttpUrl? = config()?.baseUrl?.resolve(path)

    /**
     * Opens a WebSocket such as "/ws/voice". Returns null when SAIL isn't configured. The
     * caller owns the socket and must close it.
     */
    fun webSocket(path: String, listener: WebSocketListener): WebSocket? {
        val target = url(path) ?: return null
        // OkHttp accepts http(s) URLs for WebSockets and upgrades them itself.
        return client.newWebSocket(Request.Builder().url(target).build(), listener)
    }

    private fun <B> encode(body: B, serializer: SerializationStrategy<B>): RequestBody =
        json.encodeToString(serializer, body).toRequestBody(JSON_TYPE)

    private suspend fun <T> call(
        method: String,
        path: String,
        body: RequestBody?,
        deserializer: DeserializationStrategy<T>,
    ): ApiResult<T> {
        val target = url(path) ?: return ApiResult.Err(0, "SAIL isn't set up yet")
        val request = Request.Builder()
            .url(target)
            .header("Accept", "application/json")
            .header("Cache-Control", "no-store")
            .method(method, body)
            .build()
        val response = try {
            client.newCall(request).await()
        } catch (error: SSLException) {
            // Reachable, but the phone doesn't trust SAIL's certificate (e.g. the self-signed
            // SAIL-local one before it's installed): say so instead of "unreachable".
            return ApiResult.Err(0, "This phone doesn't trust SAIL's certificate")
        } catch (error: IOException) {
            return ApiResult.Err(0, "SAIL is unreachable")
        }
        return response.use { parse(it, deserializer) }
    }

    private fun <T> parse(response: Response, deserializer: DeserializationStrategy<T>): ApiResult<T> {
        accessRefusal(response)?.let { return it }
        val text = try {
            response.body?.string().orEmpty()
        } catch (error: IOException) {
            return ApiResult.Err(0, "The response from SAIL was cut off")
        }
        if (!response.isSuccessful) {
            return ApiResult.Err(response.code, errorMessage(response.code, text))
        }
        return try {
            ApiResult.Ok(json.decodeFromString(deserializer, text.ifEmpty { "null" }))
        } catch (error: SerializationException) {
            ApiResult.Err(0, "SAIL sent a response this app doesn't understand")
        } catch (error: IllegalArgumentException) {
            ApiResult.Err(0, "SAIL sent a response this app doesn't understand")
        }
    }

    /**
     * Cloudflare Access answers an unauthenticated client by redirecting to its login page,
     * which OkHttp follows. That's not SAIL speaking, so say what actually happened.
     */
    private fun accessRefusal(response: Response): ApiResult.Err? {
        if (!response.request.url.host.endsWith(".cloudflareaccess.com") &&
            response.header("cf-access-domain") == null
        ) {
            return null
        }
        return ApiResult.Err(
            403,
            "Cloudflare Access didn't let this phone through. Check the Access service " +
                "token and that a Service Auth policy for it is attached to the SAIL application.",
        )
    }

    private fun errorMessage(status: Int, body: String): String {
        val fromBody = try {
            val obj = json.parseToJsonElement(body) as? JsonObject
            listOf("error", "message").firstNotNullOfOrNull { key ->
                (obj?.get(key) as? JsonPrimitive)?.takeIf { it.isString }?.content
            }
        } catch (error: SerializationException) {
            null
        }
        return fromBody ?: when (status) {
            401, 403 -> "The owner token was refused. Check it in Settings."
            404 -> "This SAIL server doesn't have that feature"
            503 -> "That SAIL subsystem is unavailable right now"
            else -> "SAIL returned HTTP $status"
        }
    }

    private class AuthInterceptor(private val config: () -> ServerConfig?) : Interceptor {
        override fun intercept(chain: Interceptor.Chain): Response {
            val request = chain.request()
            val current = config() ?: return chain.proceed(request)
            val sameHost = request.url.host == current.baseUrl.host &&
                request.url.port == current.baseUrl.port
            if (!sameHost) return chain.proceed(request)
            val builder = request.newBuilder()
            if (current.token.isNotEmpty()) builder.header("Authorization", "Bearer ${current.token}")
            current.access?.let { access ->
                builder.header("CF-Access-Client-Id", access.clientId)
                builder.header("CF-Access-Client-Secret", access.clientSecret)
            }
            return chain.proceed(builder.build())
        }
    }

    companion object {
        private val JSON_TYPE = "application/json; charset=utf-8".toMediaType()

        val defaultJson = Json {
            ignoreUnknownKeys = true
            explicitNulls = false
            coerceInputValues = true
            isLenient = false
        }

        /** For calls whose response body doesn't matter (or isn't JSON-shaped yet). */
        val anyJson: DeserializationStrategy<JsonElement> = JsonElement.serializer()
    }
}

private suspend fun Call.await(): Response = suspendCancellableCoroutine { continuation ->
    continuation.invokeOnCancellation { cancel() }
    enqueue(
        object : Callback {
            override fun onResponse(call: Call, response: Response) {
                continuation.resume(response)
            }

            override fun onFailure(call: Call, e: IOException) {
                if (continuation.isActive) {
                    continuation.resumeWith(Result.failure(e))
                }
            }
        },
    )
}
