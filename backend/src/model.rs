//! Konfigurasi IED + validasi + bangun IedModel libIEC61850 secara dinamis (PLAN bagian 4).
//!
//! Satu LD (default `LD0`) berisi `LLN0` + `LPHD1`, lalu per komponen:
//! - Circuit Breaker ke-n : `XCBR<n>` (Pos DPS, Loc SPS) + `CSWI<n>` (Pos DPC, Loc SPS)
//! - Telesignal           : `GGIO<inst>.Ind<k>` (SPS), k = 1..N otomatis per instance GGIO
//! - Telemetry            : LN/DO pilihan (preset atau custom), lihat [`Mx`]
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

pub enum Kind {
    CircuitBreaker { closed: bool },
    /// `GGIO<ggio>.Ind<k>`; mode: manual / toggle
    Telesignal { ggio: u32, value: bool, gen: Gen },
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

/// Validasi satu IED. ponytail: keunikan nama antar IED dicek di manager (fase 5)
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
        match &c.kind {
            Kind::CircuitBreaker { .. } => {}
            Kind::Telesignal { ggio, gen, .. } => {
                if *ggio == 0 {
                    return Err(err("instance GGIO minimal 1".into()));
                }
                if !matches!(gen.mode, Mode::Manual | Mode::Toggle) {
                    return Err(err("telesignal hanya mendukung mode manual / toggle".into()));
                }
                gen.validate().map_err(err)?;
            }
            Kind::Telemetry { mx, gen, follow_cb, .. } => {
                if mx.inst == 0 {
                    return Err(err(format!("instance {:?} minimal 1", mx.class)));
                }
                mx.cdc().map_err(err)?;
                if !points.insert((mx.ln(), mx.path())) {
                    return Err(err(format!("{}.{} sudah dipakai komponen lain", mx.ln(), mx.path())));
                }
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

/// Hasil build: model (milik pemanggil, wajib `IedModel_destroy`), atribut tetap, dan titik data.
pub struct Built {
    pub model: *mut ffi::IedModel,
    pub fixed: Vec<(String, Val)>,
    pub vendor_refs: Vec<String>,
    pub points: Vec<Point>,
}

fn c(s: &str) -> CString {
    CString::new(s).expect("nama model tidak boleh berisi NUL")
}

fn node<T>(p: *mut T) -> *mut ffi::ModelNode {
    p.cast()
}

/// Bangun model dari konfigurasi yang SUDAH lolos [`validate`].
pub fn build(cfg: &IedConfig) -> Built {
    let mut b = Built { model: ptr::null_mut(), fixed: Vec::new(), vendor_refs: Vec::new(), points: Vec::new() };
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

        // id CB -> XCBR<n>.Pos.stVal (untuk follow_cb, urutan komponen bebas)
        let cb_refs: Vec<(&str, String)> = cfg
            .components
            .iter()
            .filter(|c| matches!(c.kind, Kind::CircuitBreaker { .. }))
            .enumerate()
            .map(|(i, c)| (c.id.as_str(), format!("XCBR{}.Pos.stVal", i + 1)))
            .collect();
        let mut cb_n = 0;
        let mut ind_n: Vec<(u32, u32)> = Vec::new(); // (instance GGIO, jumlah Ind)
        let mut wye_del: HashSet<(String, String)> = HashSet::new(); // DO WYE/DEL yang sudah dibuat
        for comp in &cfg.components {
            match &comp.kind {
                Kind::CircuitBreaker { closed } => {
                    cb_n += 1;
                    let pos = Val::Dbpos(if *closed { ffi::DBPOS_ON } else { ffi::DBPOS_OFF });
                    for (cls, ctl) in [("XCBR", false), ("CSWI", true)] {
                        let lnn = format!("{cls}{cb_n}");
                        let p = ln(&lnn);
                        if ctl {
                            // ponytail: status-only dulu, ctlModel dari UI + handler kontrol di fase 4
                            ffi::CDC_DPC_create(c("Pos").as_ptr(), p, 0, ffi::CDC_CTL_MODEL_NONE);
                            b.fixed.push((format!("{lnn}.Pos.ctlModel"), Val::Int(0)));
                        } else {
                            ffi::CDC_DPS_create(c("Pos").as_ptr(), p, 0);
                        }
                        ffi::CDC_SPS_create(c("Loc").as_ptr(), p, 0);
                        status(&mut b, format!("{lnn}.Pos"), pos, Gen::manual());
                        status(&mut b, format!("{lnn}.Loc"), Val::Bool(false), Gen::manual()); // remote
                        st.push(format!("{lnn}$ST$Pos"));
                        st.push(format!("{lnn}$ST$Loc"));
                    }
                }
                Kind::Telesignal { ggio, value, gen } => {
                    let k = match ind_n.iter_mut().find(|(g, _)| g == ggio) {
                        Some((_, k)) => {
                            *k += 1;
                            *k
                        }
                        None => {
                            ind_n.push((*ggio, 1));
                            1
                        }
                    };
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
}
