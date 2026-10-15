import { useId } from "react";

/** Ring of the repository volume: repository share, other used space, free space. */
export function DiskRing({ total, free, repo, size = 188 }: { total: number; free: number; repo: number; size?: number }) {
  const id = useId();
  const stroke = 13;
  const r = (size - stroke) / 2 - 4;
  const c = 2 * Math.PI * r;
  const safeTotal = total > 0 ? total : 1;
  const repoFrac = Math.min(repo / safeTotal, 1);
  const otherFrac = Math.max(0, Math.min((total - free - repo) / safeTotal, 1 - repoFrac));
  // A tiny repository still gets a visible arc.
  const repoLen = repo > 0 ? Math.max(repoFrac * c, 6) : 0;
  const gap = 5;
  const otherLen = Math.max(otherFrac * c - gap, 0);
  return (
    <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} className="flex-none">
      <defs>
        <linearGradient id={`${id}-repo`} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0%" style={{ stopColor: "var(--a3)" }} />
          <stop offset="100%" style={{ stopColor: "var(--a1)" }} />
        </linearGradient>
        <linearGradient id={`${id}-other`} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0%" style={{ stopColor: "var(--s1)" }} />
          <stop offset="100%" style={{ stopColor: "var(--s2)" }} />
        </linearGradient>
        <filter id={`${id}-glow`} x="-30%" y="-30%" width="160%" height="160%">
          <feGaussianBlur stdDeviation="3.5" result="blur" />
          <feMerge>
            <feMergeNode in="blur" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
      </defs>
      <g transform={`rotate(-90 ${size / 2} ${size / 2})`} fill="none" strokeLinecap="round" strokeWidth={stroke}>
        <circle cx={size / 2} cy={size / 2} r={r} stroke="rgb(255 255 255 / 0.07)" />
        {otherLen > 0 && (
          <circle
            cx={size / 2}
            cy={size / 2}
            r={r}
            stroke={`url(#${id}-other)`}
            strokeDasharray={`${otherLen} ${c}`}
            strokeDashoffset={-(repoLen + gap)}
            filter={`url(#${id}-glow)`}
            style={{ transition: "stroke-dasharray 600ms ease" }}
          />
        )}
        {repoLen > 0 && (
          <circle
            cx={size / 2}
            cy={size / 2}
            r={r}
            stroke={`url(#${id}-repo)`}
            strokeDasharray={`${repoLen} ${c}`}
            filter={`url(#${id}-glow)`}
            style={{ transition: "stroke-dasharray 600ms ease" }}
          />
        )}
      </g>
    </svg>
  );
}

/** Catmull-Rom spline through the points, as a cubic Bezier SVG path. */
function smoothPath(points: [number, number][]): string {
  if (points.length === 0) return "";
  let d = `M ${points[0][0]} ${points[0][1]}`;
  for (let i = 0; i < points.length - 1; i++) {
    const p0 = points[i - 1] ?? points[i];
    const p1 = points[i];
    const p2 = points[i + 1];
    const p3 = points[i + 2] ?? p2;
    const t = 0.18;
    const c1x = p1[0] + (p2[0] - p0[0]) * t;
    const c1y = p1[1] + (p2[1] - p0[1]) * t;
    const c2x = p2[0] - (p3[0] - p1[0]) * t;
    const c2y = p2[1] - (p3[1] - p1[1]) * t;
    d += ` C ${c1x} ${c1y}, ${c2x} ${c2y}, ${p2[0]} ${p2[1]}`;
  }
  return d;
}

