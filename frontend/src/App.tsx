import { useEffect, useState } from 'react';
import { Toaster, toast } from 'sonner';
import socket from './socket';
import type { EventLog, IedInfo } from './types';
import { IedList } from './pages/IedList';
import { IedDetail } from './pages/IedDetail';

/** Halaman dari URL hash: `#/ied/SIMIED1` = detail, selain itu daftar IED (bisa di-bookmark / dibagikan) */
function routeIed() {
  const m = window.location.hash.match(/^#\/ied\/(.+)$/);
  return m ? decodeURIComponent(m[1]) : null;
}

function App() {
  const [connected, setConnected] = useState(socket.connected);
  const [ieds, setIeds] = useState<IedInfo[]>([]);
  const [ied, setIed] = useState(routeIed);
  // log kejadian semua IED (backend kirim 200 terakhir saat terhubung, lalu real-time); dibatasi 3000 di UI
  const [events, setEvents] = useState<EventLog[]>([]);

  useEffect(() => {
    const onConnect = () => setConnected(true);
    const onDisconnect = () => setConnected(false);
    const onError = (e: { message: string }) => toast.error(e.message);
    const onHash = () => setIed(routeIed());
    // socketioxide mengirim array 1 elemen sebagai objek tunggal, jadi terima keduanya
    const onEvents = (data: EventLog[] | EventLog) =>
      setEvents((ev) => {
        const list = Array.isArray(data) ? data : [data];
        const last = ev.at(-1)?.id ?? 0;
        return [...ev, ...list.filter((e) => e.id > last)].slice(-3000); // id naik terus; abaikan duplikat saat reconnect
      });
    socket.on('event_log', onEvents);
    socket.on('connect', onConnect);
    socket.on('disconnect', onDisconnect);
    socket.on('ieds', setIeds);
    socket.on('error', onError);
    window.addEventListener('hashchange', onHash);
    return () => {
      socket.off('connect', onConnect);
      socket.off('disconnect', onDisconnect);
      socket.off('ieds', setIeds);
      socket.off('error', onError);
      socket.off('event_log', onEvents);
      window.removeEventListener('hashchange', onHash);
    };
  }, []);

  const go = (name: string | null) => {
    window.location.hash = name ? `#/ied/${encodeURIComponent(name)}` : '';
  };

  return (
    <div className="min-h-screen">
      <div className="flex justify-between items-center py-3 px-4 border-b">
        <button className="flex items-center space-x-2" onClick={() => go(null)} title="Daftar IED">
          <img src="/iec.ico" alt="IEC Icon" className="w-6 h-6" />
          <p className="text-2xl font-bold">IEC 61850 Simulator</p>
        </button>
        <span
          className={`flex items-center gap-2 px-3 py-1 rounded border text-sm font-medium ${
            connected ? 'border-green-600 text-green-700 bg-green-50' : 'border-red-600 text-red-700 bg-red-50'
          }`}
        >
          <span className={`w-2 h-2 rounded-full ${connected ? 'bg-green-600' : 'bg-red-600'}`} />
          {connected ? 'Tersambung' : 'Terputus'}
        </span>
      </div>
      {ied ? (
        <IedDetail name={ied} info={ieds.find((i) => i.name === ied)} back={() => go(null)} events={events} />
      ) : (
        <IedList ieds={ieds} open={go} />
      )}
      <Toaster richColors position="bottom-right" />
    </div>
  );
}

export default App;
