// Halaman Daftar IED (PLAN 8.1): status, client terhubung, mode report/polling, kejadian terakhir.
import type { IedInfo } from '../types';
import { Button, fmtTime } from '../ui';
import { runScenario, stopScenario } from '../api';

const STATUS = {
  running: 'bg-green-100 text-green-800 border-green-600',
  hang: 'bg-yellow-100 text-yellow-800 border-yellow-600',
  off: 'bg-red-100 text-red-800 border-red-600',
};
const small = 'px-2 h-8 text-xs bg-white hover:bg-gray-300';
const active = 'px-2 h-8 text-xs bg-red-500 hover:bg-red-400 text-white';

const MODE = {
  report: { text: 'report', cls: 'bg-green-100 text-green-800 border-green-600', hint: 'ada RCB aktif' },
  polling: { text: 'polling?', cls: 'bg-yellow-100 text-yellow-800 border-yellow-600', hint: 'client terhubung tanpa RCB aktif' },
  idle: { text: 'idle', cls: 'bg-gray-100 text-gray-600 border-gray-400', hint: 'tidak ada client' },
};

export function IedList({ ieds, open }: { ieds: IedInfo[]; open: (name: string) => void }) {
  const clients = ieds.reduce((n, i) => n + i.client_count, 0);
  return (
    <div className="p-4 overflow-auto h-[calc(100vh-60px)]">
      <p className="mb-3 text-sm text-gray-600">
        {ieds.length} IED, {clients} client terhubung
      </p>
      <table className="w-full text-sm border-2 border-black">
        <thead className="bg-gray-100 border-b-2 border-black text-left">
          <tr>
            <th className="p-2">IED</th>
            <th className="p-2">Port MMS</th>
            <th className="p-2">Status</th>
            <th className="p-2">Client</th>
            <th className="p-2">Mode</th>
            <th className="p-2">Komponen</th>
            <th className="p-2">Kejadian terakhir</th>
            <th className="p-2" />
          </tr>
        </thead>
        <tbody>
          {ieds.map((i) => {
            const m = MODE[i.mode];
            const mismatch = i.client_count !== i.ied_connections;
            return (
              <tr key={i.name} className="border-b hover:bg-gray-50 align-top">
                <td className="p-2 font-bold">{i.name}</td>
                <td className="p-2 font-mono">{i.port}</td>
                <td className="p-2">
                  <span className={`px-1.5 rounded border text-xs ${STATUS[i.status]}`}>{i.status}</span>
                  {i.delay_ms > 0 && <p className="text-xs text-yellow-700">delay {i.delay_ms} ms</p>}
                  {i.scenarios.quality && <p className="text-xs text-red-700">q {i.scenarios.quality}</p>}
                  {i.scenarios.clock && <p className="text-xs text-red-700">{i.scenarios.clock}</p>}
                  {i.scenarios.storm && <p className="text-xs text-orange-600">badai data</p>}
                </td>
                <td className="p-2">
                  <span className="font-bold">{i.client_count}</span>
                  {mismatch && (
                    <span className="ml-1 text-xs text-yellow-700" title="jumlah menurut libIEC61850 berbeda (sedang konek/putus)">
                      (IED: {i.ied_connections})
                    </span>
                  )}
                  {i.clients.map((c) => (
                    <p key={c.addr} className="font-mono text-xs text-gray-600" title={`sejak ${fmtTime(c.since)}`}>
                      {c.addr}
                    </p>
                  ))}
                </td>
                <td className="p-2">
                  <span className={`px-1.5 rounded border text-xs ${m.cls}`} title={m.hint}>
                    {m.text}
                  </span>
                  {i.rcbs.map((r) => (
                    <p key={r.rcb} className="font-mono text-xs text-gray-600">
                      {r.rcb} ← {r.client}
                    </p>
                  ))}
                </td>
                <td className="p-2">{i.components}</td>
                <td className="p-2 font-mono text-xs">{fmtTime(i.last_event)}</td>
                <td className="p-2">
                  <div className="flex gap-1 justify-end">
                    <Button className="px-3 h-8 bg-white hover:bg-gray-300" onClick={() => open(i.name)}>
                      Buka
                    </Button>
                    {i.status === 'off' ? (
                      <Button className={active} onClick={() => stopScenario(i.name, 'off')}>Hidupkan</Button>
                    ) : (
                      <Button className={small} onClick={() => runScenario(i.name, 'off')}>Mati</Button>
                    )}
                    {i.status === 'hang' ? (
                      <Button className={active} onClick={() => stopScenario(i.name, 'hang')}>Lanjutkan</Button>
                    ) : (
                      <Button className={small} disabled={i.status === 'off'} onClick={() => runScenario(i.name, 'hang')}>Hang</Button>
                    )}
                    <Button className={small} disabled={i.status === 'off' || i.client_count === 0} onClick={() => runScenario(i.name, 'disconnect')}>
                      Putus
                    </Button>
                  </div>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
