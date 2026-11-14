//! State aplikasi: daftar IED + operasi konfigurasi (tambah/ubah/hapus IED & komponen, import/export)
//! + autosave. Semua perubahan konfigurasi lewat [`App::change`]: validasi dulu, lalu IED yang modelnya
//! berubah di-restart (PLAN: model IEC 61850 tidak bisa diubah saat server jalan).

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde_json::Value;
use socketioxide::SocketIo;
use tracing::{error, info};

use crate::events::{Events, Kind as Ev};
use crate::manager::{self, Slot};
use crate::model::{self, Component, IedConfig, Kind, SaveFile};
use crate::server::Ied;

pub struct App {
    slots: RwLock<Vec<Arc<Slot>>>,
    pub events: Arc<Events>,
    pub io: SocketIo,
    autosave: Option<PathBuf>,
    /// perubahan konfigurasi dijalankan satu per satu
    edit: tokio::sync::Mutex<()>,
    /// default untuk IED baru (prefix nama, LD, port)
    defaults: crate::config::Config,
}

/// Room Socket.IO per IED
pub fn room(ied: &str) -> String {
    format!("ied:{ied}")
}

impl App {
    pub fn new(io: SocketIo, events: Arc<Events>, defaults: crate::config::Config) -> Self {
        let autosave = defaults.autosave.as_ref().map(PathBuf::from);
        Self { slots: RwLock::new(Vec::new()), events, io, autosave, edit: tokio::sync::Mutex::new(()), defaults }
    }

    pub fn slots(&self) -> Vec<Arc<Slot>> {
        self.slots.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn slot(&self, name: &str) -> Result<Arc<Slot>, String> {
        self.slots().into_iter().find(|s| s.name() == name).ok_or_else(|| format!("IED {name} tidak ada"))
    }

    /// IED yang sedang hidup
    pub fn ied(&self, name: &str) -> Result<Arc<Ied>, String> {
        self.slot(name)?.running()
    }

    pub fn ieds_json(&self) -> Value {
        Value::Array(self.slots().iter().map(|s| s.info()).collect())
    }

    fn configs(&self) -> Vec<IedConfig> {
        self.slots().iter().map(|s| s.cfg()).collect()
    }

    /// Kirim `components` ke UI yang membuka IED ini (mis. setelah urutan berubah / IED dibuat ulang)
    pub fn push_components(&self, name: &str) {
        if let Some(ied) = self.slot(name).ok().and_then(|s| s.ied()) {
            if let Some(ns) = self.io.of("/") {
                let _ = ns.to(room(name)).emit("components", &ied.components_json());
            }
        }
    }

    /// Konfigurasi awal: dari file autosave kalau ada, kalau tidak dari `demo` (lalu langsung disimpan).
    pub async fn load(&self, demo: impl FnOnce() -> Result<Vec<IedConfig>, String>) -> Result<(), String> {
        let (cfgs, from) = match self.autosave.as_ref().filter(|p| p.exists()) {
            Some(p) => {
                let text = std::fs::read_to_string(p).map_err(|e| format!("gagal baca {}: {e}", p.display()))?;
                let f: SaveFile = serde_json::from_str(&text).map_err(|e| format!("{} tidak valid: {e}", p.display()))?;
                (f.ieds, format!("dimuat dari {}", p.display()))
            }
            None => (demo()?, "IED contoh (IED_COUNT)".to_string()),
        };
        let n = cfgs.len();
        let r = self.change(move |c| {
            *c = cfgs;
            Ok(format!("{n} IED {from}"))
        })
        .await;
        info!("konfigurasi: {}", r.as_ref().unwrap_or_else(|e| e));
        r.map(|_| ())
    }

    /// Ubah konfigurasi semua IED lewat `f`, validasi, terapkan (restart yang perlu), simpan, beri tahu UI.
    /// Gagal validasi = tidak ada yang berubah.
    pub async fn change(&self, f: impl FnOnce(&mut Vec<IedConfig>) -> Result<String, String>) -> Result<String, String> {
        let _g = self.edit.lock().await;
        let mut cfgs = self.configs();
        let what = f(&mut cfgs)?;
        for c in &mut cfgs {
            model::allocate(c);
        }
        model::validate_all(&cfgs)?;
        let r = manager::apply(&self.slots(), cfgs, &self.events).await;
        *self.slots.write().unwrap_or_else(|e| e.into_inner()) = r.slots;
        self.save();

        let mut msg = what;
        if !r.restarted.is_empty() {
            msg += &format!(" (jalan / restart: {})", r.restarted.join(", "));
        }
        if !r.removed.is_empty() {
            msg += &format!(" (dihapus: {})", r.removed.join(", "));
        }
        for name in r.restarted.iter().chain(&r.removed) {
            self.events.push(name, Ev::Config, msg.clone());
        }
        if r.restarted.is_empty() && r.removed.is_empty() {
            self.events.push("-", Ev::Config, msg.clone());
        }
        // of("/"): aman dipanggil sebelum namespace terdaftar (konfigurasi awal dimuat sebelum Socket.IO siap)
        if let Some(ns) = self.io.of("/") {
            let _ = ns.emit("ieds", &self.ieds_json());
        }
        for name in &r.restarted {
            self.push_components(name);
        }
        if r.errors.is_empty() {
            Ok(msg)
        } else {
            Err(format!("{msg}; tersimpan tapi gagal jalan: {}", r.errors.join("; ")))
        }
    }

    /// Simpan semua konfigurasi ke file autosave (tulis ke .tmp lalu rename, supaya file tidak setengah jadi).
    fn save(&self) {
        let Some(path) = &self.autosave else { return };
        let res = (|| {
            let body = serde_json::to_string_pretty(&SaveFile { version: 1, ieds: self.configs() }).map_err(|e| e.to_string())?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, path).map_err(|e| e.to_string())
        })();
        if let Err(e) = res {
            error!("autosave ke {} gagal: {e}", path.display());
        }
    }

