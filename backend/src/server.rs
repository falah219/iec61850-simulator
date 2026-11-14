//! Satu IED = satu IedServer libIEC61850 dengan model dinamis + task generator nilai.

use std::ffi::CString;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::task::JoinHandle;

use crate::behavior::{Mode, Rng};
use crate::ffi;
use crate::model::{self, IedConfig, Point, Val};

const VENDOR: &str = "iec61850-simulator";
/// Resolusi generator. ponytail: interval titik dibulatkan ke 100 ms; kecilkan kalau butuh lebih halus
const TICK: Duration = Duration::from_millis(100);

struct Inner {
    name: String,
    ld: String,
    model: *mut ffi::IedModel,
    server: ffi::IedServer,
}

// SAFETY: IedServer/IedModel hanya diakses lewat API libIEC61850; update nilai dilindungi
// IedServer_lockDataModel. Pointer dimiliki Inner dan dihancurkan sekali di Drop.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

impl Inner {
    /// DataAttribute dari reference relatif ke LD, mis. `XCBR1.Pos.stVal`.
    fn attr(&self, r: &str) -> Result<Attr, String> {
        let full = CString::new(format!("{}{}/{r}", self.name, self.ld)).map_err(|e| e.to_string())?;
        // SAFETY: model valid selama self hidup
        let n = unsafe { ffi::IedModel_getModelNodeByObjectReference(self.model, full.as_ptr()) };
        if n.is_null() {
            return Err(format!("reference {r} tidak ada di model"));
        }
        Ok(Attr(n.cast()))
    }

    /// Tulis nilai (+ timestamp kalau ada). Report dchg dikirim libIEC61850 otomatis.
    fn write(&self, a: &Attr, v: Val, t: Option<&Attr>) {
        // SAFETY: server & attr valid; tipe Val sesuai atribut yang dibuat di model::build
        unsafe {
            ffi::IedServer_lockDataModel(self.server);
            match v {
                Val::Bool(b) => ffi::IedServer_updateBooleanAttributeValue(self.server, a.0, b),
                Val::Int(i) => ffi::IedServer_updateInt32AttributeValue(self.server, a.0, i),
                Val::Float(f) => ffi::IedServer_updateFloatAttributeValue(self.server, a.0, f as f32),
                Val::Dbpos(d) => ffi::IedServer_updateDbposValue(self.server, a.0, d),
            }
            if let Some(t) = t {
                ffi::IedServer_updateUTCTimeAttributeValue(self.server, t.0, now_ms());
            }
            ffi::IedServer_unlockDataModel(self.server);
        }
    }

    fn dbpos(&self, a: &Attr) -> ffi::Dbpos {
        // SAFETY: server & attr valid, attr bertipe Dbpos
        unsafe { ffi::Dbpos_fromMmsValue(ffi::IedServer_getAttributeValue(self.server, a.0)) }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // SAFETY: urutan wajib: stop -> destroy server -> destroy model
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
// SAFETY: hanya dipakai bersama Arc<Inner> yang menjaga model tetap hidup
unsafe impl Send for Attr {}

pub struct Ied {
    pub name: String,
    pub port: u16,
    #[allow(dead_code)] // dipegang supaya server hidup; dibaca lewat API set nilai mulai fase 4
    inner: Arc<Inner>,
    task: JoinHandle<()>,
}

impl Ied {
    /// Validasi, bangun model, isi nilai awal, listen MMS di `0.0.0.0:port`, lalu jalankan generator.
    /// Harus dipanggil di dalam runtime tokio.
    pub fn start(cfg: &IedConfig) -> Result<Self, String> {
        model::validate(cfg)?;
        let built = model::build(cfg);
        // SAFETY: model valid dari build(); server memegang referensi model sampai destroy.
        let server = unsafe { ffi::IedServer_create(built.model) };
        let inner = Arc::new(Inner { name: cfg.name.clone(), ld: cfg.ld.clone(), model: built.model, server });

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
    if points.is_empty() {
        return;
    }
    let start = Instant::now();
    let mut rng = Rng::new(now_ms());
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        let now = Instant::now();
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
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::Gen;
    use crate::model::{Component, Kind, Mx};

    fn comp(id: &str, kind: Kind) -> Component {
        Component { id: id.into(), kind }
    }

    fn get_f(ied: &Ied, r: &str) -> f32 {
        // SAFETY: attr valid, server hidup
        unsafe { ffi::IedServer_getFloatAttributeValue(ied.inner.server, ied.inner.attr(r).unwrap().0) }
    }

    fn get_t(ied: &Ied, r: &str) -> u64 {
        // SAFETY: attr valid, server hidup
        unsafe { ffi::IedServer_getUTCTimeAttributeValue(ied.inner.server, ied.inner.attr(r).unwrap().0) }
    }

    #[tokio::test]
    async fn model_generator_follow() {
        let fast = |mode| Gen { mode, interval_ms: 100, min: 10.0, max: 20.0, scale: 0.0, step: 1.0, period_s: 1.0 };
        let cfg = IedConfig {
            name: "TEST".into(),
            ld: "LD0".into(),
            port: 10102,
            components: vec![
                comp("cb1", Kind::CircuitBreaker { closed: true }),
                comp("cb2", Kind::CircuitBreaker { closed: false }),
                comp("ts", Kind::Telesignal { ggio: 2, value: true, gen: fast(Mode::Toggle) }),
                comp("mw", Kind::Telemetry { mx: Mx::preset("MW", 1, "").unwrap(), value: 0.0, gen: fast(Mode::Random), follow_cb: Some("cb1".into()) }),
                comp("ia", Kind::Telemetry { mx: Mx::preset("A", 1, "A").unwrap(), value: 15.0, gen: fast(Mode::Sine), follow_cb: Some("cb2".into()) }),
                comp("an", Kind::Telemetry { mx: Mx::preset("generic", 1, "3").unwrap(), value: 1.5, gen: Gen::manual(), follow_cb: None }),
            ],
        };
        let ied = Ied::start(&cfg).expect("start");
        assert!(Ied::start(&cfg).is_err(), "port sama tidak boleh dipakai dua kali");

        // SAFETY: attr valid, server hidup
        let pos = unsafe { ffi::Dbpos_fromMmsValue(ffi::IedServer_getAttributeValue(ied.inner.server, ied.inner.attr("XCBR1.Pos.stVal").unwrap().0)) };
        assert_eq!(pos, ffi::DBPOS_ON);
        assert_eq!(get_f(&ied, "GGIO1.AnIn3.mag.f"), 1.5); // manual: tetap
        assert!(ied.inner.attr("CSWI2.Pos.ctlModel").is_ok());
        assert!(ied.inner.attr("GGIO2.Ind1.stVal").is_ok());
        let t0 = get_t(&ied, "MMXU1.TotW.t");

        tokio::time::sleep(Duration::from_millis(450)).await;
        let mw = get_f(&ied, "MMXU1.TotW.mag.f");
        assert!((10.0..=20.0).contains(&mw), "random di luar batas: {mw}");
        assert!(get_t(&ied, "MMXU1.TotW.t") > t0, "timestamp harus ikut berubah");
        // CB2 open -> arus fasa A ikut 0
        assert_eq!(get_f(&ied, "MMXU1.A.phsA.cVal.mag.f"), 0.0);
    }
}
