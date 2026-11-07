package dev.sail.app.feature.home

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.sail.app.core.UiState
import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.getOrNull
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

data class HomeMusicCardModel(
    val title: String,
    val subtitle: String,
    val badge: String? = null,
    val playableUri: String? = null,
    val isPlaying: Boolean = false,
    val confirmed: Boolean = false,
)

data class HomeArenaCardModel(
    val title: String,
    val subtitle: String,
    val courseId: String? = null,
    val nextLessonId: String? = null,
    val nextKind: String? = null,
    val lessonsCompleted: Int = 0,
    val lessonCount: Int = 0,
    val dueCount: Int = 0,
)

data class HomeSystemCardModel(
    val workspaceName: String,
    val statusSummary: String,
    val storageLabel: String,
    val memoryLabel: String,
    val cpuLabel: String? = null,
    val forgeSummary: String? = null,
    val lastEditedFile: String? = null,
    val pendingApprovals: Int = 0,
    val isHealthy: Boolean = true,
)

data class HomeDashboard(
    val greeting: String,
    val headline: String,
    val localTimeStr: String,
    val awayMinutes: Long? = null,
    val briefingDismissed: Boolean = false,
    val music: HomeMusicCardModel,
    val arena: HomeArenaCardModel,
    val system: HomeSystemCardModel,
    val eventsToWatch: List<BriefingEvent> = emptyList(),
    val newsRadar: List<BriefingNewsItem> = emptyList(),
    val actionBanner: String? = null,
)

