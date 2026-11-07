package dev.sail.app.feature.library

import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi

interface LibrarySource {
    suspend fun library(): ApiResult<ReaderLibrary>

    suspend fun book(id: String): ApiResult<ReaderBookDetail>

    suspend fun music(): ApiResult<MusicLibrary>

    fun coverUrl(book: ReaderBook): String? = null

    fun trackCoverUrl(track: MusicTrack): String? = null
}

class LibraryRepository(private val api: SailApi) : LibrarySource {
    override suspend fun library(): ApiResult<ReaderLibrary> = api.get<ReaderLibrary>("/api/reader/library")

    override suspend fun book(id: String): ApiResult<ReaderBookDetail> =
        api.get<ReaderBookDetail>("/api/reader/books/$id")

    override suspend fun music(): ApiResult<MusicLibrary> = api.get<MusicLibrary>("/api/music/library")

    override fun coverUrl(book: ReaderBook): String? {
        val version = book.coverVersion?.takeIf { it.isNotBlank() } ?: return null
        return api.url("/api/reader/books/${book.id}/cover")
            ?.newBuilder()
            ?.addQueryParameter("v", version)
            ?.build()
            ?.toString()
    }

    override fun trackCoverUrl(track: MusicTrack): String? {
        if (!track.hasCover) return null
        return api.url("/api/music/tracks/${track.id}/cover")?.toString()
    }
}
