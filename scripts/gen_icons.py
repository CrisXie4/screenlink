"""Generate ScreenLink icons (rounded-square logo) as PNG + ICO without PIL."""
import zlib, struct, math, os

def rounded_rect_mask(w, h, r):
    m = [[0.0]*w for _ in range(h)]
    for y in range(h):
        for x in range(w):
            dx = dy = 0.0
            if x < r: dx = r - x
            elif x >= w - r: dx = x - (w - r - 1)
            if y < r: dy = r - y
            elif y >= h - r: dy = y - (h - r - 1)
            d = math.hypot(dx, dy)
            m[y][x] = max(0.0, min(1.0, r - d + 0.5)) if (dx > 0 and dy > 0) else 1.0
    return m

def lerp(a, b, t): return tuple(int(a[i] + (b[i]-a[i])*t) for i in range(4))

BG1, BG2 = (15, 23, 42, 255), (23, 37, 68, 255)      # deep navy gradient
ACC      = (45, 212, 191, 255)                        # teal
ACC2     = (56, 189, 248, 255)                        # sky
WHITE    = (240, 253, 250, 255)

def draw(size):
    w = h = size
    img = [[(0,0,0,0)]*w for _ in range(h)]
    mask = rounded_rect_mask(w, h, int(size*0.22))
    # background gradient (diagonal)
    for y in range(h):
        for x in range(w):
            t = (x+y)/(2*w)
            img[y][x] = lerp(BG1, BG2, t)
    s = size / 128.0
    def fill_round_rect(x0, y0, x1, y1, r, color, alpha=1.0):
        for y in range(int(y0), int(y1)):
            for x in range(int(x0), int(x1)):
                if 0 <= x < w and 0 <= y < h:
                    dx = max(x0 + r - x, x - (x1 - r - 1), 0)
                    dy = max(y0 + r - y, y - (y1 - r - 1), 0)
                    d = math.hypot(dx, dy)
                    cov = max(0.0, min(1.0, r - d + 0.5)) if (dx > 0 and dy > 0) else 1.0
                    if cov > 0:
                        img[y][x] = lerp(img[y][x], color, cov*alpha)
    # phone body (left, slightly rotated feel via offset)
    fill_round_rect(22*s, 26*s, 66*s, 102*s, 7*s, ACC, 0.95)
    fill_round_rect(27*s, 34*s, 61*s, 94*s, 4*s, BG1, 1.0)   # screen
    # PC body (right)
    fill_round_rect(74*s, 40*s, 110*s, 88*s, 5*s, ACC2, 0.95)
    fill_round_rect(78*s, 45*s, 106*s, 76*s, 2*s, BG1, 1.0)
    # arrow phone -> pc
    ax, ay = 60*s, 64*s
    for i in range(int(12*s)):
        x = ax + i
        yy = ay + math.sin(i/6*math.pi)*0
        if 0 <= x < w: img[int(yy)][int(x)] = lerp(img[int(yy)][int(x)], WHITE, 0.9)
        if 0 <= x < w and 0 <= int(yy)+1 < h: img[int(yy)+1][int(x)] = lerp(img[int(yy)+1][int(x)], WHITE, 0.55)
    for i in range(int(7*s)):
        img[int(ay - i*0.6)][int(70*s - i*0.9)] = lerp(img[int(ay - i*0.6)][int(70*s - i*0.9)], WHITE, 0.9)
        img[int(ay + i*0.6)][int(70*s - i*0.9)] = lerp(img[int(ay + i*0.6)][int(70*s - i*0.9)], WHITE, 0.9)
    # apply rounded mask
    for y in range(h):
        for x in range(w):
            m = mask[y][x]
            if m < 1.0:
                r0, g0, b0, a0 = img[y][x]
                img[y][x] = (r0, g0, b0, int(a0*m))
    return img

def png_bytes(img):
    h, w = len(img), len(img[0])
    raw = b''
    for row in img:
        raw += b'\x00' + b''.join(struct.pack('4B', *px) for px in row)
    def chunk(t, d):
        c = struct.pack('>I', len(d)) + t + d
        return c + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
    return (b'\x89PNG\r\n\x1a\n'
            + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(raw, 9))
            + chunk(b'IEND', b''))

def ico_bytes(pngs):
    # ICO wrapping PNG entries (Vista+). pngs: list of (size, bytes)
    n = len(pngs)
    hdr = struct.pack('<HHH', 0, 1, n)
    entries = b''
    offset = 6 + 16*n
    datas = b''
    for size, data in pngs:
        entries += struct.pack('<BBBBHHII', size % 256, size % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        datas += data
    return hdr + entries + datas

out = os.path.join(os.path.dirname(__file__), '..', 'crates', 'screenlink-app', 'icons')
os.makedirs(out, exist_ok=True)
img32, img128 = draw(32), draw(128)
open(os.path.join(out, '32x32.png'), 'wb').write(png_bytes(img32))
open(os.path.join(out, '128x128.png'), 'wb').write(png_bytes(img128))
open(os.path.join(out, 'icon.ico'), 'wb').write(ico_bytes([(32, png_bytes(img32)), (128, png_bytes(img128))]))
print('icons written to', os.path.abspath(out))
