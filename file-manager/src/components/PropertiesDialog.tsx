import { Check, Copy, MapPin, Minus, Navigation, ShieldAlert, Star, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { api, baseName, errorText, parentOf, type Boot, type Entry, type ImageMeta, type ItemInfo, type MetaEdit } from "../lib/api";
import { copyText } from "../lib/clipboard";
import { dms } from "../lib/geo";
import { bitOf, changeOf, octal, parseOctal, SPECIAL, symbolic, triOf, WHAT, WHO, type Tri } from "../lib/perms";
import { newTaskId } from "../lib/tasks";
import { useThumbnail } from "../lib/thumbs";
import { bytesText } from "../ui/format";
import { useToast } from "../ui/ui";
import { dateTime, num, t, type Key } from "../i18n";
import { EntryGlyph } from "./Glyph";
import { MapDialog, MiniMap } from "./MapView";

/** Extensions counted as images when several items are chosen (the backend checks each file's bytes). */
const IMAGE_EXT = ["jpg", "jpeg", "png", "heic", "heif", "avif", "webp", "tif", "tiff", "dng", "cr2", "cr3", "nef", "nrw", "arw", "orf", "rw2", "raf", "pef", "srw"];
const isImageName = (name: string) => IMAGE_EXT.includes(name.split(".").pop()?.toLowerCase() ?? "");

const when = (secs: number | null) => (secs == null ? "--" : dateTime(secs, { dateStyle: "medium", timeStyle: "medium" }));

/** The item's icon, or its picture once drawn. */
function HeaderIcon({ entry }: { entry: Entry }) {
  const ref = useRef<HTMLSpanElement | null>(null);
  const thumb = useThumbnail(entry, 48, ref);
  return (
    <span ref={ref} className="grid h-12 w-12 flex-none place-items-center">
      {thumb ? <img src={thumb} alt="" className="max-h-12 max-w-12 rounded-md object-contain shadow-[0_6px_16px_-6px_rgb(0_0_0/0.7)] ring-1 ring-white/20" /> : <EntryGlyph entry={entry} size={44} />}
    </span>
  );
}

function Card({ title, action, children }: { title: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="mb-4 last:mb-0">
      <div className="mb-2 flex items-center gap-2 px-1">
        <p className="label flex-1">{title}</p>
        {action}
      </div>
      <div className="rounded-2xl bg-white/[0.04] p-4 ring-1 ring-white/8">{children}</div>
    </section>
  );
}

function Rows({ children }: { children: ReactNode }) {
  return <dl className="grid grid-cols-[7.5rem_minmax(0,1fr)] gap-x-3 gap-y-2 text-[0.78rem]">{children}</dl>;
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="text-muted">{label}</dt>
      <dd className="selectable min-w-0 break-words text-ink/90">{children}</dd>
    </>
  );
}

/** A checkbox of three states (several items may differ). */
function TriBox({ value, onChange, disabled, label }: { value: Tri; onChange: (v: Tri) => void; disabled?: boolean; label: string }) {
  const on = value === "on";
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={value === "mixed" ? "mixed" : on}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(on ? "off" : "on")}
      className="grid h-5 w-5 place-items-center rounded-md ring-1 ring-white/20 transition-colors disabled:opacity-40"
      style={on || value === "mixed" ? { background: "var(--accent)", boxShadow: "none" } : undefined}
    >
      {on && <Check className="h-3.5 w-3.5 text-paper" strokeWidth={3} />}
      {value === "mixed" && <Minus className="h-3.5 w-3.5 text-paper" strokeWidth={3} />}
    </button>
  );
}

