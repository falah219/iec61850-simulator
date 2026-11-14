//! Daftar IED. Satu [`Slot`] per IED menyimpan konfigurasinya, sehingga IED bisa dimatikan
//! (IedServer + model dihancurkan) dan dibuat ulang dari konfigurasi: skenario mati/hidup (seperti
//! reboot: buffer report kosong, nilai awal) dan restart otomatis saat komponen diubah dari UI.

use std::sync::{Arc, Mutex, RwLock};

use serde_json::{json, Value};

use crate::events::{Events, Kind as Ev};
use crate::model::{self, IedConfig};
use crate::server::Ied;

pub struct Slot {
    cfg: Mutex<IedConfig>,
    events: Arc<Events>,
    /// None = IED mati
    ied: RwLock<Option<Arc<Ied>>>,
    /// mati / hidup / ganti konfigurasi tidak boleh berjalan bersamaan
    power: tokio::sync::Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Slot {
    /// Slot dalam keadaan mati; hidupkan dengan [`Slot::turn_on`].
    pub fn new(cfg: IedConfig, events: Arc<Events>) -> Self {
        Self { cfg: Mutex::new(cfg), events, ied: RwLock::new(None), power: tokio::sync::Mutex::new(()) }
    }

    /// Validasi + jalankan IED. Harus dipanggil di dalam runtime tokio.
    #[cfg(test)]
    pub fn start(cfg: IedConfig, events: Arc<Events>) -> Result<Self, String> {
        let s = Self::new(cfg, events);
        s.turn_on()?;
        Ok(s)
    }

    pub fn name(&self) -> String {
        lock(&self.cfg).name.clone()
    }

    pub fn port(&self) -> u16 {
        lock(&self.cfg).port
    }

    /// Konfigurasi sekarang (termasuk urutan kartu terakhir)
    pub fn cfg(&self) -> IedConfig {
        lock(&self.cfg).clone()
    }

    /// IED yang sedang hidup
    pub fn ied(&self) -> Option<Arc<Ied>> {
        self.ied.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn running(&self) -> Result<Arc<Ied>, String> {
        self.ied().ok_or_else(|| format!("{} sedang mati (hidupkan dulu: stop skenario off)", self.name()))
    }

    /// Status untuk `GET /ieds` & Socket.IO `ieds` (bentuk sama saat IED mati)
    pub fn info(&self) -> Value {
        match self.ied() {
            Some(i) => i.info(),
            None => {
                let c = lock(&self.cfg);
                json!({
                    "name": c.name, "port": c.port, "status": "off", "delay_ms": 0,
                    "scenarios": { "quality": null, "clock": null, "storm": false },
                    "components": c.components.len(), "client_count": 0, "clients": [], "ied_connections": 0,
                    "mode": "idle", "rcbs": [], "last_event": self.events.last_ts(&c.name),
                })
            }
        }
    }

    /// Urutan kartu baru (tanpa restart); disimpan di konfigurasi.
    pub fn set_order(&self, ids: Vec<String>) -> Result<(), String> {
        match self.ied() {
            Some(ied) => ied.set_order(ids.clone())?,
            None => {
                let (mut a, mut b) = (ids.clone(), lock(&self.cfg).components.iter().map(|c| c.id.clone()).collect::<Vec<_>>());
                a.sort();
                b.sort();
                if a != b {
                    return Err("urutan harus berisi semua id komponen tepat sekali".into());
                }
            }
        }
        lock(&self.cfg).order = ids;
        Ok(())
    }

    /// Ganti konfigurasi (nama sama). Tidak me-restart; pemanggil mematikan / menghidupkan sesuai kebutuhan.
    pub fn set_cfg(&self, cfg: IedConfig) {
        if let Some(ied) = self.ied() {
            let _ = ied.set_order(cfg.order.clone()); // model sama: hanya urutan yang mungkin berubah
        }
        *lock(&self.cfg) = cfg;
    }

    /// Buat IedServer dari konfigurasi sekarang dan jalankan.
    pub fn turn_on(&self) -> Result<(), String> {
        let ied = Ied::start(&lock(&self.cfg).clone(), Arc::clone(&self.events))?;
        *self.ied.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(ied));
        Ok(())
    }

    /// Stop + destroy IedServer, tutup listener proxy. true = tadinya hidup.
    pub async fn turn_off(&self) -> bool {
        let _g = self.power.lock().await;
        let Some(ied) = self.ied.write().unwrap_or_else(|e| e.into_inner()).take() else { return false };
        lock(&self.cfg).order = ied.order();
        ied.shutdown().await;
        // IedServer & model dihancurkan saat Arc terakhir lepas (di sini, kecuali trip yang masih berjalan)
        true
    }

    pub async fn run_scenario(&self, name: &str, p: &Value) -> Result<String, String> {
        if name == "off" {
            return self.power(false).await;
        }
        self.running()?.run_scenario(name, p)
    }

    pub async fn stop_scenario(&self, name: &str) -> Result<String, String> {
        if name == "off" {
            return self.power(true).await;
        }
        self.running()?.stop_scenario(name)
    }

    /// Skenario IED mati: stop + destroy IedServer, tutup listener proxy. Hidup: buat ulang dari konfigurasi.
    async fn power(&self, on: bool) -> Result<String, String> {
        let (name, port) = (self.name(), self.port());
        let msg = if on {
            let _g = self.power.lock().await;
            if self.ied().is_some() {
                return Err(format!("{name} sudah hidup"));
            }
            self.turn_on()?;
            format!("{name} hidup lagi: IedServer dibuat ulang (buffer report kosong, nilai awal), port {port}")
        } else {
            if !self.turn_off().await {
                return Err(format!("{name} sudah mati"));
            }
            format!("{name} mati: IedServer di-stop & dihancurkan, port {port} ditutup")
        };
        self.events.push(&name, Ev::Scenario, format!("{} {msg}", if on { "selesai" } else { "mulai" }));
        Ok(msg)
    }
}

/// Hasil menerapkan konfigurasi baru ke daftar slot.
pub struct Applied {
    pub slots: Vec<Arc<Slot>>,
    /// IED yang di-restart / baru dijalankan (UI yang membukanya perlu `components` baru)
    pub restarted: Vec<String>,
    pub removed: Vec<String>,
    pub errors: Vec<String>,
}

/// Terapkan daftar konfigurasi baru (sudah lolos [`model::validate_all`]) ke slot yang ada.
/// IED yang modelnya berubah di-restart, IED yang hilang dimatikan & dibuang, IED baru dijalankan.
/// Semua yang perlu mati dimatikan dulu, supaya port bisa berpindah antar IED.
pub async fn apply(old: &[Arc<Slot>], new: Vec<IedConfig>, events: &Arc<Events>) -> Applied {
    let mut restart = Vec::new();
    let mut removed = Vec::new();
    for s in old {
        match new.iter().find(|c| c.name == s.name()) {
            None => {
                s.turn_off().await;
                removed.push(s.name());
            }
            Some(c) if !model::same_model(&s.cfg(), c) => {
                if s.turn_off().await {
                    restart.push(s.name());
                }
            }
            Some(_) => {}
        }
    }
    let mut out = Applied { slots: Vec::new(), restarted: Vec::new(), removed, errors: Vec::new() };
    for c in new {
        let name = c.name.clone();
        let (slot, start) = match old.iter().find(|s| s.name() == name) {
            Some(s) => {
                s.set_cfg(c);
                (Arc::clone(s), restart.contains(&name))
            }
            None => (Arc::new(Slot::new(c, Arc::clone(events))), true),
        };
        if start {
            match slot.turn_on() {
                Ok(()) => out.restarted.push(name),
                Err(e) => out.errors.push(e), // konfigurasi tetap tersimpan, IED dalam keadaan mati
            }
        }
        out.slots.push(slot);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{control, Conn, CtlVal};
    use crate::model::{Cb, Component, Kind};
    use std::time::Duration;

    fn cfg(name: &str, port: u16, cbs: &[(&str, u32)]) -> IedConfig {
        IedConfig {
            name: name.into(),
            ld: "LD0".into(),
            port,
            components: cbs.iter().map(|(id, i)| Component { id: (*id).into(), kind: Kind::CircuitBreaker(Cb { inst: *i, closed: true, ..Cb::default() }) }).collect(),
            ..Default::default()
        }
    }

    /// IED mati: IedServer dihancurkan, port tertutup; hidup: dibuat ulang (nilai awal), kontrol jalan.
    #[tokio::test(flavor = "multi_thread")]
    async fn mati_dan_hidup_lagi() {
        let events = Arc::new(Events::new());
        let slot = Slot::start(cfg("PW", 10141, &[("cb1", 1)]), Arc::clone(&events)).unwrap();
        let ctl = |v| {
            std::thread::spawn(move || control(&Conn::connect("127.0.0.1", 10141)?, "PWLD0/CSWI1.Pos", CtlVal::Bool(v), false, Duration::from_secs(3)))
                .join()
                .unwrap()
        };
        ctl(false).unwrap(); // open
        slot.set_order(vec!["cb1".into()]).unwrap();

        slot.run_scenario("off", &json!({})).await.unwrap();
        assert_eq!(slot.info()["status"], "off");
        assert!(std::net::TcpStream::connect("127.0.0.1:10141").is_err(), "port harus tertutup");
        assert!(slot.run_scenario("hang", &json!({})).await.unwrap_err().contains("mati"));
        assert!(slot.run_scenario("off", &json!({})).await.is_err());

        slot.stop_scenario("off").await.unwrap();
        let ied = slot.running().unwrap();
        assert_eq!(ied.live_values()[0]["pos"], "close", "IED dibuat ulang: posisi kembali ke konfigurasi awal");
        ctl(false).unwrap();
        assert!(slot.stop_scenario("off").await.is_err());
        let sc: Vec<String> = events.query(Some("PW"), Some(Ev::Scenario), 10).into_iter().map(|e| e.msg).collect();
        assert!(sc[0].contains("dihancurkan") && sc[1].contains("dibuat ulang"), "{sc:?}");
    }

    /// Ubah konfigurasi: hanya IED yang modelnya berubah yang restart; tukar port antar IED; hapus & tambah.
    #[tokio::test(flavor = "multi_thread")]
    async fn terapkan_konfigurasi() {
        let events = Arc::new(Events::new());
        let a = apply(&[], vec![cfg("A", 10151, &[("cb1", 1)]), cfg("B", 10152, &[("cb1", 1)])], &events).await;
        assert!(a.errors.is_empty(), "{:?}", a.errors);
        let ied_b = a.slots[1].ied().unwrap();

        // A: tambah CB + tukar port dengan B; B: port berubah; C baru
        let mut ca = cfg("A", 10152, &[("cb1", 1), ("cb2", 2)]);
        ca.order = vec!["cb2".into(), "cb1".into()];
        let mut cb = cfg("B", 10151, &[("cb1", 1)]);
        cb.order = vec!["cb1".into()];
        let b = apply(&a.slots, vec![ca, cb.clone(), cfg("C", 10153, &[])], &events).await;
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        assert_eq!(b.restarted, vec!["A".to_string(), "B".to_string(), "C".to_string()], "port B berubah -> restart");
        assert_eq!(b.slots[0].ied().unwrap().components_json()["components"][0]["id"], "cb2", "urutan tersimpan dipakai");
        assert!(!Arc::ptr_eq(&ied_b, &b.slots[1].ied().unwrap()));

        // hanya urutan / tanpa perubahan: tidak restart
        let ied_b = b.slots[1].ied().unwrap();
        let c = apply(&b.slots, vec![b.slots[0].cfg(), cb, b.slots[2].cfg()], &events).await;
        assert!(c.restarted.is_empty() && Arc::ptr_eq(&ied_b, &c.slots[1].ied().unwrap()));

        // hapus C; tambah D di port yang dipakai proses lain -> konfigurasi tersimpan tapi IED mati
        let _blocker = std::net::TcpListener::bind("0.0.0.0:10154").unwrap();
        let d = apply(&c.slots, vec![c.slots[0].cfg(), c.slots[1].cfg(), cfg("D", 10154, &[])], &events).await;
        assert_eq!(d.removed, vec!["C".to_string()]);
        assert_eq!(d.errors.len(), 1);
        assert_eq!(d.slots[2].info()["status"], "off");
        assert!(std::net::TcpStream::connect("127.0.0.1:10153").is_err(), "C sudah dihapus");
    }
}
