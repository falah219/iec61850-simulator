// Kartu komponen, gaya meniru CircuitBreakerItem / TapChangerItem / TeleSignalItem / TeleMetryItem acuan.
import { useEffect, useState } from 'react';
import type { CbComp, SetCmd, TapComp, TmComp, TsComp } from '../types';
import { AutoButton, Button, LocalRemote, Meta } from '../ui';

type Props<T> = { c: T; send: (cmd: SetCmd) => void; dragging: boolean };

const POS_TEXT = { open: 'OPEN', close: 'CLOSE', intermediate: 'INTERMEDIATE (00)', bad: 'BAD STATE (11)' };

export function CircuitBreaker({ c, send, dragging }: Props<CbComp>) {
  // seperti acuan: hijau = open, merah = close; bad state = keduanya menyala
  const green = c.pos === 'open' || c.pos === 'bad';
  const red = c.pos === 'close' || c.pos === 'bad';
  const panel = !c.local || dragging; // operasi panel hanya saat Local
  return (
    <div>
      <div className="text-center py-2 border-b">
        <p className="font-bold text-lg">{c.id}</p>
        <p className="text-xs text-gray-500 font-mono">{c.ln}</p>
      </div>
      <div className="flex flex-row border-b-2 py-2">
        <div className="flex flex-col my-2 mx-6 gap-2">
          <div className="flex w-full justify-around gap-3">
            <div className={`w-20 h-20 rounded-full border-2 border-green-600 ${green ? 'bg-green-600' : 'bg-green-200 opacity-50'}`} />
            <div className={`w-20 h-20 rounded-full border-2 border-red-600 ${red ? 'bg-red-600' : 'bg-red-200 opacity-50'}`} />
          </div>
          <p className={`text-center text-sm font-bold ${c.pos === 'intermediate' ? 'text-yellow-600 animate-pulse' : ''}`}>{POS_TEXT[c.pos]}</p>
          <div className="flex flex-row gap-2 justify-around">
            <Button className="text-xs w-12 h-12 rounded-full bg-green-600 text-white" disabled={panel} onClick={() => send({ action: 'open' })}>
              Open
            </Button>
            <Button className="text-xs w-12 h-12 rounded-full bg-red-600 text-white" disabled={panel} onClick={() => send({ action: 'close' })}>
              Close
            </Button>
          </div>
        </div>
        <div className="flex flex-col justify-center gap-2 text-sm">
          <p>ctlModel: <span className="font-bold">{c.ctl_model}</span></p>
          <p>Tempuh: <span className="font-bold">{c.travel_ms} ms</span></p>
          {c.sbo_timeout_ms !== null && <p>sboTimeout: <span className="font-bold">{c.sbo_timeout_ms} ms</span></p>}
          <p>
            Interlock: <span className="font-bold">{c.interlock ? 'aktif' : 'tidak'}</span>
            {c.accept_same && <span className="ml-2 text-xs text-gray-500">(terima posisi sama)</span>}
          </p>
          <LocalRemote local={c.local} onChange={(local) => send({ local })} disabled={dragging} />
          <Meta q={c.q} t={c.t} />
        </div>
      </div>
    </div>
  );
}

export function TapChanger({ c, send, dragging }: Props<TapComp>) {
  const panel = !c.local || dragging;
  return (
    <div>
      <div className="text-center py-2 border-b">
        <p className="font-bold text-lg">{c.id}</p>
        <p className="text-xs text-gray-500 font-mono">{c.ln}</p>
      </div>
      <div className="flex flex-row border-b-2 py-2 mb-1.5">
        <div className="flex flex-col my-2 mx-6 gap-2">
          <p className="text-6xl text-center">{c.pos}</p>
          <div className="flex flex-row gap-4 justify-around my-1">
            <Button className="text-xs w-12 h-12 rounded-full bg-green-600 text-white" disabled={panel || c.pos >= c.max} onClick={() => send({ action: 'raise' })}>
              Raise
            </Button>
            <Button className="text-xs w-12 h-12 rounded-full bg-red-600 text-white" disabled={panel || c.pos <= c.min} onClick={() => send({ action: 'lower' })}>
              Lower
            </Button>
          </div>
        </div>
        <div className="flex flex-col justify-center gap-2 text-sm">
          <p>ctlModel: <span className="font-bold">{c.ctl_model}</span></p>
          <p>Batas: <span className="font-bold">{c.min} .. {c.max}</span></p>
          <p>Interval auto: <span className="font-bold">{c.interval_ms} ms</span></p>
          <div className="flex items-center gap-4">
            <LocalRemote local={c.local} onChange={(local) => send({ local })} disabled={dragging} />
            <AutoButton on={c.auto} onClick={() => send({ auto: !c.auto })} disabled={panel} />
          </div>
          <Meta q={c.q} t={c.t} />
        </div>
      </div>
    </div>
  );
}

