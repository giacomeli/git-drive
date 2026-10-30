/** The nine permission bits, by who and what: owner, group, everyone x read, write, execute. */
export const WHO = ["owner", "group", "everyone"] as const;
export const WHAT = ["read", "write", "execute"] as const;
export type Who = (typeof WHO)[number];
export type What = (typeof WHAT)[number];

/** The special bits: set user id, set group id, sticky. */
export const SPECIAL = { setuid: 0o4000, setgid: 0o2000, sticky: 0o1000 } as const;

export const bitOf = (who: Who, what: What) => 1 << ((2 - WHO.indexOf(who)) * 3 + (2 - WHAT.indexOf(what)));

/** "rwxr-xr--", with s/S and t/T where the special bits are, as `ls -l` writes them. */
export function symbolic(mode: number): string {
  const out: string[] = [];
  for (const who of WHO) {
    for (const what of WHAT) out.push(mode & bitOf(who, what) ? what[0].replace("e", "x") : "-");
  }
  const special = (i: number, bit: number, on: string) => {
    if (mode & bit) out[i] = out[i] === "x" ? on : on.toUpperCase();
  };
  special(2, SPECIAL.setuid, "s");
  special(5, SPECIAL.setgid, "s");
  special(8, SPECIAL.sticky, "t");
  return out.join("");
}

/** "0644", or "4755" with special bits. */
export const octal = (mode: number) => (mode & 0o7777).toString(8).padStart(4, "0");

/** A typed octal mode ("644", "0755", "4755"); null when it is not one. */
export function parseOctal(text: string): number | null {
  const t = text.trim();
  if (!/^[0-7]{3,4}$/.test(t)) return null;
  return parseInt(t, 8);
}

/** A bit across several items: on in all, in none, or mixed. */
export type Tri = "on" | "off" | "mixed";

export function triOf(modes: number[], bit: number): Tri {
  const on = modes.filter((m) => m & bit).length;
  return on === 0 ? "off" : on === modes.length ? "on" : "mixed";
}

/** The change to send: bits to set and to clear, from what the user chose
 * per bit (a mixed bit left alone stays as each item has it). */
export function changeOf(chosen: Map<number, Tri>): { set: number; clear: number } {
  let set = 0;
  let clear = 0;
  for (const [bit, tri] of chosen) {
    if (tri === "on") set |= bit;
    else if (tri === "off") clear |= bit;
  }
  return { set, clear };
}
