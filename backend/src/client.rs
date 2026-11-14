//! Client kontrol MMS kecil (libIEC61850): dipakai test & perintah `ctl` untuk mencoba kontrol tanpa FEP.
//! Alurnya sama dengan FEP: select (SBO) -> operate -> tunggu CommandTermination (enhanced).

use std::ffi::{c_void, CString};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::ffi;

pub struct Conn(pub(crate) ffi::IedConnection);

impl Conn {
    pub fn connect(host: &str, port: u16) -> Result<Self, String> {
        let h = CString::new(host).map_err(|e| e.to_string())?;
        // SAFETY: koneksi dibuat & dimiliki Conn; di-destroy di Drop
        unsafe {
            let con = ffi::IedConnection_create();
            let mut err = 0;
            ffi::IedConnection_connect(con, &mut err, h.as_ptr(), port.into());
            if err != ffi::IED_ERROR_OK {
                ffi::IedConnection_destroy(con);
                return Err(format!("gagal konek ke {host}:{port} (IedClientError {err})"));
            }
            Ok(Self(con))
        }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        // SAFETY: koneksi valid
        unsafe {
            ffi::IedConnection_close(self.0);
            ffi::IedConnection_destroy(self.0);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum CtlVal {
    /// DPC/SPC: true = close/on
    Bool(bool),
    /// BSC: true = higher (raise), false = lower
    Step(bool),
}

impl CtlVal {
    pub fn parse(s: &str) -> Result<Self, String> {
        Ok(match s {
            "close" | "on" | "true" => Self::Bool(true),
            "open" | "off" | "false" => Self::Bool(false),
            "raise" | "higher" => Self::Step(true),
            "lower" => Self::Step(false),
            _ => return Err(format!("nilai '{s}' tidak dikenal (close/open/true/false/raise/lower)")),
        })
    }
}

#[derive(Default)]
struct Termination {
    result: Mutex<Option<bool>>,
    cv: Condvar,
}

unsafe extern "C" fn on_termination(p: *mut c_void, ctl: ffi::ControlObjectClient) {
    // SAFETY: p = &Termination yang hidup selama ControlObjectClient
    let t = unsafe { &*(p as *const Termination) };
    let e = unsafe { ffi::ControlObjectClient_getLastApplError(ctl) };
    if let Ok(mut r) = t.result.lock() {
        *r = Some(e.error == ffi::CONTROL_ERROR_NO_ERROR && e.addCause == ffi::ADD_CAUSE_UNKNOWN);
    }
    t.cv.notify_all();
}

/// Nama AddCause (IEC 61850-7-2) untuk pesan error.
pub fn add_cause_name(c: ffi::ControlAddCause) -> String {
    let s = match c {
        ffi::ADD_CAUSE_UNKNOWN => "unknown",
        ffi::ADD_CAUSE_NOT_SUPPORTED => "not-supported",
        ffi::ADD_CAUSE_BLOCKED_BY_SWITCHING_HIERARCHY => "blocked-by-switching-hierarchy",
        ffi::ADD_CAUSE_SELECT_FAILED => "select-failed",
        ffi::ADD_CAUSE_INVALID_POSITION => "invalid-position",
        ffi::ADD_CAUSE_POSITION_REACHED => "position-reached",
        ffi::ADD_CAUSE_STEP_LIMIT => "step-limit",
        ffi::ADD_CAUSE_BLOCKED_BY_MODE => "blocked-by-mode",
        ffi::ADD_CAUSE_BLOCKED_BY_INTERLOCKING => "blocked-by-interlocking",
        ffi::ADD_CAUSE_COMMAND_ALREADY_IN_EXECUTION => "command-already-in-execution",
        ffi::ADD_CAUSE_OBJECT_NOT_SELECTED => "object-not-selected",
        ffi::ADD_CAUSE_OBJECT_ALREADY_SELECTED => "object-already-selected",
        ffi::ADD_CAUSE_INCONSISTENT_PARAMETERS => "inconsistent-parameters",
        n => return format!("addCause {n}"),
    };
    s.into()
}

/// Kontrol satu DO (`SIMIED1LD0/CSWI1.Pos`). Ok berisi ringkasan, Err berisi alasan (termasuk AddCause).
pub fn control(conn: &Conn, reference: &str, val: CtlVal, interlock: bool, wait: Duration) -> Result<String, String> {
    let r = CString::new(reference).map_err(|e| e.to_string())?;
    let ident = CString::new("simctl").expect("tanpa NUL");
    let term = Box::<Termination>::default();
    // SAFETY: semua objek dibuat di sini dan dihancurkan sebelum return; term hidup lebih lama dari ctl
    unsafe {
        let ctl = ffi::ControlObjectClient_create(r.as_ptr(), conn.0);
        if ctl.is_null() {
            return Err(format!("{reference} bukan objek kontrol (tidak ada / status-only?)"));
        }
        ffi::ControlObjectClient_setCommandTerminationHandler(ctl, Some(on_termination), &*term as *const _ as *mut c_void);
        ffi::ControlObjectClient_setOrigin(ctl, ident.as_ptr(), ffi::CONTROL_ORCAT_REMOTE_CONTROL as i32);
        ffi::ControlObjectClient_setInterlockCheck(ctl, interlock);
        let v = match val {
            CtlVal::Bool(b) => ffi::MmsValue_newBoolean(b),
            CtlVal::Step(up) => ffi::Dbpos_toMmsValue(std::ptr::null_mut(), if up { ffi::DBPOS_ON } else { ffi::DBPOS_OFF }),
        };
        let model = ffi::ControlObjectClient_getControlModel(ctl);
        let fail = |what: &str| {
            let e = ffi::ControlObjectClient_getLastApplError(ctl);
            format!("{what} ditolak: {}", add_cause_name(e.addCause))
        };
        let res = (|| {
            let selected = match model {
                ffi::CONTROL_MODEL_SBO_NORMAL => ffi::ControlObjectClient_select(ctl),
                ffi::CONTROL_MODEL_SBO_ENHANCED => ffi::ControlObjectClient_selectWithValue(ctl, v),
                ffi::CONTROL_MODEL_STATUS_ONLY => return Err("ctlModel status-only".into()),
                _ => true,
            };
            if !selected {
                return Err(fail("select"));
            }
            if !ffi::ControlObjectClient_operate(ctl, v, 0) {
                return Err(fail("operate"));
            }
            if !matches!(model, ffi::CONTROL_MODEL_DIRECT_ENHANCED | ffi::CONTROL_MODEL_SBO_ENHANCED) {
                return Ok(format!("operate ok (ctlModel {model})"));
            }
            let g = term.result.lock().map_err(|_| "lock rusak")?;
            let (g, _) = term.cv.wait_timeout_while(g, wait, |r| r.is_none()).map_err(|_| "lock rusak")?;
            match *g {
                Some(true) => Ok(format!("operate ok, CommandTermination+ (ctlModel {model})")),
                Some(false) => Err(fail("CommandTermination-")),
                None => Err("CommandTermination tidak diterima (timeout)".into()),
            }
        })();
        ffi::MmsValue_delete(v);
        ffi::ControlObjectClient_destroy(ctl);
        res
    }
}
