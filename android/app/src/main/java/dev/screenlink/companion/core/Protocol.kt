package dev.screenlink.companion.core

/**
 * A1 占位（实现者 A 在此文件上补全）：协议常量唯一来源。
 * 帧格式与帧类型总表见 PROTOCOL.md §4；全部多字节整数大端（BE），字符串 UTF-8。
 */
object Protocol {
    /** 手机侧 TCP 监听端口（PROTOCOL.md §1） */
    const val PORT = 27188

    /** 帧头 magic，固定 "SLNK"（0x53 0x4C 0x4E 0x4B） */
    const val MAGIC = "SLNK"

    /** PC UDP 广播发现端口与探针（PROTOCOL.md §2；设备端 DiscoveryResponder 应答） */
    const val UDP_DISCOVER_PORT = 27186
    const val UDP_DISCOVER_MAGIC = "SCREENLINK_DISCOVER_V1"

    /** 协议版本（帧头 version 字节；与 mDNS TXT pv 同源，PROTOCOL.md §9） */
    const val VERSION = 1

    /** 帧头长度（字节）：magic(4) version(1) type(1) flags(2) length(4) */
    const val HEADER_SIZE = 12

    /** 任意帧 payload 硬上限 4 MiB（超出 → ERR PAYLOAD_TOO_LARGE 后 close） */
    const val MAX_PAYLOAD = 4 * 1024 * 1024

    /** DATA 帧数据部分上限 1 MiB（PROTOCOL.md §5.5） */
    const val MAX_DATA_CHUNK = 1 * 1024 * 1024

    /** 控制帧 payload 上限 64 KiB */
    const val MAX_CONTROL_PAYLOAD = 64 * 1024

    /** 单连接并发传输上限（xid 数，超限 SEND_ACK reason=BUSY） */
    const val MAX_CONCURRENT_TRANSFERS = 8

    /** 未认证并发连接上限（超过直接 close，§7） */
    const val MAX_UNAUTH_CONNECTIONS = 4

    /** 本端应用标识（HELLO.app；versionName 0.1.0 与 gradle 一致，AGP 8 默认无 BuildConfig） */
    const val APP_ID = "screenlink-android/0.1.0"

    /** HELLO 完成时限（连接后 5s） */
    const val HELLO_TIMEOUT_MS = 5_000L

    /** 认证完成时限：只约束 AUTH 帧在 HELLO 后 10s 内到达（配对流程挂起该计时，§3.2） */
    const val AUTH_TIMEOUT_MS = 10_000L

    /** 空闲 >30s 发 PING；75s 无任何入帧 → close */
    const val PING_AFTER_IDLE_MS = 30_000L
    const val IDLE_CLOSE_MS = 75_000L

    /** 配对码：6 位十进制数字；TTL 300s；单次有效；5 次失败作废 */
    const val PAIRING_CODE_LENGTH = 6
    const val PAIRING_CODE_TTL_MS = 300_000L
    const val PAIRING_MAX_FAILURES = 5

    /** 同 IP 冷却：AUTH+PAIR 合并计数，10min 滑动窗口累计 5 次 → 冷却 60s（§3.1） */
    const val IP_RATE_WINDOW_MS = 600_000L
    const val IP_RATE_MAX_FAILURES = 5
    const val IP_COOLDOWN_MS = 60_000L

    /** mDNS 服务类型与 TXT 键（PROTOCOL.md §2） */
    const val MDNS_SERVICE_TYPE = "_screenlink._tcp"
    const val TXT_PV = "pv"
    const val TXT_NM = "nm"
    const val TXT_ID = "id"
    const val TXT_PA = "pa"

    /** 帧类型总表（PROTOCOL.md §4） */
    const val T_HELLO = 0x01
    const val T_AUTH = 0x02
    const val T_AUTH_OK = 0x03
    const val T_NEED_PAIR = 0x04
    const val T_PAIR = 0x05
    const val T_PAIR_OK = 0x06
    const val T_PAIR_FAIL = 0x07
    const val T_LIST_REQ = 0x10
    const val T_LIST_RSP = 0x11
    const val T_RECV_REQ = 0x12
    const val T_SEND_REQ = 0x20
    const val T_SEND_ACK = 0x21
    const val T_DATA = 0x22
    const val T_PROGRESS = 0x23
    const val T_TRANSFER_DONE = 0x24
    const val T_CANCEL = 0x25
    const val T_PING = 0x30
    const val T_PONG = 0x31
    const val T_ERR = 0x40
    const val T_BYE = 0x41
}
