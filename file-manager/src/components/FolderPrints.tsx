import { Play } from "lucide-react";
import { useState } from "react";
import { frameWidth, printLayout } from "../lib/folderPreview";
import type { Shown } from "../lib/useFolderPreview";

const VIDEO = ["mp4", "m4v", "mov"];

/**
 * The previews laid over a folder's icon, as prints with a white frame,
 * slightly turned (Dolphin's look). They sit inside the icon's square (the
 * tile keeps its height), take no pointer (clicks, drags and drops go to
 * the folder) and fade in the first time they appear.
 */
export function FolderPrints({ prints, size, fresh }: { prints: Shown[]; size: number; fresh: boolean }) {
  // One print keeps its picture's proportions (known once it loads).
  const [aspect, setAspect] = useState(4 / 3);
  if (prints.length === 0) return null;
  const layout = printLayout(prints.length, aspect);
  const frame = frameWidth(size);
  // The first pick goes on top of the stack (the layouts list the top one last).
  const order = prints.length === 3 ? [1, 2, 0] : prints.length === 2 ? [1, 0] : [0, 1, 2, 3];
  const top = Math.max(...layout.map((b) => b.z));
  // The play mark grows with the icon (16 px at 96-128, 21 px at 192).
  const mark = Math.max(16, Math.round(size * 0.11));
  return (
    <span aria-hidden className="pointer-events-none absolute inset-0 isolate" data-folder-prints={prints.length}>
      {layout.map((b, i) => {
        const p = prints[order[i]];
        return (
          <span
            key={p.key}
            data-print
            className={`folder-print absolute ${fresh ? "print-in" : ""}`}
            style={{
              left: (b.cx - b.w / 2) * size,
              top: (b.cy - b.h / 2) * size,
              width: b.w * size,
              height: b.h * size,
              transform: `rotate(${b.rotate}deg)`,
              zIndex: b.z,
              padding: frame,
              animationDelay: fresh ? `${b.z * 50}ms` : undefined,
            }}
          >
            <img
              src={p.url}
              alt=""
              draggable={false}
              onLoad={
                prints.length === 1
                  ? (e) => {
                      const img = e.currentTarget;
                      if (img.naturalWidth > 0 && img.naturalHeight > 0) setAspect(img.naturalWidth / img.naturalHeight);
                    }
                  : undefined
              }
              className={`block h-full w-full rounded-[1px] ${prints.length === 1 ? "object-contain" : "object-cover"}`}
            />
            {VIDEO.includes(p.entry.ext) && size >= 96 && b.z === top && (
              <span className="absolute inset-0 grid place-items-center">
                <span className="grid place-items-center rounded-full bg-black/55" style={{ width: mark, height: mark }}>
                  <Play className="ml-px text-paper" style={{ width: mark / 2, height: mark / 2 }} fill="currentColor" strokeWidth={0} />
                </span>
              </span>
            )}
          </span>
        );
      })}
    </span>
  );
}
