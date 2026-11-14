// Bentuk data dari backend (Socket.IO `ieds`, `components`, `value_changed`)

export type Quality = { raw: number; text: string };

export type IedInfo = {
  name: string;
  port: number;
  /** off = skenario IED mati, hang = proxy menahan data */
  status: 'running' | 'off' | 'hang';
  delay_ms: number;
  scenarios: { quality: string | null; clock: string | null; storm: boolean };
  components: number;
  client_count: number;
  clients: { addr: string; since: number }[];
  /** jumlah koneksi menurut libIEC61850 (pembanding client_count) */
  ied_connections: number;
  mode: 'report' | 'polling' | 'idle';
  rcbs: { rcb: string; client: string }[];
  last_event: number | null;
};

export type Gen = { mode: string; interval_ms: number; min: number; max: number; scale: number };

/** config = bentuk JSON komponen (export/import), dipakai dialog ubah */
type Base = { id: string; q: Quality; t: number; config: Record<string, unknown> };

export type CbComp = Base & {
  type: 'cb';
  ln: string;
  ctl_model: string;
  travel_ms: number;
  interlock: boolean;
  accept_same: boolean;
  sbo_timeout_ms: number | null;
  pos: 'open' | 'close' | 'intermediate' | 'bad';
  local: boolean;
};

export type TapComp = Base & {
  type: 'tap';
  ln: string;
  ctl_model: string;
  min: number;
  max: number;
  interval_ms: number;
  pos: number;
  auto: boolean;
  local: boolean;
};

export type TsComp = Base & { type: 'telesignal'; ref: string; gen: Gen; value: boolean; auto: boolean | null };

export type TmComp = Base & {
  type: 'telemetry';
  ref: string;
  unit: string;
  gen: Gen;
  follow_cb: string | null;
  value: number;
  auto: boolean | null;
};

export type ProtComp = Base & { type: 'protection'; ln: string; cb: string; op: boolean; str: boolean };

export type Comp = CbComp | TapComp | TsComp | TmComp | ProtComp;

export type EventKind = 'connection' | 'rcb' | 'control' | 'report' | 'manual' | 'scenario';
export type EventLog = { id: number; ts: number; ied: string; type: EventKind; msg: string };

/** Perintah ubah manual (lihat backend Ied::set) */
export type SetCmd = { action?: string; local?: boolean; auto?: boolean; value?: boolean | number };
