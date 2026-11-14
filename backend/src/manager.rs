//! Daftar IED. Satu [`Slot`] per IED menyimpan konfigurasinya, sehingga IED bisa dimatikan
//! (IedServer + model dihancurkan) dan dihidupkan lagi (dibuat ulang dari konfigurasi, seperti reboot:
//! buffer report kosong, nilai kembali ke awal). Dasar untuk restart otomatis di fase 8.

use std::sync::{Arc, Mutex, RwLock};

use serde_json::{json, Value};

use crate::events::{Events, Kind as Ev};
use crate::model::IedConfig;
use crate::server::Ied;

pub struct Slot {
    pub name: String,
    pub port: u16,
    cfg: IedConfig,
    events: Arc<Events>,
    /// None = IED mati
    ied: RwLock<Option<Arc<Ied>>>,
    /// urutan kartu UI, dibawa melewati mati / hidup
    order: Mutex<Option<Vec<String>>>,
    /// mati / hidup tidak boleh berjalan bersamaan
    power: tokio::sync::Mutex<()>,
}

impl Slot {
    /// Validasi + jalankan IED. Harus dipanggil di dalam runtime tokio.
    pub fn start(cfg: IedConfig, events: Arc<Events>) -> Result<Self, String> {
        let ied = Ied::start(&cfg, Arc::clone(&events))?;
        Ok(Self {
            name: cfg.name.clone(),
            port: cfg.port,
            ied: RwLock::new(Some(Arc::new(ied))),
            cfg,
            events,
            order: Mutex::new(None),
            power: tokio::sync::Mutex::new(()),
        })
    }

    /// IED yang sedang hidup
    pub fn ied(&self) -> Option<Arc<Ied>> {
        self.ied.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn running(&self) -> Result<Arc<Ied>, String> {
        self.ied().ok_or_else(|| format!("{} sedang mati (hidupkan dulu: stop skenario off)", self.name))
    }

    /// Status untuk `GET /ieds` & Socket.IO `ieds` (bentuk sama saat IED mati)
    pub fn info(&self) -> Value {
        match self.ied() {
            Some(i) => i.info(),
            None => json!({
                "name": self.name, "port": self.port, "status": "off", "delay_ms": 0,
                "scenarios": { "quality": null, "clock": null, "storm": false },
                "components": self.cfg.components.len(), "client_count": 0, "clients": [], "ied_connections": 0,
                "mode": "idle", "rcbs": [], "last_event": self.events.last_ts(&self.name),
            }),
        }
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
        let _g = self.power.lock().await;
        let msg = if on {
            if self.ied().is_some() {
                return Err(format!("{} sudah hidup", self.name));
            }
            let ied = Ied::start(&self.cfg, Arc::clone(&self.events))?;
            if let Some(o) = self.order.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = ied.set_order(o);
            }
            *self.ied.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(ied));
            format!("{} hidup lagi: IedServer dibuat ulang (buffer report kosong, nilai awal), port {}", self.name, self.port)
        } else {
            let ied = self.ied.write().unwrap_or_else(|e| e.into_inner()).take().ok_or_else(|| format!("{} sudah mati", self.name))?;
            *self.order.lock().unwrap_or_else(|e| e.into_inner()) = Some(ied.order());
            ied.shutdown().await;
            // IedServer & model dihancurkan saat Arc terakhir lepas (di sini, kecuali trip yang masih berjalan)
            drop(ied);
            format!("{} mati: IedServer di-stop & dihancurkan, port {} ditutup", self.name, self.port)
        };
        self.events.push(&self.name, Ev::Scenario, format!("{} {msg}", if on { "selesai" } else { "mulai" }));
        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{control, Conn, CtlVal};
    use crate::model::{Cb, Component, Kind};
    use std::time::Duration;

    /// IED mati: IedServer dihancurkan, port tertutup; hidup: dibuat ulang (nilai awal), kontrol jalan.
    #[tokio::test(flavor = "multi_thread")]
    async fn mati_dan_hidup_lagi() {
        let events = Arc::new(Events::new());
        let cfg = IedConfig {
            name: "PW".into(),
            ld: "LD0".into(),
            port: 10141,
            components: vec![Component { id: "cb1".into(), kind: Kind::CircuitBreaker(Cb { inst: 1, closed: true, ..Cb::default() }) }],
        };
        let slot = Slot::start(cfg, Arc::clone(&events)).unwrap();
        let ctl = |v| {
            std::thread::spawn(move || control(&Conn::connect("127.0.0.1", 10141)?, "PWLD0/CSWI1.Pos", CtlVal::Bool(v), false, Duration::from_secs(3)))
                .join()
                .unwrap()
        };
        ctl(false).unwrap(); // open
        slot.running().unwrap().set_order(vec!["cb1".into()]).unwrap();

        slot.run_scenario("off", &json!({})).await.unwrap();
        assert_eq!(slot.info()["status"], "off");
        assert!(std::net::TcpStream::connect("127.0.0.1:10141").is_err(), "port harus tertutup");
        assert!(slot.run_scenario("hang", &json!({})).await.unwrap_err().contains("mati"));
        assert!(slot.run_scenario("off", &json!({})).await.is_err());

        slot.stop_scenario("off").await.unwrap();
        let ied = slot.running().unwrap();
        let pos = ied.live_values()[0]["pos"].clone();
        assert_eq!(pos, "close", "IED dibuat ulang: posisi kembali ke konfigurasi awal");
        ctl(false).unwrap();
        assert!(slot.stop_scenario("off").await.is_err());
        let sc: Vec<String> = events.query(Some("PW"), Some(Ev::Scenario), 10).into_iter().map(|e| e.msg).collect();
        assert!(sc[0].contains("dihancurkan") && sc[1].contains("dibuat ulang"), "{sc:?}");
    }
}
