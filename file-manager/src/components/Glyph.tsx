import { ArrowUpRight } from "lucide-react";
import { useId } from "react";
import type { Entry } from "../lib/api";
import { iconFor } from "../lib/kinds";
import type { Shown } from "../lib/useFolderPreview";
import { FolderPrints } from "./FolderPrints";

/** A folder in the accent gradient (the logo's colors): a darker back panel
 * with its tab, and a glassy front panel with a sheen and a lit top edge. */
function FolderGlyph({ size }: { size: number }) {
  const id = useId();
  return (
    <svg width={size} height={size * (52 / 64)} viewBox="0 0 64 52" aria-hidden>
      <defs>
        <linearGradient id={`${id}b`} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0%" style={{ stopColor: "var(--a1)" }} />
          <stop offset="100%" style={{ stopColor: "var(--a2)" }} />
        </linearGradient>
        <linearGradient id={`${id}f`} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0%" style={{ stopColor: "var(--a1)" }} />
          <stop offset="100%" style={{ stopColor: "var(--a3)" }} />
        </linearGradient>
        <linearGradient id={`${id}s`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#fff" stopOpacity="0.32" />
          <stop offset="60%" stopColor="#fff" stopOpacity="0" />
        </linearGradient>
      </defs>
      <path d="M7 3h15.2a4 4 0 0 1 2.9 1.2L29 8h28a4 4 0 0 1 4 4v34a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4V7a4 4 0 0 1 4-4z" fill={`url(#${id}b)`} />
      <path d="M7 3h15.2a4 4 0 0 1 2.9 1.2L29 8h28a4 4 0 0 1 4 4v34a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4V7a4 4 0 0 1 4-4z" fill="#0b0c24" fillOpacity="0.3" />
      <path d="M3 19a4 4 0 0 1 4-4h50a4 4 0 0 1 4 4v27a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4z" fill={`url(#${id}f)`} />
      <path d="M3 19a4 4 0 0 1 4-4h50a4 4 0 0 1 4 4v27a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4z" fill={`url(#${id}s)`} />
      <path d="M7.5 15.6h49" stroke="#fff" strokeOpacity="0.55" strokeWidth="0.8" strokeLinecap="round" />
    </svg>
  );
}

/** A glass page with a folded corner, the kind's icon in its tint and, when
 * there is room, the extension below it (as Finder labels documents). */
function DocGlyph({ entry, size }: { entry: Entry; size: number }) {
  const id = useId();
  const { Icon, color } = iconFor(entry);
  const height = size;
  const width = size * (50 / 64);
  const label = entry.ext.slice(0, 4).toUpperCase();
  return (
    <div className="relative" style={{ width, height }}>
      <svg width={width} height={height} viewBox="0 0 50 64" className="absolute inset-0" aria-hidden>
        <defs>
          <linearGradient id={`${id}p`} x1="0" y1="0" x2="0.4" y2="1">
            <stop offset="0%" stopColor="#fff" stopOpacity="0.2" />
            <stop offset="100%" stopColor="#fff" stopOpacity="0.07" />
          </linearGradient>
          <linearGradient id={`${id}t`} x1="0" y1="0" x2="1" y2="1">
            <stop offset="0%" stopColor={color} stopOpacity="0.24" />
            <stop offset="100%" stopColor={color} stopOpacity="0.03" />
          </linearGradient>
        </defs>
        <path d="M6 1.5h26.5l16 16V58a4.5 4.5 0 0 1-4.5 4.5H6A4.5 4.5 0 0 1 1.5 58V6A4.5 4.5 0 0 1 6 1.5z" fill={`url(#${id}p)`} />
        <path
          d="M6 1.5h26.5l16 16V58a4.5 4.5 0 0 1-4.5 4.5H6A4.5 4.5 0 0 1 1.5 58V6A4.5 4.5 0 0 1 6 1.5z"
          fill={`url(#${id}t)`}
          stroke="#fff"
          strokeOpacity="0.3"
          strokeWidth="1"
        />
        <path d="M32.5 1.5V13a4.5 4.5 0 0 0 4.5 4.5h11.5" fill="#fff" fillOpacity="0.16" stroke="#fff" strokeOpacity="0.3" strokeWidth="1" strokeLinejoin="round" />
      </svg>
      <div className="absolute inset-x-0 flex flex-col items-center" style={{ top: height * 0.34 }}>
        <Icon style={{ color, width: width * 0.44, height: width * 0.44 }} strokeWidth={1.7} />
        {label && size >= 56 && (
          <span className="mt-[0.2em] font-semibold tracking-[0.06em]" style={{ color, fontSize: Math.max(8, size * 0.125) }}>
            {label}
          </span>
        )}
      </div>
    </div>
  );
}

/** The large icon of an entry; links get Finder's corner arrow. */
/** A file's or folder's icon; a folder may carry previews of what it holds
 * (`prints`, laid over it; `fresh` when they appear for the first time). */
export function EntryGlyph({ entry, size, prints, fresh = false }: { entry: Entry; size: number; prints?: Shown[]; fresh?: boolean }) {
  return (
    <div className="relative grid place-items-center" style={{ width: size, height: size }}>
      {entry.is_dir ? <FolderGlyph size={size} /> : <DocGlyph entry={entry} size={size} />}
      {entry.is_dir && prints && prints.length > 0 && <FolderPrints prints={prints} size={size} fresh={fresh} />}
      {entry.kind === "symlink" && (
        <span
          className="absolute bottom-[6%] left-[4%] z-10 grid place-items-center rounded-full bg-paper text-[#20234a] shadow"
          style={{ width: size * 0.26, height: size * 0.26 }}
        >
          <ArrowUpRight style={{ width: size * 0.2, height: size * 0.2 }} strokeWidth={2.6} />
        </span>
      )}
    </div>
  );
}