class HomeViewModel(
    private val repository: HomeRepository,
) : ViewModel() {

    private val _state = MutableStateFlow<UiState<HomeDashboard>>(UiState.Loading)
    val state: StateFlow<UiState<HomeDashboard>> = _state.asStateFlow()

    private val _isRefreshing = MutableStateFlow(false)
    val isRefreshing: StateFlow<Boolean> = _isRefreshing.asStateFlow()

    init {
        refresh()
    }

    fun refresh(forceRegenerateBriefing: Boolean = false) {
        viewModelScope.launch {
            if (_state.value !is UiState.Ready) {
                _state.value = UiState.Loading
            }
            _isRefreshing.value = true
            _state.value = loadDashboard(forceRegenerateBriefing = forceRegenerateBriefing)
            _isRefreshing.value = false
        }
    }

    fun dismissBriefing() {
        viewModelScope.launch {
            when (repository.dismissBriefing()) {
                is ApiResult.Ok -> {
                    val current = (_state.value as? UiState.Ready)?.data
                    if (current != null) {
                        _state.value = UiState.Ready(
                            current.copy(
                                briefingDismissed = true,
                                eventsToWatch = emptyList(),
                                newsRadar = emptyList(),
                                actionBanner = "Briefing dismissed.",
                            ),
                        )
                    } else {
                        refresh()
                    }
                }
                is ApiResult.Err -> {
                    updateBanner("Could not dismiss briefing right now.")
                }
            }
        }
    }

    fun playMusic(uri: String) {
        if (uri.isBlank()) return
        viewModelScope.launch {
            when (val res = repository.playStagedMusic(uri)) {
                is ApiResult.Ok -> {
                    val current = (_state.value as? UiState.Ready)?.data
                    if (current != null) {
                        _state.value = UiState.Ready(
                            current.copy(
                                music = current.music.copy(confirmed = true),
                                actionBanner = if (res.data.ok) {
                                    "Playback requested on SAIL host."
                                } else {
                                    res.data.error ?: "Playback request sent."
                                },
                            ),
                        )
                    }
                }
                is ApiResult.Err -> {
                    updateBanner(res.message.ifBlank { "Unable to start playback." })
                }
            }
        }
    }

    private fun updateBanner(message: String) {
        val current = (_state.value as? UiState.Ready)?.data ?: return
        _state.value = UiState.Ready(current.copy(actionBanner = message))
    }

    suspend fun loadDashboard(forceRegenerateBriefing: Boolean = false): UiState<HomeDashboard> =
        coroutineScope {
            val briefingDeferred = async {
                if (forceRegenerateBriefing) {
                    repository.refreshBriefing()
                } else {
                    repository.fetchBriefing()
                }
            }
            val spotifyDeferred = async { repository.fetchSpotifyStatus() }
            val coursesDeferred = async { repository.fetchCourses() }
            val metricsDeferred = async { repository.fetchSystemMetrics() }
            val healthDeferred = async { repository.fetchHealth() }
            val mindDeferred = async { repository.fetchMindStatus() }

            val briefingRes = briefingDeferred.await()
            val spotifyRes = spotifyDeferred.await()
            val coursesRes = coursesDeferred.await()
            val metricsRes = metricsDeferred.await()
            val healthRes = healthDeferred.await()
            val mindRes = mindDeferred.await()

            val allResults = listOf(
                briefingRes,
                spotifyRes,
                coursesRes,
                metricsRes,
                healthRes,
                mindRes,
            )
            if (allResults.all { it is ApiResult.Err }) {
                val firstErr = allResults.filterIsInstance<ApiResult.Err>().firstOrNull()
                return@coroutineScope UiState.Unavailable(
                    firstErr?.message ?: "SAIL server is unreachable.",
                )
            }

            val briefing = briefingRes.getOrNull()
            val spotify = spotifyRes.getOrNull()
            val courses = coursesRes.getOrNull().orEmpty()
            val metrics = metricsRes.getOrNull()
            val health = healthRes.getOrNull()
            val mind = mindRes.getOrNull()

            val activeCourse = selectPrimaryCourse(courses)
            val resolvedNext = when {
                activeCourse == null -> null
                activeCourse.next != null -> activeCourse.next
                else -> repository.fetchLearnNext(activeCourse.id).getOrNull()
            }

            val musicModel = buildMusicCard(briefing = briefing, spotify = spotify)
            val arenaModel = buildArenaCard(
                briefing = briefing,
                course = activeCourse,
                next = resolvedNext,
                coursesAvailable = coursesRes is ApiResult.Ok,
            )
            val systemModel = buildSystemCard(
                briefing = briefing,
                metrics = metrics,
                healthOk = health != null,
                mind = mind,
            )

            val greetingText = briefing?.greeting
                ?.takeIf { it.isNotBlank() }
                ?.let { if (it.endsWith(".")) it else "$it." }
                ?: "Welcome back."

            val headlineText = briefing?.headline
                ?.takeIf { it.isNotBlank() }
                ?: "Here's your return briefing."

            UiState.Ready(
                HomeDashboard(
                    greeting = greetingText,
                    headline = headlineText,
                    localTimeStr = briefing?.localTimeStr.orEmpty(),
                    awayMinutes = briefing?.awayDurationMinutes,
                    briefingDismissed = briefing?.dismissed ?: false,
                    music = musicModel,
                    arena = arenaModel,
                    system = systemModel,
                    eventsToWatch = if (briefing?.dismissed == true) {
                        emptyList()
                    } else {
                        briefing?.eventsToWatch.orEmpty()
                    },
                    newsRadar = if (briefing?.dismissed == true) {
                        emptyList()
                    } else {
                        briefing?.newsRadar.orEmpty()
                    },
                ),
            )
        }

    companion object {
        internal fun selectPrimaryCourse(courses: List<HomeCourseSummary>): HomeCourseSummary? {
            if (courses.isEmpty()) return null
            val ready = courses.filter { it.status.equals("ready", ignoreCase = true) }
            val pool = ready.ifEmpty { courses }
            return pool.firstOrNull { (it.progress?.due ?: 0) > 0 }
                ?: pool.firstOrNull { (it.progress?.lessonsCompleted ?: 0) in 1 until it.lessonCount.coerceAtLeast(1) }
                ?: pool.firstOrNull { it.next?.lessonId != null }
                ?: pool.firstOrNull()
        }

        internal fun buildMusicCard(
            briefing: ReturnBriefing?,
            spotify: SpotifyStatusResponse?,
        ): HomeMusicCardModel {
            val live = spotify?.nowPlaying?.takeIf { it.name.isNotBlank() }
            if (live != null) {
                val subtitle = buildString {
                    if (live.by.isNotBlank()) append(live.by)
                    if (live.device.isNotBlank()) {
                        if (isNotEmpty()) append(" · ")
                        append(live.device)
                    }
                    if (isEmpty()) append(if (live.isPlaying) "Playing now on Spotify." else "Paused on Spotify.")
                }
                return HomeMusicCardModel(
                    title = live.name,
                    subtitle = subtitle,
                    badge = if (live.isPlaying) "LIVE" else "PAUSED",
                    playableUri = live.uri.takeIf { it.isNotBlank() },
                    isPlaying = live.isPlaying,
                )
            }

            val staged = briefing?.stagedMusic?.takeIf { it.name.isNotBlank() }
            if (staged != null) {
                val subtitle = buildString {
                    if (staged.by.isNotBlank()) append(staged.by)
                    if (staged.rationale.isNotBlank()) {
                        if (isNotEmpty()) append(" — ")
                        append(staged.rationale)
                    }
                    if (isEmpty()) append("Staged for your return.")
                }
                return HomeMusicCardModel(
                    title = staged.name,
                    subtitle = subtitle,
                    badge = staged.circadianSlot.takeIf { it.isNotBlank() }?.uppercase(),
                    playableUri = staged.uri.takeIf { it.isNotBlank() },
                    isPlaying = false,
                    confirmed = staged.confirmed,
                )
            }

            return HomeMusicCardModel(
                title = "No music staged",
                subtitle = "History is empty or unavailable.",
            )
        }

        internal fun buildArenaCard(
            briefing: ReturnBriefing?,
            course: HomeCourseSummary?,
            next: HomeLearnNext?,
            coursesAvailable: Boolean,
        ): HomeArenaCardModel {
            if (course != null) {
                val completed = course.progress?.lessonsCompleted ?: 0
                val total = course.lessonCount
                val due = course.progress?.due ?: 0
                val reason = next?.reasons?.firstOrNull()?.takeIf { it.isNotBlank() }
                val subtitle = when {
                    due > 0 -> "$due review${if (due == 1) "" else "s"} due · $completed/$total lessons"
                    reason != null -> reason
                    total > 0 -> "$completed of $total lessons · ${course.minutes} min"
                    course.summary.isNotBlank() -> course.summary
                    else -> "Resume in Arena."
                }
                return HomeArenaCardModel(
                    title = course.title,
                    subtitle = subtitle,
                    courseId = course.id,
                    nextLessonId = next?.lessonId,
                    nextKind = next?.kind?.takeIf { it.isNotBlank() },
                    lessonsCompleted = completed,
                    lessonCount = total,
                    dueCount = due,
                )
            }

            val plan = briefing?.learningPlan?.takeIf { it.topic.isNotBlank() }
            if (plan != null) {
                val subtitle = when {
                    plan.rationale.isNotBlank() -> plan.rationale
                    plan.estimatedMinutes > 0 -> "${plan.estimatedMinutes} min session staged"
                    else -> "Staged in Arena."
                }
                return HomeArenaCardModel(
                    title = plan.topic,
                    subtitle = subtitle,
                )
            }

            return HomeArenaCardModel(
                title = "Resume in Arena",
                subtitle = if (coursesAvailable) {
                    "No courses started yet."
                } else {
                    "Courses unavailable right now."
                },
            )
        }

        internal fun buildSystemCard(
            briefing: ReturnBriefing?,
            metrics: SystemMetrics?,
            healthOk: Boolean,
            mind: HomeMindStatus?,
        ): HomeSystemCardModel {
            val continuity = briefing?.projectContinuity
            val sentinel = briefing?.systemSentinel
            val workspace = continuity?.activeWorkspace?.takeIf { it.isNotBlank() } ?: "SAIL"

            val storageLabel = HomeFormatting.formatFreeStorage(metrics)
                ?: sentinel?.storageStatus?.takeIf { it.isNotBlank() }
                ?: "Unavailable"

            val memoryLabel = HomeFormatting.formatMemoryUsage(metrics)
                ?: sentinel?.memoryHeadroom?.takeIf { it.isNotBlank() }
                ?: "Unavailable"

            val cpuLabel = HomeFormatting.formatCpuPercent(metrics?.cpuPercent)

            val mindHealthy = mind?.acceptingRequests ?: true
            val isHealthy = healthOk && mindHealthy
            val statusSummary = when {
                sentinel != null &&
                    sentinel.hostHealth.isNotBlank() &&
                    !sentinel.hostHealth.contains("unavailable", ignoreCase = true) -> sentinel.hostHealth
                healthOk && mind != null && mind.acceptingRequests -> "All core systems nominal."
                healthOk -> "Host online · Core services responding."
                metrics != null -> "Host telemetry active."
                else -> "System status unavailable."
            }

            return HomeSystemCardModel(
                workspaceName = workspace,
                statusSummary = statusSummary,
                storageLabel = storageLabel,
                memoryLabel = memoryLabel,
                cpuLabel = cpuLabel,
                forgeSummary = continuity?.forgeSummary?.takeIf { it.isNotBlank() },
                lastEditedFile = continuity?.lastEditedFile?.takeIf { it.isNotBlank() },
                pendingApprovals = sentinel?.pendingGuardianApprovals ?: 0,
                isHealthy = isHealthy,
            )
        }
    }
}
