import { Maximize2, Minimize2, Pause, Play, Volume1, Volume2, VolumeX } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import { t } from "../i18n";
import { clockText, fitInside } from "../lib/player";

/** The controls hide after this long without the pointer moving, while playing. */
const IDLE_MS = 2000;


type FullscreenTarget = HTMLElement & { webkitRequestFullscreen?: () => void };
type FullscreenDoc = Document & { webkitFullscreenElement?: Element | null; webkitFullscreenEnabled?: boolean; webkitExitFullscreen?: () => void };
type WebkitVideo = HTMLVideoElement & { webkitEnterFullscreen?: () => void };

/**
 * A video with controls of the app's own (the webview's are drawn for a full
 * page: oversized in the narrow preview panel). A bar in dark glass over the
 * bottom: play, the time, a seek bar in the accent, the volume and full
 * screen; `compact` (the preview panel) keeps play, the seek bar, the time and
 * full screen on one slim line, and the container's width drops what does not
 * fit. A click on the picture plays or pauses. While playing, the bar and the
 * pointer hide after two seconds without movement. The buttons never take the
 * keyboard's focus: Space stays Quick Look's, and the arrows the folder's.
 */
export function VideoPlayer({ src, compact, onError }: { src: string; compact: boolean; onError: () => void }) {
  // The room the player has, and the frame sized to the picture inside it
  // (the controls sit on the picture, not on the letterbox around it).
  const room = useRef<HTMLDivElement | null>(null);
  const box = useRef<HTMLDivElement | null>(null);
  const [ratio, setRatio] = useState(16 / 9);
  const [roomSize, setRoomSize] = useState({ w: 0, h: 0 });
  const video = useRef<HTMLVideoElement | null>(null);
  const [playing, setPlaying] = useState(false);
  const [time, setTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [volume, setVolume] = useState(1);
  const [muted, setMuted] = useState(false);
  const [idle, setIdle] = useState(false);
  const [full, setFull] = useState(false);
  const idleTimer = useRef(0);

  const wake = useCallback(() => {
    setIdle(false);
    window.clearTimeout(idleTimer.current);
    idleTimer.current = window.setTimeout(() => setIdle(true), IDLE_MS);
  }, []);
  useEffect(() => () => window.clearTimeout(idleTimer.current), []);


  useLayoutEffect(() => {
    const el = room.current;
    if (!el) return;
    const measure = () => setRoomSize({ w: el.clientWidth, h: el.clientHeight });
    // Measured at once (before the first paint), then whenever it changes.
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    const onChange = () => {
      const doc = document as FullscreenDoc;
      setFull((doc.fullscreenElement ?? doc.webkitFullscreenElement ?? null) === box.current);
    };
    document.addEventListener("fullscreenchange", onChange);
    document.addEventListener("webkitfullscreenchange", onChange);
    return () => {
      document.removeEventListener("fullscreenchange", onChange);
      document.removeEventListener("webkitfullscreenchange", onChange);
    };
  }, []);

  const toggle = () => {
    const v = video.current;
    if (!v) return;
    if (v.paused || v.ended) void v.play().catch(() => {});
    else v.pause();
  };
  const seek = (to: number) => {
    const v = video.current;
    if (v && Number.isFinite(v.duration)) v.currentTime = Math.min(v.duration, Math.max(0, to));
  };
  const setLevel = (level: number) => {
    const v = video.current;
    if (!v) return;
    v.volume = Math.min(1, Math.max(0, level));
    v.muted = v.volume === 0;
  };
  const toggleMute = () => {
    const v = video.current;
    if (!v) return;
    if (v.muted || v.volume === 0) {
      v.muted = false;
      if (v.volume === 0) v.volume = 0.5;
    } else v.muted = true;
  };
  const toggleFull = () => {
    const doc = document as FullscreenDoc;
    if (doc.fullscreenElement ?? doc.webkitFullscreenElement) {
      if (doc.exitFullscreen) void doc.exitFullscreen().catch(() => {});
      else doc.webkitExitFullscreen?.();
      return;
    }
    // The player's own full screen (its controls along); where the webview
    // allows no element in full screen, the video's native one.
    const native = () => (video.current as WebkitVideo | null)?.webkitEnterFullscreen?.();
    const el = box.current as FullscreenTarget | null;
    if (!el || !(doc.fullscreenEnabled ?? doc.webkitFullscreenEnabled ?? false)) return native();
    if (el.requestFullscreen) void el.requestFullscreen().catch(native);
    else if (el.webkitRequestFullscreen) {
      el.webkitRequestFullscreen();
      window.setTimeout(() => !(doc.fullscreenElement ?? doc.webkitFullscreenElement) && native(), 400);
    } else native();
  };

  const hidden = playing && idle;
  const quiet = muted || volume === 0;
  const VolumeIcon = quiet ? VolumeX : volume < 0.5 ? Volume1 : Volume2;
  const bar = compact ? "h-8 gap-1.5 px-1.5 text-[0.62rem]" : "h-11 gap-2.5 px-3 text-[0.72rem]";
  const icon = compact ? "h-3.5 w-3.5" : "h-4 w-4";

  const frame = fitInside(ratio, roomSize.w, roomSize.h);
  return (
    <div ref={room} className="flex h-full min-h-0 w-full items-center justify-center">
      <div
        ref={box}
        className={`player @container relative flex items-center justify-center overflow-hidden bg-black ${full ? "" : "rounded-xl shadow-2xl shadow-black/40"} ${hidden ? "cursor-none" : ""}`}
        // Until the room is measured, the whole room (never a 0x0 box, where nothing loads).
        style={full || frame.width <= 0 || frame.height <= 0 ? { width: "100%", height: "100%" } : { width: frame.width, height: frame.height }}
        onPointerMove={wake}
        onPointerLeave={() => playing && setIdle(true)}
      >
        <video
          ref={video}
          src={src}
          playsInline
          preload={compact ? "metadata" : "auto"}
          onClick={toggle}
          onError={onError}
          onPlay={() => {
            setPlaying(true);
            wake();
          }}
          onPause={() => setPlaying(false)}
          onEnded={() => setPlaying(false)}
          onTimeUpdate={(e) => setTime(e.currentTarget.currentTime)}
          onDurationChange={(e) => setDuration(e.currentTarget.duration)}
          onLoadedMetadata={(e) => {
            const v = e.currentTarget;
            setDuration(v.duration);
            if (v.videoWidth > 0 && v.videoHeight > 0) setRatio(v.videoWidth / v.videoHeight);
          }}
          onVolumeChange={(e) => {
            setVolume(e.currentTarget.volume);
            setMuted(e.currentTarget.muted);
          }}
          className="block h-full w-full object-contain"
        />
        {!playing && (
          <button
            type="button"
            tabIndex={-1}
            aria-label={t("views.video.play")}
            onMouseDown={(e) => e.preventDefault()}
            onClick={toggle}
            className={`player-glass absolute grid place-items-center rounded-full ${compact ? "h-10 w-10" : "h-16 w-16"}`}
          >
            <Play className={compact ? "ml-0.5 h-4 w-4" : "ml-1 h-6 w-6"} fill="currentColor" strokeWidth={0} />
          </button>
        )}
        <div
          className={`player-glass absolute flex items-center rounded-full transition-opacity duration-300 ${bar} ${
            compact ? "inset-x-1.5 bottom-1.5" : "inset-x-4 bottom-4"
          } ${hidden ? "pointer-events-none opacity-0" : "opacity-100"}`}
          onClick={(e) => e.stopPropagation()}
        >
          <BarButton label={playing ? t("views.video.pause") : t("views.video.play")} onClick={toggle}>
            {playing ? <Pause className={icon} fill="currentColor" strokeWidth={0} /> : <Play className={icon} fill="currentColor" strokeWidth={0} />}
          </BarButton>
          <span className={`flex-none tabular-nums ${compact ? "hidden @[15rem]:inline" : ""}`}>{clockText(time)}</span>
          <Track
            label={t("views.video.seek")}
            value={duration > 0 ? time / duration : 0}
            onChange={(f) => seek(f * duration)}
            className="min-w-8 flex-1"
          />
          <span className="flex-none tabular-nums">{compact ? `-${clockText(Math.max(0, duration - time))}` : clockText(duration)}</span>
          <BarButton label={quiet ? t("views.video.unmute") : t("views.video.mute")} onClick={toggleMute} className={compact ? "hidden @[18rem]:grid" : ""}>
            <VolumeIcon className={icon} />
          </BarButton>
          {!compact && <Track label={t("views.video.volume")} value={quiet ? 0 : volume} onChange={setLevel} className="hidden w-20 flex-none @[30rem]:block" />}
          <BarButton label={full ? t("views.video.exitFullscreen") : t("views.video.fullscreen")} onClick={toggleFull} className={compact ? "hidden @[12rem]:grid" : ""}>
            {full ? <Minimize2 className={icon} /> : <Maximize2 className={icon} />}
          </BarButton>
        </div>
      </div>
    </div>
  );
}

function BarButton({ label, onClick, className = "", children }: { label: string; onClick: () => void; className?: string; children: ReactNode }) {
  return (
    <button
      type="button"
      tabIndex={-1}
      aria-label={label}
      title={label}
      onMouseDown={(e) => e.preventDefault()}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      // `className` may hide it in a narrow player (`hidden @[18rem]:grid`): then it owns the display.
      className={`player-btn h-6 w-6 flex-none place-items-center rounded-full ${className || "grid"}`}
    >
      {children}
    </button>
  );
}

/** A thin bar dragged with the pointer (the seek bar, the volume): `value` and the changes are fractions. */
function Track({ label, value, onChange, className = "" }: { label: string; value: number; onChange: (fraction: number) => void; className?: string }) {
  const at = (e: ReactPointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    return r.width > 0 ? Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)) : 0;
  };
  const share = `${Math.min(1, Math.max(0, value)) * 100}%`;
  return (
    <div
      role="slider"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(value * 100)}
      className={`player-track group relative h-5 cursor-pointer touch-none ${className}`}
      onMouseDown={(e) => e.preventDefault()}
      onPointerDown={(e) => {
        e.stopPropagation();
        e.currentTarget.setPointerCapture(e.pointerId);
        onChange(at(e));
      }}
      onPointerMove={(e) => {
        if (e.currentTarget.hasPointerCapture(e.pointerId)) onChange(at(e));
      }}
      onClick={(e) => e.stopPropagation()}
    >
      <span className="player-rail absolute inset-x-0 top-1/2 h-1 -translate-y-1/2 rounded-full" />
      <span className="absolute left-0 top-1/2 h-1 -translate-y-1/2 rounded-full" style={{ width: share, background: "var(--accent)" }} />
      <span
        className="player-knob absolute top-1/2 h-3 w-3 -translate-x-1/2 -translate-y-1/2 rounded-full transition-transform group-hover:scale-110"
        style={{ left: share }}
      />
    </div>
  );
}
