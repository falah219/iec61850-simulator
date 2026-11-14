// Perintah ke backend lewat Socket.IO dengan ack {ok, message} -> toast
import { toast } from 'sonner';
import socket from './socket';

type Ack = { ok: boolean; message: string };

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
