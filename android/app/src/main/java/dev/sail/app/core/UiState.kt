package dev.sail.app.core

import dev.sail.app.core.net.ApiResult

/** What a screen (or one card) shows. Never a fabricated value: Empty and Unavailable are honest. */
sealed interface UiState<out T> {
    data object Loading : UiState<Nothing>
    data class Ready<T>(val data: T) : UiState<T>
    data class Empty(val title: String, val detail: String? = null) : UiState<Nothing>
    data class Unavailable(val message: String) : UiState<Nothing>
}

/** Maps an API result: errors become [UiState.Unavailable], [isEmpty] data becomes [UiState.Empty]. */
inline fun <T> ApiResult<T>.toUiState(
    emptyTitle: String = "Nothing here yet",
    emptyDetail: String? = null,
    isEmpty: (T) -> Boolean = { false },
): UiState<T> = when (this) {
    is ApiResult.Ok -> if (isEmpty(data)) UiState.Empty(emptyTitle, emptyDetail) else UiState.Ready(data)
    is ApiResult.Err -> UiState.Unavailable(message)
}
