import { FolderSearch, HardDrive, Lock, LockOpen, Radio, Sparkles } from "lucide-react";
import type { ReactNode } from "react";
import { api, errorText, type About } from "../lib/api";
import { tildify } from "../lib/format";
import { PageHeader } from "../components/Shell";
import { Card, Pill, useToast } from "../components/ui";

const GLASS_LABEL = { "liquid-glass": "Liquid Glass", vibrancy: "Vibrancy", none: "Drawn in the window" } as const;

export function AboutView({ about }: { about: About | null }) {
  const toast = useToast();
  if (!about) {
    return (
      <div className="fade-in">
        <PageHeader title="About" />
      </div>
    );
  }
  return (
    <div className="fade-in">
      <PageHeader title="About" subtitle="Version, environment and where git-drive keeps its files" />
      <div className="grid grid-cols-12 gap-5">
        <section className="hero relative col-span-12 overflow-hidden p-7 text-white lg:col-span-5">
          <div className="relative flex h-full flex-col">
            <div className="grid h-16 w-16 place-items-center rounded-[1.4rem] bg-white/20 shadow-lg shadow-black/10 backdrop-blur">
              <HardDrive className="h-8 w-8" strokeWidth={1.8} />
            </div>
            <p className="mt-6 text-[2rem] font-semibold leading-none tracking-tight">git-drive</p>
            <p className="mt-2 text-sm text-white/80">A Git repository mounted as a drive, synced in both directions.</p>
            <div className="mt-auto flex flex-wrap gap-2 pt-8 text-[0.72rem] font-medium">
              <span className="rounded-full bg-white/18 px-3 py-1.5">Version {about.version}</span>
              <span className="rounded-full bg-white/18 px-3 py-1.5">{about.platform}</span>
            </div>
          </div>
        </section>

        <Card className="col-span-12 lg:col-span-7" title="Environment">
          <dl className="grid grid-cols-2 gap-x-6 gap-y-5">
            <Fact icon={<Radio className="h-4 w-4" />} label="Login service" value={about.service_name}>
              <Pill tone={about.service_installed ? "mint" : "amber"}>{about.service_installed ? "Installed" : "Not installed"}</Pill>
            </Fact>
            <Fact icon={<Sparkles className="h-4 w-4" />} label="Window effect" value={GLASS_LABEL[about.glass]} />
            <Fact
              icon={about.encryption ? <Lock className="h-4 w-4" /> : <LockOpen className="h-4 w-4" />}
              label="Encryption at rest"
              value={about.encryption ? "On" : "Off"}
            />
            <Fact icon={<Radio className="h-4 w-4" />} label="P2P channel" value={about.p2p ? "On" : "Off"} />
            <Fact icon={<HardDrive className="h-4 w-4" />} label="Git transport" value={about.git_transport} />
          </dl>
        </Card>

        <Card className="col-span-12" title="Files and folders" subtitle="Everything git-drive reads or writes outside the drive">
          <ul className="divide-y divide-white/6">
            {about.paths.map((p) => (
              <li key={p.label} className="flex items-center gap-4 py-3 first:pt-0 last:pb-0">
                <p className="w-36 flex-none text-[0.8rem] font-medium text-muted">{p.label}</p>
                <p className={`selectable min-w-0 flex-1 truncate font-mono text-[0.75rem] ${p.exists ? "text-ink" : "text-dim line-through"}`} title={p.path}>
                  {tildify(p.path)}
                </p>
                <button
                  className="btn btn-ghost !h-8 !px-3 !text-[0.72rem]"
                  disabled={!p.exists}
                  onClick={() => api.openPath(p.path).catch((e) => toast("error", errorText(e)))}
                >
                  <FolderSearch className="h-3.5 w-3.5" />
                  Reveal
                </button>
              </li>
            ))}
          </ul>
        </Card>
      </div>
    </div>
  );
}

function Fact({ icon, label, value, children }: { icon: ReactNode; label: string; value: string; children?: ReactNode }) {
  return (
    <div className="min-w-0">
      <dt className="flex items-center gap-2 text-[0.72rem] text-dim">
        {icon}
        {label}
      </dt>
      <dd className="mt-1.5 flex flex-wrap items-center gap-2 text-[0.9rem] font-semibold">
        <span className="truncate">{value}</span>
        {children}
      </dd>
    </div>
  );
}
