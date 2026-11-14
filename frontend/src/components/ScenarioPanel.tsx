// Panel skenario per IED (PLAN bagian 6 & 8.2). Semua tombol memanggil run_scenario / stop_scenario.
import { useState } from 'react';
import type { IedInfo } from '../types';
import { runScenario, stopScenario } from '../api';
import { Button } from '../ui';

const FLAGS = ['oldData', 'substituted', 'test', 'failure', 'inaccurate', 'operatorBlocked'];

const btn = 'px-2 h-8 bg-white hover:bg-gray-200 text-xs';
const on = 'px-2 h-8 bg-red-500 hover:bg-red-400 text-white text-xs';

export function ScenarioPanel({ ied, info, prots }: { ied: string; info?: IedInfo; prots: string[] }) {
  const [prot, setProt] = useState('');
  const [validity, setValidity] = useState('invalid');
  const [flags, setFlags] = useState<string[]>([]);
  const [delay, setDelay] = useState(1000);
  const [storm, setStorm] = useState(30);
  const [rate, setRate] = useState(50);
  const [cut, setCut] = useState('fin');
  const run = (s: string, p: object = {}) => runScenario(ied, s, p);
  const stop = (s: string) => stopScenario(ied, s);
  const off = info?.status === 'off';
  const hang = info?.status === 'hang';
  const sc = info?.scenarios;

  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-2 border-b bg-gray-50 text-sm">
      <span className="font-semibold">Skenario:</span>

      <Group title="Trip: Str & Op proteksi naik, Op pulse 200 ms, CB terkait open lewat intermediate">
        <select className="h-8 border-2 border-black rounded text-xs" value={prot} onChange={(e) => setProt(e.target.value)} disabled={prots.length === 0}>
          {prots.length === 0 && <option>tanpa proteksi</option>}
          {prots.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
        <Button className="px-2 h-8 bg-orange-500 hover:bg-orange-400 text-white text-xs" disabled={off || prots.length === 0} onClick={() => run('trip', { prot: prot || prots[0] })}>
          Trip
        </Button>
      </Group>

      <Group title="IED mati: server & proxy berhenti, port ditutup">
        {off ? (
          <Button className={on} onClick={() => stop('off')}>Hidupkan</Button>
        ) : (
          <Button className={btn} onClick={() => run('off')}>IED mati</Button>
        )}
      </Group>

      <Group title="Hang: proxy berhenti meneruskan data, TCP tetap terbuka">
        {hang ? (
          <Button className={on} onClick={() => stop('hang')}>Lanjutkan</Button>
        ) : (
          <Button className={btn} disabled={off} onClick={() => run('hang')}>Hang</Button>
        )}
      </Group>

      <Group title="Putus koneksi: semua client ditutup (FIN normal / RST paksa), IED tetap hidup">
        <select className="h-8 border-2 border-black rounded text-xs" value={cut} onChange={(e) => setCut(e.target.value)}>
          <option value="fin">FIN</option>
          <option value="rst">RST</option>
        </select>
        <Button className={btn} disabled={off} onClick={() => run('disconnect', { mode: cut })}>Putus koneksi</Button>
      </Group>

      <Group title="Delay jaringan: jeda N ms setiap data yang diteruskan proxy">
        <input type="number" min={1} max={60000} value={delay} onChange={(e) => setDelay(Number(e.target.value))} className="w-20 h-8 px-1 border-2 border-black rounded text-xs" />
        <span className="text-xs">ms</span>
        {info?.delay_ms ? (
          <Button className={on} onClick={() => stop('delay')}>Stop delay ({info.delay_ms})</Button>
        ) : (
          <Button className={btn} disabled={off} onClick={() => run('delay', { ms: delay })}>Delay</Button>
        )}
      </Group>

      <Group title="Quality semua komponen (per titik: lewat PUT /points)">
        <select className="h-8 border-2 border-black rounded text-xs" value={validity} onChange={(e) => setValidity(e.target.value)}>
          <option value="invalid">invalid</option>
          <option value="questionable">questionable</option>
          <option value="good">good</option>
        </select>
        {FLAGS.map((f) => (
          <label key={f} className="flex items-center gap-0.5 text-xs">
            <input type="checkbox" checked={flags.includes(f)} onChange={(e) => setFlags(e.target.checked ? [...flags, f] : flags.filter((x) => x !== f))} />
            {f}
          </label>
        ))}
        <Button className={btn} disabled={off} onClick={() => run('quality', { validity, flags })}>Terapkan</Button>
        {sc?.quality && <Button className={on} onClick={() => stop('quality')}>Normalkan ({sc.quality})</Button>}
      </Group>

      <Group title="Time quality di timestamp berikutnya">
        {sc?.clock ? (
          <Button className={on} onClick={() => stop('clock')}>Clock normal ({sc.clock})</Button>
        ) : (
          <>
            <Button className={btn} disabled={off} onClick={() => run('clock', { not_sync: true })}>Clock tidak sinkron</Button>
            <Button className={btn} disabled={off} onClick={() => run('clock', { failure: true, not_sync: false })}>Clock failure</Button>
          </>
        )}
      </Group>

      <Group title="Badai data: semua telemetry & telesignal berubah <rate> kali per detik selama <durasi>">
        <input type="number" min={1} max={1000} value={rate} onChange={(e) => setRate(Number(e.target.value))} className="w-14 h-8 px-1 border-2 border-black rounded text-xs" />
        <span className="text-xs">/s</span>
        <input type="number" min={1} max={600} value={storm} onChange={(e) => setStorm(Number(e.target.value))} className="w-14 h-8 px-1 border-2 border-black rounded text-xs" />
        <span className="text-xs">s</span>
        {sc?.storm ? (
          <Button className={on} onClick={() => stop('storm')}>Stop badai</Button>
        ) : (
          <Button className={btn} disabled={off} onClick={() => run('storm', { rate, duration: storm })}>Badai data</Button>
        )}
      </Group>
    </div>
  );
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center gap-1 border-l pl-3" title={title}>
      {children}
    </div>
  );
}
