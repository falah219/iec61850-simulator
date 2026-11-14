//! Konfigurasi dari env (lihat backend/.env.example).

use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    /// Port HTTP API + Socket.IO
    pub http_port: u16,
    /// Nama IED (prefix domain MMS, mis. SIMIED1 -> SIMIED1LD0)
    pub ied_name: String,
    /// Nama LD (default LD0)
    pub ld_name: String,
    /// Port MMS IED
    pub mms_port: u16,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let port = |k: &str, default: u16| match env::var(k) {
            Ok(v) => v.trim().parse().map_err(|_| format!("{k} tidak valid: {v}")),
            Err(_) => Ok(default),
        };
        Ok(Self {
            http_port: port("HTTP_PORT", 6061)?,
            ied_name: env::var("IED_NAME").unwrap_or_else(|_| "SIMIED1".into()),
            ld_name: env::var("LD_NAME").unwrap_or_else(|_| "LD0".into()),
            mms_port: port("MMS_PORT", 102)?,
        })
    }
}