/** Who may read, write and run the items; applied with the button (through folders when asked). */
function Permissions({ infos, inDrive, onApply }: { infos: ItemInfo[]; inDrive: boolean; onApply: (change: { set: number; clear: number }, recursive: boolean) => Promise<boolean> }) {
  const modes = infos.map((i) => i.mode);
  const bits = useMemo(() => [...WHO.flatMap((w) => WHAT.map((x) => bitOf(w, x))), SPECIAL.setuid, SPECIAL.setgid, SPECIAL.sticky], []);
  const initial = () => new Map(bits.map((b) => [b, triOf(modes, b)] as [number, Tri]));
  const [chosen, setChosen] = useState(initial);
  const [recursive, setRecursive] = useState(false);
  const [typed, setTyped] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // New items (after a save) start the choice over.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(() => setChosen(initial()), [modes.join(",")]);
  const changed = [...chosen].some(([b, t]) => t !== triOf(modes, b));
  const mine = infos.every((i) => i.mine);
  const links = infos.every((i) => i.kind === "symlink");
  const folders = infos.some((i) => i.kind === "dir");
  const single = infos.length === 1 ? infos[0] : null;
  const allKnown = [...chosen.values()].every((t) => t !== "mixed");
  const shownMode = allKnown ? [...chosen].reduce((m, [b, t]) => (t === "on" ? m | b : m), 0) : null;
  const set = (bit: number, t: Tri) => setChosen((c) => new Map(c).set(bit, t));
  const whoLabel = (w: (typeof WHO)[number]) =>
    w === "owner"
      ? single?.owner
        ? t("properties.ownerNamed", { name: single.owner })
        : t("properties.owner")
      : w === "group"
        ? single?.group
          ? t("properties.groupNamed", { name: single.group })
          : t("properties.group")
        : t("properties.everyone");
  const whatLabel = (x: (typeof WHAT)[number]) => t(`properties.${x}` as Key);
  const apply = async () => {
    setBusy(true);
    const ok = await onApply(changeOf(chosen), recursive && folders);
    setBusy(false);
    if (ok) setTyped(null);
  };
  return (
    <Card title={t("properties.permissions")}>
      {links ? (
        <p className="text-[0.76rem] text-muted">{t("properties.linkNoPermissions")}</p>
      ) : (
        <>
          <div className="grid grid-cols-[minmax(0,1fr)_4.5rem_4.5rem_4.5rem] items-center gap-y-2.5 text-[0.78rem]" role="group" aria-label={t("properties.permissions")}>
            <span />
            {WHAT.map((x) => (
              <span key={x} className="text-center text-[0.7rem] text-dim">
                {whatLabel(x)}
              </span>
            ))}
            {WHO.map((w) => (
              <div key={w} className="contents">
                <span className="truncate text-ink/90">{whoLabel(w)}</span>
                {WHAT.map((x) => (
                  <span key={x} className="grid place-items-center">
                    <TriBox label={`${whoLabel(w)} ${whatLabel(x)}`} value={chosen.get(bitOf(w, x))!} disabled={!mine} onChange={(t) => set(bitOf(w, x), t)} />
                  </span>
                ))}
              </div>
            ))}
          </div>
          <div className="mt-3 flex flex-wrap items-center gap-x-5 gap-y-2 border-t border-white/6 pt-3 text-[0.74rem] text-muted">
            {(
              [
                [t("properties.setuid"), SPECIAL.setuid],
                [t("properties.setgid"), SPECIAL.setgid],
                [t("properties.sticky"), SPECIAL.sticky],
              ] as const
            ).map(([label, bit]) => (
              <label key={label} className="flex items-center gap-2">
                <TriBox label={label} value={chosen.get(bit)!} disabled={!mine} onChange={(t) => set(bit, t)} />
                {label}
              </label>
            ))}
            <span className="ml-auto flex items-center gap-2 font-mono text-[0.74rem]">
              {shownMode != null && <span className="text-dim">{symbolic(shownMode)}</span>}
              <input
                aria-label={t("properties.octal")}
                className="field !h-7 w-[4.5rem] !px-2 text-center font-mono !text-[0.74rem]"
                value={typed ?? (shownMode != null ? octal(shownMode) : "")}
                placeholder={t("properties.mixed")}
                disabled={!mine}
                onChange={(e) => {
                  setTyped(e.target.value);
                  const m = parseOctal(e.target.value);
                  if (m != null) setChosen(new Map(bits.map((b) => [b, m & b ? "on" : "off"] as [number, Tri])));
                }}
              />
            </span>
          </div>
          <div className="mt-3 flex items-center gap-3">
            {folders && (
              <label className="flex items-center gap-2 text-[0.74rem] text-muted">
                <TriBox label={t("properties.recursive")} value={recursive ? "on" : "off"} disabled={!mine} onChange={(v) => setRecursive(v === "on")} />
                {t("properties.recursive")}
              </label>
            )}
            <span className="flex-1" />
            {changed && (
              <button className="btn btn-ghost !h-8 !text-[0.74rem]" onClick={() => setChosen(initial())}>
                {t("common.revert")}
              </button>
            )}
            <button className="btn btn-primary !h-8 !text-[0.74rem]" disabled={!mine || (!changed && !recursive) || busy} onClick={() => void apply()}>
              {t("common.apply")}
            </button>
          </div>
          {!mine && <p className="mt-2 text-[0.7rem] text-dim">{t("properties.onlyOwner")}</p>}
          {inDrive && <p className="mt-2 text-[0.7rem] text-dim">{t("properties.driveNote")}</p>}
        </>
      )}
    </Card>
  );
}

