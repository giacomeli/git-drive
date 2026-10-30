/** A section's heading when the items are grouped (Group By). */
export function GroupHead({ label, count, className = "" }: { label: string; count: number; className?: string }) {
  return (
    <div className={`flex items-baseline gap-2 border-b border-line pb-1.5 pt-3 text-[0.74rem] font-semibold text-ink ${className}`}>
      {label}
      <span className="text-[0.68rem] font-normal text-dim">{count}</span>
    </div>
  );
}
