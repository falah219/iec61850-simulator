# Rencana Service Simulator IEC 61850 (v2, mengacu ke `iec104_rtu_simulator`)

Dokumen acuan untuk Claude Code. Simpan di repo baru `iec61850-simulator` sebagai `docs/SIMULATOR_PLAN.md`.

Tujuan: simulator **banyak IED IEC 61850 (MMS server)** dengan **web UI untuk tester**, untuk menguji FEP IEC 61850 tanpa IED sungguhan.

Acuan: repo `falah219/iec104_rtu_simulator`, yaitu simulator RTU IEC 104 yang sudah dipakai tim. **Fitur yang sudah ada di acuan diikuti** (cara kerja, tampilan, deploy). **Fitur yang belum ada ditambahkan** (ditandai 🆕).

---

## 0. Aturan kerja (WAJIB dibaca dulu)

1. Saya pemula. Jelaskan setiap langkah dalam **bahasa Indonesia**: apa yang dibuat, kenapa, cara mengujinya.
2. Kerjakan **per fase** (bagian 9). Setelah satu fase selesai, **berhenti**, tunjukkan cara mengujinya, tunggu konfirmasi saya.
3. Lingkungan: **Windows + Docker Desktop** untuk development, server Linux untuk deploy. Semua build lewat Docker.
4. Jangan menyimpan kredensial atau `.env` asli di repo. Yang di-commit cukup `.env.example`. (Catatan: acuan meng-commit `backend/.env` dan `frontend/.env`; **jangan ditiru**.)
5. Jangan konek ke sistem atau jaringan kantor/PLN.
6. Kalau ada keputusan desain yang tidak tertulis di sini, **tanya saya dulu**.
7. Sebelum mulai, **baca repo acuan** `iec104_rtu_simulator` (terutama `frontend/src/` dan `backend/main.py`) dan tiru gaya UI serta pola event-nya.
8. **Backend WAJIB Rust** (axum + socketioxide + libIEC61850 lewat FFI, pola dari repo `fep-61850`). **Jangan memakai Python** dan jangan menyalin kode Python dari acuan. Backend Python acuan hanya dibaca untuk memahami fitur dan nama event Socket.IO. Frontend memakai React + TypeScript seperti acuan.

---

## 1. Ringkasan repo acuan (`iec104_rtu_simulator`)

| Aspek | Isi acuan |
|---|---|
| Backend | Python **FastAPI** + **python-socketio**, lib60870-C lewat wrapper ctypes (`lib/lib60870.py`) |
| Frontend | **React 19 + Vite + TypeScript + Tailwind 4**, komponen gaya shadcn (Radix: dialog, switch, radio-group, tooltip), **sonner** (toast), **@dnd-kit** (urutan kartu bisa di-drag), **socket.io-client** |
| Real-time | Socket.IO: backend emit daftar komponen setiap ada perubahan; frontend emit `add_*`, `update_*`, `remove_*`, `update_order`, `export_data`, `import_data` |
| Jumlah RTU | **1 RTU per instance** (satu port IEC 104). Banyak RTU berarti deploy banyak instance |
| Komponen | Circuit Breaker (status SP/DP, kontrol open/close, SBO, Local/Remote), Tap Changer (raise/lower, auto/manual, batas atas/bawah), Telesignal (toggle otomatis per interval), Telemetry (nilai acak min..max, scale factor, interval, auto/manual) |
| Penyimpanan | **In-memory**, plus export/import JSON lewat UI |
| Deploy | Image terpisah backend & frontend (`Dockerfile.local` / `Dockerfile.prod`), `docker-compose.yml`, manifest Kubernetes per bagian, `install.sh` / `uninstall.sh` |

---

## 2. Matriks fitur: acuan vs simulator IEC 61850