/** Keywords as chips: Return or a comma adds one, Backspace in the empty field takes the last back. */
function KeywordsField({ value, onChange, placeholder }: { value: string[]; onChange: (v: string[]) => void; placeholder: string }) {
  const [draft, setDraft] = useState("");
  const add = (text: string) => {
    const k = text.trim().replace(/,$/, "").trim();
    if (k && !value.some((v) => v.toLowerCase() === k.toLowerCase())) onChange([...value, k]);
    setDraft("");
  };
  return (
    <div className="field flex min-h-9 flex-wrap items-center gap-1.5 !h-auto !py-1.5">
      {value.map((k) => (
        <span key={k} className="flex items-center gap-1 rounded-full bg-white/10 py-0.5 pl-2.5 pr-1 text-[0.72rem]">
          {k}
          <button aria-label={t("properties.removeKeyword", { name: k })} onClick={() => onChange(value.filter((v) => v !== k))} className="grid h-4 w-4 place-items-center rounded-full hover:bg-white/15">
            <X className="h-3 w-3" />
          </button>
        </span>
      ))}
      <input
        aria-label={t("properties.keywords")}
        className="min-w-[6rem] flex-1 bg-transparent text-[0.78rem] outline-none"
        value={draft}
        placeholder={value.length === 0 ? placeholder : ""}
        onChange={(e) => (e.target.value.endsWith(",") ? add(e.target.value) : setDraft(e.target.value))}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            add(draft);
          } else if (e.key === "Backspace" && draft === "" && value.length > 0) onChange(value.slice(0, -1));
        }}
        onBlur={() => draft && add(draft)}
      />
    </div>
  );
}

/** Five stars; a click on the current one clears the rating. */
function Stars({ value, onChange, disabled }: { value: number; onChange: (v: number) => void; disabled?: boolean }) {
  const [hover, setHover] = useState(0);
  const shown = hover || value;
  return (
    <span className="flex items-center gap-0.5" role="radiogroup" aria-label={t("properties.rating")} onMouseLeave={() => setHover(0)}>
      {[1, 2, 3, 4, 5].map((n) => (
        <button
          key={n}
          type="button"
          role="radio"
          aria-checked={value === n}
          aria-label={t("properties.stars", { count: n })}
          disabled={disabled}
          onMouseEnter={() => setHover(n)}
          onClick={() => onChange(value === n ? 0 : n)}
          className="p-0.5 disabled:opacity-40"
        >
          <Star className="h-4 w-4" style={{ color: n <= shown ? "var(--color-amber)" : "rgb(255 255 255 / 0.25)" }} fill={n <= shown ? "currentColor" : "none"} />
        </button>
      ))}
    </span>
  );
}

const input = "field !h-8 w-full !text-[0.78rem]";

