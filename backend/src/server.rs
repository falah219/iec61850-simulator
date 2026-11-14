//! Satu IED = satu IedServer libIEC61850 (listen di 127.0.0.1) dengan model dinamis + handler kontrol
//! & RCB + task generator nilai, di belakang proxy TCP yang memegang port publik.

use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::behavior::{Mode, Rng};
use crate::client::add_cause_name;
use crate::events::{Events, Kind as Ev};
use crate::ffi;
use crate::model::{self, CtlAct, CtlSpec, IedConfig, Point, Refs, Val};
use crate::proxy::{self, Client, Net};

const VENDOR: &str = "iec61850-simulator";
/// Resolusi generator. ponytail: interval titik dibulatkan ke 100 ms; kecilkan kalau butuh lebih halus
const TICK: Duration = Duration::from_millis(100);
/// Report dikelompokkan per periode ini di log kejadian (PLAN 7.3)
const REPORT_FLUSH: Duration = Duration::from_secs(1);

/// Mutex yang tetap bisa dipakai walau thread lain panic saat memegangnya (data di sini hanya daftar/hitungan).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// State bersama satu IED: dipakai proxy, handler kontrol & RCB (thread libIEC61850), generator, dan API.
pub struct IedCtx {
    pub name: String,
    events: Arc<Events>,
    clients: Mutex<Vec<Client>>,
    /// RCB aktif: (nama RCB, client yang meng-enable)
    rcbs: Mutex<Vec<(String, String)>>,
    /// jumlah report yang dibuat per RCB sejak flush terakhir
    reports: Mutex<Vec<(String, u32)>>,
    /// state jaringan proxy (hang / delay / putus)
    pub net: watch::Sender<Net>,
}

impl IedCtx {
    pub fn event(&self, kind: Ev, msg: String) {
        self.events.push(&self.name, kind, msg);
    }

    pub fn add_client(&self, c: Client) {
        lock(&self.clients).push(c);
    }

    pub fn remove_client(&self, internal_port: u16) {
        lock(&self.clients).retain(|c| c.internal_port != internal_port);
    }

    pub fn client_count(&self) -> usize {
        lock(&self.clients).len()
    }

    /// Alamat client asli dari ClientConnection libIEC61850. IedServer hanya melihat 127.0.0.1:<port proxy>,
    /// jadi dipetakan lewat port internal yang dicatat proxy.
    fn peer(&self, conn: ffi::ClientConnection) -> String {
        if conn.is_null() {
            return "-".into();
        }
        // SAFETY: conn valid selama handler; string milik library, di-copy sebelum return
        let raw = unsafe { CStr::from_ptr(ffi::ClientConnection_getPeerAddress(conn)) }.to_string_lossy().into_owned();
        let port = raw.rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
        lock(&self.clients).iter().find(|c| Some(c.internal_port) == port).map_or(raw, |c| c.peer.to_string())
    }

    /// Satu kejadian "report terkirim" per periode, berisi jumlah report per RCB.
    fn flush_reports(&self) {
        let counts = std::mem::take(&mut *lock(&self.reports));
        if !counts.is_empty() {
            let list: Vec<String> = counts.iter().map(|(r, n)| format!("{r}×{n}")).collect();
            self.event(Ev::Report, format!("report terkirim ({}s): {}", REPORT_FLUSH.as_secs(), list.join(", ")));
        }
    }
}

struct Inner {
    name: String,
    ld: String,
    model: *mut ffi::IedModel,
    server: ffi::IedServer,
    /// Parameter handler kontrol (alamat Box tetap). Di-drop SETELAH server dihancurkan (lihat Drop).
    ctls: Vec<Box<Ctl>>,
    /// Parameter handler RCB (Arc::as_ptr); ikut di-drop setelah server dihancurkan.
    ctx: Arc<IedCtx>,
}

// SAFETY: IedServer/IedModel hanya diakses lewat API libIEC61850; update nilai dilindungi
// IedServer_lockDataModel. Pointer dimiliki Inner dan dihancurkan sekali di Drop.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

impl Inner {
    /// Node model dari reference relatif ke LD, mis. `XCBR1.Pos.stVal` (atribut) atau `CSWI1.Pos` (DO).
    fn node(&self, r: &str) -> Result<*mut ffi::ModelNode, String> {
        let full = CString::new(format!("{}{}/{r}", self.name, self.ld)).map_err(|e| e.to_string())?;
        // SAFETY: model valid selama self hidup
        let n = unsafe { ffi::IedModel_getModelNodeByObjectReference(self.model, full.as_ptr()) };
        if n.is_null() {
            return Err(format!("reference {r} tidak ada di model"));
        }
        Ok(n)
    }

    fn attr(&self, r: &str) -> Result<Attr, String> {
        Ok(Attr(self.node(r)?.cast()))
    }

    /// stVal/posVal + t dari satu DO
    fn pt(&self, do_ref: &str, val: &str) -> Result<Pt, String> {
        Ok(Pt { v: self.attr(&format!("{do_ref}.{val}"))?, t: self.attr(&format!("{do_ref}.t"))? })
    }

    fn write(&self, a: &Attr, v: Val, t: Option<&Attr>) {
        write(self.server, a, v, t);
    }

    fn dbpos(&self, a: &Attr) -> ffi::Dbpos {
        dbpos(self.server, a)
    }

    /// Objek kontrol dari spec model + pasang handler libIEC61850.
    fn add_ctl(&mut self, s: &CtlSpec) -> Result<(), String> {
        let act = match &s.act {
            CtlAct::Cb { mirror, travel_ms, accept_same } => Act::Cb {
                accept_same: *accept_same,
                pos: self.pt(&s.do_ref, "stVal")?,
                mirror: self.pt(mirror, "stVal")?,
                travel: Duration::from_millis(*travel_ms),
                moving: Mutex::new(Travel::Idle),
            },
            CtlAct::Tap { mirror, auto, min, max, interval_ms } => Act::Tap {
                pos: self.pt(&s.do_ref, "valWTr.posVal")?,
                mirror: self.pt(mirror, "stVal")?,
                auto: self.attr(&format!("{auto}.stVal"))?,
                min: *min,
                max: *max,
                interval: Duration::from_millis(*interval_ms),
            },
            CtlAct::Auto => Act::Auto { st: self.pt(&s.do_ref, "stVal")? },
        };
        let ctl = Box::new(Ctl {
            name: s.do_ref.clone(),
            server: self.server,
            ctx: Arc::clone(&self.ctx),
            loc: self.attr(&format!("{}.stVal", s.loc_ref))?,
            interlock: s.interlock,
            enhanced: s.enhanced,
            act,
        });
        if s.controllable {
            let d: *mut ffi::DataObject = self.node(&s.do_ref)?.cast();
            let p = &*ctl as *const Ctl as *mut c_void;
            // SAFETY: server & DO valid; p menunjuk Box yang hidup sampai server dihancurkan
            unsafe {
                ffi::IedServer_setPerformCheckHandler(self.server, d, Some(on_check), p);
                ffi::IedServer_setControlHandler(self.server, d, Some(on_operate), p);
                ffi::IedServer_setSelectStateChangedHandler(self.server, d, Some(on_select_state), p);
            }
        }
        self.ctls.push(ctl);
        Ok(())
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // SAFETY: urutan wajib: stop -> destroy server -> destroy model. self.ctls & self.ctx di-drop
        // sesudah ini, jadi tidak ada handler yang masih bisa dipanggil saat parameternya dibebaskan.
        unsafe {
            ffi::IedServer_stop(self.server);
            ffi::IedServer_destroy(self.server);
            ffi::IedModel_destroy(self.model);
        }
    }
}

/// Pointer atribut model (dimiliki IedModel, hidup selama Inner hidup).
#[derive(Clone, Copy)]
struct Attr(*mut ffi::DataAttribute);
// SAFETY: hanya dipakai bersama Inner yang menjaga model tetap hidup; akses nilai lewat API server
unsafe impl Send for Attr {}
unsafe impl Sync for Attr {}

/// Atribut nilai + timestamp satu DO.
#[derive(Clone, Copy)]
struct Pt {
    v: Attr,
    t: Attr,
}

/// Tulis nilai (+ timestamp kalau ada). Report dchg dikirim libIEC61850 otomatis.
fn write(server: ffi::IedServer, a: &Attr, v: Val, t: Option<&Attr>) {
    // SAFETY: server & attr valid; tipe Val sesuai atribut yang dibuat di model::build.
    // Aman dipanggil dari handler kontrol (lihat dokumentasi ControlHandler libIEC61850).
    unsafe {
        ffi::IedServer_lockDataModel(server);
        match v {
            Val::Bool(b) => ffi::IedServer_updateBooleanAttributeValue(server, a.0, b),
            Val::Int(i) => ffi::IedServer_updateInt32AttributeValue(server, a.0, i),
            Val::Float(f) => ffi::IedServer_updateFloatAttributeValue(server, a.0, f as f32),
            Val::Dbpos(d) => ffi::IedServer_updateDbposValue(server, a.0, d),
            Val::Uint(u) => ffi::IedServer_updateUnsignedAttributeValue(server, a.0, u),
        }
        if let Some(t) = t {
            ffi::IedServer_updateUTCTimeAttributeValue(server, t.0, now_ms());
        }
        ffi::IedServer_unlockDataModel(server);
    }
}

fn set(server: ffi::IedServer, p: &Pt, v: Val) {
    write(server, &p.v, v, Some(&p.t));
}

fn dbpos(server: ffi::IedServer, a: &Attr) -> ffi::Dbpos {
    // SAFETY: server & attr valid, attr bertipe Dbpos
    unsafe { ffi::Dbpos_fromMmsValue(ffi::IedServer_getAttributeValue(server, a.0)) }
}

fn get_bool(server: ffi::IedServer, a: &Attr) -> bool {
    // SAFETY: server & attr valid, attr BOOLEAN
    unsafe { ffi::IedServer_getBooleanAttributeValue(server, a.0) }
}

fn get_int(server: ffi::IedServer, a: &Attr) -> i32 {
    // SAFETY: server & attr valid, attr INT
    unsafe { ffi::IedServer_getInt32AttributeValue(server, a.0) }
}

fn get_time(server: ffi::IedServer, a: &Attr) -> u64 {
    // SAFETY: server & attr valid, attr Timestamp
    unsafe { ffi::IedServer_getUTCTimeAttributeValue(server, a.0) }
}

fn dbpos_name(d: ffi::Dbpos) -> &'static str {
    match d {
        ffi::DBPOS_ON => "close",
        ffi::DBPOS_OFF => "open",
        ffi::DBPOS_INTERMEDIATE_STATE => "intermediate",
        _ => "bad",
    }
}

/// Flag quality (selain validity) beserta namanya di UI / API
const QUALITY_FLAGS: [(u32, &str); 11] = [
    (ffi::QUALITY_DETAIL_OVERFLOW, "overflow"),
    (ffi::QUALITY_DETAIL_OUT_OF_RANGE, "outOfRange"),
    (ffi::QUALITY_DETAIL_BAD_REFERENCE, "badReference"),
    (ffi::QUALITY_DETAIL_OSCILLATORY, "oscillatory"),
    (ffi::QUALITY_DETAIL_FAILURE, "failure"),
    (ffi::QUALITY_DETAIL_OLD_DATA, "oldData"),
    (ffi::QUALITY_DETAIL_INCONSISTENT, "inconsistent"),
    (ffi::QUALITY_DETAIL_INACCURATE, "inaccurate"),
    (ffi::QUALITY_SOURCE_SUBSTITUTED, "substituted"),
    (ffi::QUALITY_TEST, "test"),
    (ffi::QUALITY_OPERATOR_BLOCKED, "operatorBlocked"),
];

