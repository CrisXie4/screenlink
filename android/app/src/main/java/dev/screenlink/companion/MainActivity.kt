package dev.screenlink.companion

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import dev.screenlink.companion.ui.RootScreen
import dev.screenlink.companion.ui.theme.ScreenLinkTheme

/**
 * 骨架：MainActivity（DESIGN.md §3.2）。
 * 契约：setContent { ScreenLinkTheme { RootScreen() } }；
 * RootScreen 由实现者 C 提供（骨架期为本目录 ui/RootScreen.kt 占位实现）。
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            ScreenLinkTheme {
                RootScreen()
            }
        }
    }
}
