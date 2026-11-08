package dev.sail.app.core.net

/** The outcome of one SAIL API call. HTTP and network failures are values, never exceptions. */
sealed interface ApiResult<out T> {
    data class Ok<T>(val data: T) : ApiResult<T>

    /**
     * [status] is the HTTP status, or 0 when the server couldn't be reached or the response
     * couldn't be read. [message] is safe to show to the owner.
     */
    data class Err(val status: Int, val message: String) : ApiResult<Nothing> {
        val unreachable: Boolean get() = status == 0
        val unauthorized: Boolean get() = status == 401 || status == 403
        val unavailable: Boolean get() = status == 503
    }
}

inline fun <T, R> ApiResult<T>.map(transform: (T) -> R): ApiResult<R> = when (this) {
    is ApiResult.Ok -> ApiResult.Ok(transform(data))
    is ApiResult.Err -> this
}

fun <T> ApiResult<T>.getOrNull(): T? = (this as? ApiResult.Ok)?.data

/** A non-JSON response body. Header names are lower-case. */
class BinaryBody(val bytes: ByteArray, val headers: Map<String, String>)
