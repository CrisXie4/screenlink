# ScreenLink PC ↔ 手机 传输协议 v1

版本：v1.1（2026-10-25 评审修订：NOT_FOUND 关联规则、认证时限与配对共存、不可 seek 续传流程、token 绑定实例 id、BUSY 入 ERR 枚举、同 IP 冷却统一定义）· 协议版本号 `pv = 1`
本协议是 **PC 端 Rust 实现的对接依据**；Android 端实现见 [DESIGN.md](./DESIGN.md)。
文中"手机"指监听方（本 App），"PC"指连接方（Rust 实现）。

---

## 1. 角色、端口与连接模型

| 项 | 约定 |
|---|---|
| TCP 端口 | **27188**，手机侧 `ServerSocket` 监听（绑定通配地址） |
| 角色 | 手机 = TCP 服务端；PC = TCP 客户端 |
| 连接模型 | **单连接多路复用**：PC 维持一条长连接，控制帧与文件数据帧复用同一连接，靠 `xid` 区分并发传输（上限 8） |
| 会话数 | 同一时刻最多 **1 个已认证 PC 会话**；第 2 个认证请求 → `ERR{code:"BUSY"}` 并断开（`BUSY` 是 §5.6 ERR 枚举的合法值）；未认证并发连接 ≤ 4 |
| 接入过滤 | 手机只接受源地址属于私网/链路本地（10/8、172.16/12、192.168/16、169.254/16、fc00::/7、fe80::/10），其余直接关闭 |
| 字节序 | 全部多字节整数 **大端（BE）**；字符串 UTF-8；JSON 键 snake_case、紧凑输出（无空白） |
| 明文声明 | v1 无 TLS，流量（元数据+文件字节）对局域网窃听者可见；安全模型见 §10 |

## 2. 发现：mDNS

手机侧用 `NsdManager` 注册；PC 侧（Rust，建议 `mdns-sd` crate）浏览。

- 服务类型：**`_screenlink._tcp`**（浏览名为 `_screenlink._tcp.local`）
- 实例名：`ScreenLink-<id 后 4 位 hex>`（id 见 TXT；冲突由 mDNS 协议自动后缀，PC 不必处理）
- 端口：27188

**TXT 记录约定（v1 固定 4 条）：**

| 键 | 值 | 说明 |
|---|---|---|
| `pv` | `1` | 协议版本；`≠1` 时 PC 必须跳过该实例 |
| `nm` | 如 `Pixel 8` | 设备名（`Build.MODEL`），UTF-8 |
| `id` | 8 位小写 hex | 手机首次运行随机生成的持久实例 id |
| `pa` | `0` / `1` | `1` = 配对码当前有效（等待配对）；`0` = 已配对或未开码 |

手机停止服务时注销注册；PC 应在 5s 内对新增实例完成连接尝试。

## 3. 鉴权与配对

### 3.1 连接状态机

```
 TCP accept
    │  双方 5s 内各发 HELLO（先发者不限）
    ▼
 ┌────────┐  AUTH{token_id,token} 命中   ┌──────────┐
 │ 已认证  │◄──────────────────────────── │ 未认证    │
 └────────┘                              └────┬─────┘
     │  可发 LIST/SEND/RECV/…                  │ AUTH 未命中 → NEED_PAIR（手机展示配对码）
     ▼                                        ▼
  BYE → 双方互发 BYE 后 close            PAIR{code} 校验通过 → PAIR_OK{token,…}，转"已认证"
                                          校验失败 → PAIR_FAIL（累计 5 次作废该码）
```

- **未认证允许的帧**：`HELLO、AUTH、PAIR、PING、PONG、BYE、ERR`；其余一律 `ERR{code:"AUTH"}` 后 close。
- **配对码规则**：6 位十进制数字，CSPRNG 生成；TTL 300s；单次使用；连续 5 次校验失败立即作废。
- **同 IP 冷却（限速器统一定义，A 实现以此为准）**：手机侧按源 IP 统计 **AUTH 与 PAIR 校验失败**（两者合并计数），**10 分钟滑动窗口内累计 5 次 → 该 IP 冷却 60s**（冷却期间新连接直接 close，内存计数即可，重启清零）。
- **token 持久化**：手机只存 `token_id → SHA-256(token) hex` 映射（SharedPreferences，allowBackup 已关）；PC 必须自行持久化 token 明文。token 丢失只能重新配对。