    pub fn set_order(&self, ied: &str, ids: Vec<String>) -> Result<String, String> {
        self.slot(ied)?.set_order(ids)?;
        self.save();
        self.push_components(ied);
        Ok("urutan disimpan".into())
    }

    // ---- komponen -------------------------------------------------------------------------

    pub async fn add_component(&self, ied: &str, comp: &Value) -> Result<String, String> {
        let c = parse_component(comp)?;
        self.change(|cfgs| {
            let cfg = find(cfgs, ied)?;
            if cfg.components.iter().any(|x| x.id == c.id) {
                return Err(format!("id {} sudah dipakai di {ied}", c.id));
            }
            if !cfg.order.is_empty() {
                cfg.order.push(c.id.clone());
            }
            let msg = format!("{ied}: tambah {} {}", c.type_name(), c.id);
            cfg.components.push(c);
            Ok(msg)
        })
        .await
    }

    /// Ubah komponen `id`; boleh ganti id (referensi follow_cb / proteksi ikut diganti). Jenis tidak boleh berubah.
    pub async fn update_component(&self, ied: &str, id: &str, comp: &Value) -> Result<String, String> {
        let mut c = parse_component(comp)?;
        self.change(|cfgs| {
            let cfg = find(cfgs, ied)?;
            let i = cfg.components.iter().position(|x| x.id == id).ok_or_else(|| format!("komponen {id} tidak ada di {ied}"))?;
            let old = &cfg.components[i];
            if old.type_name() != c.type_name() {
                return Err(format!("jenis komponen tidak bisa diubah ({} -> {}); hapus lalu tambah", old.type_name(), c.type_name()));
            }
            if c.id != id && cfg.components.iter().any(|x| x.id == c.id) {
                return Err(format!("id {} sudah dipakai di {ied}", c.id));
            }
            model::keep_number(old, &mut c);
            let new_id = c.id.clone();
            cfg.components[i] = c;
            if new_id != id {
                for x in &mut cfg.components {
                    match &mut x.kind {
                        Kind::Telemetry { follow_cb: Some(f), .. } if f == id => *f = new_id.clone(),
                        Kind::Protection { cb, .. } if cb == id => *cb = new_id.clone(),
                        _ => {}
                    }
                }
                cfg.order.iter_mut().filter(|o| *o == id).for_each(|o| *o = new_id.clone());
            }
            Ok(format!("{ied}: ubah {id}{}", if new_id != id { format!(" -> {new_id}") } else { String::new() }))
        })
        .await
    }

