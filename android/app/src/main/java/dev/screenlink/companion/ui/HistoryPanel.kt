package dev.screenlink.companion.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.screenlink.companion.R
import dev.screenlink.companion.data.TransferRecord
import dev.screenlink.companion.data.TransferRepository
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/**
 * C3：传输历史（DESIGN.md §4 C3）。
 * 收集 MainViewModel.history（B4 TransferRepository.observeRecent 的 Room Flow），
 * 任意写入（进度/终态）都会重发；「清空」仅删历史行，不动用户目录文件（DESIGN.md §8.8）。
 */
@Composable
fun HistoryPanel(vm: MainViewModel) {
    val history by vm.history.collectAsState(initial = emptyList())
    var confirmClear by remember { mutableStateOf(false) }

    Column(modifier = Modifier.fillMaxSize().padding(16.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = stringResource(R.string.sl_ui_history_title),
                style = MaterialTheme.typography.titleLarge,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = { confirmClear = true }) {
                Text(text = stringResource(R.string.sl_ui_history_clear))
            }
        }

        if (history.isEmpty()) {
            Spacer(modifier = Modifier.weight(1f))
            Text(
                text = stringResource(R.string.sl_ui_history_empty),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(modifier = Modifier.weight(1f))
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                items(history, key = { it.id }) { record ->
                    HistoryRow(record)
                }
            }
        }
    }

    if (confirmClear) {
        AlertDialog(
            onDismissRequest = { confirmClear = false },
            title = { Text(text = stringResource(R.string.sl_ui_history_clear_title)) },
            text = { Text(text = stringResource(R.string.sl_ui_history_clear_message)) },
            confirmButton = {
                TextButton(onClick = {
                    confirmClear = false
                    vm.clearHistory()
                }) { Text(text = stringResource(R.string.sl_ui_history_ok)) }
            },
            dismissButton = {
                TextButton(onClick = { confirmClear = false }) {
                    Text(text = stringResource(R.string.sl_ui_history_cancel))
                }
            },
        )
    }
}

@Composable
private fun HistoryRow(record: TransferRecord) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            text = stringResource(
                if (record.direction == TransferRepository.DIR_SEND) {
                    R.string.sl_ui_history_dir_send
                } else {
                    R.string.sl_ui_history_dir_recv
                },
            ),
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.primary,
        )
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = record.fileName,
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            val time = remember(record.startedAt) { formatTime(record.startedAt) }
            Text(
                text = "${humanBytes(record.sizeBytes)} · $time · ${record.peerName}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Text(
            text = statusLabel(record),
            style = MaterialTheme.typography.labelMedium,
            color = when (record.status) {
                TransferRepository.STATUS_FAILED -> MaterialTheme.colorScheme.error
                TransferRepository.STATUS_DONE -> MaterialTheme.colorScheme.primary
                else -> MaterialTheme.colorScheme.onSurfaceVariant
            },
        )
    }
}

@Composable
private fun statusLabel(record: TransferRecord): String {
    val base = when (record.status) {
        TransferRepository.STATUS_RUNNING -> stringResource(R.string.sl_ui_history_status_running)
        TransferRepository.STATUS_DONE -> stringResource(R.string.sl_ui_history_status_done)
        TransferRepository.STATUS_FAILED -> stringResource(R.string.sl_ui_history_status_failed)
        TransferRepository.STATUS_CANCELLED -> stringResource(R.string.sl_ui_history_status_cancelled)
        TransferRepository.STATUS_QUEUED -> stringResource(R.string.sl_ui_history_status_queued)
        else -> "#${record.status}"
    }
    return if (record.status == TransferRepository.STATUS_DONE && record.md5 != null) {
        base + stringResource(R.string.sl_ui_history_md5_ok)
    } else {
        base
    }
}

private val TIME_FORMAT = DateTimeFormatter.ofPattern("MM-dd HH:mm")

private fun formatTime(epochMs: Long): String =
    runCatching {
        TIME_FORMAT.format(Instant.ofEpochMilli(epochMs).atZone(ZoneId.systemDefault()))
    }.getOrDefault("-")

/** 人类可读大小（历史行与分享对话框共用，包内 internal）。 */
internal fun humanBytes(bytes: Long): String {
    if (bytes < 0) return "-"
    val units = arrayOf("B", "KB", "MB", "GB", "TB")
    var value = bytes.toDouble()
    var unit = 0
    while (value >= 1024.0 && unit < units.lastIndex) {
        value /= 1024.0
        unit++
    }
    return if (unit == 0) "${bytes}B" else String.format("%.1f%s", value, units[unit])
}
