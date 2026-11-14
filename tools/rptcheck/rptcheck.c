// Klien uji report (alat dev, tidak ikut image produksi): enable RCB, minta GI, cetak setiap report.
// rptcheck <host> <port> <rcbRef>...   mis. rptcheck host.docker.internal 102 SIMIED1LD0/LLN0.BR.brcbST01
// Lama pantau 12 detik. Build & jalankan: lihat README bagian Uji.
#include "iec61850_client.h"
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

static void handler(void* p, ClientReport r)
{
    LinkedList entries = (LinkedList) p;
    MmsValue* vals = ClientReport_getDataSetValues(r);
    printf("REPORT rptId=%s seq=%d reasons:", ClientReport_getRptId(r), ClientReport_getSeqNum(r));
    int n = LinkedList_size(entries);
    for (int i = 0; i < n; i++) {
        ReasonForInclusion rs = ClientReport_getReasonForInclusion(r, i);
        if (rs != IEC61850_REASON_NOT_INCLUDED) {
            char buf[200];
            MmsValue_printToBuffer(MmsValue_getElement(vals, i), buf, sizeof(buf));
            printf(" [%s %s=%s]", ReasonForInclusion_getValueAsString(rs),
                   (char*) LinkedList_getData(LinkedList_get(entries, i)), buf);
        }
    }
    printf("\n");
    fflush(stdout);
}

int main(int argc, char** argv)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    IedClientError err;
    IedConnection con = IedConnection_create();
    IedConnection_connect(con, &err, argv[1], atoi(argv[2]));
    if (err != IED_ERROR_OK) { printf("konek gagal %d\n", err); return 1; }

    for (int a = 3; a < argc; a++) {
        ClientReportControlBlock rcb = IedConnection_getRCBValues(con, &err, argv[a], NULL);
        if (err != IED_ERROR_OK) { printf("getRCB %s gagal %d\n", argv[a], err); return 1; }
        printf("rcb ok, dataset=%s\n", ClientReportControlBlock_getDataSetReference(rcb));
        char ds[130];
        snprintf(ds, sizeof(ds), "%s", ClientReportControlBlock_getDataSetReference(rcb));
        for (char* c = ds; *c; c++) if (*c == '$') *c = '.';
        LinkedList entries = IedConnection_getDataSetDirectory(con, &err, ds, NULL);
        printf("RCB %s rptId=%s dataset=%s (%d anggota) buffered=%d\n", argv[a],
               ClientReportControlBlock_getRptId(rcb), ds, LinkedList_size(entries),
               ClientReportControlBlock_isBuffered(rcb));
        IedConnection_installReportHandler(con, argv[a], ClientReportControlBlock_getRptId(rcb), handler, entries);
        ClientReportControlBlock_setTrgOps(rcb, TRG_OPT_DATA_CHANGED | TRG_OPT_QUALITY_CHANGED | TRG_OPT_GI);
        ClientReportControlBlock_setRptEna(rcb, true);
        IedConnection_setRCBValues(con, &err, rcb, RCB_ELEMENT_TRG_OPS | RCB_ELEMENT_RPT_ENA, true);
        if (err != IED_ERROR_OK) { printf("enable %s gagal %d\n", argv[a], err); return 1; }
        ClientReportControlBlock_setGI(rcb, true);
        IedConnection_setRCBValues(con, &err, rcb, RCB_ELEMENT_GI, true);
        printf("GI %s: %s\n", argv[a], err == IED_ERROR_OK ? "ok" : "gagal");
    }
    sleep(12);
    IedConnection_close(con);
    IedConnection_destroy(con);
    return 0;
}
