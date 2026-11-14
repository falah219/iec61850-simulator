// Halaman Detail IED (PLAN 8.2): 3 kolom seperti acuan, nilai real-time (q & t), ubah manual, drag-and-drop.
import { useEffect, useState } from 'react';
import { DndContext, closestCenter, type DragEndEvent } from '@dnd-kit/core';
import { SortableContext, arrayMove, verticalListSortingStrategy } from '@dnd-kit/sortable';
import { TbDragDrop } from 'react-icons/tb';
import { MdOutlineCheck, MdArrowBack, MdAdd, MdOutlineModeEditOutline, MdOutlineEdit, MdDeleteOutline } from 'react-icons/md';
import { CgImport } from 'react-icons/cg';
import { PiExportBold } from 'react-icons/pi';
import { ComponentDialog } from '../components/ComponentDialog';
import socket from '../socket';
import type { Comp, EventLog, IedInfo, SetCmd } from '../types';
import { Button, SortableItem } from '../ui';
import { CircuitBreaker, Protection, TapChanger, TeleSignal, Telemetry } from '../components/Cards';
import { ScenarioPanel } from '../components/ScenarioPanel';
import { EventPanel } from '../components/EventPanel';
import { call, exportFile, importFile, runScenario, setValue, updateOrder } from '../api';

const COLUMNS: { title: string; types: Comp['type'][] }[] = [
  { title: 'CB, Tap Changer & Proteksi', types: ['cb', 'tap', 'protection'] },
  { title: 'Telesignals', types: ['telesignal'] },
  { title: 'Telemetries', types: ['telemetry'] },
];

export function IedDetail({ name, info, back, events }: { name: string; info?: IedInfo; back: () => void; events: EventLog[] }) {
  const [comps, setComps] = useState<Comp[]>([]);
  const [dragging, setDragging] = useState(false);
  const [editing, setEditing] = useState(false);
  // undefined = tidak ada dialog, null = tambah komponen, Comp = ubah
  const [dialog, setDialog] = useState<Comp | null | undefined>(undefined);
  const remove = (id: string) => confirm(`Hapus ${id}? ${name} akan restart (client terputus).`) && call('remove_component', { ied: name, id });

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

  const send = (id: string) => (cmd: SetCmd) => setValue(name, id, cmd);

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
    updateOrder(name, next.map((c) => c.id));
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
              <span className={info.status === 'running' ? '' : 'font-bold text-red-600'}>{info.status}</span> · port {info.port} · {info.client_count} client
              · mode {info.mode}
              {info.rcbs.length > 0 && ` (${info.rcbs.map((r) => r.rcb).join(', ')})`}
            </span>
          )}
        </div>
        <div className="flex gap-2">
          <Button className="px-2 h-8 bg-white hover:bg-gray-300" disabled={dragging} onClick={() => setDialog(null)} title="Tambah komponen">
            <MdAdd />
          </Button>
          <Button
            className={`px-2 h-8 ${editing ? 'bg-blue-400' : 'bg-white hover:bg-gray-300'}`}
            disabled={dragging}
            onClick={() => setEditing(!editing)}
            title={editing ? 'Selesai' : 'Mode edit (ubah / hapus komponen)'}
          >
            {editing ? <MdOutlineCheck /> : <MdOutlineModeEditOutline />}
          </Button>
          <Button
            className={`px-2 h-8 ${dragging ? 'bg-blue-400' : 'bg-white hover:bg-gray-300'}`}
            disabled={editing}
            onClick={() => setDragging(!dragging)}
            title={dragging ? 'Selesai' : 'Mode drag and drop (atur urutan kartu)'}
          >
            {dragging ? <MdOutlineCheck /> : <TbDragDrop />}
          </Button>
          <Button className="px-2 h-8 bg-white hover:bg-gray-300" onClick={() => exportFile(name)} title={`Export komponen ${name} (JSON)`}>
            <PiExportBold />
          </Button>
          <Button className="px-2 h-8 bg-white hover:bg-gray-300" onClick={() => importFile(name)} title={`Import: komponen ${name} diganti isi file`}>
            <CgImport />
          </Button>
        </div>
      </div>
      {dialog !== undefined && <ComponentDialog ied={name} comp={dialog ?? undefined} comps={comps} onClose={() => setDialog(undefined)} />}

      <ScenarioPanel ied={name} info={info} prots={comps.filter((c) => c.type === 'protection').map((c) => c.id)} />
      {info?.status === 'off' && (
        <p className="px-4 py-1 bg-red-600 text-white text-sm font-bold">
          IED mati: IedServer dihancurkan & port {info.port} ditutup. Klik Hidupkan untuk membuat ulang (buffer report kosong, nilai kembali ke awal).
        </p>
      )}

      <div className="flex flex-row w-full">
        {COLUMNS.map(({ title, types }) => {
          const items = comps.filter((c) => types.includes(c.type));
          return (
            <div key={title} className="w-1/3 border-2 flex flex-col h-[calc(100vh-380px)] min-h-64">
              <div className="flex border-b-2 justify-center">
                <h2 className="text-xl font-semibold m-2">{title}</h2>
              </div>
              <div className="flex-1 overflow-y-auto">
                <DndContext collisionDetection={closestCenter} onDragEnd={onDragEnd(types)}>
                  <SortableContext items={items.map((c) => c.id)} strategy={verticalListSortingStrategy}>
                    {items.map((c) => (
                      <SortableItem key={c.id} id={c.id} enabled={dragging}>
                        <div className="relative">
                        {editing && (
                          <div className="absolute top-1 right-1 z-10 flex gap-1">
                            <Button className="w-8 h-8 bg-white text-blue-600 hover:bg-gray-200" onClick={() => setDialog(c)} title={`Ubah ${c.id}`}>
                              <MdOutlineEdit />
                            </Button>
                            <Button className="w-8 h-8 bg-white text-red-600 hover:bg-gray-200" onClick={() => remove(c.id)} title={`Hapus ${c.id}`}>
                              <MdDeleteOutline />
                            </Button>
                          </div>
                        )}
                        {c.type === 'cb' && <CircuitBreaker c={c} send={send(c.id)} dragging={dragging} />}
                        {c.type === 'tap' && <TapChanger c={c} send={send(c.id)} dragging={dragging} />}
                        {c.type === 'telesignal' && <TeleSignal c={c} send={send(c.id)} dragging={dragging} />}
                        {c.type === 'telemetry' && <Telemetry c={c} send={send(c.id)} dragging={dragging} />}
                        {c.type === 'protection' && <Protection c={c} trip={() => runScenario(name, 'trip', { prot: c.id })} dragging={dragging} />}
                        </div>
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
      <EventPanel events={events} ied={name} />
    </div>
  );
}