/// "good" | "invalid+oldData" | ...
fn quality_text(q: u32) -> String {
    let mut t = vec![match q & 3 {
        ffi::QUALITY_VALIDITY_GOOD => "good",
        ffi::QUALITY_VALIDITY_INVALID => "invalid",
        ffi::QUALITY_VALIDITY_QUESTIONABLE => "questionable",
        _ => "reserved",
    }];
    t.extend(QUALITY_FLAGS.iter().filter(|(bit, _)| q & bit != 0).map(|(_, n)| *n));
    t.join("+")
}

/// `{"validity": "invalid"|"questionable"|"good", "flags": ["oldData", ...]}` -> Quality (default invalid)
fn parse_quality(p: &Value) -> Result<u16, String> {
    let mut q = match p["validity"].as_str().unwrap_or("invalid") {
        "good" => ffi::QUALITY_VALIDITY_GOOD,
        "invalid" => ffi::QUALITY_VALIDITY_INVALID,
        "questionable" => ffi::QUALITY_VALIDITY_QUESTIONABLE,
        v => return Err(format!("validity tidak dikenal: {v} (good/invalid/questionable)")),
    };
    for f in p["flags"].as_array().into_iter().flatten() {
        let f = f.as_str().unwrap_or_default();
        let (bit, _) = QUALITY_FLAGS.iter().find(|(_, n)| *n == f).ok_or_else(|| format!("flag quality tidak dikenal: {f}"))?;
        q |= bit;
    }
    Ok(q as u16)
}

fn set_quality(server: ffi::IedServer, qs: &[Attr], q: u16) {
    // SAFETY: server & attr valid, attr Quality; report qchg dikirim libIEC61850 otomatis
    unsafe {
        ffi::IedServer_lockDataModel(server);
        for a in qs {
            ffi::IedServer_updateQuality(server, a.0, q);
        }
        ffi::IedServer_unlockDataModel(server);
    }
}

/// Quality untuk UI: `{"raw": u16, "text": "good" | "invalid+oldData" | ...}`
fn quality(server: ffi::IedServer, a: &Attr) -> Value {
    // SAFETY: server & attr valid, attr Quality (bit-string)
    let q = unsafe { ffi::Quality_fromMmsValue(ffi::IedServer_getAttributeValue(server, a.0)) } as u32;
    json!({ "raw": q, "text": quality_text(q) })
}

impl View {
    fn new(inner: &Inner, c: &model::Component) -> Result<Self, String> {
        let dos: Vec<String> = match c.refs() {
            Refs::Cb { xcbr, cswi } => vec![format!("{xcbr}.Pos"), format!("{cswi}.Pos"), format!("{xcbr}.Loc"), format!("{cswi}.Loc")],
            Refs::Tap { yltc, atcc } => vec![format!("{yltc}.TapPos"), format!("{atcc}.TapChg"), format!("{atcc}.Auto"), format!("{atcc}.Loc")],
            Refs::Prot { ln } => vec![format!("{ln}.Op"), format!("{ln}.Str")],
            Refs::Point { do_ref, .. } => vec![do_ref],
        };
        let qs = dos.iter().map(|d| inner.attr(&format!("{d}.q"))).collect::<Result<Vec<_>, _>>()?;
        let kind = match (c.refs(), &c.kind) {
            (Refs::Cb { xcbr, cswi }, model::Kind::CircuitBreaker(cb)) => VKind::Cb {
                pos: [inner.pt(&format!("{xcbr}.Pos"), "stVal")?, inner.pt(&format!("{cswi}.Pos"), "stVal")?],
                loc: [inner.pt(&format!("{xcbr}.Loc"), "stVal")?, inner.pt(&format!("{cswi}.Loc"), "stVal")?],
                q: inner.attr(&format!("{xcbr}.Pos.q"))?,
                travel: Duration::from_millis(cb.travel_ms),
            },
            (Refs::Prot { ln }, model::Kind::Protection { cb, .. }) => VKind::Prot {
                op: inner.pt(&format!("{ln}.Op"), "general")?,
                start: inner.pt(&format!("{ln}.Str"), "general")?,
                cb: cb.clone(),
            },
            (Refs::Tap { yltc, atcc }, model::Kind::TapChanger { min, max, .. }) => VKind::Tap {
                pos: inner.pt(&format!("{atcc}.TapChg"), "valWTr.posVal")?,
                mirror: inner.pt(&format!("{yltc}.TapPos"), "stVal")?,
                auto: inner.pt(&format!("{atcc}.Auto"), "stVal")?,
                loc: inner.pt(&format!("{atcc}.Loc"), "stVal")?,
                q: inner.attr(&format!("{yltc}.TapPos.q"))?,
                min: *min,
                max: *max,
            },
            (Refs::Point { do_ref, value_ref }, k) => VKind::Point {
                v: inner.attr(&value_ref)?,
                q: inner.attr(&format!("{do_ref}.q"))?,
                t: inner.attr(&format!("{do_ref}.t"))?,
                is_bool: matches!(k, model::Kind::Telesignal { .. }),
                value_ref,
            },
            _ => unreachable!("refs() sesuai kind"),
        };
        let mut desc = c.describe();
        // bentuk konfigurasi (JSON export/import), dipakai dialog ubah komponen di UI
        desc["config"] = serde_json::to_value(c).map_err(|e| e.to_string())?;
        Ok(Self { id: c.id.clone(), desc, kind, qs })
    }
}

/// Objek kontrol: parameter handler check/operate libIEC61850.
struct Ctl {
    /// DO relatif ke LD, untuk log
    name: String,
    server: ffi::IedServer,
    ctx: Arc<IedCtx>,
    loc: Attr,
    interlock: bool,
    /// security enhanced: libIEC61850 mengirim CommandTermination
    enhanced: bool,
    act: Act,
}

enum Act {
    /// CSWI.Pos (dikontrol) + XCBR.Pos (ikut)
    Cb { pos: Pt, mirror: Pt, travel: Duration, accept_same: bool, moving: Mutex<Travel> },
    /// ATCC.TapChg posVal + YLTC.TapPos (ikut)
    Tap { pos: Pt, mirror: Pt, auto: Attr, min: i32, max: i32, interval: Duration },
    Auto { st: Pt },
}

/// Tempuh CB. Selesainya tidak bergantung pada libIEC61850: kalau operate dibatalkan library
/// (mis. client putus saat security normal), tick generator tetap menyelesaikan tempuh.
#[derive(Clone, Copy, PartialEq)]
enum Travel {
    Idle,
    /// waktu selesai, posisi tujuan
    Moving(Instant, ffi::Dbpos),
    /// sudah sampai; operate yang sedang menunggu boleh mengembalikan OK
    Done,
}

// SAFETY: server & atribut hanya diakses lewat API libIEC61850 (thread-safe dengan lockDataModel);
// state Rust satu-satunya (`moving`) dilindungi Mutex.
unsafe impl Send for Ctl {}
unsafe impl Sync for Ctl {}

type Reject =(ffi::CheckHandlerResult, ffi::ControlAddCause);
const DENIED: ffi::CheckHandlerResult = ffi::CONTROL_OBJECT_ACCESS_DENIED;

impl Ctl {
    /// Uji statis sebelum select/operate. `val` null = select SBO-normal (tanpa nilai).
    fn check(&self, val: *const ffi::MmsValue, interlock_check: bool) -> Result<(), Reject> {
        if get_bool(self.server, &self.loc) {
            return Err((DENIED, ffi::ADD_CAUSE_BLOCKED_BY_SWITCHING_HIERARCHY));
        }
        if self.interlock && interlock_check {
            return Err((DENIED, ffi::ADD_CAUSE_BLOCKED_BY_INTERLOCKING));
        }
        if val.is_null() {
            return Ok(());
        }
        match &self.act {
            Act::Cb { pos, moving, accept_same, .. } => {
                if let Ok(mut m) = moving.lock() {
                    match *m {
                        Travel::Moving(..) => return Err((ffi::CONTROL_TEMPORARILY_UNAVAILABLE, ffi::ADD_CAUSE_COMMAND_ALREADY_IN_EXECUTION)),
                        Travel::Done => *m = Travel::Idle, // sisa operate yang dibatalkan library
                        Travel::Idle => {}
                    }
                }
                if !accept_same && dbpos(self.server, &pos.v) == cb_target(val) {
                    return Err((DENIED, ffi::ADD_CAUSE_POSITION_REACHED));
                }
            }
            Act::Tap { pos, auto, min, max, .. } => {
                if get_bool(self.server, auto) {
                    return Err((DENIED, ffi::ADD_CAUSE_BLOCKED_BY_MODE));
                }
                let p = get_int(self.server, &pos.v);
                match tap_step(val) {
                    Some(1) if p >= *max => return Err((DENIED, ffi::ADD_CAUSE_STEP_LIMIT)),
                    Some(-1) if p <= *min => return Err((DENIED, ffi::ADD_CAUSE_STEP_LIMIT)),
                    Some(_) => {}
                    None => return Err((ffi::CONTROL_VALUE_INVALID, ffi::ADD_CAUSE_INCONSISTENT_PARAMETERS)),
                }
            }
            Act::Auto { .. } => {}
        }
        Ok(())
    }

    /// Eksekusi. Dipanggil ulang tiap ~10 ms oleh libIEC61850 selama mengembalikan WAITING.
    fn operate(&self, val: *const ffi::MmsValue, test: bool) -> ffi::ControlHandlerResult {
        if test {
            return ffi::CONTROL_RESULT_OK; // operate mode test: tidak mengubah proses
        }
        match &self.act {
            Act::Cb { pos, mirror, travel, moving, .. } => {
                let Ok(mut m) = moving.lock() else { return ffi::CONTROL_RESULT_FAILED };
                let target = cb_target(val);
                match *m {
                    // tanpa tempuh, atau sudah di posisi tujuan (opsi accept_same): langsung selesai
                    Travel::Idle if travel.is_zero() || dbpos(self.server, &pos.v) == target => {
                        self.set_cb(target);
                        ffi::CONTROL_RESULT_OK
                    }
                    Travel::Idle => {
                        for p in [pos, mirror] {
                            set(self.server, p, Val::Dbpos(ffi::DBPOS_INTERMEDIATE_STATE));
                        }
                        *m = Travel::Moving(Instant::now() + *travel, target);
                        ffi::CONTROL_RESULT_WAITING
                    }
                    Travel::Moving(end, t) if Instant::now() >= end => {
                        self.set_cb(t);
                        *m = Travel::Idle;
                        ffi::CONTROL_RESULT_OK
                    }
                    Travel::Moving(..) => ffi::CONTROL_RESULT_WAITING,
                    // sudah diselesaikan tick generator
                    Travel::Done => {
                        *m = Travel::Idle;
                        ffi::CONTROL_RESULT_OK
                    }
                }
            }
            Act::Tap { pos, mirror, .. } => {
                let p = get_int(self.server, &pos.v) + tap_step(val).unwrap_or(0);
                set_tap(self.server, pos, mirror, p);
                ffi::CONTROL_RESULT_OK
            }
            Act::Auto { st } => {
                // SAFETY: ctlVal SPC bertipe BOOLEAN
                set(self.server, st, Val::Bool(unsafe { ffi::MmsValue_getBoolean(val) }));
                ffi::CONTROL_RESULT_OK
            }
        }
    }

