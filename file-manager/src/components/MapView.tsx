import { LocateFixed, MapPin, Maximize2, Minus, Plus, X } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { api } from "../lib/api";
import { latLonAt, tilesFor, worldPixel } from "../lib/geo";
import { t } from "../i18n";

/** OpenStreetMap's standard tiles (their use asks for the attribution shown on the map). */
const TILE_URL = (z: number, x: number, y: number) => `https://tile.openstreetmap.org/${z}/${x}/${y}.png`;
const ZOOM = { min: 3, max: 19 };

/** The tiles of a view centered on world pixel (cx, cy), and the pin at the photo's place. */
function Tiles({ cx, cy, zoom, width, height, pin }: { cx: number; cy: number; zoom: number; width: number; height: number; pin: { x: number; y: number } }) {
  const tiles = width > 0 ? tilesFor(cx, cy, zoom, width, height) : [];
  const px = pin.x - (cx - width / 2);
  const py = pin.y - (cy - height / 2);
  return (
    <>
      {tiles.map((tile) => (
        <img
          key={`${tile.z}/${tile.x}/${tile.y}/${tile.left}`}
          src={TILE_URL(tile.z, tile.x, tile.y)}
          alt=""
          draggable={false}
          className="absolute h-64 w-64 max-w-none select-none"
          style={{ left: tile.left, top: tile.top }}
        />
      ))}
      <MapPin
        className="absolute h-7 w-7 drop-shadow-[0_3px_6px_rgb(0_0_0/0.45)]"
        style={{ left: px - 14, top: py - 26, color: "var(--a2)" }}
        fill="currentColor"
        stroke="white"
        strokeWidth={1.6}
      />
    </>
  );
}

function Attribution() {
  return (
    <button
      onClick={() => void api.openUrl("https://www.openstreetmap.org/copyright")}
      className="absolute bottom-1 right-1.5 rounded bg-white/80 px-1.5 py-px text-[0.6rem] text-slate-700 hover:text-slate-900"
    >
      © OpenStreetMap
    </button>
  );
}

/** A small map of where a photo was taken, with a button that opens a larger one. */
export function MiniMap({ lat, lon, onExpand }: { lat: number; lon: number; onExpand: () => void }) {
  const box = useRef<HTMLDivElement | null>(null);
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setWidth(el.clientWidth));
    ro.observe(el);
    setWidth(el.clientWidth);
    return () => ro.disconnect();
  }, []);
  const zoom = 14;
  const p = worldPixel(lat, lon, zoom);
  return (
    <div ref={box} data-minimap className="relative h-44 overflow-hidden rounded-xl bg-[#aad3df] ring-1 ring-white/10">
      <Tiles cx={p.x} cy={p.y} zoom={zoom} width={width} height={176} pin={p} />
      <button
        aria-label={t("map.larger")}
        title={t("map.larger")}
        onClick={onExpand}
        className="absolute right-2 top-2 grid h-8 w-8 place-items-center rounded-full bg-black/45 text-paper backdrop-blur hover:bg-black/60"
      >
        <Maximize2 className="h-3.5 w-3.5" />
      </button>
      <Attribution />
    </div>
  );
}

/** The larger map over the window: drag to move, the wheel or the buttons to zoom. */
export function MapDialog({ lat, lon, title, onClose }: { lat: number; lon: number; title: string; onClose: () => void }) {
  const box = useRef<HTMLDivElement | null>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [zoom, setZoom] = useState(16);
  // The view's center, as a point (it survives a zoom).
  const [center, setCenter] = useState({ lat, lon });
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    setSize({ w: el.clientWidth, h: el.clientHeight });
    return () => ro.disconnect();
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);
  const c = worldPixel(center.lat, center.lon, zoom);
  const pin = worldPixel(lat, lon, zoom);
  const zoomBy = (d: number) => setZoom((z) => Math.max(ZOOM.min, Math.min(ZOOM.max, z + d)));
  const drag = (ev: React.MouseEvent) => {
    if (ev.button !== 0) return;
    ev.preventDefault();
    const start = { x: ev.clientX, y: ev.clientY, cx: c.x, cy: c.y };
    const move = (m: MouseEvent) => setCenter(latLonAt(start.cx - (m.clientX - start.x), start.cy - (m.clientY - start.y), zoom));
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };
  const button = "grid h-9 w-9 place-items-center rounded-full bg-black/55 text-paper backdrop-blur hover:bg-black/70";
  return createPortal(
    <div
      className="fade-in fixed inset-0 z-50 grid place-items-center bg-black/40 p-8 backdrop-blur-md"
      // Its own: a press here never reaches the Properties dialog under it (React carries events through portals).
      onMouseDown={(e) => {
        e.stopPropagation();
        onClose();
      }}
    >
      <div role="dialog" aria-modal aria-label={t("map.map")} className="glass sheet flex h-full w-full max-w-[72rem] flex-col p-4" onMouseDown={(e) => e.stopPropagation()}>
        <header className="mb-3 flex flex-none items-center gap-3">
          <MapPin className="h-4 w-4 accent-fg" />
          <p className="min-w-0 flex-1 truncate text-[0.9rem] font-semibold">{title}</p>
          <button aria-label={t("common.close")} onClick={onClose} className="grid h-8 w-8 place-items-center rounded-full bg-white/8 text-muted hover:bg-white/14 hover:text-ink">
            <X className="h-4 w-4" />
          </button>
        </header>
        <div
          ref={box}
          className="relative min-h-0 flex-1 cursor-grab overflow-hidden rounded-xl bg-[#aad3df] active:cursor-grabbing"
          onMouseDown={drag}
          onWheel={(e) => zoomBy(e.deltaY < 0 ? 1 : -1)}
        >
          <Tiles cx={c.x} cy={c.y} zoom={zoom} width={size.w} height={size.h} pin={pin} />
          <div className="absolute right-3 top-3 flex flex-col gap-2" onMouseDown={(e) => e.stopPropagation()}>
            <button aria-label={t("map.zoomIn")} title={t("map.zoomIn")} className={button} onClick={() => zoomBy(1)}>
              <Plus className="h-4 w-4" />
            </button>
            <button aria-label={t("map.zoomOut")} title={t("map.zoomOut")} className={button} onClick={() => zoomBy(-1)}>
              <Minus className="h-4 w-4" />
            </button>
            <button aria-label={t("map.backToPhoto")} title={t("map.backToPhoto")} className={button} onClick={() => setCenter({ lat, lon })}>
              <LocateFixed className="h-4 w-4" />
            </button>
          </div>
          <Attribution />
        </div>
      </div>
    </div>,
    document.body,
  );
}
