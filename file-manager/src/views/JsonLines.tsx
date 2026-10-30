import { useMemo, useRef } from "react";
import { highlightJsonLine } from "../lib/jsonView";
import { offsetsOf, useVirtual } from "../lib/virtual";

/**
 * Formatted JSON, one line per row, colored per line. Only the rows on screen
 * are drawn (a 20 MB file formats to a million lines); lines never wrap, so
 * every row has the same height and the box scrolls sideways for long ones.
 */
export function JsonLines({ lines, large }: { lines: string[]; large: boolean }) {
  const box = useRef<HTMLDivElement | null>(null);
  const height = large ? 20 : 16;
  const pad = large ? 16 : 12;
  const offsets = useMemo(() => offsetsOf(new Array<number>(lines.length).fill(height)), [lines.length, height]);
  const v = useVirtual(box, offsets, pad);
  const gutter = `${String(lines.length).length + 1}ch`;
  return (
    <div
      ref={box}
      className={`json scroll selectable h-full min-h-0 overflow-auto rounded-xl bg-black/25 font-mono ${large ? "text-[0.8rem]" : "text-[0.68rem]"}`}
      style={{ padding: `${pad}px ${pad}px` }}
    >
      <div className="min-w-max">
        <div style={{ height: v.before }} />
        {lines.slice(v.start, v.end).map((line, k) => {
          const n = v.start + k;
          return (
            <div key={n} className="flex whitespace-pre" style={{ height, lineHeight: `${height}px` }}>
              <span className="json-gutter flex-none select-none pr-3 text-right" style={{ width: gutter }}>
                {n + 1}
              </span>
              <span>
                {highlightJsonLine(line).map((p, i) => (
                  <span key={i} className={p.part === "plain" ? undefined : `json-${p.part}`}>
                    {p.text}
                  </span>
                ))}
              </span>
            </div>
          );
        })}
        <div style={{ height: v.after }} />
      </div>
    </div>
  );
}