### 3.2 AUTH / PAIR 流程帧

```
PC → HELLO  {"role":"pc","name":"DEV-PC","app":"screenlink-pc/0.1.0","pv":1}
手机 → HELLO {"role":"phone","name":"Pixel 8","app":"screenlink-android/0.1.0","pv":1}
PC → AUTH   {"token_id":"1a2b3c4d","token":"<43 字符 base64url>"}
   命中 → 手机 → AUTH_OK {"name":"Pixel 8"}
   未命中 → 手机 → NEED_PAIR {"device":"Pixel 8"}        // 同时手机 UI/通知展示配对码
          PC → PAIR  {"code":"042913"}
          成功 → 手机 → PAIR_OK {"token":"<43 字符>","token_id":"1a2b3c4d","name":"Pixel 8"}   // 本连接随即视为已认证
          失败 → 手机 → PAIR_FAIL {"attempts_left":3}    // attempts_left 为 0 时码作废，需重新触发 NEED_PAIR 流程
```

`role`/`pv` 不符：直接 `ERR{code:"PROTOCOL"}` 后 close。

**时限语义**：§7 的"认证完成时限 10s"只约束 AUTH 帧在 HELLO 后 10s 内到达；一旦手机发出 NEED_PAIR，该计时**挂起**，配对流程（人工录入 6 位码 → PAIR → PAIR_OK/PAIR_FAIL）整体由配对码 TTL 300s 与"5 次失败作废"约束，不适用 10s——否则按字面实现配对永不成功。

### 3.3 配对码 → token 派生（精确定义）

```
code  : 6 位十进制 ASCII（例 "042913"）
seed  : SecureRandom 生成 32 字节（每次配对重新生成，不出手机）
token = base64url_nopad( HKDF-SHA256(
            ikm  = seed,
            salt = ASCII("screenlink/v1/pair/" + code),
            info = ASCII("token"),
            L    = 32 ) )                    // 恒为 43 字符
token_id = lower_hex( SHA-256( ASCII(token) ) )[0..8]     // 取前 8 个 hex 字符
手机存储： token_id → SHA-256( ASCII(token) )（64 hex）
PC 存储：  token 明文 + token_id
```

HKDF 依 RFC 5869：`PRK = HMAC-SHA256(salt, ikm)`；`OKM = HMAC-SHA256(PRK, info ‖ 0x01)[0..32]`。
**设计理由（决策 D3）**：6 位码仅 20 位熵，直接派生 token 会被在线爆破；引入 32B CSPRNG 种子后 token 恒为 256 位熵，配对码只承担"一次性在线交换门槛"（限次+限时）。

## 4. 帧格式

```
偏移  长度  字段
0     4    magic      固定 "SLNK"（0x53 0x4C 0x4E 0x4B）
4     1    version    = 1
5     1    type       帧类型（下表）
6     2    flags      保留，v1 恒 0（bit0 预留给未来压缩标志）
8     4    length     payload 字节数，u32 BE；不含 12 字节帧头
12    ..   payload
```

- 帧头 12 字节 + payload；接收方先读 12 字节再读 length 字节。
- **硬上限**：任意帧 payload ≤ 4 MiB（超出 → `ERR{code:"PAYLOAD_TOO_LARGE"}` 后 close）；其中 DATA 帧有效负载 ≤ 1 MiB（§5.5），控制帧 ≤ 64 KiB。
- magic/version 不符 → `ERR{code:"PROTOCOL"}` 后 close。
- **未知帧类型且 version==1 → 读掉 payload 并忽略**（向前兼容预留）；version ≠ 1 → 直接 close。
- 完整性由长度前缀 + TCP 校验和保证，帧内不再另设校验。