    fn set_cb(&self, d: ffi::Dbpos) {
        if let Act::Cb { pos, mirror, .. } = &self.act {
            for p in [pos, mirror] {
                set(self.server, p, Val::Dbpos(d));
            }
        }
    }

    /// Dari tick generator: selesaikan tempuh yang sudah lewat waktunya walau library tidak memanggil lagi.
    fn finish_travel(&self, now: Instant) {
        if let Act::Cb { moving, .. } = &self.act {
            if let Ok(mut m) = moving.lock() {
                if let Travel::Moving(end, t) = *m {
                    if now >= end {
                        self.set_cb(t);
                        *m = Travel::Done;
                    }
                }
            }
        }
    }

    fn describe(&self, val: *const ffi::MmsValue) -> String {
        if val.is_null() {
            return "select".into();
        }
        match &self.act {
            Act::Cb { .. } => if cb_target(val) == ffi::DBPOS_ON { "close" } else { "open" }.into(),
            Act::Tap { .. } => match tap_step(val) {
                Some(1) => "raise".into(),
                Some(_) => "lower".into(),
                None => "stop/invalid".into(),
            },
            // SAFETY: ctlVal SPC bertipe BOOLEAN
            Act::Auto { .. } => format!("auto={}", unsafe { ffi::MmsValue_getBoolean(val) }),
        }
    }
}

fn set_tap(server: ffi::IedServer, pos: &Pt, mirror: &Pt, p: i32) {
    set(server, pos, Val::Int(p));
    set(server, mirror, Val::Int(p));
}

/// ctlVal DPC (BOOLEAN): true = close
fn cb_target(val: *const ffi::MmsValue) -> ffi::Dbpos {
    // SAFETY: val tidak null, ctlVal DPC bertipe BOOLEAN
    if unsafe { ffi::MmsValue_getBoolean(val) } { ffi::DBPOS_ON } else { ffi::DBPOS_OFF }
}

/// ctlVal BSC (bit-string 2 bit, encoding sama dengan Dbpos): lower = 01, higher = 10
fn tap_step(val: *const ffi::MmsValue) -> Option<i32> {
    // SAFETY: val tidak null, ctlVal BSC bertipe CODEDENUM (bit-string 2)
    match unsafe { ffi::Dbpos_fromMmsValue(val) } {
        ffi::DBPOS_ON => Some(1),
        ffi::DBPOS_OFF => Some(-1),
        _ => None,
    }
}

/// Ringkasan perintah untuk log: `CSWI1.Pos select close`, `ATCC1.TapChg operate raise`
fn what(ctl: &Ctl, action: ffi::ControlAction, val: *const ffi::MmsValue) -> String {
    // SAFETY: action valid selama handler
    let op = if unsafe { ffi::ControlAction_isSelect(action) } { "select" } else { "operate" };
    if val.is_null() {
        format!("{} {op}", ctl.name)
    } else {
        format!("{} {op} {}", ctl.name, ctl.describe(val))
    }
}

/// orIdent + orCat + alamat client asli, untuk log
fn origin(ctl: &Ctl, action: ffi::ControlAction) -> String {
    let mut n = 0;
    // SAFETY: action valid selama handler; buffer orIdent milik library, dibaca sebelum return
    unsafe {
        let p = ffi::ControlAction_getOrIdent(action, &mut n);
        let ident = if p.is_null() { "-".into() } else { String::from_utf8_lossy(std::slice::from_raw_parts(p, n as usize)).into_owned() };
        let from = ctl.ctx.peer(ffi::ControlAction_getClientConnection(action));
        format!("orIdent={ident} orCat={} dari {from}", ffi::ControlAction_getOrCat(action))
    }
}

unsafe extern "C" fn on_check(
    action: ffi::ControlAction,
    p: *mut c_void,
    val: *mut ffi::MmsValue,
    _test: bool,
    interlock_check: bool,
) -> ffi::CheckHandlerResult {
    // SAFETY: p = Box<Ctl> dari add_ctl, hidup sampai server dihancurkan
    let ctl = unsafe { &*(p as *const Ctl) };
    catch_unwind(AssertUnwindSafe(|| {
        let ilk = if interlock_check { ", interlock-check" } else { "" };
        match ctl.check(val, interlock_check) {
            Ok(()) => {
                ctl.ctx.event(Ev::Control, format!("{} diterima{ilk} ({})", what(ctl, action, val), origin(ctl, action)));
                ffi::CONTROL_ACCEPTED
            }
            Err((res, cause)) => {
                // SAFETY: action valid selama handler
                unsafe { ffi::ControlAction_setAddCause(action, cause) };
                let msg = format!("{} ditolak: {}{ilk} ({})", what(ctl, action, val), add_cause_name(cause), origin(ctl, action));
                ctl.ctx.event(Ev::Control, msg);
                res
            }
        }
    }))
    .unwrap_or(ffi::CONTROL_HARDWARE_FAULT)
}

unsafe extern "C" fn on_operate(action: ffi::ControlAction, p: *mut c_void, val: *mut ffi::MmsValue, test: bool) -> ffi::ControlHandlerResult {
    // SAFETY: p = Box<Ctl> dari add_ctl, hidup sampai server dihancurkan
    let ctl = unsafe { &*(p as *const Ctl) };
    catch_unwind(AssertUnwindSafe(|| {
        let r = ctl.operate(val, test);
        let term = match (ctl.enhanced, r) {
            (_, ffi::CONTROL_RESULT_WAITING) => return r,
            (true, ffi::CONTROL_RESULT_OK) => ", CommandTermination+",
            (true, _) => ", CommandTermination-",
            (false, _) => "",
        };
        let res = if r == ffi::CONTROL_RESULT_OK { "selesai" } else { "gagal" };
        let test = if test { " [test]" } else { "" };
        ctl.ctx.event(Ev::Control, format!("{} {res}{test}{term} ({})", what(ctl, action, val), origin(ctl, action)));
        r
    }))
    .unwrap_or(ffi::CONTROL_RESULT_FAILED)
}

/// Select batal karena cancel / sboTimeout / client putus (select & operate sudah tercatat di handler lain).
unsafe extern "C" fn on_select_state(action: ffi::ControlAction, p: *mut c_void, _selected: bool, reason: ffi::SelectStateChangedReason) {
    // SAFETY: p = Box<Ctl> dari add_ctl, hidup sampai server dihancurkan
    let ctl = unsafe { &*(p as *const Ctl) };
    let why = match reason {
        ffi::SELECT_STATE_REASON_CANCELED => "cancel",
        ffi::SELECT_STATE_REASON_TIMEOUT => "sboTimeout habis",
        ffi::SELECT_STATE_REASON_DISCONNECTED => "client putus",
        _ => return,
    };
    let _ = catch_unwind(AssertUnwindSafe(|| ctl.ctx.event(Ev::Control, format!("{} unselect: {why} ({})", ctl.name, origin(ctl, action)))));
}

unsafe extern "C" fn on_rcb(
    p: *mut c_void,
    rcb: *mut ffi::ReportControlBlock,
    conn: ffi::ClientConnection,
    event: ffi::IedServer_RCBEventType,
    _param: *const c_char,
    _err: ffi::MmsDataAccessError,
) {
    // SAFETY: p = Arc<IedCtx> milik Inner, hidup sampai server dihancurkan; rcb valid selama handler
    let ctx = unsafe { &*(p as *const IedCtx) };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let name = unsafe { CStr::from_ptr(ffi::ReportControlBlock_getName(rcb)) }.to_string_lossy().into_owned();
        match event {
            // libIEC61850 juga mem-buffer report untuk BRCB yang disable; yang dihitung hanya RCB aktif (= terkirim)
            ffi::RCB_EVENT_REPORT_CREATED if unsafe { ffi::ReportControlBlock_getRptEna(rcb) } => {
                let mut r = lock(&ctx.reports);
                match r.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, c)) => *c += 1,
                    None => r.push((name, 1)),
                }
            }
            ffi::RCB_EVENT_ENABLE => {
                let who = ctx.peer(conn);
                let kind = if unsafe { ffi::ReportControlBlock_isBuffered(rcb) } { "BRCB" } else { "URCB" };
                let ds = if name.contains("ST") { "DS_ST" } else { "DS_MX" };
                ctx.event(Ev::Rcb, format!("{name} ({kind}, dataset {ds}) di-enable oleh {who}"));
                let mut r = lock(&ctx.rcbs);
                r.retain(|(n, _)| *n != name);
                r.push((name, who));
            }
            ffi::RCB_EVENT_DISABLE => {
                ctx.flush_reports(); // report sebelum disable tercatat lebih dulu
                lock(&ctx.rcbs).retain(|(n, _)| *n != name);
                let who = if conn.is_null() { "(client putus)".into() } else { format!("oleh {}", ctx.peer(conn)) };
                ctx.event(Ev::Rcb, format!("{name} di-disable {who}"));
            }
            ffi::RCB_EVENT_GI => ctx.event(Ev::Rcb, format!("{name} GI oleh {}", ctx.peer(conn))),
            ffi::RCB_EVENT_OVERFLOW => ctx.event(Ev::Rcb, format!("{name} buffer overflow")),
            _ => {}
        }
    }));
}

pub struct Ied {
    pub name: String,
    pub port: u16,
    pub ctx: Arc<IedCtx>,
    inner: Arc<Inner>,
    /// Titik data + state generator (dipakai bersama task generator & ubah manual dari UI)
    points: Arc<Mutex<Vec<Live>>>,
    /// Tampilan per komponen, urut sesuai konfigurasi
    views: Vec<View>,
    /// Urutan kartu di UI (id komponen). ponytail: hanya di memori; disimpan bersama autosave di fase 8
    order: Mutex<Vec<String>>,
    /// skenario yang sedang aktif selain jaringan: (quality, clock) untuk status
    scen: Mutex<(Option<String>, Option<String>)>,
    /// task proxy & generator; diambil oleh [`Ied::shutdown`]
    proxy: Mutex<Option<JoinHandle<()>>>,
    task: Mutex<Option<JoinHandle<()>>>,
    /// task badai data yang sedang jalan
    storm: Mutex<Option<JoinHandle<()>>>,
}

/// Satu komponen untuk UI: deskripsi statis + atribut untuk baca nilai live & ubah manual.
struct View {
    id: String,
    desc: Value,
    kind: VKind,
    /// semua atribut q komponen ini (skenario quality)
    qs: Vec<Attr>,
}

enum VKind {
    /// pos: [XCBR.Pos, CSWI.Pos], loc: [XCBR.Loc, CSWI.Loc]
    Cb { pos: [Pt; 2], loc: [Pt; 2], q: Attr, travel: Duration },
    /// proteksi: Op (ACT) & Str (ACD) `.general`; `cb` = id komponen CB yang di-trip
    Prot { op: Pt, start: Pt, cb: String },
    /// pos: ATCC.TapChg posVal, mirror: YLTC.TapPos
    Tap { pos: Pt, mirror: Pt, auto: Pt, loc: Pt, q: Attr, min: i32, max: i32 },
    /// telesignal (bool) / telemetry (float); state generator dicari lewat `value_ref`
    Point { value_ref: String, v: Attr, q: Attr, t: Attr, is_bool: bool },
}