/** Smooth area chart with a gradient stroke (activity per bucket, oldest first). */
export function AreaChart({ values, height = 150 }: { values: number[]; height?: number }) {
  const id = useId();
  const width = 600;
  const pad = 10;
  const data = values.length > 1 ? values : [0, 0];
  const max = Math.max(...data, 1);
  const step = (width - pad * 2) / (data.length - 1);
  const points: [number, number][] = data.map((v, i) => [
    pad + i * step,
    height - pad - (v / max) * (height - pad * 2.6),
  ]);
  const line = smoothPath(points);
  const area = `${line} L ${points[points.length - 1][0]} ${height} L ${points[0][0]} ${height} Z`;
  const last = points[points.length - 1];
  return (
    <svg viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" className="h-full w-full overflow-visible">
      <defs>
        <linearGradient id={`${id}-stroke`} x1="0" y1="0" x2="1" y2="0">
          <stop offset="0%" style={{ stopColor: "var(--s1)" }} />
          <stop offset="55%" style={{ stopColor: "var(--a1)" }} />
          <stop offset="100%" style={{ stopColor: "var(--a3)" }} />
        </linearGradient>
        <linearGradient id={`${id}-fill`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" style={{ stopColor: "var(--a1)", stopOpacity: 0.38 }} />
          <stop offset="100%" style={{ stopColor: "var(--a1)", stopOpacity: 0 }} />
        </linearGradient>
      </defs>
      {[0.25, 0.5, 0.75].map((f) => (
        <line
          key={f}
          x1={0}
          x2={width}
          y1={height * f}
          y2={height * f}
          stroke="rgb(255 255 255 / 0.05)"
          strokeDasharray="4 6"
          vectorEffect="non-scaling-stroke"
        />
      ))}
      <path d={area} fill={`url(#${id}-fill)`} />
      <path
        d={line}
        fill="none"
        stroke={`url(#${id}-stroke)`}
        strokeWidth={2.5}
        strokeLinecap="round"
        vectorEffect="non-scaling-stroke"
        style={{ filter: "drop-shadow(0 4px 10px rgb(var(--a1-rgb) / 0.55))" }}
      />
      <circle cx={last[0]} cy={last[1]} r={4.5} style={{ fill: "var(--a3)" }} stroke="white" strokeWidth={2} vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

/** Thin gradient bar under a metric (as in the reference cards). */
export function MeterBar({ fraction, from, to }: { fraction: number; from: string; to: string }) {
  const f = Math.max(0, Math.min(1, fraction));
  return (
    <div className="h-1.5 w-full overflow-hidden rounded-full bg-white/8">
      <div
        className="h-full rounded-full"
        style={{
          width: `${Math.max(f * 100, 6)}%`,
          background: `linear-gradient(90deg, ${from}, ${to})`,
          boxShadow: `0 0 12px color-mix(in srgb, ${to} 55%, transparent)`,
          transition: "width 500ms ease",
        }}
      />
    </div>
  );
}

/** Decorative waves and particles of the hero card. */
export function HeroWaves() {
  return (
    <svg className="pointer-events-none absolute inset-0 h-full w-full" viewBox="0 0 640 260" preserveAspectRatio="none">
      <path d="M0 190 C 120 150, 210 230, 330 190 S 520 120, 640 170 L 640 260 L 0 260 Z" fill="white" fillOpacity="0.09" />
      <path d="M0 215 C 140 185, 250 250, 380 212 S 560 165, 640 205 L 640 260 L 0 260 Z" fill="white" fillOpacity="0.08" />
      <path d="M0 240 C 160 220, 300 262, 420 236 S 590 214, 640 232 L 640 260 L 0 260 Z" style={{ fill: "var(--a3)", fillOpacity: 0.22 }} />
      {[
        [470, 62, 3, "#ffe27a"],
        [520, 96, 2.2, "white"],
        [560, 48, 2.6, "#ff9ccf"],
        [430, 110, 1.8, "white"],
        [600, 120, 2, "#9ef0ff"],
        [380, 52, 1.6, "white"],
      ].map(([x, y, r, c]) => (
        <circle key={`${x}-${y}`} cx={x as number} cy={y as number} r={r as number} fill={c as string} fillOpacity="0.9" />
      ))}
    </svg>
  );
}