**HELLO 帧完整十六进制示例**（payload = `{"app":"screenlink-pc/0.1.0","name":"DEV-PC","pv":1,"role":"pc"}`，64 字节 = 0x40）：

```
53 4C 4E 4B  01  01  00 00  00 00 00 40   7B 22 61 70 70 22 3A ...
└magic─┘    │ver │type│flags│ └length=64┘   └─────payload──────
帧总长 12 + 64 = 76 字节
```

**DATA 帧头示例**（xid=42，携带 262144 字节块，payload = 4 + 262144 = 262148 = 0x0003FFF4）：

```
53 4C 4E 4B  01  22  00 00  00 03 FF F4   00 00 00 2A  <块字节…>
                          │type=DATA│     └xid=42┘
```

### 帧类型总表

| type | 名称 | 方向 | payload |
|---|---|---|---|
| 0x01 | HELLO | 双向 | JSON |
| 0x02 | AUTH | PC→手机 | JSON |
| 0x03 | AUTH_OK | 手机→PC | JSON |
| 0x04 | NEED_PAIR | 手机→PC | JSON |
| 0x05 | PAIR | PC→手机 | JSON |
| 0x06 | PAIR_OK | 手机→PC | JSON |
| 0x07 | PAIR_FAIL | 手机→PC | JSON |
| 0x10 | LIST_REQ | PC→手机 | JSON（空 `{}`） |
| 0x11 | LIST_RSP | 手机→PC | JSON |
| 0x12 | RECV_REQ | PC→手机 | JSON |
| 0x20 | SEND_REQ | 双向（谁发谁=发送方） | JSON |
| 0x21 | SEND_ACK | 接收方→发送方 | JSON |
| 0x22 | DATA | 发送方→接收方 | 二进制：`xid(4B BE) + 原始字节` |
| 0x23 | PROGRESS | 接收方→发送方 | JSON |
| 0x24 | TRANSFER_DONE | 接收方→发送方 | JSON |
| 0x25 | CANCEL | 双向 | JSON |
| 0x30 | PING | 双向 | JSON `{}` |
| 0x31 | PONG | 双向 | JSON `{}` |
| 0x40 | ERR | 双向 | JSON |
| 0x41 | BYE | 双向 | JSON |

## 5. 命令集

以下 JSON 字段除标注可空外均为必填。`xid`：发送方分配的 u32，**非 0**、连接内唯一、随机起点（如 0x1000 递增）；连接断开即全部失效。唯一例外：§5.2 NOT_FOUND 响应固定 `xid=0` 作"无传输"哨兵。

### 5.1 LIST（PC 查询手机待发送队列）

```
PC → LIST_REQ  {}
手机 → LIST_RSP {"files":[{"id":"12","name":"IMG_1.jpg","size":204800,"mime":"image/jpeg"}, ...]}
```

`files` 为手机 outbox（分享时 PC 未连接而排队的条目，Room 中 `status=QUEUED` 的行；字段 `id`=历史行 id 字符串）。空队列返回 `{"files":[]}`。

### 5.2 RECV（PC 主动拉取，触发手机作为发送方）

```
PC → RECV_REQ  {"id":"12"}          // LIST_RSP 返回的 id；或 {"name":"IMG_1.jpg"} 二选一
手机 → SEND_REQ {"xid":1001,"name":"IMG_1.jpg","size":204800,"mime":"image/jpeg","md5":null}
PC  → SEND_ACK {"xid":1001,"accept":true,"offset":0}
手机 → DATA ...                    // 后续走 §6 标准传输
```

**NOT_FOUND 的关联规则**：RECV_REQ 流程从未发过 SEND_REQ，PC 无从得知 xid，故找不到条目时手机回：

```
手机 → SEND_ACK {"xid":0,"accept":false,"reason":"NOT_FOUND","ref":"12"}
```

- `xid` 固定为 **0**（哨兵值：正常 xid 恒非 0，见 §5 开头；0 表示"该响应不对应任何已建立的传输"，PC 不得用它发 DATA/CANCEL）。
- `ref`（必填）= **原样回显** RECV_REQ 的 `id` 或 `name` 字符串，PC 以此关联是哪个请求失败。
- PC 应**串行**发送 RECV_REQ（收到响应再发下一个）；若实现流水线并发，只能靠 `ref` 关联，此时同一 `ref` 值在请求挂起期间不得重复使用。

