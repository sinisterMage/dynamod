/// Well-known filesystem paths for dynamod components.

/// Runtime directory for sockets and state.
pub const RUN_DIR: &str = "/run/dynamod";

/// Control socket path for dynamodctl <-> svmgr communication.
pub const CONTROL_SOCK: &str = "/run/dynamod/control.sock";

/// Service configuration directory.
pub const SERVICES_DIR: &str = "/etc/dynamod/services";

/// Supervisor configuration directory.
pub const SUPERVISORS_DIR: &str = "/etc/dynamod/supervisors";

/// Environment variable name for the init socket fd.
pub const INIT_FD_ENV: &str = "DYNAMOD_INIT_FD";