| Fitur | Acuan IEC 104 | Simulator IEC 61850 |
|---|---|---|
| Web UI React + Tailwind + kartu komponen | ✅ | **Ikuti** stack & gaya yang sama |
| Real-time ke UI | ✅ Socket.IO | **Ikuti** Socket.IO (frontend tetap pakai `socket.io-client`) |
| Tambah/ubah/hapus komponen dari UI | ✅ | **Ikuti** (catatan: model IEC 61850 tidak bisa diubah saat server jalan, jadi IED **di-restart otomatis** setelah perubahan; tampilkan peringatan di UI) |
| Circuit Breaker (status, open/close, SBO, Local/Remote) | ✅ | **Ikuti**, dipetakan ke `XCBR` + `CSWI` + `Loc` (bagian 4) |
| Tap Changer (raise/lower, auto/manual, batas) | ✅ | **Ikuti**, dipetakan ke `YLTC`/`ATCC` |
| Telesignal auto toggle | ✅ | **Ikuti** |
| Telemetry acak min..max, interval, auto/manual | ✅ | **Ikuti**, plus pola tambahan (bagian 5) |
| Ubah nilai manual (geser MW, ubah posisi breaker) | ✅ (manual mode / tombol) | **Ikuti**, plus slider untuk telemetry |
| Urutan kartu drag-and-drop | ✅ | **Ikuti** |
| Export/import JSON | ✅ | **Ikuti** (per IED dan semua IED) |
| Toast notifikasi | ✅ sonner | **Ikuti** |
| Docker + Kubernetes, frontend & backend terpisah | ✅ | **Ikuti** pola yang sama |
| 🆕 **Banyak IED dalam satu instance** + **halaman daftar IED** (status, jumlah client terhubung, mode) | ❌ (1 RTU per instance, client hanya di log) | **Tambah** |
| 🆕 **Tombol skenario**: trip, IED mati, IED hang, quality invalid, putus koneksi | ❌ | **Tambah** |
| 🆕 **Log kejadian** di UI: perintah kontrol dari FEP, report terkirim, client konek/putus | ❌ (hanya log backend) | **Tambah** |
| 🆕 Quality & timestamp per titik | ❌ | **Tambah** (wajib di IEC 61850) |
| 🆕 Dataset + Report Control Block otomatis per IED | – (tidak ada di IEC 104) | **Tambah** |
| 🆕 HTTP API untuk uji otomatis (skrip/CI) | ❌ (hanya Socket.IO) | **Tambah** |
| 🆕 Simpan otomatis ke file (bertahan saat restart) | ❌ (hilang saat restart) | **Tambah**, opsional, lewat volume |
| 🆕 Import model dari file SCL (ICD/CID) | – | **Tambah di fase akhir** (opsional) |

---

## 3. Arsitektur

```
[Frontend React]  ── Socket.IO ──►  [Backend simulator]
 (daftar IED,        ◄── event ──     ├── Manager IED (N IED, masing-masing IedServer libIEC61850)
  kartu komponen,                     ├── Proxy TCP per IED (untuk skenario hang / putus koneksi)
  skenario, log)                      ├── Generator nilai (auto mode)
                                      ├── Log kejadian (ring buffer)
[Skrip uji / CI] ── HTTP API ──►      └── Simpan otomatis JSON (opsional)
                                                │
                                   MMS port 102 / 10102.. ──► FEP IEC 61850
```

### 3.1 Bahasa backend (SUDAH DIPUTUSKAN: opsi A, Rust)

| Opsi | Kelebihan | Kekurangan |
|---|---|---|
| **A. Rust** (axum + **socketioxide** untuk Socket.IO) **(rekomendasi)** | Pola FFI libIEC61850 sudah terbukti di FEP (`build.rs`, `bindgen`, `catch_unwind`); kuat untuk 100+ IED; satu bahasa dengan FEP | Beda dengan acuan (Python) |
| B. Python FastAPI (sama dengan acuan) | Sama persis dengan acuan, mudah dirawat tim acuan | Wrapper ctypes libIEC61850 harus ditulis sendiri (acuan butuh ~6.500 baris untuk lib60870); callback C ke Python (kontrol, koneksi) rawan crash; lebih berat untuk banyak IED |

**Keputusan: opsi A (Rust).** Frontend **tetap mengikuti acuan** (React + `socket.io-client`), jadi tester tidak merasakan perbedaan. Ikuti struktur dan gaya kode backend FEP `fep-61850` (`config/`, `logger.rs`, `error.rs`, pola FFI di `build.rs` / `ffi.rs`, `catch_unwind` di callback C).

### 3.2 libIEC61850
- Pakai source di `native/libiec61850` (v1.6.2.1), **salin dari repo FEP `fep-61850`** beserta `THIRD_PARTY_NOTICES.md`, supaya versinya sama dengan FEP.
- Model IED dibuat **dinamis lewat kode** (`iec61850_dynamic_model.h` + `iec61850_cdc.h`: `IedModel_create`, `LogicalDevice_create`, `LogicalNode_create`, `CDC_DPC_create`, `CDC_MV_create`, dll.), bukan dari file `.cfg`. Inilah yang memungkinkan tambah/hapus komponen dari UI seperti acuan.
- Lisensi GPLv3: aman karena simulator hanya dipakai internal untuk uji.

