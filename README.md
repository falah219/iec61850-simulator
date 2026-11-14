# IEC 61850 Simulator

Simulator banyak IED IEC 61850 (MMS server) dengan web UI untuk tester, untuk menguji FEP IEC 61850
tanpa IED sungguhan. Rencana lengkap: [docs/PLAN.MD](docs/PLAN.MD).

| Bagian | Teknologi | Port |
|---|---|---|
| `backend/` | Rust (axum + socketioxide) + libIEC61850 v1.6.2.1 lewat FFI | `6061` (HTTP API + Socket.IO) |
| `frontend/` | React 19 + Vite + TypeScript + Tailwind 4 + socket.io-client | `4061` (web UI) |

## Menjalankan (Windows + Docker Desktop)

```sh
docker compose up --build
```

- UI: http://localhost:4061 (pojok kanan atas harus **Tersambung**)
- Health check: http://localhost:6061/health

- MMS (IEC 61850): `localhost:102`, IED `SIMIED1`, domain `SIMIED1LD0`
- Banyak IED: isi `IED_COUNT=20` di `backend/.env` (salin dari `backend/.env.example`). IED ke-n bernama
  `SIMIED<n>`, port `102` untuk SIMIED1 lalu `10102`, `10103`, ... (SIMIED20 = `10120`)
- Status IED: http://localhost:6061/ieds, log kejadian: http://localhost:6061/events

Build pertama backend lama (compile libIEC61850 + crate Rust), berikutnya cepat karena cache.

## UI (http://localhost:4061)

- **Daftar IED**: port, status, jumlah client + alamatnya, mode (`report` + RCB aktif & client-nya /
  `polling?` / `idle`), jumlah komponen, kejadian terakhir. Tombol **Buka** ke detail.
- **Detail IED** (`#/ied/SIMIED1`, bisa di-bookmark): 3 kolom seperti acuan (CB & Tap Changer,
  Telesignal, Telemetry), nilai real-time dengan badge quality dan timestamp `t`.
- Ubah manual (dicatat sebagai kejadian `manual`):
  - CB: Open/Close dan tap Raise/Lower/Auto **hanya saat Local** (seperti acuan: Remote = dikendalikan
    SCADA/FEP). Switch L/R mengubah `Loc` (ikut report).
  - Telesignal ON/OFF dan telemetry (input angka + slider) hanya saat tombol **A** (auto) mati.
- Tombol drag-and-drop di kanan atas: atur urutan kartu (per kolom, disimpan di memori backend).

Socket.IO dari UI: `subscribe_ied` / `unsubscribe_ied` (room per IED), `set_value` dan `update_order`
(hasil lewat ack `{ok, message}`), `get_initial_data`. Ke UI: `ieds`, `components` (config + nilai),
`value_changed` (hanya yang berubah, tiap 300 ms, hanya ke room IED itu), `event_log`, `error`.

## Deploy

Hasil uji beban 100 IED (CPU, memori, latensi UI & kontrol): [docs/LOAD_TEST.md](docs/LOAD_TEST.md).

**Server tanpa Kubernetes** (image prod: backend di-strip, UI disajikan nginx; port sama dengan dev):

```sh
VITE_BACKEND_URL=http://<ip-server>:6061 docker compose -f docker-compose.prod.yml up -d --build
```

**Kubernetes** (pola sama dengan `iec104_rtu_simulator`: build, push ke registry, `kubectl apply`):

```sh
./install.sh <image-tag> <http-port> <http-nodeport> <ui-nodeport> <backend-host> <mms-port-awal> [jumlah-port-mms=100] [ied-count=1]
./install.sh 1 6061 30061 30041 192.168.1.10 30102 100 20
./uninstall.sh 1               # volume autosave dibiarkan
./uninstall.sh 1 --hapus-data  # ikut hapus konfigurasi semua IED
```

- Port MMS di dalam container dibuat **sama dengan NodePort** (`MMS_PORT` = mms-port-awal,
  `MMS_PORT_BASE` = +1), jadi port yang tampil di UI = port yang dipakai FEP. Service berisi
  `jumlah-port-mms` port (default 100: 30102-30201).
- Env opsional: `REGISTRY` (grita.id/scada), `NAMESPACE` (scada-grita), `PULL_SECRET` (harbor-secret), `UI_PORT` (4061).
- Autosave di PVC `iec61850-simulator-data-<tag>` (100 Mi, ReadWriteOnce), deployment `Recreate`.
  `ied-count` hanya dipakai saat volume masih kosong.