impl Ied {
    /// Validasi, bangun model, isi nilai awal, pasang handler, jalankan IedServer di 127.0.0.1:<acak>
    /// dengan proxy di `0.0.0.0:port`, lalu jalankan generator. Harus dipanggil di dalam runtime tokio.
    pub fn start(cfg: &IedConfig, events: Arc<Events>) -> Result<Self, String> {
        model::validate(cfg)?;
        // Port publik dipegang proxy; gagal di sini = port bentrok, sebelum model dibuat
        let public = std::net::TcpListener::bind(("0.0.0.0", cfg.port))
            .and_then(|l| l.set_nonblocking(true).map(|_| l))
            .and_then(tokio::net::TcpListener::from_std)
            .map_err(|e| format!("IED {}: port MMS {} tidak bisa dipakai: {e}", cfg.name, cfg.port))?;
        // ponytail: port internal dipilih OS lalu dilepas sebelum IedServer_start (jeda kecil, bisa bentrok);
        // ganti ke socket yang diwariskan kalau pernah terjadi
        let internal = std::net::TcpListener::bind(("127.0.0.1", 0))
            .and_then(|l| l.local_addr())
            .map_err(|e| format!("IED {}: tidak dapat port internal: {e}", cfg.name))?
            .port();

        let built = model::build(cfg);
        // SAFETY: model valid dari build(); server memegang referensi model sampai destroy.
        let server = unsafe { ffi::IedServer_create(built.model) };
        let ctx = Arc::new(IedCtx {
            name: cfg.name.clone(),
            events,
            clients: Mutex::new(Vec::new()),
            rcbs: Mutex::new(Vec::new()),
            reports: Mutex::new(Vec::new()),
            net: watch::Sender::new(Net::default()),
        });
        let mut inner = Inner { name: cfg.name.clone(), ld: cfg.ld.clone(), model: built.model, server, ctls: Vec::new(), ctx: Arc::clone(&ctx) };

        for (r, v) in &built.fixed {
            inner.write(&inner.attr(r)?, *v, None);
        }
        let vendor = CString::new(VENDOR).expect("tanpa NUL");
        for r in &built.vendor_refs {
            // SAFETY: atribut VisString, library meng-copy string
            unsafe { ffi::IedServer_updateVisibleStringAttributeValue(server, inner.attr(r)?.0, vendor.as_ptr().cast_mut()) };
        }
        let mut live = Vec::new();
        for p in built.points {
            let rt = Live::new(&inner, p)?;
            inner.write(&rt.attr, rt.p.init, Some(&rt.t));
            live.push(rt);
        }
        let views = cfg.components.iter().map(|c| View::new(&inner, c)).collect::<Result<Vec<_>, _>>()?;
        for s in &built.ctls {
            inner.add_ctl(s)?;
        }
        let local = CString::new("127.0.0.1").expect("tanpa NUL");
        // SAFETY: server valid; parameter RCB = IedCtx yang dipegang Inner sampai server dihancurkan
        unsafe {
            ffi::IedServer_setRCBEventHandler(server, Some(on_rcb), Arc::as_ptr(&ctx) as *mut c_void);
            ffi::IedServer_setLocalIpAddress(server, local.as_ptr());
            ffi::IedServer_start(server, internal.into());
        }
        let inner = Arc::new(inner);

        let points = Arc::new(Mutex::new(live));
        let task = Mutex::new(Some(tokio::spawn(generate(Arc::clone(&inner), Arc::clone(&points)))));
        let proxy = Mutex::new(Some(tokio::spawn(proxy::run(public, internal, Arc::clone(&ctx)))));
        let ids: Vec<String> = views.iter().map(|v| v.id.clone()).collect();
        let mut saved = cfg.order.clone();
        saved.sort();
        let mut sorted = ids.clone();
        sorted.sort();
        // urutan tersimpan dipakai kalau masih cocok dengan komponen sekarang
        let order = Mutex::new(if saved == sorted { cfg.order.clone() } else { ids });
        let (scen, storm) = (Mutex::new((None, None)), Mutex::new(None));
        Ok(Self { name: cfg.name.clone(), port: cfg.port, ctx, inner, points, views, order, scen, proxy, task, storm })
    }

    /// IED mati: listener proxy & semua koneksi ditutup, generator berhenti, IedServer di-stop.
    /// Selesai = port publik sudah bebas. Server & model dihancurkan saat `Ied` terakhir di-drop.
    pub async fn shutdown(&self) {
        let handles = [lock(&self.proxy).take(), lock(&self.task).take(), lock(&self.storm).take()];
        for h in handles.into_iter().flatten() {
            h.abort();
            let _ = h.await; // task benar-benar selesai -> listener & koneksi tertutup
        }
        // SAFETY: server valid; idempoten (Drop Inner memanggil stop lagi)
        unsafe { ffi::IedServer_stop(self.inner.server) };
    }

    /// Urutan kartu sekarang (dibawa ke IED baru saat dihidupkan lagi)
    pub fn order(&self) -> Vec<String> {
        lock(&self.order).clone()
    }

    /// Nilai live satu komponen: posisi/nilai, auto, local, quality, timestamp
    fn live(&self, v: &View) -> Value {
        let s = self.inner.server;
        match &v.kind {
            VKind::Prot { op, start, .. } => json!({
                "id": v.id, "op": get_bool(s, &op.v), "str": get_bool(s, &start.v), "q": quality(s, &v.qs[0]), "t": get_time(s, &op.t),
            }),
            VKind::Cb { pos, loc, q, .. } => json!({
                "id": v.id, "pos": dbpos_name(dbpos(s, &pos[0].v)), "local": get_bool(s, &loc[1].v),
                "q": quality(s, q), "t": get_time(s, &pos[0].t),
            }),
            VKind::Tap { pos, auto, loc, q, mirror, .. } => json!({
                "id": v.id, "pos": get_int(s, &pos.v), "auto": get_bool(s, &auto.v), "local": get_bool(s, &loc.v),
                "q": quality(s, q), "t": get_time(s, &mirror.t),
            }),
            VKind::Point { value_ref, v: a, q, t, is_bool } => {
                let auto = lock(&self.points).iter().find(|l| l.p.value_ref == *value_ref).map(|l| l.auto);
                let value = if *is_bool {
                    json!(get_bool(s, a))
                } else {
                    // SAFETY: server & attr valid, attr FLOAT32
                    let f = unsafe { ffi::IedServer_getFloatAttributeValue(s, a.0) } as f64;
                    json!((f * 1e4).round() / 1e4) // buang noise f32 (50.099998 -> 50.1)
                };
                json!({ "id": v.id, "value": value, "auto": auto, "q": quality(s, q), "t": get_time(s, t) })
            }
        }
    }

    /// Nilai live semua komponen (untuk `value_changed`)
    pub fn live_values(&self) -> Vec<Value> {
        self.views.iter().map(|v| self.live(v)).collect()
    }

    /// Event `components`: konfigurasi + nilai live, urut sesuai urutan kartu
    pub fn components_json(&self) -> Value {
        let order = lock(&self.order).clone();
        let list = order
            .iter()
            .filter_map(|id| self.views.iter().find(|v| v.id == *id))
            .map(|v| {
                let mut d = v.desc.clone();
                if let (Some(d), Value::Object(l)) = (d.as_object_mut(), self.live(v)) {
                    d.extend(l);
                }
                d
            })
            .collect();
        json!({ "ied": self.name, "components": Value::Array(list) })
    }

    /// Urutan kartu baru; harus berisi semua id komponen tepat sekali.
    pub fn set_order(&self, ids: Vec<String>) -> Result<(), String> {
        let mut sorted = ids.clone();
        sorted.sort();
        let mut all: Vec<String> = self.views.iter().map(|v| v.id.clone()).collect();
        all.sort();
        if sorted != all {
            return Err("urutan harus berisi semua id komponen tepat sekali".into());
        }
        *lock(&self.order) = ids;
        Ok(())
    }

    /// Ubah manual dari UI / API (dicatat sebagai kejadian `manual`).
    /// CB & tap: `{"action": "open"|"close"|"raise"|"lower"}`, `{"local": bool}`, tap `{"auto": bool}`.
    /// Telesignal/telemetry: `{"value": bool|number}` (mode manual), `{"auto": bool}`.
    pub fn set(&self, id: &str, cmd: &Value) -> Result<String, String> {
        let v = self.views.iter().find(|v| v.id == id).ok_or_else(|| format!("komponen {id} tidak ada di {}", self.name))?;
        let s = self.inner.server;
        if let Some(q) = cmd.get("quality") {
            let raw = parse_quality(q)?;
            set_quality(s, &v.qs, raw);
            let msg = format!("{id} quality = {}", quality_text(raw as u32));
            self.ctx.event(Ev::Manual, format!("{msg} (dari UI/API)"));
            return Ok(msg);
        }
        let remote = |id: &str| format!("{id} mode Remote: pindah ke Local dulu untuk operasi dari UI");
        let lr = |l: bool| if l { "Local" } else { "Remote" };
        let msg = match &v.kind {
            VKind::Cb { pos, loc, .. } => {
                if let Some(l) = cmd["local"].as_bool() {
                    loc.iter().for_each(|p| set(s, p, Val::Bool(l)));
                    format!("{id} -> {}", lr(l))
                } else {
                    let (target, name) = match cmd["action"].as_str() {
                        Some("open") => (ffi::DBPOS_OFF, "open"),
                        Some("close") => (ffi::DBPOS_ON, "close"),
                        _ => return Err("aksi CB harus open / close".into()),
                    };
                    // seperti acuan: operasi di panel hanya saat Local (Remote = dikendalikan SCADA/FEP)
                    if !get_bool(s, &loc[1].v) {
                        return Err(remote(id));
                    }
                    pos.iter().for_each(|p| set(s, p, Val::Dbpos(target)));
                    format!("{id} {name}")
                }
            }
            VKind::Tap { pos, mirror, auto, loc, min, max, .. } => {
                if let Some(l) = cmd["local"].as_bool() {
                    set(s, loc, Val::Bool(l));
                    format!("{id} -> {}", lr(l))
                } else if !get_bool(s, &loc.v) {
                    return Err(remote(id));
                } else if let Some(a) = cmd["auto"].as_bool() {
                    set(s, auto, Val::Bool(a));
                    format!("{id} auto={a}")
                } else {
                    let p = get_int(s, &pos.v);
                    let n = match cmd["action"].as_str() {
                        Some("raise") => p + 1,
                        Some("lower") => p - 1,
                        _ => return Err("aksi tap harus raise / lower".into()),
                    };
                    if !(*min..=*max).contains(&n) {
                        return Err(format!("{id}: posisi {n} di luar batas {min}..{max}"));
                    }
                    set_tap(s, pos, mirror, n);
                    format!("{id} {} -> {n}", cmd["action"].as_str().unwrap_or_default())
                }
            }
            VKind::Prot { .. } => return Err(format!("{id}: pakai skenario trip")),
            VKind::Point { value_ref, is_bool, .. } => {
                let mut pts = lock(&self.points);
                let l = pts.iter_mut().find(|l| l.p.value_ref == *value_ref).ok_or("titik tidak ditemukan")?;
                if let Some(a) = cmd["auto"].as_bool() {
                    if l.p.gen.mode == Mode::Manual {
                        return Err(format!("{id} tidak punya generator (mode manual), auto tidak bisa dinyalakan"));
                    }
                    l.auto = a;
                    format!("{id} auto={a}")
                } else {
                    if l.auto {
                        return Err(format!("{id} mode auto: matikan auto dulu untuk ubah nilai manual"));
                    }
                    let n = if *is_bool {
                        cmd["value"].as_bool().map(|b| b as u8 as f64)
                    } else {
                        cmd["value"].as_f64().filter(|f| f.is_finite())
                    };
                    let n = n.ok_or_else(|| format!("nilai untuk {id} tidak valid: {}", cmd["value"]))?;
                    l.own = n; // ditulis generator pada tick berikutnya (tetap mematuhi follow CB)
                    format!("{id} = {}", cmd["value"])
                }
            }
        };
        self.ctx.event(Ev::Manual, format!("{msg} (dari UI/API)"));
        Ok(msg)
    }

