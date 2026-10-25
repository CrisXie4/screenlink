package dev.screenlink.companion.ui

import android.app.Application
import android.net.Uri
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.screenlink.companion.data.AppDatabase
import dev.screenlink.companion.data.TransferRecord
import dev.screenlink.companion.data.TransferRepository
import dev.screenlink.companion.net.AgentServer
import dev.screenlink.companion.net.AgentState
import dev.screenlink.companion.saf.SafStore
import dev.screenlink.companion.service.AgentService
import dev.screenlink.companion.transfer.ProgressEvent
import dev.screenlink.companion.transfer.ProgressHub
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

/**
 * C5：装配 B 的 TransferRepository + A 的 AgentServer.state / ProgressHub（DESIGN.md §4 C5），
 * 以 Contract 签名（A/B 对外契约）转发给三个 Panel；不引入 DI，手动构造依赖（DESIGN.md §3.1）。
 */
class MainViewModel(application: Application) : AndroidViewModel(application) {

    private val repository =
        TransferRepository(AppDatabase.get(application).transferDao(), application)

    private val safStore = SafStore(application)

    /** A5：服务状态（Idle/Listening/AwaitingPairing/Paired/Failed），StatusPanel 渲染。 */
    val agentState: StateFlow<AgentState> = AgentServer.state

    /** B4：最近 200 条历史（Room Flow，任意写入都会重发），HistoryPanel 收集。 */
    val history: Flow<List<TransferRecord>> = repository.observeRecent()

    /** A11：进度总线（lossy，DROP_OLDEST——仅展示用，不可作为状态依据），StatusPanel「最近活动」行。 */
    val transferEvents: SharedFlow<ProgressEvent> = ProgressHub.events

    private val _safTree = MutableStateFlow<Uri?>(null)

    /** B5：当前有效的 SAF tree uri；null = 未设置或授权已失效。 */
    val safTree: StateFlow<Uri?> = _safTree

    init {
        refreshSaf()
    }

    /** 校验持久化授权 + 探活；任何异常（含 B 未就绪）按「未设置」降级，不让设置页崩溃。 */
    fun refreshSaf() {
        viewModelScope.launch(Dispatchers.IO) {
            _safTree.value = runCatching { safStore.current() }.getOrNull()
        }
    }

    /** OpenDocumentTree 回调：保存授权并刷新状态（uri 为 null 表示用户取消，忽略）。 */
    fun onTreePicked(uri: Uri?) {
        if (uri == null) return
        viewModelScope.launch(Dispatchers.IO) {
            runCatching { safStore.save(uri) }
            _safTree.value = runCatching { safStore.current() }.getOrNull()
        }
    }

    /**
     * 启动前台服务（A6）。POST_NOTIFICATIONS 运行时请求由 StatusPanel 先行处理；
     * 未授予通知权限时服务仍可运行（进度通知缺席，属可接受降级）。
     */
    fun startAgent() {
        AgentService.start(getApplication())
    }

    /** 停止前台服务（A6）。 */
    fun stopAgent() {
        AgentService.stop(getApplication())
    }

    /** 清空历史（仅删行，不动用户目录文件，DESIGN.md §8.8）。 */
    fun clearHistory() {
        viewModelScope.launch {
            repository.clear()
        }
    }
}