- Resource backend: request 500m / 128 Mi, limit 3 CPU / 512 Mi (dari uji beban). Probe `GET /health`.
- Manifest: [backend/k8s/](backend/k8s/), [frontend/k8s/](frontend/k8s/) (diisi `envsubst` oleh install.sh).

## Konfigurasi: tambah / ubah / hapus, export / import, autosave

- **Daftar IED**: *Tambah IED* (satu, atau banyak sekaligus dengan komponen disalin dari IED lain
  sebagai template; nama `SIMIED<n>` & port bebas berikutnya otomatis), ubah nama / LD / port, hapus,
  *Export* / *Import* semua IED.
- **Detail IED**: `+` tambah komponen, tombol pensil = mode edit (ubah / hapus di tiap kartu),
  export / import komponen IED itu.
- Model IEC 61850 tidak bisa diubah saat server jalan, jadi **IED yang modelnya berubah restart
  otomatis** (client terputus, nilai kembali ke awal); IED lain tidak terganggu. Urutan kartu tidak memicu restart.
- Validasi dulu: kalau ditolak (id/nomor/port bentrok, CB masih dipakai follow_cb / proteksi, dll.)
  tidak ada yang berubah.
- **Nomor otomatis tidak dipakai ulang**: instance CB (XCBR/CSWI), Tap (YLTC/ATCC), Proteksi (PTOC/PDIS),
  `Ind<k>` & `AnIn<k>` per GGIO diambil dari penghitung per IED (`counters` di JSON) yang tidak pernah
  turun. Hapus CB2 lalu tambah CB baru -> XCBR3. Nomor instance MMXU / GGIO dipilih tester.
- **Autosave** (`AUTOSAVE_PATH`, di docker compose `/data/simulator.json` di volume `simulator-data`):
  disimpan tiap perubahan; saat start file dimuat (IED_COUNT diabaikan). Mulai dari nol:
  `docker compose down -v`.
- Import semua IED = konfigurasi lama diganti; import di detail IED = komponen IED itu diganti
  (nama / LD / port tetap).

Format JSON (sama untuk export, import, autosave):

```json
{ "version": 1, "ieds": [ { "name": "SIMIED1", "ld": "LD0", "port": 102,
  "counters": { "XCBR": 2, "GGIO1.Ind": 2 }, "order": [],
  "components": [
    { "id": "cb1", "type": "cb", "inst": 1, "closed": true, "ctl_model": "sbo-enhanced", "travel_ms": 100,
      "local": false, "interlock": false, "accept_same": false, "sbo_timeout_ms": 30000 },
    { "id": "tap1", "type": "tap", "inst": 1, "pos": 9, "min": 1, "max": 17, "ctl_model": "direct-enhanced", "interval_ms": 5000 },
    { "id": "ts1", "type": "telesignal", "ggio": 1, "ind": 1, "value": true, "gen": { "mode": "toggle", "interval_ms": 5000 } },
    { "id": "mw", "type": "telemetry", "mx": { "class": "MMXU", "inst": 1, "do": "TotW", "phase": null }, "value": 50,
      "gen": { "mode": "random_walk", "interval_ms": 1000, "min": 0, "max": 100, "scale": 0.1, "step": 5 }, "follow_cb": "cb1" },
    { "id": "prot1", "type": "protection", "class": "PTOC", "inst": 1, "cb": "cb1" }
  ] } ] }
```

Nomor `inst` / `ind` 0 atau tidak diisi = otomatis; `"do": "AnIn"` tanpa angka = AnIn otomatis.

| HTTP | Fungsi |
|---|---|
| `GET /export[?ied=SIMIED1]` | konfigurasi semua IED / satu IED |
| `POST /import[?ied=SIMIED1]` | body file JSON di atas |

Socket.IO: `add_ied {name?, ld?, port?, template?, count?}`, `update_ied {ied, name?, ld?, port?}`,
`remove_ied {ied}`, `add_component {ied, component}`, `update_component {ied, id, component}`,
`remove_component {ied, id}`, `export_data {ied?}` (ack `{ok, data}`), `import_data {data, ied?}`.

## Skenario (UI + HTTP API)

