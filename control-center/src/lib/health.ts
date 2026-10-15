import type { Overview } from "./api";
import type { Tone } from "../components/ui";

export interface Health {
  tone: Tone;
  title: string;
  detail: string;
}

/** The daemon runs but the volume is not mounted yet. */
export function booting(o: Overview | null): boolean {
  return o?.service === "running" && o.status?.mounted_at === null;
}

/** One headline for the drive, most urgent condition first. */
export function health(o: Overview | null): Health {
  if (!o) return { tone: "dim", title: "Connecting", detail: "Reading the daemon state" };
  if (o.service === "not_installed") {
    return { tone: "dim", title: "Not installed", detail: "The login service is not set up on this machine" };
  }
  const s = o.status;
  if (o.service === "stopped" || !s) {
    return { tone: "coral", title: "Stopped", detail: "The drive is not mounted and nothing syncs" };
  }
  if (s.mounted_at === null) {
    return { tone: "cyan", title: "Starting", detail: "Replaying the journal and indexing the files; the drive appears when it is ready" };
  }
  if (s.commit_blocked) {
    return { tone: "coral", title: "Commit blocked", detail: s.commit_blocked };
  }
  if (s.pending_ops > 0) {
    const n = s.pending_ops;
    return { tone: "cyan", title: "Syncing", detail: `${n} change${n === 1 ? "" : "s"} waiting to be committed` };
  }
  if (s.unpushed > 0) {
    const n = s.unpushed;
    return { tone: "amber", title: "Publishing", detail: `${n} commit${n === 1 ? "" : "s"} not on the remote yet` };
  }
  if (s.last_pull && /error/.test(s.last_pull.outcome)) {
    return { tone: "amber", title: "Remote unreachable", detail: `Last pull: ${s.last_pull.outcome}` };
  }
  return { tone: "mint", title: "Up to date", detail: "Every change is committed and published" };
}
