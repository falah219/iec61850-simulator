//! Log kejadian (PLAN 7.3): ring buffer di memori + broadcast real-time ke UI + log backend.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::{debug, info};

/// Kapasitas ring buffer (PLAN: default 5.000 kejadian)
pub const CAP: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// client konek / putus
    Connection,
    /// RCB enable / disable / GI / overflow
    Rcb,
    /// select / operate / cancel dari client
    Control,
    /// report terkirim, dikelompokkan per detik
    Report,
    /// perubahan nilai manual dari UI / API
    Manual,
    /// skenario mulai / selesai
    Scenario,
    /// perubahan konfigurasi (IED / komponen / import)
    Config,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::Rcb => "rcb",
            Self::Control => "control",
            Self::Report => "report",
            Self::Manual => "manual",
            Self::Scenario => "scenario",
            Self::Config => "config",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Connection, Self::Rcb, Self::Control, Self::Report, Self::Manual, Self::Scenario, Self::Config].into_iter().find(|k| k.as_str() == s)
    }
}

#[derive(Debug, Clone)]
pub struct Event {
    pub id: u64,
    pub ts_ms: u64,
    pub ied: String,
    pub kind: Kind,
    pub msg: String,
}

impl Event {
    pub fn json(&self) -> Value {
        json!({ "id": self.id, "ts": self.ts_ms, "ied": self.ied, "type": self.kind.as_str(), "msg": self.msg })
    }
}

pub struct Events {
    /// (id terakhir, isi)
    buf: Mutex<(u64, VecDeque<Event>)>,
    tx: broadcast::Sender<Event>,
}

impl Events {
    pub fn new() -> Self {
        Self { buf: Mutex::new((0, VecDeque::new())), tx: broadcast::channel(1024).0 }
    }

    /// Catat kejadian. Aman dipanggil dari thread libIEC61850 (handler C).
    pub fn push(&self, ied: &str, kind: Kind, msg: String) {
        if kind == Kind::Report {
            debug!("[{ied}] {} {msg}", kind.as_str()); // ramai, tidak dicetak di level info
        } else {
            info!("[{ied}] {} {msg}", kind.as_str());
        }
        let Ok(mut b) = self.buf.lock() else { return };
        b.0 += 1;
        let ev = Event { id: b.0, ts_ms: crate::server::now_ms(), ied: ied.into(), kind, msg };
        b.1.push_back(ev.clone());
        if b.1.len() > CAP {
            b.1.pop_front();
        }
        let _ = self.tx.send(ev); // tidak ada subscriber = tidak apa-apa
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// `limit` kejadian terbaru yang cocok filter, urut lama -> baru.
    pub fn query(&self, ied: Option<&str>, kind: Option<Kind>, limit: usize) -> Vec<Event> {
        let Ok(b) = self.buf.lock() else { return Vec::new() };
        let mut v: Vec<Event> = b
            .1
            .iter()
            .rev()
            .filter(|e| ied.is_none_or(|i| e.ied == i) && kind.is_none_or(|k| e.kind == k))
            .take(limit)
            .cloned()
            .collect();
        v.reverse();
        v
    }

    /// Waktu kejadian terakhir satu IED (ms epoch)
    pub fn last_ts(&self, ied: &str) -> Option<u64> {
        self.buf.lock().ok()?.1.iter().rev().find(|e| e.ied == ied).map(|e| e.ts_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_dan_filter() {
        let ev = Events::new();
        for i in 0..CAP + 10 {
            let kind = if i % 2 == 0 { Kind::Control } else { Kind::Connection };
            ev.push(if i % 3 == 0 { "A" } else { "B" }, kind, format!("e{i}"));
        }
        assert_eq!(ev.query(None, None, usize::MAX).len(), CAP);
        let last = ev.query(None, None, 2);
        assert_eq!((last[0].msg.as_str(), last[1].msg.as_str()), ("e5008", "e5009"));
        assert!(ev.query(Some("A"), Some(Kind::Control), 50).iter().all(|e| e.ied == "A" && e.kind == Kind::Control));
        assert_eq!(Kind::parse("rcb"), Some(Kind::Rcb));
        assert!(ev.last_ts("A").is_some() && ev.last_ts("X").is_none());
    }
}
