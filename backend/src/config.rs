//! Konfigurasi dari env (lihat backend/.env.example).

use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    /// Port HTTP API + Socket.IO
    pub http_port: u16,
    /// Jumlah IED contoh yang dibuat saat start (SIMIED1..N)
    pub ied_count: u16,
    /// Prefix nama IED: `<prefix><n>`, mis. SIMIED1 -> domain SIMIED1LD0
    pub ied_prefix: String,
    /// Nama LD (default LD0)
    pub ld_name: String,
    /// Port MMS IED pertama (default 102)
    pub mms_port: u16,
    /// Port MMS IED ke-2 dst: base, base+1, ... (default 10102)
    pub mms_port_base: u16,
    /// File JSON konfigurasi (dimuat saat start, disimpan tiap perubahan). None = tanpa autosave
    pub autosave: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let num = |k: &str, default: u16| match env::var(k) {
            Ok(v) => v.trim().parse().map_err(|_| format!("{k} tidak valid: {v}")),
            Err(_) => Ok(default),
        };
        let c = Self {
            http_port: num("HTTP_PORT", 6061)?,
            ied_count: num("IED_COUNT", 1)?,
            ied_prefix: env::var("IED_PREFIX").unwrap_or_else(|_| "SIMIED".into()),
            ld_name: env::var("LD_NAME").unwrap_or_else(|_| "LD0".into()),
            mms_port: num("MMS_PORT", 102)?,
            mms_port_base: num("MMS_PORT_BASE", 10102)?,
            autosave: env::var("AUTOSAVE_PATH").ok().filter(|p| !p.trim().is_empty()),
        };
        if c.ied_count == 0 {
            return Err("IED_COUNT minimal 1".into());
        }
        Ok(c)
    }

    /// Port MMS IED ke-n (1-based): 102, 10102, 10103, ...
    pub fn port_of(&self, n: u16) -> Result<u16, String> {
        if n == 1 {
            return Ok(self.mms_port);
        }
        self.mms_port_base.checked_add(n - 2).ok_or_else(|| format!("port IED ke-{n} melebihi 65535"))
    }
}
