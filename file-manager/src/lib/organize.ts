import type { Entry } from "./api";
import { family, kindLabel, type Family } from "./kinds";
import { language, t } from "../i18n";

/** What the items are ordered by (the list's columns, and the Sort By menu). */
export type SortKey = "name" | "kind" | "modified" | "created" | "size";
export interface Sort {
  key: SortKey;
  asc: boolean;
}

/** What the items are grouped by (the Group By menu); the column view never groups. */
export type GroupKey = "none" | SortKey;

/** A sort key's name, in the app's language (the Sort By and Group By menus). */
export const sortLabel = (key: SortKey): string => t(`organize.sort.${key}`);

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

/** Folders stay on top; then the chosen key, name as the tie-breaker. */
export function sortEntries(entries: Entry[], sort: Sort): Entry[] {
  const dir = sort.asc ? 1 : -1;
  const by = (a: Entry, b: Entry): number => {
    switch (sort.key) {
      case "modified":
        return (a.modified ?? 0) - (b.modified ?? 0);
      case "created":
        return (a.created ?? 0) - (b.created ?? 0);
      case "size":
        return a.size - b.size;
      case "kind":
        return collator.compare(kindLabel(a), kindLabel(b));
      default:
        return collator.compare(a.name, b.name);
    }
  };
  return [...entries].sort((a, b) => Number(b.is_dir) - Number(a.is_dir) || dir * by(a, b) || collator.compare(a.name, b.name));
}

export interface Group {
  key: string;
  /** The section's heading; empty when nothing is grouped. */
  label: string;
  entries: Entry[];
}

/**
 * A group an item falls in: `rank` orders the groups in their natural
 * direction (names A to Z, kinds alphabetically, dates and sizes newest and
 * largest first); `pin` keeps a group first (-1, folders) or last (1, the
 * unknown) whichever way the groups run.
 */
interface Bucket {
  key: string;
  label: string;
  rank: number | string;
  pin: -1 | 0 | 1;
}

const kindGroup = (fam: Family): string => t(`organize.kind.${fam}`);

function nameBucket(entry: Entry): Bucket {
  const first = entry.name.normalize("NFD").charAt(0).toUpperCase();
  if (/[0-9]/.test(first)) return { key: "0-9", label: "0–9", rank: " ", pin: 0 };
  if (/[A-Z]/.test(first)) return { key: first, label: first, rank: first, pin: 0 };
  return { key: "other", label: t("organize.name.other"), rank: "", pin: 1 };
}

function kindBucket(entry: Entry): Bucket {
  const fam = family(entry);
  const label = kindGroup(fam);
  return { key: fam, label, rank: label, pin: fam === "folder" ? -1 : fam === "other" ? 1 : 0 };
}

const DAY = 86_400_000;
/** A month's name as a heading ("March", "Março"), in the app's language. */
function monthName(month: number): string {
  const name = new Date(2000, month, 1).toLocaleString(language(), { month: "long" });
  return name.charAt(0).toLocaleUpperCase(language()) + name.slice(1);
}

/** Finder's date sections: Today, Yesterday, Previous 7 Days, Previous 30 Days, then months of this year, then years. */
function dateBucket(secs: number | null, now: Date): Bucket {
  if (secs == null) return { key: "unknown", label: t("organize.date.unknown"), rank: 0, pin: 1 };
  const d = new Date(secs * 1000);
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const at = d.getTime();
  // Negative ranks put the newest first in ascending rank order.
  if (at >= today) return { key: "today", label: t("organize.date.today"), rank: -1e9, pin: 0 };
  if (at >= today - DAY) return { key: "yesterday", label: t("organize.date.yesterday"), rank: -1e9 + 1, pin: 0 };
  if (at >= today - 7 * DAY) return { key: "7d", label: t("organize.date.week"), rank: -1e9 + 2, pin: 0 };
  if (at >= today - 30 * DAY) return { key: "30d", label: t("organize.date.month"), rank: -1e9 + 3, pin: 0 };
  if (d.getFullYear() === now.getFullYear()) return { key: `m${d.getMonth()}`, label: monthName(d.getMonth()), rank: -1e6 - d.getMonth(), pin: 0 };
  return { key: `y${d.getFullYear()}`, label: String(d.getFullYear()), rank: -d.getFullYear(), pin: 0 };
}

const SIZES: [number, "gb" | "hundredMb" | "mb" | "hundredKb" | "small"][] = [
  [1e9, "gb"],
  [1e8, "hundredMb"],
  [1e6, "mb"],
  [1e5, "hundredKb"],
  [0, "small"],
];

function sizeBucket(entry: Entry): Bucket {
  // A folder's size is not measured here.
  if (entry.is_dir) return { key: "folders", label: t("organize.size.folders"), rank: 0, pin: -1 };
  const i = SIZES.findIndex(([min]) => entry.size >= min);
  return { key: `s${i}`, label: t(`organize.size.${SIZES[i][1]}`), rank: i, pin: 0 };
}

function bucketOf(entry: Entry, group: Exclude<GroupKey, "none">, now: Date): Bucket {
  switch (group) {
    case "name":
      return nameBucket(entry);
    case "kind":
      return kindBucket(entry);
    case "modified":
      return dateBucket(entry.modified, now);
    case "created":
      return dateBucket(entry.created, now);
    case "size":
      return sizeBucket(entry);
  }
}

/** The direction each grouping runs in when nothing says otherwise. */
const NATURAL_ASC: Record<SortKey, boolean> = { name: true, kind: true, modified: false, created: false, size: false };

/**
 * The sorted items in sections, as Finder's Group By draws them. Each
 * section keeps the items' order; the sections run in their natural order,
 * or the other way when the items are sorted by the same key the other way
 * (folders stay first and the unknown last).
 */
export function groupEntries(sorted: Entry[], group: GroupKey, sort: Sort, now = new Date()): Group[] {
  if (group === "none") return [{ key: "all", label: "", entries: sorted }];
  const groups = new Map<string, { bucket: Bucket; entries: Entry[] }>();
  for (const e of sorted) {
    const bucket = bucketOf(e, group, now);
    const g = groups.get(bucket.key);
    if (g) g.entries.push(e);
    else groups.set(bucket.key, { bucket, entries: [e] });
  }
  const flip = sort.key === group && sort.asc !== NATURAL_ASC[group] ? -1 : 1;
  const cmp = (a: Bucket, b: Bucket) => {
    if (a.pin !== b.pin) return a.pin - b.pin;
    const by = typeof a.rank === "number" && typeof b.rank === "number" ? a.rank - b.rank : collator.compare(String(a.rank), String(b.rank));
    return flip * by;
  };
  return [...groups.values()]
    .sort((a, b) => cmp(a.bucket, b.bucket))
    .map(({ bucket, entries }) => ({ key: bucket.key, label: bucket.label, entries }));
}
