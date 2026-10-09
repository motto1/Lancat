// Spring physics baked into Web Animations keyframes. Only transform + opacity are animated,
// so everything stays on the compositor (60/120 fps).

export interface Pose {
  x?: number; // px
  y?: number; // px
  scale?: number;
  opacity?: number;
}

export interface SpringOpts {
  stiffness?: number;
  damping?: number;
  mass?: number;
  /** Opacity reaches its target after this many ms (no overshoot on opacity). */
  fadeMs?: number;
}

/** Simulates a unit spring 0 -> 1; returns samples [t(ms), p]. */
export function springCurve({ stiffness = 300, damping = 24, mass = 1 }: SpringOpts = {}): [number, number][] {
  const dt = 1 / 240;
  let p = 0;
  let v = 0;
  let t = 0;
  const out: [number, number][] = [[0, 0]];
  let lastSample = 0;
  while (t < 2.5) {
    const a = (-stiffness * (p - 1) - damping * v) / mass;
    v += a * dt;
    p += v * dt;
    t += dt;
    if (t - lastSample >= 1 / 60) {
      out.push([t * 1000, p]);
      lastSample = t;
    }
    if (Math.abs(p - 1) < 0.0008 && Math.abs(v) < 0.01) break;
  }
  out.push([t * 1000, 1]);
  return out;
}

const curveCache = new Map<string, [number, number][]>();

function lerp(a: number, b: number, p: number) {
  return a + (b - a) * p;
}

function transformOf(x: number, y: number, s: number) {
  return `translate3d(${x.toFixed(2)}px, ${y.toFixed(2)}px, 0) scale(${s.toFixed(4)})`;
}

/** Animates `el` from `from` to `to` with a spring. Resolves when settled. */
export function springTo(el: Element, from: Pose, to: Pose, opts: SpringOpts = {}): Animation {
  const key = `${opts.stiffness ?? 300}/${opts.damping ?? 24}/${opts.mass ?? 1}`;
  let curve = curveCache.get(key);
  if (!curve) {
    curve = springCurve(opts);
    curveCache.set(key, curve);
  }
  const duration = curve[curve.length - 1][0];
  const fade = opts.fadeMs ?? 140;
  const f = { x: from.x ?? 0, y: from.y ?? 0, scale: from.scale ?? 1, opacity: from.opacity ?? 1 };
  const t = { x: to.x ?? 0, y: to.y ?? 0, scale: to.scale ?? 1, opacity: to.opacity ?? 1 };
  const keyframes: Keyframe[] = curve.map(([ms, p]) => ({
    offset: ms / duration,
    transform: transformOf(lerp(f.x, t.x, p), lerp(f.y, t.y, p), lerp(f.scale, t.scale, p)),
    opacity: lerp(f.opacity, t.opacity, Math.min(1, ms / fade)),
  }));
  return el.animate(keyframes, { duration, easing: "linear", fill: "both" });
}

/** A quick ease-in exit (used for collapse). */
export function exitTo(el: Element, from: Pose, to: Pose, duration = 200): Animation {
  const f = { x: from.x ?? 0, y: from.y ?? 0, scale: from.scale ?? 1, opacity: from.opacity ?? 1 };
  const t = { x: to.x ?? 0, y: to.y ?? 0, scale: to.scale ?? 1, opacity: to.opacity ?? 1 };
  return el.animate(
    [
      { transform: transformOf(f.x, f.y, f.scale), opacity: f.opacity },
      { transform: transformOf(t.x, t.y, t.scale), opacity: t.opacity },
    ],
    // slight anticipation then accelerate toward the edge ("回弹收起")
    { duration, easing: "cubic-bezier(0.5, -0.35, 0.75, 0.6)", fill: "both" },
  );
}

/** Offset that hides a panel toward the given screen edge. */
export function edgeOffset(edge: string, dist = 40): Pose {
  switch (edge) {
    case "left":
      return { x: -dist };
    case "top":
      return { y: -dist };
    case "bottom":
      return { y: dist };
    default:
      return { x: dist };
  }
}
