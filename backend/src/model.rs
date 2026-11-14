//! Konfigurasi IED + validasi + bangun IedModel libIEC61850 secara dinamis (PLAN bagian 4).
//!
//! Satu LD (default `LD0`) berisi `LLN0` + `LPHD1`, lalu per komponen:
//! - Circuit Breaker      : `XCBR<n>` (Pos DPS, Loc SPS) + `CSWI<n>` (Pos DPC dengan ctlModel [+ sboTimeout], Loc SPS)
//! - Tap Changer          : `YLTC<n>.TapPos` (INS) + `ATCC<n>` (TapChg BSC raise/lower, Auto SPC, Loc SPS)
//! - Telesignal           : `GGIO<inst>.Ind<k>` (SPS)
//! - Telemetry            : LN/DO pilihan (preset atau custom), lihat [`Mx`]
//!
//! Semua nomor instance / Ind / AnIn disimpan di komponen (dikunci), tidak dihitung dari urutan.
//! ponytail: "tidak dipakai ulang setelah dihapus" butuh penghitung per IED; dibuat bersama tambah komponen (fase 8)
//!
//! Dataset `LLN0.DS_ST` (semua titik ST) & `LLN0.DS_MX` (semua titik MX), masing-masing dengan
//! BRCB `brcb<ST|MX>01..02` dan URCB `urcb<ST|MX>01..02`.

use std::collections::HashSet;
use std::ffi::CString;
use std::ptr;

use crate::behavior::{Gen, Mode};
use crate::ffi;

pub struct IedConfig {
    pub name: String,
    pub ld: String,
    pub port: u16,
    pub components: Vec<Component>,
}

pub struct Component {
    pub id: String,
    pub kind: Kind,
}

/// ctlModel IEC 61850-7-3 (nilainya sama dengan enum di standar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtlModel {
    StatusOnly = 0,
    DirectNormal = 1,
    SboNormal = 2,
    DirectEnhanced = 3,
    SboEnhanced = 4,
}

impl CtlModel {
    /// controlOptions untuk `CDC_*_create`: struktur SBO/SBOw/Oper/Cancel + origin/ctlNum/stSeld.
    fn options(self) -> u32 {
        let m = self as u32;
        match self {
            Self::StatusOnly => m,
            Self::DirectNormal | Self::DirectEnhanced => m | ffi::CDC_CTL_OPTION_ORIGIN | ffi::CDC_CTL_OPTION_CTL_NUM,
            Self::SboNormal | Self::SboEnhanced => {
                m | ffi::CDC_CTL_OPTION_ORIGIN
                    | ffi::CDC_CTL_OPTION_CTL_NUM
                    | ffi::CDC_CTL_OPTION_ST_SELD
                    | ffi::CDC_CTL_MODEL_HAS_CANCEL
            }
        }
    }

    fn is_sbo(self) -> bool {
        matches!(self, Self::SboNormal | Self::SboEnhanced)
    }

    fn is_enhanced(self) -> bool {
        matches!(self, Self::DirectEnhanced | Self::SboEnhanced)
    }
}

/// Circuit Breaker: `XCBR<inst>` + `CSWI<inst>`.
#[derive(Debug, Clone)]
pub struct Cb {
    /// Nomor instance LN, dikunci per komponen (tidak bergeser saat komponen lain dihapus)
    pub inst: u32,
    pub closed: bool,
    pub ctl: CtlModel,
    /// Lama di posisi intermediate saat operate
    pub travel_ms: u64,
    /// Loc = true: perintah ditolak (blocked-by-switching-hierarchy)
    pub local: bool,
    /// Tolak perintah yang minta interlock-check (blocked-by-interlocking)
    pub interlock: bool,
    /// true = perintah ke posisi yang sudah tercapai tetap diterima; false = tolak (position-reached)
    pub accept_same: bool,
    /// `CSWI.Pos.sboTimeout` (CF), hanya untuk model SBO
    pub sbo_timeout_ms: u32,
}

impl Default for Cb {
    fn default() -> Self {
        Self {
            inst: 1,
            closed: false,
            ctl: CtlModel::SboEnhanced,
            travel_ms: 100,
            local: false,
            interlock: false,
            accept_same: false,
            sbo_timeout_ms: 30_000,
        }
    }
}

