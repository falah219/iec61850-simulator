// Panel log kejadian (PLAN 8.2): filter jenis, auto-scroll, jeda, bersihkan.
import { useEffect, useRef, useState } from 'react';
import type { EventKind, EventLog } from '../types';
import { Button, fmtTime } from '../ui';

const KINDS: { k: EventKind; label: string; cls: string }[] = [
  { k: 'control', label: 'kontrol', cls: 'text-blue-700' },
  { k: 'report', label: 'report', cls: 'text-gray-500' },
  { k: 'rcb', label: 'rcb', cls: 'text-purple-700' },
  { k: 'connection', label: 'koneksi', cls: 'text-teal-700' },
  { k: 'scenario', label: 'skenario', cls: 'text-orange-600' },
  { k: 'manual', label: 'manual', cls: 'text-green-700' },
];

export function EventPanel({ events, ied }: { events: EventLog[]; ied: string }) {
  const [kinds, setKinds] = useState<EventKind[]>(KINDS.map((k) => k.k));
  const [paused, setPaused] = useState<EventLog[] | null>(null);
  const [clearedAt, setClearedAt] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  const list = (paused ?? events).filter((e) => e.ied === ied && e.id > clearedAt && kinds.includes(e.type));

  useEffect(() => {
    if (!paused && box.current) box.current.scrollTop = box.current.scrollHeight;
  }, [list.length, paused]);

  return (
    <div className="border-t-2 border-black flex flex-col h-56">
      <div className="flex items-center gap-3 px-3 py-1 border-b bg-gray-50 text-xs">
        <span className="font-semibold text-sm">Log kejadian</span>
        {KINDS.map(({ k, label, cls }) => (
          <label key={k} className={`flex items-center gap-1 ${cls}`}>
            <input type="checkbox" checked={kinds.includes(k)} onChange={(e) => setKinds(e.target.checked ? [...kinds, k] : kinds.filter((x) => x !== k))} />
            {label}
          </label>
        ))}
        <div className="ml-auto flex gap-1">
          <Button className={`px-2 h-7 text-xs ${paused ? 'bg-yellow-300' : 'bg-white hover:bg-gray-200'}`} onClick={() => setPaused(paused ? null : events)}>
            {paused ? 'Lanjut' : 'Jeda'}
          </Button>
          <Button className="px-2 h-7 text-xs bg-white hover:bg-gray-200" onClick={() => setClearedAt(events.at(-1)?.id ?? 0)}>
            Bersihkan
          </Button>
        </div>
      </div>
      <div ref={box} className="flex-1 overflow-y-auto font-mono text-xs px-3 py-1">
        {list.map((e) => (
          <div key={e.id} className="flex gap-2">
            <span className="text-gray-500 shrink-0">{fmtTime(e.ts)}</span>
            <span className={`w-16 shrink-0 ${KINDS.find((k) => k.k === e.type)?.cls ?? ''}`}>{e.type}</span>
            <span className="break-all">{e.msg}</span>
          </div>
        ))}
        {list.length === 0 && <p className="text-gray-400">Belum ada kejadian</p>}
      </div>
    </div>
  );
}
