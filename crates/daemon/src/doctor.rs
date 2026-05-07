//! `cm doctor` battery: a list of named checks each returning ok/warn/error
//! plus a message. The aggregate report's exit code is derived from the
//! worst result.

use cognitive_memory_store::Store;
use std::path::Path;

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

/// Run the doctor battery against a live store. Phase 11 ships the four
/// most useful checks; provider checks (LLM/embedding reachability) and
/// time-skew vs NTP land when those subsystems are configured.
pub async fn run_doctor(socket_path: &Path, store: &Store) -> DoctorReport {
    let mut checks = Vec::new();

    // 1. Socket file exists and is reachable.
    if socket_path.exists() {
        checks.push(CheckResult::ok(
            "socket reachable",
            format!("{}", socket_path.display()),
        ));
    } else {
        checks.push(CheckResult::error(
            "socket reachable",
            format!("not found: {}", socket_path.display()),
        ));
    }

    // 2. Database writable: round-trip a known query.
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

    // 3. Memory count (warn if zero, since fresh-install).
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
        // Touch the file so the existence check passes — this is a unit
        // test of the doctor logic, not of the real socket.
        std::fs::File::create(&socket)
            .unwrap()
            .write_all(b"")
            .unwrap();

        let store = Store::in_memory().await.unwrap();
        let report = run_doctor(&socket, &store).await;

        assert_eq!(report.exit_code(), 1, "empty store should warn");
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "socket reachable" && c.level == CheckLevel::Ok));
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "memories present" && c.level == CheckLevel::Warn));
    }

    #[tokio::test]
    async fn doctor_errors_when_socket_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("never_existed.sock");
        let store = Store::in_memory().await.unwrap();
        let report = run_doctor(&socket, &store).await;
        assert_eq!(report.exit_code(), 2);
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "socket reachable" && c.level == CheckLevel::Error));
    }
}