/** The description of one image, saved together. */
function Description({ meta, onSave }: { meta: ImageMeta; onSave: (edit: MetaEdit) => Promise<boolean> }) {
  const from = useCallback(
    (m: ImageMeta) => ({
      title: m.title ?? "",
      description: m.description ?? "",
      creator: m.creator ?? "",
      copyright: m.copyright ?? "",
      keywords: m.keywords,
      rating: m.rating ?? 0,
      taken: m.taken ?? "",
    }),
    [],
  );
  const [f, setF] = useState(() => from(meta));
  const [busy, setBusy] = useState(false);
  useEffect(() => setF(from(meta)), [meta, from]);
  const base = from(meta);
  const edit: MetaEdit = {};
  if (f.title !== base.title) edit.title = f.title;
  if (f.description !== base.description) edit.description = f.description;
  if (f.creator !== base.creator) edit.creator = f.creator;
  if (f.copyright !== base.copyright) edit.copyright = f.copyright;
  if (f.keywords.join("\n") !== base.keywords.join("\n")) edit.keywords = f.keywords;
  if (f.rating !== base.rating) edit.rating = f.rating;
  if (f.taken !== base.taken && f.taken) edit.taken = f.taken;
  const dirty = Object.keys(edit).length > 0;
  const off = meta.editing.descriptive === "none";
  const save = async () => {
    setBusy(true);
    await onSave(edit);
    setBusy(false);
  };
  return (
    <Card
      title={t("properties.description")}
      action={
        dirty && (
          <span className="flex gap-2">
            <button className="btn btn-ghost !h-7 !px-3 !text-[0.72rem]" onClick={() => setF(from(meta))}>
              {t("common.revert")}
            </button>
            <button className="btn btn-primary !h-7 !px-3 !text-[0.72rem]" disabled={busy} onClick={() => void save()}>
              {t("common.save")}
            </button>
          </span>
        )
      }
    >
      <div className="grid grid-cols-[7.5rem_minmax(0,1fr)] items-center gap-x-3 gap-y-2.5 text-[0.78rem]">
        <label className="text-muted">{t("properties.title")}</label>
        <input aria-label={t("properties.title")} className={input} disabled={off} value={f.title} onChange={(e) => setF({ ...f, title: e.target.value })} />
        <label className="self-start pt-2 text-muted">{t("properties.description")}</label>
        <textarea
          aria-label={t("properties.description")}
          className="field min-h-[4rem] w-full resize-y !py-2 !text-[0.78rem]"
          disabled={off}
          value={f.description}
          onChange={(e) => setF({ ...f, description: e.target.value })}
        />
        <label className="self-start pt-2 text-muted">{t("properties.keywords")}</label>
        <KeywordsField value={f.keywords} onChange={(keywords) => setF({ ...f, keywords })} placeholder={t("properties.keywordsPlaceholder")} />
        <label className="text-muted">{t("properties.creator")}</label>
        <input aria-label={t("properties.creator")} className={input} disabled={off} value={f.creator} onChange={(e) => setF({ ...f, creator: e.target.value })} />
        <label className="text-muted">{t("properties.copyright")}</label>
        <input aria-label={t("properties.copyright")} className={input} disabled={off} value={f.copyright} onChange={(e) => setF({ ...f, copyright: e.target.value })} />
        <label className="text-muted">{t("properties.rating")}</label>
        <Stars value={f.rating} disabled={off} onChange={(rating) => setF({ ...f, rating })} />
        <label className="text-muted">{t("properties.taken")}</label>
        <span className="flex items-center gap-2">
          <input
            aria-label={t("properties.dateTaken")}
            type="datetime-local"
            step={1}
            className={`${input} max-w-[15rem]`}
            disabled={off}
            value={f.taken}
            onChange={(e) => setF({ ...f, taken: e.target.value })}
          />
          {meta.taken_offset && <span className="text-[0.7rem] text-dim">UTC{meta.taken_offset}</span>}
        </span>
      </div>
      {meta.editing.note && <p className="mt-3 text-[0.7rem] text-dim">{meta.editing.note}</p>}
      {meta.sidecar && <p className="mt-1 text-[0.7rem] text-dim">{t("properties.sidecar", { name: baseName(meta.sidecar) })}</p>}
    </Card>
  );
}

function Facts({ meta }: { meta: ImageMeta }) {
  const groups = [
    ["camera", t("properties.camera"), meta.camera],
    ["exposure", t("properties.exposure"), meta.exposure],
    ["image", t("properties.image"), meta.image],
  ] as const;
  const any = groups.some(([, , list]) => list.length > 0) || meta.width != null;
  if (!any) return null;
  return (
    <Card title={t("properties.photo")}>
      <div className="grid gap-x-8 gap-y-4 sm:grid-cols-2">
        {groups.map(([id, title, list]) =>
          list.length > 0 || (id === "image" && meta.width != null) ? (
            <div key={id}>
              <p className="mb-2 text-[0.72rem] font-semibold text-muted">{title}</p>
              <Rows>
                {id === "image" && meta.width != null && (
                  <Row label={t("properties.size")}>
                    {t("properties.pixels", { width: meta.width, height: meta.height ?? 0, mp: num((meta.width * (meta.height ?? 0)) / 1e6, 1) })}
                  </Row>
                )}
                {id === "image" && <Row label={t("properties.format")}>{meta.format}</Row>}
                {list.map((f) => (
                  <Row key={f.label} label={f.label}>
                    {f.value}
                  </Row>
                ))}
              </Rows>
            </div>
          ) : null,
        )}
      </div>
    </Card>
  );
}

