// Perintah ke backend lewat Socket.IO dengan ack {ok, message} -> toast
import { toast } from 'sonner';
import socket from './socket';

export type Ack = { ok: boolean; message: string; data?: unknown };

const show = (quiet: boolean) => (r: Ack) => (r.ok ? !quiet && toast.success(r.message) : toast.error(r.message));

export function runScenario(ied: string, scenario: string, params: object = {}) {
  socket.emit('run_scenario', { ied, scenario, params }, show(false));
}

export function stopScenario(ied: string, scenario: string) {
  socket.emit('stop_scenario', { ied, scenario }, show(false));
}

/** Ubah manual; sukses tidak perlu toast (nilai langsung terlihat di kartu) */
export function setValue(ied: string, id: string, cmd: object) {
  socket.emit('set_value', { ied, id, ...cmd }, show(true));
}

export function updateOrder(ied: string, order: string[]) {
  socket.emit('update_order', { ied, order }, show(true));
}

/** Perintah konfigurasi (fase 8): toast hasil, resolve dengan ack supaya dialog bisa menutup diri kalau ok */
export function call(event: string, payload: object): Promise<Ack> {
  return new Promise((resolve) =>
    socket.timeout(60_000).emit(event, payload, (err: Error | null, r: Ack) => {
      const ack = err ? { ok: false, message: 'tidak ada jawaban dari backend' } : r;
      if (event !== 'export_data' || !ack.ok) show(false)(ack);
      resolve(ack);
    }),
  );
}

/** Export konfigurasi (semua IED / satu IED) sebagai file JSON */
export async function exportFile(ied?: string) {
  const r = await call('export_data', ied ? { ied } : {});
  if (!r.ok) return;
  const url = URL.createObjectURL(new Blob([JSON.stringify(r.data, null, 2)], { type: 'application/json' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = `${ied ?? 'semua-ied'}-iec61850-simulator.json`;
  a.click();
  URL.revokeObjectURL(url);
}

/** Pilih file JSON lalu import (semua IED / komponen satu IED), setelah konfirmasi */
export function importFile(ied?: string) {
  const input = document.createElement('input');
  input.type = 'file';
  input.accept = '.json,application/json';
  input.onchange = async () => {
    const f = input.files?.[0];
    if (!f) return;
    let data: unknown;
    try {
      data = JSON.parse(await f.text());
    } catch {
      toast.error(`${f.name} bukan JSON valid`);
      return;
    }
    const what = ied ? `Semua komponen ${ied} diganti isi ${f.name}` : `SEMUA IED diganti isi ${f.name}`;
    if (confirm(`${what}. IED yang berubah akan restart (client terputus). Lanjut?`)) await call('import_data', { data, ied });
  };
  input.click();
}
