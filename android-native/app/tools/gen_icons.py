#!/usr/bin/env python3
"""从项目图标 docs/assets/logo.svg（桌面 exe / 托盘 / 窗口用的像素猫 + 双向半箭头）生成安卓图标资源。

输出（相对 android-native/app/src/main/res）：
  drawable/ic_launcher_foreground.xml   自适应图标前景（108dp 矢量，像素猫居中，落在 66dp 安全区内）
  drawable/ic_launcher_monochrome.xml   Android 13+ 主题图标：猫剪影，两个半箭头镂空
  drawable/ic_stat.xml                  通知小图标（24dp 白色剪影，箭头镂空）
  drawable/ic_tile.xml                  快捷设置 Tile 图标（同通知小图标）
  values/ic_launcher_background.xml     背景色 = 桌面图标底色 #F3EEE3
  mipmap-*/ic_launcher.png / ic_launcher_round.png   旧式位图（圆角方 / 圆），48dp 各密度
用法：python3 app/tools/gen_icons.py [repo 根目录]（需要 Pillow）
"""
import re, sys, os
from PIL import Image, ImageDraw

ROOT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "..", "..", "..")
SVG = os.path.join(ROOT, "docs", "assets", "logo.svg")
RES = os.path.join(ROOT, "android-native", "app", "src", "main", "res")
BG = "#F3EEE3"
ARROW = {"#E6A38A", "#E1947B", "#DB8E74", "#D4886D"}   # 两个半箭头的四种浅色

src = open(SVG, encoding="utf-8").read()
rects = re.findall(r'<rect x="([\d.]+)" y="([\d.]+)" width="([\d.]+)" height="[\d.]+" fill="(#\w+)"', src)
step = 34.56
x0 = min(float(r[0]) for r in rects); y0 = min(float(r[1]) for r in rects)
cells = {}
for x, y, w, col in rects:
    cells[(round((float(x) - x0) / step), round((float(y) - y0) / step))] = col.upper()
W = max(c[0] for c in cells) + 1; H = max(c[1] for c in cells) + 1   # 16 × 14


def runs(pred):
    """按行合并相邻格子成横条，返回 (col, row, len)。"""
    out = []
    for y in range(H):
        x = 0
        while x < W:
            if pred(x, y):
                s = x
                while x < W and pred(x, y): x += 1
                out.append((s, y, x - s))
            else:
                x += 1
    return out


def path(rs, cell, ox, oy, grow=0.0):
    f = lambda v: ("%.3f" % v).rstrip("0").rstrip(".")
    return "".join(f"M{f(ox + s * cell)},{f(oy + y * cell)}h{f(n * cell + grow)}v{f(cell + grow)}h{f(-(n * cell + grow))}z" for s, y, n in rs)


def vector(size_dp, viewport, body):
    return (f'<?xml version="1.0" encoding="utf-8"?>\n<!-- 由 app/tools/gen_icons.py 从 docs/assets/logo.svg 生成，勿手改 -->\n'
            f'<vector xmlns:android="http://schemas.android.com/apk/res/android"\n    android:width="{size_dp}dp" android:height="{size_dp}dp"\n'
            f'    android:viewportWidth="{viewport}" android:viewportHeight="{viewport}">\n{body}</vector>\n')


def silhouette_xml(size, cell, color):
    ox = (size - W * cell) / 2; oy = (size - H * cell) / 2
    solid = runs(lambda x, y: (x, y) in cells and cells[(x, y)] not in ARROW)
    return vector(size, size, f'    <path android:fillColor="{color}" android:pathData="{path(solid, cell, ox, oy)}" />\n')


def write(rel, text):
    p = os.path.join(RES, rel); os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, "w", encoding="utf-8", newline="\n").write(text); print("wrote", rel)


# ---- 自适应前景：108dp，格子 2.75dp（猫 44×38.5dp，最远角距中心约 27dp < 安全区半径 33dp）
CELL = 2.75
ox = (108 - W * CELL) / 2; oy = (108 - H * CELL) / 2
body = []
# 先铺一层整只猫的底色，避免相邻色块之间抗锯齿出现发丝缝
body.append(f'    <path android:fillColor="#FFD16F4E" android:pathData="{path(runs(lambda x, y: (x, y) in cells), CELL, ox, oy)}" />\n')
for col in sorted(set(cells.values())):
    rs = runs(lambda x, y, col=col: cells.get((x, y)) == col)
    body.append(f'    <path android:fillColor="#FF{col[1:]}" android:pathData="{path(rs, CELL, ox, oy, 0.02)}" />\n')
write("drawable/ic_launcher_foreground.xml", vector(108, 108, "".join(body)))
write("drawable/ic_launcher_monochrome.xml", silhouette_xml(108, CELL, "#FF000000"))
# ---- 通知 / Tile：24dp，格子 1.375dp（猫 22×19.25dp）
write("drawable/ic_stat.xml", silhouette_xml(24, 1.375, "#FFFFFFFF"))
write("drawable/ic_tile.xml", silhouette_xml(24, 1.375, "#FFFFFFFF"))
write("values/ic_launcher_background.xml",
      f'<?xml version="1.0" encoding="utf-8"?>\n<resources>\n    <!-- 桌面图标底色（docs/assets/logo.svg） -->\n    <color name="ic_launcher_background">{BG}</color>\n</resources>\n')


# ---- 位图渲染（旧式 mipmap + 预览）
def render(px, shape, scale=4):
    """shape: 'square'（圆角方，同桌面）/ 'round' / 'full'（自适应 108dp 全画布，无遮罩）。"""
    S = px * scale
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    if shape == "full":
        d.rectangle([0, 0, S, S], fill=BG); cell = S * CELL / 108
    else:
        m = S * 0.04
        if shape == "round": d.ellipse([m, m, S - m, S - m], fill=BG)
        else: d.rounded_rectangle([m, m, S - m, S - m], radius=S * 0.22, fill=BG)
        cell = S * (CELL / 72)        # 与启动器里 72dp 可见区内的比例一致
    ox_ = (S - W * cell) / 2; oy_ = (S - H * cell) / 2
    for (x, y), col in cells.items():
        d.rectangle([ox_ + x * cell, oy_ + y * cell, ox_ + (x + 1) * cell - 1, oy_ + (y + 1) * cell - 1], fill=col)
    return img.resize((px, px), Image.LANCZOS)


for dens, px in {"mdpi": 48, "hdpi": 72, "xhdpi": 96, "xxhdpi": 144, "xxxhdpi": 192}.items():
    for name, shape in (("ic_launcher", "square"), ("ic_launcher_round", "round")):
        p = os.path.join(RES, f"mipmap-{dens}", name + ".png"); os.makedirs(os.path.dirname(p), exist_ok=True)
        render(px, shape).save(p, optimize=True); print("wrote", p)

if __name__ == "__main__" and os.environ.get("PREVIEW"):
    render(512, "square").save(os.environ["PREVIEW"])
