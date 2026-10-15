import { FolderGit2 } from "lucide-react";
import { useId } from "react";

/** The folder-with-a-branch mark, stroked with the accent gradient. */
export function BrandMark({ size = 42 }: { size?: number }) {
  const id = useId();
  return (
    // userSpaceOnUse: the branch's vertical line has no width, so a gradient
    // relative to the shape's bounding box would not paint it.
    <FolderGit2 size={size} color={`url(#${id})`} strokeWidth={1.9} aria-hidden>
      <defs>
        <linearGradient id={id} gradientUnits="userSpaceOnUse" x1="2" y1="3" x2="22" y2="22">
          <stop offset="0%" style={{ stopColor: "var(--a1)" }} />
          <stop offset="100%" style={{ stopColor: "var(--a3)" }} />
        </linearGradient>
      </defs>
    </FolderGit2>
  );
}

/** Stacked logo: mark above the letter-spaced wordmark, in the accent colors. */
export function BrandLogo({ caption }: { caption?: string }) {
  return (
    <div className="flex flex-col items-center text-center">
      <BrandMark />
      {/* The trailing letter spacing is offset so the wordmark stays centered. */}
      <p
        className="accent-text mt-2.5 text-[0.82rem] font-semibold uppercase leading-none tracking-[0.3em]"
        style={{ marginRight: "-0.3em" }}
      >
        git-drive
      </p>
      {caption && <p className="mt-1.5 text-[0.68rem] text-dim">{caption}</p>}
    </div>
  );
}
