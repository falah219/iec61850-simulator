//! Simulator IED IEC 61850: HTTP API + Socket.IO untuk UI tester.
//! N IED contoh (IED_COUNT) di belakang proxy TCP, log kejadian, GET /ieds & /events, Socket.IO untuk UI.
//! `iec61850-simulator ctl <host> <port> <reference> <nilai> [--interlock]`: kirim perintah kontrol (uji tanpa FEP).

mod behavior;
mod client;
mod config;
mod events;
mod ffi;
mod logger;
mod manager;
mod model;
mod proxy;
mod server;

use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::{json, Value};
use socketioxide::extract::{AckSender, Data, SocketRef};
use socketioxide::SocketIo;
use tower_http::cors::CorsLayer;
use tracing::{error, info};

use behavior::{Gen, Mode};
use model::{Cb, Component, CtlModel, IedConfig, Kind, Mx, ProtClass};

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

/// State bersama HTTP API & Socket.IO.
struct App {
    slots: Vec<manager::Slot>,
    events: Arc<events::Events>,
    io: SocketIo,
}

impl App {
    fn ieds_json(&self) -> Value {
        Value::Array(self.slots.iter().map(manager::Slot::info).collect())
    }

    fn slot(&self, name: &str) -> Result<&manager::Slot, String> {
        self.slots.iter().find(|s| s.name == name).ok_or_else(|| format!("IED {name} tidak ada"))
    }

    /// IED yang sedang hidup
    fn ied(&self, name: &Value) -> Result<Arc<server::Ied>, String> {
        self.slot(name.as_str().unwrap_or_default())?.running()
    }

    /// Kirim `components` ke UI yang membuka IED ini (mis. setelah urutan berubah / IED hidup lagi)
    fn push_components(&self, name: &str) {
        if let Some(ied) = self.slot(name).ok().and_then(manager::Slot::ied) {
            let _ = self.io.to(room(name)).emit("components", &ied.components_json());
        }
    }

    async fn run_scenario(&self, name: &str, sc: &str, p: &Value) -> Result<String, String> {
        self.slot(name)?.run_scenario(sc, p).await
    }

    async fn stop_scenario(&self, name: &str, sc: &str) -> Result<String, String> {
        let r = self.slot(name)?.stop_scenario(sc).await;
        if r.is_ok() && sc == "off" {
            self.push_components(name); // IED baru: UI yang terbuka perlu konfigurasi + nilai awal
        }
        r
    }
}

/// Room Socket.IO per IED
fn room(ied: &str) -> String {
    format!("ied:{ied}")
}

fn ack_json(r: Result<String, String>) -> Value {
    match r {
        Ok(m) => json!({ "ok": true, "message": m }),
        Err(m) => json!({ "ok": false, "message": m }),
    }
}