    /// Komponen dari id atau reference DO/LN (`cb1`, `MMXU1.TotW`, `XCBR1`), untuk `PUT /points/{ref}`.
    pub fn resolve(&self, r: &str) -> Option<String> {
        self.views
            .iter()
            .find(|v| v.id == r || v.desc["ref"] == r || v.desc["ln"].as_str().is_some_and(|ln| ln.split(" / ").any(|l| l == r)))
            .map(|v| v.id.clone())
    }

    /// Jalankan skenario (PLAN bagian 6). Hasil dicatat sebagai kejadian `scenario`.
    pub fn run_scenario(&self, name: &str, p: &Value) -> Result<String, String> {
        let s = self.inner.server;
        let msg = match name {
            "trip" => self.trip(p)?,
            "hang" => {
                self.ctx.net.send_modify(|n| n.hang = true);
                "hang: proxy berhenti meneruskan data, TCP tetap terbuka".into()
            }
            "disconnect" => {
                let rst = match p["mode"].as_str().unwrap_or("fin") {
                    "fin" => false,
                    "rst" => true,
                    m => return Err(format!("mode putus tidak dikenal: {m} (fin/rst)")),
                };
                let n = self.ctx.client_count();
                self.ctx.net.send_modify(|n| {
                    n.epoch += 1;
                    n.rst = rst;
                });
                format!("putus koneksi ({}): {n} client ditutup, IED tetap jalan", if rst { "RST" } else { "FIN" })
            }
            "delay" => {
                let ms = p["ms"].as_u64().unwrap_or(1000);
                if !(1..=60_000).contains(&ms) {
                    return Err("delay harus 1..60000 ms".into());
                }
                self.ctx.net.send_modify(|n| n.delay_ms = ms);
                format!("delay jaringan {ms} ms")
            }
            "quality" => {
                let raw = parse_quality(p)?;
                let ids: Option<Vec<&str>> = p["components"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect());
                let mut n = 0;
                for v in self.views.iter().filter(|v| ids.as_ref().is_none_or(|ids| ids.contains(&v.id.as_str()))) {
                    set_quality(s, &v.qs, raw);
                    n += 1;
                }
                if n == 0 {
                    return Err("tidak ada komponen yang cocok".into());
                }
                let text = quality_text(raw as u32);
                lock(&self.scen).0 = Some(text.clone());
                format!("quality {text} pada {n} komponen")
            }
            "clock" => {
                let failure = p["failure"].as_bool().unwrap_or(false);
                let not_sync = p["not_sync"].as_bool().unwrap_or(!failure);
                // SAFETY: server valid; berlaku untuk timestamp yang ditulis sesudah ini
                unsafe { ffi::IedServer_setTimeQuality(s, false, failure, not_sync, 0) };
                let text = [(not_sync, "ClockNotSynchronized"), (failure, "ClockFailure")]
                    .iter()
                    .filter(|(on, _)| *on)
                    .map(|(_, t)| *t)
                    .collect::<Vec<_>>()
                    .join("+");
                lock(&self.scen).1 = Some(text.clone());
                format!("time quality {text} (berlaku untuk timestamp berikutnya)")
            }
            "storm" => {
                let rate = p["rate"].as_u64().unwrap_or(50);
                let dur = p["duration"].as_u64().unwrap_or(30);
                if !(1..=1000).contains(&rate) || !(1..=600).contains(&dur) {
                    return Err("badai: rate 1..1000 per detik, duration 1..600 s".into());
                }
                let mut st = lock(&self.storm);
                if st.as_ref().is_some_and(|h| !h.is_finished()) {
                    return Err("badai data sedang jalan".into());
                }
                *st = Some(tokio::spawn(storm(Arc::clone(&self.inner), Arc::clone(&self.points), rate, Duration::from_secs(dur))));
                format!("badai data {dur} s, semua telemetry & telesignal berubah {rate}x/detik")
            }
            _ => return Err(format!("skenario tidak dikenal: {name} (trip/off/hang/disconnect/delay/quality/clock/storm)")),
        };
        self.ctx.event(Ev::Scenario, format!("mulai {msg}"));
        Ok(msg)
    }

    /// Hentikan skenario yang punya keadaan (off -> hidupkan, hang -> lanjutkan, quality -> normalkan, ...).
    pub fn stop_scenario(&self, name: &str) -> Result<String, String> {
        let s = self.inner.server;
        let msg = match name {
            "hang" => {
                self.ctx.net.send_modify(|n| n.hang = false);
                "hang selesai, data diteruskan lagi".into()
            }
            "delay" => {
                self.ctx.net.send_modify(|n| n.delay_ms = 0);
                "delay jaringan dimatikan".into()
            }
            "quality" => {
                for v in &self.views {
                    set_quality(s, &v.qs, 0);
                }
                lock(&self.scen).0 = None;
                "quality dinormalkan (good) di semua komponen".into()
            }
            "clock" => {
                // SAFETY: server valid
                unsafe { ffi::IedServer_setTimeQuality(s, false, false, false, 0) };
                lock(&self.scen).1 = None;
                "time quality normal".into()
            }
            "storm" => {
                let h = lock(&self.storm).take().filter(|h| !h.is_finished()).ok_or("badai data tidak sedang jalan")?;
                h.abort();
                "badai data dihentikan".into()
            }
            "trip" | "disconnect" => return Err(format!("{name} sekali jalan, tidak perlu dihentikan")),
            _ => return Err(format!("skenario tidak dikenal: {name}")),
        };
        self.ctx.event(Ev::Scenario, format!("selesai {msg}"));
        Ok(msg)
    }

    /// Trip: Str & Op proteksi naik, Op pulse `pulse_ms` (default 200), CB terkait open lewat intermediate.
    fn trip(&self, p: &Value) -> Result<String, String> {
        let prot = self
            .views
            .iter()
            .find(|v| matches!(v.kind, VKind::Prot { .. }) && p["prot"].as_str().is_none_or(|id| id == v.id))
            .ok_or("tidak ada komponen proteksi (atau id prot salah)")?;
        let VKind::Prot { op, start, cb } = &prot.kind else { unreachable!() };
        let cbv = self.views.iter().find(|v| v.id == *cb).ok_or("CB proteksi tidak ditemukan")?;
        let VKind::Cb { pos, travel, .. } = &cbv.kind else { return Err("cb proteksi bukan CB".into()) };
        let pulse = Duration::from_millis(p["pulse_ms"].as_u64().unwrap_or(200).clamp(10, 10_000));
        let (op, start, pos, travel) = (*op, *start, *pos, *travel);
        let inner = Arc::clone(&self.inner);
        let was_open = dbpos(inner.server, &pos[0].v) == ffi::DBPOS_OFF;
        tokio::spawn(async move {
            let inner = &inner; // Inner Send+Sync; pointer server tidak boleh ditahan melewati await
            // CB sudah open: hanya pulsa Op (Str tidak naik, Pos tidak berubah)
            if !was_open {
                set(inner.server, &start, Val::Bool(true));
            }
            set(inner.server, &op, Val::Bool(true));
            let breaker = async {
                if was_open {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await; // waktu kerja trip coil
                // selalu Pos 00 (intermediate) -> 01 (open), juga untuk CB tanpa waktu tempuh
                pos.iter().for_each(|p| set(inner.server, p, Val::Dbpos(ffi::DBPOS_INTERMEDIATE_STATE)));
                tokio::time::sleep(travel).await;
                pos.iter().for_each(|p| set(inner.server, p, Val::Dbpos(ffi::DBPOS_OFF)));
            };
            let reset = async {
                tokio::time::sleep(pulse).await;
                set(inner.server, &op, Val::Bool(false));
                if !was_open {
                    set(inner.server, &start, Val::Bool(false));
                }
            };
            tokio::join!(breaker, reset);
        });
        let ms = pulse.as_millis();
        Ok(if was_open {
            format!("trip {}: {cb} sudah open, hanya pulsa Op {ms} ms", prot.id)
        } else {
            format!("trip {}: Str & Op pulse {ms} ms, {cb} Pos 00 -> 01", prot.id)
        })
    }

    /// Status untuk `GET /ieds` & event Socket.IO `ieds`.
    pub fn info(&self) -> Value {
        let clients = lock(&self.ctx.clients).clone();
        let rcbs = lock(&self.ctx.rcbs).clone();
        let net = *self.ctx.net.borrow();
        let scen = lock(&self.scen).clone();
        let status = if net.hang {
            "hang"
        } else {
            "running"
        };
        // "report" = ada RCB aktif; client terhubung tanpa RCB aktif = kemungkinan polling
        let mode = match (rcbs.is_empty(), clients.is_empty()) {
            (false, _) => "report",
            (true, false) => "polling",
            (true, true) => "idle",
        };
        json!({
            "name": self.name,
            "port": self.port,
            "status": status,
            "delay_ms": net.delay_ms,
            "scenarios": {
                "quality": scen.0,
                "clock": scen.1,
                "storm": lock(&self.storm).as_ref().is_some_and(|h| !h.is_finished()),
            },
            "components": self.views.len(),
            "client_count": clients.len(),
            "clients": clients.iter().map(|c| json!({ "addr": c.peer.to_string(), "since": c.since_ms })).collect::<Vec<_>>(),
            // pembanding: koneksi yang dilihat libIEC61850 (harus sama dengan client_count)
            // SAFETY: server valid selama inner hidup
            "ied_connections": unsafe { ffi::IedServer_getNumberOfOpenConnections(self.inner.server) },
            "mode": mode,
            "rcbs": rcbs.iter().map(|(r, c)| json!({ "rcb": r, "client": c })).collect::<Vec<_>>(),
            "last_event": self.ctx.events.last_ts(&self.name),
        })
    }
}

impl Drop for Ied {
    fn drop(&mut self) {
        // Proxy berhenti (koneksi client ditutup); Inner (server) dihancurkan setelah task generator
        // ikut selesai (Arc terakhir)
        for h in [lock(&self.proxy).take(), lock(&self.task).take(), lock(&self.storm).take()].into_iter().flatten() {
            h.abort();
        }
    }
}

/// Titik dengan generator / follow, beserta state runtime-nya.
struct Live {
    p: Point,
    attr: Attr,
    t: Attr,
    follow: Option<Attr>,
    /// nilai generator sendiri (dipertahankan selama di-gate follow)
    own: f64,
    /// nilai yang sekarang ada di server
    cur: f64,
    due: Instant,
    /// generator jalan (mode != manual dan auto tidak dimatikan dari UI)
    auto: bool,
}

impl Live {
    fn new(inner: &Inner, p: Point) -> Result<Self, String> {
        let v = match p.init {
            Val::Bool(b) => b as u8 as f64,
            Val::Int(i) => i.into(),
            Val::Uint(u) => u.into(),
            Val::Float(f) => f,
            Val::Dbpos(_) => 0.0, // CB tidak punya generator
        };
        Ok(Self {
            attr: inner.attr(&p.value_ref)?,
            t: inner.attr(&p.t_ref)?,
            follow: p.follow.as_deref().map(|r| inner.attr(r)).transpose()?,
            own: v,
            cur: v,
            due: Instant::now() + Duration::from_millis(p.gen.interval_ms),
            auto: p.gen.mode != Mode::Manual,
            p,
        })
    }