Di UI: panel **Skenario** di detail IED (+ tombol cepat Mati / Hang / Putus di daftar IED), hasil
muncul sebagai toast dan di panel **Log kejadian** (filter jenis, jeda, bersihkan).

| Skenario | Jalankan (`POST /ieds/{ied}/scenarios/{s}`, body opsional) | Hentikan (`.../{s}/stop`) |
|---|---|---|
| `trip` | `{"prot": "prot1", "pulse_ms": 200}`: Str & Op naik, +20 ms CB Pos `00` (intermediate), + waktu tempuh Pos `01` (open), arus/daya yang ikut CB jadi 0, Op & Str turun setelah pulse. CB sudah open: hanya pulsa Op | - |
| `off` | IED mati: listener proxy & koneksi ditutup, IedServer di-stop & dihancurkan (beserta model) | dibuat ulang dari konfigurasi seperti reboot: buffer report kosong, nilai awal (urutan kartu tetap) |
| `hang` | proxy berhenti meneruskan data, TCP tetap terbuka | lanjutkan (data yang tertahan diteruskan) |
| `disconnect` | `{"mode": "fin"\|"rst"}`: semua koneksi client ditutup (FIN, atau RST lewat SO_LINGER 0), IED tetap hidup | - |
| `delay` | `{"ms": 1000}`: jeda tiap data yang diteruskan proxy | delay 0 |
| `quality` | `{"validity": "invalid"\|"questionable", "flags": ["oldData", "substituted", "test", ...], "components": ["mw"]}` (tanpa `components` = semua) | normalkan: hanya q kembali good di semua komponen (nilai tidak berubah), report qchg terkirim |
| `clock` | `{"not_sync": true}` / `{"failure": true}`: time quality di timestamp berikutnya | normal |
| `storm` | `{"rate": 50, "duration": 30}`: semua telemetry & telesignal berubah `rate` kali/detik (1..1000) selama `duration` detik | berhenti (atau otomatis setelah durasi) |

Ubah satu titik: `PUT /ieds/{ied}/points/{ref}` dengan `ref` = id komponen atau reference
(`cb1`, `XCBR1`, `MMXU1.TotW`, `GGIO1.Ind1`). Body sama dengan ubah manual UI
(`{"action": "open"}`, `{"local": true}`, `{"auto": false}`, `{"value": 42}`), plus
`{"quality": {"validity": "invalid", "flags": ["oldData"]}}`. Jawaban: `{ok, message}`; 400 kalau ditolak, 404 kalau IED tidak ada.

```sh
curl -X POST localhost:6061/ieds/SIMIED1/scenarios/trip
curl -X POST localhost:6061/ieds/SIMIED1/scenarios/delay -d '{"ms": 500}'
curl -X POST localhost:6061/ieds/SIMIED1/scenarios/delay/stop
curl -X PUT  localhost:6061/ieds/SIMIED1/points/MMXU1.TotW -d '{"quality": {"validity": "questionable", "flags": ["oldData"]}}'
curl "localhost:6061/events?ied=SIMIED1&type=scenario"
```

Socket.IO: `run_scenario {ied, scenario, params}`, `stop_scenario {ied, scenario}` (ack `{ok, message}`).
`GET /ieds` menambah `status` (`running` / `off` / `hang`), `delay_ms`, `scenarios` (quality, clock, storm).

## Banyak IED, proxy, log kejadian

Setiap IED: IedServer libIEC61850 listen di `127.0.0.1:<port acak>`, proxy TCP memegang port publik.
Proxy menghitung client dan mencatat alamat client asli (IedServer hanya melihat `127.0.0.1`).
Skenario hang / putus koneksi / delay memakai proxy ini di fase 7.

`GET /ieds`: per IED `name`, `port`, `status`, `components`, `client_count` + `clients` (alamat, sejak),
`ied_connections` (jumlah koneksi menurut libIEC61850, pembanding), `mode` (`report` = ada RCB aktif,
`polling` = ada client tanpa RCB aktif, `idle`), `rcbs` (RCB aktif + client), `last_event`.

`GET /events?ied=SIMIED1&type=control&limit=100`: log kejadian (ring buffer 5000, urut lama -> baru).