/** A two-step button: the first click asks, the second does it. */
function Confirmed({ label, ask, onConfirm, disabled }: { label: string; ask: string; onConfirm: () => void; disabled?: boolean }) {
  const [asking, setAsking] = useState(false);
  if (!asking)
    return (
      <button className="btn btn-ghost !h-8 !text-[0.74rem] text-coral" disabled={disabled} onClick={() => setAsking(true)}>
        {label}
      </button>
    );
  return (
    <span className="flex items-center gap-2 text-[0.74rem]">
      <span className="text-muted">{ask}</span>
      <button className="btn btn-ghost !h-8 !text-[0.74rem]" onClick={() => setAsking(false)}>
        {t("common.cancel")}
      </button>
      <button
        className="btn btn-danger !h-8 !text-[0.74rem]"
        onClick={() => {
          setAsking(false);
          onConfirm();
        }}
      >
        {label}
      </button>
    </span>
  );
}

function Location({ meta, platform, showMap, onExpand, onRemove }: { meta: ImageMeta; platform: Boot["platform"]; showMap: boolean; onExpand: () => void; onRemove: () => void }) {
  const g = meta.gps;
  if (!g && !meta.has_location) return null;
  const mapsUrl = g
    ? platform === "macOS"
      ? `https://maps.apple.com/?ll=${g.lat},${g.lon}&q=${encodeURIComponent(t("properties.photo"))}`
      : `https://www.openstreetmap.org/?mlat=${g.lat}&mlon=${g.lon}#map=16/${g.lat}/${g.lon}`
    : null;
  return (
    <Card title={t("properties.location")}>
      {g ? (
        <>
          {showMap && <MiniMap lat={g.lat} lon={g.lon} onExpand={onExpand} />}
          <div className={showMap ? "mt-3" : ""}>
            <Rows>
              <Row label={t("properties.coordinates")}>
                {dms(g.lat, "N", "S")}, {dms(g.lon, "E", "W")}
                <span className="ml-2 text-dim">
                  ({g.lat.toFixed(6)}, {g.lon.toFixed(6)})
                </span>
              </Row>
              {g.altitude != null && <Row label={t("properties.altitude")}>{t("properties.meters", { value: num(g.altitude, 1) })}</Row>}
              {g.direction != null && (
                <Row label={t("properties.direction")}>
                  <span className="inline-flex items-center gap-1.5">
                    <Navigation className="h-3.5 w-3.5 text-dim" style={{ transform: `rotate(${g.direction}deg)` }} />
                    {t("properties.fromNorth", { degrees: num(Math.round(g.direction)) })}
                  </span>
                </Row>
              )}
            </Rows>
          </div>
        </>
      ) : (
        <p className="text-[0.76rem] text-muted">{t("properties.locationUnreadable")}</p>
      )}
      <div className="mt-3 flex flex-wrap items-center gap-2">
        {mapsUrl && (
          <button className="btn btn-ghost !h-8 !text-[0.74rem]" onClick={() => void api.openUrl(mapsUrl)}>
            <MapPin className="h-3.5 w-3.5" />
            {platform === "macOS" ? t("properties.openInMaps") : t("properties.openInOsm")}
          </button>
        )}
        <span className="flex-1" />
        {meta.editing.privacy && <Confirmed label={t("properties.removeLocation")} ask={t("properties.removeLocationAsk")} onConfirm={onRemove} />}
      </div>
    </Card>
  );
}