    fn val(&self, v: f64) -> Val {
        match self.p.init {
            Val::Bool(_) => Val::Bool(v != 0.0),
            Val::Int(_) => Val::Int(v as i32),
            _ => Val::Float(v),
        }
    }
}

async fn generate(inner: Arc<Inner>, points: Arc<Mutex<Vec<Live>>>) {
    // (tap, jatuh tempo, arah +1/-1)
    let mut taps: Vec<(&Ctl, Instant, i32)> =
        inner.ctls.iter().filter(|c| matches!(c.act, Act::Tap { .. })).map(|c| (&**c, Instant::now(), 1)).collect();
    let cbs: Vec<&Ctl> = inner.ctls.iter().filter(|c| matches!(c.act, Act::Cb { .. })).map(|c| &**c).collect();
    let start = Instant::now();
    let mut flush_due = start + REPORT_FLUSH;
    let mut rng = Rng::new(now_ms());
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        let now = Instant::now();
        if now >= flush_due {
            inner.ctx.flush_reports();
            flush_due = now + REPORT_FLUSH;
        }
        // dulu, supaya titik yang follow CB langsung melihat posisi akhir
        for c in &cbs {
            c.finish_travel(now);
        }
        for p in lock(&points).iter_mut() {
            if p.auto && now >= p.due {
                p.own = p.p.gen.next(p.own, (now - start).as_secs_f64(), &mut rng);
                p.due = now + Duration::from_millis(p.p.gen.interval_ms);
            }
            let gated = p.follow.as_ref().is_some_and(|f| inner.dbpos(f) != ffi::DBPOS_ON);
            let v = if gated { 0.0 } else { p.own };
            if v != p.cur {
                inner.write(&p.attr, p.val(v), Some(&p.t));
                p.cur = v;
            }
        }
        // Tap changer mode auto (ATCC.Auto = true): 1 langkah tiap interval, berbalik arah di batas
        for (ctl, due, dir) in &mut taps {
            let Act::Tap { pos, mirror, auto, min, max, interval } = &ctl.act else { continue };
            if now < *due {
                continue;
            }
            *due = now + *interval;
            if get_bool(inner.server, auto) {
                let p = get_int(inner.server, &pos.v);
                if !(*min..=*max).contains(&(p + *dir)) {
                    *dir = -*dir;
                }
                set_tap(inner.server, pos, mirror, p + *dir);
            }
        }
    }
}

