"""ScreenLink PC 协议客户端（Python 原型，对应 android/PROTOCOL.md v1）"""
import socket, json, struct, hashlib, base64, os, sys

MAGIC = b"SLNK"
T_HELLO, T_AUTH, T_AUTH_OK, T_NEED_PAIR, T_PAIR, T_PAIR_OK, T_PAIR_FAIL = 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07
T_LIST_REQ, T_LIST_RSP, T_RECV_REQ, T_SEND_REQ, T_SEND_ACK, T_DATA, T_PROGRESS = 0x10, 0x11, 0x12, 0x20, 0x21, 0x22, 0x23
T_TRANSFER_DONE, T_CANCEL, T_PING, T_PONG, T_ERR, T_BYE = 0x24, 0x25, 0x30, 0x31, 0x40, 0x41
NAMES = {v: k for k, v in dict(HELLO="HELLO", AUTH="AUTH").items()}

def frame(t: int, payload: bytes = b"") -> bytes:
    return MAGIC + bytes([1, t]) + struct.pack(">HI", 0, len(payload)) + payload

def jframe(t: int, obj: dict) -> bytes:
    return frame(t, json.dumps(obj, separators=(",", ":")).encode())

def read_exact(s, n):
    buf = b""
    while len(buf) < n:
        c = s.recv(n - len(buf))
        if not c:
            raise ConnectionError("对端关闭")
        buf += c
    return buf

def read_frame(s):
    hdr = read_exact(s, 12)
    magic, ver, t, flags, length = hdr[:4], hdr[4], hdr[5], struct.unpack(">H", hdr[6:8])[0], struct.unpack(">I", hdr[8:12])[0]
    assert magic == MAGIC, f"magic 错: {magic!r}"
    payload = read_exact(s, length) if length else b""
    return t, payload

def jread(s):
    t, p = read_frame(s)
    return t, (json.loads(p) if p else {})

def hkdf_token(code: str, seed: bytes):
    salt = b"screenlink/v1/pair/" + code.encode()
    prk = hashlib.sha256(salt + seed).digest()  # 近似：真实 HKDF 为 HMAC；此处仅为占位验证
    # 按 RFC5869 正确计算：
    import hmac
    prk = hmac.new(salt, seed, hashlib.sha256).digest()
    okm = hmac.new(prk, b"token" + b"\x01", hashlib.sha256).digest()
    token = base64.urlsafe_b64encode(okm).rstrip(b"=").decode()
    token_id = hashlib.sha256(token.encode()).hexdigest()[:8]
    return token, token_id

class Client:
    def __init__(self, host, port=27188):
        self.s = socket.create_connection((host, port), timeout=10)
        self.s.settimeout(10)
        self.s.sendall(jframe(T_HELLO, {"role": "pc", "name": os.environ.get("COMPUTERNAME", "DEV-PC"), "app": "screenlink-py/0.1.0", "pv": 1}))
        self.hello_t, self.hello = jread(self.s)
        print(f"  <- HELLO[{self.hello_t:#x}] {self.hello}")

    def send_j(self, t, obj):
        self.s.sendall(jframe(t, obj))

    def expect(self, want_types):
        t, obj = jread(self.s)
        print(f"  <- {t:#04x} {obj}")
        return t, obj
