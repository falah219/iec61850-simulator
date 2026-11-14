// Uji beban (alat dev, tidak ikut image produksi): seperti FEP ke banyak IED sekaligus.
// - satu koneksi MMS per IED, enable brcbMX01 + brcbST01 (dchg/qchg/GI), hitung report per detik
// - tiap detik operate CSWI1.Pos (open/close bergantian) di `ctl` IED pertama, ukur waktu sampai
//   CommandTermination (sudah termasuk waktu tempuh CB)
//
// loadtest <host> <jumlah_ied> <detik> [ctl=5] [port1=102] [port_base=10102] [prefix=SIMIED] [ld=LD0]
// IED ke-n: nama <prefix><n>, port port1 (n=1) / port_base+n-2 (n>=2), sama dengan simulator.
#include "iec61850_client.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static volatile long reports = 0;
static volatile int term_done = 0;

static void on_report(void* p, ClientReport r)
{
    (void) p;
    (void) r;
    __sync_fetch_and_add(&reports, 1);
}

static void on_term(void* p, ControlObjectClient c)
{
    (void) p;
    (void) c;
    term_done = 1;
}

static double now_ms(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1e3 + t.tv_nsec / 1e6;
}

static int cmp(const void* a, const void* b)
{
    double x = *(const double*) a, y = *(const double*) b;
    return (x > y) - (x < y);
}

int main(int argc, char** argv)
{
    if (argc < 4) {
        printf("pakai: loadtest <host> <jumlah_ied> <detik> [ctl=5] [port1=102] [port_base=10102] [prefix=SIMIED] [ld=LD0]\n");
        return 2;
    }
    const char* host = argv[1];
    int n = atoi(argv[2]), secs = atoi(argv[3]);
    int nctl = argc > 4 ? atoi(argv[4]) : 5;
    int port1 = argc > 5 ? atoi(argv[5]) : 102, base = argc > 6 ? atoi(argv[6]) : 10102;
    const char* prefix = argc > 7 ? argv[7] : "SIMIED";
    const char* ld = argc > 8 ? argv[8] : "LD0";

    IedConnection* con = calloc(n, sizeof(IedConnection));
    ControlObjectClient* ctl = calloc(n, sizeof(ControlObjectClient));
    int ok = 0;
    double t0 = now_ms();
    for (int i = 0; i < n; i++) {
        IedClientError err;
        int port = i == 0 ? port1 : base + i - 1;
        con[i] = IedConnection_create();
        IedConnection_connect(con[i], &err, host, port);
        if (err != IED_ERROR_OK) {
            printf("IED %d (port %d): konek gagal %d\n", i + 1, port, err);
            continue;
        }
        const char* rcbs[] = {"BR.brcbMX01", "BR.brcbST01"};
        for (int k = 0; k < 2; k++) {
            char ref[160];
            snprintf(ref, sizeof(ref), "%s%d%s/LLN0.%s", prefix, i + 1, ld, rcbs[k]);
            ClientReportControlBlock rcb = IedConnection_getRCBValues(con[i], &err, ref, NULL);
            if (err != IED_ERROR_OK) {
                printf("%s: getRCB gagal %d\n", ref, err);
                continue;
            }
            IedConnection_installReportHandler(con[i], ref, ClientReportControlBlock_getRptId(rcb), on_report, NULL);
            ClientReportControlBlock_setTrgOps(rcb, TRG_OPT_DATA_CHANGED | TRG_OPT_QUALITY_CHANGED | TRG_OPT_GI);
            ClientReportControlBlock_setRptEna(rcb, true);
            IedConnection_setRCBValues(con[i], &err, rcb, RCB_ELEMENT_TRG_OPS | RCB_ELEMENT_RPT_ENA, true);
            ClientReportControlBlock_destroy(rcb);
        }
        if (i < nctl) {
            char ref[160];
            snprintf(ref, sizeof(ref), "%s%d%s/CSWI1.Pos", prefix, i + 1, ld);
            ctl[i] = ControlObjectClient_create(ref, con[i]);
            if (ctl[i]) {
                ControlObjectClient_setOrigin(ctl[i], "loadtest", CONTROL_ORCAT_REMOTE_CONTROL);
                ControlObjectClient_setCommandTerminationHandler(ctl[i], on_term, NULL);
            }
        }
        ok++;
    }
    printf("%d/%d IED terhubung + 2 BRCB aktif dalam %.0f ms\n", ok, n, now_ms() - t0);

    double* lat = calloc(secs * (nctl > 0 ? nctl : 1) + 1, sizeof(double));
    int nlat = 0, fail = 0;
    long last = reports;
    for (int s = 0; s < secs; s++) {
        double start = now_ms();
        for (int i = 0; i < nctl && i < n; i++) {
            if (!ctl[i])
                continue;
            MmsValue* v = MmsValue_newBoolean(s % 2 == 0); // close / open bergantian
            term_done = 0;
            double t = now_ms();
            bool sel = ControlObjectClient_getControlModel(ctl[i]) != CONTROL_MODEL_SBO_ENHANCED || ControlObjectClient_selectWithValue(ctl[i], v);
            if (sel && ControlObjectClient_operate(ctl[i], v, 0)) {
                while (!term_done && now_ms() - t < 3000)
                    usleep(1000);
                if (term_done)
                    lat[nlat++] = now_ms() - t;
                else
                    fail++;
            } else {
                fail++; // mis. position-reached di detik pertama
            }
            MmsValue_delete(v);
        }
        double left = 1000 - (now_ms() - start);
        if (left > 0)
            usleep((useconds_t) (left * 1000));
        long r = reports;
        printf("detik %2d: %6ld report/s\n", s + 1, r - last);
        fflush(stdout);
        last = r;
    }
    printf("total %ld report dalam %d s (rata-rata %.0f/s)\n", reports, secs, (double) reports / secs);
    if (nlat > 0) {
        qsort(lat, nlat, sizeof(double), cmp);
        printf("kontrol select+operate -> CommandTermination (termasuk tempuh CB): n=%d p50=%.0f ms p95=%.0f ms max=%.0f ms, gagal %d\n",
               nlat, lat[nlat / 2], lat[(int) (nlat * 0.95)], lat[nlat - 1], fail);
    }
    for (int i = 0; i < n; i++) {
        if (ctl[i])
            ControlObjectClient_destroy(ctl[i]);
        IedConnection_close(con[i]);
        IedConnection_destroy(con[i]);
    }
    return 0;
}
