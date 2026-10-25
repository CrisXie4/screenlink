package dev.screenlink.companion.share

import android.app.Application
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.IntentCompat
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import dev.screenlink.companion.R
import dev.screenlink.companion.data.AppDatabase
import dev.screenlink.companion.data.TransferRecord
import dev.screenlink.companion.data.TransferRepository
import dev.screenlink.companion.net.AgentServer
import dev.screenlink.companion.net.AgentState
import dev.screenlink.companion.transfer.ProgressEvent
import dev.screenlink.companion.transfer.ProgressHub
import dev.screenlink.companion.transfer.TransferEngine
import dev.screenlink.companion.ui.theme.ScreenLinkTheme
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.onEach
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/**
 * C6：系统分享「发送到电脑」目标 Activity（DESIGN.md §11，manifest 已声明 SEND/SEND_MULTIPLE、
 * 透明对话框主题、excludeFromRecents）。
 *
 *  1. onCreate 先 `TransferEngine.init(applicationContext)`（幂等）——覆盖「新装 App 未启动过
 *     服务、直接系统分享」路径（§11.2），随后 `submitShares`；
 *  2. **每 intent 只提交一次**：解析结果拼接串作去重键存 ViewModel（§11.3）。旋转/分屏重建时
 *     onCreate 重跑但键相同 → 只恢复进度展示，绝不重复提交；
 *  3. 进度对话框：逐文件状态取 Room 行（每次 updateProgress 都触发 observeRecent 重发），
 *     另订阅 ProgressHub 作「最近活动」行。**finish 判定锚定 Room 行全部到达终态**
 *     ——事件流 lossy（DROP_OLDEST 缓冲 64），分享大量文件时 Finished 事件必被丢弃，
 *     按事件计数等待会永久挂起（评审遗留）；
 *  4. 全部结束后停留数秒再 finish，**不提前 finish**（§11.5：分享 URI 读授权随 Activity
 *     销毁失效，必须等引擎完成内容读取/落盘）。
 */
class SendToPcActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // §11.2：submitShares 之前必须 init（幂等，未初始化会被契约层快速失败）
        TransferEngine.init(applicationContext)
        val vm = ViewModelProvider(this)[SendToVm::class.java]
        vm.submitOnce(extractSharedUris(intent))
        setContent {
            ScreenLinkTheme {
                SendToDialog(vm = vm, onFinish = { finish() })
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        // 本 Activity 未声明 singleTop 等 launchMode，正常运行不会进入；
        // 防御性处理：新 intent 内容不同会因去重键不同而触发重新提交
        ViewModelProvider(this)[SendToVm::class.java].submitOnce(extractSharedUris(intent))
    }

    /** 从 SEND / SEND_MULTIPLE intent 提取分享流 uri 列表（EXTRA_TEXT 不在本期范围）。 */
    private fun extractSharedUris(intent: Intent?): List<Uri> = when (intent?.action) {
        Intent.ACTION_SEND ->
            listOfNotNull(
                IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java),
            )

        Intent.ACTION_SEND_MULTIPLE ->
            IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)
                .orEmpty()

        else -> emptyList()
    }
}

// ---------------------------------------------------------------- ViewModel

/** 单个分享文件行（Room 行投影）。 */
data class ShareFileRow(
    val id: Long,
    val name: String,
    val status: Int,
    val transferred: Long,
    val total: Long,
)

sealed interface SharePhase {
    /** submitShares 未返回。 */
    data object Submitting : SharePhase

    /** 已提交，等待各记录终态。needPairing=提交时引擎处于「已监听未配对」。 */
    data class Active(val needPairing: Boolean) : SharePhase

    /** 全部记录终态。failed 含 CANCELLED。 */
    data class Done(val ok: Int, val queued: Int, val failed: Int, val needPairing: Boolean) : SharePhase

    /** 提交异常 / 等待超时。 */
    data class Fatal(val message: String) : SharePhase

    /** intent 无 EXTRA_STREAM。 */
    data object Empty : SharePhase
}

data class ShareUiState(
    val phase: SharePhase = SharePhase.Submitting,
    val files: List<ShareFileRow> = emptyList(),
    val activityLine: String? = null,
)

/**
 * 分享提交域 ViewModel（Activity 存续期，旋转重建不丢）：
 * 承载「每 intent 只提交一次」的去重键与提交任务（DESIGN.md §11.3）。
 * 注意保持 public：ViewModelProvider 默认工厂经反射实例化。
 */
class SendToVm(application: Application) : AndroidViewModel(application) {

    private val _ui = MutableStateFlow(ShareUiState())
    val ui: StateFlow<ShareUiState> = _ui

    private val lock = Any()
    private var submittedKey: String? = null
    private var watchJob: Job? = null
    private var hubJob: Job? = null

