// Paths and anchors of a rendered Markdown file, pure (tested in Node).

/** Where `src` points, from the folder of the Markdown file: a path on disk, or null for a remote or inline source. */
export function resolveLocal(dir: string, src: string): string | null {
  if (!src.trim()) return null;
  if (/^[a-z][a-z0-9+.-]*:/i.test(src) && !/^file:/i.test(src)) return null;
  if (src.startsWith("//") || src.startsWith("#")) return null;
  let path = src.replace(/^file:\/\//i, "").replace(/[?#].*$/, "");
  try {
    path = decodeURIComponent(path);
  } catch {
    // A stray % keeps the path as written.
  }
  const parts = path.startsWith("/") ? [] : dir.split("/").filter(Boolean);
  for (const p of path.split("/")) {
    if (p === "" || p === ".") continue;
    if (p === "..") parts.pop();
    else parts.push(p);
  }
  return `/${parts.join("/")}`;
}

/** A heading's anchor, as GitHub makes them: lowercase, punctuation dropped, spaces to dashes. */
export function slugOf(text: string): string {
  return text
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s_-]/gu, "")
    .replace(/\s/g, "-");
}
