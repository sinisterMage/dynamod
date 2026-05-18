//! pam_dynamod_logind.so
//!
//! Session-management PAM module. Mirrors what `pam_systemd.so` does:
//! on session-open, call `org.freedesktop.login1.Manager.CreateSession` and
//! propagate XDG_* environment variables; on session-close, call
//! `Manager.ReleaseSession`.
//!
//! Without this module, nothing in the dynamod stack ever creates login1
//! sessions, so libsystemd's `sd_pid_get_session()` (which KWin and other
//! KDE components rely on for DRM master access) always returns ENOENT.
//!
//! Build & install:
//!   cargo build --release -p pam_dynamod_logind --target x86_64-unknown-linux-gnu
//!   install -m 0755 libpam_dynamod_logind.so /usr/lib/security/pam_dynamod_logind.so
//!
//! Drop into PAM stacks (e.g. `/etc/pam.d/sddm`, `/etc/pam.d/login`):
//!   session   optional   pam_dynamod_logind.so
//!
//! We are NOT an authentication module — `pam_sm_authenticate` and
//! `pam_sm_setcred` always return PAM_SUCCESS so we can be safely chained
//! into existing auth stacks without altering authentication semantics.

#![allow(
    non_camel_case_types,
    unsafe_op_in_unsafe_fn,
    clippy::missing_safety_doc
)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;

mod pam {
    use super::{c_char, c_int, c_void};

    pub const PAM_SUCCESS: c_int = 0;
    pub const PAM_IGNORE: c_int = 25;

    pub const PAM_USER: c_int = 2;
    pub const PAM_TTY: c_int = 3;
    pub const PAM_RHOST: c_int = 4;
    pub const PAM_RUSER: c_int = 8;
    pub const PAM_SERVICE: c_int = 1;
    pub const PAM_XDISPLAY: c_int = 11;

    #[repr(C)]
    pub struct pam_handle_t {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        pub fn pam_get_item(
            pamh: *mut pam_handle_t,
            item_type: c_int,
            item: *mut *const c_void,
        ) -> c_int;

        pub fn pam_putenv(pamh: *mut pam_handle_t, name_value: *const c_char) -> c_int;

        pub fn pam_getenv(pamh: *mut pam_handle_t, name: *const c_char) -> *const c_char;

        pub fn pam_syslog(
            pamh: *mut pam_handle_t,
            priority: c_int,
            fmt: *const c_char,
            ...
        );
    }
}

