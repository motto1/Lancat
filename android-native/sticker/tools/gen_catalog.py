#!/usr/bin/env python3
"""从桌面 crates/lancat-slint/src/emoji_catalog.rs + src/assets/emoji/*.json 生成 :sticker 的目录和资源。

用法：gen_catalog.py <emoji_catalog.rs> <emoji_json_dir> <sticker_module_dir>
  - 写 src/main/kotlin/fan/lancat/sticker/EmojiCatalogData.kt（自动生成，勿手改）
  - 写 src/main/assets/emoji/<id>.z：raw deflate（无 zlib 头），level 9，与桌面 build.rs 的
    flate2::DeflateEncoder + Compression::best() 相同格式；运行时用 java.util.zip.Inflater(nowrap=true) 解压
  - 复制 LICENSE.md → src/main/assets/emoji/LICENSE.md（CC BY 4.0 署名）
"""
import os
import re
import sys
import zlib


def kstr(s: str) -> str:
    return '"' + s.replace('\\', '\\\\').replace('"', '\\"').replace('$', '\\$') + '"'


def main():
    rs_path, json_dir, mod_dir = sys.argv[1:4]
    rs = open(rs_path, encoding='utf-8').read()
    cats = re.findall(r'Cat \{ id: "([^"]*)", label: "([^"]*)", icon: "([^"]*)" \}', rs)
    emo = re.findall(r'Emoji \{ code: "([^"]*)", ch: "([^"]*)", cat: "([^"]*)", kw: "([^"]*)" \}', rs)
    assert cats and emo, 'parse failed'
    ids = [e[0] for e in emo]
    assert len(ids) == len(set(ids)), 'duplicate id'

    out_assets = os.path.join(mod_dir, 'src/main/assets/emoji')
    os.makedirs(out_assets, exist_ok=True)
    for f in os.listdir(out_assets):
        if f.endswith('.z'):
            os.remove(os.path.join(out_assets, f))
    raw_total = z_total = 0
    for code in ids:
        raw = open(os.path.join(json_dir, code + '.json'), 'rb').read()
        c = zlib.compressobj(9, zlib.DEFLATED, -15, 9)
        z = c.compress(raw) + c.flush()
        assert zlib.decompress(z, -15) == raw
        open(os.path.join(out_assets, code + '.z'), 'wb').write(z)
        raw_total += len(raw)
        z_total += len(z)
    lic = os.path.join(json_dir, 'LICENSE.md')
    if os.path.exists(lic):
        open(os.path.join(out_assets, 'LICENSE.md'), 'wb').write(open(lic, 'rb').read())

    lines = []
    lines.append('// 自动生成，勿手改：sticker/tools/gen_catalog.py ← 桌面 crates/lancat-slint/src/emoji_catalog.rs')
    lines.append('// （其上游是 src/lib/emoji-catalog.ts）。Noto Animated Emoji，Google，CC BY 4.0。')
    lines.append('package fan.lancat.sticker')
    lines.append('')
    lines.append('internal object EmojiCatalogData {')
    lines.append('    /** (id, title, icon)，顺序与桌面相同；含 "recent" 和 "stickers" 两个动态分类。 */')
    lines.append('    val CATEGORIES: Array<Array<String>> = arrayOf(')
    for i, l, ic in cats:
        lines.append(f'        arrayOf({kstr(i)}, {kstr(l)}, {kstr(ic)}),')
    lines.append('    )')
    lines.append('')
    lines.append('    /** (id, char, category, keywords 空格分隔) */')
    lines.append('    val EMOJI: Array<Array<String>> = arrayOf(')
    for code, ch, cat, kw in emo:
        lines.append(f'        arrayOf({kstr(code)}, {kstr(ch)}, {kstr(cat)}, {kstr(kw)}),')
    lines.append('    )')
    lines.append('}')
    kt = os.path.join(mod_dir, 'src/main/kotlin/fan/lancat/sticker/EmojiCatalogData.kt')
    open(kt, 'w', encoding='utf-8').write('\n'.join(lines) + '\n')
    print(f'categories={len(cats)} emoji={len(emo)} raw={raw_total} deflate={z_total}')


if __name__ == '__main__':
    main()