### 3.3 Proxy TCP per IED (untuk skenario jaringan)
- Setiap IED: `IedServer` listen di port **internal** (127.0.0.1, port acak). Simulator membuka port **publik** (102 / 10102 / ...) dan meneruskan byte ke port internal.
- Dengan proxy ini skenario bisa dibuat tanpa mengubah libIEC61850:
  - **Hang**: proxy berhenti meneruskan data, tapi koneksi TCP tetap terbuka (FEP harus mendeteksi lewat timeout/liveness).
  - **Putus koneksi**: proxy menutup semua koneksi client (RST/FIN), IED tetap jalan, client boleh konek lagi.
  - **Delay**: tambah jeda N ms pada data yang diteruskan.
- **Jumlah client terhubung** dihitung di proxy (sekaligus dicocokkan dengan `IedServer_setConnectionIndicationHandler`).

---

## 4. Komponen per IED (padanan acuan → IEC 61850)

Setiap IED punya Logical Device `LD0` (nama bisa diatur) dengan `LLN0` + `LPHD1`. Komponen dari UI menjadi Logical Node berikut:

| Komponen (UI) | Logical Node | Data Object | Catatan |
|---|---|---|---|
| **Circuit Breaker** | `XCBR<n>` + `CSWI<n>` | `XCBR.Pos` (DPS, ST), `CSWI.Pos` (DPC, CO + ST), `XCBR.Loc` / `CSWI.Loc` (SPS) | `ctlModel` dipilih di UI: status-only, direct-normal, sbo-normal, direct-enhanced, sbo-enhanced. **Waktu tempuh** (default 100 ms) dengan `intermediate-state`. Menolak perintah saat **Local** (AddCause `blocked-by-switching-hierarchy`). Opsi: tolak dengan interlock (`Check` + AddCause) |
| **Disconnector / Earth switch** (opsional) | `XSWI<n>` + `CSWI` | sama dengan CB | Seperti CB tanpa proteksi |
| **Tap Changer** | `YLTC<n>` + `ATCC<n>` | `YLTC.TapPos` (INS) / `ATCC.TapChg` (BSC), `ATCC.Auto` (SPC) | raise/lower dari FEP & UI, batas atas/bawah, auto/manual seperti acuan |
| **Telesignal** | `GGIO<n>` | `Ind<k>` (SPS) | auto toggle per interval seperti acuan |
| **Telemetry** | `MMXU<n>` (atau `GGIO.AnIn`) | `TotW`, `TotVAr`, `Hz`, `PhV.phsA..C`, `A.phsA..C` (MV/CMV) | unit, scale, min/max, interval, auto/manual seperti acuan; deadband `db` |
| 🆕 **Proteksi** | `PTOC<n>` / `PDIS<n>` | `Op` (ACT), `Str` (ACD) | dipakai skenario trip |

Setiap titik punya `q` (quality) dan `t` (timestamp) yang diperbarui setiap nilai berubah.

**Dataset & RCB otomatis** per IED (dibuat ulang saat komponen berubah):
- `LLN0.DS_ST` (semua titik ST) dan `LLN0.DS_MX` (semua MX)
- BRCB `brcbST01..02`, `brcbMX01..02` dan URCB `urcbST01..02`, `urcbMX01..02`, TrgOps dchg/qchg/integrity/gi

---

## 5. Generator nilai (auto mode)

Mengikuti acuan (`interval`, `auto_mode`, acak min..max dengan `scale_factor`), plus pola tambahan:

| Tipe | Perilaku | Di acuan? |
|---|---|---|
| `random` | Acak min..max kelipatan scale factor | ✅ |
| `toggle` | Status 0/1 bergantian tiap interval | ✅ |
| 🆕 `random_walk` | Naik-turun kecil, tidak keluar min/max | ❌ |
| 🆕 `sine` | Gelombang (beban harian), dengan amplitudo dan periode | ❌ |
| 🆕 `follow` | Ikut titik lain, misalnya arus 0 saat CB open | ❌ |

Mode manual: nilai hanya berubah dari UI, HTTP API, atau perintah FEP.

---

## 6. Skenario (🆕, tidak ada di acuan)

Tombol di UI per IED + endpoint HTTP. Semua skenario dicatat di log kejadian.

