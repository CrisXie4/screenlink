package dev.screenlink.companion.shizuku

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.concurrent.Volatile
import java.net.Inet4Address
import java.net.NetworkInterface

/** [WirelessDebugEnabler.enable] 的结果。 */
sealed interface WirelessEnableResult {
    /** 开关已置 1 并回读校验通过；[ip] 为本机局域网 IPv4（可能为 null，例如未连 Wi-Fi）。 */
    data class Enabled(val ip: String?) : WirelessEnableResult

    /** 开启或校验失败，[reason] 供 UI 展示。 */
    data class Failed(val reason: String) : WirelessEnableResult
}

/** mDNS 发现到的一条 adb 服务（配对或连接）。展示地址 = 本机 IP + 解析出的端口。 */
data class DiscoveredAdbService(
    val serviceName: String,
    val port: Int,
)

/**
 * C9：shell 开无线调试 + 两阶段 mDNS（DESIGN.md §10）。
 *
 * 1. 开启：`settings put global adb_wifi_enabled 1`（shell 身份具备 WRITE_SECURE_SETTINGS），
 *    回读 `settings get global adb_wifi_enabled` 校验；
 * 2. 配对（首次一次即可）：**只在系统「使用配对码配对设备」对话框存续期间**手机才广播
 *    `_adb-tls-pairing._tcp.`（含配对端口）——经 NsdManager 发现并解析端口，
 *    展示 `<ip>:<配对端口>`，引导 PC `adb pair` + 对话框上的 6 位码；
 * 3. 连接：发现 `_adb-tls-connect._tcp.` 取连接端口，展示 `<ip>:<port>` 供 PC `adb connect`。
 *
 * IP 一律取本机网卡（服务就广播在本机）；不依赖 resolve 结果里的 host——API 34+ 的
 * resolveService 不再回填 host 地址，端口不受影响。
 */
object WirelessDebugEnabler {

    const val SETTING_KEY = "adb_wifi_enabled"
    const val PAIRING_SERVICE_TYPE = "_adb-tls-pairing._tcp."
    const val CONNECT_SERVICE_TYPE = "_adb-tls-connect._tcp."

    /**
     * 开启无线调试并回读校验。要求 Shizuku 已授权（ShellRunner 抛 ShellException 时按失败降级返回）。
     */
    suspend fun enable(): WirelessEnableResult {
        return try {
            val put = ShellRunner.run("settings put global $SETTING_KEY 1")
            if (put.exitCode != 0) {
                return WirelessEnableResult.Failed(
                    "settings put 退出码 ${put.exitCode}：${(put.stderr.ifBlank { put.stdout }).take(200)}",
                )
            }
            val get = ShellRunner.run("settings get global $SETTING_KEY")
            if (get.exitCode == 0 && get.stdout.trim() == "1") {
                WirelessEnableResult.Enabled(localIpv4())
            } else {
                WirelessEnableResult.Failed("回读 $SETTING_KEY=\"${get.stdout.trim()}\"（期望 \"1\"，部分 ROM 需手动到设置页确认）")
            }
        } catch (e: ShellException) {
            WirelessEnableResult.Failed((e.message ?: "shell 执行失败").take(200))
        } catch (e: IllegalStateException) {
            WirelessEnableResult.Failed((e.message ?: "Shizuku 不可用").take(200))
        } catch (e: Exception) {
            WirelessEnableResult.Failed("${e.javaClass.simpleName}: ${e.message}".take(200))
        }
    }

    /** 本机局域网 IPv4（站点内地址优先），展示给 PC 连接用；无则 null。 */
    fun localIpv4(): String? = runCatching {
        NetworkInterface.getNetworkInterfaces().asSequence()
            .filter { it.isUp && !it.isLoopback }
            .flatMap { it.inetAddresses.asSequence() }
            .filterIsInstance<Inet4Address>()
            .firstOrNull { it.isSiteLocalAddress }
            ?.hostAddress
    }.getOrNull()