    pub async fn remove_component(&self, ied: &str, id: &str) -> Result<String, String> {
        self.change(|cfgs| {
            let cfg = find(cfgs, ied)?;
            let users: Vec<&str> = cfg
                .components
                .iter()
                .filter(|x| match &x.kind {
                    Kind::Telemetry { follow_cb: Some(f), .. } => f == id,
                    Kind::Protection { cb, .. } => cb == id,
                    _ => false,
                })
                .map(|x| x.id.as_str())
                .collect();
            if !users.is_empty() {
                return Err(format!("{id} masih dipakai oleh {} (follow_cb / proteksi); ubah itu dulu", users.join(", ")));
            }
            let n = cfg.components.len();
            cfg.components.retain(|x| x.id != id);
            if cfg.components.len() == n {
                return Err(format!("komponen {id} tidak ada di {ied}"));
            }
            cfg.order.retain(|o| o != id);
            Ok(format!("{ied}: hapus {id}"))
        })
        .await
    }

    // ---- IED ------------------------------------------------------------------------------

    /// Tambah IED. `{name?, ld?, port?, template?, count?}`: tanpa nama = `<prefix><n>` berikutnya,
    /// tanpa port = port bebas berikutnya (102, 10102, 10103, ...). `template` = salin komponen IED itu.
    /// `count` > 1 = banyak IED sekaligus (nama & port otomatis).
    pub async fn add_ied(&self, p: &Value) -> Result<String, String> {
        let count = p["count"].as_u64().unwrap_or(1);
        if !(1..=200).contains(&count) {
            return Err("jumlah IED 1..200".into());
        }
        let d = &self.defaults;
        self.change(|cfgs| {
            let base = match p["template"].as_str().filter(|t| !t.is_empty()) {
                Some(t) => {
                    let mut c = find(cfgs, t)?.clone();
                    c.order.clear(); // urutan default untuk salinan
                    c
                }
                None => IedConfig::default(),
            };
            let mut added = Vec::new();
            for _ in 0..count {
                let name = match p["name"].as_str().filter(|n| !n.is_empty() && count == 1) {
                    Some(n) => n.to_string(),
                    None => (1..).map(|n| format!("{}{n}", d.ied_prefix)).find(|n| !cfgs.iter().any(|c| c.name == *n)).expect("tak hingga"),
                };
                let port = match p["port"].as_u64().filter(|_| count == 1) {
                    Some(x) => u16::try_from(x).map_err(|_| format!("port tidak valid: {x}"))?,
                    None => std::iter::once(d.mms_port)
                        .chain(d.mms_port_base..=u16::MAX)
                        .find(|x| !cfgs.iter().any(|c| c.port == *x))
                        .ok_or("tidak ada port bebas")?,
                };
                let ld = p["ld"].as_str().filter(|l| !l.is_empty()).map_or_else(|| if base.ld.is_empty() { d.ld_name.clone() } else { base.ld.clone() }, String::from);
                added.push(format!("{name}:{port}"));
                cfgs.push(IedConfig { name, ld, port, ..base.clone() });
            }
            Ok(format!("tambah IED {}", added.join(", ")))
        })
        .await
    }

    /// Ubah nama / LD / port IED (`{name?, ld?, port?}`); IED restart. Ganti nama = IED lama dihapus, baru dibuat.
    pub async fn update_ied(&self, ied: &str, p: &Value) -> Result<String, String> {
        self.change(|cfgs| {
            let cfg = find(cfgs, ied)?;
            if let Some(n) = p["name"].as_str().filter(|n| !n.is_empty()) {
                cfg.name = n.into();
            }
            if let Some(l) = p["ld"].as_str().filter(|l| !l.is_empty()) {
                cfg.ld = l.into();
            }
            if let Some(x) = p["port"].as_u64() {
                cfg.port = u16::try_from(x).map_err(|_| format!("port tidak valid: {x}"))?;
            }
            Ok(format!("ubah IED {ied} -> {} (LD {}, port {})", cfg.name, cfg.ld, cfg.port))
        })
        .await
    }