pub enum Kind {
    CircuitBreaker(Cb),
    /// `YLTC<inst>` + `ATCC<inst>`. Posisi tap `min..=max` (posVal INT8: -64..63). `auto`: ATCC.Auto awal;
    /// saat auto, tap bergerak 1 langkah tiap `interval_ms` (berbalik arah di batas) dan raise/lower
    /// ditolak (blocked-by-mode).
    TapChanger { inst: u32, pos: i32, min: i32, max: i32, ctl: CtlModel, local: bool, auto: bool, interval_ms: u64 },
    /// `GGIO<ggio>.Ind<ind>`; mode: manual / toggle. Nomor dikunci per komponen.
    Telesignal { ggio: u32, ind: u32, value: bool, gen: Gen },
    /// `follow_cb`: id CB; nilai jadi 0 selama CB tidak close (mis. arus saat CB open)
    Telemetry { mx: Mx, value: f64, gen: Gen, follow_cb: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LnClass {
    Mmxu,
    Ggio,
}

/// Lokasi titik telemetry: `<class><inst>.<do_name>[.<phase>]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Mx {
    pub class: LnClass,
    pub inst: u32,
    pub do_name: String,
    /// Sub-DO fasa untuk WYE/DEL, mis. `phsA`, `phsAB`
    pub phase: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cdc {
    Mv,
    Wye,
    Del,
}

const WYE_PHASES: &[&str] = &["phsA", "phsB", "phsC", "neut", "net", "res"];
const DEL_PHASES: &[&str] = &["phsAB", "phsBC", "phsCA"];

/// DO yang boleh dipakai di MMXU (opsi "custom"), beserta CDC-nya.
const MMXU_DOS: &[(&str, Cdc)] = &[
    ("TotW", Cdc::Mv),
    ("TotVAr", Cdc::Mv),
    ("TotVA", Cdc::Mv),
    ("TotPF", Cdc::Mv),
    ("Hz", Cdc::Mv),
    ("PhV", Cdc::Wye),
    ("A", Cdc::Wye),
    ("W", Cdc::Wye),
    ("VAr", Cdc::Wye),
    ("VA", Cdc::Wye),
    ("PF", Cdc::Wye),
    ("Z", Cdc::Wye),
    ("PPV", Cdc::Del),
];

impl Mx {
    /// Preset untuk UI: `MW`, `MVAR`, `Hz`, `kV` (fasa A/B/C), `A` (fasa A/B/C), `generic` (GGIO.AnIn<k>).
    /// `idx`: fasa (`A`/`B`/`C`) untuk kV & A, nomor k untuk generic.
    pub fn preset(name: &str, inst: u32, idx: &str) -> Result<Self, String> {
        let phase = || match idx {
            "A" | "B" | "C" => Ok(Some(format!("phs{idx}"))),
            _ => Err(format!("fasa harus A, B, atau C (bukan '{idx}')")),
        };
        let (class, do_name, phase) = match name {
            "MW" => (LnClass::Mmxu, "TotW".into(), None),
            "MVAR" => (LnClass::Mmxu, "TotVAr".into(), None),
            "Hz" => (LnClass::Mmxu, "Hz".into(), None),
            "kV" => (LnClass::Mmxu, "PhV".into(), phase()?),
            "A" => (LnClass::Mmxu, "A".into(), phase()?),
            "generic" => {
                let k: u32 = idx.parse().map_err(|_| format!("nomor AnIn tidak valid: '{idx}'"))?;
                (LnClass::Ggio, format!("AnIn{k}"), None)
            }
            _ => return Err(format!("preset telemetry tidak dikenal: {name}")),
        };
        Ok(Self { class, inst, do_name, phase })
    }

    fn ln(&self) -> String {
        match self.class {
            LnClass::Mmxu => format!("MMXU{}", self.inst),
            LnClass::Ggio => format!("GGIO{}", self.inst),
        }
    }

    /// CDC dari DO, sekaligus validasi nama DO & fasa untuk kelas LN-nya.
    fn cdc(&self) -> Result<Cdc, String> {
        let cdc = match self.class {
            LnClass::Mmxu => MMXU_DOS.iter().find(|(d, _)| *d == self.do_name).map(|(_, c)| *c),
            LnClass::Ggio => self
                .do_name
                .strip_prefix("AnIn")
                .and_then(|k| k.parse::<u32>().ok())
                .filter(|k| *k >= 1 && !self.do_name.starts_with("AnIn0"))
                .map(|_| Cdc::Mv),
        };
        let cdc = cdc.ok_or_else(|| format!("DO '{}' tidak valid untuk {}", self.do_name, self.ln()))?;
        let phases: &[&str] = match cdc {
            Cdc::Mv => &[],
            Cdc::Wye => WYE_PHASES,
            Cdc::Del => DEL_PHASES,
        };
        match (&self.phase, phases.is_empty()) {
            (None, true) => Ok(cdc),
            (Some(p), false) if phases.contains(&p.as_str()) => Ok(cdc),
            (None, false) => Err(format!("{}.{} butuh fasa ({})", self.ln(), self.do_name, phases.join("/"))),
            (Some(p), _) => Err(format!("fasa '{p}' tidak valid untuk {}.{}", self.ln(), self.do_name)),
        }
    }

    /// Path titik di bawah LN, `.`-separated, mis. `PhV.phsA`
    fn path(&self) -> String {
        match &self.phase {
            Some(p) => format!("{}.{p}", self.do_name),
            None => self.do_name.clone(),
        }
    }
}

/// Huruf/angka/_, diawali huruf.
fn valid_ident(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Validasi semua IED: masing-masing valid, nama IED & port MMS unik.
pub fn validate_all(cfgs: &[IedConfig]) -> Result<(), String> {
    let (mut names, mut ports) = (HashSet::new(), HashSet::new());
    for c in cfgs {
        validate(c)?;
        if !names.insert(c.name.as_str()) {
            return Err(format!("nama IED {} dipakai lebih dari sekali", c.name));
        }
        if !ports.insert(c.port) {
            return Err(format!("port MMS {} dipakai lebih dari satu IED ({})", c.port, c.name));
        }
    }
    Ok(())
}

/// Validasi satu IED (keunikan antar IED: [`validate_all`]).
pub fn validate(cfg: &IedConfig) -> Result<(), String> {
    for (what, s) in [("nama IED", &cfg.name), ("nama LD", &cfg.ld)] {
        if !valid_ident(s) {
            return Err(format!("{what} '{s}' tidak valid: hanya huruf, angka, _, diawali huruf"));
        }
    }
    if cfg.name.len() + cfg.ld.len() > 32 {
        return Err(format!("nama IED + LD '{}{}' lebih dari 32 karakter", cfg.name, cfg.ld));
    }

    let mut ids = HashSet::new();
    let mut points = HashSet::new();
    let cbs: HashSet<&str> = cfg
        .components
        .iter()
        .filter(|c| matches!(c.kind, Kind::CircuitBreaker { .. }))
        .map(|c| c.id.as_str())
        .collect();
    for c in &cfg.components {
        if !ids.insert(c.id.as_str()) {
            return Err(format!("id komponen duplikat: {}", c.id));
        }
        let err = |e: String| format!("komponen {}: {e}", c.id);
        // Nomor instance/Ind dikunci per komponen: nol atau bentrok dengan komponen lain ditolak
        let (inst, ln, path) = match &c.kind {
            Kind::CircuitBreaker(cb) => (cb.inst, format!("XCBR{}", cb.inst), String::new()),
            Kind::TapChanger { inst, .. } => (*inst, format!("ATCC{inst}"), String::new()),
            Kind::Telesignal { ggio, ind, .. } => ((*ggio).min(*ind), format!("GGIO{ggio}"), format!("Ind{ind}")),
            Kind::Telemetry { mx, .. } => (mx.inst, mx.ln(), mx.path()),
        };
        if inst == 0 {
            return Err(err("nomor instance / Ind minimal 1".into()));
        }
        if !points.insert((ln.clone(), path.clone())) {
            return Err(err(format!("{ln}{}{path} sudah dipakai komponen lain", if path.is_empty() { "" } else { "." })));
        }
        match &c.kind {
            Kind::CircuitBreaker(cb) => {
                if cb.travel_ms > 60_000 {
                    return Err(err("waktu tempuh maksimal 60000 ms".into()));
                }
                if !(1000..=600_000).contains(&cb.sbo_timeout_ms) {
                    return Err(err("sboTimeout harus 1000..600000 ms".into()));
                }
            }
            Kind::TapChanger { pos, min, max, interval_ms, .. } => {
                if !(-64..=63).contains(min) || !(-64..=63).contains(max) || min >= max {
                    return Err(err(format!("batas tap harus -64 <= min < max <= 63 (min {min}, max {max})")));
                }
                if !(min..=max).contains(&pos) {
                    return Err(err(format!("posisi tap {pos} di luar {min}..{max}")));
                }
                if *interval_ms < 100 {
                    return Err(err("interval minimal 100 ms".into()));
                }
            }
            Kind::Telesignal { gen, .. } => {
                if !matches!(gen.mode, Mode::Manual | Mode::Toggle) {
                    return Err(err("telesignal hanya mendukung mode manual / toggle".into()));
                }
                gen.validate().map_err(err)?;
            }
            Kind::Telemetry { mx, gen, follow_cb, .. } => {
                mx.cdc().map_err(err)?;
                if gen.mode == Mode::Toggle {
                    return Err(err("mode toggle hanya untuk telesignal".into()));
                }
                gen.validate().map_err(err)?;
                if let Some(cb) = follow_cb {
                    if !cbs.contains(cb.as_str()) {
                        return Err(err(format!("follow_cb '{cb}' bukan id circuit breaker")));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Nilai satu atribut, ditulis ke server setelah model dibuat.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Val {
    Bool(bool),
    Int(i32),
    Float(f64),
    Dbpos(ffi::Dbpos),
    Uint(u32),
}

/// Satu titik data (punya stVal/mag + q + t). Reference relatif ke LD, mis. `XCBR1.Pos.stVal`.
pub struct Point {
    pub value_ref: String,
    pub t_ref: String,
    pub init: Val,
    pub gen: Gen,
    /// value_ref `XCBR<n>.Pos.stVal` milik CB yang diikuti
    pub follow: Option<String>,
}

/// Objek kontrol (DO dengan ctlModel != status-only). Reference relatif ke LD, tanpa atribut.
pub struct CtlSpec {
    /// DO yang dikontrol client, mis. `CSWI1.Pos`, `ATCC1.TapChg`, `ATCC1.Auto`
    pub do_ref: String,
    /// `<LN>.Loc`: true = local, perintah ditolak
    pub loc_ref: String,
    pub interlock: bool,
    /// false = status-only: tidak ada handler kontrol (spec tetap dipakai generator auto tap)
    pub controllable: bool,
    /// security enhanced (ada CommandTermination)
    pub enhanced: bool,
    pub act: CtlAct,
}

pub enum CtlAct {
    /// `mirror`: DO posisi yang ikut berubah (`XCBR<n>.Pos`)
    /// `accept_same`: perintah ke posisi yang sudah tercapai diterima (bukan position-reached)
    Cb { mirror: String, travel_ms: u64, accept_same: bool },
    /// `mirror`: `YLTC<n>.TapPos`, `auto`: `ATCC<n>.Auto`
    Tap { mirror: String, auto: String, min: i32, max: i32, interval_ms: u64 },
    /// ATCC.Auto: tulis stVal = ctlVal
    Auto,
}

/// Hasil build: model (milik pemanggil, wajib `IedModel_destroy`), atribut tetap, dan titik data.
pub struct Built {
    pub model: *mut ffi::IedModel,
    pub fixed: Vec<(String, Val)>,
    pub vendor_refs: Vec<String>,
    pub points: Vec<Point>,
    pub ctls: Vec<CtlSpec>,
}

fn c(s: &str) -> CString {
    CString::new(s).expect("nama model tidak boleh berisi NUL")
}

fn node<T>(p: *mut T) -> *mut ffi::ModelNode {
    p.cast()
}

/// Bangun model dari konfigurasi yang SUDAH lolos [`validate`].
pub fn build(cfg: &IedConfig) -> Built {
    let mut b = Built { model: ptr::null_mut(), fixed: Vec::new(), vendor_refs: Vec::new(), points: Vec::new(), ctls: Vec::new() };
    let mut st: Vec<String> = Vec::new(); // anggota DS_ST, format MMS (LN$FC$DO)
    let mut mx: Vec<String> = Vec::new();
    let status = |b: &mut Built, do_ref: String, init: Val, gen: Gen| {
        b.points.push(Point {
            value_ref: format!("{do_ref}.stVal"),
            t_ref: format!("{do_ref}.t"),
            init,
            gen,
            follow: None,
        });
    };

    // SAFETY: semua pointer dibuat oleh libIEC61850 dan dimiliki model; string di-copy oleh library.
    unsafe {
        b.model = ffi::IedModel_create(c(&cfg.name).as_ptr());
        let ld = ffi::LogicalDevice_create(c(&cfg.ld).as_ptr(), b.model);
        let mut lns: Vec<(String, *mut ffi::LogicalNode)> = Vec::new();
        let mut ln = |name: &str| -> *mut ffi::ModelNode {
            if let Some((_, p)) = lns.iter().find(|(n, _)| n == name) {
                return node(*p);
            }
            let p = ffi::LogicalNode_create(c(name).as_ptr(), ld);
            lns.push((name.into(), p));
            node(p)
        };

        // LLN0: Mod/Beh/Health dibaca FEP untuk cek liveness (LLN0.Mod.stVal)
        let lln0 = ln("LLN0");
        ffi::CDC_ENC_create(c("Mod").as_ptr(), lln0, 0, ffi::CDC_CTL_MODEL_NONE);
        ffi::CDC_ENS_create(c("Beh").as_ptr(), lln0, 0);
        ffi::CDC_ENS_create(c("Health").as_ptr(), lln0, 0);
        ffi::CDC_LPL_create(c("NamPlt").as_ptr(), lln0, 0);
        for d in ["Mod", "Beh", "Health"] {
            status(&mut b, format!("LLN0.{d}"), Val::Int(1), Gen::manual()); // on / Ok
        }
        b.fixed.push(("LLN0.Mod.ctlModel".into(), Val::Int(0)));
        b.vendor_refs.push("LLN0.NamPlt.vendor".into());

        let lphd = ln("LPHD1");
        ffi::CDC_DPL_create(c("PhyNam").as_ptr(), lphd, 0);
        ffi::CDC_ENS_create(c("PhyHealth").as_ptr(), lphd, 0);
        ffi::CDC_SPS_create(c("Proxy").as_ptr(), lphd, 0);
        b.vendor_refs.push("LPHD1.PhyNam.vendor".into());
        status(&mut b, "LPHD1.PhyHealth".into(), Val::Int(1), Gen::manual());
        status(&mut b, "LPHD1.Proxy".into(), Val::Bool(false), Gen::manual());

        // id CB -> XCBR<inst>.Pos.stVal (untuk follow_cb, urutan komponen bebas)
        let cb_refs: Vec<(&str, String)> = cfg
            .components
            .iter()
            .filter_map(|c| match &c.kind {
                Kind::CircuitBreaker(cb) => Some((c.id.as_str(), format!("XCBR{}.Pos.stVal", cb.inst))),
                _ => None,
            })
            .collect();
        // sboTimeout (CF) di DO kontrol; dibaca libIEC61850 setiap select
        let sbo_timeout = |b: &mut Built, d: *mut ffi::DataObject, do_ref: String, ms: u32| {
            ffi::DataAttribute_create(c("sboTimeout").as_ptr(), node(d), ffi::IEC61850_INT32U, ffi::IEC61850_FC_CF, 0, 0, 0);
            b.fixed.push((format!("{do_ref}.sboTimeout"), Val::Uint(ms)));
        };
        let mut wye_del: HashSet<(String, String)> = HashSet::new(); // DO WYE/DEL yang sudah dibuat
        for comp in &cfg.components {
            match &comp.kind {
                Kind::CircuitBreaker(cb) => {
                    let n = cb.inst;
                    let pos = Val::Dbpos(if cb.closed { ffi::DBPOS_ON } else { ffi::DBPOS_OFF });
                    for (cls, is_ctl) in [("XCBR", false), ("CSWI", true)] {
                        let lnn = format!("{cls}{n}");
                        let p = ln(&lnn);
                        if is_ctl {
                            let d = ffi::CDC_DPC_create(c("Pos").as_ptr(), p, 0, cb.ctl.options());
                            if cb.ctl.is_sbo() {
                                sbo_timeout(&mut b, d, format!("{lnn}.Pos"), cb.sbo_timeout_ms);
                            }
                        } else {
                            ffi::CDC_DPS_create(c("Pos").as_ptr(), p, 0);
                        }
                        ffi::CDC_SPS_create(c("Loc").as_ptr(), p, 0);
                        status(&mut b, format!("{lnn}.Pos"), pos, Gen::manual());
                        status(&mut b, format!("{lnn}.Loc"), Val::Bool(cb.local), Gen::manual());
                        st.push(format!("{lnn}$ST$Pos"));
                        st.push(format!("{lnn}$ST$Loc"));
                    }
                    if cb.ctl != CtlModel::StatusOnly {
                        b.ctls.push(CtlSpec {
                            do_ref: format!("CSWI{n}.Pos"),
                            loc_ref: format!("CSWI{n}.Loc"),
                            interlock: cb.interlock,
                            controllable: true,
                            enhanced: cb.ctl.is_enhanced(),
                            act: CtlAct::Cb { mirror: format!("XCBR{n}.Pos"), travel_ms: cb.travel_ms, accept_same: cb.accept_same },
                        });
                    }
                }
                Kind::TapChanger { inst, pos, min, max, ctl, local, auto, interval_ms } => {
                    let (yltc, atcc) = (format!("YLTC{inst}"), format!("ATCC{inst}"));
                    ffi::CDC_INS_create(c("TapPos").as_ptr(), ln(&yltc), 0);
                    let a = ln(&atcc);
                    let d = ffi::CDC_BSC_create(c("TapChg").as_ptr(), a, 0, ctl.options(), false);
                    if ctl.is_sbo() {
                        // ponytail: tap belum punya setelan sboTimeout sendiri, pakai default CB
                        sbo_timeout(&mut b, d, format!("{atcc}.TapChg"), Cb::default().sbo_timeout_ms);
                    }
                    // Auto/manual bisa dikontrol FEP (direct-normal) selama TapChg tidak status-only
                    let auto_ctl = if *ctl == CtlModel::StatusOnly { CtlModel::StatusOnly } else { CtlModel::DirectNormal };
                    ffi::CDC_SPC_create(c("Auto").as_ptr(), a, 0, auto_ctl.options());
                    ffi::CDC_SPS_create(c("Loc").as_ptr(), a, 0);
                    status(&mut b, format!("{yltc}.TapPos"), Val::Int(*pos), Gen::manual());
                    b.points.push(Point {
                        value_ref: format!("{atcc}.TapChg.valWTr.posVal"),
                        t_ref: format!("{atcc}.TapChg.t"),
                        init: Val::Int(*pos),
                        gen: Gen::manual(),
                        follow: None,
                    });
                    status(&mut b, format!("{atcc}.Auto"), Val::Bool(*auto), Gen::manual());
                    status(&mut b, format!("{atcc}.Loc"), Val::Bool(*local), Gen::manual());
                    for d in ["TapChg", "Auto", "Loc"] {
                        st.push(format!("{atcc}$ST${d}"));
                    }
                    st.push(format!("{yltc}$ST$TapPos"));
                    // spec Tap selalu ada (dipakai generator auto); handler hanya kalau bisa dikontrol
                    let controllable = *ctl != CtlModel::StatusOnly;
                    let loc_ref = format!("{atcc}.Loc");
                    b.ctls.push(CtlSpec {
                        do_ref: format!("{atcc}.TapChg"),
                        loc_ref: loc_ref.clone(),
                        interlock: false,
                        controllable,
                        enhanced: ctl.is_enhanced(),
                        act: CtlAct::Tap {
                            mirror: format!("{yltc}.TapPos"),
                            auto: format!("{atcc}.Auto"),
                            min: *min,
                            max: *max,
                            interval_ms: *interval_ms,
                        },
                    });
                    if controllable {
                        b.ctls.push(CtlSpec {
                            do_ref: format!("{atcc}.Auto"),
                            loc_ref,
                            interlock: false,
                            controllable,
                            enhanced: false,
                            act: CtlAct::Auto,
                        });
                    }
                }
                Kind::Telesignal { ggio, ind: k, value, gen } => {
                    let lnn = format!("GGIO{ggio}");
                    ffi::CDC_SPS_create(c(&format!("Ind{k}")).as_ptr(), ln(&lnn), 0);
                    status(&mut b, format!("{lnn}.Ind{k}"), Val::Bool(*value), gen.clone());
                    st.push(format!("{lnn}$ST$Ind{k}"));
                }
                Kind::Telemetry { mx: m, value, gen, follow_cb } => {
                    let lnn = m.ln();
                    let p = ln(&lnn);
                    let cdc = m.cdc().expect("sudah divalidasi");
                    match cdc {
                        Cdc::Mv => {
                            ffi::CDC_MV_create(c(&m.do_name).as_ptr(), p, 0, false);
                        }
                        Cdc::Wye | Cdc::Del if wye_del.insert((lnn.clone(), m.do_name.clone())) => {
                            let name = c(&m.do_name);
                            if cdc == Cdc::Wye {
                                ffi::CDC_WYE_create(name.as_ptr(), p, 0);
                            } else {
                                ffi::CDC_DEL_create(name.as_ptr(), p, 0);
                            }
                        }
                        _ => {} // WYE/DEL sudah ada (fasa lain dari DO yang sama)
                    }
                    let path = m.path();
                    // MV: mag.f; CMV (fasa): cVal.mag.f
                    let mag = if m.phase.is_some() { "cVal.mag.f" } else { "mag.f" };
                    b.points.push(Point {
                        value_ref: format!("{lnn}.{path}.{mag}"),
                        t_ref: format!("{lnn}.{path}.t"),
                        init: Val::Float(*value),
                        gen: gen.clone(),
                        follow: follow_cb.as_ref().and_then(|cb| cb_refs.iter().find(|(i, _)| i == cb)).map(|(_, r)| r.clone()),
                    });
                    mx.push(format!("{lnn}$MX${}", path.replace('.', "$")));
                }
            }
        }

        let lln0: *mut ffi::LogicalNode = lln0.cast();
        for (ds, fc, members) in [("DS_ST", "ST", &st), ("DS_MX", "MX", &mx)] {
            if members.is_empty() {
                continue; // RCB dengan dataset kosong tidak berguna
            }
            let set = ffi::DataSet_create(c(ds).as_ptr(), lln0);
            for m in members {
                ffi::DataSetEntry_create(set, c(m).as_ptr(), -1, ptr::null());
            }
            // nama dataset saja; libIEC61850 menambahkan prefix "<LD>/LLN0$" sendiri
            let dataset = c(ds);
            let trg = (ffi::TRG_OPT_DATA_CHANGED | ffi::TRG_OPT_QUALITY_CHANGED | ffi::TRG_OPT_INTEGRITY | ffi::TRG_OPT_GI) as u8;
            let opt = (ffi::RPT_OPT_SEQ_NUM
                | ffi::RPT_OPT_TIME_STAMP
                | ffi::RPT_OPT_REASON_FOR_INCLUSION
                | ffi::RPT_OPT_DATA_SET
                | ffi::RPT_OPT_CONF_REV) as u8;
            for (prefix, buffered) in [("brcb", true), ("urcb", false)] {
                for i in 1..=2 {
                    let opt = if buffered { opt | (ffi::RPT_OPT_ENTRY_ID | ffi::RPT_OPT_BUFFER_OVERFLOW) as u8 } else { opt };
                    // rptId NULL -> libIEC61850 memakai RptID default (reference RCB)
                    // ponytail: BufTm 0 & IntgPd 0, client (FEP) yang mengatur
                    ffi::ReportControlBlock_create(
                        c(&format!("{prefix}{fc}{i:02}")).as_ptr(),
                        lln0,
                        ptr::null(),
                        buffered,
                        dataset.as_ptr(),
                        1,
                        trg,
                        opt,
                        0,
                        0,
                    );
                }
            }
        }
    }
    b
}

/// Reference titik-titik satu komponen (relatif ke LD), untuk tampilan & ubah manual dari UI.
pub enum Refs {
    Cb { xcbr: String, cswi: String },
    Tap { yltc: String, atcc: String },
    /// telesignal / telemetry: DO + atribut nilai (`stVal` / `mag.f` / `cVal.mag.f`)
    Point { do_ref: String, value_ref: String },
}

impl CtlModel {
    fn name(self) -> &'static str {
        match self {
            Self::StatusOnly => "status-only",
            Self::DirectNormal => "direct-normal",
            Self::SboNormal => "sbo-normal",
            Self::DirectEnhanced => "direct-enhanced",
            Self::SboEnhanced => "sbo-enhanced",
        }
    }
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Manual => "manual",
        Mode::Toggle => "toggle",
        Mode::Random => "random",
        Mode::RandomWalk => "random_walk",
        Mode::Sine => "sine",
    }
}

impl Mx {
    /// Satuan tampilan di UI (nilai di model tetap apa adanya)
    fn unit(&self) -> &'static str {
        match self.do_name.as_str() {
            "TotW" | "W" => "MW",
            "TotVAr" | "VAr" => "MVAr",
            "TotVA" | "VA" => "MVA",
            "Hz" => "Hz",
            "PhV" | "PPV" => "kV",
            "A" => "A",
            "Z" => "Ω",
            _ => "",
        }
    }
}

impl Component {
    pub fn refs(&self) -> Refs {
        match &self.kind {
            Kind::CircuitBreaker(cb) => Refs::Cb { xcbr: format!("XCBR{}", cb.inst), cswi: format!("CSWI{}", cb.inst) },
            Kind::TapChanger { inst, .. } => Refs::Tap { yltc: format!("YLTC{inst}"), atcc: format!("ATCC{inst}") },
            Kind::Telesignal { ggio, ind, .. } => {
                let do_ref = format!("GGIO{ggio}.Ind{ind}");
                Refs::Point { value_ref: format!("{do_ref}.stVal"), do_ref }
            }
            Kind::Telemetry { mx, .. } => {
                let do_ref = format!("{}.{}", mx.ln(), mx.path());
                let mag = if mx.phase.is_some() { "cVal.mag.f" } else { "mag.f" };
                Refs::Point { value_ref: format!("{do_ref}.{mag}"), do_ref }
            }
        }
    }

    /// Konfigurasi komponen untuk UI (bagian statis event `components`)
    pub fn describe(&self) -> serde_json::Value {
        use serde_json::json;
        let gen = |g: &Gen| json!({ "mode": mode_name(g.mode), "interval_ms": g.interval_ms, "min": g.min, "max": g.max, "scale": g.scale });
        match (&self.kind, self.refs()) {
            (Kind::CircuitBreaker(cb), Refs::Cb { xcbr, cswi }) => json!({
                "id": self.id, "type": "cb", "ln": format!("{xcbr} / {cswi}"), "ctl_model": cb.ctl.name(),
                "travel_ms": cb.travel_ms, "interlock": cb.interlock, "accept_same": cb.accept_same,
                "sbo_timeout_ms": if cb.ctl.is_sbo() { Some(cb.sbo_timeout_ms) } else { None },
            }),
            (Kind::TapChanger { min, max, ctl, interval_ms, .. }, Refs::Tap { yltc, atcc }) => json!({
                "id": self.id, "type": "tap", "ln": format!("{yltc} / {atcc}"), "ctl_model": ctl.name(),
                "min": min, "max": max, "interval_ms": interval_ms,
            }),
            (Kind::Telesignal { gen: g, .. }, Refs::Point { do_ref, .. }) => json!({
                "id": self.id, "type": "telesignal", "ref": do_ref, "gen": gen(g),
            }),
            (Kind::Telemetry { mx, gen: g, follow_cb, .. }, Refs::Point { do_ref, .. }) => json!({
                "id": self.id, "type": "telemetry", "ref": do_ref, "unit": mx.unit(), "gen": gen(g), "follow_cb": follow_cb,
            }),
            _ => unreachable!("refs() sesuai kind"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(components: Vec<Component>) -> IedConfig {
        IedConfig { name: "SIMIED1".into(), ld: "LD0".into(), port: 102, components }
    }

    fn tm(id: &str, mx: Mx) -> Component {
        Component { id: id.into(), kind: Kind::Telemetry { mx, value: 0.0, gen: Gen::manual(), follow_cb: None } }
    }

    #[test]
    fn preset_telemetry() {
        assert_eq!(Mx::preset("MW", 1, "").unwrap().path(), "TotW");
        assert_eq!(Mx::preset("kV", 2, "B").unwrap().path(), "PhV.phsB");
        assert_eq!(Mx::preset("A", 1, "C").unwrap().ln(), "MMXU1");
        assert_eq!(Mx::preset("generic", 3, "7").unwrap().ln(), "GGIO3");
        assert!(Mx::preset("kV", 1, "D").is_err());
        assert!(Mx::preset("kV", 1, "").is_err());
        assert!(Mx::preset("X", 1, "").is_err());
    }

    #[test]
    fn custom_dibatasi_do_valid() {
        let mx = |class, d: &str, p: Option<&str>| Mx { class, inst: 1, do_name: d.into(), phase: p.map(Into::into) };
        assert!(mx(LnClass::Mmxu, "TotVA", None).cdc().is_ok());
        assert!(mx(LnClass::Mmxu, "PPV", Some("phsAB")).cdc().is_ok());
        assert!(mx(LnClass::Mmxu, "PPV", Some("phsA")).cdc().is_err());
        assert!(mx(LnClass::Mmxu, "PhV", None).cdc().is_err()); // WYE butuh fasa
        assert!(mx(LnClass::Mmxu, "TotW", Some("phsA")).cdc().is_err()); // MV tanpa fasa
        assert!(mx(LnClass::Mmxu, "Ngawur", None).cdc().is_err());
        assert!(mx(LnClass::Mmxu, "AnIn1", None).cdc().is_err());
        assert!(mx(LnClass::Ggio, "AnIn12", None).cdc().is_ok());
        assert!(mx(LnClass::Ggio, "AnIn0", None).cdc().is_err());
        assert!(mx(LnClass::Ggio, "AnIn01", None).cdc().is_err());
        assert!(mx(LnClass::Ggio, "TotW", None).cdc().is_err());
    }

    #[test]
    fn validasi_nama_dan_duplikat() {
        assert!(validate(&cfg(vec![])).is_ok());
        for (name, ld) in [("1IED", "LD0"), ("SIM-1", "LD0"), ("SIM1", "_LD"), ("", "LD0"), (&"A".repeat(30)[..], "LD0")] {
            let c = IedConfig { name: name.into(), ld: ld.into(), port: 102, components: vec![] };
            assert!(validate(&c).is_err(), "{name}{ld} harus ditolak");
        }
        let c = IedConfig { name: "A".repeat(29), ld: "LD0".into(), port: 102, components: vec![] };
        assert!(validate(&c).is_ok()); // pas 32

        let dup = cfg(vec![tm("a", Mx::preset("kV", 1, "A").unwrap()), tm("b", Mx::preset("kV", 1, "A").unwrap())]);
        assert!(validate(&dup).unwrap_err().contains("sudah dipakai"));
        // fasa berbeda / instance berbeda boleh
        let ok = cfg(vec![
            tm("a", Mx::preset("kV", 1, "A").unwrap()),
            tm("b", Mx::preset("kV", 1, "B").unwrap()),
            tm("c", Mx::preset("kV", 2, "A").unwrap()),
        ]);
        assert!(validate(&ok).is_ok());
        assert!(validate(&cfg(vec![tm("a", Mx::preset("MW", 0, "").unwrap())])).is_err());

        let follow_salah = cfg(vec![Component {
            id: "t".into(),
            kind: Kind::Telemetry { mx: Mx::preset("MW", 1, "").unwrap(), value: 0.0, gen: Gen::manual(), follow_cb: Some("x".into()) },
        }]);
        assert!(validate(&follow_salah).is_err());
    }

    #[test]
    fn nama_dan_port_unik_antar_ied() {
        let ied = |name: &str, port| IedConfig { name: name.into(), ld: "LD0".into(), port, components: vec![] };
        assert!(validate_all(&[ied("SIMIED1", 102), ied("SIMIED2", 10102)]).is_ok());
        assert!(validate_all(&[ied("SIMIED1", 102), ied("SIMIED1", 10102)]).unwrap_err().contains("nama IED"));
        assert!(validate_all(&[ied("SIMIED1", 102), ied("SIMIED2", 102)]).unwrap_err().contains("port MMS"));
    }

    #[test]
    fn nomor_instance_dikunci_dan_unik() {
        let k = |id: &str, kind| Component { id: id.into(), kind };
        let cbk = |inst| Kind::CircuitBreaker(Cb { inst, ..Cb::default() });
        let ts = |ggio, ind| Kind::Telesignal { ggio, ind, value: false, gen: Gen::manual() };
        // nomor tidak harus berurutan (komponen di tengah boleh sudah dihapus)
        assert!(validate(&cfg(vec![k("a", cbk(3)), k("b", cbk(1)), k("c", ts(1, 7)), k("d", ts(1, 2))])).is_ok());
        assert!(validate(&cfg(vec![k("a", cbk(2)), k("b", cbk(2))])).unwrap_err().contains("XCBR2 sudah dipakai"));
        assert!(validate(&cfg(vec![k("a", ts(1, 4)), k("b", ts(1, 4))])).unwrap_err().contains("GGIO1.Ind4"));
        assert!(validate(&cfg(vec![k("a", cbk(0))])).is_err());
        assert!(validate(&cfg(vec![k("a", ts(1, 0))])).is_err());
        assert!(validate(&cfg(vec![k("a", Kind::CircuitBreaker(Cb { sbo_timeout_ms: 500, ..Cb::default() }))])).is_err());
    }
}
