package dev.screenlink.companion.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.List
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Wifi
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.screenlink.companion.R

/**
 * C1：骨架契约入口（MainActivity `setContent { ScreenLinkTheme { RootScreen() } }` 的编译依赖）。
 * 契约：**无参** `@Composable fun RootScreen()`，签名不得变更（DESIGN.md §3.2/§12）。
 * 三段式底部导航：状态 / 历史 / 设置（DESIGN.md §4 C1）。
 */
private enum class Tab(val labelRes: Int, val icon: ImageVector) {
    Status(R.string.sl_ui_tab_status, Icons.Filled.Wifi),
    History(R.string.sl_ui_tab_history, Icons.Filled.List),
    Settings(R.string.sl_ui_tab_settings, Icons.Filled.Settings),
}

@Composable
fun RootScreen() {
    val vm: MainViewModel = viewModel()
    var selected by rememberSaveable { mutableIntStateOf(0) }
    val tabs = Tab.entries

    Scaffold(
        bottomBar = {
            NavigationBar {
                tabs.forEachIndexed { index, tab ->
                    NavigationBarItem(
                        selected = selected == index,
                        onClick = { selected = index },
                        icon = { Icon(imageVector = tab.icon, contentDescription = null) },
                        label = { Text(text = stringResource(tab.labelRes)) },
                    )
                }
            }
        },
    ) { innerPadding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding),
        ) {
            when (tabs[selected]) {
                Tab.Status -> StatusPanel(vm)
                Tab.History -> HistoryPanel(vm)
                Tab.Settings -> SettingsPanel(vm)
            }
        }
    }
}
