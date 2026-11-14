// Dialog tambah IED (satu / banyak dari template) atau ubah nama / LD / port IED.
import { useState } from 'react';
import type { IedInfo } from '../types';
import { call } from '../api';
import { Button } from '../ui';
import { Field, Modal } from './ComponentDialog';

const inp = 'w-full h-9 px-2 border-2 border-black rounded text-sm';

export function IedDialog({ ied, ieds, onClose }: { ied?: IedInfo; ieds: IedInfo[]; onClose: () => void }) {
  const editing = ied !== undefined;
  const [name, setName] = useState(ied?.name ?? '');
  const [ld, setLd] = useState(editing ? '' : 'LD0');
  const [port, setPort] = useState(ied ? String(ied.port) : '');
  const [template, setTemplate] = useState('');
  const [count, setCount] = useState(1);
  const [busy, setBusy] = useState(false);

  const save = async () => {
    setBusy(true);
    const p = { name: name.trim(), ld: ld.trim(), port: port ? Number(port) : undefined };
    const r = editing ? await call('update_ied', { ied: ied.name, ...p }) : await call('add_ied', { ...p, template, count });
    setBusy(false);
    if (r.ok) onClose();
  };

  return (
    <Modal title={editing ? `Ubah ${ied.name}` : 'Tambah IED'} onClose={onClose}>
      <div className="grid grid-cols-2 gap-x-4 gap-y-2">
        {(editing || count === 1) && (
          <>
            <Field label="Nama IED (kosong = otomatis SIMIED<n>)">
              <input className={inp} value={name} onChange={(e) => setName(e.target.value)} />
            </Field>
            <Field label="Port MMS (kosong = port bebas berikutnya)">
              <input className={inp} type="number" value={port} onChange={(e) => setPort(e.target.value)} />
            </Field>
          </>
        )}
        <Field label={editing ? 'LD (kosong = tetap)' : 'LD'}>
          <input className={inp} value={ld} onChange={(e) => setLd(e.target.value)} />
        </Field>
        {!editing && (
          <>
            <Field label="Komponen dari template">
              <select className={inp} value={template} onChange={(e) => setTemplate(e.target.value)}>
                <option value="">kosong (tambah komponen nanti)</option>
                {ieds.map((i) => (
                  <option key={i.name} value={i.name}>
                    salin dari {i.name} ({i.components} komponen)
                  </option>
                ))}
              </select>
            </Field>
            <Field label="Jumlah IED (nama & port otomatis kalau > 1)">
              <input className={inp} type="number" min={1} max={200} value={count} onChange={(e) => setCount(Math.max(1, Number(e.target.value)))} />
            </Field>
          </>
        )}
      </div>
      <p className="mt-4 text-sm text-gray-600">
        Nama & LD: huruf/angka/_, diawali huruf, gabungan maks 32 karakter. Port dibuka di docker-compose: 102 dan 10102-10200.
        {editing && ' Menyimpan akan me-restart IED (client terputus, nilai kembali ke awal).'}
      </p>
      <div className="mt-4 flex justify-end gap-2">
        <Button className="px-4 h-9 bg-white hover:bg-gray-200" onClick={onClose}>
          Batal
        </Button>
        <Button className="px-4 h-9 bg-blue-600 hover:bg-blue-500 text-white" disabled={busy} onClick={save}>
          {busy ? 'Menyimpan…' : editing ? 'Simpan & restart' : `Tambah ${count} IED`}
        </Button>
      </div>
    </Modal>
  );
}
