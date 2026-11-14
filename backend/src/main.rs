//! Simulator IED IEC 61850: HTTP API + Socket.IO untuk UI tester.
//! Fase 3: satu IED contoh dengan generator nilai, dataset & RCB otomatis.

mod behavior;
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
use model::{Component, IedConfig, Kind, Mx};

#[tokio::main]
async fn main() {
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

/// IED contoh. ponytail: komponen tetap di kode; tambah/ubah dari UI di fase 8
fn demo_ied(cfg: &config::Config) -> Result<IedConfig, String> {
    let g = |mode, interval_ms, min, max, scale| Gen { mode, interval_ms, min, max, scale, step: (max - min) * 0.05, period_s: 60.0 };
    let tm = |id: &str, mx: Mx, value, gen, follow: Option<&str>| Component {
        id: id.into(),
        kind: Kind::Telemetry { mx, value, gen, follow_cb: follow.map(Into::into) },
    };
    let ts = |id: &str, value, gen| Component { id: id.into(), kind: Kind::Telesignal { ggio: 1, value, gen } };
    Ok(IedConfig {
        name: cfg.ied_name.clone(),
        ld: cfg.ld_name.clone(),
        port: cfg.mms_port,
        components: vec![
            Component { id: "cb1".into(), kind: Kind::CircuitBreaker { closed: true } },
            ts("ts1", true, g(Mode::Toggle, 5000, 0.0, 1.0, 0.0)),
            ts("ts2", false, Gen::manual()),
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