    pub async fn remove_ied(&self, ied: &str) -> Result<String, String> {
        self.change(|cfgs| {
            let n = cfgs.len();
            cfgs.retain(|c| c.name != ied);
            if cfgs.len() == n {
                return Err(format!("IED {ied} tidak ada"));
            }
            Ok(format!("hapus IED {ied}"))
        })
        .await
    }

    // ---- export / import ------------------------------------------------------------------

    /// Semua IED, atau satu IED (`{"version":1,"ieds":[...]}`)
    pub fn export(&self, ied: Option<&str>) -> Result<Value, String> {
        let ieds = match ied {
            Some(n) => vec![self.slot(n)?.cfg()],
            None => self.configs(),
        };
        serde_json::to_value(SaveFile { version: 1, ieds }).map_err(|e| e.to_string())
    }

    /// Tanpa `ied`: semua IED diganti isi file. Dengan `ied`: komponen IED itu diganti komponen
    /// IED pertama di file (nama / LD / port tetap; penghitung nomor tidak turun).
    pub async fn import(&self, data: &Value, ied: Option<&str>) -> Result<String, String> {
        let f: SaveFile = serde_json::from_value(data.clone()).map_err(|e| format!("file import tidak valid: {e}"))?;
        match ied {
            None => {
                let n = f.ieds.len();
                self.change(move |c| {
                    *c = f.ieds;
                    Ok(format!("import {n} IED (konfigurasi lama diganti)"))
                })
                .await
            }
            Some(name) => {
                let src = f.ieds.into_iter().next().ok_or("file import tidak berisi IED")?;
                self.change(|cfgs| {
                    let cfg = find(cfgs, name)?;
                    for (k, v) in src.counters {
                        let e = cfg.counters.entry(k).or_insert(0);
                        *e = (*e).max(v);
                    }
                    let n = src.components.len();
                    cfg.components = src.components;
                    cfg.order = src.order;
                    Ok(format!("{name}: import {n} komponen (komponen lama diganti)"))
                })
                .await
            }
        }
    }
}

fn find<'a>(cfgs: &'a mut [IedConfig], name: &str) -> Result<&'a mut IedConfig, String> {
    cfgs.iter_mut().find(|c| c.name == name).ok_or_else(|| format!("IED {name} tidak ada"))
}

