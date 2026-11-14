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

Build pertama backend lama (compile libIEC61850 + crate Rust), berikutnya cepat karena cache.

## Isi IED contoh (sementara di kode, `backend/src/main.rs`)

| Komponen | Reference | Generator |
|---|---|---|
| Circuit Breaker `cb1` | `XCBR1.Pos` (DPS, close), `XCBR1.Loc`, `CSWI1.Pos` (DPC, status-only), `CSWI1.Loc` | manual |
| Telesignal | `GGIO1.Ind1` / `GGIO1.Ind2` | toggle 5 s / manual |
| MW | `MMXU1.TotW` | random_walk 0..100, ikut `cb1` |
| MVAR | `MMXU1.TotVAr` | random -20..20, ikut `cb1` |
| Hz | `MMXU1.Hz` | random_walk 49.8..50.2 |
| kV fasa A | `MMXU1.PhV.phsA` | sine 19..21 |
| A fasa A | `MMXU1.A.phsA` | sine 200..400, ikut `cb1` |
| Generik | `GGIO1.AnIn1` | manual 123.4 |
| Wajib | `LLN0.Mod/Beh/Health/NamPlt`, `LPHD1.PhyNam/PhyHealth/Proxy` | |

"Ikut `cb1`" = nilai 0 selama CB tidak close. Dataset & RCB otomatis di `LLN0`:
`DS_ST` (semua titik ST) dan `DS_MX` (semua titik MX), masing-masing dengan BRCB `brcb<ST|MX>01..02`
dan URCB `urcb<ST|MX>01..02` (TrgOps dchg/qchg/integrity/gi, RptID default = reference RCB).

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