    /**
     * 同一 uri 列表只提交一次；不同内容（onNewIntent 场景）取消旧等待、重新提交。
     */
    fun submitOnce(uris: List<Uri>) {
        val key = uris.joinToString(separator = "\n") { it.toString() }
        synchronized(lock) {
            if (key == submittedKey) return
            submittedKey = key
        }
        watchJob?.cancel()
        hubJob?.cancel()
        if (uris.isEmpty()) {
            _ui.value = ShareUiState(SharePhase.Empty)
            return
        }
        _ui.value = ShareUiState(SharePhase.Submitting)
        watchJob = viewModelScope.launch {
            try {
                val result = TransferEngine.submitShares(uris)
                // 注：SubmitResult 在 A8 实现中嵌套于 TransferEngine（DESIGN.md §4 原文为顶层，
                // 属契约漂移，随 A 现码为准）
                val needPairing = result is TransferEngine.SubmitResult.NeedPairing
                _ui.update { it.copy(phase = SharePhase.Active(needPairing)) }
                // recordIds 定义在三个子类而非接口上（A8 现码），按 sealed 分支收窄取出
                val recordIds = when (result) {
                    is TransferEngine.SubmitResult.Queued -> result.recordIds
                    is TransferEngine.SubmitResult.Sent -> result.recordIds
                    is TransferEngine.SubmitResult.NeedPairing -> result.recordIds
                }
                watchUntilTerminal(recordIds.toSet(), needPairing)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                _ui.update { it.copy(phase = SharePhase.Fatal(e.message ?: e.javaClass.simpleName)) }
            }
        }
        // ProgressHub：仅「最近活动」展示；事件 lossy，不可用于终态判定（评审遗留规避）
        hubJob = viewModelScope.launch {
            val names = HashMap<Long, String>()
            ProgressHub.events.collect { ev ->
                val line = when (ev) {
                    is ProgressEvent.Started -> {
                        names[ev.recordId] = ev.fileName
                        "开始传输 ${ev.fileName}"
                    }

                    is ProgressEvent.Progress -> {
                        val name = names[ev.recordId] ?: "文件"
                        if (ev.total > 0) "$name ${(ev.transferred * 100 / ev.total)}%" else name
                    }

                    is ProgressEvent.Finished -> {
                        val name = names[ev.recordId] ?: "文件"
                        if (ev.ok) "$name 完成" else "$name 失败"
                    }
                }
                _ui.update { it.copy(activityLine = line) }
            }
        }
    }

    /**
     * 锚定 Room：订阅 observeRecent，发布逐文件行，直到本批 id 全部终态
     * （DONE/FAILED/CANCELLED/QUEUED——QUEUED 对本次会话即终点：内容已入 outbox，稍后自动发）。
     * 总超时兜底：引擎异常卡死时不至于把分享者永久扣在本页。
     */
    private suspend fun watchUntilTerminal(ids: Set<Long>, needPairing: Boolean) {
        val app = getApplication<Application>()
        val repo = TransferRepository(AppDatabase.get(app).transferDao(), app)
        if (ids.isEmpty()) {
            // 引擎未产生记录（无可发送内容）：按空完成收尾
            _ui.update { it.copy(phase = SharePhase.Done(0, 0, 0, needPairing)) }
            return
        }
        val finalRows = withTimeoutOrNull(MAX_WAIT_MS) {
            repo.observeRecent(RECENT_LIMIT)
                .onEach { rows -> publishRows(ids, rows) }
                .first { rows ->
                    val mine = rows.filter { it.id in ids }
                    mine.size == ids.size && mine.all { it.status in TERMINAL_STATUSES }
                }
        }
        if (finalRows == null) {
            _ui.update {
                it.copy(phase = SharePhase.Fatal(app.getString(R.string.sl_ui_share_timeout)))
            }
            return
        }
        val mine = finalRows.filter { it.id in ids }
        _ui.update {
            it.copy(
                phase = SharePhase.Done(
                    ok = mine.count { r -> r.status == TransferRepository.STATUS_DONE },
                    queued = mine.count { r -> r.status == TransferRepository.STATUS_QUEUED },
                    failed = mine.count { r ->
                        r.status == TransferRepository.STATUS_FAILED ||
                            r.status == TransferRepository.STATUS_CANCELLED
                    },
                    needPairing = needPairing,
                ),
            )
        }
    }

    private fun publishRows(ids: Set<Long>, rows: List<TransferRecord>) {
        val mine = rows.filter { it.id in ids }
            .sortedBy { it.id }
            .map {
                ShareFileRow(
                    id = it.id,
                    name = it.fileName,
                    status = it.status,
                    transferred = it.transferredBytes,
                    total = it.sizeBytes,
                )
            }
        _ui.update { it.copy(files = mine) }
    }