### 5.3 SEND（推送，双向可用）

```
发送方 → SEND_REQ {"xid":1002,"name":"report.pdf","size":1048576,"mime":"application/pdf","md5":"<32 hex 可空>"}
接收方 → SEND_ACK {"xid":1002,"accept":true,"offset":524288}     // offset=已有前缀字节数，从该处续传
       → SEND_ACK {"xid":1002,"accept":false,"reason":"NO_STORAGE"|"DENIED"|"BUSY"|"INVALID"}
发送方 → DATA ...（从 offset 起）
```

约束：`name` 为纯文件名（不含路径分隔符，接收方必须拒绝含 `/ \ ..` 的名字 → reason `INVALID`）；`size ≥ 0`（发送方无法确定大小时须先落本地缓存取得 size）；`md5` 可空（发送方已知才带，见 §6.4）。
同一连接并发传输 ≤ 8；超限 → `SEND_ACK reason=BUSY`。

### 5.4 PROGRESS（接收方→发送方，信息性）

```
接收方 → PROGRESS {"xid":1002,"received":1572864}
```

节奏要求：接收方每收满 4 MiB **或** 每 500ms 至少发一帧；允许丢帧（仅展示用，不用于确认）。**流量控制依赖 TCP 窗口**：接收方读循环始终在读，无需显式信用机制。

### 5.5 DATA 与结束

- DATA payload = `xid(4B BE)` + 数据字节；单帧数据部分 ≤ 1 MiB；按 offset 顺序发送，无序号（TCP 有序）。
- 接收方判断 `offset_本帧前累计 + 数据长 == size` 即收满 → 走 §6.4 结算，发 `TRANSFER_DONE`。

### 5.6 CANCEL / PING / BYE / ERR

```
双向 → CANCEL {"xid":1002,"keep":true,"reason":"user"}   // keep=true 接收方保留半成品供续传；false 删除
双向 → PING {}            // 双方空闲 >30s 时空闲方发 PING
     → PONG {}
双向 → BYE {"reason":"shutdown"}   // 对端须回 BYE，双方随后关闭；发送前清空发送缓冲
双向 → ERR  {"code":"PROTOCOL"|"AUTH"|"PAYLOAD_TOO_LARGE"|"TIMEOUT"|"INTERNAL"|"BUSY","message":"..."}  // 严重错误：发 ERR 后立即 close
              // BUSY = 并发/会话数受限（如同一时刻第 2 个认证请求，见 §1）；与 §5.3 SEND_ACK reason=BUSY（传输层并发超限）是两回事
```

## 6. 文件传输与断点续传

### 6.1 正常时序

```
发送方                          接收方
  │ SEND_REQ{xid,name,size,mime,md5} │
  │────────────────────────────────►│ 查重名/查半成品/查空间
  │ SEND_ACK{accept:true,offset}     │
  │◄────────────────────────────────│
  │ DATA[xid][offset..offset+1MiB)   │
  │────────────────────────────────►│
  │ DATA ...                         │
  │────────────────────────────────►│
  │ PROGRESS{received}（≥4MiB/500ms）│
  │◄────────────────────────────────│
  │              ... 收满 size ...    │
  │ TRANSFER_DONE{status:"OK",md5}   │
  │◄────────────────────────────────│
```

### 6.2 offset 续传规则

