// Halaman Detail IED (PLAN 8.2): 3 kolom seperti acuan, nilai real-time (q & t), ubah manual, drag-and-drop.
import { useEffect, useState } from 'react';
import { DndContext, closestCenter, type DragEndEvent } from '@dnd-kit/core';
import { SortableContext, arrayMove, verticalListSortingStrategy } from '@dnd-kit/sortable';
import { TbDragDrop } from 'react-icons/tb';
import { MdOutlineCheck, MdArrowBack } from 'react-icons/md';
import { toast } from 'sonner';
import socket from '../socket';
import type { Comp, IedInfo, SetCmd } from '../types';
import { Button, SortableItem } from '../ui';
import { CircuitBreaker, TapChanger, TeleSignal, Telemetry } from '../components/Cards';

type Ack = { ok: boolean; message: string };

const COLUMNS: { title: string; types: Comp['type'][] }[] = [
  { title: 'Circuit Breakers & Tap Changers', types: ['cb', 'tap'] },
  { title: 'Telesignals', types: ['telesignal'] },
  { title: 'Telemetries', types: ['telemetry'] },
];

export function IedDetail({ name, info, back }: { name: string; info?: IedInfo; back: () => void }) {
  const [comps, setComps] = useState<Comp[]>([]);
  const [dragging, setDragging] = useState(false);

  useEffect(() => {
    const subscribe = () => socket.emit('subscribe_ied', { ied: name });
    const onComponents = (d: { ied: string; components: Comp[] }) => d.ied === name && setComps(d.components);
    const onValues = (d: { ied: string; values: Partial<Comp>[] }) => {
      if (d.ied !== name) return;
      setComps((cs) => cs.map((c) => ({ ...c, ...d.values.find((v) => v.id === c.id) }) as Comp));
    };
    subscribe();
    socket.on('connect', subscribe); // backend restart / reconnect: masuk room lagi
    socket.on('components', onComponents);
    socket.on('value_changed', onValues);
    return () => {
      socket.emit('unsubscribe_ied');
      socket.off('connect', subscribe);
      socket.off('components', onComponents);
      socket.off('value_changed', onValues);
    };
  }, [name]);

  const send = (id: string) => (cmd: SetCmd) =>
    socket.emit('set_value', { ied: name, id, ...cmd }, (r: Ack) => !r.ok && toast.error(r.message));

  // Drag di satu kolom: urutan kolom itu diganti, posisi kolom lain tetap
  const onDragEnd = (types: Comp['type'][]) => (e: DragEndEvent) => {
    if (!e.over || e.active.id === e.over.id) return;
    const inCol = comps.filter((c) => types.includes(c.type));
    const from = inCol.findIndex((c) => c.id === e.active.id);
    const to = inCol.findIndex((c) => c.id === e.over!.id);
    const moved = arrayMove(inCol, from, to);
    let k = 0;
    const next = comps.map((c) => (types.includes(c.type) ? moved[k++] : c));
    setComps(next);
    socket.emit('update_order', { ied: name, order: next.map((c) => c.id) }, (r: Ack) => !r.ok && toast.error(r.message));
  };

  return (
    <div>
      <div className="flex items-center justify-between px-4 py-2 border-b">
        <div className="flex items-center gap-3">
          <Button className="px-2 h-8 bg-white hover:bg-gray-300" onClick={back} title="Kembali ke daftar IED">
            <MdArrowBack />
          </Button>
          <p className="text-xl font-bold">{name}</p>
          {info && (
            <span className="text-sm text-gray-600">
              port {info.port} · {info.client_count} client · mode {info.mode}
              {info.rcbs.length > 0 && ` (${info.rcbs.map((r) => r.rcb).join(', ')})`}
            </span>
          )}
        </div>
        <Button
          className={`px-2 h-8 ${dragging ? 'bg-blue-400' : 'bg-white hover:bg-gray-300'}`}
          onClick={() => setDragging(!dragging)}
          title={dragging ? 'Selesai' : 'Mode drag and drop (atur urutan kartu)'}
        >
          {dragging ? <MdOutlineCheck /> : <TbDragDrop />}
        </Button>
      </div>

      <div className="flex flex-row w-full">
        {COLUMNS.map(({ title, types }) => {
          const items = comps.filter((c) => types.includes(c.type));
          return (
            <div key={title} className="w-1/3 border-2 flex flex-col h-[calc(100vh-110px)]">
              <div className="flex border-b-2 justify-center">
                <h2 className="text-xl font-semibold m-2">{title}</h2>
              </div>
              <div className="flex-1 overflow-y-auto">
                <DndContext collisionDetection={closestCenter} onDragEnd={onDragEnd(types)}>
                  <SortableContext items={items.map((c) => c.id)} strategy={verticalListSortingStrategy}>
                    {items.map((c) => (
                      <SortableItem key={c.id} id={c.id} enabled={dragging}>
                        {c.type === 'cb' && <CircuitBreaker c={c} send={send(c.id)} dragging={dragging} />}
                        {c.type === 'tap' && <TapChanger c={c} send={send(c.id)} dragging={dragging} />}
                        {c.type === 'telesignal' && <TeleSignal c={c} send={send(c.id)} dragging={dragging} />}
                        {c.type === 'telemetry' && <Telemetry c={c} send={send(c.id)} dragging={dragging} />}
                      </SortableItem>
                    ))}
                  </SortableContext>
                </DndContext>
                {items.length === 0 && <p className="p-4 text-center text-gray-400">Tidak ada komponen</p>}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
