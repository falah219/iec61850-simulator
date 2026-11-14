# Laporan uji beban: 100 IED (fase 9)

Tanggal uji: 14-11-2026. Lingkungan: laptop Windows 11 + Docker Desktop (8 CPU, 7,7 GiB untuk Docker),
backend & klien uji di container yang sama host-nya. Angka di server Linux biasanya lebih baik
(tanpa lapisan VM Docker Desktop).

## Ringkasan

| Kondisi | CPU backend | Memori backend |
|---|---|---|
| 100 IED idle, **sebelum** patch libIEC61850 | ~280% (hampir 3 core) | ~55 MiB |
| 100 IED idle, setelah patch | ~55-80% | ~53 MiB |
| 100 IED + 100 klien (masing-masing 2 BRCB aktif) + kontrol | ~55-60% | ~70 MiB |
| Puncak: di atas + badai data di 100 IED (10 perubahan/s per titik, ~9.000 update/s) | ~200-300% | ~117 MiB |

| Latensi | p50 | p95 | max |
|---|---|---|---|
| UI: `set_value` -> nilai kembali lewat `value_changed` (normal) | 177 ms | 343 ms | 344 ms |
| UI: sama, saat beban puncak | 246 ms | 334 ms | 346 ms |
| Kontrol SBO-enhanced: select -> CommandTermination+ (termasuk tempuh CB 100 ms), 100 klien report | 113 ms | 148 ms | 279 ms |
| Kontrol sama, saat beban puncak | 166 ms | 419 ms | 1.784 ms |

| UI (browser, stack prod nginx) | Hasil |
|---|---|
| Daftar 100 IED tampil (muat halaman + data) | 1,2 s |
| Buka detail IED | 0,5 s |
| Daftar `ieds` 100 IED (Socket.IO) | ~24 KB, diterima 45-75 ms setelah konek |
| Memori JS halaman detail | 13 MB |

Report: ~400-560 report/s dari generator normal (100 IED x 2 BRCB); ~10.000 report/s saat badai.
Semua 100 klien terhubung & 200 BRCB aktif dalam ~6 s. `client_count` = `ied_connections` = 100.

Latensi UI dibatasi desain: tick generator 100 ms + throttle `value_changed` 300 ms (PLAN 7.1), jadi
maksimal ~400 ms; tidak bertambah saat beban puncak. Kontrol melambat saat badai karena antrean report.

## Perbaikan yang ditemukan uji ini

1. **CPU idle tinggi.** Build libIEC61850 memakai `CONFIG_MMS_SINGLE_THREADED` (default CMake): satu loop
   per IED yang `poll()` 1 ms lalu menjalankan tugas periodik -> 1000 putaran/detik per IED. Patch:
   `poll()` 10 ms ([backend/native/patches/0001-cpu-idle-loop.patch](../backend/native/patches/0001-cpu-idle-loop.patch),
   dicatat di THIRD_PARTY_NOTICES.md). Koneksi & data tetap langsung diproses; resolusi timer report /
   sboTimeout / polling operate jadi 10 ms. (Dengan 100 ms CPU idle ~26%, tapi CommandTermination bisa
   telat sampai 100 ms, jadi dipilih 10 ms.)
2. **Binary tidak di-link ulang saat libIEC61850 berubah** (cache `target`): `build.rs` sekarang memantau
   `libiec61850.a` / `libhal.a`. Tanpa ini patch (atau versi baru dari FEP) tidak masuk ke binary.
3. **`Cargo.lock` di repo belum ikut `serde` (fase 8)**: build lokal memperbaruinya diam-diam di container;
   build prod (`--locked`) menangkapnya.
4. **`envsubst` mengganti variabel di komentar manifest** -> YAML rusak. Komentar dibetulkan dan
   `install.sh` hanya mengganti variabel yang terdaftar.

## Kebutuhan resource (dasar `backend/k8s/deployment.yaml`)

- request 500m CPU / 128 MiB, limit 3 CPU / 512 MiB (puncak terukur ~3 CPU / 120 MiB).
- Frontend (nginx): request 10m / 16 MiB.
- Batas desain lain: maks 5 koneksi MMS per IED (default libIEC61850), 100 port MMS di compose /
  install.sh (bisa ditambah).

## Cara mengulang

```sh
# 100 IED (volume baru, karena IED_COUNT hanya dipakai saat belum ada autosave)
docker compose down -v
# backend/.env: IED_COUNT=100
docker compose up -d --build
docker stats iec61850_backend_simulator

# 100 klien MMS (2 BRCB per IED) + kontrol di 5 IED, 30 detik
docker build -f backend/Dockerfile.local --target builder -t iec61850-sim-builder backend
docker run --rm -v "$PWD/tools/loadtest:/w" iec61850-sim-builder sh -c "gcc -O2 -I/usr/local/include/libiec61850 /w/loadtest.c -o /tmp/lt /usr/local/lib/libiec61850.a /usr/local/lib/libhal.a -lpthread -lm -lrt && /tmp/lt host.docker.internal 100 30 5"

# beban puncak: badai data di semua IED
for n in $(seq 1 100); do curl -s -X POST localhost:6061/ieds/SIMIED$n/scenarios/storm -d '{"rate":10,"duration":40}'; done
```

Latensi UI diukur dengan klien socket.io (`set_value` lalu tunggu `value_changed` berisi nilai yang sama).
