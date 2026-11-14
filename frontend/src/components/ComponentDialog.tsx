// Dialog tambah / ubah komponen (PLAN 8.2). Menghasilkan JSON konfigurasi komponen (format export/import);
// backend memvalidasi, mengisi nomor otomatis, lalu me-restart IED.
import { useState, type ReactNode } from 'react';
import type { Comp } from '../types';
import { call } from '../api';
import { Button } from '../ui';

type Cfg = Record<string, any>; // eslint-disable-line @typescript-eslint/no-explicit-any

const TYPES = [
  { v: 'cb', label: 'Circuit Breaker' },
  { v: 'tap', label: 'Tap Changer' },
  { v: 'telesignal', label: 'Telesignal' },
  { v: 'telemetry', label: 'Telemetry' },
  { v: 'protection', label: 'Proteksi' },
];
const CTL = ['status-only', 'direct-normal', 'sbo-normal', 'direct-enhanced', 'sbo-enhanced'];
// DO MMXU yang valid (sama dengan backend model.rs) + fasa per CDC
const MV = ['TotW', 'TotVAr', 'TotVA', 'TotPF', 'Hz'];
const WYE = ['PhV', 'A', 'W', 'VAr', 'VA', 'PF', 'Z'];
const PHASES: Record<string, string[]> = { WYE: ['phsA', 'phsB', 'phsC', 'neut', 'net', 'res'], DEL: ['phsAB', 'phsBC', 'phsCA'] };
const cdc = (d: string) => (MV.includes(d) ? 'MV' : WYE.includes(d) ? 'WYE' : d === 'PPV' ? 'DEL' : '');
const PRESETS: Record<string, { do: string; phase?: boolean }> = {
  MW: { do: 'TotW' },
  MVAR: { do: 'TotVAr' },
  Hz: { do: 'Hz' },
  kV: { do: 'PhV', phase: true },
  A: { do: 'A', phase: true },
};

const GEN = { mode: 'random_walk', interval_ms: 1000, min: 0, max: 100, scale: 0.1, step: 5, period_s: 60 };

function defaults(type: string, n: number): Cfg {
  const id = `${{ cb: 'cb', tap: 'tap', telesignal: 'ts', telemetry: 'tm', protection: 'prot' }[type]}${n}`;
  switch (type) {
    case 'cb':
      return { id, type, closed: true, ctl_model: 'sbo-enhanced', travel_ms: 100, local: false, interlock: false, accept_same: false, sbo_timeout_ms: 30000 };
    case 'tap':
      return { id, type, pos: 1, min: 1, max: 17, ctl_model: 'direct-enhanced', local: false, auto: false, interval_ms: 5000 };
    case 'telesignal':
      return { id, type, ggio: 1, value: false, gen: { ...GEN, mode: 'manual', interval_ms: 5000 } };
    case 'telemetry':
      return { id, type, mx: { class: 'MMXU', inst: 1, do: 'TotW', phase: null }, value: 0, gen: { ...GEN }, follow_cb: null };
    default:
      return { id, type, class: 'PTOC', cb: '' };
  }
}

/** Preset telemetry dari mx (untuk dialog ubah) */
function presetOf(mx: Cfg): string {
  if (mx.class === 'GGIO') return 'generic';
  const p = Object.entries(PRESETS).find(([, v]) => v.do === mx.do && (v.phase ? ['phsA', 'phsB', 'phsC'].includes(mx.phase) : !mx.phase));
  return p ? p[0] : 'custom';
}