    companion object {
        /** 引擎卡死兜底（大文件多文件排队上传可能耗时很久，取宽松值）。 */
        private const val MAX_WAIT_MS = 30 * 60_000L

        private const val RECENT_LIMIT = 500

        /** QUEUED 对本次会话视为终点（§11：内容已复制进 outbox，PC 连接后经 LIST/RECV 拉走）。 */
        private val TERMINAL_STATUSES = setOf(
            TransferRepository.STATUS_DONE,
            TransferRepository.STATUS_FAILED,
            TransferRepository.STATUS_CANCELLED,
            TransferRepository.STATUS_QUEUED,
        )
    }
}

// ---------------------------------------------------------------- 对话框 UI

@Composable
private fun SendToDialog(vm: SendToVm, onFinish: () -> Unit) {
    val ui by vm.ui.collectAsState()
    val agentState by AgentServer.state.collectAsState()

    // 终态停留片刻自动收尾；needPairing 时多留几秒让用户看到配对码
    LaunchedEffect(ui.phase) {
        when (val p = ui.phase) {
            is SharePhase.Done -> {
                delay(if (p.needPairing) 8_000L else 2_000L)
                onFinish()
            }

            is SharePhase.Fatal -> {
                delay(6_000L)
                onFinish()
            }

            is SharePhase.Empty -> {
                delay(2_000L)
                onFinish()
            }

            else -> {}
        }
    }

    // 浮窗主题（windowIsFloating）下窗口按内容包裹：用 widthIn 撑起稳定的对话框宽度
    Surface(
        shape = MaterialTheme.shapes.large,
        color = MaterialTheme.colorScheme.surface,
        tonalElevation = 6.dp,
        modifier = Modifier
            .widthIn(min = 280.dp, max = 440.dp)
            .padding(24.dp),
    ) {
        Column(
            modifier = Modifier.padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(
                text = stringResource(R.string.sl_ui_share_title),
                style = MaterialTheme.typography.titleLarge,
            )

            when (val p = ui.phase) {
                is SharePhase.Submitting -> Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    CircularProgressIndicator(modifier = Modifier.padding(2.dp), strokeWidth = 2.dp)
                    Text(text = stringResource(R.string.sl_ui_share_submitting))
                }

                is SharePhase.Empty ->
                    Text(text = stringResource(R.string.sl_ui_share_none))

                is SharePhase.Fatal ->
                    Text(
                        text = stringResource(R.string.sl_ui_share_error, p.message),
                        color = MaterialTheme.colorScheme.error,
                    )

                is SharePhase.Active -> {
                    if (p.needPairing) {
                        PairingHint(agentState)
                    }
                    FileRows(ui.files)
                    ui.activityLine?.let { ActivityLine(it) }
                }

                is SharePhase.Done -> {
                    if (p.needPairing) {
                        PairingHint(agentState)
                    }
                    if (p.queued > 0) {
                        Text(
                            text = stringResource(R.string.sl_ui_share_queued_hint),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Text(
                        text = stringResource(R.string.sl_ui_share_summary, p.ok, p.queued, p.failed),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    Text(
                        text = stringResource(R.string.sl_ui_share_all_done),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.primary,
                    )
                    FileRows(ui.files)
                }
            }
        }
    }
}

@Composable
private fun PairingHint(agentState: AgentState) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(
            text = stringResource(R.string.sl_ui_share_pairing_hint),
            style = MaterialTheme.typography.bodyMedium,
        )
        val code = (agentState as? AgentState.AwaitingPairing)?.code
        if (code != null) {
            Text(
                text = code,
                fontSize = 36.sp,
                fontWeight = FontWeight.Bold,
                letterSpacing = 6.sp,
                textAlign = TextAlign.Center,
                modifier = Modifier.padding(top = 2.dp),
            )
        }
    }
}

@Composable
private fun FileRows(files: List<ShareFileRow>) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        files.forEach { file ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = file.name,
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    text = fileStatusText(file),
                    style = MaterialTheme.typography.labelSmall,
                    color = if (file.status == TransferRepository.STATUS_FAILED ||
                        file.status == TransferRepository.STATUS_CANCELLED
                    ) {
                        MaterialTheme.colorScheme.error
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                    modifier = Modifier.padding(start = 8.dp),
                )
            }
        }
    }
}

@Composable
private fun ActivityLine(line: String) {
    Text(
        text = line,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun fileStatusText(file: ShareFileRow): String = when (file.status) {
    TransferRepository.STATUS_RUNNING ->
        stringResource(R.string.sl_ui_history_status_running) +
            if (file.total > 0) " ${(file.transferred * 100 / file.total)}%" else ""

    TransferRepository.STATUS_DONE -> stringResource(R.string.sl_ui_history_status_done)
    TransferRepository.STATUS_FAILED -> stringResource(R.string.sl_ui_history_status_failed)
    TransferRepository.STATUS_CANCELLED -> stringResource(R.string.sl_ui_history_status_cancelled)
    TransferRepository.STATUS_QUEUED -> stringResource(R.string.sl_ui_history_status_queued)
    else -> "#${file.status}"
}