    /**
     * 创建一个持续型发现会话（未启动）；UI 在按钮点击时 [Discovery.start]，
     * 离开页面时 [Discovery.stop]（配对服务只在系统对话框存续期间广播，需持续监听而非一次性探测）。
     */
    fun discover(context: Context, serviceType: String): Discovery {
        val nsd = context.getSystemService(NsdManager::class.java)
            ?: throw IllegalStateException("NsdManager 不可用")
        return Discovery(nsd, serviceType)
    }

    class Discovery internal constructor(
        private val nsd: NsdManager,
        private val serviceType: String,
    ) {
        private val _services = MutableStateFlow<List<DiscoveredAdbService>>(emptyList())

        /** 已发现并解析出端口的服务列表（去重）。 */
        val services: StateFlow<List<DiscoveredAdbService>> = _services

        private val _error = MutableStateFlow<String?>(null)

        /** 发现启动失败等致命错误（服务丢失不算）。 */
        val error: StateFlow<String?> = _error

        @Volatile
        private var started = false

        @Volatile
        private var stopping = false

        private val discoveryListener = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(serviceType: String?) {}

            override fun onStartDiscoveryFailed(serviceType: String?, errorCode: Int) {
                _error.value = "mDNS 发现启动失败（code=$errorCode）"
            }

            override fun onServiceFound(serviceInfo: NsdServiceInfo?) {
                serviceInfo ?: return
                // 解析端口；系统对话框关/开会反复 found/lost，解析失败直接忽略该条
                runCatching {
                    nsd.resolveService(serviceInfo, object : NsdManager.ResolveListener {
                        override fun onResolveFailed(info: NsdServiceInfo?, errorCode: Int) {}

                        override fun onServiceResolved(info: NsdServiceInfo?) {
                            info ?: return
                            add(info)
                        }
                    })
                }
            }

            override fun onServiceLost(serviceInfo: NsdServiceInfo?) {
                serviceInfo ?: return
                val name = serviceInfo.serviceName ?: return
                _services.value = _services.value.filterNot { it.serviceName == name }
            }

            override fun onDiscoveryStopped(serviceType: String?) {}

            override fun onStopDiscoveryFailed(serviceType: String?, errorCode: Int) {}
        }

        private fun add(info: NsdServiceInfo) {
            val item = DiscoveredAdbService(
                serviceName = info.serviceName ?: "",
                port = info.port,
            )
            val current = _services.value
            if (current.none { it.serviceName == item.serviceName && it.port == item.port }) {
                _services.value = current + item
            }
        }

        /** 启动发现（幂等）。失败经 [error] 上报。 */
        fun start() {
            synchronized(this) {
                if (started || stopping) return
                started = true
            }
            runCatching {
                nsd.discoverServices(serviceType, NsdManager.PROTOCOL_DNS_SD, discoveryListener)
            }.onFailure {
                started = false
                _error.value = "discoverServices: ${it.message ?: it.javaClass.simpleName}"
            }
        }

        /** 停止发现（幂等；未完成启动时跳过底层调用）。 */
        fun stop() {
            synchronized(this) {
                if (!started || stopping) return
                stopping = true
            }
            runCatching { nsd.stopServiceDiscovery(discoveryListener) }
        }
    }

    /**
     * 一次性等待：[timeoutMs] 内发现任一服务即返回，否则 null。
     * 适合「按需探测」场景；对话框存续期的持续展示用 [discover]。
     */
    suspend fun awaitService(
        context: Context,
        serviceType: String,
        timeoutMs: Long = 20_000L,
    ): DiscoveredAdbService? {
        val d = discover(context, serviceType)
        try {
            d.start()
            return withTimeoutOrNull(timeoutMs) {
                d.services.first { it.isNotEmpty() }.firstOrNull()
            }
        } finally {
            d.stop()
        }
    }
}