| type | Isi |
|---|---|
| `connection` | client konek / putus (alamat asli, jumlah byte, jumlah client) |
| `rcb` | RCB enable (BRCB/URCB, dataset, oleh client), disable, GI, buffer overflow |
| `control` | select / operate diterima / ditolak + AddCause / selesai + CommandTermination, unselect (cancel, sboTimeout, client putus); dengan orIdent, orCat, alamat client |
| `report` | jumlah report terkirim per RCB, dikelompokkan per detik |
| `manual` | perubahan dari UI / API (CB open/close, L/R, tap, auto, nilai, quality) |
| `scenario` | skenario mulai / selesai |

Socket.IO: `ieds` (dikirim saat terhubung dan setiap ada perubahan, maks 1x/detik), `event_log`
(200 kejadian terakhir saat terhubung, lalu real-time). Semua kejadian juga ditulis ke log backend
(`report` di level debug).

## Isi IED contoh (`backend/src/main.rs`; dipakai saat belum ada file autosave)

| Komponen | Reference | Generator |
|---|---|---|
| Circuit Breaker `cb1` | `XCBR1.Pos` (DPS, close), `XCBR1.Loc`, `CSWI1.Pos` (DPC, **sbo-enhanced**, tempuh 100 ms), `CSWI1.Loc` | kontrol |
| Circuit Breaker `cb2` | `XCBR2.Pos` (open), `CSWI2.Pos` (DPC, **sbo-normal**, tempuh 100 ms) | kontrol |
| Tap Changer `tap1` | `ATCC1.TapChg` (BSC, **direct-enhanced**, posisi 9, batas 1..17), `ATCC1.Auto` (SPC, direct-normal), `ATCC1.Loc`, `YLTC1.TapPos` (INS) | kontrol / auto |
| Telesignal | `GGIO1.Ind1` / `GGIO1.Ind2` | toggle 5 s / manual |
| MW | `MMXU1.TotW` | random_walk 0..100, ikut `cb1` |
| MVAR | `MMXU1.TotVAr` | random -20..20, ikut `cb1` |
| Hz | `MMXU1.Hz` | random_walk 49.8..50.2 |
| kV fasa A | `MMXU1.PhV.phsA` | sine 19..21 |
| A fasa A | `MMXU1.A.phsA` | sine 200..400, ikut `cb1` |
| Generik | `GGIO1.AnIn1` | manual 123.4 |
| Proteksi `prot1` | `PTOC1.Op` (ACT), `PTOC1.Str` (ACD), trip `cb1` | skenario trip |
| Wajib | `LLN0.Mod/Beh/Health/NamPlt`, `LPHD1.PhyNam/PhyHealth/Proxy` | |

"Ikut `cb1`" = nilai 0 selama CB tidak close. Dataset & RCB otomatis di `LLN0`:
`DS_ST` (semua titik ST) dan `DS_MX` (semua titik MX), masing-masing dengan BRCB `brcb<ST|MX>01..02`
dan URCB `urcb<ST|MX>01..02` (TrgOps dchg/qchg/integrity/gi, RptID default = reference RCB).

### Kontrol (CB & tap changer)

- ctlModel per komponen: status-only, direct-normal, sbo-normal, direct-enhanced, sbo-enhanced.
  CommandTermination (+/-) dikirim untuk model enhanced.
- CB: saat operate `XCBR.Pos` & `CSWI.Pos` jadi intermediate (`00`) selama waktu tempuh, lalu posisi akhir.
  Tempuh tetap selesai walau client putus di tengah.
- Tap: raise/lower mengubah `ATCC.TapChg.valWTr.posVal` dan `YLTC.TapPos` (INS) ±1. Saat `ATCC.Auto` = true,
  tap bergerak 1 langkah tiap interval dan berbalik arah di batas.
- Opsi per CB: `local`, `interlock` (aktif), `accept_same` (default tolak `position-reached`; opsi
  "selalu terima" langsung CommandTermination+ tanpa bergerak), `sbo_timeout_ms`.
- Model SBO punya atribut `<DO>.sboTimeout` (CF, default **30000 ms**, per CB). Select yang tidak diikuti
  operate dalam waktu itu batal. `Loc` ada di `DS_ST`, jadi ikut report.
- Nomor instance LN (`XCBR<n>`, `ATCC<n>`, `MMXU<n>`, `GGIO<n>`) dan `Ind<k>`/`AnIn<k>` disimpan di komponen,
  tidak dihitung dari urutan, sehingga tidak bergeser saat komponen lain dihapus. Nomor ganda ditolak.

