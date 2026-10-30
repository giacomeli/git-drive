// JSON laid out for reading: the file's own tokens re-indented, never parsed
// into numbers and back (a 19-digit id keeps every digit, `1.0` stays `1.0`,
// escapes stay as written), and each line colored on its own.

const NUMBER = /-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/y;
const LITERAL = /true|false|null/y;
const HEX4 = /^[0-9a-fA-F]{4}$/;

/** The end (exclusive) of the string starting at `at` (a quote), or -1 when it is not valid JSON. */
function stringEnd(text: string, at: number): number {
  for (let i = at + 1; i < text.length; i++) {
    const c = text.charCodeAt(i);
    if (c === 0x22) return i + 1;
    if (c < 0x20) return -1;
    if (c === 0x5c) {
      const e = text[i + 1];
      if (e === "u") {
        if (!HEX4.test(text.slice(i + 2, i + 6))) return -1;
        i += 5;
      } else if (e !== undefined && '"\\/bfnrt'.includes(e)) i += 1;
      else return -1;
    }
  }
  return -1;
}

const isSpace = (c: number) => c === 0x20 || c === 0x0a || c === 0x0d || c === 0x09;

/**
 * `text` re-indented by `indent` spaces, one line per entry, or null when it
 * is not strict JSON (comments, trailing commas, single quotes, a second
 * value...). Empty objects and arrays stay on their line (`{}`, `[]`).
 */
export function formatJson(text: string, indent = 2): string[] | null {
  const lines: string[] = [];
  const stack: ("{" | "[")[] = [];
  let cur = "";
  let i = text.charCodeAt(0) === 0xfeff ? 1 : 0;
  // What may come next (set by `valueDone` too, which the compiler does not follow).
  let expect = "value" as "value" | "key" | "colon" | "commaOrClose" | "end";
  const pad = () => " ".repeat(stack.length * indent);
  const breakLine = () => {
    lines.push(cur);
    cur = pad();
  };
  const skip = () => {
    while (i < text.length && isSpace(text.charCodeAt(i))) i++;
  };
  const valueDone = () => {
    expect = stack.length === 0 ? "end" : "commaOrClose";
  };
  for (;;) {
    skip();
    if (i >= text.length) break;
    const c = text[i];
    if (expect === "end") return null;
    if (c === "{" || c === "[") {
      if (expect !== "value") return null;
      const close = c === "{" ? "}" : "]";
      i++;
      skip();
      if (text[i] === close) {
        cur += c + close;
        i++;
        valueDone();
        continue;
      }
      cur += c;
      stack.push(c);
      breakLine();
      expect = c === "{" ? "key" : "value";
      continue;
    }
    if (c === "}" || c === "]") {
      const open = stack[stack.length - 1];
      if (expect !== "commaOrClose" || (c === "}") !== (open === "{")) return null;
      stack.pop();
      i++;
      breakLine();
      cur += c;
      valueDone();
      continue;
    }
    if (c === ",") {
      if (expect !== "commaOrClose") return null;
      cur += ",";
      i++;
      breakLine();
      expect = stack[stack.length - 1] === "{" ? "key" : "value";
      continue;
    }
    if (c === ":") {
      if (expect !== "colon") return null;
      cur += ": ";
      i++;
      expect = "value";
      continue;
    }
    if (c === '"') {
      const end = stringEnd(text, i);
      if (end < 0) return null;
      const s = text.slice(i, end);
      i = end;
      if (expect === "key") {
        cur += s;
        expect = "colon";
      } else if (expect === "value") {
        cur += s;
        valueDone();
      } else return null;
      continue;
    }
    if (expect !== "value") return null;
    NUMBER.lastIndex = i;
    LITERAL.lastIndex = i;
    const m = NUMBER.exec(text) ?? LITERAL.exec(text);
    if (!m) return null;
    // `truefalse` or `12abc`: the token must end there.
    const after = text[i + m[0].length];
    if (after !== undefined && !isSpace(after.charCodeAt(0)) && !",:]}".includes(after)) return null;
    cur += m[0];
    i += m[0].length;
    valueDone();
  }
  if (expect !== "end") return null;
  lines.push(cur);
  return lines;
}

export type JsonPart = "key" | "string" | "number" | "literal" | "punct" | "plain";

/** One formatted line in colored parts (a string followed by a colon is a key). */
export function highlightJsonLine(line: string): { text: string; part: JsonPart }[] {
  const out: { text: string; part: JsonPart }[] = [];
  let i = 0;
  while (i < line.length) {
    const c = line[i];
    if (c === " ") {
      let j = i;
      while (line[j] === " ") j++;
      out.push({ text: line.slice(i, j), part: "plain" });
      i = j;
    } else if (c === '"') {
      const end = stringEnd(line, i);
      const j = end < 0 ? line.length : end;
      let k = j;
      while (line[k] === " ") k++;
      out.push({ text: line.slice(i, j), part: line[k] === ":" ? "key" : "string" });
      i = j;
    } else if ("{}[]:,".includes(c)) {
      out.push({ text: c, part: "punct" });
      i++;
    } else {
      let j = i;
      while (j < line.length && !" ,:]}".includes(line[j])) j++;
      const word = line.slice(i, j);
      out.push({ text: word, part: /^(true|false|null)$/.test(word) ? "literal" : "number" });
      i = j;
    }
  }
  return out;
}
