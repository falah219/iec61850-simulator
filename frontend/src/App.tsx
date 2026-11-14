import { useEffect, useState } from 'react';
import socket from './socket';

function App() {
  const [connected, setConnected] = useState(socket.connected);
  const [ieds, setIeds] = useState<unknown[]>([]);

  useEffect(() => {
    const onConnect = () => setConnected(true);
    const onDisconnect = () => setConnected(false);
    socket.on('connect', onConnect);
    socket.on('disconnect', onDisconnect);
    socket.on('ieds', setIeds);
    return () => {
      socket.off('connect', onConnect);
      socket.off('disconnect', onDisconnect);
      socket.off('ieds', setIeds);
    };
  }, []);

  return (
    <div className="min-h-screen">
      <div className="flex justify-between items-center py-3 px-4 border-b">
        <div className="flex items-center space-x-2">
          <img src="/iec.ico" alt="IEC Icon" className="w-6 h-6" />
          <p className="text-2xl font-bold">IEC 61850 Simulator</p>
        </div>
        <span
          className={`flex items-center gap-2 px-3 py-1 rounded border text-sm font-medium ${
            connected ? 'border-green-600 text-green-700 bg-green-50' : 'border-red-600 text-red-700 bg-red-50'
          }`}
        >
          <span className={`w-2 h-2 rounded-full ${connected ? 'bg-green-600' : 'bg-red-600'}`} />
          {connected ? 'Tersambung' : 'Terputus'}
        </span>
      </div>
      <p className="p-4 text-gray-500">{ieds.length} IED</p>
    </div>
  );
}

export default App;