async fn run() -> Result<(), String> {
    let cfg = config::Config::from_env()?;
    info!("simulator mulai, libIEC61850 {}", lib_version());

    let cfgs = (1..=cfg.ied_count).map(|n| demo_ied(&cfg, n)).collect::<Result<Vec<_>, _>>()?;
    model::validate_all(&cfgs)?;
    let events = Arc::new(events::Events::new());
    let mut slots = Vec::new();
    for c in cfgs {
        let ld = c.ld.clone();
        let slot = manager::Slot::start(c, Arc::clone(&events))?;
        info!("IED {} jalan, MMS port {}, domain {}{ld}", slot.name, slot.port, slot.name);
        slots.push(slot);
    }
    let (sio_layer, io) = SocketIo::new_layer();
    let app = Arc::new(App { slots, events, io: io.clone() });

    let a = Arc::clone(&app);
    io.ns("/", move |s: SocketRef| {
        info!("UI tersambung: {}", s.id);
        let _ = s.emit("ieds", &a.ieds_json());
        let recent: Vec<Value> = a.events.query(None, None, 200).iter().map(events::Event::json).collect();
        let _ = s.emit("event_log", &recent);
        s.on_disconnect(|s: SocketRef| info!("UI terputus: {}", s.id));

        let a2 = Arc::clone(&a);
        s.on("get_initial_data", move |s: SocketRef| {
            let _ = s.emit("ieds", &a2.ieds_json());
        });
        // Buka halaman detail IED: masuk room IED itu (satu IED per tab), terima `components` lalu `value_changed`.
        // IED yang sedang mati: tetap masuk room, `components` dikirim saat hidup lagi.
        let a2 = Arc::clone(&a);
        s.on("subscribe_ied", move |s: SocketRef, Data(d): Data<Value>| {
            let name = d["ied"].as_str().unwrap_or_default();
            match a2.slot(name) {
                Ok(slot) => {
                    s.leave_all();
                    s.join(room(name));
                    if let Some(ied) = slot.ied() {
                        let _ = s.emit("components", &ied.components_json());
                    }
                }
                Err(e) => {
                    let _ = s.emit("error", &json!({ "message": e }));
                }
            }
        });
        s.on("unsubscribe_ied", |s: SocketRef| {
            s.leave_all();
        });
        // Ubah manual; hasil lewat ack {ok, message} supaya UI bisa menampilkan toast
        let a2 = Arc::clone(&a);
        s.on("set_value", move |Data(d): Data<Value>, ack: AckSender| {
            let res = a2.ied(&d["ied"]).and_then(|ied| ied.set(d["id"].as_str().unwrap_or_default(), &d));
            let _ = ack.send(&ack_json(res));
        });
        // Skenario (PLAN bagian 6): {ied, scenario, params} / {ied, scenario}
        let a2 = Arc::clone(&a);
        s.on("run_scenario", move |Data(d): Data<Value>, ack: AckSender| async move {
            let res = a2.run_scenario(d["ied"].as_str().unwrap_or_default(), d["scenario"].as_str().unwrap_or_default(), &d["params"]).await;
            let _ = ack.send(&ack_json(res));
        });
        let a2 = Arc::clone(&a);
        s.on("stop_scenario", move |Data(d): Data<Value>, ack: AckSender| async move {
            let res = a2.stop_scenario(d["ied"].as_str().unwrap_or_default(), d["scenario"].as_str().unwrap_or_default()).await;
            let _ = ack.send(&ack_json(res));
        });
        let a2 = Arc::clone(&a);
        s.on("update_order", move |Data(d): Data<Value>, ack: AckSender| {
            let res = a2.ied(&d["ied"]).and_then(|ied| {
                let ids = d["order"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
                ied.set_order(ids).map(|_| "urutan disimpan".to_string())
            });
            if res.is_ok() {
                a2.push_components(d["ied"].as_str().unwrap_or_default());
            }
            let _ = ack.send(&ack_json(res));
        });
    });
    // Nilai live: per IED tiap 300 ms, hanya yang berubah, hanya ke room IED itu (PLAN 7.1)
    let a = Arc::clone(&app);
    tokio::spawn(async move {
        let mut last: Vec<Vec<Value>> = vec![Vec::new(); a.slots.len()];
        let mut tick = tokio::time::interval(Duration::from_millis(300));
        loop {
            tick.tick().await;
            for (slot, last) in a.slots.iter().zip(&mut last) {
                let Some(ied) = slot.ied() else {
                    last.clear();
                    continue;
                };
                let now = ied.live_values();
                let changed: Vec<&Value> = now.iter().filter(|v| !last.contains(v)).collect();
                if !changed.is_empty() {
                    let _ = a.io.to(room(&slot.name)).emit("value_changed", &json!({ "ied": slot.name, "values": changed }));
                }
                *last = now;
            }
        }
    });
    // Log kejadian real-time ke semua UI (volume kecil: report sudah dikelompokkan per detik)
    let mut rx = app.events.subscribe();
    let io2 = io.clone();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(e) => {
                    let _ = io2.emit("event_log", &[e.json()]);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    });
    // Daftar IED (status, jumlah client, RCB aktif) dikirim ulang hanya kalau berubah, maks 1x per detik
    let a = Arc::clone(&app);
    tokio::spawn(async move {
        let mut last = Value::Null;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            let now = a.ieds_json();
            // last_event berubah terus selama ada report; tidak memicu kirim ulang sendiri
            let mut cmp = now.clone();
            cmp.as_array_mut().into_iter().flatten().for_each(|i| i["last_event"] = Value::Null);
            if cmp != last {
                let _ = a.io.emit("ieds", &now);
                last = cmp;
            }
        }
    });

    // CORS paling luar: UI jalan di port lain (Socket.IO polling butuh CORS)
    let http = Router::new()
        .route("/health", get(health))
        .route("/ieds", get(get_ieds))
        .route("/events", get(get_events))
        .route("/ieds/{name}/scenarios/{scenario}", post(post_scenario))
        .route("/ieds/{name}/scenarios/{scenario}/stop", post(post_scenario_stop))
        .route("/ieds/{name}/points/{reference}", put(put_point))
        .with_state(Arc::clone(&app))
        .layer(sio_layer)
        .layer(CorsLayer::permissive());
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", cfg.http_port))
        .await
        .map_err(|e| format!("gagal listen HTTP di port {}: {e}", cfg.http_port))?;
    info!("HTTP + Socket.IO di port {}", cfg.http_port);
    axum::serve(listener, http)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| format!("server HTTP berhenti karena error: {e}"))?;
    Ok(()) // IED (server MMS + proxy) berhenti saat App di-drop
}

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