| Skenario | Yang terjadi | Yang diuji di FEP |
|---|---|---|
| **Trip** | `PTOC.Op` pulse (default 200 ms) → CB `XCBR.Pos` jadi off (lewat intermediate) → arus/daya terkait jadi 0 | Report data-change, event cepat, urutan timestamp (SOE) |
| **IED mati** | Server dan proxy berhenti (port ditutup); tombol **Hidupkan** untuk kembali | Status offline (`RTU_STATUS`), reconnect |
| **IED hang** | Proxy berhenti meneruskan data, TCP tetap terbuka; tombol **Lanjutkan** | Deteksi timeout/liveness, reconnect |
| **Putus koneksi** | Proxy menutup semua koneksi client; IED tetap hidup | Reconnect, BRCB/EntryID |
| **Quality invalid** | Set `q` titik terpilih (atau semua) ke invalid / questionable / oldData / substituted / test; tombol **Normalkan** | Pemetaan quality & flag di Kafka |
| 🆕 Clock tidak sinkron | Set time quality `ClockNotSynchronized` / `ClockFailure` | Flag `CLOCK_NOT_SYNC` |
| 🆕 Delay jaringan | Proxy menambah jeda N ms | Timeout FEP |
| 🆕 Badai data | Semua titik berubah cepat selama N detik | Performa FEP & Kafka |

---

## 7. Backend: Socket.IO, HTTP API, log

### 7.1 Socket.IO (pola sama dengan acuan)
- Event dari frontend: `add_ied`, `update_ied`, `remove_ied`, `add_component`, `update_component`, `remove_component`, `update_order`, `set_value`, `run_scenario`, `stop_scenario`, `export_data`, `import_data`, `get_initial_data`, `subscribe_ied` / `unsubscribe_ied`.
- Event ke frontend: `ieds` (daftar IED + status), `components` (per IED), `value_changed` (perubahan nilai, ringkas), `event_log`, `error`.
- **Perbedaan dari acuan (karena banyak IED):** jangan emit seluruh daftar setiap ada perubahan. Pakai **room per IED** (hanya client yang membuka IED itu yang menerima nilainya) dan **gabungkan perubahan tiap 200–500 ms** (throttle), supaya UI tetap ringan untuk 100 IED.

### 7.2 HTTP API (🆕, untuk uji otomatis)
| Endpoint | Fungsi |
|---|---|
| `GET /health` | Health check Docker/Kubernetes |
| `GET /ieds` | Daftar IED, port, status, jumlah client, status RCB |
| `POST /ieds/{name}/scenarios/{scenario}` | Jalankan skenario (body: parameter, mis. durasi) |
| `POST /ieds/{name}/scenarios/{scenario}/stop` | Hentikan skenario |
| `PUT /ieds/{name}/points/{ref}` | Set nilai/quality satu titik |
| `GET /events?ied=&type=&limit=` | Ambil log kejadian |
| `GET /export`, `POST /import` | Sama dengan export/import di UI |

### 7.3 Log kejadian (🆕)
- Ring buffer di memori (default 5.000 kejadian), dikirim real-time ke UI lewat `event_log`.
- Jenis kejadian:
  - **client konek / putus** (IP:port client)
  - **RCB di-enable / disable** (oleh client mana, BRCB/URCB, dataset)
  - **perintah kontrol dari FEP**: select / operate / cancel, nilai, `orIdent`, `orCat`, hasil (diterima / ditolak + AddCause), CommandTermination
  - **report terkirim** (RCB, jumlah entri, reason). Dikelompokkan per detik supaya tidak banjir.
  - **perubahan nilai manual** dari UI/API
  - **skenario** mulai/selesai
- Juga ditulis ke log backend (format sama dengan logger FEP: console + file harian).

### 7.4 Penyimpanan
- Seperti acuan: in-memory + export/import JSON.
- 🆕 Opsional: `AUTOSAVE_PATH` (mis. `/data/simulator.json`), disimpan otomatis tiap perubahan konfigurasi dan dimuat saat start. Di Kubernetes pakai volume.

---

## 8. Frontend (mengikuti acuan)

Stack sama dengan acuan: React 19 + Vite + TypeScript + Tailwind 4 + komponen gaya shadcn (Radix) + sonner + @dnd-kit + socket.io-client. Gaya kartu komponen meniru `CircuitBreakerItem.tsx`, `TapChangerItem.tsx`, `TeleSignalItem.tsx`, `TeleMetryItem.tsx`.

### 8.1 Halaman **Daftar IED** (🆕)
Tabel/kartu per IED:
- nama IED, port, **status** (berjalan / mati / hang / delay), **jumlah client terhubung** (dengan IP), **mode**: RCB yang sedang aktif dan oleh siapa (*report*), atau tidak ada RCB aktif (*kemungkinan polling*)
- jumlah komponen, waktu kejadian terakhir
- tombol cepat: buka, mati/hidupkan, hang/lanjutkan, putus koneksi
- tombol tambah IED, tambah banyak IED sekaligus (mis. 20 IED dari satu template), export/import