export function TeleSignal({ c, send, dragging }: Props<TsComp>) {
  const auto = c.auto === true;
  return (
    <div className="p-3 flex items-center text-center border-b-2">
      <div className="w-1/3">
        <p className="font-bold">{c.id}</p>
        <p className="text-xs text-gray-500 font-mono">{c.ref}</p>
      </div>
      <div className="flex flex-col w-1/3 items-center gap-1">
        <p className={`${c.value ? 'text-green-500' : 'text-red-500'} text-2xl font-bold`}>{c.value ? 'ON' : 'OFF'}</p>
        <Meta q={c.q} t={c.t} />
      </div>
      <div className="flex w-1/3 gap-1 justify-center">
        {c.auto !== null && c.gen.mode !== 'manual' && (
          <AutoButton on={auto} onClick={() => send({ auto: !auto })} disabled={dragging} />
        )}
        <Button
          className={`w-12 h-9 text-white ${c.value ? 'bg-green-500' : 'bg-red-500'}`}
          onClick={() => send({ value: !c.value })}
          disabled={auto || dragging}
          title={auto ? `auto ${c.gen.mode} tiap ${c.gen.interval_ms} ms` : 'toggle'}
        >
          {c.value ? 'ON' : 'OFF'}
        </Button>
      </div>
    </div>
  );
}

export function Telemetry({ c, send, dragging }: Props<TmComp>) {
  const auto = c.auto === true;
  const hasGen = c.gen.mode !== 'manual';
  const [draft, setDraft] = useState(String(c.value));
  const [editing, setEditing] = useState(false);
  useEffect(() => {
    if (!editing) setDraft(String(c.value));
  }, [c.value, editing]);
  const commit = () => {
    setEditing(false);
    const v = Number(draft);
    if (draft.trim() !== '' && Number.isFinite(v) && v !== c.value) send({ value: v });
    else setDraft(String(c.value));
  };
  const step = c.gen.scale > 0 ? c.gen.scale : (c.gen.max - c.gen.min) / 100 || 1;
  return (
    <div className="p-3 flex items-center text-center border-b-2">
      <div className="w-1/3">
        <p className="font-bold">{c.id}</p>
        <p className="text-xs text-gray-500 font-mono">{c.ref}</p>
        {c.follow_cb && <p className="text-xs text-gray-500">ikut {c.follow_cb}</p>}
      </div>
      <div className="flex flex-col w-1/3 items-center gap-1">
        <p className="text-2xl font-bold">
          {c.value} {c.unit}
        </p>
        <Meta q={c.q} t={c.t} />
        {hasGen && (
          <p className="text-xs text-gray-500">
            {c.gen.mode} {c.gen.min}..{c.gen.max}, {c.gen.interval_ms} ms
          </p>
        )}
      </div>
      <div className="flex flex-col w-1/3 gap-1 items-center">
        <div className="flex gap-1 items-center">
          <input
            type="number"
            step={step}
            value={draft}
            disabled={auto || dragging}
            onFocus={() => setEditing(true)}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={commit}
            onKeyDown={(e) => e.key === 'Enter' && (e.target as HTMLInputElement).blur()}
            className="w-24 h-9 px-2 border-2 border-black rounded text-right disabled:opacity-50"
            title="Nilai manual (Enter untuk kirim)"
          />
          {hasGen && <AutoButton on={auto} onClick={() => send({ auto: !auto })} disabled={dragging} />}
        </div>
        {hasGen && (
          <input
            type="range"
            min={c.gen.min}
            max={c.gen.max}
            step={step}
            value={Math.min(Math.max(editing ? Number(draft) || 0 : c.value, c.gen.min), c.gen.max)}
            disabled={auto || dragging}
            // nilai ditahan lokal selama digeser, dikirim saat dilepas
            onChange={(e) => {
              setEditing(true);
              setDraft(e.target.value);
            }}
            onPointerUp={commit}
            onKeyUp={commit}
            className="w-32 disabled:opacity-50"
          />
        )}
      </div>
    </div>
  );
}
