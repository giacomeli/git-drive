import type { RepoUsage } from "./api";
import { bytesText } from "../ui/format";
import { t } from "../i18n";

// The drive's repository against what its host accepts (status.json's `repo_usage`).

/** The repository's bar color: coral at or past the limit, amber past the host's recommended size or 80% of the limit. */
export function repoColor(u: RepoUsage): string {
  if (u.limit != null && u.used >= u.limit) return "var(--color-coral)";
  if ((u.recommended != null && u.used >= u.recommended) || (u.limit != null && u.used > 0.8 * u.limit)) return "var(--color-amber)";
  return "var(--accent)";
}

/** "5.2 GB of 10.0 GB on GitHub" ("of your 50.0 GB" for a quota), or the size alone where no limit is known. */
export function repoText(u: RepoUsage): string {
  const where = u.host ? t("sidebar.repo.onHost", { host: u.host }) : "";
  if (u.limit == null) return `${bytesText(u.used)}${where}`;
  return t(u.from_config ? "sidebar.repo.ofYour" : "sidebar.repo.of", { used: bytesText(u.used), limit: bytesText(u.limit), where });
}

/** What the limit means, for the bar's tooltip. */
export function limitNote(u: RepoUsage): string {
  if (u.limit == null) return t("sidebar.repo.noLimit", { host: u.host || t("sidebar.repo.thisHost") });
  const mark = u.recommended != null ? t("sidebar.repo.mark", { host: u.host, size: bytesText(u.recommended) }) : "";
  const limit = bytesText(u.limit);
  if (u.from_config) return t("sidebar.repo.yourQuota", { limit, mark });
  if (u.host === "GitHub") return t("sidebar.repo.github", { limit, mark });
  return t("sidebar.repo.hostLimit", { limit, host: u.host, mark });
}