/** The same changes for several images: only what is touched is written. */
function Batch({ count, onApply }: { count: number; onApply: (edit: MetaEdit) => Promise<boolean> }) {
  const [creator, setCreator] = useState<string | null>(null);
  const [copyright, setCopyright] = useState<string | null>(null);
  const [keywords, setKeywords] = useState<string[]>([]);
  const [rating, setRating] = useState<number | null>(null);
  const edit: MetaEdit = {};
  if (creator != null) edit.creator = creator;
  if (copyright != null) edit.copyright = copyright;
  if (keywords.length > 0) edit.add_keywords = keywords;
  if (rating != null) edit.rating = rating;
  const dirty = Object.keys(edit).length > 0;
  return (
    <Card title={t("common.image", { count })}>
      <div className="grid grid-cols-[7.5rem_minmax(0,1fr)] items-center gap-x-3 gap-y-2.5 text-[0.78rem]">
        <label className="text-muted">{t("properties.creator")}</label>
        <input aria-label={t("properties.creator")} className={input} placeholder={t("properties.eachKeeps")} value={creator ?? ""} onChange={(e) => setCreator(e.target.value)} />
        <label className="text-muted">{t("properties.copyright")}</label>
        <input aria-label={t("properties.copyright")} className={input} placeholder={t("properties.eachKeeps")} value={copyright ?? ""} onChange={(e) => setCopyright(e.target.value)} />
        <label className="self-start pt-2 text-muted">{t("properties.addKeywords")}</label>
        <KeywordsField value={keywords} onChange={setKeywords} placeholder={t("properties.addedToEach")} />
        <label className="text-muted">{t("properties.rating")}</label>
        <span className="flex items-center gap-3">
          <Stars value={rating ?? 0} onChange={setRating} />
          {rating == null ? <span className="text-[0.7rem] text-dim">{t("properties.eachKeeps")}</span> : null}
        </span>
      </div>
      <div className="mt-4 flex flex-wrap items-center gap-2">
        <Confirmed label={t("properties.removeLocations")} ask={t("properties.fromAll", { count })} onConfirm={() => void onApply({ remove_location: true })} />
        <Confirmed label={t("properties.removeAllMetadata")} ask={t("properties.fromAllStrip", { count })} onConfirm={() => void onApply({ strip_all: true })} />
        <span className="flex-1" />
        <button className="btn btn-primary !h-8 !text-[0.74rem]" disabled={!dirty} onClick={() => void onApply(edit).then((ok) => ok && (setCreator(null), setCopyright(null), setKeywords([]), setRating(null)))}>
          {t("properties.applyToAll")}
        </button>
      </div>
      <p className="mt-2 text-[0.7rem] text-dim">{t("properties.batchNote")}</p>
    </Card>
  );
}

/**
 * Properties of the chosen items (Finder's Get Info), over the window:
 * general facts, permissions, extended attributes and, for an image, its
 * description, camera, exposure and location, with privacy actions. Several
 * items show what they share and take the same changes at once.
 */
