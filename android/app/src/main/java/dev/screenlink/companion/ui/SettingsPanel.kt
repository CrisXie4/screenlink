package dev.screenlink.companion.ui

import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.screenlink.companion.R
import dev.screenlink.companion.shizuku.ScrcpyLauncher
import dev.screenlink.companion.shizuku.ScrcpyState
import dev.screenlink.companion.shizuku.ShizukuGate
import dev.screenlink.companion.shizuku.ShizukuStatus
import dev.screenlink.companion.shizuku.WirelessDebugEnabler
import dev.screenlink.companion.shizuku.WirelessEnableResult
import kotlinx.coroutines.launch

/**
 * C4：设置页（DESIGN.md §4 C4 / §8 / §10）。
 *  - 保存目录：`ActivityResultContracts.OpenDocumentTree` → B5 `SafStore.save`；
 *    无有效目录时提示重新授权（PC 发来的文件无处写入）。
 *  - Shizuku 卡片：安装检测（C7）→ 授权 → 无线调试（C9）→ scrcpy shell 直启（C10）。
 *    **运行时可选**：未安装 / 未授权时仅展示引导文案并降级，所有提权操作隐藏，
 *    文件传输主链路不依赖 Shizuku。
 */
@Composable
fun SettingsPanel(vm: MainViewModel) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    // ---- 状态源 ----
    val safUri by vm.safTree.collectAsState()
    val shizukuStatus by ShizukuGate.status.collectAsState()
    val scrcpyState by ScrcpyLauncher.state.collectAsState()

    // C9 两类服务的持续发现（配对服务只在系统配对对话框存续期间广播）
    val pairingDiscovery = remember {
        WirelessDebugEnabler.discover(context, WirelessDebugEnabler.PAIRING_SERVICE_TYPE)
    }
    val connectDiscovery = remember {
        WirelessDebugEnabler.discover(context, WirelessDebugEnabler.CONNECT_SERVICE_TYPE)
    }
    val pairingServices by pairingDiscovery.services.collectAsState()
    val pairingError by pairingDiscovery.error.collectAsState()
    val connectServices by connectDiscovery.services.collectAsState()
    val connectError by connectDiscovery.error.collectAsState()
    val localIp = remember { WirelessDebugEnabler.localIpv4() }

    // 离开页面：注销 Shizuku 监听、停止 mDNS 发现
    DisposableEffect(Unit) {
        ShizukuGate.attach()
        onDispose {
            ShizukuGate.detach()
            pairingDiscovery.stop()
            connectDiscovery.stop()
        }
    }

    var enablingWireless by remember { mutableStateOf(false) }
    var wirelessResult by remember { mutableStateOf<WirelessEnableResult?>(null) }

    val treeLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocumentTree(),
    ) { uri: Uri? -> vm.onTreePicked(uri) }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(text = stringResource(R.string.sl_ui_settings_title), style = MaterialTheme.typography.titleLarge)

        // ---- 保存目录 ----
        Card(modifier = Modifier.fillMaxWidth()) {
            Column(
                modifier = Modifier.padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    text = stringResource(R.string.sl_ui_settings_storage_title),
                    style = MaterialTheme.typography.titleMedium,
                )
                val current = safUri
                Text(
                    text = if (current == null) {
                        stringResource(R.string.sl_ui_settings_storage_none)
                    } else {
                        val name = remember(current) { treeDisplayName(context, current) }
                        stringResource(R.string.sl_ui_settings_storage_current, name)
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedButton(onClick = { treeLauncher.launch(null) }) {
                    Text(text = stringResource(R.string.sl_ui_settings_storage_pick))
                }
            }
        }

        // ---- Shizuku 卡片 ----
        Card(modifier = Modifier.fillMaxWidth()) {
            Column(
                modifier = Modifier.padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    text = stringResource(R.string.sl_ui_shizuku_title),
                    style = MaterialTheme.typography.titleMedium,
                )
                when (shizukuStatus) {
                    is ShizukuStatus.Unavailable -> {
                        Text(
                            text = stringResource(R.string.sl_ui_shizuku_unavailable),
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        Text(
                            text = stringResource(R.string.sl_ui_shizuku_unavailable_hint),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        TextButton(onClick = { ShizukuGate.refresh() }) {
                            Text(text = stringResource(R.string.sl_ui_shizuku_refresh))
                        }
                    }

                    is ShizukuStatus.Unauthorized -> {
                        Text(
                            text = stringResource(R.string.sl_ui_shizuku_unauthorized),
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        Button(onClick = { ShizukuGate.requestPermission() }) {
                            Text(text = stringResource(R.string.sl_ui_shizuku_grant))
                        }
                    }

                    is ShizukuStatus.Authorized -> {
                        Text(
                            text = stringResource(R.string.sl_ui_shizuku_authorized),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.primary,
                        )

                        // -- 无线调试（C9） --
                        Text(
                            text = stringResource(R.string.sl_ui_shizuku_wireless_title),
                            style = MaterialTheme.typography.titleSmall,
                        )
                        Row(
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Button(
                                onClick = {
                                    scope.launch {
                                        enablingWireless = true
                                        wirelessResult = WirelessDebugEnabler.enable()
                                        enablingWireless = false
                                    }
                                },
                                enabled = !enablingWireless,
                            ) {
                                Text(
                                    text = stringResource(
                                        if (enablingWireless) {
                                            R.string.sl_ui_shizuku_wireless_enabling
                                        } else {
                                            R.string.sl_ui_shizuku_wireless_enable
                                        },
                                    ),
                                )
                            }
                        }
                        when (val r = wirelessResult) {
                            is WirelessEnableResult.Enabled -> Text(
                                text = stringResource(
                                    R.string.sl_ui_shizuku_wireless_enabled,
                                    r.ip ?: stringResource(R.string.sl_ui_status_ip_unknown),
                                ),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.primary,
                            )

                            is WirelessEnableResult.Failed -> Text(
                                text = stringResource(R.string.sl_ui_shizuku_wireless_failed, r.reason),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.error,
                            )

                            null -> {}
                        }

                        Text(
                            text = stringResource(R.string.sl_ui_shizuku_pairing_hint),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            OutlinedButton(onClick = { pairingDiscovery.start() }) {
                                Text(text = stringResource(R.string.sl_ui_shizuku_pairing_find))
                            }
                            OutlinedButton(onClick = { connectDiscovery.start() }) {
                                Text(text = stringResource(R.string.sl_ui_shizuku_connect_find))
                            }
                        }
                        pairingError?.let {
                            Text(
                                text = it,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.error,
                            )
                        }
                        val host = localIp ?: stringResource(R.string.sl_ui_status_ip_unknown)
                        val pairingSvc = pairingServices.firstOrNull()
                        if (pairingSvc == null) {
                            Text(
                                text = stringResource(R.string.sl_ui_shizuku_pairing_none),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        } else {
                            Text(
                                text = stringResource(
                                    R.string.sl_ui_shizuku_pairing_found,
                                    "$host:${pairingSvc.port}",
                                ),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.primary,
                            )
                        }
                        val connectSvc = connectServices.firstOrNull()
                        if (connectSvc == null) {
                            Text(
                                text = stringResource(R.string.sl_ui_shizuku_connect_none),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        } else {
                            Text(
                                text = stringResource(
                                    R.string.sl_ui_shizuku_connect_found,
                                    "$host:${connectSvc.port}",
                                ),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.primary,
                            )
                        }

                        // -- scrcpy（C10） --
                        Text(
                            text = stringResource(R.string.sl_ui_scrcpy_title),
                            style = MaterialTheme.typography.titleSmall,
                        )
                        when (val sc = scrcpyState) {
                            is ScrcpyState.Idle -> Button(onClick = {
                                scope.launch { ScrcpyLauncher.start(context) }
                            }) {
                                Text(text = stringResource(R.string.sl_ui_scrcpy_start))
                            }

                            is ScrcpyState.Pushing, is ScrcpyState.Starting -> Row(
                                verticalAlignment = Alignment.CenterVertically,
                                horizontalArrangement = Arrangement.spacedBy(8.dp),
                            ) {
                                CircularProgressIndicator(modifier = Modifier.size(16.dp), strokeWidth = 2.dp)
                                Text(
                                    text = stringResource(R.string.sl_ui_scrcpy_starting),
                                    style = MaterialTheme.typography.bodySmall,
                                )
                            }

                            is ScrcpyState.Running -> {
                                Text(
                                    text = stringResource(R.string.sl_ui_scrcpy_running),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.primary,
                                )
                                Text(
                                    text = stringResource(R.string.sl_ui_scrcpy_recipe_title),
                                    style = MaterialTheme.typography.labelMedium,
                                )
                                SelectionContainer {
                                    Text(
                                        text = ScrcpyLauncher.pcRecipe(
                                            ip = localIp,
                                            connectPort = connectServices.firstOrNull()?.port,
                                            pairingPort = pairingServices.firstOrNull()?.port,
                                        ),
                                        fontFamily = FontFamily.Monospace,
                                        fontSize = 12.sp,
                                        style = MaterialTheme.typography.bodySmall,
                                    )
                                }
                                OutlinedButton(onClick = { ScrcpyLauncher.stop() }) {
                                    Text(text = stringResource(R.string.sl_ui_scrcpy_stop))
                                }
                            }

                            is ScrcpyState.Failed -> {
                                Text(
                                    text = stringResource(R.string.sl_ui_scrcpy_failed, sc.reason),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.error,
                                )
                                Button(onClick = {
                                    scope.launch { ScrcpyLauncher.start(context) }
                                }) {
                                    Text(text = stringResource(R.string.sl_ui_scrcpy_start))
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/** tree uri 的展示名（query 树根 DISPLAY_NAME；失败回退 lastPathSegment）。 */
private fun treeDisplayName(context: Context, uri: Uri): String = runCatching {
    context.contentResolver.query(
        uri,
        arrayOf(DocumentsContract.Document.COLUMN_DISPLAY_NAME),
        null,
        null,
        null,
    )?.use { cursor ->
        if (cursor.moveToFirst()) cursor.getString(0) else null
    }
}.getOrNull() ?: uri.lastPathSegment ?: uri.toString()