| Ditolak kalau | AddCause |
|---|---|
| `Loc` = true (local) | `blocked-by-switching-hierarchy` |
| interlock aktif di komponen & client minta interlock-check | `blocked-by-interlocking` |
| CB sudah di posisi tujuan (kecuali opsi `accept_same`) | `position-reached` |
| CB masih bergerak (tempuh) | `command-already-in-execution` |
| tap di batas atas/bawah | `step-limit` |
| tap mode auto | `blocked-by-mode` |

AddCause hanya terlihat di client untuk model enhanced (security normal tidak mengirim LastApplError).

### Telemetry: preset LN/DO

| Preset | Reference |
|---|---|
| MW / MVAR / Hz | `MMXU<n>.TotW` / `MMXU<n>.TotVAr` / `MMXU<n>.Hz` (MV) |
| kV fasa / A fasa | `MMXU<n>.PhV.phs<A/B/C>` / `MMXU<n>.A.phs<A/B/C>` (CMV) |
| Generik | `GGIO<n>.AnIn<k>` (MV) |
| Custom | MMXU: TotW, TotVAr, TotVA, TotPF, Hz (MV); PhV, A, W, VAr, VA, PF, Z (WYE, fasa phsA/B/C/neut/net/res); PPV (DEL, phsAB/BC/CA). GGIO: AnIn<k> |

Titik yang sama (LN + DO + fasa) di dua komponen ditolak. Nama IED & LD: huruf/angka/_, diawali huruf,
gabungan maksimal 32 karakter.

## Uji

Unit test (di dalam Docker, karena butuh libIEC61850):

```sh
docker build -f backend/Dockerfile.local --target builder -t iec61850-sim-builder backend
docker run --rm iec61850-sim-builder cargo test
```

Kirim perintah kontrol tanpa FEP (client MMS bawaan, alurnya sama dengan FEP: select -> operate ->
tunggu CommandTermination). Hasil & AddCause dicetak, log kontrol ada di `docker compose logs backend`:

```sh
docker compose exec backend ./iec61850-simulator ctl 127.0.0.1 102 SIMIED1LD0/CSWI1.Pos open
docker compose exec backend ./iec61850-simulator ctl 127.0.0.1 102 SIMIED1LD0/CSWI1.Pos close --interlock
docker compose exec backend ./iec61850-simulator ctl 127.0.0.1 102 SIMIED1LD0/ATCC1.TapChg raise
docker compose exec backend ./iec61850-simulator ctl 127.0.0.1 102 SIMIED1LD0/ATCC1.Auto true
```

Jumlah client & log dari luar container (client dari container lain masuk lewat port yang di-publish):

```sh
docker run --rm iec61850-sim-builder iec61850-simulator ctl host.docker.internal 10120 SIMIED20LD0/CSWI1.Pos open
curl "http://localhost:6061/events?ied=SIMIED20&type=control"
curl http://localhost:6061/ieds
```

Pantau report (alat dev [tools/rptcheck/rptcheck.c](tools/rptcheck/rptcheck.c), tidak ikut image produksi):
compile di image builder, lalu enable RCB + GI dan cetak report selama 12 detik:

```sh
docker run --rm -v "$PWD/tools/rptcheck:/w" iec61850-sim-builder sh -c "gcc -I/usr/local/include/libiec61850 /w/rptcheck.c -o /tmp/rptcheck /usr/local/lib/libiec61850.a /usr/local/lib/libhal.a -lpthread -lm -lrt && /tmp/rptcheck host.docker.internal 102 SIMIED1LD0/LLN0.BR.brcbST01"
```

Baca model & nilai dari repo FEP `fep-61850` (container `dev` harus jalan):

```sh
docker compose -f docker-compose.dev.yml exec -w /work/tools/iec61850-cli dev cargo run --release --bin browse -- host.docker.internal 102
docker compose -f docker-compose.dev.yml exec dev mms_utility -h host.docker.internal -p 102 -a SIMIED1LD0 -r 'XCBR1$ST$Pos$stVal'
```

## Konfigurasi

Salin `backend/.env.example` ke `backend/.env` kalau ingin mengubah nilai (opsional).
Alamat backend untuk UI: env `VITE_BACKEND_URL` saat build (lihat `frontend/.env.example`);
kosong berarti `http://<host halaman>:6061`.

`.env` asli tidak di-commit.