fn parse_component(v: &Value) -> Result<Component, String> {
    let c: Component = serde_json::from_value(v.clone()).map_err(|e| format!("komponen tidak valid: {e}"))?;
    if c.id.trim().is_empty() {
        return Err("id komponen wajib diisi".into());
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn app(autosave: Option<String>) -> App {
        let (_, io) = SocketIo::new_layer();
        let mut d = crate::config::Config::from_env().unwrap();
        d.mms_port = 10161;
        d.mms_port_base = 10162;
        d.autosave = autosave;
        App::new(io, Arc::new(Events::new()), d)
    }

    /// Alur fase 8: tambah IED (template, banyak), komponen, nomor tidak dipakai ulang, autosave & muat ulang.
    #[tokio::test(flavor = "multi_thread")]
    async fn konfigurasi_dari_ui_dan_autosave() {
        let dir = std::env::temp_dir().join(format!("simtest{}", crate::server::now_ms()));
        let path = dir.join("simulator.json").to_string_lossy().into_owned();
        let a = app(Some(path.clone())).await;
        a.load(|| Ok(vec![])).await.unwrap();

        a.add_ied(&json!({})).await.unwrap();
        assert_eq!(a.slot("SIMIED1").unwrap().port(), 10161);
        a.add_component("SIMIED1", &json!({ "id": "cb1", "type": "cb", "closed": true })).await.unwrap();
        a.add_component("SIMIED1", &json!({ "id": "cb2", "type": "cb" })).await.unwrap();
        a.add_component("SIMIED1", &json!({ "id": "mw", "type": "telemetry", "mx": { "class": "MMXU", "inst": 1, "do": "TotW" }, "follow_cb": "cb2" }))
            .await
            .unwrap();
        a.add_component("SIMIED1", &json!({ "id": "p1", "type": "protection", "class": "PTOC", "cb": "cb1" })).await.unwrap();
        assert!(a.add_component("SIMIED1", &json!({ "id": "cb1", "type": "cb" })).await.unwrap_err().contains("sudah dipakai"));
        assert!(a.add_component("SIMIED1", &json!({ "id": "x", "type": "ngawur" })).await.unwrap_err().contains("tidak valid"));
        assert!(a.remove_component("SIMIED1", "cb2").await.unwrap_err().contains("mw"));

        // ganti id CB: referensi ikut; nomor XCBR dikunci
        a.update_component("SIMIED1", "cb2", &json!({ "id": "cbB", "type": "cb", "inst": 99, "ctl_model": "direct-normal" })).await.unwrap();
        let cfg = a.slot("SIMIED1").unwrap().cfg();
        assert_eq!(cfg.components[1].number(), Some(("XCBR".into(), 2)));
        assert!(matches!(&cfg.components[2].kind, Kind::Telemetry { follow_cb: Some(f), .. } if f == "cbB"));
        assert!(a.update_component("SIMIED1", "cbB", &json!({ "id": "cbB", "type": "tap" })).await.is_err());

        // hapus mw lalu cbB, tambah CB baru -> XCBR3 (bukan 2)
        a.remove_component("SIMIED1", "mw").await.unwrap();
        a.remove_component("SIMIED1", "cbB").await.unwrap();
        a.add_component("SIMIED1", &json!({ "id": "cb3", "type": "cb" })).await.unwrap();
        assert!(a.ied("SIMIED1").unwrap().components_json()["components"].as_array().unwrap().iter().any(|c| c["ln"] == "XCBR3 / CSWI3"));

        // banyak IED dari template; port lanjut 10162..
        a.add_ied(&json!({ "template": "SIMIED1", "count": 3 })).await.unwrap();
        let names: Vec<String> = a.slots().iter().map(|s| format!("{}:{}", s.name(), s.port())).collect();
        assert_eq!(names, ["SIMIED1:10161", "SIMIED2:10162", "SIMIED3:10163", "SIMIED4:10164"]);
        assert_eq!(a.slot("SIMIED3").unwrap().cfg().components.len(), 3);
        assert!(a.update_ied("SIMIED2", &json!({ "port": 10161 })).await.unwrap_err().contains("port MMS"));
        a.update_ied("SIMIED4", &json!({ "name": "GI_X", "port": 10170 })).await.unwrap();
        a.remove_ied("SIMIED3").await.unwrap();
        a.set_order("SIMIED1", vec!["cb3".into(), "p1".into(), "cb1".into()]).unwrap();

        // autosave: muat ulang di "container baru" -> konfigurasi sama
        let saved = std::fs::read_to_string(&path).unwrap();
        let before: Vec<IedConfig> = a.slots().iter().map(|s| s.cfg()).collect();
        for s in a.slots() {
            s.turn_off().await;
        }
        let b = app(Some(path.clone())).await;
        b.load(|| Err("tidak boleh dipanggil: file autosave ada".into())).await.unwrap();
        let after: Vec<IedConfig> = b.slots().iter().map(|s| s.cfg()).collect();
        assert_eq!(before, after, "{saved}");
        assert_eq!(b.ied("SIMIED1").unwrap().components_json()["components"][0]["id"], "cb3");

        // import per IED: komponen diganti, penghitung tidak turun
        let one = json!({ "version": 1, "ieds": [{ "name": "LAIN", "port": 1, "components": [{ "id": "c", "type": "cb" }] }] });
        b.import(&one, Some("SIMIED2")).await.unwrap();
        let c2 = b.slot("SIMIED2").unwrap().cfg();
        assert_eq!(c2.port, 10162);
        assert_eq!(c2.components[0].number(), Some(("XCBR".into(), 4)), "penghitung SIMIED2 sudah 3");
        // import semua: diganti; file rusak ditolak tanpa perubahan
        assert!(b.import(&json!({ "ieds": "x" }), None).await.is_err());
        b.import(&b.export(Some("SIMIED1")).unwrap(), None).await.unwrap();
        assert_eq!(b.slots().len(), 1);
        for s in b.slots() {
            s.turn_off().await;
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