/// Ok -> 200 `{ok, message}`; IED tidak ada -> 404; ditolak -> 400 `{ok: false, message}`
fn api_result(app: &App, name: &str, r: Result<String, String>) -> ApiResult {
    let code = if app.slot(name).is_err() { StatusCode::NOT_FOUND } else { StatusCode::BAD_REQUEST };
    r.map(|m| Json(ack_json(Ok(m)))).map_err(|m| (code, Json(ack_json(Err(m)))))
}

/// Body JSON opsional (kosong = `{}`)
fn body_json(b: &Bytes) -> Result<Value, (StatusCode, Json<Value>)> {
    if b.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }
    serde_json::from_slice(b).map_err(|e| (StatusCode::BAD_REQUEST, Json(ack_json(Err(format!("body bukan JSON valid: {e}"))))))
}

/// `POST /ieds/{name}/scenarios/{scenario}` (body: parameter, mis. `{"ms": 500}`)
async fn post_scenario(State(app): State<Arc<App>>, Path((name, sc)): Path<(String, String)>, body: Bytes) -> ApiResult {
    let p = body_json(&body)?;
    let r = app.run_scenario(&name, &sc, &p).await;
    api_result(&app, &name, r)
}

/// `POST /ieds/{name}/scenarios/{scenario}/stop`
async fn post_scenario_stop(State(app): State<Arc<App>>, Path((name, sc)): Path<(String, String)>) -> ApiResult {
    let r = app.stop_scenario(&name, &sc).await;
    api_result(&app, &name, r)
}

/// `PUT /ieds/{name}/points/{ref}`: ref = id komponen atau reference (`MMXU1.TotW`, `XCBR1`);
/// body sama dengan `set_value` UI, plus `{"quality": {"validity": "invalid", "flags": ["oldData"]}}`
async fn put_point(State(app): State<Arc<App>>, Path((name, r)): Path<(String, String)>, body: Bytes) -> ApiResult {
    let cmd = body_json(&body)?;
    let res = app.ied(&json!(name)).and_then(|ied| {
        let id = ied.resolve(&r).ok_or_else(|| format!("titik {r} tidak ada di {name}"))?;
        ied.set(&id, &cmd)
    });
    api_result(&app, &name, res)
}

/// `GET /ieds`: status semua IED
async fn get_ieds(State(app): State<Arc<App>>) -> Json<Value> {
    Json(app.ieds_json())
}

/// `GET /events?ied=&type=&limit=`: log kejadian terbaru (urut lama -> baru, default 100)
async fn get_events(State(app): State<Arc<App>>, Query(q): Query<HashMap<String, String>>) -> Result<Json<Value>, (StatusCode, String)> {
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    let kind = match q.get("type") {
        Some(t) => Some(events::Kind::parse(t).ok_or_else(|| bad(format!("type tidak dikenal: {t} (connection/rcb/control/report/manual/scenario)")))?),
        None => None,
    };
    let limit = match q.get("limit") {
        Some(l) => l.parse().map_err(|_| bad(format!("limit tidak valid: {l}")))?,
        None => 100,
    };
    let list = app.events.query(q.get("ied").map(String::as_str), kind, limit);
    Ok(Json(Value::Array(list.iter().map(events::Event::json).collect())))
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

/// IED contoh ke-n (semua IED memakai template yang sama). ponytail: komponen tetap di kode; tambah/ubah dari UI di fase 8
fn demo_ied(cfg: &config::Config, n: u16) -> Result<IedConfig, String> {
    let g = |mode, interval_ms, min, max, scale| Gen { mode, interval_ms, min, max, scale, step: (max - min) * 0.05, period_s: 60.0 };
    let tm = |id: &str, mx: Mx, value, gen, follow: Option<&str>| Component {
        id: id.into(),
        kind: Kind::Telemetry { mx, value, gen, follow_cb: follow.map(Into::into) },
    };
    let ts = |id: &str, ind, value, gen| Component { id: id.into(), kind: Kind::Telesignal { ggio: 1, ind, value, gen } };
    let cb = |id: &str, c: Cb| Component { id: id.into(), kind: Kind::CircuitBreaker(c) };
    Ok(IedConfig {
        name: format!("{}{n}", cfg.ied_prefix),
        ld: cfg.ld_name.clone(),
        port: cfg.port_of(n)?,
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
            Component { id: "prot1".into(), kind: Kind::Protection { class: ProtClass::Ptoc, inst: 1, cb: "cb1".into() } },
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
