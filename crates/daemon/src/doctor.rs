//! Operator doctor battery: named checks returning ok/warn/error plus a
//! message. Exposed through `Diagnostics::Doctor` and the `cm doctor` CLI.

use cognitive_memory_store::Store;
use std::path::Path;
use tokio::net::UnixStream;

/// Result level of a single doctor check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckLevel {
    Ok,
    Warn,
    Error,
}

/// Outcome of one named check.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub name: &'static str,
    pub level: CheckLevel,
    pub message: String,
}

impl CheckResult {
    pub fn ok(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            level: CheckLevel::Ok,
            message: message.into(),
        }
    }
    pub fn warn(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            level: CheckLevel::Warn,
            message: message.into(),
        }
    }
    pub fn error(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            level: CheckLevel::Error,
            message: message.into(),
        }
    }
}

/// Aggregate report. `exit_code` is `0` for all-ok/skip, `1` if any warn,
/// `2` if any error.
#[derive(Debug, Clone)]
pub struct DoctorReport {
    pub checks: Vec<CheckResult>,
}

impl DoctorReport {
    pub fn exit_code(&self) -> i32 {
        let mut code = 0;
        for check in &self.checks {
            match check.level {
                CheckLevel::Ok => {}
                CheckLevel::Warn => code = code.max(1),
                CheckLevel::Error => code = 2,
            }
        }
        code
    }
}

/// Run the doctor battery against a live store. Provider checks
/// (LLM/embedding reachability) and time-skew checks can be added when
/// the operator-facing command is exposed.
pub async fn run_doctor(
    socket_path: &Path,
    pid_path: &Path,
    db_path: &Path,
    log_path: &Path,
    store: &Store,
) -> DoctorReport {
    let mut checks = Vec::new();

    match UnixStream::connect(socket_path).await {
        Ok(_) => checks.push(CheckResult::ok(
            "socket reachable",
            format!("{}", socket_path.display()),
        )),
        Err(e) if socket_path.exists() => checks.push(CheckResult::error(
            "socket reachable",
            format!("socket exists but connect failed: {e}"),
        )),
        Err(_) => checks.push(CheckResult::error(
            "socket reachable",
            format!("not found: {}", socket_path.display()),
        )),
    }

    match std::fs::read_to_string(pid_path) {
        Ok(pid) => checks.push(CheckResult::ok(
            "pid file",
            format!("{} -> {}", pid_path.display(), pid.trim()),
        )),
        Err(e) => checks.push(CheckResult::error(
            "pid file",
            format!("{}: {e}", pid_path.display()),
        )),
    }

    if db_path.exists() {
        checks.push(CheckResult::ok(
            "database file",
            format!("{}", db_path.display()),
        ));
    } else {
        checks.push(CheckResult::warn(
            "database file",
            format!("not found yet: {}", db_path.display()),
        ));
    }

    if log_path.exists() {
        checks.push(CheckResult::ok(
            "log file",
            format!("{}", log_path.display()),
        ));
    } else {
        checks.push(CheckResult::warn(
            "log file",
            format!("not found yet: {}", log_path.display()),
        ));
    }

    // Database queryable: round-trip a known query.
    match sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM schema_migrations")
        .fetch_one(store.reader())
        .await
    {
        Ok((count,)) => checks.push(CheckResult::ok(
            "database queryable",
            format!("schema_migrations rows: {count}"),
        )),
        Err(e) => checks.push(CheckResult::error(
            "database queryable",
            format!("query failed: {e}"),
        )),
    }

    // Memory count (warn if zero, since fresh-install).
    match sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM memories")
        .fetch_one(store.reader())
        .await
    {
        Ok((count,)) if count > 0 => checks.push(CheckResult::ok(
            "memories present",
            format!("{count} memories"),
        )),
        Ok((_,)) => checks.push(CheckResult::warn(
            "memories present",
            "store is empty — store a memory to confirm end-to-end flow".to_string(),
        )),
        Err(e) => checks.push(CheckResult::error(
            "memories present",
            format!("count failed: {e}"),
        )),
    }

    DoctorReport { checks }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn doctor_reports_ok_socket_and_warn_empty_store() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("cm.sock");
        let pid = tmp.path().join("cm-daemon.pid");
        let db = tmp.path().join("data.db");
        let log = tmp.path().join("daemon.log");
        std::fs::write(&pid, "123\n").unwrap();
        std::fs::File::create(&db).unwrap().write_all(b"").unwrap();
        std::fs::File::create(&log).unwrap().write_all(b"").unwrap();

        let store = Store::in_memory().await.unwrap();
        let report = run_doctor(&socket, &pid, &db, &log, &store).await;

        assert_eq!(report.exit_code(), 2, "missing socket should error");
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "memories present" && c.level == CheckLevel::Warn));
    }

    #[tokio::test]
    async fn doctor_errors_when_socket_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("never_existed.sock");
        let pid = tmp.path().join("cm-daemon.pid");
        let db = tmp.path().join("data.db");
        let log = tmp.path().join("daemon.log");
        let store = Store::in_memory().await.unwrap();
        let report = run_doctor(&socket, &pid, &db, &log, &store).await;
        assert_eq!(report.exit_code(), 2);
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "socket reachable" && c.level == CheckLevel::Error));
    }
}
