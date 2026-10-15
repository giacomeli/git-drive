import { useEffect, useState } from "react";
import { ACCENTS, accentById, applyAccent, DEFAULT_ACCENT } from "../lib/accent";
import { api, errorText } from "../lib/api";
import { Card, useToast } from "../components/ui";

/** Accent color picker in the style of the macOS one: applies at once, saved right away. */
export function ThemeCard() {
  const toast = useToast();
  const [accent, setAccent] = useState<string | null>(null);

  useEffect(() => {
    api
      .uiPrefs()
      .then((p) => setAccent(accentById(p.accent).id))
      .catch(() => setAccent(DEFAULT_ACCENT));
  }, []);

  const choose = (id: string) => {
    if (id === accent) return;
    const previous = accent ?? DEFAULT_ACCENT;
    applyAccent(id);
    setAccent(id);
    api.saveUiPrefs({ accent: id }).catch((e) => {
      applyAccent(previous);
      setAccent(previous);
      toast("error", `Could not save the accent color: ${errorText(e)}`);
    });
  };

  return (
    <Card title="Theme" subtitle="How this window looks; applies at once and is saved right away">
      <div className="flex items-start gap-6">
        <div className="min-w-0 flex-1 pt-1.5">
          <p className="text-[0.84rem] font-medium">Accent color</p>
          <p className="mt-0.5 text-[0.7rem] text-dim">Buttons, toggles, charts and the highlight card</p>
        </div>
        <div className="flex flex-none flex-wrap justify-end gap-x-3.5 gap-y-1" role="radiogroup" aria-label="Accent color">
          {ACCENTS.map((a) => {
            const selected = a.id === accent;
            return (
              <div key={a.id} className="flex w-7 flex-col items-center">
                <button
                  type="button"
                  role="radio"
                  aria-checked={selected}
                  aria-label={a.label}
                  title={a.label}
                  onClick={() => choose(a.id)}
                  className="h-7 w-7 rounded-full transition-transform duration-150 hover:scale-110 active:scale-95"
                  style={{
                    background: a.swatch,
                    boxShadow: "inset 0 0 0 1px rgb(255 255 255 / 0.18), 0 4px 10px -4px rgb(0 0 0 / 0.5)",
                    outline: selected ? `2.5px solid ${a.id === "multicolor" ? "var(--a2)" : a.swatch}` : "none",
                    outlineOffset: 3,
                  }}
                />
                <span
                  className={`mt-2.5 whitespace-nowrap text-[0.66rem] font-medium text-muted transition-opacity ${
                    selected ? "opacity-100" : "opacity-0"
                  }`}
                >
                  {a.label}
                </span>
              </div>
            );
          })}
        </div>
      </div>
    </Card>
  );
}