### 8.2 Halaman **Detail IED**
- Kartu komponen (CB, tap changer, telesignal, telemetry, proteksi) seperti acuan, drag-and-drop urutan, tambah/ubah/hapus lewat dialog
- **Nilai real-time**, termasuk quality (badge warna) dan timestamp
- **Ubah manual**: tombol open/close CB, toggle Local/Remote, slider/input telemetry, raise/lower tap, toggle telesignal
- **Panel skenario**: trip, IED mati, hang, putus koneksi, quality invalid, (opsional) clock tidak sinkron, delay, badai data
- **Panel log kejadian**: filter per jenis (kontrol / report / koneksi / skenario / manual), auto-scroll, tombol jeda dan bersihkan

### 8.3 Lain-lain
- Toast untuk error dan konfirmasi (seperti acuan)
- Indikator koneksi UI ke backend (tersambung / terputus)
- Konfigurasi alamat backend lewat env `VITE_BACKEND_URL` (acuan memakai `VITE_FASTAPI_HOST` / `VITE_FASTAPI_PORT`)

---

## 9. Struktur repo (usulan, mengikuti acuan)

```
iec61850-simulator/
├── README.md
├── THIRD_PARTY_NOTICES.md
├── docker-compose.yml            # backend + frontend (dev)
├── docker-compose.prod.yml
├── install.sh / uninstall.sh     # seperti acuan
├── backend/
│   ├── Cargo.toml, build.rs, wrapper.h
│   ├── native/libiec61850/       # salinan dari fep-61850
│   ├── src/
│   │   ├── main.rs, config.rs, logger.rs, error.rs
│   │   ├── ffi.rs
│   │   ├── model/        # bangun IedModel dinamis dari komponen
│   │   ├── server/       # IedServer per IED, control handler, connection handler
│   │   ├── proxy/        # proxy TCP per IED (hang, putus, delay, hitung client)
│   │   ├── behavior/     # generator nilai
│   │   ├── scenario/     # trip, mati, hang, quality, ...
│   │   ├── events/       # log kejadian (ring buffer)
│   │   ├── api/          # HTTP API (axum)
│   │   ├── socket/       # Socket.IO (socketioxide)
│   │   └── storage/      # export/import, autosave
│   ├── Dockerfile.local, Dockerfile.prod, .env.example
│   └── k8s/
└── frontend/
    ├── src/ (App.tsx, socket.ts, components/, pages/, lib/)
    ├── Dockerfile.local, Dockerfile.prod, .env.example
    └── k8s/
```

---

## 10. Fase pengerjaan

| Fase | Isi | Kriteria selesai |
|---|---|---|
| **1** | Kerangka repo seperti acuan + Docker. Backend: libIEC61850 ter-build, `GET /health`. Frontend: halaman kosong yang tersambung ke Socket.IO | `docker compose up` jalan, UI menampilkan "tersambung" |
| **2** | 1 IED dengan model dinamis berisi 1 CB + 1 telemetry + 1 telesignal, nilai tetap | `tools/iec61850-cli browse` dari repo FEP bisa membaca model & nilai |
| **3** | Generator nilai (random, toggle, random_walk, sine, follow) + `q` & `t` + dataset & RCB otomatis | FEP (mode REPORT) menerima report berubah-ubah |
| **4** | Kontrol CB (semua ctlModel, waktu tempuh, Local/Remote, AddCause, CommandTermination) + tap changer | Perintah dari FEP membuka/menutup CB, status ikut berubah |
| **5** | Banyak IED + proxy TCP + jumlah client + log kejadian (backend) | 20 IED jalan, jumlah client benar, log kontrol tercatat |
| **6** | Frontend: daftar IED + detail IED + kartu komponen seperti acuan + ubah manual + drag-and-drop | Tester bisa mengoperasikan simulator dari UI |
| **7** | Skenario (trip, mati, hang, putus koneksi, quality invalid, clock, delay, badai) di UI + HTTP API + panel log kejadian | Semua skenario terpicu dari UI dan `curl`, dan terlihat efeknya di FEP |
| **8** | Tambah/ubah/hapus komponen dari UI (restart IED otomatis) + export/import + autosave | Konfigurasi bertahan setelah restart container |
| **9** | Uji beban 100 IED, Dockerfile prod, manifest Kubernetes, `install.sh` | Laporan CPU/memori/latensi UI untuk 100 IED |
| **10** (opsional) | Import model dari file SCL (ICD/CID) | IED dari file ICD vendor bisa disimulasikan |

Mulai dari **Fase 1**.