// ---------------- PAM hook implementations ----------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_authenticate(
    _pamh: *mut pam::pam_handle_t,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    pam::PAM_IGNORE
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_setcred(
    _pamh: *mut pam::pam_handle_t,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    pam::PAM_SUCCESS
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_acct_mgmt(
    _pamh: *mut pam::pam_handle_t,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    pam::PAM_IGNORE
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_open_session(
    pamh: *mut pam::pam_handle_t,
    _flags: c_int,
    argc: c_int,
    argv: *const *const c_char,
) -> c_int {
    let args = parse_args(argc, argv);
    match do_open_session(pamh, &args) {
        Ok(()) => pam::PAM_SUCCESS,
        Err(e) => {
            log_err(pamh, &format!("open_session: {e}"));
            // Optional module: never block login on our failures.
            pam::PAM_SUCCESS
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_close_session(
    pamh: *mut pam::pam_handle_t,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    if let Err(e) = do_close_session(pamh) {
        log_err(pamh, &format!("close_session: {e}"));
    }
    pam::PAM_SUCCESS
}

// ---------------- Implementation ----------------

#[derive(Default)]
struct ModuleArgs {
    class: Option<String>,
    type_: Option<String>,
    desktop: Option<String>,
}

unsafe fn parse_args(argc: c_int, argv: *const *const c_char) -> ModuleArgs {
    let mut out = ModuleArgs::default();
    if argv.is_null() {
        return out;
    }
    for i in 0..argc {
        let p = *argv.offset(i as isize);
        if p.is_null() {
            continue;
        }
        let Ok(s) = CStr::from_ptr(p).to_str() else {
            continue;
        };
        if let Some(v) = s.strip_prefix("class=") {
            out.class = Some(v.to_string());
        } else if let Some(v) = s.strip_prefix("type=") {
            out.type_ = Some(v.to_string());
        } else if let Some(v) = s.strip_prefix("desktop=") {
            out.desktop = Some(v.to_string());
        }
    }
    out
}

unsafe fn do_open_session(
    pamh: *mut pam::pam_handle_t,
    args: &ModuleArgs,
) -> Result<(), String> {
    let user = get_item(pamh, pam::PAM_USER)?.ok_or("no PAM_USER")?;
    let service = get_item(pamh, pam::PAM_SERVICE)?.unwrap_or_default();
    let tty = get_item(pamh, pam::PAM_TTY)?.unwrap_or_default();
    let rhost = get_item(pamh, pam::PAM_RHOST)?.unwrap_or_default();
    let ruser = get_item(pamh, pam::PAM_RUSER)?.unwrap_or_default();
    let display = get_item(pamh, pam::PAM_XDISPLAY)?.unwrap_or_default();

    let uid = lookup_uid(&user).ok_or_else(|| format!("no passwd entry for '{user}'"))?;

    let (vtnr, tty_clean, display_out) = classify_tty(&tty, &display);

    let remote = !rhost.is_empty() && rhost != "localhost";
    let seat = if remote || vtnr == 0 { String::new() } else { "seat0".to_string() };

    let session_type = args
        .type_
        .clone()
        .unwrap_or_else(|| infer_type(&display_out, &tty_clean));
    let class = args.class.clone().unwrap_or_else(|| "user".to_string());
    let desktop = args.desktop.clone().unwrap_or_default();

    let conn = zbus::blocking::Connection::system()
        .map_err(|e| format!("system bus: {e}"))?;

    let pid = libc::getpid() as u32;

    // Manager.CreateSession signature: uusssssussbss → susoshb
    // Return: (id, object_path, runtime_path, seat_id, vtnr, existing)
    let body = (
        uid,
        pid,
        service.as_str(),
        session_type.as_str(),
        class.as_str(),
        desktop.as_str(),
        seat.as_str(),
        vtnr,
        tty_clean.as_str(),
        display_out.as_str(),
        remote,
        ruser.as_str(),
        rhost.as_str(),
    );

    let reply = conn
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "CreateSession",
            &body,
        )
        .map_err(|e| format!("CreateSession: {e}"))?;

    let (session_id, _path, runtime_path, seat_out, vtnr_out, _existing): (
        String,
        zbus::zvariant::OwnedObjectPath,
        String,
        String,
        u32,
        bool,
    ) = reply
        .body()
        .deserialize()
        .map_err(|e| format!("decode reply: {e}"))?;

    set_pam_env(pamh, "XDG_SESSION_ID", &session_id);
    set_pam_env(pamh, "XDG_RUNTIME_DIR", &runtime_path);
    if !seat_out.is_empty() {
        set_pam_env(pamh, "XDG_SEAT", &seat_out);
    }
    if vtnr_out > 0 {
        set_pam_env(pamh, "XDG_VTNR", &vtnr_out.to_string());
    }
    set_pam_env(pamh, "XDG_SESSION_TYPE", &session_type);
    set_pam_env(pamh, "XDG_SESSION_CLASS", &class);
    if !desktop.is_empty() {
        set_pam_env(pamh, "XDG_SESSION_DESKTOP", &desktop);
    }
    Ok(())
}

unsafe fn do_close_session(pamh: *mut pam::pam_handle_t) -> Result<(), String> {
    let session_id = match get_env(pamh, "XDG_SESSION_ID") {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(()),
    };

    let conn = zbus::blocking::Connection::system()
        .map_err(|e| format!("system bus: {e}"))?;

    let _ = conn.call_method(
        Some("org.freedesktop.login1"),
        "/org/freedesktop/login1",
        Some("org.freedesktop.login1.Manager"),
        "ReleaseSession",
        &(session_id.as_str(),),
    );
    Ok(())
}

unsafe fn get_item(
    pamh: *mut pam::pam_handle_t,
    item: c_int,
) -> Result<Option<String>, String> {
    let mut p: *const c_void = ptr::null();
    let rc = pam::pam_get_item(pamh, item, &mut p);
    if rc != pam::PAM_SUCCESS {
        return Err(format!("pam_get_item({item}) = {rc}"));
    }
    if p.is_null() {
        return Ok(None);
    }
    let cs = CStr::from_ptr(p as *const c_char);
    Ok(Some(cs.to_string_lossy().into_owned()))
}

unsafe fn set_pam_env(pamh: *mut pam::pam_handle_t, name: &str, value: &str) {
    let Ok(kv) = CString::new(format!("{name}={value}")) else {
        return;
    };
    let _ = pam::pam_putenv(pamh, kv.as_ptr());
}

unsafe fn get_env(pamh: *mut pam::pam_handle_t, name: &str) -> Option<String> {
    let Ok(cname) = CString::new(name) else {
        return None;
    };
    let p = pam::pam_getenv(pamh, cname.as_ptr());
    if p.is_null() {
        return None;
    }
    Some(CStr::from_ptr(p).to_string_lossy().into_owned())
}

unsafe fn log_err(pamh: *mut pam::pam_handle_t, msg: &str) {
    if let Ok(cmsg) = CString::new(msg) {
        let fmt = CString::new("%s").unwrap();
        pam::pam_syslog(pamh, libc::LOG_WARNING, fmt.as_ptr(), cmsg.as_ptr());
    }
}

fn lookup_uid(user: &str) -> Option<u32> {
    let cname = CString::new(user).ok()?;
    unsafe {
        let pw = libc::getpwnam(cname.as_ptr());
        if pw.is_null() {
            None
        } else {
            Some((*pw).pw_uid)
        }
    }
}

/// Decide vtnr / tty / display values to pass to CreateSession.
///
/// PAM_TTY can be "tty1", ":0", "/dev/pts/2", or empty (sshd). We need to
/// disambiguate because login1 keys VT allocation off vtnr.
fn classify_tty(tty: &str, display: &str) -> (u32, String, String) {
    if tty.starts_with(':') {
        // X display like ":0"
        return (0, String::new(), tty.to_string());
    }
    let tty_clean = tty.trim_start_matches("/dev/").to_string();
    let vtnr = if let Some(rest) = tty_clean.strip_prefix("tty") {
        rest.parse::<u32>().unwrap_or(0)
    } else {
        0
    };
    (vtnr, tty_clean, display.to_string())
}

fn infer_type(display: &str, tty: &str) -> String {
    if !display.is_empty() {
        return "x11".to_string();
    }
    if tty.is_empty() {
        return "unspecified".to_string();
    }
    "tty".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tty_classifier_handles_common_inputs() {
        assert_eq!(classify_tty("tty1", ""), (1, "tty1".into(), "".into()));
        assert_eq!(classify_tty("tty7", ""), (7, "tty7".into(), "".into()));
        assert_eq!(classify_tty("/dev/tty2", ""), (2, "tty2".into(), "".into()));
        assert_eq!(classify_tty(":0", ""), (0, "".into(), ":0".into()));
        assert_eq!(classify_tty("", ":1"), (0, "".into(), ":1".into()));
        // pts: no vt
        assert_eq!(
            classify_tty("pts/3", ""),
            (0, "pts/3".into(), "".into())
        );
    }

    #[test]
    fn type_inference() {
        assert_eq!(infer_type(":0", "tty1"), "x11");
        assert_eq!(infer_type("", "tty1"), "tty");
        assert_eq!(infer_type("", ""), "unspecified");
    }
}