/// Badai data: semua telemetry & telesignal (GGIO.Ind) berubah `rate` kali per detik selama `dur`.
/// Menulis langsung (lebih cepat dari tick generator); titik yang di-gate follow CB tetap 0.
async fn storm(inner: Arc<Inner>, points: Arc<Mutex<Vec<Live>>>, rate: u64, dur: Duration) {
    let end = Instant::now() + dur;
    let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / rate as f64));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut rng = Rng::new(now_ms() ^ 0x5eed);
    while Instant::now() < end {
        tick.tick().await;
        for p in lock(&points).iter_mut() {
            let v = match p.p.init {
                Val::Float(_) if p.p.gen.max > p.p.gen.min && p.p.gen.mode != Mode::Manual => {
                    crate::behavior::Gen { mode: Mode::Random, ..p.p.gen.clone() }.next(p.own, 0.0, &mut rng)
                }
                Val::Float(_) => p.own * (0.9 + 0.2 * rng.f64()), // tanpa batas: ±10%
                Val::Bool(_) if p.p.value_ref.starts_with("GGIO") => 1.0 - p.own,
                _ => continue,
            };
            p.own = v;
            if p.follow.as_ref().is_some_and(|f| inner.dbpos(f) != ffi::DBPOS_ON) {
                continue;
            }
            inner.write(&p.attr, p.val(v), Some(&p.t));
            p.cur = v;
        }
    }
    inner.ctx.event(Ev::Scenario, "selesai badai data (durasi habis)".into());
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::Gen;
    use crate::client::{control, Conn, CtlVal};
    use crate::model::{Cb, Component, CtlModel, Kind, Mx};

    fn comp(id: &str, kind: Kind) -> Component {
        Component { id: id.into(), kind }
    }

    fn cb(inst: u32, closed: bool, ctl: CtlModel) -> Cb {
        Cb { inst, closed, ctl, travel_ms: 0, ..Cb::default() }
    }

    fn get_f(ied: &Ied, r: &str) -> f32 {
        // SAFETY: attr valid, server hidup
        unsafe { ffi::IedServer_getFloatAttributeValue(ied.inner.server, ied.inner.attr(r).unwrap().0) }
    }

    fn get_t(ied: &Ied, r: &str) -> u64 {
        // SAFETY: attr valid, server hidup
        unsafe { ffi::IedServer_getUTCTimeAttributeValue(ied.inner.server, ied.inner.attr(r).unwrap().0) }
    }

    fn pos(ied: &Ied, r: &str) -> ffi::Dbpos {
        ied.inner.dbpos(&ied.inner.attr(r).unwrap())
    }

    fn int(ied: &Ied, r: &str) -> i32 {
        get_int(ied.inner.server, &ied.inner.attr(r).unwrap())
    }

    #[tokio::test]
    async fn model_generator_follow() {
        let fast = |mode| Gen { mode, interval_ms: 100, min: 10.0, max: 20.0, scale: 0.0, step: 1.0, period_s: 1.0 };
        let cfg = IedConfig {
            name: "TEST".into(),
            ld: "LD0".into(),
            port: 10102,
            components: vec![
                comp("cb1", Kind::CircuitBreaker(cb(1, true, CtlModel::StatusOnly))),
                comp("cb2", Kind::CircuitBreaker(cb(2, false, CtlModel::DirectNormal))),
                comp("ts", Kind::Telesignal { ggio: 2, ind: 5, value: true, gen: fast(Mode::Toggle) }),
                comp("mw", Kind::Telemetry { mx: Mx::preset("MW", 1, "").unwrap(), value: 0.0, gen: fast(Mode::Random), follow_cb: Some("cb1".into()) }),
                comp("ia", Kind::Telemetry { mx: Mx::preset("A", 1, "A").unwrap(), value: 15.0, gen: fast(Mode::Sine), follow_cb: Some("cb2".into()) }),
                comp("an", Kind::Telemetry { mx: Mx::preset("generic", 1, "3").unwrap(), value: 1.5, gen: Gen::manual(), follow_cb: None }),
            ],
            ..Default::default()
        };
        let ied = Ied::start(&cfg, Arc::new(Events::new())).expect("start");
        assert!(Ied::start(&cfg, Arc::new(Events::new())).is_err(), "port sama tidak boleh dipakai dua kali");

        assert_eq!(pos(&ied, "XCBR1.Pos.stVal"), ffi::DBPOS_ON);
        assert_eq!(get_f(&ied, "GGIO1.AnIn3.mag.f"), 1.5); // manual: tetap
        assert_eq!(int(&ied, "CSWI1.Pos.ctlModel"), 0);
        assert_eq!(int(&ied, "CSWI2.Pos.ctlModel"), 1);
        assert!(ied.inner.attr("CSWI2.Pos.Oper").is_ok());
        assert!(ied.inner.attr("CSWI1.Pos.Oper").is_err(), "status-only tanpa Oper");
        assert!(ied.inner.attr("GGIO2.Ind5.stVal").is_ok(), "nomor Ind dari komponen, bukan urutan");
        assert!(ied.inner.attr("CSWI2.Pos.sboTimeout").is_err(), "sboTimeout hanya untuk SBO");
        let t0 = get_t(&ied, "MMXU1.TotW.t");

        tokio::time::sleep(Duration::from_millis(450)).await;
        let mw = get_f(&ied, "MMXU1.TotW.mag.f");
        assert!((10.0..=20.0).contains(&mw), "random di luar batas: {mw}");
        assert!(get_t(&ied, "MMXU1.TotW.t") > t0, "timestamp harus ikut berubah");
        // CB2 open -> arus fasa A ikut 0
        assert_eq!(get_f(&ied, "MMXU1.A.phsA.cVal.mag.f"), 0.0);
    }

    /// Kontrol lewat MMS sungguhan (client libIEC61850), semua ctlModel + AddCause + travel + tap.
    #[tokio::test(flavor = "multi_thread")]
    async fn kontrol_cb_dan_tap() {
        let tap = |inst, auto, interval_ms| Kind::TapChanger { inst, pos: 0, min: -1, max: 1, ctl: CtlModel::DirectEnhanced, local: false, auto, interval_ms };
        let k = Kind::CircuitBreaker;
        let cfg = IedConfig {
            name: "CTL".into(),
            ld: "LD0".into(),
            port: 10103,
            components: vec![
                comp("cb1", k(cb(1, true, CtlModel::DirectNormal))),
                comp("cb2", k(Cb { travel_ms: 400, ..cb(2, false, CtlModel::SboEnhanced) })),
                comp("cb3", k(Cb { local: true, ..cb(3, true, CtlModel::DirectEnhanced) })),
                comp("cb4", k(Cb { interlock: true, ..cb(4, true, CtlModel::SboNormal) })),
                comp("cb5", k(Cb { travel_ms: 300, ..cb(5, false, CtlModel::SboNormal) })),
                comp("cb6", k(Cb { accept_same: true, travel_ms: 300, ..cb(9, true, CtlModel::DirectEnhanced) })),
                comp("cb7", k(Cb { sbo_timeout_ms: 1000, ..cb(7, true, CtlModel::SboNormal) })),
                comp("tap1", tap(1, false, 100_000)),
                comp("tap2", tap(2, true, 100)),
            ],
            ..Default::default()
        };
        let ied = Ied::start(&cfg, Arc::new(Events::new())).expect("start");
        let wait = Duration::from_secs(3);
        let ctl = move |r: &str, v, ilk| {
            let r = format!("CTLLD0/{r}");
            std::thread::spawn(move || control(&Conn::connect("127.0.0.1", 10103).unwrap(), &r, CtlVal::parse(v).unwrap(), ilk, wait))
        };

        // direct-normal tanpa tempuh
        ctl("CSWI1.Pos", "open", false).join().unwrap().expect("cb1 open");
        assert_eq!(pos(&ied, "XCBR1.Pos.stVal"), ffi::DBPOS_OFF);
        assert_eq!(pos(&ied, "CSWI1.Pos.stVal"), ffi::DBPOS_OFF);

        // sbo-enhanced dengan tempuh 400 ms: intermediate dulu, lalu CommandTermination+
        let h = ctl("CSWI2.Pos", "close", false);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(pos(&ied, "XCBR2.Pos.stVal"), ffi::DBPOS_INTERMEDIATE_STATE);
        let ok = h.join().unwrap().expect("cb2 close");
        assert!(ok.contains("CommandTermination+"), "{ok}");
        assert_eq!(pos(&ied, "XCBR2.Pos.stVal"), ffi::DBPOS_ON);
        let e = ctl("CSWI2.Pos", "close", false).join().unwrap().unwrap_err();
        assert!(e.contains("position-reached"), "{e}");

        // Local -> blocked-by-switching-hierarchy
        let e = ctl("CSWI3.Pos", "open", false).join().unwrap().unwrap_err();
        assert!(e.contains("blocked-by-switching-hierarchy"), "{e}");
        assert_eq!(pos(&ied, "XCBR3.Pos.stVal"), ffi::DBPOS_ON);

        // interlock: ditolak hanya kalau client minta interlock-check (SBO-normal: dicek saat operate;
        // security normal tidak mengirim LastApplError, jadi AddCause tidak terlihat di client)
        let e = ctl("CSWI4.Pos", "open", true).join().unwrap().unwrap_err();
        assert!(e.contains("operate ditolak"), "{e}");
        assert_eq!(pos(&ied, "XCBR4.Pos.stVal"), ffi::DBPOS_ON);
        ctl("CSWI4.Pos", "open", false).join().unwrap().expect("cb4 tanpa interlock-check");
        assert_eq!(pos(&ied, "XCBR4.Pos.stVal"), ffi::DBPOS_OFF);

        // security normal + tempuh: client langsung putus setelah operate, CB tetap harus sampai;
        // selama bergerak perintah lain ditolak
        ctl("CSWI5.Pos", "close", false).join().unwrap().expect("cb5 close");
        assert_eq!(pos(&ied, "XCBR5.Pos.stVal"), ffi::DBPOS_INTERMEDIATE_STATE);
        assert!(ctl("CSWI5.Pos", "open", false).join().unwrap().is_err(), "harus ditolak saat bergerak");
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(pos(&ied, "XCBR5.Pos.stVal"), ffi::DBPOS_ON);
        assert_eq!(pos(&ied, "CSWI5.Pos.stVal"), ffi::DBPOS_ON);
        ctl("CSWI5.Pos", "open", false).join().unwrap().expect("cb5 open setelah sampai");

        // opsi accept_same: close saat sudah close diterima, langsung selesai tanpa intermediate
        let ok = ctl("CSWI9.Pos", "close", false).join().unwrap().expect("accept_same");
        assert!(ok.contains("CommandTermination+"), "{ok}");
        assert_eq!(pos(&ied, "XCBR9.Pos.stVal"), ffi::DBPOS_ON);

        // sboTimeout (CF): default 30000, bisa diatur per CB, dan dipakai libIEC61850
        let u32_of = |r: &str| unsafe { ffi::IedServer_getUInt32AttributeValue(ied.inner.server, ied.inner.attr(r).unwrap().0) };
        assert_eq!(u32_of("CSWI2.Pos.sboTimeout"), 30_000);
        assert_eq!(u32_of("CSWI7.Pos.sboTimeout"), 1000);
        let late = std::thread::spawn(|| {
            let conn = Conn::connect("127.0.0.1", 10103).unwrap();
            let r = CString::new("CTLLD0/CSWI7.Pos").unwrap();
            // SAFETY: objek client dibuat & dihancurkan di sini
            unsafe {
                let c = ffi::ControlObjectClient_create(r.as_ptr(), conn.0);
                assert!(ffi::ControlObjectClient_select(c));
                std::thread::sleep(Duration::from_millis(1500));
                let v = ffi::MmsValue_newBoolean(false);
                let ok = ffi::ControlObjectClient_operate(c, v, 0);
                ffi::MmsValue_delete(v);
                ffi::ControlObjectClient_destroy(c);
                ok
            }
        });
        assert!(!late.join().unwrap(), "operate setelah sboTimeout harus ditolak");
        assert_eq!(pos(&ied, "XCBR7.Pos.stVal"), ffi::DBPOS_ON);

        // tap changer: raise sampai batas, lalu step-limit; YLTC.TapPos ikut
        ctl("ATCC1.TapChg", "raise", false).join().unwrap().expect("raise");
        assert_eq!(int(&ied, "ATCC1.TapChg.valWTr.posVal"), 1);
        assert_eq!(int(&ied, "YLTC1.TapPos.stVal"), 1);
        let e = ctl("ATCC1.TapChg", "raise", false).join().unwrap().unwrap_err();
        assert!(e.contains("step-limit"), "{e}");
        // auto on -> raise/lower ditolak blocked-by-mode
        ctl("ATCC1.Auto", "true", false).join().unwrap().expect("auto on");
        let e = ctl("ATCC1.TapChg", "lower", false).join().unwrap().unwrap_err();
        assert!(e.contains("blocked-by-mode"), "{e}");

        // tap2 auto tiap 100 ms: 1 langkah, berbalik arah di batas -> mencapai kedua batas
        let mut seen = std::collections::HashSet::new();
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let p = int(&ied, "YLTC2.TapPos.stVal");
            assert!((-1..=1).contains(&p), "tap auto keluar batas: {p}");
            seen.insert(p);
        }
        assert!(seen.contains(&-1) && seen.contains(&1), "tap auto harus bolak-balik: {seen:?}");
    }

    /// Fase 5: banyak IED di belakang proxy, jumlah client, RCB aktif, dan log kejadian.
    #[tokio::test(flavor = "multi_thread")]
    async fn banyak_ied_client_dan_log() {
        let events = Arc::new(Events::new());
        let fast = Gen { mode: Mode::Random, interval_ms: 100, min: 0.0, max: 100.0, scale: 0.0, step: 1.0, period_s: 1.0 };
        let ied = |name: &str, port| IedConfig {
            name: name.into(),
            ld: "LD0".into(),
            port,
            components: vec![
                comp("cb1", Kind::CircuitBreaker(cb(1, true, CtlModel::SboEnhanced))),
                comp("mw", Kind::Telemetry { mx: Mx::preset("MW", 1, "").unwrap(), value: 0.0, gen: fast.clone(), follow_cb: None }),
            ],
            ..Default::default()
        };
        let ieds: Vec<Ied> =
            [("MA", 10111), ("MB", 10112), ("MC", 10113)].iter().map(|(n, p)| Ied::start(&ied(n, *p), Arc::clone(&events)).unwrap()).collect();
        let (a, b, c) = (&ieds[0], &ieds[1], &ieds[2]);
        let conn = |port| std::thread::spawn(move || Conn::connect("127.0.0.1", port).unwrap()).join().unwrap();
        let (a1, a2, b1) = (conn(10111), conn(10111), conn(10112));
        tokio::time::sleep(Duration::from_millis(300)).await;

        // jumlah client: dihitung proxy, dicocokkan dengan koneksi yang dilihat libIEC61850
        for (ied, n) in [(a, 2), (b, 1), (c, 0)] {
            let i = ied.info();
            assert_eq!(i["client_count"], n, "{}", ied.name);
            assert_eq!(i["ied_connections"], n, "{}", ied.name);
        }
        assert_eq!(a.info()["mode"], "polling");
        assert_eq!(c.info()["mode"], "idle");

        // RCB enable oleh client a1 -> mode report, client tercatat dengan alamat asli
        let addrs: Vec<String> = a.info()["clients"].as_array().unwrap().iter().map(|c| c["addr"].as_str().unwrap().to_string()).collect();
        let rcb = CString::new("MALD0/LLN0.BR.brcbST01").unwrap();
        let a1_raw = a1.0 as usize;
        std::thread::spawn(move || {
            let con = a1_raw as ffi::IedConnection;
            let mut err = 0;
            // SAFETY: koneksi a1 hidup sampai akhir test; objek RCB dihancurkan di sini
            unsafe {
                let r = ffi::IedConnection_getRCBValues(con, &mut err, rcb.as_ptr(), std::ptr::null_mut());
                assert_eq!(err, ffi::IED_ERROR_OK);
                ffi::ClientReportControlBlock_setRptEna(r, true);
                ffi::IedConnection_setRCBValues(con, &mut err, r, ffi::RCB_ELEMENT_RPT_ENA, true);
                assert_eq!(err, ffi::IED_ERROR_OK);
                ffi::ClientReportControlBlock_destroy(r);
            }
        })
        .join()
        .unwrap();
        let i = a.info();
        assert_eq!(i["mode"], "report");
        assert_eq!(i["rcbs"][0]["rcb"], "brcbST01");
        assert!(addrs.contains(&i["rcbs"][0]["client"].as_str().unwrap().to_string()), "RCB harus tercatat dengan alamat client asli: {i}");

        // kontrol lewat koneksi a2 -> log diterima & selesai, alamat = salah satu client asli
        let r = std::thread::spawn(move || control(&a2, "MALD0/CSWI1.Pos", CtlVal::Bool(false), false, Duration::from_secs(3))).join().unwrap();
        assert!(r.is_ok(), "{r:?}");
        let ctl: Vec<String> = events.query(Some("MA"), Some(Ev::Control), 10).into_iter().map(|e| e.msg).collect();
        assert!(ctl.iter().any(|m| m.contains("CSWI1.Pos select open diterima")), "{ctl:?}");
        let done = ctl.iter().find(|m| m.contains("operate open selesai, CommandTermination+")).expect("operate selesai tercatat");
        assert!(addrs.iter().any(|a| done.contains(&format!("dari {a})"))), "alamat client asli: {done} vs {addrs:?}");

        // report dikelompokkan per detik (CB berubah + MW random tiap 100 ms)
        tokio::time::sleep(Duration::from_millis(1300)).await;
        let rep = events.query(Some("MA"), Some(Ev::Report), 5);
        assert!(rep.iter().any(|e| e.msg.contains("brcbST01×")), "{rep:?}");
        assert!(rep.iter().all(|e| !e.msg.contains("brcbMX01")), "RCB yang tidak di-enable tidak boleh terhitung: {rep:?}");

        // client putus -> jumlah turun, RCB ikut disable, tercatat
        drop((a1, b1));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(a.info()["client_count"], 0);
        assert_eq!(b.info()["client_count"], 0);
        assert_eq!(a.info()["mode"], "idle");
        let con: Vec<String> = events.query(Some("MA"), Some(Ev::Connection), 10).into_iter().map(|e| e.msg).collect();
        assert!(con.iter().filter(|m| m.starts_with("client konek")).count() >= 2 && con.iter().any(|m| m.starts_with("client putus")), "{con:?}");
        assert!(events.query(Some("MA"), Some(Ev::Rcb), 10).iter().any(|e| e.msg.contains("brcbST01 di-disable")));
    }

    /// Fase 6: ubah manual dari UI, mode auto, aturan Local/Remote, urutan kartu.
    #[tokio::test(flavor = "multi_thread")]
    async fn ubah_manual_dari_ui() {
        let events = Arc::new(Events::new());
        let rnd = Gen { mode: Mode::Random, interval_ms: 100, min: 10.0, max: 20.0, scale: 0.0, step: 1.0, period_s: 1.0 };
        let cfg = IedConfig {
            name: "UI".into(),
            ld: "LD0".into(),
            port: 10121,
            components: vec![
                comp("cb1", Kind::CircuitBreaker(cb(1, false, CtlModel::SboEnhanced))),
                comp("tap1", Kind::TapChanger { inst: 1, pos: 1, min: 1, max: 2, ctl: CtlModel::DirectEnhanced, local: false, auto: false, interval_ms: 1000 }),
                comp("ts1", Kind::Telesignal { ggio: 1, ind: 1, value: false, gen: Gen::manual() }),
                comp("mw", Kind::Telemetry { mx: Mx::preset("MW", 1, "").unwrap(), value: 15.0, gen: rnd, follow_cb: None }),
            ],
            ..Default::default()
        };
        let ied = Ied::start(&cfg, Arc::clone(&events)).unwrap();
        let live = |id: &str| ied.live_values().into_iter().find(|v| v["id"] == id).unwrap();
        let set = |id: &str, cmd: Value| ied.set(id, &cmd);

        // CB: operasi dari UI hanya saat Local
        assert!(set("cb1", json!({ "action": "close" })).unwrap_err().contains("Remote"));
        set("cb1", json!({ "local": true })).unwrap();
        set("cb1", json!({ "action": "close" })).unwrap();
        assert_eq!(live("cb1")["pos"], "close");
        assert_eq!(live("cb1")["local"], true);
        assert_eq!(live("cb1")["q"]["text"], "good");

        // tap: Local + batas
        set("tap1", json!({ "local": true })).unwrap();
        set("tap1", json!({ "action": "raise" })).unwrap();
        assert_eq!(live("tap1")["pos"], 2);
        assert!(set("tap1", json!({ "action": "raise" })).unwrap_err().contains("batas"));

        // telesignal manual, telemetry auto -> harus dimatikan dulu
        set("ts1", json!({ "value": true })).unwrap();
        assert!(set("ts1", json!({ "auto": true })).unwrap_err().contains("generator"));
        assert_eq!(live("mw")["auto"], true);
        assert!(set("mw", json!({ "value": 42.5 })).unwrap_err().contains("auto"));
        set("mw", json!({ "auto": false })).unwrap();
        set("mw", json!({ "value": 42.5 })).unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(live("ts1")["value"], true);
        assert_eq!(live("mw")["value"], 42.5);
        assert!(live("mw")["t"].as_u64().unwrap() > 0);

        // urutan kartu
        assert!(ied.set_order(vec!["mw".into(), "cb1".into()]).is_err());
        ied.set_order(vec!["mw".into(), "ts1".into(), "tap1".into(), "cb1".into()]).unwrap();
        let c = ied.components_json();
        assert_eq!(c["components"][0]["id"], "mw");
        assert_eq!(c["components"][0]["ref"], "MMXU1.TotW");
        assert_eq!(c["components"][0]["unit"], "MW");
        assert_eq!(c["components"][3]["ctl_model"], "sbo-enhanced");
        assert_eq!(c["components"][3]["pos"], "close");

        let manual = events.query(Some("UI"), Some(Ev::Manual), 20);
        assert_eq!(manual.len(), 7, "{manual:?}"); // hanya 7 perubahan yang berhasil
    }

    /// Fase 7: skenario trip, quality, hang, delay, putus, mati/hidup, badai.
    #[tokio::test(flavor = "multi_thread")]
    async fn skenario() {
        use std::io::{Read, Write};
        let events = Arc::new(Events::new());
        let rnd = Gen { mode: Mode::Random, interval_ms: 100, min: 100.0, max: 200.0, scale: 0.0, step: 1.0, period_s: 1.0 };
        let cfg = IedConfig {
            name: "SC".into(),
            ld: "LD0".into(),
            port: 10131,
            components: vec![
                comp("cb1", Kind::CircuitBreaker(Cb { travel_ms: 200, ..cb(1, true, CtlModel::SboEnhanced) })),
                comp("prot1", Kind::Protection { class: model::ProtClass::Ptoc, inst: 1, cb: "cb1".into() }),
                comp("ia", Kind::Telemetry { mx: Mx::preset("A", 1, "A").unwrap(), value: 150.0, gen: rnd, follow_cb: Some("cb1".into()) }),
                comp("an", Kind::Telemetry { mx: Mx::preset("generic", 1, "1").unwrap(), value: 50.0, gen: Gen::manual(), follow_cb: None }),
            ],
            ..Default::default()
        };
        let ied = Ied::start(&cfg, Arc::clone(&events)).unwrap();
        let live = |id: &str| ied.live_values().into_iter().find(|v| v["id"] == id).unwrap();
        let sleep = |ms| tokio::time::sleep(Duration::from_millis(ms));
        assert_eq!(ied.resolve("XCBR1").as_deref(), Some("cb1"));
        assert_eq!(ied.resolve("MMXU1.A.phsA").as_deref(), Some("ia"));
        assert_eq!(ied.resolve("PTOC1").as_deref(), Some("prot1"));

        // trip: Str/Op naik, CB intermediate lalu open, arus ikut 0, Op turun setelah pulse
        ied.run_scenario("trip", &json!({ "pulse_ms": 300 })).unwrap();
        sleep(100).await;
        assert_eq!(live("prot1")["op"], true);
        assert_eq!(live("prot1")["str"], true);
        assert_eq!(live("cb1")["pos"], "intermediate");
        sleep(250).await;
        assert_eq!(live("cb1")["pos"], "open");
        assert_eq!(live("ia")["value"], 0.0);
        sleep(150).await;
        assert_eq!(live("prot1")["op"], false);
        assert!(ied.run_scenario("trip", &json!({ "prot": "x" })).is_err());
        // CB sudah open: hanya pulsa Op, Str tidak naik, Pos tetap
        let m = ied.run_scenario("trip", &json!({})).unwrap();
        assert!(m.contains("hanya pulsa Op"), "{m}");
        sleep(100).await;
        assert_eq!(live("prot1")["op"], true);
        assert_eq!(live("prot1")["str"], false);
        assert_eq!(live("cb1")["pos"], "open");
        sleep(200).await;
        assert_eq!(live("prot1")["op"], false);

        // quality per komponen + normalkan
        ied.run_scenario("quality", &json!({ "validity": "invalid", "flags": ["oldData"], "components": ["an"] })).unwrap();
        assert_eq!(live("an")["q"]["text"], "invalid+oldData");
        assert_eq!(live("cb1")["q"]["text"], "good");
        assert_eq!(ied.info()["scenarios"]["quality"], "invalid+oldData");
        assert!(ied.run_scenario("quality", &json!({ "flags": ["ngawur"] })).is_err());
        let before = live("an")["value"].clone();
        ied.stop_scenario("quality").unwrap();
        assert_eq!(live("an")["q"]["text"], "good");
        assert_eq!(live("an")["value"], before, "normalkan hanya mengubah quality");

        // hang: TCP tetap terbuka, data ditahan, lalu diteruskan setelah lanjut
        // COTP Connection Request (TPKT 22 byte); IED menjawab Connection Confirm
        const CR: [u8; 22] = [3, 0, 0, 22, 17, 0xe0, 0, 0, 0, 1, 0, 0xc0, 1, 0x0a, 0xc2, 2, 0, 1, 0xc1, 2, 0, 1];
        ied.run_scenario("hang", &json!({})).unwrap();
        assert_eq!(ied.info()["status"], "hang");
        let mut raw = std::thread::spawn(|| std::net::TcpStream::connect("127.0.0.1:10131").unwrap()).join().unwrap();
        raw.set_read_timeout(Some(Duration::from_millis(400))).unwrap();
        raw.write_all(&CR).unwrap();
        let mut buf = [0u8; 64];
        let mut r2 = raw.try_clone().unwrap();
        assert!(std::thread::spawn(move || r2.read(&mut buf)).join().unwrap().is_err(), "selama hang tidak boleh ada jawaban");
        ied.stop_scenario("hang").unwrap();
        let n = std::thread::spawn(move || raw.read(&mut buf).map(|n| (n, buf[5]))).join().unwrap().unwrap();
        assert_eq!(n.1, 0xd0, "setelah lanjut, Connection Confirm harus sampai (n={})", n.0);

        // delay: asosiasi MMS (beberapa bolak-balik) jadi jauh lebih lama
        let t = Instant::now();
        drop(std::thread::spawn(|| Conn::connect("127.0.0.1", 10131).unwrap()).join().unwrap());
        let normal = t.elapsed();
        ied.run_scenario("delay", &json!({ "ms": 300 })).unwrap();
        let t = Instant::now();
        let c = std::thread::spawn(|| Conn::connect("127.0.0.1", 10131).unwrap()).join().unwrap();
        assert!(t.elapsed() > normal + Duration::from_millis(500), "delay tidak terasa: {:?} vs {normal:?}", t.elapsed());
        ied.stop_scenario("delay").unwrap();

        // putus koneksi: client ditutup, IED tetap jalan
        sleep(200).await;
        assert!(ied.info()["client_count"].as_u64().unwrap() >= 1);
        ied.run_scenario("disconnect", &json!({})).unwrap();
        sleep(300).await;
        assert_eq!(ied.info()["client_count"], 0);
        drop(c);
        // mode rst: client menerima RST (connection reset), bukan FIN (read = 0)
        let mut raw = std::thread::spawn(|| std::net::TcpStream::connect("127.0.0.1:10131").unwrap()).join().unwrap();
        sleep(200).await;
        assert!(ied.run_scenario("disconnect", &json!({ "mode": "ngawur" })).is_err());
        ied.run_scenario("disconnect", &json!({ "mode": "rst" })).unwrap();
        sleep(200).await;
        let mut b = [0u8; 8];
        let e = std::thread::spawn(move || raw.read(&mut b)).join().unwrap();
        assert_eq!(e.map_err(|e| e.kind()), Err(std::io::ErrorKind::ConnectionReset));

        // clock: bit ClockNotSynchronized (0x20) / ClockFailure (0x40) di timestamp yang ditulis sesudahnya
        let tq = || {
            let a = ied.inner.attr("GGIO1.AnIn1.t").unwrap();
            // SAFETY: attr valid, bertipe UtcTime
            unsafe { ffi::MmsValue_getUtcTimeQuality(ffi::IedServer_getAttributeValue(ied.inner.server, a.0)) }
        };
        ied.run_scenario("clock", &json!({})).unwrap();
        ied.set("an", &json!({ "value": 7.0 })).unwrap();
        sleep(250).await;
        assert_eq!(tq() & 0x60, 0x20, "ClockNotSynchronized");
        ied.run_scenario("clock", &json!({ "failure": true, "not_sync": false })).unwrap();
        ied.set("an", &json!({ "value": 8.0 })).unwrap();
        sleep(250).await;
        assert_eq!(tq() & 0x60, 0x40, "ClockFailure");
        ied.stop_scenario("clock").unwrap();
        ied.set("an", &json!({ "value": 50.0 })).unwrap();
        sleep(250).await;
        assert_eq!(tq() & 0x60, 0, "normal");

        // badai: titik manual pun berubah, lalu berhenti sendiri
        assert!(ied.run_scenario("storm", &json!({ "rate": 5000 })).is_err());
        ied.run_scenario("storm", &json!({ "rate": 50, "duration": 1 })).unwrap();
        assert!(ied.run_scenario("storm", &json!({})).is_err(), "badai kedua saat masih jalan");
        sleep(400).await;
        assert_ne!(live("an")["value"], 50.0);
        assert_eq!(ied.info()["scenarios"]["storm"], true);
        sleep(1000).await;
        assert_eq!(ied.info()["scenarios"]["storm"], false);
        let sc: Vec<String> = events.query(Some("SC"), Some(Ev::Scenario), 50).into_iter().map(|e| e.msg).collect();
        assert!(sc.iter().any(|m| m.contains("badai data (durasi habis)")), "{sc:?}");
    }
}
