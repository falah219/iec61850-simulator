//! Simulator IED IEC 61850: HTTP API + Socket.IO untuk UI tester.
//! Fase 4: satu IED contoh (generator, dataset & RCB, kontrol CB + tap changer).
//! `iec61850-simulator ctl <host> <port> <reference> <nilai> [--interlock]`: kirim perintah kontrol (uji tanpa FEP).

mod behavior;
mod client;
mod config;
mod ffi;
mod logger;
mod model;
mod server;

use std::ffi::CStr;

use axum::{routing::get, Json, Router};
use serde_json::{json, Value};
use socketioxide::{extract::SocketRef, SocketIo};
use tower_http::cors::CorsLayer;
use tracing::{error, info};

use behavior::{Gen, Mode};
use model::{Cb, Component, CtlModel, IedConfig, Kind, Mx};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("ctl") {
        std::process::exit(ctl_cmd(&args[2..]));
    }
    dotenv::dotenv().ok();
    // Guard dipegang sampai akhir main: sisa log ditulis ke file saat di-drop.
    let _log_guard = logger::init_logger();
    if let Err(e) = run().await {
        error!("{e}");
        std::process::exit(1);
    }
    info!("simulator berhenti");
}

async fn run() -> Result<(), String> {
    let cfg = config::Config::from_env()?;
    info!("simulator mulai, libIEC61850 {}", lib_version());

    let ied = server::Ied::start(&demo_ied(&cfg)?)?;
    info!("IED {} jalan, MMS port {}, domain {}{}", ied.name, ied.port, ied.name, cfg.ld_name);

    let ieds = json!([{ "name": ied.name, "port": ied.port, "status": "running" }]);
    let (sio_layer, io) = SocketIo::new_layer();
    io.ns("/", move |s: SocketRef| {
        info!("UI tersambung: {}", s.id);
        let _ = s.emit("ieds", &ieds);
        s.on_disconnect(|s: SocketRef| info!("UI terputus: {}", s.id));
    });

    // CORS paling luar: UI jalan di port lain (Socket.IO polling butuh CORS)
    let app = Router::new().route("/health", get(health)).layer(sio_layer).layer(CorsLayer::permissive());
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", cfg.http_port))
        .await
        .map_err(|e| format!("gagal listen HTTP di port {}: {e}", cfg.http_port))?;
    info!("HTTP + Socket.IO di port {}", cfg.http_port);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| format!("server HTTP berhenti karena error: {e}"))?;

    drop(ied); // stop MMS server sebelum keluar
    Ok(())
}

/// `ctl <host> <port> <reference> <nilai> [--interlock]`, mis. `ctl 127.0.0.1 102 SIMIED1LD0/CSWI1.Pos open`
fn ctl_cmd(a: &[String]) -> i32 {
    let usage = "pakai: ctl <host> <port> <reference> <close|open|true|false|raise|lower> [--interlock]";
    let res = (|| {
        let [host, port, reference, val, rest @ ..] = a else { return Err(usage.to_string()) };
        let port = port.parse().map_err(|_| format!("port tidak valid: {port}"))?;
        let val = client::CtlVal::parse(val)?;
        let interlock = rest.iter().any(|s| s == "--interlock");
        let conn = client::Conn::connect(host, port)?;
        client::control(&conn, reference, val, interlock, std::time::Duration::from_secs(10))
    })();
    match res {
        Ok(s) => {
            println!("OK: {s}");
            0
        }
        Err(e) => {
            println!("GAGAL: {e}");
            1
        }
    }
}

/// IED contoh. ponytail: komponen tetap di kode; tambah/ubah dari UI di fase 8
fn demo_ied(cfg: &config::Config) -> Result<IedConfig, String> {
    let g = |mode, interval_ms, min, max, scale| Gen { mode, interval_ms, min, max, scale, step: (max - min) * 0.05, period_s: 60.0 };
    let tm = |id: &str, mx: Mx, value, gen, follow: Option<&str>| Component {
        id: id.into(),
        kind: Kind::Telemetry { mx, value, gen, follow_cb: follow.map(Into::into) },
    };
    let ts = |id: &str, ind, value, gen| Component { id: id.into(), kind: Kind::Telesignal { ggio: 1, ind, value, gen } };
    let cb = |id: &str, c: Cb| Component { id: id.into(), kind: Kind::CircuitBreaker(c) };
    Ok(IedConfig {
        name: cfg.ied_name.clone(),
        ld: cfg.ld_name.clone(),
        port: cfg.mms_port,
        components: vec![
            cb("cb1", Cb { inst: 1, closed: true, ctl: CtlModel::SboEnhanced, ..Cb::default() }),
            cb("cb2", Cb { inst: 2, closed: false, ctl: CtlModel::SboNormal, ..Cb::default() }),
            Component {
                id: "tap1".into(),
                kind: Kind::TapChanger { inst: 1, pos: 9, min: 1, max: 17, ctl: CtlModel::DirectEnhanced, local: false, auto: false, interval_ms: 5000 },
            },
            ts("ts1", 1, true, g(Mode::Toggle, 5000, 0.0, 1.0, 0.0)),
            ts("ts2", 2, false, Gen::manual()),
            tm("mw", Mx::preset("MW", 1, "")?, 50.0, g(Mode::RandomWalk, 1000, 0.0, 100.0, 0.1), Some("cb1")),
            tm("mvar", Mx::preset("MVAR", 1, "")?, 10.0, g(Mode::Random, 2000, -20.0, 20.0, 0.5), Some("cb1")),
            tm("hz", Mx::preset("Hz", 1, "")?, 50.0, g(Mode::RandomWalk, 1000, 49.8, 50.2, 0.01), None),
            tm("kva", Mx::preset("kV", 1, "A")?, 20.0, g(Mode::Sine, 1000, 19.0, 21.0, 0.01), None),
            tm("ia", Mx::preset("A", 1, "A")?, 300.0, g(Mode::Sine, 1000, 200.0, 400.0, 0.1), Some("cb1")),
            tm("an1", Mx::preset("generic", 1, "1")?, 123.4, Gen::manual(), None),
        ],
    })
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "libiec61850": lib_version() }))
}

/// Versi libIEC61850 yang ter-link (bukti FFI jalan).
fn lib_version() -> String {
    // SAFETY: mengembalikan string statis milik library, tidak perlu di-free
    unsafe { CStr::from_ptr(ffi::LibIEC61850_getVersionString()) }.to_string_lossy().into_owned()
}

/// Ctrl+C atau SIGTERM (docker stop / Kubernetes). Sama dengan fep-61850.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
