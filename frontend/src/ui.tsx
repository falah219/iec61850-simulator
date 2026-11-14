// Komponen kecil bergaya acuan (tombol berbingkai hitam, switch L/R), tanpa library UI tambahan.
import type { ButtonHTMLAttributes, ReactNode } from 'react';
import * as SwitchPrimitive from '@radix-ui/react-switch';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import type { Quality } from './types';

export function Button({ className = '', ...props }: ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      className={`inline-flex items-center justify-center gap-1 rounded border-2 border-black text-sm font-medium transition active:scale-[0.98] disabled:opacity-50 disabled:cursor-not-allowed disabled:active:scale-100 ${className}`}
      {...props}
    />
  );
}

/** Switch L/R seperti acuan: kiri = Local, kanan (checked) = Remote */
export function LocalRemote({ local, onChange, disabled }: { local: boolean; onChange: (local: boolean) => void; disabled?: boolean }) {
  return (
    <div className="flex items-center gap-2" title="Local / Remote (Loc.stVal)">
      <span className={`font-bold ${local ? 'text-red-500' : ''}`}>L</span>
      <SwitchPrimitive.Root
        checked={!local}
        onCheckedChange={(remote) => onChange(!remote)}
        disabled={disabled}
        className="bg-black inline-flex h-5 w-9 shrink-0 items-center rounded-full border-2 border-transparent disabled:opacity-50"
      >
        <SwitchPrimitive.Thumb className="bg-white block size-4 rounded-full shadow transition-transform data-[state=checked]:translate-x-4" />
      </SwitchPrimitive.Root>
      <span className={`font-bold ${!local ? 'text-red-500' : ''}`}>R</span>
    </div>
  );
}

/** Tombol "A" (auto) seperti acuan */
export function AutoButton({ on, onClick, disabled }: { on: boolean; onClick: () => void; disabled?: boolean }) {
  return (
    <Button
      title={on ? 'Auto (generator jalan)' : 'Manual'}
      className={`w-9 h-9 ${on ? 'bg-green-500 hover:bg-green-400 text-white' : 'bg-white hover:bg-gray-300 text-green-600'}`}
      onClick={onClick}
      disabled={disabled}
    >
      A
    </Button>
  );
}

export function QualityBadge({ q }: { q: Quality }) {
  const color = q.text.startsWith('good')
    ? 'bg-green-100 text-green-800 border-green-600'
    : q.text.startsWith('invalid')
      ? 'bg-red-100 text-red-800 border-red-600'
      : 'bg-yellow-100 text-yellow-800 border-yellow-600';
  return <span className={`px-1.5 rounded border text-xs font-mono ${color}`} title={`q = 0x${q.raw.toString(16)}`}>{q.text}</span>;
}

/** Jam lokal dengan milidetik, mis. 14:03:21.457 */
export function fmtTime(ms: number | null | undefined) {
  if (!ms) return '-';
  const d = new Date(ms);
  return `${d.toLocaleTimeString('id-ID', { hour12: false })}.${String(d.getMilliseconds()).padStart(3, '0')}`;
}

export function Meta({ q, t }: { q: Quality; t: number }) {
  return (
    <div className="flex items-center gap-2 text-xs text-gray-600">
      <QualityBadge q={q} />
      <span className="font-mono" title="timestamp t">{fmtTime(t)}</span>
    </div>
  );
}

export function SortableItem({ id, enabled, children }: { id: string; enabled: boolean; children: ReactNode }) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id, disabled: !enabled });
  return (
    <div
      ref={setNodeRef}
      style={{ transform: CSS.Transform.toString(transform), transition, opacity: isDragging ? 0.5 : 1, cursor: enabled ? 'grab' : 'default' }}
      {...(enabled ? { ...attributes, ...listeners } : {})}
    >
      {children}
    </div>
  );
}