1. 接收方收到 SEND_REQ 后，按 `(md5 若有，否则 name+size)` 匹配本地半成品（手机侧 = SAF 同名文档 + 历史行 status∈{RUNNING,FAILED}；PC 侧 = 同目录 `<name>.part`）。
2. 命中且半成品 size ≤ 请求 size → `offset = 半成品 size`；未命中 → `offset = 0`。
3. 发送方**必须**从 offset 起发送（源为文件则 seek）。**源为流且不可 seek 时**：
   - `offset == 0`：正常发送，无影响。
   - `offset > 0`：发送方无法从半成品末尾续写，**禁止直接重开同名 SEND_REQ**——SEND_REQ 无 offset 字段，offset 完全由接收方按本地半成品状态决定，接收方会再次 ACK 同一 offset，形成协商死循环。正确流程：
     ```
     发送方 → CANCEL {"xid":1002,"keep":false,"reason":"unseekable"}   // 接收方删除半成品（§6.4）
     发送方 → SEND_REQ {"xid":1003,...}（新 xid）                        // 此时接收方已无半成品
     接收方 → SEND_ACK {"xid":1003,"accept":true,"offset":0}            // 必然从 0 全量
     ```
     即先以 `CANCEL keep=false` 显式清除半成品，再用**新 xid** 重开传输。
4. 接收方以追加模式写入（手机 SAF `"wa"` 流；PC `OpenOptions::append`），并在写入任何续传字节**之前**对已有前缀完成一次 re-hash（串行化处理即可，期间的 TCP 背压交给窗口）。
5. 前缀 re-hash 结果 + 本次新收字节滚动合并 = 最终整文件 MD5（无需结束时重读全文件）。

### 6.3 MD5 校验规则

- 算法：MD5（RFC 1321），32 位小写 hex。选 MD5 是**任务约定 + 防意外损坏**用途，对抗性场景见 §10。
- 发送方在 SEND_REQ 带了 `md5` → 接收方收满后严格比对：一致 → `TRANSFER_DONE{"status":"OK","md5":...}`；不一致 → `status:"MD5_MISMATCH"` 且**删除半成品**，发送方若要重试只能从 0 全量重发（不知损坏区间，offset 续传不安全）。
- 发送方未带 `md5`（典型：手机作为发送方响应 RECV_REQ）→ 接收方仍计算全文件 MD5，`TRANSFER_DONE.md5` 必填，接收方把它落历史记录（信息性，不强校验）。
- `status:"IO_ERROR"`：本地写失败；**保留半成品**，后续可 offset 续传。

### 6.4 TRANSFER_DONE 汇总

```
接收方 → TRANSFER_DONE {"xid":1002,"status":"OK"|"MD5_MISMATCH"|"IO_ERROR","md5":"<32 hex>"}
```

| status | 半成品处置 | 发送方可否续传重试 |
|---|---|---|
| OK | 改名为正式文件 | —（已成功） |
| MD5_MISMATCH | 删除 | 只能全量重发 |
| IO_ERROR | 保留 | 可 offset 续传 |
| 对端 CANCEL keep=true | 保留 | 可 offset 续传 |
| 对端 CANCEL keep=false | 删除 | 全量 |

连接意外断开 = 隐式 CANCEL keep=true；重连后重新走 SEND_REQ 协商 offset。

## 7. 超时与限制汇总

| 项 | 值 |
|---|---|
| HELLO 完成时限 | 连接后 5s |
| 认证完成时限 | **10s 只约束 AUTH 帧到达**（HELLO 后 10s 内 PC 须发出 AUTH，无 token 则发占位 AUTH 触发 NEED_PAIR）。**进入 NEED_PAIR→PAIR 配对流程后 10s 计时挂起**（人工输入 6 位码必然超时），配对全程由配对码 TTL 300s + 失败次数上限约束；PAIR_OK 即认证完成 |
| 空闲保活 | 空闲 >30s 发 PING；**75s 无任何入帧 → close** |
| 单帧 payload | 硬上限 4 MiB；控制帧 ≤64 KiB；DATA 数据 ≤1 MiB |
| 并发传输（xid） | ≤8 |
| 并发连接 | 已认证会话 1；未认证 ≤4 |
| AUTH/PAIR 校验失败 | 单连接 5 次即 close；同 IP **AUTH+PAIR 合并**计数，10min 窗口累计 5 次 → 冷却 60s（定义见 §3.1） |
| 配对码 | 6 位数字；TTL 300s；单次有效；5 次错作废 |
| 文件大小 | u64 无上限；接收方空间不足 ACK `NO_STORAGE` |

## 8. PC 端（Rust）对接清单

