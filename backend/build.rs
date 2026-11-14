use std::env;
use std::path::PathBuf;

// libIEC61850 di-build & di-install ke /usr/local oleh Dockerfile (dari native/libiec61850).
const INCLUDE_DIR: &str = "/usr/local/include/libiec61850";

fn main() {
    println!("cargo:rustc-link-search=native=/usr/local/lib");
    // Library statis: iec61850 bergantung pada hal (socket, thread, time)
    println!("cargo:rustc-link-lib=static=iec61850");
    println!("cargo:rustc-link-lib=static=hal");
    println!("cargo:rustc-link-lib=pthread");
    println!("cargo:rustc-link-lib=m");
    println!("cargo:rustc-link-lib=rt");
    println!("cargo:rerun-if-changed=wrapper.h");

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{INCLUDE_DIR}"))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .prepend_enum_name(false)
        // Server + model dinamis + CDC + handler kontrol & RCB; API client (kontrol, RCB) untuk test & perintah `ctl`
        .allowlist_function(
            "LibIEC61850_getVersionString|IedServer_.*|IedModel_.*|LogicalDevice_create|LogicalNode_create|CDC_.*_create|Dbpos_.*\
             |DataSet_create|DataSetEntry_create|DataAttribute_create|ReportControlBlock_create\
             |ReportControlBlock_getName|ReportControlBlock_isBuffered|ReportControlBlock_getRptEna|ControlAction_.*|ClientConnection_getPeerAddress\
             |MmsValue_getBoolean|MmsValue_newBoolean|MmsValue_delete|Quality_fromMmsValue|MmsValue_getUtcTimeQuality\
             |IedConnection_create|IedConnection_connect|IedConnection_close|IedConnection_destroy|ControlObjectClient_.*\
             |IedConnection_getRCBValues|IedConnection_setRCBValues|ClientReportControlBlock_.*",
        )
        .allowlist_var("CDC_.*|TRG_OPT_.*|RPT_OPT_.*|CONTROL_ORCAT_.*|RCB_ELEMENT_.*|QUALITY_.*")
        .wrap_unsafe_ops(true)
        .generate()
        .expect("gagal generate binding libIEC61850");

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    bindings.write_to_file(out.join("bindings.rs")).expect("gagal menulis bindings.rs");
}
