// "Liquid glass" for in-page surfaces, no native help needed: an SVG displacement filter used as a
// backdrop-filter (Chromium/WebView2 supports `backdrop-filter: url(#…)`). For each element a
// displacement map is drawn on a canvas: flat in the middle, bending the backdrop inward along a
// rounded-rect rim like the edge of a lens. Usage: <div use:liquid={{ radius: 14 }}>.
// The page cannot see the desktop behind the window, so this refracts the app's own content.

let svgRoot: SVGSVGElement | null = null;
let seq = 0;

function root(): SVGSVGElement {
  if (svgRoot) return svgRoot;
  svgRoot = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svgRoot.setAttribute("width", "0");
  svgRoot.setAttribute("height", "0");
  svgRoot.style.position = "absolute";
  svgRoot.style.pointerEvents = "none";
  document.body.appendChild(svgRoot);
  return svgRoot;
}

/** Displacement maps by size/shape: a per-pixel JS loop + PNG encode (tens of ms for a toast-
 *  sized element) used to run again for every toast / picker hint, although they come in a few
 *  sizes only. Small LRU. */
const mapCache = new Map<string, string>();
function cachedMap(w: number, h: number, r: number, bevel: number): string {
  const key = `${w}x${h}:${r}:${bevel}`;
  let url = mapCache.get(key);
  if (url) mapCache.delete(key);
  else url = displacementMap(w, h, r, bevel);
  mapCache.set(key, url);
  while (mapCache.size > 16) mapCache.delete(mapCache.keys().next().value!);
  return url;
}

/** R = x shift, G = y shift (128 = none), strongest at the rim, fading over `bevel` px. */
function displacementMap(w: number, h: number, r: number, bevel: number): string {
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  const ctx = c.getContext("2d")!;
  const img = ctx.createImageData(w, h);
  const hx = w / 2,
    hy = h / 2;
  r = Math.min(r, hx, hy);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const fx = x + 0.5 - hx,
        fy = y + 0.5 - hy;
      const qx = Math.abs(fx) - (hx - r),
        qy = Math.abs(fy) - (hy - r);
      const out = Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - r;
      const d = -out;
      let nx = 0,
        ny = 0,
        k = 0;
      if (d >= 0 && d < bevel) {
        if (qx > 0 && qy > 0) {
          const l = Math.hypot(qx, qy) || 1;
          nx = qx / l;
          ny = qy / l;
        } else if (qx > qy) nx = 1;
        else ny = 1;
        nx *= Math.sign(fx);
        ny *= Math.sign(fy);
        k = (1 - d / bevel) ** 2;
      }
      const i = (y * w + x) * 4;
      // sample from further inside (towards the centre): the rim magnifies/bends the backdrop
      img.data[i] = 128 - nx * k * 127;
      img.data[i + 1] = 128 - ny * k * 127;
      img.data[i + 2] = 128;
      img.data[i + 3] = 255;
    }
  }
  ctx.putImageData(img, 0, 0);
  return c.toDataURL();
}

export interface LiquidOpts {
  radius?: number;
  bevel?: number;
  /** max displacement in px */
  depth?: number;
  blur?: number;
}

export function liquid(node: HTMLElement, opts: LiquidOpts = {}) {
  const id = `lq-${++seq}`;
  const NS = "http://www.w3.org/2000/svg";
  const filter = document.createElementNS(NS, "filter");
  filter.setAttribute("id", id);
  filter.setAttribute("x", "0");
  filter.setAttribute("y", "0");
  filter.setAttribute("width", "100%");
  filter.setAttribute("height", "100%");
  filter.setAttribute("color-interpolation-filters", "sRGB");
  const fe = document.createElementNS(NS, "feImage");
  fe.setAttribute("result", "map");
  fe.setAttribute("preserveAspectRatio", "none");
  const disp = document.createElementNS(NS, "feDisplacementMap");
  disp.setAttribute("in", "SourceGraphic");
  disp.setAttribute("in2", "map");
  disp.setAttribute("xChannelSelector", "R");
  disp.setAttribute("yChannelSelector", "G");
  filter.append(fe, disp);
  root().appendChild(filter);

  let o = opts;
  let lastKey = "";
  let size = { w: 0, h: 0 };
  const update = () => {
    const { w, h } = size;
    if (!w || !h) return;
    const radius = o.radius ?? 14;
    const bevel = o.bevel ?? Math.min(14, h / 2);
    const key = `${w}x${h}:${radius}:${bevel}`;
    if (key === lastKey) return;
    lastKey = key;
    fe.setAttribute("href", cachedMap(w, h, radius, bevel));
    fe.setAttribute("width", String(w));
    fe.setAttribute("height", String(h));
    disp.setAttribute("scale", String((o.depth ?? 18) * 2));
    const f = `url(#${id}) blur(${o.blur ?? 6}px) saturate(170%)`;
    node.style.setProperty("backdrop-filter", f);
    node.style.setProperty("-webkit-backdrop-filter", f);
  };
  // sizes come from the observer (its first callback arrives before the first paint), so mounting
  // does not force a synchronous layout with offsetWidth/offsetHeight
  const ro = new ResizeObserver((entries) => {
    const b = entries[entries.length - 1].borderBoxSize?.[0];
    size = b ? { w: Math.round(b.inlineSize), h: Math.round(b.blockSize) } : { w: Math.round(node.offsetWidth), h: Math.round(node.offsetHeight) };
    update();
  });
  ro.observe(node);
  return {
    update(n: LiquidOpts) {
      const next = n ?? {};
      // the template passes a fresh object literal on every update: only redo on real changes
      if (next.radius === o.radius && next.bevel === o.bevel && next.depth === o.depth && next.blur === o.blur) return;
      o = next;
      lastKey = "";
      update();
    },
    destroy() {
      ro.disconnect();
      filter.remove();
    },
  };
}