1. `mdns-sd` 浏览 `_screenlink._tcp.local`，过滤 TXT `pv==1`，取 `nm/id/pa` 展示。
2. `TcpStream::connect(ip:27188)` → 互发 HELLO → **持久化 token 时必须同时绑定首次配对时该 mDNS 实例的 TXT `id`**（如 `~/.screenlink/token.json` 记 `{id, token_id, token}`，0600）。后续连接规则：仅当目标实例 TXT `id` 与持久化的 `id` **一致**才发送已存 token 的 AUTH；`id` 不匹配或未见过 → 一律视为未配对，走 NEED_PAIR/PAIR 重新配对，**绝不向未知实例发送已持久化的 token**（防伪造实例收割 token，见 §10）。无 token/NEED_PAIR 时走 PAIR（UI 录入 6 位码）。
3. 维持单条长连接（指数退避重连 1s→30s，抖动 ±20%）；空闲 30s 发 PING。
4. 推送手机：LIST→RECV_REQ 拉取 outbox；SEND_REQ 推本地文件（先算 MD5 一并带上）。
5. 接收按 §6 写 `<name>.part` + append，收满比对 MD5 后改名。
6. 帧读写参考实现（tokio）：

```rust
async fn read_frame(r: &mut (impl tokio::io::AsyncRead + Unpin)) -> anyhow::Result<(u8, Vec<u8>)> {
    let mut h = [0u8; 12];
    r.read_exact(&mut h).await?;
    if &h[0..4] != b"SLNK" { anyhow::bail!("bad magic"); }
    if h[4] != 1 { anyhow::bail!("unsupported version {}", h[4]); }
    let len = u32::from_be_bytes(h[8..12].try_into()?) as usize;
    if len > 4 * 1024 * 1024 { anyhow::bail!("frame too large: {len}"); }
    let mut p = vec![0u8; len];
    r.read_exact(&mut p).await?;
    Ok((h[5], p)) // (type, payload)
}
```

## 9. 版本与兼容策略

- 帧头 `version` 与 TXT `pv` 同步演进；v1 内**新增命令类型**（type 值递增）双方按"未知类型忽略"共存。
- 语义破坏性变更（改字段、改帧头）必须 bump version，新旧实例互不发现（`pv` 过滤）。
- flags bit0 预留压缩；启用前 version 不变、通过 HELLO 新字段协商。

## 10. 安全考量（v1 明示接受的风险）

- **主动 token 收割链（必须缓解）**：PC 会在发现实例后**主动**发起连接并发送明文 token，因此 LAN 内伪造 `_screenlink._tcp` 实例的主机可被动收割 PC 发来的 token，再冒充 PC 连接真机完成任意收发。缓解（§8.2 强制）：PC 将持久化 token 与首次配对时的实例 `id`（手机持久随机 8 hex，见 §2）绑定，**只向 `id` 一致的实例发 AUTH**；`id` 不符的实例走重新配对，攻击者拿不到配对码（配对码只在真机 UI 展示且 300s/5 次约束）即无法完成 PAIR。残余风险：攻击者在真机首次配对窗口内抢先冒充真机实例（同 `id`）诱导 PC 配对，属 v1 接受的局域网风险。
- 无 TLS：局域网窃听者可见文件内容与元数据；token 亦在线上明文传输（每次连接）。缓解：仅私网/链路本地接入过滤 + token 256 位熵 + 认证限速 + §8.2 的实例 `id` 绑定（使窃听者必须先攻破一次配对才能获得可用 token，而配对窗口外无从取得）。家庭/办公可信局域网场景接受此风险；如需强化，后续版本升级 version + TLS（rustls / Android SSLSocket）。
- 6 位配对码为 20 位熵，仅作为**在线**一次性门槛（300s TTL、5 次失败作废、IP 冷却），不承担离线强度——离线强度由 HKDF 种子保证（§3.3）。
- token 校验用常量时间比较（Rust `subtle` / Android `MessageDigest.isEqual`），防计时侧信道。
- 接收方必须净化 `name`（拒绝路径分隔符与 `..`），手机侧文件只落在 SAF 授权目录内。
