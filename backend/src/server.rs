//! Satu IED = satu IedServer libIEC61850 dengan model dinamis + handler kontrol + task generator nilai.

use std::ffi::{c_void, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::behavior::{Mode, Rng};
use crate::client::add_cause_name;
use crate::ffi;
use crate::model::{self, CtlAct, CtlSpec, IedConfig, Point, Val};

const VENDOR: &str = "iec61850-simulator";
/// Resolusi generator. ponytail: interval titik dibulatkan ke 100 ms; kecilkan kalau butuh lebih halus
const TICK: Duration = Duration::from_millis(100);

struct Inner {
    name: String,
    ld: String,
    model: *mut ffi::IedModel,
    server: ffi::IedServer,
    /// Parameter handler kontrol (alamat Box tetap). Di-drop SETELAH server dihancurkan (lihat Drop).
    ctls: Vec<Box<Ctl>>,
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
            name: format!("{}{}/{}", self.name, self.ld, s.do_ref),
            server: self.server,
            loc: self.attr(&format!("{}.stVal", s.loc_ref))?,
            interlock: s.interlock,
            act,
        });
        if s.controllable {
            let d: *mut ffi::DataObject = self.node(&s.do_ref)?.cast();
            let p = &*ctl as *const Ctl as *mut c_void;
            // SAFETY: server & DO valid; p menunjuk Box yang hidup sampai server dihancurkan
            unsafe {
                ffi::IedServer_setPerformCheckHandler(self.server, d, Some(on_check), p);
                ffi::IedServer_setControlHandler(self.server, d, Some(on_operate), p);
            }
        }
        self.ctls.push(ctl);
        Ok(())
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // SAFETY: urutan wajib: stop -> destroy server -> destroy model. self.ctls di-drop sesudah ini,
        // jadi tidak ada handler yang masih bisa dipanggil saat Box-nya dibebaskan.
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

/// Objek kontrol: parameter handler check/operate libIEC61850.
struct Ctl {
    /// reference lengkap DO, untuk log
    name: String,
    server: ffi::IedServer,
    loc: Attr,
    interlock: bool,
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

/// orIdent + orCat pengirim perintah, untuk log
fn origin(action: ffi::ControlAction) -> String {
    let mut n = 0;
    // SAFETY: action valid selama handler; buffer orIdent milik library, dibaca sebelum return
    unsafe {
        let p = ffi::ControlAction_getOrIdent(action, &mut n);
        let ident = if p.is_null() { "-".into() } else { String::from_utf8_lossy(std::slice::from_raw_parts(p, n as usize)).into_owned() };
        format!("orIdent={ident} orCat={}", ffi::ControlAction_getOrCat(action))
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
    catch_unwind(AssertUnwindSafe(|| match ctl.check(val, interlock_check) {
        Ok(()) => ffi::CONTROL_ACCEPTED,
        Err((res, cause)) => {
            // SAFETY: action valid selama handler
            unsafe { ffi::ControlAction_setAddCause(action, cause) };
            warn!("kontrol {} {} ditolak: {} ({})", ctl.name, ctl.describe(val), add_cause_name(cause), origin(action));
            res
        }
    }))
    .unwrap_or(ffi::CONTROL_HARDWARE_FAULT)
}

unsafe extern "C" fn on_operate(action: ffi::ControlAction, p: *mut c_void, val: *mut ffi::MmsValue, test: bool) -> ffi::ControlHandlerResult {
    // SAFETY: p = Box<Ctl> dari add_ctl, hidup sampai server dihancurkan
    let ctl = unsafe { &*(p as *const Ctl) };
    catch_unwind(AssertUnwindSafe(|| {
        let r = ctl.operate(val, test);
        if r == ffi::CONTROL_RESULT_OK {
            info!("kontrol {} {} selesai{} ({})", ctl.name, ctl.describe(val), if test { " [test]" } else { "" }, origin(action));
        }
        r
    }))
    .unwrap_or(ffi::CONTROL_RESULT_FAILED)
}

pub struct Ied {
    pub name: String,
    pub port: u16,
    #[allow(dead_code)] // dipegang supaya server hidup; dibaca lewat API set nilai mulai fase 5
    inner: Arc<Inner>,
    task: JoinHandle<()>,
}

impl Ied {
    /// Validasi, bangun model, isi nilai awal, pasang handler kontrol, listen MMS di `0.0.0.0:port`,
    /// lalu jalankan generator. Harus dipanggil di dalam runtime tokio.
    pub fn start(cfg: &IedConfig) -> Result<Self, String> {
        model::validate(cfg)?;
        let built = model::build(cfg);
        // SAFETY: model valid dari build(); server memegang referensi model sampai destroy.
        let server = unsafe { ffi::IedServer_create(built.model) };
        let mut inner = Inner { name: cfg.name.clone(), ld: cfg.ld.clone(), model: built.model, server, ctls: Vec::new() };

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
            if rt.p.gen.mode != Mode::Manual || rt.follow.is_some() {
                live.push(rt);
            }
        }
        for s in &built.ctls {
            inner.add_ctl(s)?;
        }
        let inner = Arc::new(inner);

        // IedServer_isRunning tetap true walau bind gagal, jadi cek port dulu.
        // ponytail: ada jeda kecil antara cek dan start; hilang di fase 5 (proxy memegang port publik)
        std::net::TcpListener::bind(("0.0.0.0", cfg.port))
            .map_err(|e| format!("IED {}: port MMS {} tidak bisa dipakai: {e}", cfg.name, cfg.port))?;
        // SAFETY: server valid
        unsafe { ffi::IedServer_start(server, cfg.port.into()) };

        let task = tokio::spawn(generate(Arc::clone(&inner), live));
        Ok(Self { name: cfg.name.clone(), port: cfg.port, inner, task })
    }
}

impl Drop for Ied {
    fn drop(&mut self) {
        // Inner (server) dihancurkan setelah task generator ikut selesai (Arc terakhir)
        self.task.abort();
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

async fn generate(inner: Arc<Inner>, mut points: Vec<Live>) {
    // (tap, jatuh tempo, arah +1/-1)
    let mut taps: Vec<(&Ctl, Instant, i32)> =
        inner.ctls.iter().filter(|c| matches!(c.act, Act::Tap { .. })).map(|c| (&**c, Instant::now(), 1)).collect();
    let cbs: Vec<&Ctl> = inner.ctls.iter().filter(|c| matches!(c.act, Act::Cb { .. })).map(|c| &**c).collect();
    if points.is_empty() && taps.is_empty() && cbs.is_empty() {
        return;
    }
    let start = Instant::now();
    let mut rng = Rng::new(now_ms());
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        let now = Instant::now();
        // dulu, supaya titik yang follow CB langsung melihat posisi akhir
        for c in &cbs {
            c.finish_travel(now);
        }
        for p in &mut points {
            if p.p.gen.mode != Mode::Manual && now >= p.due {
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

fn now_ms() -> u64 {
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
        };
        let ied = Ied::start(&cfg).expect("start");
        assert!(Ied::start(&cfg).is_err(), "port sama tidak boleh dipakai dua kali");

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
        };
        let ied = Ied::start(&cfg).expect("start");
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
}