export function PropertiesDialog({
  paths,
  boot,
  showMap,
  onPermissions,
  onMeta,
  onClose,
}: {
  paths: string[];
  boot: Boot;
  showMap: boolean;
  onPermissions: (paths: string[], change: { set: number; clear: number }, recursive: boolean) => Promise<boolean>;
  onMeta: (paths: string[], edit: MetaEdit) => Promise<boolean>;
  onClose: () => void;
}) {
  const toast = useToast();
  const [infos, setInfos] = useState<ItemInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [meta, setMeta] = useState<ImageMeta | null | undefined>(undefined);
  const [total, setTotal] = useState<[number, number] | null>(null);
  const [mapOpen, setMapOpen] = useState(false);
  const mapRef = useRef(mapOpen);
  mapRef.current = mapOpen;
  const single = paths.length === 1;

  const load = useCallback(() => {
    api.properties(paths).then(setInfos, (e) => setError(errorText(e)));
    if (paths.length === 1) api.imageMeta(paths[0]).then(setMeta, () => setMeta(null));
  }, [paths]);
  useEffect(load, [load]);

  // A folder's size, or several items' total: measured once, stopped when the dialog closes.
  const needsTotal = infos != null && (infos.length > 1 || infos[0]?.kind === "dir");
  useEffect(() => {
    if (!needsTotal) return;
    const id = newTaskId();
    let alive = true;
    api.totalSize(paths, id).then(
      (t) => alive && setTotal(t),
      () => {},
    );
    return () => {
      alive = false;
      api.cancelTask(id).catch(() => {});
    };
  }, [needsTotal, paths]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || mapRef.current) return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  const drive = boot.places.find((p) => p.icon === "drive")?.path;
  const inDrive = drive != null && paths.some((p) => p === drive || p.startsWith(`${drive}/`));
  const first = infos?.[0];
  const entry: Entry | null = first
    ? { name: first.name, path: first.path, kind: first.kind === "dir" ? "dir" : first.kind === "symlink" ? "symlink" : "file", is_dir: first.kind === "dir", size: first.size, modified: first.modified ?? 0, created: first.created, hidden: first.name.startsWith("."), ext: first.name.includes(".") ? first.name.split(".").pop()!.toLowerCase() : "", thumb: first.kind === "file" && isImageName(first.name) }
    : null;
  const images = useMemo(() => (infos ?? []).filter((i) => i.kind === "file" && isImageName(i.name)), [infos]);
  const kinds = infos
    ? (
        [
          [infos.filter((i) => i.kind === "dir").length, "common.folder"],
          [images.length, "common.image"],
          [infos.filter((i) => i.kind === "file" && !isImageName(i.name)).length, "properties.otherFile"],
          [infos.filter((i) => i.kind === "symlink").length, "properties.link"],
        ] as [number, Key][]
      )
        .filter(([n]) => n > 0)
        .map(([n, key]) => t(key, { count: n }))
        .join(", ")
    : "";
  const parents = [...new Set(paths.map((p) => parentOf(p) ?? "/"))];
  const quarantine = single && first?.xattrs.some((x) => x.name === "com.apple.quarantine");

  const saveMeta = async (targets: string[], edit: MetaEdit) => {
    const ok = await onMeta(targets, edit);
    if (ok) load();
    return ok;
  };
  const savePerms = async (change: { set: number; clear: number }, recursive: boolean) => {
    const ok = await onPermissions(paths, change, recursive);
    if (ok) load();
    return ok;
  };

  const sizeText = () => {
    if (!infos || !first) return "";
    if (single && first.kind !== "dir") return t("properties.sizeBytes", { size: bytesText(first.size), count: first.size });
    if (!total) return t("properties.measuring");
    return t("properties.sizeIn", { size: bytesText(total[0]), count: total[1] });
  };

  return createPortal(
    <div className="fade-in fixed inset-0 z-40 grid place-items-center bg-black/35 p-10 backdrop-blur-md" onMouseDown={onClose}>
      <div
        role="dialog"
        aria-modal
        aria-label={t("properties.properties")}
        className="glass sheet flex h-[min(46rem,calc(100vh-5rem))] w-[min(48rem,100%)] flex-col p-5"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="flex flex-none items-center gap-3.5">
          {entry && single ? (
            <HeaderIcon entry={entry} />
          ) : (
            <span className="grid h-12 w-12 flex-none place-items-center">
              <Copy className="h-6 w-6 accent-fg" />
            </span>
          )}
          <div className="min-w-0 flex-1">
            <h2 className="selectable truncate text-[1.05rem] font-semibold leading-tight">{single ? (first?.name ?? baseName(paths[0])) : t("common.item", { count: paths.length })}</h2>
            <p className="mt-0.5 truncate text-[0.74rem] text-muted">{single ? `${meta?.format ?? (first?.kind === "dir" ? t("properties.kindFolder") : first?.kind === "symlink" ? t("properties.kindLink") : t("properties.kindFile"))} · ${sizeText()}` : kinds}</p>
          </div>
          <button aria-label={t("common.close")} onClick={onClose} className="grid h-9 w-9 flex-none place-items-center rounded-full bg-white/8 text-muted hover:bg-white/14 hover:text-ink">
            <X className="h-4 w-4" />
          </button>
        </header>
        <div className="scroll fade-edges -mx-1 mt-4 min-h-0 flex-1 px-1 py-3" data-properties>
          {error && <p className="text-[0.8rem] text-coral">{error}</p>}
          {infos && first && (
            <>
              <Card title={t("properties.general")}>
                <Rows>
                  {!single && <Row label={t("properties.items")}>{kinds}</Row>}
                  <Row label={t("properties.size")}>{sizeText()}</Row>
                  {single && first.kind !== "dir" && <Row label={t("properties.onDisk")}>{bytesText(first.on_disk)}</Row>}
                  <Row label={t("properties.where")}>
                    <span className="inline-flex items-center gap-2">
                      {parents.length === 1 ? parents[0] : t("common.folder", { count: parents.length })}
                      {parents.length === 1 && (
                        <button
                          aria-label={t("properties.copyPath")}
                          title={t("properties.copyPath")}
                          className="text-dim hover:text-ink"
                          onClick={() => void copyText(single ? first.path : parents[0]).then(() => toast("ok", t("properties.pathCopied")))}
                        >
                          <Copy className="h-3.5 w-3.5" />
                        </button>
                      )}
                    </span>
                  </Row>
                  {single && (
                    <>
                      {first.link_target && (
                        <Row label={t("properties.pointsTo")}>
                          {first.link_target}
                          {first.link_broken && <span className="ml-2 text-coral">{t("properties.notThere")}</span>}
                        </Row>
                      )}
                      <Row label={t("properties.created")}>{when(first.created)}</Row>
                      <Row label={t("properties.modified")}>{when(first.modified)}</Row>
                      <Row label={t("properties.opened")}>{when(first.accessed)}</Row>
                      <Row label={t("properties.owner")}>
                        {t("properties.ownerLine", { owner: first.owner ?? "--", uid: String(first.uid), group: first.group ?? "--", gid: String(first.gid) })}
                      </Row>
                      <Row label={t("properties.inode")}>
                        {t("properties.inodeLine", { inode: String(first.inode), device: String(first.device), links: t("properties.link", { count: first.links }) })}
                      </Row>
                    </>
                  )}
                </Rows>
              </Card>
              {meta && single && (
                <>
                  <Description meta={meta} onSave={(edit) => saveMeta(paths, edit)} />
                  <Facts meta={meta} />
                  <Location
                    meta={meta}
                    platform={boot.platform}
                    showMap={showMap}
                    onExpand={() => setMapOpen(true)}
                    onRemove={() => void saveMeta(paths, { remove_location: true })}
                  />
                  {meta.editing.privacy && (
                    <Card title={t("properties.privacy")}>
                      <div className="flex flex-wrap items-center gap-3">
                        <ShieldAlert className="h-4 w-4 flex-none text-dim" />
                        <p className="min-w-0 flex-1 text-[0.74rem] text-muted">{t("properties.privacyNote")}</p>
                        <Confirmed label={t("properties.removeAllMetadata")} ask={t("properties.sure")} onConfirm={() => void saveMeta(paths, { strip_all: true })} />
                      </div>
                    </Card>
                  )}
                </>
              )}
              {!single && images.length > 0 && <Batch count={images.length} onApply={(edit) => saveMeta(images.map((i) => i.path), edit)} />}
              <Permissions infos={infos} inDrive={inDrive} onApply={savePerms} />
              {single && first.xattrs.length > 0 && (
                <Card title={t("properties.xattrs")}>
                  <ul className="space-y-1 font-mono text-[0.72rem]">
                    {first.xattrs.map((x) => (
                      <li key={x.name} className="flex gap-3">
                        <span className="selectable min-w-0 flex-1 truncate text-ink/90">{x.name}</span>
                        <span className="text-dim">{bytesText(x.size)}</span>
                      </li>
                    ))}
                  </ul>
                  {quarantine && boot.platform === "macOS" && (
                    <div className="mt-3 flex items-center gap-3 border-t border-white/6 pt-3">
                      <p className="min-w-0 flex-1 text-[0.72rem] text-muted">{t("properties.quarantineNote")}</p>
                      <button
                        className="btn btn-ghost !h-8 !text-[0.74rem]"
                        onClick={() =>
                          api.removeQuarantine(first.path).then(
                            () => {
                              toast("ok", t("properties.quarantineGone"));
                              load();
                            },
                            (e) => toast("error", errorText(e)),
                          )
                        }
                      >
                        {t("properties.removeQuarantine")}
                      </button>
                    </div>
                  )}
                </Card>
              )}
            </>
          )}
        </div>
      </div>
      {mapOpen && meta?.gps && <MapDialog lat={meta.gps.lat} lon={meta.gps.lon} title={first?.name ?? ""} onClose={() => setMapOpen(false)} />}
    </div>,
    document.body,
  );
}
