import DOMPurify from "dompurify";
import { marked } from "marked";
import { useEffect, useRef } from "react";
import { api, fileUrl } from "../lib/api";
import { t } from "../i18n";
import { resolveLocal, slugOf } from "../lib/markdownLinks";

// Loaded on demand (`React.lazy` in Preview.tsx): marked and DOMPurify stay
// out of the window's first bundle.

/** YAML front matter shown as a code block instead of a rule and a paragraph. */
function withFrontMatter(text: string): string {
  const m = /^---\r?\n([\s\S]*?)\r?\n---\r?\n/.exec(text);
  return m ? `\`\`\`yaml\n${m[1]}\n\`\`\`\n${text.slice(m[0].length)}` : text;
}

/** The document as safe nodes: sanitized, local pictures through the asset protocol, remote ones as links, headings with anchors. */
function render(text: string, dir: string): DocumentFragment {
  const html = marked.parse(withFrontMatter(text), { async: false, gfm: true }) as string;
  const doc = DOMPurify.sanitize(html, {
    RETURN_DOM_FRAGMENT: true,
    FORBID_TAGS: ["style", "form", "iframe", "object", "embed", "link", "meta", "base", "button", "textarea", "select"],
    FORBID_ATTR: ["style"],
  });
  for (const img of [...doc.querySelectorAll("img")]) {
    const src = img.getAttribute("src") ?? "";
    if (/^data:image\//i.test(src)) continue;
    const local = resolveLocal(dir, src);
    if (local) {
      img.setAttribute("src", fileUrl(local));
      img.setAttribute("loading", "lazy");
      continue;
    }
    // Remote pictures are not fetched (a tracker would learn the file was opened): a link to them instead.
    const link = document.createElement("a");
    link.setAttribute("href", src);
    link.className = "md-remote";
    link.textContent = t("views.markdown.remoteImage", { alt: img.getAttribute("alt") || src });
    img.replaceWith(link);
  }
  const seen = new Map<string, number>();
  for (const h of [...doc.querySelectorAll("h1, h2, h3, h4, h5, h6")]) {
    const base = slugOf(h.textContent ?? "");
    const n = seen.get(base) ?? 0;
    seen.set(base, n + 1);
    h.id = n === 0 ? base : `${base}-${n}`;
  }
  for (const a of [...doc.querySelectorAll("a[href]")]) a.setAttribute("title", a.getAttribute("href") ?? "");
  return doc;
}

/**
 * A Markdown file rendered (GitHub's flavor: tables, task lists,
 * strikethrough), sanitized. Links never navigate the window: web and mail
 * links open in the browser, `#anchors` scroll to their heading, and links to
 * files only show where they lead.
 */
export default function MarkdownView({ text, path, large }: { text: string; path: string; large: boolean }) {
  const box = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    el.replaceChildren(render(text, path.slice(0, path.lastIndexOf("/")) || "/"));
    el.scrollTop = 0;
  }, [text, path]);
  return (
    <div
      ref={box}
      className={`md scroll selectable h-full min-h-0 overflow-auto rounded-xl bg-black/25 ${large ? "md-large px-8 py-6" : "px-4 py-3"}`}
      onClick={(e) => {
        const a = (e.target as HTMLElement).closest("a");
        if (!a) return;
        e.preventDefault();
        const href = a.getAttribute("href") ?? "";
        if (/^(https?|mailto):/i.test(href)) void api.openUrl(href).catch(() => {});
        else if (href.startsWith("#") && box.current) {
          // Scrolled by hand: scrollIntoView would also move the containers around the preview.
          const panel = box.current;
          const id = decodeURIComponent(href.slice(1));
          const target = panel.querySelector(`[id="${CSS.escape(id)}"]`);
          if (target) panel.scrollTo({ top: panel.scrollTop + target.getBoundingClientRect().top - panel.getBoundingClientRect().top - 8, behavior: "smooth" });
        }
      }}
    />
  );
}