export function ComponentDialog({ ied, comp, comps, onClose }: { ied: string; comp?: Comp; comps: Comp[]; onClose: () => void }) {
  const editing = comp !== undefined;
  const [c, setC] = useState<Cfg>(() => (comp ? structuredClone((comp as Cfg).config) : defaults('cb', comps.length + 1)));
  const [preset, setPreset] = useState(() => (c.type === 'telemetry' ? presetOf(c.mx) : 'MW'));
  const [busy, setBusy] = useState(false);
  const set = (path: string, v: unknown) =>
    setC((old) => {
      const n = structuredClone(old);
      const keys = path.split('.');
      keys.slice(0, -1).reduce((o, k) => o[k], n)[keys.at(-1)!] = v;
      return n;
    });
  const cbs = comps.filter((x) => x.type === 'cb').map((x) => x.id);
  const isSbo = String(c.ctl_model).startsWith('sbo');

  const choosePreset = (p: string) => {
    setPreset(p);
    if (p === 'generic') set('mx', { class: 'GGIO', inst: 1, do: 'AnIn', phase: null });
    else if (p !== 'custom') set('mx', { class: 'MMXU', inst: c.mx.inst || 1, do: PRESETS[p].do, phase: PRESETS[p].phase ? 'phsA' : null });
  };

  const save = async () => {
    setBusy(true);
    const r = editing ? await call('update_component', { ied, id: comp.id, component: c }) : await call('add_component', { ied, component: c });
    setBusy(false);
    if (r.ok) onClose();
  };

  const num = (path: string, label: string, extra: object = {}) => (
    <Field label={label}>
      <input type="number" className={inp} value={get(c, path) ?? ''} onChange={(e) => set(path, e.target.value === '' ? 0 : Number(e.target.value))} {...extra} />
    </Field>
  );
  const chk = (path: string, label: string) => (
    <label className="flex items-center gap-2 text-sm">
      <input type="checkbox" checked={!!get(c, path)} onChange={(e) => set(path, e.target.checked)} />
      {label}
    </label>
  );
  const sel = (path: string, label: string, opts: string[]) => (
    <Field label={label}>
      <select className={inp} value={get(c, path) ?? ''} onChange={(e) => set(path, e.target.value)}>
        {opts.map((o) => (
          <option key={o} value={o}>
            {o}
          </option>
        ))}
      </select>
    </Field>
  );
  const locked = (label: string, v: unknown) => editing && <Field label={label}><span className="font-mono text-sm">{String(v)} (dikunci)</span></Field>;

  return (
    <Modal title={editing ? `Ubah ${comp.id}` : 'Tambah komponen'} onClose={onClose}>
      <div className="grid grid-cols-2 gap-x-4 gap-y-2">
        <Field label="Jenis">
          <select className={inp} value={c.type} disabled={editing} onChange={(e) => setC(defaults(e.target.value, comps.length + 1))}>
            {TYPES.map((t) => (
              <option key={t.v} value={t.v}>
                {t.label}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Id (nama di UI)">
          <input className={inp} value={c.id} onChange={(e) => set('id', e.target.value)} />
        </Field>

        {c.type === 'cb' && (
          <>
            {locked('Instance XCBR/CSWI', c.inst)}
            {sel('ctl_model', 'ctlModel', CTL)}
            {num('travel_ms', 'Waktu tempuh (ms)', { min: 0, max: 60000 })}
            {isSbo && num('sbo_timeout_ms', 'sboTimeout (ms)', { min: 1000, max: 600000 })}
            <div className="col-span-2 flex flex-wrap gap-4">
              {chk('closed', 'Awal close')}
              {chk('local', 'Local')}
              {chk('interlock', 'Interlock aktif')}
              {chk('accept_same', 'Terima perintah ke posisi sama')}
            </div>
          </>
        )}

        {c.type === 'tap' && (
          <>
            {locked('Instance YLTC/ATCC', c.inst)}
            {sel('ctl_model', 'ctlModel', CTL)}
            {num('pos', 'Posisi awal')}
            {num('min', 'Batas bawah', { min: -64, max: 63 })}
            {num('max', 'Batas atas', { min: -64, max: 63 })}
            {num('interval_ms', 'Interval auto (ms)', { min: 100 })}
            <div className="col-span-2 flex gap-4">
              {chk('local', 'Local')}
              {chk('auto', 'Auto')}
            </div>
          </>
        )}

        {c.type === 'telesignal' && (
          <>
            {num('ggio', 'Instance GGIO', { min: 1 })}
            {locked('Ind', c.ind)}
            {sel('gen.mode', 'Mode', ['manual', 'toggle'])}
            {c.gen.mode === 'toggle' && num('gen.interval_ms', 'Interval toggle (ms)', { min: 100 })}
            <div className="col-span-2">{chk('value', 'Awal ON')}</div>
          </>
        )}

        {c.type === 'telemetry' && (
          <>
            <Field label="Preset">
              <select className={inp} value={preset} onChange={(e) => choosePreset(e.target.value)}>
                {['MW', 'MVAR', 'Hz', 'kV', 'A', 'generic', 'custom'].map((p) => (
                  <option key={p} value={p}>
                    {p === 'generic' ? 'Generik (GGIO.AnIn)' : p === 'kV' ? 'kV fasa' : p === 'A' ? 'A fasa' : p}
                  </option>
                ))}
              </select>
            </Field>
            {num('mx.inst', preset === 'generic' ? 'Instance GGIO' : 'Instance MMXU', { min: 1 })}
            {preset === 'custom' && (
              <Field label="DO MMXU">
                <select
                  className={inp}
                  value={c.mx.do}
                  onChange={(e) => set('mx', { ...c.mx, class: 'MMXU', do: e.target.value, phase: PHASES[cdc(e.target.value)]?.[0] ?? null })}
                >
                  {[...MV, ...WYE, 'PPV'].map((d) => (
                    <option key={d} value={d}>
                      {d} ({cdc(d)})
                    </option>
                  ))}
                </select>
              </Field>
            )}
            {(PRESETS[preset]?.phase || (preset === 'custom' && PHASES[cdc(c.mx.do)])) &&
              sel('mx.phase', 'Fasa', preset === 'custom' ? PHASES[cdc(c.mx.do)] : ['phsA', 'phsB', 'phsC'])}
            {preset === 'generic' && <Field label="AnIn">{editing && c.mx.do !== 'AnIn' ? `${c.mx.do} (dikunci)` : 'otomatis'}</Field>}
            {num('value', 'Nilai awal')}
            {sel('gen.mode', 'Mode generator', ['manual', 'random', 'random_walk', 'sine'])}
            {c.gen.mode !== 'manual' && (
              <>
                {num('gen.interval_ms', 'Interval (ms)', { min: 100 })}
                {num('gen.min', 'Min')}
                {num('gen.max', 'Max')}
                {num('gen.scale', 'Scale (kelipatan, 0 = bebas)', { step: 'any', min: 0 })}
                {c.gen.mode === 'random_walk' && num('gen.step', 'Langkah maks', { step: 'any', min: 0 })}
                {c.gen.mode === 'sine' && num('gen.period_s', 'Periode (s)', { min: 1 })}
              </>
            )}
            <Field label="Ikut CB (0 saat CB tidak close)">
              <select className={inp} value={c.follow_cb ?? ''} onChange={(e) => set('follow_cb', e.target.value || null)}>
                <option value="">-</option>
                {cbs.map((x) => (
                  <option key={x}>{x}</option>
                ))}
              </select>
            </Field>
          </>
        )}

        {c.type === 'protection' && (
          <>
            {sel('class', 'Kelas', ['PTOC', 'PDIS'])}
            {locked('Instance', c.inst)}
            <Field label="CB yang di-trip">
              <select className={inp} value={c.cb} onChange={(e) => set('cb', e.target.value)}>
                <option value="">pilih CB</option>
                {cbs.map((x) => (
                  <option key={x}>{x}</option>
                ))}
              </select>
            </Field>
          </>
        )}
      </div>

      <p className="mt-4 p-2 text-sm bg-yellow-50 border border-yellow-500 rounded">
        Menyimpan akan me-restart {ied}: client (FEP) terputus, nilai kembali ke awal. Nomor instance / Ind / AnIn diisi otomatis dan tidak dipakai ulang.
      </p>
      <div className="mt-4 flex justify-end gap-2">
        <Button className="px-4 h-9 bg-white hover:bg-gray-200" onClick={onClose}>
          Batal
        </Button>
        <Button className="px-4 h-9 bg-blue-600 hover:bg-blue-500 text-white" disabled={busy} onClick={save}>
          {busy ? 'Menyimpan…' : 'Simpan & restart'}
        </Button>
      </div>
    </Modal>
  );
}

const inp = 'w-full h-9 px-2 border-2 border-black rounded text-sm disabled:opacity-50';

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function get(o: Cfg, path: string): any {
  return path.split('.').reduce<Cfg | undefined>((x, k) => x?.[k], o);
}

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="flex flex-col gap-1 text-sm">
      <span className="text-gray-600">{label}</span>
      {children}
    </label>
  );
}

/** Dialog sederhana (overlay + kartu), tutup dengan Esc / klik di luar */
export function Modal({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  return (
    <div className="fixed inset-0 z-50 bg-black/40 flex items-center justify-center p-4" onMouseDown={onClose} onKeyDown={(e) => e.key === 'Escape' && onClose()}>
      <div className="bg-white border-2 border-black rounded-lg w-full max-w-2xl max-h-[90vh] overflow-y-auto p-5" onMouseDown={(e) => e.stopPropagation()}>
        <h2 className="text-xl font-bold mb-4">{title}</h2>
        {children}
      </div>
    </div>
  );
}
