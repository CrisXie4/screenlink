package dev.screenlink.companion.ui

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import dev.screenlink.companion.R
import dev.screenlink.companion.net.AgentState
import dev.screenlink.companion.transfer.ProgressEvent
import kotlinx.coroutines.delay

/**
 * C2：监听状态卡（DESIGN.md §4 C2）。
 *  - 收集 MainViewModel.agentState（A5 StateFlow）渲染 IP:端口 / 配对码大字 + 过期倒计时；
 *  - 启停 AgentService（A6）；API 33+ 启动前请求 POST_NOTIFICATIONS（拒绝不阻塞，通知缺席降级）；
 *  - 订阅 ProgressHub（A11，lossy）显示「最近活动」行——仅展示，终态以历史页（Room）为准。
 */
@Composable
fun StatusPanel(vm: MainViewModel) {
    val state by vm.agentState.collectAsState()
    val context = LocalContext.current

    // 250ms 粒度时钟：配对码倒计时
    val now by produceState(0L) {
        while (true) {
            value = System.currentTimeMillis()
            delay(250)
        }
    }

    // 最近活动（lossy 事件流，仅供展示）
    val fileNames = remember { mutableStateMapOf<Long, String>() }
    var activityLine by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(Unit) {
        vm.transferEvents.collect { ev ->
            activityLine = when (ev) {
                is ProgressEvent.Started -> {
                    fileNames[ev.recordId] = ev.fileName
                    "开始传输 ${ev.fileName}"
                }
                is ProgressEvent.Progress -> {
                    val name = fileNames[ev.recordId] ?: "文件"
                    val percent = if (ev.total > 0) " ${(ev.transferred * 100 / ev.total)}%" else ""
                    "$name $percent"
                }
                is ProgressEvent.Finished -> {
                    val name = fileNames[ev.recordId] ?: "文件"
                    if (ev.ok) "$name 完成" else "$name 失败"
                }
            }
        }
    }

    // POST_NOTIFICATIONS：未授予则先弹系统授权，回调里继续启动（拒绝也照常启动，通知降级）
    var pendingStart by rememberSaveable { mutableStateOf(false) }
    val notifLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { _ ->
        if (pendingStart) {
            pendingStart = false
            vm.startAgent()
        }
    }
    fun startWithPermission() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            pendingStart = true
            notifLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
        } else {
            vm.startAgent()
        }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(text = stringResource(R.string.sl_ui_status_title), style = MaterialTheme.typography.titleLarge)

        Card(modifier = Modifier.fillMaxWidth()) {
            Column(
                modifier = Modifier.padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                when (val s = state) {
                    is AgentState.Idle -> {
                        Text(text = stringResource(R.string.sl_ui_status_idle), style = MaterialTheme.typography.bodyLarge)
                        StartButton(onStart = { startWithPermission() })
                    }

                    is AgentState.Listening -> {
                        val addr = s.ip ?: stringResource(R.string.sl_ui_status_ip_unknown)
                        Text(
                            text = stringResource(R.string.sl_ui_status_listening, "$addr:${s.port}"),
                            style = MaterialTheme.typography.bodyLarge,
                        )
                        StopButton(onStop = { vm.stopAgent() })
                    }

                    is AgentState.AwaitingPairing -> {
                        Text(
                            text = stringResource(R.string.sl_ui_status_pairing_hint),
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        // 配对码大字
                        Text(
                            text = s.code,
                            fontSize = 48.sp,
                            fontWeight = FontWeight.Bold,
                            letterSpacing = 8.sp,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        val remaining = ((s.expiresAtEpochMs - now).coerceAtLeast(0L) / 1000L).toInt()
                        Text(
                            text = stringResource(R.string.sl_ui_status_pairing_expires_in, remaining),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.fillMaxWidth(),
                            textAlign = TextAlign.Center,
                        )
                        StopButton(onStop = { vm.stopAgent() })
                    }

                    is AgentState.Paired -> {
                        val addr = s.ip ?: stringResource(R.string.sl_ui_status_ip_unknown)
                        Text(
                            text = stringResource(R.string.sl_ui_status_paired, s.peerName, addr),
                            style = MaterialTheme.typography.bodyLarge,
                        )
                        StopButton(onStop = { vm.stopAgent() })
                    }

                    is AgentState.Failed -> {
                        Text(
                            text = stringResource(R.string.sl_ui_status_failed, s.reason),
                            style = MaterialTheme.typography.bodyLarge,
                            color = MaterialTheme.colorScheme.error,
                        )
                        StartButton(onStart = { startWithPermission() })
                    }
                }
            }
        }

        Text(
            text = stringResource(R.string.sl_ui_status_notif_hint),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        activityLine?.let { line ->
            Text(
                text = stringResource(R.string.sl_ui_status_recent_activity, line),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun StartButton(onStart: () -> Unit) {
    Button(onClick = onStart, modifier = Modifier.fillMaxWidth()) {
        Text(text = stringResource(R.string.sl_ui_status_start))
    }
}

@Composable
private fun StopButton(onStop: () -> Unit) {
    OutlinedButton(onClick = onStop, modifier = Modifier.fillMaxWidth()) {
        Text(text = stringResource(R.string.sl_ui_status_stop))
    }
}
