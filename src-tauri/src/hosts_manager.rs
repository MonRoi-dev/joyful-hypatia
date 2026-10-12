use crate::engine::VpnError;
use std::fs::{copy, rename, write, read_to_string};
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::Command;
use tokio::sync::Mutex;

const HOSTS_PATH: &str = r"C:\Windows\System32\drivers\etc\hosts";
const BACKUP_PATH: &str = r"C:\Windows\System32\drivers\etc\hosts.bak";
const TMP_PATH: &str = r"C:\Windows\System32\drivers\etc\hosts.tmp";
const SECTION_START: &str = "# BEGIN JOYFUL_VPN MANAGED HOSTS";
const SECTION_END: &str = "# END JOYFUL_VPN MANAGED HOSTS";

pub struct HostsManager {
    lock: Mutex<()>,
}

impl HostsManager {
    pub fn new() -> Self {
        Self {
            lock: Mutex::new(()),
        }
    }

    pub async fn apply_entries(&self, entries: &[(Ipv4Addr, String)]) -> Result<(), VpnError> {
        let _guard = self.lock.lock().await;

        if !Path::new(BACKUP_PATH).exists() && Path::new(HOSTS_PATH).exists() {
            let _ = copy(HOSTS_PATH, BACKUP_PATH);
        }

        let raw_content = read_to_string(HOSTS_PATH).unwrap_or_default();
        let mut sanitized_lines = Vec::new();
        let mut inside_section = false;

        for line in raw_content.lines() {
            if line.trim() == SECTION_START {
                inside_section = true;
                continue;
            }
            if line.trim() == SECTION_END {
                inside_section = false;
                continue;
            }
            if !inside_section {
                sanitized_lines.push(line);
            }
        }

        let mut buffer = sanitized_lines.join("\r\n");
        if !buffer.is_empty() && !buffer.ends_with("\r\n") {
            buffer.push_str("\r\n");
        }

        if !entries.is_empty() {
            buffer.push_str(SECTION_START);
            buffer.push_str("\r\n");
            for (ip, hostname) in entries {
                buffer.push_str(&format!("{ip} {hostname}\r\n"));
            }
            buffer.push_str(SECTION_END);
            buffer.push_str("\r\n");
        }

        write(TMP_PATH, buffer.as_bytes())?;
        rename(TMP_PATH, HOSTS_PATH)?;

        let mut flush_cmd = Command::new("ipconfig");
        flush_cmd.arg("/flushdns");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            flush_cmd.creation_flags(0x08000000);
        }
        let _ = flush_cmd.output();

        Ok(())
    }

    pub async fn revert_to_backup(&self) -> Result<(), VpnError> {
        let _guard = self.lock.lock().await;

        if Path::new(BACKUP_PATH).exists() {
            let _ = copy(BACKUP_PATH, HOSTS_PATH);
            let mut flush_cmd = Command::new("ipconfig");
            flush_cmd.arg("/flushdns");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                flush_cmd.creation_flags(0x08000000);
            }
            let _ = flush_cmd.output();
        }

        Ok(())
    }
}
