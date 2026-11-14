# Third-party notices

## libIEC61850

| | |
|---|---|
| Versi | **v1.6.2.1** (tag git `v1.6.2.1`) |
| Commit | `6e7a603c771d6124cd3cafea7ec38b7e0f2e7aaf` |
| Sumber | https://github.com/mz-automation/libiec61850 |
| Pembuat | MZ Automation GmbH |
| Lisensi | **GPLv3** (lihat [backend/native/libiec61850/COPYING](backend/native/libiec61850/COPYING)) atau lisensi komersial dari MZ Automation |
| Lokasi di repo | [backend/native/libiec61850/](backend/native/libiec61850/) |
| Dipakai oleh | `backend/Dockerfile.local`: build dari folder ini, tanpa unduh dari internet. Di-link statis ke binary `iec61850-simulator`. |

### Asal salinan

Folder ini disalin **apa adanya** dari repo FEP `fep-61850` (`native/libiec61850/`), supaya versinya
sama persis dengan FEP. Perubahan terhadap source asli sama dengan yang dicatat di
`THIRD_PARTY_NOTICES.md` repo FEP:

- Folder `.git` tidak disertakan.
- `examples/`: hanya `server_example_basic_io` dan `mms_utility` yang disertakan; `examples/CMakeLists.txt`
  diubah untuk hanya memanggil keduanya. (Simulator ini build dengan `-DBUILD_EXAMPLES=OFF`.)
- Test & fuzzing yang tidak dipakai dihapus: `fuzz/`, `dotnet/tests/`, `pyiec61850/test_pyiec61850.py`.

File lain (termasuk `COPYING`, `README.md`, `CHANGELOG`) tidak diubah.

### Lisensi

libIEC61850 berlisensi ganda: GPLv3 atau komersial. Simulator me-link libIEC61850 secara statis dan
**hanya dipakai internal untuk pengujian** (tidak didistribusikan), jadi aman di bawah GPLv3.

### Ganti versi

Ikuti versi FEP: salin ulang `native/libiec61850/` dari repo `fep-61850`, perbarui versi & commit di
file ini, lalu build ulang image backend.
